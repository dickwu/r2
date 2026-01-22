use crate::commands::upload_cache::update_cache_after_upload;
use crate::db::{self, UploadSession};
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::Mutex;

type HmacSha256 = Hmac<Sha256>;

// Multipart upload threshold: 100MB
const MULTIPART_THRESHOLD: u64 = 100 * 1024 * 1024;
// Part size: 20MB per chunk
const PART_SIZE: u64 = 20 * 1024 * 1024;
// Concurrent uploads: 6 parts in parallel
const CONCURRENCY: usize = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct R2Config {
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadProgress {
    pub task_id: String,
    pub percent: u32,
    pub uploaded_bytes: u64,
    pub total_bytes: u64,
    pub speed: f64, // bytes per second
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    pub task_id: String,
    pub success: bool,
    pub error: Option<String>,
}

// Global cancel registry
lazy_static::lazy_static! {
    static ref CANCEL_REGISTRY: Mutex<HashMap<String, Arc<AtomicBool>>> = Mutex::new(HashMap::new());
}

fn sha256_hash(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn get_signing_key(secret_key: &str, date_stamp: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(
        format!("AWS4{}", secret_key).as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// Encode URI path - encode each segment individually, keep / as separator
fn encode_uri_path(path: &str) -> String {
    path.split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Generate AWS Signature V4 presigned URL
fn generate_presigned_url(
    config: &R2Config,
    method: &str,
    key: &str,
    expires_in: u64,
    query_params: Option<&[(&str, &str)]>,
    _content_type: Option<&str>,
) -> String {
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let region = "auto";
    let service = "s3";

    let host = format!("{}.r2.cloudflarestorage.com", config.account_id);
    let canonical_uri = format!("/{}/{}", config.bucket, encode_uri_path(key));

    let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, region, service);

    // Build query string
    let mut query_parts: Vec<(String, String)> = vec![
        (
            "X-Amz-Algorithm".to_string(),
            "AWS4-HMAC-SHA256".to_string(),
        ),
        (
            "X-Amz-Credential".to_string(),
            urlencoding::encode(&format!("{}/{}", config.access_key_id, credential_scope))
                .to_string(),
        ),
        ("X-Amz-Date".to_string(), amz_date.clone()),
        ("X-Amz-Expires".to_string(), expires_in.to_string()),
        ("X-Amz-SignedHeaders".to_string(), "host".to_string()),
    ];

    if let Some(params) = query_params {
        for (k, v) in params {
            query_parts.push((k.to_string(), v.to_string()));
        }
    }

    query_parts.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical_query_string = query_parts
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");

    // Canonical headers
    let canonical_headers = format!("host:{}\n", host);
    let signed_headers = "host";

    // For presigned URL, payload is UNSIGNED-PAYLOAD
    let payload_hash = "UNSIGNED-PAYLOAD";

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method,
        canonical_uri,
        canonical_query_string,
        canonical_headers,
        signed_headers,
        payload_hash
    );

    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date,
        credential_scope,
        sha256_hash(canonical_request.as_bytes())
    );

    let signing_key = get_signing_key(&config.secret_access_key, &date_stamp, region, service);
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    format!(
        "https://{}{}?{}&X-Amz-Signature={}",
        host, canonical_uri, canonical_query_string, signature
    )
}

/// Upload a single file using PUT (for files < 100MB)
#[allow(clippy::too_many_arguments)]
async fn upload_single_part(
    client: &Client,
    config: &R2Config,
    key: &str,
    file_path: &PathBuf,
    content_type: &str,
    task_id: &str,
    app: &AppHandle,
    cancelled: &Arc<AtomicBool>,
) -> Result<(), String> {
    let presigned_url = generate_presigned_url(config, "PUT", key, 3600, None, Some(content_type));

    let file_size = tokio::fs::metadata(file_path)
        .await
        .map_err(|e| format!("Failed to get file metadata: {}", e))?
        .len();

    // Read file into memory for small files
    let mut file = File::open(file_path)
        .await
        .map_err(|e| format!("Failed to open file: {}", e))?;

    let mut buffer = Vec::with_capacity(file_size as usize);
    file.read_to_end(&mut buffer)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;

    if cancelled.load(Ordering::SeqCst) {
        return Err("Upload cancelled".to_string());
    }

    let start_time = std::time::Instant::now();

    let response = client
        .put(&presigned_url)
        .header("Content-Type", content_type)
        .body(buffer)
        .send()
        .await
        .map_err(|e| format!("Upload request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("Upload failed: {} - {}", status, text));
    }

    let elapsed = start_time.elapsed().as_secs_f64();
    let speed = if elapsed > 0.0 {
        file_size as f64 / elapsed
    } else {
        0.0
    };

    let _ = app.emit(
        "upload-progress",
        UploadProgress {
            task_id: task_id.to_string(),
            percent: 100,
            uploaded_bytes: file_size,
            total_bytes: file_size,
            speed,
        },
    );

    Ok(())
}

/// Multipart upload for large files with resume support
#[allow(clippy::too_many_arguments)]
async fn upload_multipart(
    client: &Client,
    config: &R2Config,
    key: &str,
    file_path: &PathBuf,
    content_type: &str,
    task_id: &str,
    app: &AppHandle,
    cancelled: &Arc<AtomicBool>,
) -> Result<(), String> {
    let metadata = tokio::fs::metadata(file_path)
        .await
        .map_err(|e| format!("Failed to get file metadata: {}", e))?;

    let file_size = metadata.len();
    let file_mtime = metadata
        .modified()
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
        })
        .unwrap_or(0);

    let total_parts = file_size.div_ceil(PART_SIZE) as usize;
    let file_path_str = file_path.to_string_lossy().to_string();

    // Check for existing resumable session
    let (upload_id, existing_parts, session_id) = match db::find_resumable_session(
        &file_path_str,
        file_size as i64,
        file_mtime,
        key,
        &config.bucket,
        &config.account_id,
    )
    .await
    {
        Ok(Some(session)) => {
            // Found existing session - verify upload_id is still valid on R2
            let upload_id = session.upload_id.clone().unwrap();

            // Get completed parts from DB
            let parts = db::get_completed_parts(&session.id)
                .await
                .map_err(|e| format!("Failed to get completed parts: {}", e))?;

            let part_set: HashSet<i32> = parts.iter().map(|p| p.part_number).collect();
            let part_map: HashMap<i32, String> = parts
                .iter()
                .map(|p| (p.part_number, p.etag.clone()))
                .collect();

            log::info!(
                "Resuming upload {} with {} completed parts",
                session.id,
                part_set.len()
            );

            (upload_id, Some((part_set, part_map)), session.id)
        }
        _ => {
            // Create new multipart upload
            let create_url = generate_presigned_url(
                config,
                "POST",
                key,
                3600,
                Some(&[("uploads", "")]),
                Some(content_type),
            );

            let create_response = client
                .post(&create_url)
                .send()
                .await
                .map_err(|e| format!("Failed to initiate multipart upload: {}", e))?;

            if !create_response.status().is_success() {
                let status = create_response.status();
                let text = create_response.text().await.unwrap_or_default();
                return Err(format!(
                    "Failed to initiate multipart upload: {} - {}",
                    status, text
                ));
            }

            let create_xml = create_response
                .text()
                .await
                .map_err(|e| format!("Failed to read response: {}", e))?;

            let upload_id = create_xml
                .split("<UploadId>")
                .nth(1)
                .and_then(|s| s.split("</UploadId>").next())
                .ok_or("Failed to parse UploadId from response")?
                .to_string();

            // Create session in DB
            let now = Utc::now().timestamp();
            let session = UploadSession {
                id: task_id.to_string(),
                file_path: file_path_str.clone(),
                file_size: file_size as i64,
                file_mtime,
                object_key: key.to_string(),
                bucket: config.bucket.clone(),
                account_id: config.account_id.clone(),
                upload_id: Some(upload_id.clone()),
                content_type: content_type.to_string(),
                total_parts: total_parts as i32,
                created_at: now,
                updated_at: now,
                status: "uploading".to_string(),
            };

            if let Err(e) = db::create_session(&session).await {
                log::warn!("Failed to save session to DB: {}", e);
            }

            (upload_id, None, task_id.to_string())
        }
    };

    // Calculate already uploaded bytes for progress
    let (completed_part_set, completed_part_map) =
        existing_parts.unwrap_or((HashSet::new(), HashMap::new()));
    let already_uploaded: u64 = completed_part_set
        .iter()
        .map(|&part_num| {
            let start = (part_num as u64 - 1) * PART_SIZE;
            let end = std::cmp::min(start + PART_SIZE, file_size);
            end - start
        })
        .sum();

    // Track progress
    let uploaded_bytes = Arc::new(AtomicU64::new(already_uploaded));
    let start_time = std::time::Instant::now();

    // Store completed parts (include already completed ones)
    let completed_parts: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(
        completed_part_map
            .iter()
            .map(|(&n, e)| (n as usize, e.clone()))
            .collect(),
    ));

    // Filter out already completed parts
    let part_numbers: Vec<usize> = (1..=total_parts)
        .filter(|&n| !completed_part_set.contains(&(n as i32)))
        .collect();

    // Emit initial progress if resuming
    if already_uploaded > 0 {
        let percent = ((already_uploaded as f64 / file_size as f64) * 100.0) as u32;
        let _ = app.emit(
            "upload-progress",
            UploadProgress {
                task_id: task_id.to_string(),
                percent,
                uploaded_bytes: already_uploaded,
                total_bytes: file_size,
                speed: 0.0,
            },
        );
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));
    let mut handles = Vec::new();

    for part_number in part_numbers {
        if cancelled.load(Ordering::SeqCst) {
            break;
        }

        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let client = client.clone();
        let config = config.clone();
        let key = key.to_string();
        let upload_id = upload_id.clone();
        let file_path = file_path.clone();
        let task_id = task_id.to_string();
        let session_id = session_id.clone();
        let app = app.clone();
        let cancelled = cancelled.clone();
        let uploaded_bytes = uploaded_bytes.clone();
        let completed_parts = completed_parts.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit;

            if cancelled.load(Ordering::SeqCst) {
                return Err("Upload cancelled".to_string());
            }

            let start = (part_number as u64 - 1) * PART_SIZE;
            let end = std::cmp::min(start + PART_SIZE, file_size);
            let part_size = end - start;

            // Read part from file
            let mut file = File::open(&file_path)
                .await
                .map_err(|e| format!("Failed to open file: {}", e))?;

            file.seek(SeekFrom::Start(start))
                .await
                .map_err(|e| format!("Failed to seek: {}", e))?;

            let mut buffer = vec![0u8; part_size as usize];
            file.read_exact(&mut buffer)
                .await
                .map_err(|e| format!("Failed to read file part: {}", e))?;

            if cancelled.load(Ordering::SeqCst) {
                return Err("Upload cancelled".to_string());
            }

            // Generate presigned URL for this part
            let part_url = generate_presigned_url(
                &config,
                "PUT",
                &key,
                3600,
                Some(&[
                    ("partNumber", &part_number.to_string()),
                    ("uploadId", &upload_id),
                ]),
                None,
            );

            let response = client
                .put(&part_url)
                .body(buffer)
                .send()
                .await
                .map_err(|e| format!("Failed to upload part {}: {}", part_number, e))?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                return Err(format!(
                    "Failed to upload part {}: {} - {}",
                    part_number, status, text
                ));
            }

            let etag = response
                .headers()
                .get("ETag")
                .and_then(|v| v.to_str().ok())
                .ok_or(format!("No ETag returned for part {}", part_number))?
                .to_string();

            // Save completed part to DB
            if let Err(e) = db::save_completed_part(&session_id, part_number as i32, &etag).await {
                log::warn!("Failed to save part {} to DB: {}", part_number, e);
            }

            // Update progress
            let new_uploaded = uploaded_bytes.fetch_add(part_size, Ordering::SeqCst) + part_size;
            let percent = ((new_uploaded as f64 / file_size as f64) * 100.0) as u32;
            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                (new_uploaded - already_uploaded) as f64 / elapsed
            } else {
                0.0
            };

            let _ = app.emit(
                "upload-progress",
                UploadProgress {
                    task_id: task_id.clone(),
                    percent,
                    uploaded_bytes: new_uploaded,
                    total_bytes: file_size,
                    speed,
                },
            );

            // Store completed part
            completed_parts.lock().await.push((part_number, etag));

            Ok::<(), String>(())
        });

        handles.push(handle);
    }

    // Wait for all parts
    let mut upload_error: Option<String> = None;
    for handle in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if upload_error.is_none() {
                    upload_error = Some(e);
                }
            }
            Err(e) => {
                if upload_error.is_none() {
                    upload_error = Some(format!("Task panicked: {}", e));
                }
            }
        }
    }

    if cancelled.load(Ordering::SeqCst) {
        // Mark session as cancelled but don't delete (can be resumed later)
        let _ = db::update_session_status(&session_id, "cancelled").await;

        // Note: We don't abort the multipart upload on R2 so it can be resumed
        return Err("Upload cancelled".to_string());
    }

    if let Some(err) = upload_error {
        // Mark session as failed but keep it for retry
        let _ = db::update_session_status(&session_id, "uploading").await;
        return Err(err);
    }

    // 3. Complete multipart upload
    let mut parts = completed_parts.lock().await;
    parts.sort_by_key(|(n, _)| *n);

    let parts_xml = parts
        .iter()
        .map(|(n, etag)| {
            format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                n, etag
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let complete_xml = format!(
        "<CompleteMultipartUpload>{}</CompleteMultipartUpload>",
        parts_xml
    );

    let complete_url = generate_presigned_url(
        config,
        "POST",
        key,
        3600,
        Some(&[("uploadId", &upload_id)]),
        None,
    );

    let complete_response = client
        .post(&complete_url)
        .header("Content-Type", "application/xml")
        .body(complete_xml)
        .send()
        .await
        .map_err(|e| format!("Failed to complete multipart upload: {}", e))?;

    if !complete_response.status().is_success() {
        let status = complete_response.status();
        let text = complete_response.text().await.unwrap_or_default();
        return Err(format!(
            "Failed to complete multipart upload: {} - {}",
            status, text
        ));
    }

    // Mark session as completed and clean up
    let _ = db::update_session_status(&session_id, "completed").await;
    let _ = db::delete_session(&session_id).await;

    // Final progress update
    let elapsed = start_time.elapsed().as_secs_f64();
    let speed = if elapsed > 0.0 {
        (file_size - already_uploaded) as f64 / elapsed
    } else {
        0.0
    };

    let _ = app.emit(
        "upload-progress",
        UploadProgress {
            task_id: task_id.to_string(),
            percent: 100,
            uploaded_bytes: file_size,
            total_bytes: file_size,
            speed,
        },
    );

    Ok(())
}

/// Main upload command - called from frontend
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn upload_file(
    app: AppHandle,
    task_id: String,
    file_path: String,
    key: String,
    content_type: String,
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
        });
    }

    let file_size = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("Failed to get file metadata: {}", e))?
        .len();

    // Register cancel flag
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut registry = CANCEL_REGISTRY.lock().await;
        registry.insert(task_id.clone(), cancelled.clone());
    }

    let client = Client::builder()
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let result = if file_size < MULTIPART_THRESHOLD {
        upload_single_part(
            &client,
            &config,
            &key,
            &path,
            &content_type,
            &task_id,
            &app,
            &cancelled,
        )
        .await
    } else {
        upload_multipart(
            &client,
            &config,
            &key,
            &path,
            &content_type,
            &task_id,
            &app,
            &cancelled,
        )
        .await
    };

    // Cleanup cancel flag
    {
        let mut registry = CANCEL_REGISTRY.lock().await;
        registry.remove(&task_id);
    }

    match result {
        Ok(()) => {
            let last_modified = chrono::Utc::now().to_rfc3339();
            if let Err(err) = update_cache_after_upload(
                &app,
                &config.bucket,
                &config.account_id,
                &key,
                file_size as i64,
                &last_modified,
            )
            .await
            {
                log::warn!("Failed to update cache after upload: {}", err);
            }

            Ok(UploadResult {
                task_id,
                success: true,
                error: None,
            })
        }
        Err(e) => Ok(UploadResult {
            task_id,
            success: false,
            error: Some(e),
        }),
    }
}

