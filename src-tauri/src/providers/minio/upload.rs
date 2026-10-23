use super::types::{create_minio_client, MinioConfig, MinioResult};
use crate::upload_control::{
    read_file_range, sdk_attempt_failure, sdk_upload_timeouts, AttemptFailure, UploadHooks,
    CANCELLED_MESSAGE,
};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

pub async fn upload_content(
    config: &MinioConfig,
    key: &str,
    content: Vec<u8>,
    content_type: Option<&str>,
) -> MinioResult<String> {
    let client = create_minio_client(config).await?;
    let body = ByteStream::from(content);

    let mut request = client
        .put_object()
        .bucket(&config.bucket)
        .key(key)
        .body(body);

    if let Some(ct) = content_type {
        request = request.content_type(ct);
    }

    let response = request.send().await?;
    let etag = response.e_tag().unwrap_or_default().to_string();

    Ok(etag)
}

/// PUT a whole file, retrying transient failures (see [`UploadHooks::retry`]);
/// each attempt reopens the file and runs under size-scaled timeouts.
pub async fn upload_file_simple(
    config: &MinioConfig,
    key: &str,
    file_path: &Path,
    content_type: Option<&str>,
    hooks: &UploadHooks,
) -> MinioResult<String> {
    let client = &create_minio_client(config).await?;
    let file_size = tokio::fs::metadata(file_path).await?.len();

    let response = hooks
        .retry(|_| async move {
            let body = ByteStream::from_path(file_path)
                .await
                .map_err(|e| AttemptFailure::permanent(format!("Failed to read file: {}", e)))?;
            client
                .put_object()
                .bucket(&config.bucket)
                .key(key)
                .set_content_type(content_type.map(str::to_owned))
                .body(body)
                .customize()
                .config_override(sdk_upload_timeouts(file_size))
                .send()
                .await
                .map_err(|e| sdk_attempt_failure(&e))
        })
        .await?;
    let etag = response.e_tag().unwrap_or_default().to_string();

    Ok(etag)
}

pub async fn initiate_multipart_upload(
    config: &MinioConfig,
    key: &str,
    content_type: Option<&str>,
    hooks: &UploadHooks,
) -> MinioResult<String> {
    let client = &create_minio_client(config).await?;

    let response = hooks
        .retry(|_| async move {
            client
                .create_multipart_upload()
                .bucket(&config.bucket)
                .key(key)
                .set_content_type(content_type.map(str::to_owned))
                .send()
                .await
                .map_err(|e| sdk_attempt_failure(&e))
        })
        .await?;
    let upload_id = response
        .upload_id()
        .ok_or("No upload ID returned")?
        .to_string();

    Ok(upload_id)
}

/// Upload `len` bytes of the file at `offset` as one part, retrying transient
/// failures; each attempt re-reads the part under size-scaled timeouts.
#[allow(clippy::too_many_arguments)]
pub async fn upload_part(
    config: &MinioConfig,
    key: &str,
    upload_id: &str,
    part_number: i32,
    file_path: &Path,
    offset: u64,
    len: u64,
    hooks: &UploadHooks,
) -> MinioResult<String> {
    let client = &create_minio_client(config).await?;

    let response = hooks
        .retry(|_| async move {
            let data = read_file_range(file_path, offset, len)
                .await
                .map_err(AttemptFailure::permanent)?;
            client
                .upload_part()
                .bucket(&config.bucket)
                .key(key)
                .upload_id(upload_id)
                .part_number(part_number)
                .body(ByteStream::from(data))
                .customize()
                .config_override(sdk_upload_timeouts(len))
                .send()
                .await
                .map_err(|e| sdk_attempt_failure(&e))
        })
        .await?;

    let etag = response.e_tag().unwrap_or_default().to_string();
    Ok(etag)
}

#[allow(dead_code)]
pub async fn complete_multipart_upload(
    config: &MinioConfig,
    key: &str,
    upload_id: &str,
    parts: Vec<(i32, String)>,
) -> MinioResult<()> {
    let client = create_minio_client(config).await?;

    let completed_parts: Vec<CompletedPart> = parts
        .into_iter()
        .map(|(part_number, etag)| {
            CompletedPart::builder()
                .part_number(part_number)
                .e_tag(etag)
                .build()
        })
        .collect();

    let completed_upload = CompletedMultipartUpload::builder()
        .set_parts(Some(completed_parts))
        .build();

    client
        .complete_multipart_upload()
        .bucket(&config.bucket)
        .key(key)
        .upload_id(upload_id)
        .multipart_upload(completed_upload)
        .send()
        .await?;

    Ok(())
}

#[allow(dead_code)]
pub async fn abort_multipart_upload(
    config: &MinioConfig,
    key: &str,
    upload_id: &str,
) -> MinioResult<()> {
    let client = create_minio_client(config).await?;

    client
        .abort_multipart_upload()
        .bucket(&config.bucket)
        .key(key)
        .upload_id(upload_id)
        .send()
        .await?;

    Ok(())
}

