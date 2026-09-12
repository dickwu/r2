//! Replayable, identity-bound relay transfers and multipart recovery.
use super::config::MoveConfig;
use super::planner::{head_identity, storage_error, TRANSFER_MARKER};
use super::state::update_move_status;
use super::types::{MoveProgress, MAX_CONCURRENT_PARTS};
use crate::db;
use crate::db::move_sessions::{get_move_journal, save_move_journal, MoveJournal, SourceIdentity};
use crate::db::MoveSession;
use crate::providers::s3_client::{describe_s3_error, is_transient_s3_error};
use crate::transfer_progress::SpeedWindow;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::operation::head_object::HeadObjectOutput;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use futures_util::{stream, StreamExt};
use reqwest::Client;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

#[path = "relay_protocol.rs"]
pub(crate) mod protocol;
use protocol::{
    attempt_timeout, fetch_payload, interruptible, retry_delay, Payload, MAX_ATTEMPTS, MIB,
};

const MULTIPART_THRESHOLD: u64 = 100 * MIB;
const GIB: u64 = 1024 * MIB;
const TIB: u64 = 1024 * GIB;

pub(crate) fn shared_http_client() -> Result<Client, String> {
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .connect_timeout(Duration::from_secs(10))
                // Object encoding is storage metadata, never a reason to transform bytes in a relay.
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| format!("Cannot create relay HTTP client: {e}"))
        })
        .clone()
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MultipartPlan {
    pub part_size: u64,
    pub total_parts: i32,
}

impl MultipartPlan {
    pub(crate) fn new(
        config: &MoveConfig,
        total: u64,
        persisted: Option<u64>,
    ) -> Result<Self, String> {
        // AWS's current limit is 10,000 x 5 GiB. R2's exact documented footnotes
        // are smaller than the rounded table. Compatible endpoints use the
        // conservative established 5 TiB ceiling until capability discovery exists.
        let (max_object, max_part) = match config {
            MoveConfig::R2(_) => (5 * TIB - 5 * GIB, 5 * GIB - 5 * MIB),
            MoveConfig::Aws(cfg)
                if cfg
                    .endpoint_host
                    .as_deref()
                    .is_none_or(|host| host.trim().is_empty()) =>
            {
                (10_000 * 5 * GIB, 5 * GIB)
            }
            _ => (5 * TIB, 5 * GIB),
        };
        if total == 0 || total > max_object {
            return Err("Object size exceeds destination multipart capability".into());
        }
        let part_size =
            persisted.unwrap_or_else(|| (20 * MIB).max(total.div_ceil(10_000)).div_ceil(MIB) * MIB);
        if !(5 * MIB..=max_part).contains(&part_size) {
            return Err("Invalid persisted multipart part size".into());
        }
        let total_parts = total.div_ceil(part_size);
        if total_parts > 10_000 {
            return Err("Multipart plan would exceed 10,000 parts".into());
        }
        Ok(Self {
            part_size,
            total_parts: total_parts as i32,
        })
    }

    pub(crate) fn range(&self, part: i32, total: u64) -> (u64, u64) {
        let start = (part as u64 - 1) * self.part_size;
        (start, (start + self.part_size).min(total) - 1)
    }
}

pub(crate) fn data_timeouts(bytes: u64) -> aws_sdk_s3::config::Builder {
    aws_sdk_s3::config::Builder::new().timeout_config(
        TimeoutConfig::builder()
            .operation_timeout(attempt_timeout(bytes))
            .operation_attempt_timeout(attempt_timeout(bytes))
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .build(),
    )
}

async fn save_phase(journal: &mut MoveJournal, phase: &str) -> Result<(), String> {
    journal.stage = phase.into();
    save_move_journal(journal)
        .await
        .map_err(|e| format!("Cannot persist move recovery phase: {e}"))
}

