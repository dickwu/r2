use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart, Delete, ObjectIdentifier};
use aws_sdk_s3::Client;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

pub type R2Result<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct R2Config {
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct R2Object {
    pub key: String,
    pub size: i64,
    pub last_modified: String,
    pub etag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct R2Bucket {
    pub name: String,
    pub creation_date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListObjectsResult {
    pub objects: Vec<R2Object>,
    pub folders: Vec<String>,
    pub truncated: bool,
    pub continuation_token: Option<String>,
}

/// Create an S3 client configured for Cloudflare R2
pub async fn create_r2_client(config: &R2Config) -> R2Result<Client> {
    let credentials = Credentials::new(
        &config.access_key_id,
        &config.secret_access_key,
        None,
        None,
        "r2-provider",
    );

    let endpoint_url = format!("https://{}.r2.cloudflarestorage.com", config.account_id);

    let s3_config = S3ConfigBuilder::new()
        .credentials_provider(credentials)
        .region(Region::new("auto"))
        .endpoint_url(endpoint_url)
        .force_path_style(true)
        .build();

    Ok(Client::from_conf(s3_config))
}

/// List all buckets in the R2 account
pub async fn list_buckets(config: &R2Config) -> R2Result<Vec<R2Bucket>> {
    let client = create_r2_client(config).await?;
    let response = client.list_buckets().send().await?;

    let buckets = response
        .buckets()
        .iter()
        .filter_map(|bucket| {
            let name = bucket.name()?.to_string();
            let creation_date = bucket
                .creation_date()
                .map(|dt| dt.to_string())
                .unwrap_or_default();
            Some(R2Bucket {
                name,
                creation_date,
            })
        })
        .collect();

    Ok(buckets)
}

/// List objects in a bucket with optional prefix and delimiter
pub async fn list_objects(
    config: &R2Config,
    prefix: Option<&str>,
    delimiter: Option<&str>,
    continuation_token: Option<&str>,
    max_keys: Option<i32>,
) -> R2Result<ListObjectsResult> {
    let client = create_r2_client(config).await?;

    let mut request = client
        .list_objects_v2()
        .bucket(&config.bucket)
        .max_keys(max_keys.unwrap_or(1000));

    if let Some(p) = prefix {
        request = request.prefix(p);
    }
    if let Some(d) = delimiter {
        request = request.delimiter(d);
    }
    if let Some(token) = continuation_token {
        request = request.continuation_token(token);
    }

    let response = request.send().await?;

    let objects = response
        .contents()
        .iter()
        .filter_map(|obj| {
            let key = obj.key()?.to_string();
            // Skip directory markers
            if key.ends_with('/') {
                return None;
            }
            Some(R2Object {
                key,
                size: obj.size().unwrap_or(0),
                last_modified: obj
                    .last_modified()
                    .map(|dt| dt.to_string())
                    .unwrap_or_default(),
                etag: obj.e_tag().unwrap_or_default().to_string(),
            })
        })
        .collect();

    let folders = response
        .common_prefixes()
        .iter()
        .filter_map(|prefix| prefix.prefix().map(|s| s.to_string()))
        .collect();

    Ok(ListObjectsResult {
        objects,
        folders,
        truncated: response.is_truncated().unwrap_or(false),
        continuation_token: response.next_continuation_token().map(|s| s.to_string()),
    })
}

/// List all objects recursively (for caching)
pub async fn list_all_objects_recursive(
    config: &R2Config,
    progress_callback: Option<Box<dyn Fn(usize) + Send + Sync>>,
) -> R2Result<Vec<R2Object>> {
    use tokio::sync::mpsc;
    use std::sync::{Arc, Mutex};
    
    let client = create_r2_client(config).await?;
    let all_objects = Arc::new(Mutex::new(Vec::new()));
    let (tx, mut rx) = mpsc::channel::<Vec<R2Object>>(4); // Buffer up to 4 pages
    
    let all_objects_clone = all_objects.clone();
    let callback = Arc::new(progress_callback);
    
    // Spawner task: aggregates results and reports progress
    let aggregator = tokio::spawn(async move {
        while let Some(objects) = rx.recv().await {
            let mut all = all_objects_clone.lock().unwrap();
            all.extend(objects);
            let count = all.len();
            drop(all); // Release lock before callback
            
            if let Some(ref cb) = *callback {
                cb(count);
            }
        }
    });
    
    // Fetcher: sequential fetch with pipelined processing
    let mut continuation_token: Option<String> = None;
    
    loop {
        let mut request = client
            .list_objects_v2()
            .bucket(&config.bucket)
            .max_keys(1000);

        if let Some(token) = &continuation_token {
            request = request.continuation_token(token);
        }

        let response = request.send().await?;
        let is_truncated = response.is_truncated().unwrap_or(false);
        let next_token = response.next_continuation_token().map(|s| s.to_string());

        // Process response in parallel while we can fetch the next page
        let objects: Vec<R2Object> = response
            .contents()
            .iter()
            .filter_map(|obj| {
                let key = obj.key()?.to_string();
                // Skip directory markers
                if key.ends_with('/') {
                    return None;
                }
                Some(R2Object {
                    key,
                    size: obj.size().unwrap_or(0),
                    last_modified: obj
                        .last_modified()
                        .map(|dt| dt.to_string())
                        .unwrap_or_default(),
                    etag: obj.e_tag().unwrap_or_default().to_string(),
                })
            })
            .collect();

        // Send to aggregator (non-blocking if buffer has space)
        if tx.send(objects).await.is_err() {
            break; // Receiver dropped
        }

        if !is_truncated {
            break;
        }

        continuation_token = next_token;
    }
    
    // Close sender and wait for aggregator to finish
    drop(tx);
    let _ = aggregator.await;
    
    // Extract final results
    let result = match Arc::try_unwrap(all_objects) {
        Ok(mutex) => mutex.into_inner().unwrap(),
        Err(arc) => arc.lock().unwrap().clone(),
    };
    
    Ok(result)
}

/// Delete a single object
pub async fn delete_object(config: &R2Config, key: &str) -> R2Result<()> {
    let client = create_r2_client(config).await?;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(key)
        .send()
        .await?;
    Ok(())
}

/// Delete multiple objects in batch
#[allow(dead_code)]
pub async fn delete_objects(config: &R2Config, keys: Vec<String>) -> R2Result<()> {
    if keys.is_empty() {
        return Ok(());
    }

    let client = create_r2_client(config).await?;
    
    let objects: Vec<ObjectIdentifier> = keys
        .iter()
        .map(|key| ObjectIdentifier::builder().key(key).build().unwrap())
        .collect();

    let delete = Delete::builder().set_objects(Some(objects)).build()?;

    client
        .delete_objects()
        .bucket(&config.bucket)
        .delete(delete)
        .send()
        .await?;

    Ok(())
}

/// Copy an object (used for rename)
pub async fn copy_object(config: &R2Config, source_key: &str, dest_key: &str) -> R2Result<()> {
    let client = create_r2_client(config).await?;
    let copy_source = format!("{}/{}", config.bucket, source_key);

    client
        .copy_object()
        .bucket(&config.bucket)
        .copy_source(copy_source)
        .key(dest_key)
        .send()
        .await?;

    Ok(())
}

/// Rename an object (copy then delete)
pub async fn rename_object(config: &R2Config, old_key: &str, new_key: &str) -> R2Result<()> {
    copy_object(config, old_key, new_key).await?;
    delete_object(config, old_key).await?;
    Ok(())
}

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
pub async fn abort_multipart_upload(
    config: &R2Config,
    key: &str,
    upload_id: &str,
) -> R2Result<()> {
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

/// Generate a presigned URL for object access
pub async fn generate_presigned_url(
    config: &R2Config,
    key: &str,
    expires_in_secs: u64,
) -> R2Result<String> {
    use aws_sdk_s3::presigning::PresigningConfig;
    use std::time::Duration;

    let client = create_r2_client(config).await?;
    
    let presigning_config = PresigningConfig::builder()
        .expires_in(Duration::from_secs(expires_in_secs))
        .build()?;

    let presigned_request = client
        .get_object()
        .bucket(&config.bucket)
        .key(key)
        .presigned(presigning_config)
        .await?;

    Ok(presigned_request.uri().to_string())
}

/// Upload a large file using multipart upload (AWS SDK orchestration)
/// This is the "good taste" version - let the SDK do the work
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
    use tokio::fs::File;
    use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
    
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
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let uploaded_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let callback = std::sync::Arc::new(progress_callback);
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
            let etag = upload_part(
                &config,
                &key,
                &upload_id,
                part_number as i32,
                buffer,
            ).await?;
            
            // Update progress
            let new_total = uploaded_bytes.fetch_add(part_data_size, std::sync::atomic::Ordering::SeqCst) + part_data_size;
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
        ).await
    }
}

// Tauri command structures
#[derive(Debug, Clone, Serialize)]
pub struct UploadProgress {
    pub task_id: String,
    pub percent: u32,
    pub uploaded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    pub task_id: String,
    pub success: bool,
    pub error: Option<String>,
    pub upload_id: Option<String>,
}

/// Tauri command: Upload file using AWS SDK (clean implementation)
#[tauri::command]
pub async fn upload_file_sdk(
    app: AppHandle,
    task_id: String,
    file_path: String,
    key: String,
    content_type: Option<String>,
    account_id: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
) -> Result<UploadResult, String> {
    let config = R2Config {
        account_id,
        bucket,
        access_key_id,
        secret_access_key,
    };

    let path = PathBuf::from(&file_path);
    if !path.exists() {
        return Ok(UploadResult {
            task_id,
            success: false,
            error: Some(format!("File not found: {}", file_path)),
            upload_id: None,
        });
    }

    // Get file size for progress tracking
    let file_size = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("Failed to get file metadata: {}", e))?
        .len();

    // Create progress callback that emits Tauri events
    let task_id_clone = task_id.clone();
    let app_clone = app.clone();
    let progress_callback = Box::new(move |uploaded: u64, total: u64| {
        let percent = if total > 0 {
            ((uploaded as f64 / total as f64) * 100.0) as u32
        } else {
            0
        };
        
        let _ = app_clone.emit(
            "upload-progress",
            UploadProgress {
                task_id: task_id_clone.clone(),
                percent,
                uploaded_bytes: uploaded,
                total_bytes: total,
            },
        );
    });

    // Call the upload function
    let result = upload_file(
        &config,
        &key,
        &path,
        content_type.as_deref(),
        Some(progress_callback),
    )
    .await;

    match result {
        Ok(upload_id_or_etag) => {
            // Emit final 100% progress
            let _ = app.emit(
                "upload-progress",
                UploadProgress {
                    task_id: task_id.clone(),
                    percent: 100,
                    uploaded_bytes: file_size,
                    total_bytes: file_size,
                },
            );
            
            Ok(UploadResult {
                task_id,
                success: true,
                error: None,
                upload_id: Some(upload_id_or_etag),
            })
        }
        Err(e) => Ok(UploadResult {
            task_id,
            success: false,
            error: Some(e.to_string()),
            upload_id: None,
        }),
    }
}
