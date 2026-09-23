//! Conditional server-side copy with part-level recovery for verified S3 endpoints.
use super::config::MoveConfig;
use super::planner::{encoded_copy_source, storage_error, TransferPlan, TRANSFER_MARKER};
use crate::db::{
    self,
    move_sessions::{save_move_journal, MoveJournal, SourceIdentity},
    MoveSession,
};
use crate::providers::multipart::{complete_receipts, PartReceipt, PartReconciler};
use crate::providers::operation::{
    execute as execute_operation, AttemptError, OperationContext, OperationKind,
};
use aws_sdk_s3::{
    error::ProvideErrorMetadata,
    types::{CompletedMultipartUpload, CompletedPart, MetadataDirective},
};
use futures_util::{stream, StreamExt};
use log::info;
use std::{
    collections::HashSet,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter};

const SERVER_COPY_PART_CONCURRENCY: usize = 4;
/// The SDK's own per-operation timeout (providers/s3_client.rs). A single
/// CopyObject, CreateMultipartUpload or Complete may legitimately run that long
/// server-side; cutting it shorter only manufactures `outcome_unknown`.
const SERVER_COPY_MUTATION_BUDGET: Duration = Duration::from_secs(120);
/// Replayable part copies and ListParts pages: room for every executor attempt
/// at the SDK's per-attempt timeout. The mount's rename part copies use it too.
pub(crate) const SERVER_COPY_OPERATION_BUDGET: Duration = Duration::from_secs(3 * 120);

fn check_control(cancelled: &AtomicBool, paused: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::SeqCst) {
        return Err("cancelled: Move cancelled; source retained".into());
    }
    if paused.load(Ordering::SeqCst) {
        return Err("paused: Move paused; source retained".into());
    }
    Ok(())
}

fn operation_context<'a>(
    kind: OperationKind,
    endpoint: &'a str,
    scope: &'a str,
    identity: &'a str,
    cancelled: &'a AtomicBool,
    paused: &'a AtomicBool,
) -> OperationContext<'a> {
    OperationContext::new(
        kind,
        endpoint,
        scope,
        identity,
        tokio::time::Instant::now() + SERVER_COPY_OPERATION_BUDGET,
        cancelled,
    )
    .with_pause(paused)
    .with_max_attempts(3)
}

fn operation_scope(account_id: &str, bucket: &str) -> String {
    format!("{account_id}:{bucket}")
}

async fn await_unknown_mutation<T, E>(
    operation: &str,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
    future: impl Future<Output = Result<T, E>>,
) -> Result<Result<T, E>, String> {
    tokio::pin!(future);
    let timeout = tokio::time::sleep(SERVER_COPY_MUTATION_BUDGET);
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            result = &mut future => return Ok(result),
            _ = &mut timeout => {
                return Err(format!("outcome_unknown: {operation} did not finish before the mutation deadline; reconcile destination before retry"));
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if cancelled.load(Ordering::SeqCst) {
                    return Err(format!("outcome_unknown: {operation} was cancelled after dispatch; reconcile destination before retry"));
                }
                if paused.load(Ordering::SeqCst) {
                    return Err(format!("outcome_unknown: {operation} was paused after dispatch; reconcile destination before retry"));
                }
            }
        }
    }
}

struct PartCopyJob<'a> {
    client: &'a aws_sdk_s3::Client,
    dest: &'a MoveConfig,
    session: &'a MoveSession,
    source: &'a str,
    source_etag: &'a str,
    source_size: u64,
    upload_id: &'a str,
    part_number: i32,
    plan: super::stream::MultipartPlan,
    endpoint: &'a str,
    scope: &'a str,
    cancelled: &'a AtomicBool,
    paused: &'a AtomicBool,
}

