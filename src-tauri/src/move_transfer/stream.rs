//! Replayable, identity-bound relay transfers and multipart recovery.
use super::config::MoveConfig;
use super::planner::{
    head_identity_checked, scope, storage_error, verified_destination, verify_unknown_content,
    TRANSFER_MARKER,
};
use super::state::update_move_status;
use super::types::{MoveProgress, MAX_CONCURRENT_PARTS};
use crate::db;
use crate::db::move_sessions::{get_move_journal, save_move_journal, MoveJournal, SourceIdentity};
use crate::db::MoveSession;
use crate::providers::multipart::{complete_receipts, PartReceipt, PartReconciler};
use crate::providers::operation::{
    execute as execute_operation, AttemptError, OperationContext, OperationError, OperationKind,
};
use crate::providers::s3_client::{describe_s3_error, StorageErrorClass};
use crate::transfer_progress::SpeedWindow;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::operation::head_object::HeadObjectOutput;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use futures_util::{stream, StreamExt};
use reqwest::Client;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

#[path = "relay_protocol.rs"]
pub(crate) mod protocol;
use protocol::{
    attempt_timeout, interruptible, operation_budget, Payload, RelayBudget, MAX_ATTEMPTS, MIB,
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
            // Upload response headers normally arrive only after the request body.
            // This first-byte timeout must allow the same size budget as the upload.
            .read_timeout(attempt_timeout(bytes))
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
    budget: &RelayBudget,
    http: &Client,
    source_client: &aws_sdk_s3::Client,
    source_config: &MoveConfig,
    key: &str,
    source: &SourceIdentity,
    range: Option<(u64, u64)>,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<Payload, String> {
    let expected = range
        .map(|(start, end)| end - start + 1)
        .unwrap_or(source.size);
    // Relay memory first, and only then a request slot (see RelayBudget). The
    // reservation outlives failed attempts and becomes the payload's.
    let reservation = budget
        .reserve(range, source.size, cancelled, paused)
        .await
        .map_err(|error| error.message)?;
    let endpoint = source_config.operation_endpoint();
    let scope = format!("{}:{}", endpoint, source_config.bucket());
    let identity = format!(
        "{}:{}:{}:{:?}",
        key,
        source.etag,
        source.version_id.as_deref().unwrap_or(""),
        range
    );
    let context = OperationContext::new(
        OperationKind::Get,
        &endpoint,
        &scope,
        &identity,
        tokio::time::Instant::now() + operation_budget(expected),
        cancelled,
    )
    .with_pause(paused)
    .with_max_attempts(MAX_ATTEMPTS as u32);
    let fetched = execute_operation(&context, || async {
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
                    .map_err(|e| AttemptError::permanent(e.to_string()))?,
            ),
        )
        .await
        .map_err(AttemptError::permanent)?
        .map_err(|e| AttemptError::permanent(format!("Cannot sign source read: {e}")))?;
        match tokio::time::timeout(
            attempt_timeout(expected),
            reservation.fetch(http, signed.uri(), &source.etag, cancelled, paused),
        )
        .await
        {
            Ok(Ok(fetched)) => Ok(fetched),
            Ok(Err(error)) if error.retryable => {
                let mut attempt = AttemptError::transient(error.message);
                if let Some(wait) = error.retry_after {
                    attempt = attempt.with_retry_after(wait);
                }
                Err(attempt)
            }
            Ok(Err(error)) => Err(AttemptError::permanent(error.message)),
            Err(_) => Err(AttemptError::transient(
                "source read exhausted its time budget",
            )),
        }
    })
    .await
    .map_err(|error| super::planner::operation_error("source read", error))?;
    Ok(reservation.into_payload(fetched))
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
    local: Vec<PartReceipt>,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<Option<std::collections::BTreeMap<i32, PartReceipt>>, String> {
    let mut reconciler = PartReconciler::new(total, plan.part_size, local)?;
    let mut marker = None;
    let mut seen_markers = HashSet::new();
    let endpoint = config.operation_endpoint();
    let list_scope = scope(config)?;
    loop {
        let marker_identity = marker.as_deref().unwrap_or("start");
        let identity = format!("{upload_id}:{marker_identity}");
        let context = OperationContext::new(
            OperationKind::ListParts,
            &endpoint,
            &list_scope,
            &identity,
            tokio::time::Instant::now() + Duration::from_secs(30),
            cancelled,
        )
        .with_pause(paused)
        .with_max_attempts(MAX_ATTEMPTS as u32);
        let page = match execute_operation(&context, || async {
            client
                .list_parts()
                .bucket(config.bucket())
                .key(key)
                .upload_id(upload_id)
                .set_part_number_marker(marker.clone())
                .max_parts(1000)
                .send()
                .await
                .map_err(|error| AttemptError::from_sdk(&error))
        })
        .await
        {
            Ok(page) => page,
            Err(OperationError::Failed { error, .. })
                if error.class == StorageErrorClass::NotFound
                    && error.message == "NoSuchUpload" =>
            {
                return Ok(None);
            }
            Err(error) => return Err(super::planner::operation_error("ListParts", error)),
        };
        for part in page.parts() {
            reconciler.accept(PartReceipt {
                number: part
                    .part_number()
                    .ok_or("ListParts returned an invalid part number")?,
                etag: part
                    .e_tag()
                    .ok_or("ListParts returned a missing ETag")?
                    .to_owned(),
                size: part
                    .size()
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or("ListParts returned an invalid size")?,
            })?;
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
    Ok(Some(reconciler.finish()))
}

#[allow(clippy::too_many_arguments)]
async fn transfer_part(
    budget: &RelayBudget,
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
        budget,
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
    // The payload holds relay memory while UploadPart waits for a request slot;
    // no read waits for memory while holding one, so that wait always ends.
    let endpoint = dest_config.operation_endpoint();
    let dest_scope = scope(dest_config)?;
    let identity = format!("{}:{}:{}", upload_id, part, payload.len);
    // The deadline covers every attempt; data_timeouts bounds each one.
    let context = OperationContext::new(
        OperationKind::UploadPart,
        &endpoint,
        &dest_scope,
        &identity,
        tokio::time::Instant::now() + operation_budget(payload.len),
        cancelled,
    )
    .with_pause(paused)
    .with_max_attempts(MAX_ATTEMPTS as u32);
    let output = execute_operation(&context, || async {
        let body = payload
            .body
            .try_clone()
            .ok_or_else(|| AttemptError::permanent("Upload part payload is not replayable"))?;
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
            .send()
            .await
            .map_err(|error| AttemptError::from_sdk(&error))
    })
    .await
    .map_err(|error| format!("{error}: UploadPart"))?;
    let etag = output
        .e_tag()
        .filter(|value| !value.is_empty())
        .ok_or("UploadPart returned no ETag")?
        .to_owned();
    db::save_move_upload_part(&session.id, part, &etag, payload.len as i64)
        .await
        .map_err(|e| format!("Cannot persist uploaded part: {e}"))?;
    Ok((part, etag, payload.len))
}

/// Validate an already-published object before changing recovery phase. With
/// no response receipt, metadata alone cannot establish that its bytes are ours.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reconcile_uploaded_destination(
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    session: &MoveSession,
    journal: &mut MoveJournal,
    identity: SourceIdentity,
    head: &HeadObjectOutput,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<(), String> {
    verified_destination(
        journal,
        &identity,
        head.metadata()
            .and_then(|metadata| metadata.get(TRANSFER_MARKER))
            .map(String::as_str),
    )?;
    if journal.destination.is_none() {
        interruptible(
            cancelled,
            paused,
            verify_unknown_content(
                source_config,
                dest_config,
                &session.source_key,
                &session.dest_key,
                &journal.source,
                &identity,
                cancelled,
                paused,
            ),
        )
        .await??;
    }
    journal.destination = Some(identity);
    save_phase(journal, "copied").await
}

/// An absent destination after an unknown completion is not proof of failure.
/// Only an explicit missing upload ID permits the atomic conditional restart.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn recover_absent_multipart(
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    session: &MoveSession,
    journal: &mut MoveJournal,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<bool, String> {
    if journal.destination.is_some() {
        return Err("conflict: An acknowledged destination is now absent; do not recreate an object another client may have deleted. Source retained.".into());
    }
    let (upload_id, part_size) = db::get_move_upload_session(&session.id)
        .await
        .map_err(|error| format!("Cannot inspect unknown multipart upload: {error}"))?
        .ok_or(
            "outcome_unknown: Destination publication cannot yet be confirmed; source retained",
        )?;
    let plan = MultipartPlan::new(dest_config, journal.source.size, Some(part_size as u64))?;
    let client = dest_config.client().await?;
    let local = db::get_move_upload_parts(&session.id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(number, etag, size)| PartReceipt {
            number,
            etag,
            size: size as u64,
        })
        .collect();
    if reconcile_parts(
        &client,
        dest_config,
        &session.dest_key,
        &upload_id,
        plan,
        journal.source.size,
        local,
        cancelled,
        paused,
    )
    .await?
    .is_some()
    {
        return Err("outcome_unknown: Multipart upload still exists; completion may still be in flight, source retained".into());
    }
    recover_missing_upload(
        source_config,
        dest_config,
        session,
        journal,
        &upload_id,
        cancelled,
        paused,
    )
    .await
}

/// Called only after S3 explicitly returns NoSuchUpload. Unknown HEAD results
/// and foreign destinations keep the recovery record; confirmed absence clears
/// obsolete geometry before a future conditional upload can begin.
#[allow(clippy::too_many_arguments)]
async fn recover_missing_upload(
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    session: &MoveSession,
    journal: &mut MoveJournal,
    upload_id: &str,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<bool, String> {
    match interruptible(
        cancelled,
        paused,
        head_identity_checked(dest_config, &session.dest_key, cancelled, paused),
    )
    .await??
    {
        Some((identity, head)) => {
            reconcile_uploaded_destination(
                source_config,
                dest_config,
                session,
                journal,
                identity,
                &head,
                cancelled,
                paused,
            )
            .await?;
            Ok(true)
        }
        None => {
            if journal.destination.is_some() {
                return Err("conflict: An acknowledged destination is now absent; its receipt and source were retained".into());
            }
            let mut reset = journal.clone();
            reset.stage = "transferring".into();
            reset.destination = None;
            db::move_sessions::reset_missing_move_upload(upload_id, &reset)
                .await
                .map_err(|error| format!("Cannot reset expired multipart upload: {error}"))?;
            *journal = reset;
            Ok(false)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_multipart_upload(
    dest_client: &aws_sdk_s3::Client,
    dest_config: &MoveConfig,
    source_head: &HeadObjectOutput,
    session: &MoveSession,
    total: u64,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<(MultipartPlan, String), String> {
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
    Ok((plan, upload_id))
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
    let (plan, upload_id) = prepare_multipart_upload(
        dest_client,
        dest_config,
        source_head,
        session,
        total,
        cancelled,
        paused,
    )
    .await?;
    let local = db::get_move_upload_parts(&session.id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(number, etag, size)| PartReceipt {
            number,
            etag,
            size: size as u64,
        })
        .collect();
    let completed = reconcile_parts(
        dest_client,
        dest_config,
        &session.dest_key,
        &upload_id,
        plan,
        total,
        local,
        cancelled,
        paused,
    )
    .await?;
    let Some(mut completed) = completed else {
        if recover_missing_upload(
            source_config,
            dest_config,
            session,
            journal,
            &upload_id,
            cancelled,
            paused,
        )
        .await?
        {
            return Ok(total);
        }
        return Err("transient: Multipart upload expired; its obsolete parts were cleared. Resume to start a new upload.".into());
    };
    // Replace stale local inventory after the complete remote pagination succeeds.
    db::delete_move_upload_parts(&session.id)
        .await
        .map_err(|e| format!("Cannot reconcile multipart journal: {e}"))?;
    for receipt in completed.values() {
        db::save_move_upload_part(
            &session.id,
            receipt.number,
            &receipt.etag,
            receipt.size as i64,
        )
        .await
        .map_err(|e| format!("Cannot persist reconciled part: {e}"))?;
    }
    let mut uploaded: u64 = completed.values().map(|receipt| receipt.size).sum();
    let speed = SpeedWindow::with_baseline(uploaded);
    let pending: Vec<i32> = (1..=plan.total_parts)
        .filter(|part| !completed.contains_key(part))
        .collect();
    journal.metrics.copy_requests = journal
        .metrics
        .copy_requests
        .saturating_add(pending.len() as u64);
    journal.metrics.max_copy_in_flight = journal.metrics.max_copy_in_flight.max(
        pending
            .len()
            .min(MAX_CONCURRENT_PARTS)
            .try_into()
            .unwrap_or(u32::MAX),
    );
    save_move_journal(journal)
        .await
        .map_err(|e| format!("Cannot persist move metrics: {e}"))?;
    let stopped = AtomicBool::new(false);
    let source = journal.source.clone();
    let budget = RelayBudget::shared();
    let jobs = stream::iter(pending.into_iter().map(|part| {
        let stopped = &stopped;
        let upload_id = &upload_id;
        let source = &source;
        let budget = &budget;
        async move {
            if stopped.load(Ordering::SeqCst) {
                return Ok(None);
            }
            let result = transfer_part(
                budget,
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
                completed.insert(
                    part,
                    PartReceipt {
                        number: part,
                        etag,
                        size,
                    },
                );
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
    let receipts = complete_receipts(total, plan.part_size, completed.into_values())?;
    let parts = receipts
        .into_iter()
        .map(|receipt| {
            CompletedPart::builder()
                .part_number(receipt.number)
                .e_tag(receipt.etag)
                .build()
        })
        .collect();
    finish_multipart_upload(
        dest_client,
        source_config,
        dest_config,
        session,
        journal,
        &upload_id,
        parts,
        cancelled,
        paused,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn finish_multipart_upload(
    dest_client: &aws_sdk_s3::Client,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    session: &MoveSession,
    journal: &mut MoveJournal,
    upload_id: &str,
    parts: Vec<CompletedPart>,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<u64, String> {
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
            .upload_id(upload_id)
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
            if error.code() == Some("NoSuchUpload") {
                if recover_missing_upload(
                    source_config,
                    dest_config,
                    session,
                    journal,
                    upload_id,
                    cancelled,
                    paused,
                )
                .await?
                {
                    return Ok(journal.source.size);
                }
                return Err("transient: Multipart upload expired before publication; resume to start a new upload.".into());
            }
            // A generic 404 does not prove expiration. Keep the uncertain
            // phase so the worker verifies the destination before any replay.
            if matches!(status, Some(400..=499)) && !matches!(status, Some(404 | 408)) {
                save_phase(journal, "transferring").await?;
            }
            return Err(message);
        }
    };
    record_destination(journal, output.e_tag(), output.version_id()).await?;
    Ok(journal.source.size)
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
        &RelayBudget::shared(),
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
        head_identity_checked(source_config, &session.source_key, cancelled, paused),
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
    journal
        .metrics
        .copy_started_at_ms
        .get_or_insert_with(|| chrono::Utc::now().timestamp_millis());
    save_phase(&mut journal, "transferring").await?;
    let copied = if journal.source.size >= MULTIPART_THRESHOLD {
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
        journal.metrics.copy_requests = journal.metrics.copy_requests.saturating_add(1);
        journal.metrics.max_copy_in_flight = journal.metrics.max_copy_in_flight.max(1);
        save_move_journal(&journal)
            .await
            .map_err(|e| format!("Cannot persist move metrics: {e}"))?;
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
    }?;
    journal.metrics.copy_completed_at_ms = Some(chrono::Utc::now().timestamp_millis());
    save_move_journal(&journal)
        .await
        .map_err(|e| format!("Cannot persist move metrics: {e}"))?;
    Ok(copied)
}

#[cfg(test)]
pub(crate) mod tests {
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
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn part_xml(number: i32, size: u64) -> String {
        format!(
            "<Part><PartNumber>{number}</PartNumber><ETag>\"part-{number}\"</ETag><Size>{size}</Size></Part>"
        )
    }

    #[tokio::test]
    async fn reconciles_more_than_one_thousand_remote_parts() {
        let first_parts: String = (1..=1000).map(|part| part_xml(part, 20 * MIB)).collect();
        let first = format!(
            "<ListPartsResult><IsTruncated>true</IsTruncated><NextPartNumberMarker>1000</NextPartNumberMarker>{first_parts}</ListPartsResult>"
        );
        let second = format!(
            "<ListPartsResult><IsTruncated>false</IsTruncated>{}</ListPartsResult>",
            part_xml(1001, 20 * MIB)
        );
        let (config, server) = s3_fixture(vec![xml_response(&first), xml_response(&second)]).await;
        let plan = MultipartPlan::new(&config, 1001 * 20 * MIB, Some(20 * MIB)).unwrap();
        let local = (1..=1001)
            .map(|number| PartReceipt {
                number,
                etag: format!("\"part-{number}\""),
                size: 20 * MIB,
            })
            .collect();
        let parts = reconcile_parts(
            &config.client().await.unwrap(),
            &config,
            "object",
            "upload",
            plan,
            1001 * 20 * MIB,
            local,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        let parts = parts.expect("multipart upload still exists");
        assert_eq!(parts.len(), 1001);
        assert_eq!(parts[&1001].etag, "\"part-1001\"");
        assert_eq!(parts[&1001].size, 20 * MIB);
        let requests = server.await.unwrap();
        assert!(requests[1].contains("part-number-marker=1000"));
    }

    #[tokio::test]
    async fn listparts_retry_reconciles_existing_receipts_without_reupload() {
        let parts_xml = format!(
            "<ListPartsResult><IsTruncated>false</IsTruncated>{}{}</ListPartsResult>",
            part_xml(1, 5 * MIB),
            part_xml(2, 5 * MIB)
        );
        let transient = "HTTP/1.1 503 Unavailable\r\nContent-Type: application/xml\r\nContent-Length: 66\r\nConnection: close\r\n\r\n<Error><Code>ServiceUnavailable</Code><Message>try</Message></Error>"
            .to_string();
        let (config, server) = s3_fixture(vec![transient, xml_response(&parts_xml)]).await;
        let plan = MultipartPlan::new(&config, 10 * MIB, Some(5 * MIB)).unwrap();
        let local = vec![
            PartReceipt {
                number: 1,
                etag: "\"part-1\"".into(),
                size: 5 * MIB,
            },
            PartReceipt {
                number: 2,
                etag: "\"part-2\"".into(),
                size: 5 * MIB,
            },
        ];
        let parts = reconcile_parts(
            &config.client().await.unwrap(),
            &config,
            "object",
            "upload",
            plan,
            10 * MIB,
            local,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap()
        .expect("multipart upload still exists");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[&1].etag, "\"part-1\"");
        assert_eq!(parts[&2].etag, "\"part-2\"");
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.contains("uploadId=upload")));
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
            let local = vec![PartReceipt {
                number: 1,
                etag: "\"part-1\"".into(),
                size: 20 * MIB,
            }];
            assert!(reconcile_parts(
                &config.client().await.unwrap(),
                &config,
                "object",
                "upload",
                plan,
                40 * MIB,
                local,
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
            &RelayBudget::shared(),
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
    pub(crate) fn fixture_config(endpoint: &str) -> MoveConfig {
        MoveConfig::Minio(crate::providers::minio::MinioConfig {
            bucket: "bucket".into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            endpoint_scheme: "http".into(),
            endpoint_host: endpoint.strip_prefix("http://").unwrap().into(),
            force_path_style: true,
        })
    }

    pub(crate) async fn test_db_guard() -> tokio::sync::MutexGuard<'static, ()> {
        static DB_TESTS: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        DB_TESTS
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    pub(crate) async fn journal_fixture(name: &str, size: u64) -> (MoveSession, MoveJournal) {
        static DATABASE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        DATABASE
            .get_or_init(|| async {
                db::init_db(std::path::Path::new(":memory:")).await.unwrap();
            })
            .await;
        let id = format!("{name}-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let session = MoveSession {
            id: id.clone(),
            source_key: "source".into(),
            dest_key: "destination".into(),
            source_bucket: "bucket".into(),
            source_account_id: "account".into(),
            source_provider: "minio".into(),
            dest_bucket: "bucket".into(),
            dest_account_id: "account".into(),
            dest_provider: "minio".into(),
            delete_original: true,
            file_size: size as i64,
            progress: 99,
            status: "error".into(),
            error: None,
            created_at: 0,
            updated_at: 0,
        };
        db::move_sessions::create_move_session(&session)
            .await
            .unwrap();
        let journal = MoveJournal {
            task_id: id,
            stage: "outcome_unknown".into(),
            source: SourceIdentity {
                size,
                etag: "\"source\"".into(),
                version_id: None,
            },
            source_scope: "fixture".into(),
            dest_scope: "fixture".into(),
            destination: None,
            retry: Default::default(),
            metrics: Default::default(),
        };
        save_move_journal(&journal).await.unwrap();
        db::save_move_upload_session(&session.id, "expired-upload", (5 * MIB) as i64)
            .await
            .unwrap();
        db::save_move_upload_part(&session.id, 1, "old-part", size as i64)
            .await
            .unwrap();
        (session, journal)
    }

    fn completed_part(etag: &str) -> Vec<CompletedPart> {
        vec![CompletedPart::builder().part_number(1).e_tag(etag).build()]
    }

    fn missing_upload() -> crate::test_s3::Response {
        crate::test_s3::Response::xml(
            404,
            "<Error><Code>NoSuchUpload</Code><Message>The upload does not exist</Message></Error>",
        )
    }

    #[tokio::test]
    async fn consumed_completion_verifies_content_and_records_copied_without_reupload() {
        let _guard = test_db_guard().await;
        use crate::test_s3::{serve, Response};
        for changed in [false, true] {
            let (session, mut journal) = journal_fixture("consumed", 8).await;
            let marker = session.id.clone();
            let fixture = serve(move |request| {
                let marker = marker.clone();
                async move {
                    if request.method == "POST" {
                        return missing_upload();
                    }
                    if request.method == "HEAD" {
                        return Response::empty(200)
                            .header("etag", "\"destination\"")
                            .header("content-length", 8)
                            .header("x-amz-meta-r2-move-task", marker);
                    }
                    let source = request.path.split('?').next().unwrap().ends_with("/source");
                    Response::xml(
                        200,
                        if changed && !source {
                            "modified"
                        } else {
                            "original"
                        },
                    )
                    .header(
                        "etag",
                        if source {
                            "\"source\""
                        } else {
                            "\"destination\""
                        },
                    )
                }
            })
            .await;
            let config = fixture_config(&fixture.endpoint);
            let result = finish_multipart_upload(
                &fixture.client,
                &config,
                &config,
                &session,
                &mut journal,
                "expired-upload",
                completed_part("old-part"),
                &AtomicBool::new(false),
                &AtomicBool::new(false),
            )
            .await;
            let saved = get_move_journal(&session.id).await.unwrap().unwrap();
            if changed {
                assert!(result.unwrap_err().starts_with("conflict:"));
                assert_eq!(saved.stage, "outcome_unknown");
                assert!(saved.destination.is_none());
            } else {
                assert_eq!(result.unwrap(), 8);
                assert_eq!(saved.stage, "copied");
                assert_eq!(saved.destination.unwrap().etag, "\"destination\"");
            }
            let requests = fixture.requests.lock().unwrap();
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.method == "POST")
                    .count(),
                1
            );
            assert!(!requests
                .iter()
                .any(|request| request.method == "PUT" || request.method == "DELETE"));
            let reads: Vec<_> = requests
                .iter()
                .filter(|request| request.method == "GET")
                .collect();
            assert_eq!(reads.len(), 2);
            assert!(reads
                .iter()
                .all(|request| request.headers.contains_key("if-match")));
            assert_eq!(
                requests[0].headers.get("if-none-match").map(String::as_str),
                Some("*")
            );
        }
    }

    #[tokio::test]
    async fn expired_completion_clears_obsolete_parts_then_uploads_new_conditional_session() {
        let _guard = test_db_guard().await;
        use crate::test_s3::{serve, Response};
        let (session, mut journal) = journal_fixture("expired", 8).await;
        let fixture = serve(|request| async move {
            if request.path.contains("uploadId=expired-upload") { return missing_upload(); }
            if request.method == "HEAD" { return Response::empty(404); }
            if request.method == "POST" && request.path.contains("uploads") {
                return Response::xml(200, "<InitiateMultipartUploadResult><UploadId>replacement-upload</UploadId></InitiateMultipartUploadResult>");
            }
            if request.method == "GET" {
                return Response::xml(206, "original").header("etag", "\"source\"").header("content-range", "bytes 0-7/8");
            }
            if request.method == "PUT" {
                assert_eq!(request.body, b"original");
                return Response::empty(200).header("etag", "\"new-part\"");
            }
            assert_eq!(request.method, "POST");
            assert_eq!(request.headers.get("if-none-match").map(String::as_str), Some("*"));
            Response::xml(200, "<CompleteMultipartUploadResult><ETag>\"destination\"</ETag></CompleteMultipartUploadResult>")
        }).await;
        let config = fixture_config(&fixture.endpoint);
        let error = finish_multipart_upload(
            &fixture.client,
            &config,
            &config,
            &session,
            &mut journal,
            "expired-upload",
            completed_part("old-part"),
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap_err();
        assert!(error.starts_with("transient:"));
        assert_eq!(
            get_move_journal(&session.id).await.unwrap().unwrap().stage,
            "transferring"
        );
        assert!(db::get_move_upload_session(&session.id)
            .await
            .unwrap()
            .is_none());
        assert!(db::get_move_upload_parts(&session.id)
            .await
            .unwrap()
            .is_empty());
        let (plan, upload_id) = prepare_multipart_upload(
            &fixture.client,
            &config,
            &HeadObjectOutput::builder().build(),
            &session,
            8,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!(upload_id, "replacement-upload");
        let (number, etag, size) = transfer_part(
            &RelayBudget::shared(),
            &shared_http_client().unwrap(),
            &fixture.client,
            &fixture.client,
            &config,
            &config,
            &session,
            &journal.source,
            &upload_id,
            1,
            plan,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!((number, size), (1, 8));
        assert_eq!(
            finish_multipart_upload(
                &fixture.client,
                &config,
                &config,
                &session,
                &mut journal,
                &upload_id,
                completed_part(&etag),
                &AtomicBool::new(false),
                &AtomicBool::new(false)
            )
            .await
            .unwrap(),
            8
        );
        assert_eq!(
            get_move_journal(&session.id)
                .await
                .unwrap()
                .unwrap()
                .destination
                .unwrap()
                .etag,
            "\"destination\""
        );
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.path.contains("uploadId=expired-upload"))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "PUT")
                .count(),
            1
        );
        assert!(!requests.iter().any(|request| request.method == "DELETE"));
    }

    #[tokio::test]
    async fn unknown_completion_requires_missing_mpu_and_fresh_absence_before_reset() {
        let _guard = test_db_guard().await;
        use crate::test_s3::{serve, Response};
        for missing in [false, true] {
            let (session, mut journal) = journal_fixture("unknown-resume", 8).await;
            let fixture = serve(move |request| async move {
                if request.method == "HEAD" {
                    return Response::empty(404);
                }
                if missing {
                    missing_upload()
                } else {
                    Response::xml(
                        200,
                        "<ListPartsResult><IsTruncated>false</IsTruncated></ListPartsResult>",
                    )
                }
            })
            .await;
            let config = fixture_config(&fixture.endpoint);
            let result = recover_absent_multipart(
                &config,
                &config,
                &session,
                &mut journal,
                &AtomicBool::new(false),
                &AtomicBool::new(false),
            )
            .await;
            if missing {
                assert!(!result.unwrap());
                assert!(db::get_move_upload_session(&session.id)
                    .await
                    .unwrap()
                    .is_none());
                assert_eq!(journal.stage, "transferring");
            } else {
                assert!(result.unwrap_err().starts_with("outcome_unknown:"));
                assert!(db::get_move_upload_session(&session.id)
                    .await
                    .unwrap()
                    .is_some());
                assert_eq!(journal.stage, "outcome_unknown");
            }
            let requests = fixture.requests.lock().unwrap();
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.method == "HEAD")
                    .count(),
                usize::from(missing)
            );
        }
    }

    #[tokio::test]
    async fn missing_upload_does_not_clear_foreign_destination_or_failed_head() {
        let _guard = test_db_guard().await;
        use crate::test_s3::{serve, Response};
        for head_status in [200, 403] {
            let (session, mut journal) = journal_fixture("foreign-or-denied", 8).await;
            let fixture = serve(move |request| async move {
                if request.method == "POST" {
                    missing_upload()
                } else {
                    Response::empty(head_status)
                        .header("etag", "\"foreign\"")
                        .header("content-length", 8)
                        .header("x-amz-meta-r2-move-task", "another-task")
                }
            })
            .await;
            let config = fixture_config(&fixture.endpoint);
            assert!(finish_multipart_upload(
                &fixture.client,
                &config,
                &config,
                &session,
                &mut journal,
                "expired-upload",
                completed_part("old-part"),
                &AtomicBool::new(false),
                &AtomicBool::new(false)
            )
            .await
            .is_err());
            assert!(db::get_move_upload_session(&session.id)
                .await
                .unwrap()
                .is_some());
            assert_eq!(
                db::get_move_upload_parts(&session.id).await.unwrap().len(),
                1
            );
            assert_eq!(
                get_move_journal(&session.id).await.unwrap().unwrap().stage,
                "outcome_unknown"
            );
        }
    }

    #[tokio::test]
    async fn upload_part_allows_a_response_after_thirty_seconds_without_replay() {
        let _guard = test_db_guard().await;
        use crate::test_s3::{serve, Response};
        let (session, journal) = journal_fixture("slow-upload", 5 * MIB).await;
        let fixture = serve(|request| async move {
            if request.method == "GET" {
                return Response {
                    status: 206,
                    headers: vec![
                        ("etag".into(), "\"source\"".into()),
                        (
                            "content-range".into(),
                            format!("bytes 0-{}/{}", 5 * MIB - 1, 5 * MIB),
                        ),
                    ],
                    body: vec![b'x'; (5 * MIB) as usize],
                };
            }
            assert_eq!(request.method, "PUT");
            assert_eq!(request.body.len(), (5 * MIB) as usize);
            // The real S3 first-response-byte timer includes sending the body.
            // The old 30s setting retried here even though its 70s operation
            // budget had ample time remaining. Keep this as an actual wire test.
            tokio::time::sleep(Duration::from_secs(31)).await;
            Response::empty(200).header("etag", "\"slow-part\"")
        })
        .await;
        let config = fixture_config(&fixture.endpoint);
        let plan = MultipartPlan::new(&config, 5 * MIB, Some(5 * MIB)).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(36),
            transfer_part(
                &RelayBudget::shared(),
                &shared_http_client().unwrap(),
                &fixture.client,
                &fixture.client,
                &config,
                &config,
                &session,
                &journal.source,
                "expired-upload",
                1,
                plan,
                &AtomicBool::new(false),
                &AtomicBool::new(false),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.1, "\"slow-part\"");
        assert_eq!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.method == "PUT")
                .count(),
            1
        );
    }
    #[tokio::test(start_paused = true)]
    async fn a_read_stalled_past_one_attempt_timeout_still_gets_its_retry() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        const HEAD: &[u8] = b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-3/4\r\nContent-Length: 2\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n";
        async fn read_request(socket: &mut tokio::net::TcpStream) {
            let mut request = Vec::new();
            let mut chunk = [0_u8; 2048];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let size = socket.read(&mut chunk).await.unwrap();
                assert!(size > 0, "connection closed before the request ended");
                request.extend_from_slice(&chunk[..size]);
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = fixture_config(&format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            // Every gap of the first response is inside the idle timeout, but
            // the whole response outlasts attempt_timeout(2) = 31 s.
            let (mut stalled, _) = listener.accept().await.unwrap();
            let trickle = tokio::spawn(async move {
                read_request(&mut stalled).await;
                tokio::time::sleep(Duration::from_secs(20)).await;
                let _ = stalled.write_all(HEAD).await;
                let _ = stalled.write_all(b"c").await;
                tokio::time::sleep(Duration::from_secs(20)).await;
                let _ = stalled.write_all(b"d").await;
            });
            let (mut retry, _) = listener.accept().await.unwrap();
            read_request(&mut retry).await;
            retry.write_all(HEAD).await.unwrap();
            retry.write_all(b"cd").await.unwrap();
            trickle.abort();
        });
        let config_client = config.client().await.unwrap();
        let start = tokio::time::Instant::now();
        let payload = download_part(
            &RelayBudget::shared(),
            &shared_http_client().unwrap(),
            &config_client,
            &config,
            "object",
            &SourceIdentity {
                size: 4,
                etag: "\"v1\"".into(),
                version_id: None,
            },
            Some((2, 3)),
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!(payload.len, 2);
        assert!(start.elapsed() >= attempt_timeout(2));
        server.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn an_upload_part_stalled_past_one_attempt_timeout_still_gets_its_retry() {
        let _guard = test_db_guard().await;
        use crate::test_s3::{serve, Response};
        use std::sync::atomic::AtomicUsize;
        let (session, journal) = journal_fixture("stalled-upload-part", 8).await;
        let puts = Arc::new(AtomicUsize::new(0));
        let fixture = serve({
            let puts = puts.clone();
            move |request| {
                let puts = puts.clone();
                async move {
                    if request.method == "GET" {
                        return Response::xml(206, "original")
                            .header("etag", "\"source\"")
                            .header("content-range", "bytes 0-7/8");
                    }
                    if puts.fetch_add(1, Ordering::SeqCst) == 0 {
                        // Answers after attempt_timeout(8) = 31 s has passed.
                        tokio::time::sleep(Duration::from_secs(40)).await;
                    }
                    Response::empty(200).header("etag", "\"retried-part\"")
                }
            }
        })
        .await;
        let config = fixture_config(&fixture.endpoint);
        let plan = MultipartPlan::new(&config, 8, Some(5 * MIB)).unwrap();
        let start = tokio::time::Instant::now();
        let (_, etag, size) = transfer_part(
            &RelayBudget::shared(),
            &shared_http_client().unwrap(),
            &fixture.client,
            &fixture.client,
            &config,
            &config,
            &session,
            &journal.source,
            "upload",
            1,
            plan,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!((etag.as_str(), size), ("\"retried-part\"", 8));
        assert_eq!(puts.load(Ordering::SeqCst), 2);
        assert!(start.elapsed() >= attempt_timeout(8));
    }

    #[tokio::test]
    async fn parts_sharing_an_endpoint_take_relay_memory_before_a_request_slot() {
        let _guard = test_db_guard().await;
        use crate::test_s3::{serve, Response};
        const PARTS: i32 = 12;
        let part_size = 5 * MIB;
        let total = PARTS as u64 * part_size;
        let (session, journal) = journal_fixture("shared-endpoint-budget", total).await;
        let fixture = serve(move |request| async move {
            if request.method == "PUT" {
                return Response::empty(200).header("etag", "\"relay-part\"");
            }
            let (start, end) = request.headers["range"]
                .strip_prefix("bytes=")
                .and_then(|range| range.split_once('-'))
                .unwrap();
            let (start, end): (u64, u64) = (start.parse().unwrap(), end.parse().unwrap());
            Response {
                status: 206,
                headers: vec![
                    ("etag".into(), "\"source\"".into()),
                    (
                        "content-range".into(),
                        format!("bytes {start}-{end}/{total}"),
                    ),
                ],
                body: vec![b'x'; (end - start + 1) as usize],
            }
        })
        .await;
        // One endpoint serves every GET and UploadPart (eight data slots),
        // twelve parts are in flight, and relay memory holds only four.
        let config = fixture_config(&fixture.endpoint);
        let plan = MultipartPlan::new(&config, total, Some(part_size)).unwrap();
        let budget = RelayBudget::with_capacity(20, 2);
        let http = shared_http_client().unwrap();
        let (cancelled, paused) = (AtomicBool::new(false), AtomicBool::new(false));
        let transfers = (1..=PARTS).map(|part| {
            transfer_part(
                &budget,
                &http,
                &fixture.client,
                &fixture.client,
                &config,
                &config,
                &session,
                &journal.source,
                "upload",
                part,
                plan,
                &cancelled,
                &paused,
            )
        });
        // Each read may take attempt_timeout(5 MiB) = 70 s. Reads that wait
        // for memory while holding the slots the loaded parts need to upload
        // stall until that deadline and then fail.
        let results = tokio::time::timeout(
            Duration::from_secs(20),
            futures_util::future::join_all(transfers),
        )
        .await
        .expect("relay parts stalled holding request slots while waiting for relay memory");
        for (part, result) in (1..=PARTS).zip(results) {
            assert_eq!(result.unwrap(), (part, "\"relay-part\"".into(), part_size));
        }
        let requests = fixture.requests.lock().unwrap();
        for method in ["GET", "PUT"] {
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.method == method)
                    .count(),
                PARTS as usize
            );
        }
    }

    #[tokio::test]
    async fn unknown_upload_with_a_destination_receipt_never_restarts_after_disappearance() {
        let _guard = test_db_guard().await;
        use crate::test_s3::{serve, Response};
        let (session, mut journal) = journal_fixture("acknowledged-destination-missing", 8).await;
        let receipt = SourceIdentity {
            size: 8,
            etag: "\"acknowledged\"".into(),
            version_id: None,
        };
        journal.destination = Some(receipt.clone());
        save_move_journal(&journal).await.unwrap();
        let fixture = serve(|_| async { Response::empty(404) }).await;
        let config = fixture_config(&fixture.endpoint);
        let error = recover_absent_multipart(
            &config,
            &config,
            &session,
            &mut journal,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap_err();
        assert!(error.starts_with("conflict:"));
        assert!(fixture.requests.lock().unwrap().is_empty());
        assert!(db::get_move_upload_session(&session.id)
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            get_move_journal(&session.id)
                .await
                .unwrap()
                .unwrap()
                .destination,
            Some(receipt)
        );
        assert_eq!(
            db::get_move_upload_parts(&session.id).await.unwrap().len(),
            1
        );
    }
}
