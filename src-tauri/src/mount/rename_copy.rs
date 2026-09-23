//! Resumable large NFS rename copies; relay bodies share Move's byte budget.
use super::*;
use crate::move_transfer::server_copy::SERVER_COPY_OPERATION_BUDGET;
use crate::move_transfer::stream::{
    data_timeouts,
    protocol::{attempt_timeout, operation_budget, Payload, RelayBudget, MAX_ATTEMPTS},
    shared_http_client, MultipartPlan,
};
use crate::providers::{
    conditional::Condition,
    multipart::{complete_receipts, PartReceipt, PartReconciler},
};
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
        let request = self
            .inner
            .client
            .head_object()
            .bucket(&self.inner.bucket)
            .key(&object.from);
        let scope = self.storage_scope(&object.from);
        let context =
            self.read_operation_context(OperationKind::Head, &scope, "", Duration::from_secs(30));
        let head = execute_storage_operation(&context, || {
            let request = request.clone();
            async move {
                request
                    .send()
                    .await
                    .map_err(|error| AttemptError::from_sdk(&error))
            }
        })
        .await
        .map_err(|error| self.map_operation_error("HEAD", error))?;
        if head.e_tag() != Some(object.source_etag.as_str())
            || head.content_length() != Some(object.size as i64)
        {
            return Err(nfsstat3::NFS3ERR_IO);
        }
        if journal.upload_id.is_some() {
            let mut marker: Option<String> = None;
            let mut seen = HashSet::new();
            let local = journal
                .parts
                .iter()
                .map(|(number, etag)| {
                    Self::rename_part_receipt(&plan, object.size, *number, etag.clone())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut reconciler =
                PartReconciler::new(object.size, plan.part_size, local).map_err(|message| {
                    log::error!("mount: invalid rename multipart journal: {message}");
                    nfsstat3::NFS3ERR_IO
                })?;
            loop {
                let upload_id = journal
                    .upload_id
                    .as_deref()
                    .ok_or(nfsstat3::NFS3ERR_IO)?
                    .to_string();
                let request = self
                    .inner
                    .client
                    .list_parts()
                    .bucket(&self.inner.bucket)
                    .key(&object.to)
                    .upload_id(upload_id)
                    .set_part_number_marker(marker.clone());
                let scope = self.storage_scope(&object.to);
                let context = self.read_operation_context(
                    OperationKind::ListParts,
                    &scope,
                    "",
                    Duration::from_secs(30),
                );
                let page = execute_storage_operation(&context, || {
                    let request = request.clone();
                    async move {
                        request
                            .send()
                            .await
                            .map_err(|error| AttemptError::from_sdk(&error))
                    }
                })
                .await;
                let page = match page {
                    Ok(page) => page,
                    Err(error) if matches!(error.class(), StorageErrorClass::NotFound) => {
                        journal.upload_id = None;
                        journal.parts.clear();
                        journal.completing = false;
                        break;
                    }
                    Err(error) => return Err(self.map_operation_error("ListParts", error)),
                };
                for part in page.parts() {
                    let number = part.part_number().ok_or(nfsstat3::NFS3ERR_IO)?;
                    let size = part
                        .size()
                        .filter(|size| *size >= 0)
                        .ok_or(nfsstat3::NFS3ERR_IO)? as u64;
                    let receipt = Self::rename_part_receipt(
                        &plan,
                        object.size,
                        number,
                        part.e_tag().ok_or(nfsstat3::NFS3ERR_IO)?.to_string(),
                    )?;
                    if size != receipt.size {
                        return Err(nfsstat3::NFS3ERR_IO);
                    }
                    reconciler.accept(receipt).map_err(|message| {
                        log::error!("mount: invalid rename ListParts receipt: {message}");
                        nfsstat3::NFS3ERR_IO
                    })?;
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
                journal.parts = reconciler
                    .finish()
                    .into_iter()
                    .map(|(number, part)| (number, part.etag))
                    .collect();
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
        let pending = (1..=plan.total_parts)
            .filter(|number| !journal.parts.contains_key(number))
            .collect::<Vec<_>>();
        let stopped = AtomicBool::new(false);
        let jobs = stream::iter(pending.into_iter().map(|number| {
            let stopped = &stopped;
            let upload_id = &upload_id;
            let source = &source;
            async move {
                if stopped.load(Ordering::SeqCst) {
                    return Ok(None);
                }
                let result = self
                    .copy_rename_part(object, upload_id, source, server_parts, number, &plan)
                    .await;
                if result.is_err() {
                    stopped.store(true, Ordering::SeqCst);
                }
                result.map(Some)
            }
        }))
        .buffer_unordered(4);
        tokio::pin!(jobs);
        let mut first_error = None;
        while let Some(result) = jobs.next().await {
            match result {
                Ok(Some((number, etag))) => {
                    journal.parts.insert(number, etag);
                    stage::write_json_atomic(&path, &journal)
                        .await
                        .map_err(|_| nfsstat3::NFS3ERR_IO)?;
                }
                Ok(None) => {}
                Err(status) if first_error.is_none() => {
                    first_error = Some(status);
                }
                Err(_) => {}
            }
        }
        if let Some(status) = first_error {
            return Err(status);
        }
        journal.completing = true;
        stage::write_json_atomic(&path, &journal)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let receipts = journal
            .parts
            .iter()
            .map(|(number, etag)| {
                Self::rename_part_receipt(&plan, object.size, *number, etag.clone())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let parts = complete_receipts(object.size, plan.part_size, receipts)
            .map_err(|message| {
                log::error!("mount: incomplete rename multipart completion: {message}");
                nfsstat3::NFS3ERR_IO
            })?
            .into_iter()
            .map(|part| {
                CompletedPart::builder()
                    .part_number(part.number)
                    .e_tag(part.etag)
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

    fn rename_part_receipt(
        plan: &MultipartPlan,
        total: u64,
        number: i32,
        etag: String,
    ) -> Result<PartReceipt, nfsstat3> {
        if !(1..=plan.total_parts).contains(&number) {
            return Err(nfsstat3::NFS3ERR_IO);
        }
        let (start, end) = plan.range(number, total);
        Ok(PartReceipt {
            number,
            etag,
            size: end - start + 1,
        })
    }

    pub(super) async fn copy_rename_part(
        &self,
        object: &RenameObject,
        upload_id: &str,
        source: &str,
        server_parts: bool,
        number: i32,
        plan: &MultipartPlan,
    ) -> Result<(i32, String), nfsstat3> {
        let (start, end) = plan.range(number, object.size);
        let length = end - start + 1;
        if !server_parts {
            let payload = self.rename_range(object, start, end).await?;
            let scope = self.storage_scope(&object.to);
            let identity = format!(
                "{}:{}:{}:{start}-{end}:{upload_id}:{number}",
                object.from,
                object.source_etag,
                object.source_version.as_deref().unwrap_or_default()
            );
            // Room for every attempt; data_timeouts bounds each one.
            let context = self
                .read_operation_context(
                    OperationKind::UploadPart,
                    &scope,
                    &identity,
                    operation_budget(length),
                )
                .with_max_attempts(MAX_ATTEMPTS as u32);
            let etag = execute_storage_operation(&context, || async {
                let body = payload
                    .body
                    .try_clone()
                    .ok_or_else(|| AttemptError::permanent("Relay payload is not replayable"))?;
                let response = self
                    .inner
                    .client
                    .upload_part()
                    .bucket(&self.inner.bucket)
                    .key(&object.to)
                    .upload_id(upload_id)
                    .part_number(number)
                    .content_length(length as i64)
                    .body(ByteStream::new(body))
                    .customize()
                    .config_override(data_timeouts(length))
                    .send()
                    .await
                    .map_err(|error| AttemptError::from_sdk(&error))?;
                response
                    .e_tag()
                    .filter(|etag| !etag.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| AttemptError::permanent("UploadPart returned no ETag"))
            })
            .await
            .map_err(|error| self.map_operation_error("UploadPart", error))?;
            return Ok((number, etag));
        }

        let request = self
            .inner
            .client
            .upload_part_copy()
            .bucket(&self.inner.bucket)
            .key(&object.to)
            .upload_id(upload_id)
            .part_number(number)
            .copy_source(source)
            .copy_source_range(format!("bytes={start}-{end}"));
        let request = if object.source_version.is_some() {
            request
        } else {
            request.copy_source_if_match(&object.source_etag)
        };
        let scope = self.storage_scope(&object.to);
        let identity = format!(
            "{}:{}:{}:{start}-{end}:{}:{upload_id}:{number}",
            object.from,
            object.source_etag,
            object.source_version.as_deref().unwrap_or_default(),
            object.to
        );
        // Each attempt is bounded by the client's own operation timeout.
        let context = self.read_operation_context(
            OperationKind::UploadPartCopy,
            &scope,
            &identity,
            SERVER_COPY_OPERATION_BUDGET,
        );
        let etag = execute_storage_operation(&context, || {
            let request = request.clone();
            async move {
                let response = request
                    .send()
                    .await
                    .map_err(|error| AttemptError::from_sdk(&error))?;
                response
                    .copy_part_result()
                    .and_then(|part| part.e_tag())
                    .filter(|etag| !etag.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| AttemptError::permanent("UploadPartCopy returned no ETag"))
            }
        })
        .await
        .map_err(|error| self.map_operation_error("UploadPartCopy", error))?;
        Ok((number, etag))
    }

    async fn rename_range(
        &self,
        object: &RenameObject,
        start: u64,
        end: u64,
    ) -> Result<Payload, nfsstat3> {
        let http = shared_http_client().map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let paused = AtomicBool::new(false);
        let length = end - start + 1;
        // Relay memory before a request slot, as for Move parts (see RelayBudget).
        let reservation = RelayBudget::shared()
            .reserve(
                Some((start, end)),
                object.size,
                &self.inner.aborted,
                &paused,
            )
            .await
            .map_err(|error| {
                self.io_failed(format!("GET: {}", error.message));
                nfsstat3::NFS3ERR_IO
            })?;
        let scope = self.storage_scope(&object.from);
        let identity = format!(
            "{}:{}:{}:{start}-{end}",
            object.from,
            object.source_etag,
            object.source_version.as_deref().unwrap_or_default()
        );
        // Room for every attempt; the timeout around each read bounds it.
        let context = self
            .read_operation_context(
                OperationKind::Get,
                &scope,
                &identity,
                operation_budget(length),
            )
            .with_pause(&paused)
            .with_max_attempts(MAX_ATTEMPTS as u32);
        execute_storage_operation(&context, || async {
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
                        .map_err(|error| AttemptError::permanent(error.to_string()))?,
                )
                .await
                .map_err(|error| {
                    AttemptError::permanent(format!("Cannot sign source read: {error}"))
                })?;
            match tokio::time::timeout(
                attempt_timeout(length),
                reservation.fetch(
                    &http,
                    signed.uri(),
                    &object.source_etag,
                    &self.inner.aborted,
                    &paused,
                ),
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
                Err(_) => Err(AttemptError::transient("Source response body timed out")),
            }
        })
        .await
        .map(|fetched| reservation.into_payload(fetched))
        .map_err(|error| self.map_operation_error("GET", error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_s3::{serve, Response};
    use std::sync::atomic::AtomicUsize;

    fn filesystem(client: Client, endpoint: String) -> S3NfsFs {
        S3NfsFs::new_with_endpoint(
            client,
            "photos".into(),
            endpoint,
            false,
            std::env::temp_dir().join(format!(
                "r2-rename-copy-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            )),
        )
    }

    fn object(size: u64) -> RenameObject {
        RenameObject {
            from: "source".into(),
            to: "target".into(),
            source_etag: "\"v1\"".into(),
            size,
            destination_etag: None,
            phase: "pending".into(),
            replaced_etag: None,
            source_version: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_relay_read_stalled_past_one_attempt_timeout_still_gets_its_retry() {
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
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let client = crate::providers::s3_client::create_s3_client(
            &crate::providers::s3_client::S3ClientConfig {
                access_key_id: "fixture",
                secret_access_key: "fixture-secret",
                region: "us-east-1",
                endpoint_url: Some(&endpoint),
                force_path_style: true,
            },
        )
        .unwrap();
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
        let fs = filesystem(client, format!("rename-read-stall:{endpoint}"));
        let start = tokio::time::Instant::now();
        let payload = fs.rename_range(&object(4), 2, 3).await.unwrap();
        assert_eq!(payload.len, 2);
        assert!(start.elapsed() >= attempt_timeout(2));
        server.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_relay_upload_part_stalled_past_one_attempt_timeout_still_gets_its_retry() {
        let puts = Arc::new(AtomicUsize::new(0));
        let fixture = serve({
            let puts = puts.clone();
            move |request| {
                let puts = puts.clone();
                async move {
                    if request.method == "GET" {
                        return Response::xml(206, "original")
                            .header("etag", "\"v1\"")
                            .header("content-range", "bytes 0-7/8");
                    }
                    if puts.fetch_add(1, Ordering::SeqCst) == 0 {
                        // Answers after attempt_timeout(8) = 31 s has passed.
                        tokio::time::sleep(Duration::from_secs(40)).await;
                    }
                    Response::empty(200).header("etag", "\"relay-part\"")
                }
            }
        })
        .await;
        let fs = filesystem(
            fixture.client.clone(),
            format!("rename-upload-stall:{}", fixture.endpoint),
        );
        let plan = MultipartPlan {
            part_size: 5 * 1024 * 1024,
            total_parts: 1,
        };
        let start = tokio::time::Instant::now();
        assert_eq!(
            fs.copy_rename_part(&object(8), "upload", "", false, 1, &plan)
                .await
                .unwrap(),
            (1, "\"relay-part\"".into())
        );
        assert_eq!(puts.load(Ordering::SeqCst), 2);
        assert!(start.elapsed() >= attempt_timeout(8));
    }
}