async fn copy_part(job: PartCopyJob<'_>) -> Result<(i32, String, i64), String> {
    check_control(job.cancelled, job.paused)?;
    let (start, end) = job.plan.range(job.part_number, job.source_size);
    let size = (end - start + 1) as i64;
    let identity = format!(
        "{}:{}:{}:{}:{}",
        job.upload_id, job.part_number, start, end, job.source_etag
    );
    let context = operation_context(
        OperationKind::UploadPartCopy,
        job.endpoint,
        job.scope,
        &identity,
        job.cancelled,
        job.paused,
    );
    let response = execute_operation(&context, || async {
        job.client
            .upload_part_copy()
            .bucket(job.dest.bucket())
            .key(&job.session.dest_key)
            .upload_id(job.upload_id)
            .part_number(job.part_number)
            .copy_source(job.source)
            .copy_source_if_match(job.source_etag)
            .copy_source_range(format!("bytes={start}-{end}"))
            .send()
            .await
            .map_err(|error| AttemptError::from_sdk(&error))
    })
    .await
    .map_err(|error| super::planner::operation_error("UploadPartCopy", error))?;
    let etag = response
        .copy_part_result()
        .and_then(|r| r.e_tag())
        .ok_or("Missing copied part ETag")?
        .to_string();
    db::save_move_upload_part(&job.session.id, job.part_number, &etag, size)
        .await
        .map_err(|e| format!("Cannot journal copied part: {e}"))?;
    Ok((job.part_number, etag, size))
}

async fn save(journal: &MoveJournal) -> Result<(), String> {
    save_move_journal(journal)
        .await
        .map_err(|e| format!("Cannot persist move recovery state: {e}"))
}