async fn record_destination(
    journal: &mut MoveJournal,
    etag: Option<&str>,
    version: Option<&str>,
) -> Result<(), String> {
    if let Some(etag) = etag.filter(|value| !value.is_empty()) {
        journal.destination = Some(SourceIdentity {
            size: journal.source.size,
            etag: etag.to_owned(),
            version_id: version
                .filter(|value| !value.is_empty() && *value != "null")
                .map(str::to_owned),
        });
        save_move_journal(journal)
            .await
            .map_err(|e| format!("Cannot persist committed destination identity: {e}"))?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn download_part(
    http: &Client,
    source_client: &aws_sdk_s3::Client,
    source_config: &MoveConfig,
    key: &str,
    source: &SourceIdentity,
    range: Option<(u64, u64)>,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<Payload, String> {
    for attempt in 0..MAX_ATTEMPTS {
        // Generate a new signature for every attempt/part; no transfer keeps an
        // hour-old URL. Version and If-Match are included in the signed request.
        let request = source_client
            .get_object()
            .bucket(source_config.bucket())
            .key(key)
            .if_match(&source.etag)
            .set_version_id(source.version_id.clone());
        let signed = interruptible(
            cancelled,
            paused,
            request.presigned(
                PresigningConfig::expires_in(Duration::from_secs(900))
                    .map_err(|e| e.to_string())?,
            ),
        )
        .await?
        .map_err(|e| format!("Cannot sign source read: {e}"))?;
        let expected = range
            .map(|(start, end)| end - start + 1)
            .unwrap_or(source.size);
        let result = interruptible(
            cancelled,
            paused,
            tokio::time::timeout(
                attempt_timeout(expected),
                fetch_payload(
                    http,
                    signed.uri(),
                    range,
                    source.size,
                    &source.etag,
                    cancelled,
                    paused,
                ),
            ),
        )
        .await?;
        match result {
            Ok(Ok(payload)) => return Ok(payload),
            Ok(Err(error)) if error.retryable && attempt + 1 < MAX_ATTEMPTS => {
                retry_delay(attempt, error.retry_after, cancelled, paused).await?;
            }
            Ok(Err(error)) => return Err(error.message),
            Err(_) if attempt + 1 < MAX_ATTEMPTS => {
                retry_delay(attempt, None, cancelled, paused).await?;
            }
            Err(_) => return Err("transient: source read exhausted its time budget".into()),
        }
    }
    unreachable!("bounded read attempts return a result")
}

/// The remote inventory is authoritative for an existing MPU, including parts
/// accepted just before a crash. Never trust local success rows on their own.
#[allow(clippy::too_many_arguments)]
async fn reconcile_parts(
    client: &aws_sdk_s3::Client,
    config: &MoveConfig,
    key: &str,
    upload_id: &str,
    plan: MultipartPlan,
    total: u64,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<BTreeMap<i32, (String, u64)>, String> {
    let mut result = BTreeMap::new();
    let mut marker = None;
    let mut seen_markers = HashSet::new();
    loop {
        let mut response = None;
        for attempt in 0..MAX_ATTEMPTS {
            match interruptible(
                cancelled,
                paused,
                client
                    .list_parts()
                    .bucket(config.bucket())
                    .key(key)
                    .upload_id(upload_id)
                    .set_part_number_marker(marker.clone())
                    .max_parts(1000)
                    .send(),
            )
            .await?
            {
                Ok(page) => {
                    response = Some(page);
                    break;
                }
                Err(error) if is_transient_s3_error(&error) && attempt + 1 < MAX_ATTEMPTS => {
                    retry_delay(attempt, None, cancelled, paused).await?
                }
                Err(error) => {
                    return Err(storage_error(
                        "ListParts",
                        error.code(),
                        error.raw_response().map(|r| r.status().as_u16()),
                        describe_s3_error(&error),
                        false,
                    ))
                }
            }
        }
        let page = response.ok_or("ListParts exhausted retry budget")?;
        for part in page.parts() {
            let number = part
                .part_number()
                .filter(|p| *p > 0 && *p <= plan.total_parts)
                .ok_or("ListParts returned an invalid part number")?;
            let etag = part
                .e_tag()
                .filter(|s| !s.is_empty())
                .ok_or("ListParts returned a missing ETag")?
                .to_owned();
            let size = part
                .size()
                .and_then(|n| u64::try_from(n).ok())
                .ok_or("ListParts returned an invalid size")?;
            let (start, end) = plan.range(number, total);
            if size != end - start + 1 {
                return Err(
                    "conflict: remote multipart geometry differs from the saved plan".into(),
                );
            }
            if result.insert(number, (etag, size)).is_some() {
                return Err("ListParts returned a duplicate part".into());
            }
        }
        if !page.is_truncated().unwrap_or(false) {
            break;
        }
        let next = page
            .next_part_number_marker()
            .filter(|m| !m.is_empty())
            .ok_or("Truncated ListParts response has no cursor")?
            .to_owned();
        let next_number: i32 = next
            .parse()
            .map_err(|_| "ListParts cursor is not a part number")?;
        let previous: i32 = marker
            .as_deref()
            .unwrap_or("0")
            .parse()
            .map_err(|_| "Invalid previous ListParts cursor")?;
        if page.parts().is_empty()
            || next_number <= previous
            || next_number > plan.total_parts
            || !seen_markers.insert(next.clone())
        {
            return Err("ListParts returned a nonadvancing cursor".into());
        }
        marker = Some(next);
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn transfer_part(
    http: &Client,
    source_client: &aws_sdk_s3::Client,
    dest_client: &aws_sdk_s3::Client,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    session: &MoveSession,
    source: &SourceIdentity,
    upload_id: &str,
    part: i32,
    plan: MultipartPlan,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<(i32, String, u64), String> {
    let payload = download_part(
        http,
        source_client,
        source_config,
        &session.source_key,
        source,
        Some(plan.range(part, source.size)),
        cancelled,
        paused,
    )
    .await?;
    for attempt in 0..MAX_ATTEMPTS {
        let body = payload
            .body
            .try_clone()
            .ok_or("Upload part payload is not replayable")?;
        let result = interruptible(
            cancelled,
            paused,
            dest_client
                .upload_part()
                .bucket(dest_config.bucket())
                .key(&session.dest_key)
                .upload_id(upload_id)
                .part_number(part)
                .content_length(payload.len as i64)
                .body(ByteStream::new(body))
                .customize()
                .config_override(data_timeouts(payload.len))
                .send(),
        )
        .await?;
        match result {
            Ok(output) => {
                let etag = output
                    .e_tag()
                    .filter(|value| !value.is_empty())
                    .ok_or("UploadPart returned no ETag")?
                    .to_owned();
                db::save_move_upload_part(&session.id, part, &etag, payload.len as i64)
                    .await
                    .map_err(|e| format!("Cannot persist uploaded part: {e}"))?;
                return Ok((part, etag, payload.len));
            }
            // Replaying the same part number is safe even when the previous
            // attempt committed: immutable bytes and upload identity are fixed.
            Err(error) if is_transient_s3_error(&error) && attempt + 1 < MAX_ATTEMPTS => {
                retry_delay(attempt, None, cancelled, paused).await?
            }
            Err(error) => {
                return Err(storage_error(
                    "UploadPart",
                    error.code(),
                    error.raw_response().map(|r| r.status().as_u16()),
                    describe_s3_error(&error),
                    false,
                ))
            }
        }
    }
    unreachable!("bounded part attempts return a result")
}

#[allow(clippy::too_many_arguments)]
async fn stream_multipart(
    http: &Client,
    source_client: &aws_sdk_s3::Client,
    dest_client: &aws_sdk_s3::Client,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    source_head: &HeadObjectOutput,
    session: &MoveSession,
    journal: &mut MoveJournal,
    app: &AppHandle,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<u64, String> {
    let total = journal.source.size;
    let saved = db::get_move_upload_session(&session.id)
        .await
        .map_err(|e| format!("Cannot read multipart recovery record: {e}"))?;
    let plan = MultipartPlan::new(
        dest_config,
        total,
        saved.as_ref().map(|(_, size)| *size as u64),
    )?;
    let upload_id = match saved {
        Some((upload_id, _)) => upload_id,
        None => {
            let mut metadata = source_head.metadata().cloned().unwrap_or_default();
            metadata.insert(TRANSFER_MARKER.into(), session.id.clone());
            let output = interruptible(
                cancelled,
                paused,
                dest_client
                    .create_multipart_upload()
                    .bucket(dest_config.bucket())
                    .key(&session.dest_key)
                    .set_metadata(Some(metadata))
                    .set_content_type(source_head.content_type().map(str::to_owned))
                    .set_cache_control(source_head.cache_control().map(str::to_owned))
                    .set_content_disposition(source_head.content_disposition().map(str::to_owned))
                    .set_content_encoding(source_head.content_encoding().map(str::to_owned))
                    .set_content_language(source_head.content_language().map(str::to_owned))
                    .send(),
            )
            .await?
            .map_err(|e| describe_s3_error(&e))?;
            let upload_id = output
                .upload_id()
                .ok_or("CreateMultipartUpload returned no upload ID")?
                .to_owned();
            if let Err(error) =
                db::save_move_upload_session(&session.id, &upload_id, plan.part_size as i64).await
            {
                // No destination publication occurred. Best-effort cleanup; the
                // original DB failure remains visible and no transfer continues.
                let _ = tokio::time::timeout(
                    Duration::from_secs(15),
                    dest_client
                        .abort_multipart_upload()
                        .bucket(dest_config.bucket())
                        .key(&session.dest_key)
                        .upload_id(&upload_id)
                        .send(),
                )
                .await;
                return Err(format!("Cannot persist multipart upload identity: {error}"));
            }
            upload_id
        }
    };
    let mut completed = reconcile_parts(
        dest_client,
        dest_config,
        &session.dest_key,
        &upload_id,
        plan,
        total,
        cancelled,
        paused,
    )
    .await?;
    // Replace stale local inventory after the complete remote pagination succeeds.
    db::delete_move_upload_parts(&session.id)
        .await
        .map_err(|e| format!("Cannot reconcile multipart journal: {e}"))?;
    for (part, (etag, size)) in &completed {
        db::save_move_upload_part(&session.id, *part, etag, *size as i64)
            .await
            .map_err(|e| format!("Cannot persist reconciled part: {e}"))?;
    }
    let mut uploaded: u64 = completed.values().map(|(_, size)| *size).sum();
    let speed = SpeedWindow::with_baseline(uploaded);
    let pending: Vec<i32> = (1..=plan.total_parts)
        .filter(|part| !completed.contains_key(part))
        .collect();
    let stopped = AtomicBool::new(false);
    let source = journal.source.clone();
    let jobs = stream::iter(pending.into_iter().map(|part| {
        let stopped = &stopped;
        let upload_id = &upload_id;
        let source = &source;
        async move {
            if stopped.load(Ordering::SeqCst) {
                return Ok(None);
            }
            let result = transfer_part(
                http,
                source_client,
                dest_client,
                source_config,
                dest_config,
                session,
                source,
                upload_id,
                part,
                plan,
                cancelled,
                paused,
            )
            .await;
            if result.is_err() {
                stopped.store(true, Ordering::SeqCst);
            }
            result.map(Some)
        }
    }))
    .buffer_unordered(MAX_CONCURRENT_PARTS);
    tokio::pin!(jobs);
    let mut first_error = None;
    while let Some(result) = jobs.next().await {
        match result {
            Ok(Some((part, etag, size))) => {
                completed.insert(part, (etag, size));
                uploaded += size;
                let percent = ((uploaded as f64 / total as f64) * 100.0).floor().min(99.0) as u32;
                let _ = app.emit(
                    "move-progress",
                    MoveProgress {
                        task_id: session.id.clone(),
                        phase: "uploading".into(),
                        percent,
                        transferred_bytes: uploaded,
                        total_bytes: total,
                        speed: speed.sample(uploaded),
                    },
                );
                let _ = db::update_move_progress(&session.id, percent as i64).await;
            }
            Ok(None) => {}
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    interruptible(cancelled, paused, std::future::ready(())).await?;
    if completed.len() != plan.total_parts as usize {
        return Err("Missing upload parts".into());
    }
    let parts = completed
        .into_iter()
        .map(|(part, (etag, _))| {
            CompletedPart::builder()
                .part_number(part)
                .e_tag(etag)
                .build()
        })
        .collect();
    // Persist before dispatch: cancellation, network loss, or a crash can leave
    // Complete committed. The worker reconciles the marker before any replay.
    save_phase(journal, "outcome_unknown").await?;
    let output = interruptible(
        cancelled,
        paused,
        dest_client
            .complete_multipart_upload()
            .if_none_match("*")
            .bucket(dest_config.bucket())
            .key(&session.dest_key)
            .upload_id(&upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            )
            .send(),
    )
    .await?;
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            let status = error.raw_response().map(|r| r.status().as_u16());
            let message = storage_error(
                "CompleteMultipartUpload",
                error.code(),
                status,
                describe_s3_error(&error),
                true,
            );
            if matches!(status, Some(400..=499)) && status != Some(408) {
                save_phase(journal, "transferring").await?;
            }
            return Err(message);
        }
    };
    record_destination(journal, output.e_tag(), output.version_id()).await?;
    Ok(total)
}

#[allow(clippy::too_many_arguments)]
async fn stream_single_put(
    http: &Client,
    source_client: &aws_sdk_s3::Client,
    dest_client: &aws_sdk_s3::Client,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    source_head: &HeadObjectOutput,
    session: &MoveSession,
    journal: &mut MoveJournal,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<u64, String> {
    let payload = download_part(
        http,
        source_client,
        source_config,
        &session.source_key,
        &journal.source,
        None,
        cancelled,
        paused,
    )
    .await?;
    let mut metadata = source_head.metadata().cloned().unwrap_or_default();
    metadata.insert(TRANSFER_MARKER.into(), session.id.clone());
    save_phase(journal, "outcome_unknown").await?;
    // Do not automatically replay a potentially committed full PUT.
    let output = interruptible(
        cancelled,
        paused,
        dest_client
            .put_object()
            .if_none_match("*")
            .bucket(dest_config.bucket())
            .key(&session.dest_key)
            .set_metadata(Some(metadata))
            .content_length(payload.len as i64)
            .set_content_type(source_head.content_type().map(str::to_owned))
            .set_cache_control(source_head.cache_control().map(str::to_owned))
            .set_content_disposition(source_head.content_disposition().map(str::to_owned))
            .set_content_encoding(source_head.content_encoding().map(str::to_owned))
            .set_content_language(source_head.content_language().map(str::to_owned))
            .body(ByteStream::new(
                payload
                    .body
                    .try_clone()
                    .ok_or("Full upload payload is not replayable")?,
            ))
            .customize()
            .config_override(data_timeouts(payload.len))
            .send(),
    )
    .await?;
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            let status = error.raw_response().map(|r| r.status().as_u16());
            let message = storage_error(
                "PutObject",
                error.code(),
                status,
                describe_s3_error(&error),
                true,
            );
            if matches!(status, Some(400..=499)) && status != Some(408) {
                save_phase(journal, "transferring").await?;
            }
            return Err(message);
        }
    };
    record_destination(journal, output.e_tag(), output.version_id()).await?;
    Ok(payload.len)
}

pub(crate) async fn stream_transfer_without_temp(
    client: &Client,
    session: &MoveSession,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    app: &AppHandle,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
) -> Result<u64, String> {
    let mut journal = get_move_journal(&session.id)
        .await
        .map_err(|e| format!("Cannot read move recovery journal: {e}"))?
        .ok_or("Source identity must be journaled before relay")?;
    let (identity, head) = interruptible(
        cancelled,
        paused,
        head_identity(source_config, &session.source_key),
    )
    .await??
    .ok_or("Source object no longer exists")?;
    if identity != journal.source {
        return Err("conflict: source changed since the transfer was recorded".into());
    }
    if journal.source.etag.is_empty() {
        return Err("Source cannot be frozen without an ETag".into());
    }
    if journal.stage == "outcome_unknown" {
        return Err("outcome_unknown: destination must be reconciled before relay resumes".into());
    }
    let source_client = source_config.client().await?;
    let dest_client = dest_config.client().await?;
    let condition = if journal.source.size >= MULTIPART_THRESHOLD {
        crate::providers::conditional::Condition::CompleteCreate
    } else {
        crate::providers::conditional::Condition::PutCreate
    };
    if !interruptible(cancelled, paused, dest_config.supports_condition(condition)).await?? {
        return Err("needs_action: This endpoint did not enforce conditional destination creation; the source and destination were retained".into());
    }
    update_move_status(app, &session.id, "uploading", None).await;
    save_phase(&mut journal, "transferring").await?;
    if journal.source.size >= MULTIPART_THRESHOLD {
        stream_multipart(
            client,
            &source_client,
            &dest_client,
            source_config,
            dest_config,
            &head,
            session,
            &mut journal,
            app,
            cancelled,
            paused,
        )
        .await
    } else {
        stream_single_put(
            client,
            &source_client,
            &dest_client,
            source_config,
            dest_config,
            &head,
            session,
            &mut journal,
            cancelled,
            paused,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn aws() -> MoveConfig {
        MoveConfig::Aws(crate::providers::aws::AwsConfig {
            bucket: "b".into(),
            access_key_id: "a".into(),
            secret_access_key: "s".into(),
            region: "us-east-1".into(),
            endpoint_scheme: None,
            endpoint_host: None,
            force_path_style: false,
        })
    }
    #[test]
    fn plans_more_than_195_gib_without_exceeding_parts_limit() {
        let size = 201 * GIB;
        let plan = MultipartPlan::new(&aws(), size, None).unwrap();
        assert!(plan.part_size > 20 * MIB);
        assert!(plan.total_parts <= 10_000);
        assert_eq!(plan.range(plan.total_parts, size).1, size - 1);
    }
    #[test]
    fn preserves_resume_geometry_and_rejects_invalid_old_plan() {
        let size = 201 * GIB;
        assert!(MultipartPlan::new(&aws(), size, Some(20 * MIB)).is_err());
        let resumed = MultipartPlan::new(&aws(), size, Some(32 * MIB)).unwrap();
        assert_eq!(resumed.part_size, 32 * MIB);
        assert!(MultipartPlan::new(&aws(), size, Some(0)).is_err());
    }
    #[test]
    fn provider_limits_are_distinct_and_boundary_parts_valid() {
        let max_aws = 10_000 * 5 * GIB;
        let plan = MultipartPlan::new(&aws(), max_aws, None).unwrap();
        assert_eq!(plan.total_parts, 10_000);
        assert_eq!(plan.part_size, 5 * GIB);
        assert!(MultipartPlan::new(&aws(), max_aws + 1, None).is_err());
        let r2 = MoveConfig::R2(crate::r2::R2Config {
            account_id: "a".into(),
            bucket: "b".into(),
            access_key_id: "a".into(),
            secret_access_key: "s".into(),
        });
        assert!(MultipartPlan::new(&r2, 5 * TIB - 5 * GIB, None).is_ok());
        assert!(MultipartPlan::new(&r2, 5 * TIB - 5 * GIB + 1, None).is_err());
        assert!(MultipartPlan::new(&r2, GIB, Some(5 * GIB)).is_err());
    }
    async fn s3_fixture(
        responses: Vec<String>,
    ) -> (MoveConfig, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = MoveConfig::Minio(crate::providers::minio::MinioConfig {
            bucket: "bucket".into(),
            access_key_id: "test-access".into(),
            secret_access_key: "test-secret".into(),
            endpoint_scheme: "http".into(),
            endpoint_host: listener.local_addr().unwrap().to_string(),
            force_path_style: true,
        });
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 2048];
                loop {
                    let size = socket.read(&mut chunk).await.unwrap();
                    request.extend_from_slice(&chunk[..size]);
                    if size == 0 || request.windows(4).any(|part| part == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&request).into_owned());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (config, handle)
    }

    fn xml_response(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
    }

    fn part_xml(number: i32, size: u64) -> String {
        format!("<Part><PartNumber>{number}</PartNumber><ETag>\"part-{number}\"</ETag><Size>{size}</Size></Part>")
    }

    #[tokio::test]
    async fn reconciles_more_than_one_thousand_remote_parts() {
        let first_parts: String = (1..=1000).map(|part| part_xml(part, 20 * MIB)).collect();
        let first = format!("<ListPartsResult><IsTruncated>true</IsTruncated><NextPartNumberMarker>1000</NextPartNumberMarker>{first_parts}</ListPartsResult>");
        let second = format!(
            "<ListPartsResult><IsTruncated>false</IsTruncated>{}</ListPartsResult>",
            part_xml(1001, 20 * MIB)
        );
        let (config, server) = s3_fixture(vec![xml_response(&first), xml_response(&second)]).await;
        let plan = MultipartPlan::new(&config, 1001 * 20 * MIB, Some(20 * MIB)).unwrap();
        let parts = reconcile_parts(
            &config.client().await.unwrap(),
            &config,
            "object",
            "upload",
            plan,
            1001 * 20 * MIB,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!(parts.len(), 1001);
        assert_eq!(parts[&1001], ("\"part-1001\"".into(), 20 * MIB));
        let requests = server.await.unwrap();
        assert!(requests[1].contains("part-number-marker=1000"));
    }

    #[tokio::test]
    async fn rejects_truncated_parts_without_cursor_and_wrong_geometry() {
        for xml in [
            format!(
                "<ListPartsResult><IsTruncated>true</IsTruncated>{}</ListPartsResult>",
                part_xml(1, 20 * MIB)
            ),
            format!(
                "<ListPartsResult><IsTruncated>false</IsTruncated>{}</ListPartsResult>",
                part_xml(1, MIB)
            ),
        ] {
            let (config, server) = s3_fixture(vec![xml_response(&xml)]).await;
            let plan = MultipartPlan::new(&config, 40 * MIB, Some(20 * MIB)).unwrap();
            assert!(reconcile_parts(
                &config.client().await.unwrap(),
                &config,
                "object",
                "upload",
                plan,
                40 * MIB,
                &AtomicBool::new(false),
                &AtomicBool::new(false)
            )
            .await
            .is_err());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn retries_only_the_failed_range_with_a_fresh_conditional_signature() {
        let failed = "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_string();
        let success = "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-3/4\r\nContent-Length: 2\r\nETag: \"v1\"\r\nConnection: close\r\n\r\ncd".to_string();
        let (config, server) = s3_fixture(vec![failed, success]).await;
        let source = SourceIdentity {
            size: 4,
            etag: "\"v1\"".into(),
            version_id: Some("version-one".into()),
        };
        let payload = download_part(
            &shared_http_client().unwrap(),
            &config.client().await.unwrap(),
            &config,
            "object",
            &source,
            Some((2, 3)),
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!(payload.len, 2);
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests {
            assert!(request.contains("versionId=version-one"));
            assert!(request.to_ascii_lowercase().contains("if-match: \"v1\""));
            assert!(request.to_ascii_lowercase().contains("range: bytes=2-3"));
        }
    }
}