/// Cancel an upload
#[tauri::command]
pub async fn cancel_upload(task_id: String) -> Result<(), String> {
    let registry = CANCEL_REGISTRY.lock().await;
    if let Some(cancelled) = registry.get(&task_id) {
        cancelled.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// Get file info for a path
#[tauri::command]
pub async fn get_file_info(file_path: String) -> Result<(u64, String), String> {
    let path = PathBuf::from(&file_path);
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("Failed to get file metadata: {}", e))?;

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    Ok((metadata.len(), file_name))
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderFileInfo {
    pub file_path: String,
    pub relative_path: String, // Path relative to selected folder
    pub file_size: u64,
}

/// Recursively get all files in a directory (using stack-based iteration)
#[tauri::command]
pub async fn get_folder_files(folder_path: String) -> Result<Vec<FolderFileInfo>, String> {
    let root = PathBuf::from(&folder_path);
    if !root.is_dir() {
        return Err(format!("Not a directory: {}", folder_path));
    }

    let mut files = Vec::new();
    let mut stack = vec![root.clone()];

    while let Some(current) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&current)
            .await
            .map_err(|e| format!("Failed to read directory {}: {}", current.display(), e))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| format!("Failed to read entry: {}", e))?
        {
            let path = entry.path();

            // Skip hidden files/directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }

            let metadata = tokio::fs::metadata(&path)
                .await
                .map_err(|e| format!("Failed to get metadata for {}: {}", path.display(), e))?;

            if metadata.is_file() {
                let relative = path
                    .strip_prefix(&root)
                    .map_err(|e| format!("Failed to get relative path: {}", e))?;

                files.push(FolderFileInfo {
                    file_path: path.to_string_lossy().to_string(),
                    relative_path: relative.to_string_lossy().to_string(),
                    file_size: metadata.len(),
                });
            } else if metadata.is_dir() {
                stack.push(path);
            }
        }
    }

    // Sort files by relative path for consistent ordering
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    Ok(files)
}

