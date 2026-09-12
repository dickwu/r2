//! Resumable large NFS rename copies; relay bodies share Move's byte budget.
use super::*;
use crate::move_transfer::stream::{
    data_timeouts,
    protocol::{attempt_timeout, fetch_payload, interruptible, retry_delay, Payload, MAX_ATTEMPTS},
    shared_http_client, MultipartPlan,
};
use crate::providers::{conditional::Condition, s3_client::is_transient_s3_error};
use aws_sdk_s3::presigning::PresigningConfig;
use std::collections::HashSet;

impl S3NfsFs {
    pub(super) async fn copy_large_object(
        &self,
        object: &RenameObject,
        token: &str,
    ) -> Result<String, nfsstat3> {
        let config = self
            .inner
            .transfer_config
            .get()
            .ok_or(nfsstat3::NFS3ERR_NOTSUPP)?;
        let condition = if object.replaced_etag.is_some() {
            Condition::CompleteMatch
        } else {
            Condition::CompleteCreate
        };
        if !self.condition_supported(condition).await? {
            return Err(nfsstat3::NFS3ERR_NOTSUPP);
        }
        let server_parts = object.source_version.is_some()
            || self.condition_supported(Condition::PartCopySource).await?;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        object.to.hash(&mut hash);
        let path = self
            .inner
            .staging_root
            .join(format!("rename-part-{token}-{:016x}.json", hash.finish()));
        let mut journal: stage::MultipartJournal = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| nfsstat3::NFS3ERR_IO)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(_) => return Err(nfsstat3::NFS3ERR_IO),
        };
        let plan = MultipartPlan::new(
            config,
            object.size,
            (journal.part_size > 0).then_some(journal.part_size),
        )
        .map_err(|_| nfsstat3::NFS3ERR_FBIG)?;
        journal.part_size = plan.part_size;
        if journal.precondition.is_none() {
            if journal.completing {
                return Err(nfsstat3::NFS3ERR_IO);
            }
            journal.precondition = Some(match &object.replaced_etag {
                Some(etag) => stage::PublicationGuard::Match { etag: etag.clone() },
                None => stage::PublicationGuard::Absent,
            });
        }
        let guard = journal.precondition.clone().ok_or(nfsstat3::NFS3ERR_IO)?;
        if !guard.is_valid() {
            return Err(nfsstat3::NFS3ERR_IO);
        }
        if journal.completing {
            let current = self.object_head(&object.to).await?;
            let etag = current
                .as_ref()
                .map(|head| {
                    head.e_tag()
                        .filter(|etag| !etag.is_empty())
                        .ok_or(nfsstat3::NFS3ERR_IO)
                })
                .transpose()?;
            if !guard.matches(etag) {
                return Err(nfsstat3::NFS3ERR_IO);
            }
        }
        stage::write_json_atomic(&path, &journal)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let head = self
            .inner
            .client
            .head_object()
            .bucket(&self.inner.bucket)
            .key(&object.from)
            .send()
            .await
            .map_err(|e| map_s3_error(&e))?;
        if head.e_tag() != Some(object.source_etag.as_str())
            || head.content_length() != Some(object.size as i64)
        {
            return Err(nfsstat3::NFS3ERR_IO);
        }
        if journal.upload_id.is_some() {
            let mut marker: Option<String> = None;
            let mut seen = HashSet::new();
            let mut parts = BTreeMap::new();
            loop {
                let page = self
                    .inner
                    .client
                    .list_parts()
                    .bucket(&self.inner.bucket)
                    .key(&object.to)
                    .upload_id(journal.upload_id.as_deref().ok_or(nfsstat3::NFS3ERR_IO)?)
                    .set_part_number_marker(marker.clone())
                    .send()
                    .await;
                let page = match page {
                    Ok(page) => page,
                    Err(error)
                        if error.as_service_error().and_then(|e| e.code())
                            == Some("NoSuchUpload") =>
                    {
                        journal.upload_id = None;
                        journal.parts.clear();
                        journal.completing = false;
                        break;
                    }
                    Err(error) => return Err(map_s3_error(&error)),
                };
                for part in page.parts() {
                    let number = part.part_number().ok_or(nfsstat3::NFS3ERR_IO)?;
                    if number < 1 || number > plan.total_parts {
                        return Err(nfsstat3::NFS3ERR_IO);
                    }
                    let (start, end) = plan.range(number, object.size);
                    if part.size() != Some((end - start + 1) as i64)
                        || parts
                            .insert(
                                number,
                                part.e_tag().ok_or(nfsstat3::NFS3ERR_IO)?.to_string(),
                            )
                            .is_some()
                    {
                        return Err(nfsstat3::NFS3ERR_IO);
                    }
                }
                if !page.is_truncated().unwrap_or(false) {
                    break;
                }
                let next = page
                    .next_part_number_marker()
                    .filter(|v| !v.is_empty())
                    .ok_or(nfsstat3::NFS3ERR_IO)?
                    .to_string();
                if !seen.insert(next.clone()) {
                    return Err(nfsstat3::NFS3ERR_IO);
                }
                marker = Some(next);
            }
            if journal.upload_id.is_some() {
                journal.parts = parts;
            }
            stage::write_json_atomic(&path, &journal)
                .await
                .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        }
        if journal.upload_id.is_none() {
            let mut metadata = head.metadata().cloned().unwrap_or_default();
            metadata.insert("r2-rename-operation".into(), token.into());
            let created = self
                .inner
                .client
                .create_multipart_upload()
                .bucket(&self.inner.bucket)
                .key(&object.to)
                .set_metadata(Some(metadata))
                .set_content_type(head.content_type().map(str::to_string))
                .set_content_encoding(head.content_encoding().map(str::to_string))
                .set_cache_control(head.cache_control().map(str::to_string))
                .set_content_disposition(head.content_disposition().map(str::to_string))
                .set_content_language(head.content_language().map(str::to_string))
                .send()
                .await
                .map_err(|e| map_s3_error(&e))?;
            journal.upload_id = Some(created.upload_id().ok_or(nfsstat3::NFS3ERR_IO)?.into());
            stage::write_json_atomic(&path, &journal)
                .await
                .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        }
        let upload_id = journal.upload_id.clone().ok_or(nfsstat3::NFS3ERR_IO)?;
        let mut source = encode_copy_source(&self.inner.bucket, &object.from);
        if let Some(version) = &object.source_version {
            source.push_str(&format!("?versionId={}", urlencoding::encode(version)));
        }
        let paused = AtomicBool::new(false);
        for number in 1..=plan.total_parts {
            if journal.parts.contains_key(&number) {
                continue;
            }
            let (start, end) = plan.range(number, object.size);
            let length = end - start + 1;
            let payload = if server_parts {
                None
            } else {
                Some(self.rename_range(object, start, end).await?)
            };
            let mut etag = None;
            for attempt in 0..MAX_ATTEMPTS {
                let result = if let Some(payload) = &payload {
                    let body =
                        ByteStream::new(payload.body.try_clone().ok_or(nfsstat3::NFS3ERR_IO)?);
                    interruptible(
                        &self.inner.shutdown,
                        &paused,
                        self.inner
                            .client
                            .upload_part()
                            .bucket(&self.inner.bucket)
                            .key(&object.to)
                            .upload_id(&upload_id)
                            .part_number(number)
                            .content_length(length as i64)
                            .body(body)
                            .customize()
                            .config_override(data_timeouts(length))
                            .send(),
                    )
                    .await
                    .map_err(|_| nfsstat3::NFS3ERR_IO)?
                    .map(|r| r.e_tag().map(str::to_string))
                    .map_err(|e| (is_transient_s3_error(&e), map_s3_error(&e)))
                } else {
                    let request = self
                        .inner
                        .client
                        .upload_part_copy()
                        .bucket(&self.inner.bucket)
                        .key(&object.to)
                        .upload_id(&upload_id)
                        .part_number(number)
                        .copy_source(&source)
                        .copy_source_range(format!("bytes={start}-{end}"));
                    let request = if object.source_version.is_some() {
                        request
                    } else {
                        request.copy_source_if_match(&object.source_etag)
                    };
                    interruptible(&self.inner.shutdown, &paused, request.send())
                        .await
                        .map_err(|_| nfsstat3::NFS3ERR_IO)?
                        .map(|r| {
                            r.copy_part_result()
                                .and_then(|p| p.e_tag())
                                .map(str::to_string)
                        })
                        .map_err(|e| (is_transient_s3_error(&e), map_s3_error(&e)))
                };
                match result {
                    Ok(value) => {
                        etag = value;
                        break;
                    }
                    Err((true, _)) if attempt + 1 < MAX_ATTEMPTS => {
                        retry_delay(attempt, None, &self.inner.shutdown, &paused)
                            .await
                            .map_err(|_| nfsstat3::NFS3ERR_IO)?
                    }
                    Err((_, status)) => return Err(status),
                }
            }
            journal
                .parts
                .insert(number, etag.ok_or(nfsstat3::NFS3ERR_IO)?);
            stage::write_json_atomic(&path, &journal)
                .await
                .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        }
        journal.completing = true;
        stage::write_json_atomic(&path, &journal)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let parts = journal
            .parts
            .iter()
            .map(|(number, etag)| {
                CompletedPart::builder()
                    .part_number(*number)
                    .e_tag(etag)
                    .build()
            })
            .collect();
        let request = self
            .inner
            .client
            .complete_multipart_upload()
            .bucket(&self.inner.bucket)
            .key(&object.to)
            .upload_id(&upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            );
        let request = match guard {
            stage::PublicationGuard::Absent => request.if_none_match("*"),
            stage::PublicationGuard::Match { etag } => request.if_match(etag),
        };
        match request.send().await {
            Ok(output) => {
                let etag = output.e_tag().ok_or(nfsstat3::NFS3ERR_IO)?.to_string();
                let _ = tokio::fs::remove_file(path).await;
                Ok(etag)
            }
            Err(error) => {
                if !upload_error(&error).uncertain {
                    journal.completing = false;
                    stage::write_json_atomic(&path, &journal)
                        .await
                        .map_err(|_| nfsstat3::NFS3ERR_IO)?;
                }
                Err(map_s3_error(&error))
            }
        }
    }

    async fn rename_range(
        &self,
        object: &RenameObject,
        start: u64,
        end: u64,
    ) -> Result<Payload, nfsstat3> {
        let http = shared_http_client().map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let paused = AtomicBool::new(false);
        for attempt in 0..MAX_ATTEMPTS {
            let signed = self
                .inner
                .client
                .get_object()
                .bucket(&self.inner.bucket)
                .key(&object.from)
                .if_match(&object.source_etag)
                .set_version_id(object.source_version.clone())
                .presigned(
                    PresigningConfig::expires_in(Duration::from_secs(900))
                        .map_err(|_| nfsstat3::NFS3ERR_IO)?,
                )
                .await
                .map_err(|_| nfsstat3::NFS3ERR_IO)?;
            let result = tokio::time::timeout(
                attempt_timeout(end - start + 1),
                fetch_payload(
                    &http,
                    signed.uri(),
                    Some((start, end)),
                    object.size,
                    &object.source_etag,
                    &self.inner.shutdown,
                    &paused,
                ),
            )
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
            match result {
                Ok(payload) => return Ok(payload),
                Err(error) if error.retryable && attempt + 1 < MAX_ATTEMPTS => {
                    retry_delay(attempt, error.retry_after, &self.inner.shutdown, &paused)
                        .await
                        .map_err(|_| nfsstat3::NFS3ERR_IO)?
                }
                Err(_) => return Err(nfsstat3::NFS3ERR_IO),
            }
        }
        Err(nfsstat3::NFS3ERR_IO)
    }
}