/// A SingleCopy plan for an endpoint that is neither native AWS nor R2 must
/// come from the worker's execution plan, which only chooses it once
/// CopyCreate and CopySource are confirmed. Nothing is probed again here, so a
/// failing probe cannot fail a copy that was already confirmed.
#[allow(deprecated)] // AWS SDK has not exposed a string setter for outgoing Expires.
#[allow(clippy::too_many_arguments)] // Preserve explicit mutation, journal, and cancellation ownership at call sites.
pub(crate) async fn copy(
    plan: TransferPlan,
    session: &MoveSession,
    dest: &MoveConfig,
    source_head: &aws_sdk_s3::operation::head_object::HeadObjectOutput,
    journal: &mut MoveJournal,
    app: Option<&AppHandle>,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
) -> Result<u64, String> {
    check_control(cancelled, paused)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    journal.metrics.copy_started_at_ms.get_or_insert(now_ms);
    save(journal).await?;
    let client = dest.client().await?;
    let source = encoded_copy_source(
        &session.source_bucket,
        &session.source_key,
        journal.source.version_id.as_deref(),
    );
    let mut metadata = source_head.metadata().cloned().unwrap_or_default();
    metadata.insert(TRANSFER_MARKER.into(), session.id.clone());
    if plan == TransferPlan::SingleCopy {
        journal.stage = "outcome_unknown".into();
        save(journal).await?;
        let request = client
            .copy_object()
            .bucket(dest.bucket())
            .key(&session.dest_key)
            .copy_source(source)
            .copy_source_if_match(&journal.source.etag)
            .metadata_directive(MetadataDirective::Replace)
            .set_metadata(Some(metadata))
            .set_content_type(source_head.content_type().map(str::to_string))
            .set_cache_control(source_head.cache_control().map(str::to_string))
            .set_content_disposition(source_head.content_disposition().map(str::to_string))
            .set_content_encoding(source_head.content_encoding().map(str::to_string))
            .set_content_language(source_head.content_language().map(str::to_string))
            .set_expires(source_head.expires().cloned());
        check_control(cancelled, paused)?;
        let response = if matches!(dest, MoveConfig::R2(_)) {
            await_unknown_mutation(
                "CopyObject",
                cancelled,
                paused,
                request
                    .customize()
                    .mutate_request(|request| {
                        request
                            .headers_mut()
                            .insert("cf-copy-destination-if-none-match", "*");
                    })
                    .send(),
            )
            .await?
        } else {
            await_unknown_mutation(
                "CopyObject",
                cancelled,
                paused,
                request.if_none_match("*").send(),
            )
            .await?
        };
        match response {
            Ok(response) => {
                journal.destination =
                    response
                        .copy_object_result()
                        .and_then(|r| r.e_tag())
                        .map(|etag| SourceIdentity {
                            size: journal.source.size,
                            etag: etag.into(),
                            version_id: response
                                .version_id()
                                .filter(|v| *v != "null")
                                .map(str::to_string),
                        });
                journal.metrics.copy_completed_at_ms = Some(chrono::Utc::now().timestamp_millis());
                save(journal).await?;
                return Ok(journal.source.size);
            }
            Err(error) => {
                let reason = storage_error(
                    "CopyObject",
                    error.as_service_error().and_then(|e| e.code()),
                    error.raw_response().map(|r| r.status().as_u16()),
                    &error,
                    true,
                );
                if !reason.starts_with("outcome_unknown:") {
                    journal.stage = "transferring".into();
                    save(journal).await?;
                }
                return Err(reason);
            }
        }
    }

    let persisted = db::get_move_upload_session(&session.id)
        .await
        .map_err(|e| e.to_string())?;
    let geometry = super::stream::MultipartPlan::new(
        dest,
        journal.source.size,
        persisted.as_ref().map(|(_, size)| *size as u64),
    )?;
    let upload_id = if let Some((id, _)) = persisted {
        id
    } else {
        check_control(cancelled, paused)?;
        let response = await_unknown_mutation(
            "CreateMultipartUpload",
            cancelled,
            paused,
            client
                .create_multipart_upload()
                .bucket(dest.bucket())
                .key(&session.dest_key)
                .set_metadata(Some(metadata))
                .set_content_type(source_head.content_type().map(str::to_string))
                .set_cache_control(source_head.cache_control().map(str::to_string))
                .set_content_disposition(source_head.content_disposition().map(str::to_string))
                .set_content_encoding(source_head.content_encoding().map(str::to_string))
                .set_content_language(source_head.content_language().map(str::to_string))
                .set_expires(source_head.expires().cloned())
                .send(),
        )
        .await?
        .map_err(|e| {
            storage_error(
                "CreateMultipartUpload",
                e.as_service_error().and_then(|e| e.code()),
                e.raw_response().map(|r| r.status().as_u16()),
                &e,
                false,
            )
        })?;
        let id = response
            .upload_id()
            .ok_or("Missing multipart upload ID")?
            .to_string();
        if let Err(error) =
            db::save_move_upload_session(&session.id, &id, geometry.part_size as i64).await
        {
            let _ = client
                .abort_multipart_upload()
                .bucket(dest.bucket())
                .key(&session.dest_key)
                .upload_id(&id)
                .send()
                .await;
            return Err(format!("Cannot journal multipart upload: {error}"));
        }
        id
    };

    let local: Vec<PartReceipt> = db::get_move_upload_parts(&session.id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(number, etag, size)| PartReceipt {
            number,
            etag,
            size: size as u64,
        })
        .collect::<Vec<_>>();
    let mut reconciler = PartReconciler::new(journal.source.size, geometry.part_size, local)?;
    let operation_endpoint = dest.operation_endpoint();
    let operation_scope = operation_scope(&session.dest_account_id, &session.dest_bucket);
    let mut marker = None;
    let mut seen = HashSet::new();
    loop {
        check_control(cancelled, paused)?;
        let marker_identity = marker.as_deref().unwrap_or("start");
        let identity = format!("{upload_id}:{marker_identity}:{}", journal.source.etag);
        let context = operation_context(
            OperationKind::ListParts,
            &operation_endpoint,
            &operation_scope,
            &identity,
            cancelled,
            paused,
        );
        let page = execute_operation(&context, || async {
            client
                .list_parts()
                .bucket(dest.bucket())
                .key(&session.dest_key)
                .upload_id(&upload_id)
                .set_part_number_marker(marker.clone())
                .send()
                .await
                .map_err(|error| AttemptError::from_sdk(&error))
        })
        .await
        .map_err(|error| super::planner::operation_error("ListParts", error))?;
        for part in page.parts() {
            reconciler.accept(PartReceipt {
                number: part.part_number().ok_or("Missing copied part number")?,
                etag: part.e_tag().ok_or("Missing copied part ETag")?.to_string(),
                size: part
                    .size()
                    .and_then(|size| u64::try_from(size).ok())
                    .ok_or("Missing copied part size")?,
            })?;
        }
        if !page.is_truncated().unwrap_or(false) {
            break;
        }
        let next = page
            .next_part_number_marker()
            .filter(|s| !s.is_empty())
            .ok_or("Truncated ListParts omitted cursor")?
            .to_string();
        if !seen.insert(next.clone()) {
            return Err("ListParts repeated cursor".into());
        }
        marker = Some(next);
    }
    let mut completed = reconciler.finish();

    let pending: Vec<i32> = (1..=geometry.total_parts)
        .filter(|number| !completed.contains_key(number))
        .collect();
    journal.metrics.copy_requests = journal
        .metrics
        .copy_requests
        .saturating_add(pending.len() as u64);
    journal.metrics.max_copy_in_flight = journal.metrics.max_copy_in_flight.max(
        pending
            .len()
            .min(SERVER_COPY_PART_CONCURRENCY)
            .try_into()
            .unwrap_or(u32::MAX),
    );
    save(journal).await?;
    let source_etag = journal.source.etag.clone();
    let source_size = journal.source.size;
    let client_ref = &client;
    let source_ref = source.as_str();
    let source_etag_ref = source_etag.as_str();
    let upload_id_ref = upload_id.as_str();
    let endpoint_ref = operation_endpoint.as_str();
    let scope_ref = operation_scope.as_str();
    let stopped = AtomicBool::new(false);
    let jobs = stream::iter(pending.into_iter().map(|number| {
        let stopped = &stopped;
        let client = client_ref;
        let source = source_ref;
        let source_etag = source_etag_ref;
        let upload_id = upload_id_ref;
        let endpoint = endpoint_ref;
        let scope = scope_ref;
        async move {
            if stopped.load(Ordering::SeqCst) {
                return Ok(None);
            }
            let result = copy_part(PartCopyJob {
                client,
                dest,
                session,
                source,
                source_etag,
                source_size,
                upload_id,
                part_number: number,
                plan: geometry,
                endpoint,
                scope,
                cancelled,
                paused,
            })
            .await;
            if result.is_err() {
                stopped.store(true, Ordering::SeqCst);
            }
            result.map(Some)
        }
    }))
    .buffer_unordered(SERVER_COPY_PART_CONCURRENCY);
    tokio::pin!(jobs);
    let mut first_error = None;
    let mut copied_bytes: u64 = completed.values().map(|receipt| receipt.size).sum();
    while let Some(result) = jobs.next().await {
        match result {
            Ok(Some((number, etag, size))) => {
                completed.insert(
                    number,
                    PartReceipt {
                        number,
                        etag,
                        size: size as u64,
                    },
                );
                copied_bytes += size as u64;
                let percent = ((copied_bytes as f64 / source_size as f64) * 100.0)
                    .floor()
                    .min(99.0) as i64;
                let _ = db::update_move_progress(&session.id, percent).await;
                if let Some(app) = app {
                    let _ = app.emit(
                        "move-progress",
                        crate::move_transfer::types::MoveProgress {
                            task_id: session.id.clone(),
                            phase: "copying".into(),
                            percent: percent as u32,
                            transferred_bytes: copied_bytes,
                            total_bytes: source_size,
                            speed: 0.0,
                        },
                    );
                }
                info!(
                    "server_copy_part_complete: task={} part={} bytes={} copied={} total={}",
                    session.id, number, size, copied_bytes, source_size
                );
            }
            Ok(None) => {}
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    let receipts = complete_receipts(
        journal.source.size,
        geometry.part_size,
        completed.into_values(),
    )?;
    check_control(cancelled, paused)?;
    journal.stage = "outcome_unknown".into();
    save(journal).await?;
    let parts = receipts
        .into_iter()
        .map(|receipt| {
            CompletedPart::builder()
                .part_number(receipt.number)
                .e_tag(receipt.etag)
                .build()
        })
        .collect();
    check_control(cancelled, paused)?;
    let complete_response = await_unknown_mutation(
        "CompleteMultipartUpload",
        cancelled,
        paused,
        client
            .complete_multipart_upload()
            .if_none_match("*")
            .bucket(dest.bucket())
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
    match complete_response {
        Ok(response) => {
            journal.destination = response.e_tag().map(|etag| SourceIdentity {
                size: journal.source.size,
                etag: etag.into(),
                version_id: response
                    .version_id()
                    .filter(|v| *v != "null")
                    .map(str::to_string),
            });
            journal.metrics.copy_completed_at_ms = Some(chrono::Utc::now().timestamp_millis());
            save(journal).await?;
            Ok(journal.source.size)
        }
        Err(error) => {
            let reason = storage_error(
                "CompleteMultipartUpload",
                error.as_service_error().and_then(|e| e.code()),
                error.raw_response().map(|r| r.status().as_u16()),
                &error,
                true,
            );
            if !reason.starts_with("outcome_unknown:") && !reason.starts_with("not_found:") {
                journal.stage = "transferring".into();
                save(journal).await?;
            }
            Err(reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::move_transfer::{
        planner::TransferPlan,
        stream::{
            protocol::MIB,
            tests::{journal_fixture, test_db_guard},
        },
    };
    use crate::test_s3::{serve, Response};
    use aws_sdk_s3::operation::head_object::HeadObjectOutput;
    use std::sync::Mutex;
    use std::time::Duration;

    fn list_parts_empty() -> &'static str {
        "<ListPartsResult><IsTruncated>false</IsTruncated></ListPartsResult>"
    }

    fn upload_id_xml() -> &'static str {
        "<InitiateMultipartUploadResult><UploadId>server-copy-upload</UploadId></InitiateMultipartUploadResult>"
    }

    fn complete_xml() -> &'static str {
        "<CompleteMultipartUploadResult><ETag>\"destination\"</ETag></CompleteMultipartUploadResult>"
    }

    async fn multipart_fixture(
        name: &str,
        total: u64,
        part_size: i64,
    ) -> (MoveSession, MoveJournal) {
        let (session, mut journal) = journal_fixture(name, total).await;
        journal.stage = "transferring".into();
        save_move_journal(&journal).await.unwrap();
        db::delete_move_upload_parts(&session.id).await.unwrap();
        db::save_move_upload_session(&session.id, "server-copy-upload", part_size)
            .await
            .unwrap();
        (session, journal)
    }

    #[tokio::test]
    async fn multipart_copy_retries_transient_list_and_part_copy_with_same_identity() {
        let _guard = test_db_guard().await;
        let (session, mut journal) =
            multipart_fixture("server-copy-retry", 10 * MIB, 5 * MIB as i64).await;
        journal.source.version_id = Some("version-one".into());
        save_move_journal(&journal).await.unwrap();
        let list_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let copy_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fixture = serve({
            let list_attempts = list_attempts.clone();
            let copy_attempts = copy_attempts.clone();
            move |request| {
                let list_attempts = list_attempts.clone();
                let copy_attempts = copy_attempts.clone();
                async move {
                    if request.method == "GET" {
                        if list_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            return Response::xml(
                                503,
                                "<Error><Code>ServiceUnavailable</Code><Message>try again</Message></Error>",
                            );
                        }
                        return Response::xml(200, list_parts_empty());
                    }
                    if request.method == "PUT" {
                        assert_eq!(
                            request.headers.get("x-amz-copy-source-if-match").map(String::as_str),
                            Some("\"source\"")
                        );
                        assert!(request
                            .headers
                            .get("x-amz-copy-source")
                            .is_some_and(|source| source.contains("versionId=version-one")));
                        assert!(request.headers.contains_key("x-amz-copy-source-range"));
                        if copy_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            return Response::xml(
                                503,
                                "<Error><Code>ServiceUnavailable</Code><Message>try again</Message></Error>",
                            );
                        }
                        return Response::xml(
                            200,
                            "<CopyPartResult><ETag>\"copied-part\"</ETag></CopyPartResult>",
                        );
                    }
                    if request.method == "POST" && request.path.contains("uploadId=") {
                        return Response::xml(200, complete_xml());
                    }
                    Response::xml(200, upload_id_xml())
                }
            }
        })
        .await;
        let config = crate::move_transfer::stream::tests::fixture_config(&fixture.endpoint);

        let copied = copy(
            TransferPlan::MultipartCopy,
            &session,
            &config,
            &HeadObjectOutput::builder().build(),
            &mut journal,
            None,
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();

        assert_eq!(copied, 10 * MIB);
        assert!(list_attempts.load(Ordering::SeqCst) >= 2);
        assert!(copy_attempts.load(Ordering::SeqCst) >= 3);
        let parts = db::get_move_upload_parts(&session.id).await.unwrap();
        assert_eq!(parts.len(), 2);
    }

    #[tokio::test]
    async fn multipart_copy_resumes_only_missing_parts() {
        let _guard = test_db_guard().await;
        let (session, mut journal) =
            multipart_fixture("server-copy-resume-missing", 20 * MIB, 5 * MIB as i64).await;
        db::save_move_upload_part(&session.id, 1, "\"part-1\"", 5 * MIB as i64)
            .await
            .unwrap();
        db::save_move_upload_part(&session.id, 3, "\"part-3\"", 5 * MIB as i64)
            .await
            .unwrap();
        let copied_parts = Arc::new(Mutex::new(Vec::new()));
        let fixture = serve({
            let copied_parts = copied_parts.clone();
            move |request| {
                let copied_parts = copied_parts.clone();
                async move {
                    if request.method == "GET" {
                        return Response::xml(
                            200,
                            "<ListPartsResult><IsTruncated>false</IsTruncated><Part><PartNumber>1</PartNumber><ETag>\"part-1\"</ETag><Size>5242880</Size></Part><Part><PartNumber>3</PartNumber><ETag>\"part-3\"</ETag><Size>5242880</Size></Part></ListPartsResult>",
                        );
                    }
                    if request.method == "PUT" {
                        let part_number = request
                            .path
                            .split("partNumber=")
                            .nth(1)
                            .and_then(|tail| tail.split('&').next())
                            .unwrap()
                            .parse::<i32>()
                            .unwrap();
                        copied_parts.lock().unwrap().push(part_number);
                        return Response::xml(
                            200,
                            &format!(
                                "<CopyPartResult><ETag>\"part-{part_number}\"</ETag></CopyPartResult>"
                            ),
                        );
                    }
                    if request.method == "POST" && request.path.contains("uploadId=") {
                        return Response::xml(200, complete_xml());
                    }
                    Response::xml(200, upload_id_xml())
                }
            }
        })
        .await;
        let config = crate::move_transfer::stream::tests::fixture_config(&fixture.endpoint);

        copy(
            TransferPlan::MultipartCopy,
            &session,
            &config,
            &HeadObjectOutput::builder().build(),
            &mut journal,
            None,
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();

        let mut copied = copied_parts.lock().unwrap().clone();
        copied.sort();
        assert_eq!(copied, [2, 4]);
    }

    #[tokio::test]
    async fn lost_complete_response_preserves_unknown_stage() {
        let _guard = test_db_guard().await;
        let (session, mut journal) =
            multipart_fixture("server-copy-lost-complete", 5 * MIB, 5 * MIB as i64).await;
        let fixture = serve(|request| async move {
            if request.method == "GET" {
                return Response::xml(200, list_parts_empty());
            }
            if request.method == "PUT" {
                return Response::xml(
                    200,
                    "<CopyPartResult><ETag>\"part-1\"</ETag></CopyPartResult>",
                );
            }
            if request.method == "POST" && request.path.contains("uploadId=") {
                return Response::xml(
                    500,
                    "<Error><Code>InternalError</Code><Message>lost</Message></Error>",
                );
            }
            Response::xml(200, upload_id_xml())
        })
        .await;
        let config = crate::move_transfer::stream::tests::fixture_config(&fixture.endpoint);

        let error = copy(
            TransferPlan::MultipartCopy,
            &session,
            &config,
            &HeadObjectOutput::builder().build(),
            &mut journal,
            None,
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_err();

        assert!(error.starts_with("outcome_unknown:"));
        let saved = db::move_sessions::get_move_journal(&session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.stage, "outcome_unknown");
    }

    #[tokio::test]
    async fn cancellation_during_complete_preserves_unknown_stage() {
        let _guard = test_db_guard().await;
        let (session, mut journal) =
            multipart_fixture("server-copy-cancel-complete", 5 * MIB, 5 * MIB as i64).await;
        let cancel = Arc::new(AtomicBool::new(false));
        let fixture = serve({
            let cancel = cancel.clone();
            move |request| {
                let cancel = cancel.clone();
                async move {
                    if request.method == "GET" {
                        return Response::xml(200, list_parts_empty());
                    }
                    if request.method == "PUT" {
                        return Response::xml(
                            200,
                            "<CopyPartResult><ETag>\"part-1\"</ETag></CopyPartResult>",
                        );
                    }
                    if request.method == "POST" && request.path.contains("uploadId=") {
                        cancel.store(true, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        return Response::xml(200, complete_xml());
                    }
                    Response::xml(200, upload_id_xml())
                }
            }
        })
        .await;
        let config = crate::move_transfer::stream::tests::fixture_config(&fixture.endpoint);

        let error = copy(
            TransferPlan::MultipartCopy,
            &session,
            &config,
            &HeadObjectOutput::builder().build(),
            &mut journal,
            None,
            &cancel,
            &Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_err();

        assert!(error.starts_with("outcome_unknown:"));
        let saved = db::move_sessions::get_move_journal(&session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.stage, "outcome_unknown");
        assert!(fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.method == "POST" && request.path.contains("uploadId=")));
    }

    #[tokio::test]
    async fn multipart_copy_runs_parts_concurrently_and_completes_by_part_number() {
        let _guard = test_db_guard().await;
        let (session, mut journal) =
            multipart_fixture("server-copy-parallel", 40 * MIB, 5 * MIB as i64).await;
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let complete_body = Arc::new(Mutex::new(Vec::new()));
        let fixture = serve({
            let active = active.clone();
            let max_active = max_active.clone();
            let complete_body = complete_body.clone();
            move |request| {
                let active = active.clone();
                let max_active = max_active.clone();
                let complete_body = complete_body.clone();
                async move {
                    if request.method == "GET" {
                        return Response::xml(200, list_parts_empty());
                    }
                    if request.method == "PUT" {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(now, Ordering::SeqCst);
                        let part_number = request
                            .path
                            .split("partNumber=")
                            .nth(1)
                            .and_then(|tail| tail.split('&').next())
                            .unwrap()
                            .to_string();
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        return Response::xml(
                            200,
                            &format!(
                                "<CopyPartResult><ETag>\"copied-{part_number}\"</ETag></CopyPartResult>"
                            ),
                        );
                    }
                    if request.method == "POST" && request.path.contains("uploadId=") {
                        *complete_body.lock().unwrap() = request.body;
                        return Response::xml(200, complete_xml());
                    }
                    Response::xml(200, upload_id_xml())
                }
            }
        })
        .await;
        let config = crate::move_transfer::stream::tests::fixture_config(&fixture.endpoint);

        copy(
            TransferPlan::MultipartCopy,
            &session,
            &config,
            &HeadObjectOutput::builder().build(),
            &mut journal,
            None,
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();

        assert!(
            max_active.load(Ordering::SeqCst) > 1,
            "multipart part copy should overlap under bounded concurrency"
        );
        assert!(
            max_active.load(Ordering::SeqCst) <= SERVER_COPY_PART_CONCURRENCY,
            "multipart part copy must respect the concurrency cap"
        );
        let body = String::from_utf8(complete_body.lock().unwrap().clone()).unwrap();
        let positions = [1, 2, 3, 4, 5, 6, 7, 8].map(|part| {
            body.find(&format!("<PartNumber>{part}</PartNumber>"))
                .expect("complete body contains every part")
        });
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[tokio::test]
    async fn a_confirmed_single_copy_is_not_failed_by_a_repeated_capability_probe() {
        let _guard = test_db_guard().await;
        let (session, mut journal) = journal_fixture("single-copy-probe-denied", 8).await;
        // The worker confirmed the conditions before choosing SingleCopy; a
        // probe repeated now (after the capability cache expired) is denied.
        let fixture = serve(|request| async move {
            if request.path.contains("/.r2-operation-checks/") {
                return Response::xml(
                    403,
                    "<Error><Code>AccessDenied</Code><Message>probe denied</Message></Error>",
                );
            }
            assert_eq!(request.method, "PUT");
            assert_eq!(
                request.headers.get("if-none-match").map(String::as_str),
                Some("*")
            );
            assert_eq!(
                request
                    .headers
                    .get("x-amz-copy-source-if-match")
                    .map(String::as_str),
                Some("\"source\"")
            );
            Response::xml(
                200,
                "<CopyObjectResult><ETag>\"destination\"</ETag></CopyObjectResult>",
            )
        })
        .await;
        let config = crate::move_transfer::stream::tests::fixture_config(&fixture.endpoint);

        let copied = copy(
            TransferPlan::SingleCopy,
            &session,
            &config,
            &HeadObjectOutput::builder().build(),
            &mut journal,
            None,
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();

        assert_eq!(copied, 8);
        let saved = db::move_sessions::get_move_journal(&session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.destination.unwrap().etag, "\"destination\"");
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].path.contains("/destination"));
    }
}
