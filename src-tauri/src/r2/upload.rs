//! R2 upload operations (simple, multipart)

use super::types::{create_r2_client, R2Config, R2Result};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::Semaphore;

/// Upload a file (simple PUT for files < 100MB)
#[allow(dead_code)]
pub async fn upload_file_simple(
    config: &R2Config,
    key: &str,
    file_path: &Path,
    content_type: Option<&str>,
) -> R2Result<String> {
    let client = create_r2_client(config).await?;
    let body = ByteStream::from_path(file_path).await?;

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

/// Initiate multipart upload
#[allow(dead_code)]
pub async fn initiate_multipart_upload(
    config: &R2Config,
    key: &str,
    content_type: Option<&str>,
) -> R2Result<String> {
    let client = create_r2_client(config).await?;

    let mut request = client
        .create_multipart_upload()
        .bucket(&config.bucket)
        .key(key);

    if let Some(ct) = content_type {
        request = request.content_type(ct);
    }

    let response = request.send().await?;
    let upload_id = response
        .upload_id()
        .ok_or("No upload ID returned")?
        .to_string();

    Ok(upload_id)
}

/// Upload a part in multipart upload
#[allow(dead_code)]
pub async fn upload_part(
    config: &R2Config,
    key: &str,
    upload_id: &str,
    part_number: i32,
    data: Vec<u8>,
) -> R2Result<String> {
    let client = create_r2_client(config).await?;
    let body = ByteStream::from(data);

    let response = client
        .upload_part()
        .bucket(&config.bucket)
        .key(key)
        .upload_id(upload_id)
        .part_number(part_number)
        .body(body)
        .send()
        .await?;

    let etag = response.e_tag().unwrap_or_default().to_string();
    Ok(etag)
}

/// Complete multipart upload
#[allow(dead_code)]
pub async fn complete_multipart_upload(
    config: &R2Config,
    key: &str,
    upload_id: &str,
    parts: Vec<(i32, String)>, // (part_number, etag)
) -> R2Result<()> {
    let client = create_r2_client(config).await?;

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

/// Abort multipart upload
#[allow(dead_code)]
pub async fn abort_multipart_upload(config: &R2Config, key: &str, upload_id: &str) -> R2Result<()> {
    let client = create_r2_client(config).await?;

    client
        .abort_multipart_upload()
        .bucket(&config.bucket)
        .key(key)
        .upload_id(upload_id)
        .send()
        .await?;

    Ok(())
}

/// List parts of a multipart upload (for resume)
#[allow(dead_code)]
pub async fn list_parts(
    config: &R2Config,
    key: &str,
    upload_id: &str,
) -> R2Result<Vec<(i32, String)>> {
    let client = create_r2_client(config).await?;

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

/// Upload a large file using multipart upload (AWS SDK orchestration)
#[allow(dead_code)]
pub async fn upload_file_multipart(
    config: &R2Config,
    key: &str,
    file_path: &Path,
    content_type: Option<&str>,
    part_size: u64,
    concurrency: usize,
    progress_callback: Option<Box<dyn Fn(u64, u64) + Send + Sync>>,
) -> R2Result<String> {
    // Get file size
    let metadata = tokio::fs::metadata(file_path).await?;
    let file_size = metadata.len();

    if file_size == 0 {
        return Err("Cannot upload empty file".into());
    }

    // Initiate multipart upload
    let upload_id = initiate_multipart_upload(config, key, content_type).await?;

    // Calculate parts
    let total_parts = ((file_size + part_size - 1) / part_size) as usize;

    // Upload parts concurrently
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let uploaded_bytes = Arc::new(AtomicU64::new(0));
    let callback = Arc::new(progress_callback);
    let mut handles = Vec::new();

    for part_number in 1..=total_parts {
        let permit = semaphore.clone().acquire_owned().await?;
        let config = config.clone();
        let key = key.to_string();
        let upload_id = upload_id.clone();
        let file_path = file_path.to_path_buf();
        let uploaded_bytes = uploaded_bytes.clone();
        let callback = callback.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit;

            // Calculate byte range for this part
            let start = (part_number as u64 - 1) * part_size;
            let end = std::cmp::min(start + part_size, file_size);
            let part_data_size = end - start;

            // Read part data
            let mut file = File::open(&file_path).await?;
            file.seek(SeekFrom::Start(start)).await?;

            let mut buffer = vec![0u8; part_data_size as usize];
            file.read_exact(&mut buffer).await?;

            // Upload this part
            let etag = upload_part(&config, &key, &upload_id, part_number as i32, buffer).await?;

            // Update progress
            let new_total = uploaded_bytes.fetch_add(part_data_size, Ordering::SeqCst) + part_data_size;
            if let Some(ref cb) = *callback {
                cb(new_total, file_size);
            }

            Ok::<(i32, String), Box<dyn std::error::Error + Send + Sync>>((part_number as i32, etag))
        });

        handles.push(handle);
    }

    // Wait for all parts and collect results
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

    // If any part failed, abort the upload
    if let Some(err) = first_error {
        let _ = abort_multipart_upload(config, key, &upload_id).await;
        return Err(err);
    }

    // Sort parts by part number (required by S3/R2)
    parts.sort_by_key(|(n, _)| *n);

    // Complete the multipart upload
    complete_multipart_upload(config, key, &upload_id, parts).await?;

    // Return the upload_id as confirmation
    Ok(upload_id)
}

/// Smart upload: automatically choose between simple and multipart based on size
#[allow(dead_code)]
pub async fn upload_file(
    config: &R2Config,
    key: &str,
    file_path: &Path,
    content_type: Option<&str>,
    progress_callback: Option<Box<dyn Fn(u64, u64) + Send + Sync>>,
) -> R2Result<String> {
    const MULTIPART_THRESHOLD: u64 = 100 * 1024 * 1024; // 100MB
    const PART_SIZE: u64 = 20 * 1024 * 1024; // 20MB per part
    const CONCURRENCY: usize = 6; // 6 parallel uploads

    let file_size = tokio::fs::metadata(file_path).await?.len();

    if file_size < MULTIPART_THRESHOLD {
        // Small file: simple PUT
        if let Some(ref cb) = progress_callback {
            cb(file_size, file_size);
        }
        upload_file_simple(config, key, file_path, content_type).await
    } else {
        // Large file: multipart upload
        upload_file_multipart(
            config,
            key,
            file_path,
            content_type,
            PART_SIZE,
            CONCURRENCY,
            progress_callback,
        )
        .await
    }
}