/// Get all pending/uploading sessions (for showing resumable uploads in UI)
#[tauri::command]
pub async fn get_pending_uploads() -> Result<Vec<db::UploadSession>, String> {
    db::get_pending_sessions()
        .await
        .map_err(|e| format!("Failed to get pending sessions: {}", e))
}

/// Get upload session by ID
#[tauri::command]
pub async fn get_upload_session(session_id: String) -> Result<Option<db::UploadSession>, String> {
    db::get_session(&session_id)
        .await
        .map_err(|e| format!("Failed to get session: {}", e))
}

/// Get completed parts count for a session (for progress display)
#[tauri::command]
pub async fn get_session_progress(session_id: String) -> Result<(i32, i32), String> {
    let session = db::get_session(&session_id)
        .await
        .map_err(|e| format!("Failed to get session: {}", e))?
        .ok_or("Session not found")?;

    let parts = db::get_completed_parts(&session_id)
        .await
        .map_err(|e| format!("Failed to get parts: {}", e))?;

    Ok((parts.len() as i32, session.total_parts))
}

/// Delete an upload session (e.g., user wants to restart from scratch)
#[tauri::command]
pub async fn delete_upload_session(session_id: String) -> Result<(), String> {
    db::delete_session(&session_id)
        .await
        .map_err(|e| format!("Failed to delete session: {}", e))
}

/// Clean up old sessions
#[tauri::command]
pub async fn cleanup_old_sessions() -> Result<usize, String> {
    db::cleanup_old_sessions()
        .await
        .map_err(|e| format!("Failed to cleanup: {}", e))
}

/// Check if a file has a resumable upload session
#[tauri::command]
pub async fn check_resumable_upload(
    file_path: String,
    object_key: String,
    bucket: String,
    account_id: String,
) -> Result<Option<db::UploadSession>, String> {
    let path = PathBuf::from(&file_path);
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("Failed to get file metadata: {}", e))?;

    let file_size = metadata.len() as i64;
    let file_mtime = metadata
        .modified()
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
        })
        .unwrap_or(0);

    match db::find_resumable_session(
        &file_path,
        file_size,
        file_mtime,
        &object_key,
        &bucket,
        &account_id,
    )
    .await
    {
        Ok(result) => Ok(result),
        Err(e) => Err(format!("Failed to check resumable session: {}", e)),
    }
}
