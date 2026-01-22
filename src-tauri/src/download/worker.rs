//! Download worker - internal download logic with streaming and progress tracking

use crate::db::{self, DownloadSession};
use crate::providers::{aws, minio, rustfs};
use crate::r2::R2Config;
use reqwest::Client;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::Mutex;

use super::types::{DownloadProgress, DownloadStatusChanged, MAX_CONCURRENT_DOWNLOADS};

#[derive(Debug, Clone)]
pub(crate) enum DownloadConfig {
    R2(R2Config),
    Aws(aws::AwsConfig),
    Minio(minio::MinioConfig),
    Rustfs(rustfs::RustfsConfig),
}

/// Write buffer size for downloads (2 MB) - reduces I/O operations
const WRITE_BUFFER_SIZE: usize = 2 * 1024 * 1024;

// Global cancel/pause registry for downloads
lazy_static::lazy_static! {
    pub(crate) static ref DOWNLOAD_CANCEL_REGISTRY: Mutex<HashMap<String, Arc<AtomicBool>>> = Mutex::new(HashMap::new());
    pub(crate) static ref DOWNLOAD_PAUSE_REGISTRY: Mutex<HashMap<String, Arc<AtomicBool>>> = Mutex::new(HashMap::new());
}

/// Download a single file with streaming and progress (internal)
pub(crate) async fn download_file_internal(
    client: &Client,
    config: &DownloadConfig,
    key: &str,
    destination: &PathBuf,
    task_id: &str,
    file_size: u64,
    app: &AppHandle,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
) -> Result<(), String> {
    // Emit initial progress to show download task has started
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            task_id: task_id.to_string(),
            percent: 0,
            downloaded_bytes: 0,
            total_bytes: file_size,
            speed: 0.0,
        },
    );

    // Generate presigned URL for the object (fresh URL every time)
    let presigned_url: String = match config {
        DownloadConfig::R2(cfg) => crate::r2::generate_presigned_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e))?,
        DownloadConfig::Aws(cfg) => aws::generate_presigned_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e))?,
        DownloadConfig::Minio(cfg) => minio::generate_presigned_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e))?,
        DownloadConfig::Rustfs(cfg) => rustfs::generate_presigned_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e))?,
    };

    // Check if we should resume from existing partial file
    let existing_bytes = if destination.exists() {
        tokio::fs::metadata(destination)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };

    // Start the download request with range header if resuming
    let mut request = client.get(&presigned_url);
    if existing_bytes > 0 {
        request = request.header("Range", format!("bytes={}-", existing_bytes));
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("Download request failed: {}", e))?;

    if !response.status().is_success() && response.status().as_u16() != 206 {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("Download failed: {} - {}", status, text));
    }

    // Get content length for progress tracking
    let total_bytes = if file_size > 0 {
        file_size
    } else {
        let content_length = response.content_length().unwrap_or(0) + existing_bytes;
        // Update file size in database if we got it from Content-Length
        if content_length > 0 {
            let _ = db::update_download_file_size(task_id, content_length as i64).await;
        }
        content_length
    };

    // Create parent directories if needed
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    // Open or create the destination file (append mode if resuming)
    let mut file = if existing_bytes > 0 {
        let mut f = OpenOptions::new()
            .write(true)
            .open(destination)
            .await
            .map_err(|e| format!("Failed to open file: {}", e))?;
        f.seek(SeekFrom::End(0))
            .await
            .map_err(|e| format!("Failed to seek: {}", e))?;
        f
    } else {
        File::create(destination)
            .await
            .map_err(|e| format!("Failed to create file: {}", e))?
    };

    // Track progress
    let downloaded_bytes = Arc::new(AtomicU64::new(existing_bytes));
    let start_time = std::time::Instant::now();
    let start_bytes = existing_bytes;

    // Emit initial progress event to show download has started
    let initial_percent = if total_bytes > 0 {
        ((existing_bytes as f64 / total_bytes as f64) * 100.0) as u32
    } else {
        0
    };
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            task_id: task_id.to_string(),
            percent: initial_percent,
            downloaded_bytes: existing_bytes,
            total_bytes,
            speed: 0.0,
        },
    );

    // Stream the response body to the file with buffered writes
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;

    // Write buffer to reduce I/O operations
    let mut write_buffer = Vec::with_capacity(WRITE_BUFFER_SIZE);

    while let Some(chunk_result) = stream.next().await {
        // Check for cancel
        if cancelled.load(Ordering::SeqCst) {
            drop(file);
            let _ = tokio::fs::remove_file(destination).await;
            let _ = db::update_download_status(task_id, "cancelled", None).await;
            let _ = app.emit(
                "download-status-changed",
                DownloadStatusChanged {
                    task_id: task_id.to_string(),
                    status: "cancelled".to_string(),
                    error: None,
                },
            );
            return Err("Download cancelled".to_string());
        }

        // Check for pause - flush buffer and save progress before pausing
        if paused.load(Ordering::SeqCst) {
            // Flush any buffered data to disk
            if !write_buffer.is_empty() {
                file.write_all(&write_buffer)
                    .await
                    .map_err(|e| format!("Failed to write buffer: {}", e))?;
                write_buffer.clear();
            }

            // Get current progress
            let current = downloaded_bytes.load(Ordering::SeqCst);
            let percent = if total_bytes > 0 {
                std::cmp::min(((current as f64 / total_bytes as f64) * 100.0) as u32, 100)
            } else {
                0
            };

            // Save progress to database
            let _ = db::update_download_progress(task_id, current as i64).await;
            let _ = db::update_download_status(task_id, "paused", None).await;

            // Emit progress event so UI shows accurate progress when paused
            let _ = app.emit(
                "download-progress",
                DownloadProgress {
                    task_id: task_id.to_string(),
                    percent,
                    downloaded_bytes: current,
                    total_bytes,
                    speed: 0.0, // Speed is 0 when paused
                },
            );

            // Emit status change event
            let _ = app.emit(
                "download-status-changed",
                DownloadStatusChanged {
                    task_id: task_id.to_string(),
                    status: "paused".to_string(),
                    error: None,
                },
            );
            return Err("Download paused".to_string());
        }

        let chunk = chunk_result.map_err(|e| format!("Failed to read chunk: {}", e))?;

        // Add to write buffer
        write_buffer.extend_from_slice(&chunk);

        // Update progress counter atomically
        downloaded_bytes.fetch_add(chunk.len() as u64, Ordering::SeqCst);

        // Flush buffer when it reaches target size and emit progress
        if write_buffer.len() >= WRITE_BUFFER_SIZE {
            file.write_all(&write_buffer)
                .await
                .map_err(|e| format!("Failed to write buffer: {}", e))?;
            write_buffer.clear();

            // Get current downloaded bytes for accurate progress
            let current_downloaded = downloaded_bytes.load(Ordering::SeqCst);

            // Calculate percent (ensure it's within 0-100)
            let percent = if total_bytes > 0 {
                std::cmp::min(
                    ((current_downloaded as f64 / total_bytes as f64) * 100.0) as u32,
                    100,
                )
            } else {
                0
            };

            let elapsed = start_time.elapsed().as_secs_f64();
            let bytes_this_session = current_downloaded.saturating_sub(start_bytes);
            let speed = if elapsed > 0.0 {
                bytes_this_session as f64 / elapsed
            } else {
                0.0
            };

            let _ = app.emit(
                "download-progress",
                DownloadProgress {
                    task_id: task_id.to_string(),
                    percent,
                    downloaded_bytes: current_downloaded,
                    total_bytes,
                    speed,
                },
            );

            // Update progress in DB on each buffer flush
            let _ = db::update_download_progress(task_id, current_downloaded as i64).await;
        }
    }

    // Flush remaining buffer
    if !write_buffer.is_empty() {
        file.write_all(&write_buffer)
            .await
            .map_err(|e| format!("Failed to write remaining buffer: {}", e))?;
    }

    // Ensure all data is written
    file.flush()
        .await
        .map_err(|e| format!("Failed to flush file: {}", e))?;

    // Final progress update
    let elapsed = start_time.elapsed().as_secs_f64();
    let final_downloaded = downloaded_bytes.load(Ordering::SeqCst);
    let bytes_this_session = final_downloaded - start_bytes;
    let speed = if elapsed > 0.0 {
        bytes_this_session as f64 / elapsed
    } else {
        0.0
    };

    // Emit global progress event (100% complete)
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            task_id: task_id.to_string(),
            percent: 100,
            downloaded_bytes: final_downloaded,
            total_bytes,
            speed,
        },
    );

    // Update DB with completed status
    let _ = db::update_download_progress(task_id, final_downloaded as i64).await;
    let _ = db::update_download_status(task_id, "completed", None).await;

    // Emit status change event for completion
    let _ = app.emit(
        "download-status-changed",
        DownloadStatusChanged {
            task_id: task_id.to_string(),
            status: "completed".to_string(),
            error: None,
        },
    );

    // Emit download-complete event for queue management
    let _ = app.emit("download-complete", task_id.to_string());

    Ok(())
}

