//! Conditional server-side copy with part-level recovery for native AWS S3.
use super::config::MoveConfig;
use super::planner::{
    encoded_copy_source, native_aws, storage_error, TransferPlan, TRANSFER_MARKER,
};
use crate::db::{
    self,
    move_sessions::{save_move_journal, MoveJournal, SourceIdentity},
    MoveSession,
};
use aws_sdk_s3::{
    error::ProvideErrorMetadata,
    types::{CompletedMultipartUpload, CompletedPart, MetadataDirective},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

fn check_control(cancelled: &AtomicBool, paused: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::SeqCst) {
        return Err("cancelled: Move cancelled; source retained".into());
    }
    if paused.load(Ordering::SeqCst) {
        return Err("paused: Move paused; source retained".into());
    }
    Ok(())
}

async fn save(journal: &MoveJournal) -> Result<(), String> {
    save_move_journal(journal)
        .await
        .map_err(|e| format!("Cannot persist move recovery state: {e}"))
}

#[allow(deprecated)] // AWS SDK has not exposed a string setter for outgoing Expires.
pub(crate) async fn copy(
    plan: TransferPlan,
    session: &MoveSession,
    dest: &MoveConfig,
    source_head: &aws_sdk_s3::operation::head_object::HeadObjectOutput,
    journal: &mut MoveJournal,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
) -> Result<u64, String> {
    check_control(cancelled, paused)?;
    let client = dest.client().await?;
    let source = encoded_copy_source(
        &session.source_bucket,
        &session.source_key,
        journal.source.version_id.as_deref(),
    );
    let mut metadata = source_head.metadata().cloned().unwrap_or_default();
    metadata.insert(TRANSFER_MARKER.into(), session.id.clone());
    if plan == TransferPlan::SingleCopy {
        if !native_aws(dest)
            && !matches!(dest, MoveConfig::R2(_))
            && (!dest
                .supports_condition(crate::providers::conditional::Condition::CopyCreate)
                .await?
                || !dest
                    .supports_condition(crate::providers::conditional::Condition::CopySource)
                    .await?)
        {
            return Err("needs_action: Conditional destination copy has not been verified for this endpoint; destination and source retained".into());
        }
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
        let response = if matches!(dest, MoveConfig::R2(_)) {
            request
                .customize()
                .mutate_request(|request| {
                    request
                        .headers_mut()
                        .insert("cf-copy-destination-if-none-match", "*");
                })
                .send()
                .await
        } else {
            request.if_none_match("*").send().await
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
        let response = client
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
            .send()
            .await
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

    let local: HashMap<i32, (String, i64)> = db::get_move_upload_parts(&session.id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(n, etag, size)| (n, (etag, size)))
        .collect();
    let mut completed = BTreeMap::new();
    let mut marker = None;
    let mut seen = HashSet::new();
    loop {
        check_control(cancelled, paused)?;
        let page = client
            .list_parts()
            .bucket(dest.bucket())
            .key(&session.dest_key)
            .upload_id(&upload_id)
            .set_part_number_marker(marker.clone())
            .send()
            .await
            .map_err(|e| {
                storage_error(
                    "ListParts",
                    e.as_service_error().and_then(|e| e.code()),
                    e.raw_response().map(|r| r.status().as_u16()),
                    &e,
                    false,
                )
            })?;
        for part in page.parts() {
            let number = part.part_number().ok_or("Missing copied part number")?;
            if number < 1 || number > geometry.total_parts {
                return Err("conflict: Unexpected copied part number".into());
            }
            let (start, end) = geometry.range(number, journal.source.size);
            let size = (end - start + 1) as i64;
            let etag = part.e_tag().ok_or("Missing copied part ETag")?;
            // A remote part whose response was lost can be safely recopied with
            // the same source condition. Only trust parts also in our journal.
            if part.size() == Some(size)
                && local
                    .get(&number)
                    .is_some_and(|(e, s)| e == etag && *s == size)
            {
                completed.insert(number, etag.to_string());
            }
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

    for number in 1..=geometry.total_parts {
        check_control(cancelled, paused)?;
        if completed.contains_key(&number) {
            continue;
        }
        let (start, end) = geometry.range(number, journal.source.size);
        let response = client
            .upload_part_copy()
            .bucket(dest.bucket())
            .key(&session.dest_key)
            .upload_id(&upload_id)
            .part_number(number)
            .copy_source(&source)
            .copy_source_if_match(&journal.source.etag)
            .copy_source_range(format!("bytes={start}-{end}"))
            .send()
            .await
            .map_err(|e| {
                storage_error(
                    "UploadPartCopy",
                    e.as_service_error().and_then(|e| e.code()),
                    e.raw_response().map(|r| r.status().as_u16()),
                    &e,
                    false,
                )
            })?;
        let etag = response
            .copy_part_result()
            .and_then(|r| r.e_tag())
            .ok_or("Missing copied part ETag")?
            .to_string();
        db::save_move_upload_part(&session.id, number, &etag, (end - start + 1) as i64)
            .await
            .map_err(|e| format!("Cannot journal copied part: {e}"))?;
        completed.insert(number, etag);
    }
    check_control(cancelled, paused)?;
    journal.stage = "outcome_unknown".into();
    save(journal).await?;
    let parts = completed
        .into_iter()
        .map(|(number, etag)| {
            CompletedPart::builder()
                .part_number(number)
                .e_tag(etag)
                .build()
        })
        .collect();
    match client
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
        .send()
        .await
    {
        Ok(response) => {
            journal.destination = response.e_tag().map(|etag| SourceIdentity {
                size: journal.source.size,
                etag: etag.into(),
                version_id: response
                    .version_id()
                    .filter(|v| *v != "null")
                    .map(str::to_string),
            });
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