#[allow(dead_code)]
pub async fn list_parts(
    config: &MinioConfig,
    key: &str,
    upload_id: &str,
) -> MinioResult<Vec<(i32, String)>> {
    let client = create_minio_client(config).await?;

    let response = client
        .list_parts()
        .bucket(&config.bucket)
        .key(key)
        .upload_id(upload_id)
        .send()
        .await?;

    let parts = response
        .parts()
        .iter()
        .filter_map(|part| {
            let part_number = part.part_number()?;
            let etag = part.e_tag()?.to_string();
            Some((part_number, etag))
        })
        .collect();

    Ok(parts)
}

#[allow(clippy::too_many_arguments)]
pub async fn upload_file_multipart(
    config: &MinioConfig,
    key: &str,
    file_path: &Path,
    content_type: Option<&str>,
    part_size: u64,
    concurrency: usize,
    progress_callback: Option<Box<dyn Fn(u64, u64) + Send + Sync>>,
    hooks: &UploadHooks,
) -> MinioResult<String> {
    let metadata = tokio::fs::metadata(file_path).await?;
    let file_size = metadata.len();

    if file_size == 0 {
        return Err("Cannot upload empty file".into());
    }

    let upload_id = initiate_multipart_upload(config, key, content_type, hooks).await?;
    let total_parts = file_size.div_ceil(part_size) as usize;

    let semaphore = Arc::new(Semaphore::new(concurrency));
    let uploaded_bytes = Arc::new(AtomicU64::new(0));
    let callback = Arc::new(progress_callback);
    // Set once a part has failed for good: start no further parts.
    let part_failed = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();

    for part_number in 1..=total_parts {
        if hooks.cancelled.load(Ordering::SeqCst) || part_failed.load(Ordering::SeqCst) {
            break;
        }
        let permit = semaphore.clone().acquire_owned().await?;
        let config = config.clone();
        let key = key.to_string();
        let upload_id = upload_id.clone();
        let file_path = file_path.to_path_buf();
        let uploaded_bytes = uploaded_bytes.clone();
        let callback = callback.clone();
        let hooks = hooks.clone();
        let part_failed = part_failed.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit;
            let start = (part_number as u64 - 1) * part_size;
            let end = std::cmp::min(start + part_size, file_size);
            let part_data_size = end - start;

            let etag = upload_part(
                &config,
                &key,
                &upload_id,
                part_number as i32,
                &file_path,
                start,
                part_data_size,
                &hooks,
            )
            .await
            .inspect_err(|_| part_failed.store(true, Ordering::SeqCst))?;

            let new_total =
                uploaded_bytes.fetch_add(part_data_size, Ordering::SeqCst) + part_data_size;
            if let Some(ref cb) = *callback {
                cb(new_total, file_size);
            }

            Ok::<(i32, String), Box<dyn std::error::Error + Send + Sync>>((
                part_number as i32,
                etag,
            ))
        });

        handles.push(handle);
    }

    let mut parts = Vec::new();
    let mut first_error: Option<Box<dyn std::error::Error + Send + Sync>> = None;

    for handle in handles {
        match handle.await {
            Ok(Ok((part_number, etag))) => {
                parts.push((part_number, etag));
            }
            Ok(Err(e)) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(format!("Task failed: {}", e).into());
                }
            }
        }
    }

    if hooks.cancelled.load(Ordering::SeqCst) {
        let _ = abort_multipart_upload(config, key, &upload_id).await;
        return Err(CANCELLED_MESSAGE.into());
    }

    if let Some(err) = first_error {
        let _ = abort_multipart_upload(config, key, &upload_id).await;
        return Err(err);
    }

    parts.sort_by_key(|(n, _)| *n);
    complete_multipart_upload(config, key, &upload_id, parts).await?;
    Ok(upload_id)
}

/// Upload a file (multipart from 100 MB). `hooks` carries the cancel flag and
/// the retry reporter of the command that owns the upload slot.
pub async fn upload_file(
    config: &MinioConfig,
    key: &str,
    file_path: &Path,
    content_type: Option<&str>,
    progress_callback: Option<Box<dyn Fn(u64, u64) + Send + Sync>>,
    hooks: &UploadHooks,
) -> MinioResult<String> {
    const MULTIPART_THRESHOLD: u64 = 100 * 1024 * 1024;
    const PART_SIZE: u64 = 20 * 1024 * 1024;
    const CONCURRENCY: usize = 6;

    let file_size = tokio::fs::metadata(file_path).await?.len();

    if file_size < MULTIPART_THRESHOLD {
        let etag = upload_file_simple(config, key, file_path, content_type, hooks).await?;
        if let Some(ref cb) = progress_callback {
            cb(file_size, file_size);
        }
        Ok(etag)
    } else {
        upload_file_multipart(
            config,
            key,
            file_path,
            content_type,
            PART_SIZE,
            CONCURRENCY,
            progress_callback,
            hooks,
        )
        .await
    }
}