/// Spawn a download task
pub(crate) async fn spawn_download_task(
    app: AppHandle,
    session: DownloadSession,
    config: DownloadConfig,
) {
    let task_id = session.id.clone();

    // Register cancel and pause flags
    let cancelled = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    {
        let mut cancel_registry = DOWNLOAD_CANCEL_REGISTRY.lock().await;
        cancel_registry.insert(task_id.clone(), cancelled.clone());
        let mut pause_registry = DOWNLOAD_PAUSE_REGISTRY.lock().await;
        pause_registry.insert(task_id.clone(), paused.clone());
    }

    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            let _ = db::update_download_status(&task_id, "failed", Some(&e.to_string())).await;
            let _ = app.emit(
                "download-status-changed",
                DownloadStatusChanged {
                    task_id: task_id.clone(),
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                },
            );
            return;
        }
    };

    let destination = PathBuf::from(&session.local_path).join(&session.file_name);

    let result = download_file_internal(
        &client,
        &config,
        &session.object_key,
        &destination,
        &task_id,
        session.file_size as u64,
        &app,
        &cancelled,
        &paused,
    )
    .await;

    // Cleanup registries
    {
        let mut cancel_registry = DOWNLOAD_CANCEL_REGISTRY.lock().await;
        cancel_registry.remove(&task_id);
        let mut pause_registry = DOWNLOAD_PAUSE_REGISTRY.lock().await;
        pause_registry.remove(&task_id);
    }

    // Handle result
    if let Err(e) = result {
        if !e.contains("paused") && !e.contains("cancelled") {
            let _ = db::update_download_status(&task_id, "failed", Some(&e)).await;
            // Emit status change event for failure
            let _ = app.emit(
                "download-status-changed",
                DownloadStatusChanged {
                    task_id: task_id.clone(),
                    status: "failed".to_string(),
                    error: Some(e),
                },
            );
        }
    }
}

/// Internal function to process download queue - returns sessions to start
pub(crate) async fn get_pending_sessions_to_start(
    app: &AppHandle,
    bucket: &str,
    account_id: &str,
) -> Result<Vec<DownloadSession>, String> {
    // Count currently active downloads
    let active_count = db::count_active_downloads(bucket, account_id)
        .await
        .map_err(|e| format!("Failed to count active downloads: {}", e))?;

    let slots_available = MAX_CONCURRENT_DOWNLOADS - active_count;
    if slots_available <= 0 {
        return Ok(Vec::new());
    }

    // Get pending tasks up to available slots
    let pending = db::get_pending_downloads(bucket, account_id, slots_available)
        .await
        .map_err(|e| format!("Failed to get pending downloads: {}", e))?;

    // Update status to downloading in DB for each and emit events
    for session in &pending {
        let _ = db::update_download_status(&session.id, "downloading", None).await;
        let _ = app.emit(
            "download-status-changed",
            DownloadStatusChanged {
                task_id: session.id.clone(),
                status: "downloading".to_string(),
                error: None,
            },
        );
    }

    Ok(pending)
}
