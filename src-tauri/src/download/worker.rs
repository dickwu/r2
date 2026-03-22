//! Download worker - internal download logic with streaming and progress tracking
//!
//! Dispatches downloads based on file size:
//! - Files < 10MB: single-stream download (original path)
//! - Files >= 10MB: multi-chunk parallel download via range-dl crate

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

use super::types::{
    ChunkProgressInfo, DownloadChunkProgressEvent, DownloadProgress, DownloadStatusChanged,
    MAX_CONCURRENT_DOWNLOADS,
};

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
#[allow(clippy::too_many_arguments)]
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
    let presigned_url = generate_presigned_url_for_config(config, key, 3600).await?;

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

/// Minimum file size (10 MB) to use chunked parallel download.
/// Files smaller than this use the single-stream path.
const CHUNKED_DOWNLOAD_THRESHOLD: u64 = 10 * 1024 * 1024;

/// Generate a presigned URL for any provider (shared helper to avoid DRY violations).
pub(crate) async fn generate_presigned_url_for_config(
    config: &DownloadConfig,
    key: &str,
    ttl: u64,
) -> Result<String, String> {
    match config {
        DownloadConfig::R2(cfg) => crate::r2::generate_presigned_url(cfg, key, ttl)
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e)),
        DownloadConfig::Aws(cfg) => aws::generate_presigned_url(cfg, key, ttl)
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e)),
        DownloadConfig::Minio(cfg) => minio::generate_presigned_url(cfg, key, ttl)
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e)),
        DownloadConfig::Rustfs(cfg) => rustfs::generate_presigned_url(cfg, key, ttl)
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e)),
    }
}

/// Download a file using the range-dl crate's multi-chunk parallel engine.
/// Emits both legacy aggregate events (backwards-compatible) and new chunk events.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn download_file_chunked(
    config: &DownloadConfig,
    key: &str,
    destination: &PathBuf,
    task_id: &str,
    file_size: u64,
    app: &AppHandle,
) -> Result<(), String> {
    use range_dl::{ChunkEvent, DownloadTarget, RangeDownloadConfig, RangeDownloader};

    // Build URL provider closure that generates fresh presigned URLs
    let cfg = config.clone();
    let key_owned = key.to_string();
    let url_provider: range_dl::UrlProvider = Box::new(move || {
        let cfg = cfg.clone();
        let key = key_owned.clone();
        Box::pin(async move { generate_presigned_url_for_config(&cfg, &key, 3600).await })
    });

    let dl_config = RangeDownloadConfig::default();
    let target = DownloadTarget {
        file_size,
        destination: destination.clone(),
    };

    let downloader = RangeDownloader::new(url_provider, target, dl_config);
    let (mut rx, control) = downloader.start().await?;

    let task_id_owned = task_id.to_string();

    // Register cancel/pause control in the registries so commands.rs can signal them
    {
        // We bridge the old AtomicBool-based system to the new watch/CancellationToken system
        // by spawning a watcher task that polls the old registries
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let pause_flag = Arc::new(AtomicBool::new(false));
        {
            let mut cancel_reg = DOWNLOAD_CANCEL_REGISTRY.lock().await;
            cancel_reg.insert(task_id_owned.clone(), cancel_flag.clone());
            let mut pause_reg = DOWNLOAD_PAUSE_REGISTRY.lock().await;
            pause_reg.insert(task_id_owned.clone(), pause_flag.clone());
        }

        // Bridge task: polls old AtomicBool flags and forwards to new control handles
        let cancel_token = control.cancel.clone();
        let pause_sender = control.pause.clone();
        let cancel_flag_clone = cancel_flag.clone();
        let pause_flag_clone = pause_flag.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
            loop {
                interval.tick().await;
                if cancel_flag_clone.load(Ordering::SeqCst) {
                    cancel_token.cancel();
                    break;
                }
                if pause_flag_clone.load(Ordering::SeqCst) {
                    let _ = pause_sender.send(true);
                    break;
                }
                // Stop polling if the token is already cancelled
                if cancel_token.is_cancelled() {
                    break;
                }
            }
        });
    }

    // Process events from the range-dl engine
    while let Some(event) = rx.recv().await {
        match event {
            ChunkEvent::Progress {
                chunks,
                aggregate_speed,
                aggregate_downloaded,
                total_bytes,
            } => {
                // Emit legacy aggregate progress event (backwards-compatible)
                let percent = if total_bytes > 0 {
                    std::cmp::min(
                        ((aggregate_downloaded as f64 / total_bytes as f64) * 100.0) as u32,
                        100,
                    )
                } else {
                    0
                };
                let _ = app.emit(
                    "download-progress",
                    DownloadProgress {
                        task_id: task_id_owned.clone(),
                        percent,
                        downloaded_bytes: aggregate_downloaded,
                        total_bytes,
                        speed: aggregate_speed,
                    },
                );

                // Emit new chunk-level progress event
                let chunk_infos: Vec<ChunkProgressInfo> = chunks
                    .iter()
                    .map(|c| ChunkProgressInfo {
                        chunk_id: c.chunk_id,
                        start: c.start,
                        end: c.end,
                        downloaded_bytes: c.downloaded_bytes,
                        speed: c.speed,
                        status: format!("{:?}", c.status),
                    })
                    .collect();
                let _ = app.emit(
                    "download-chunk-progress",
                    DownloadChunkProgressEvent {
                        task_id: task_id_owned.clone(),
                        chunks: chunk_infos,
                        aggregate_speed,
                        aggregate_downloaded,
                        total_bytes,
                    },
                );

                // Periodic DB progress update (the engine handles throttling to 200ms)
                let _ =
                    db::update_download_progress(&task_id_owned, aggregate_downloaded as i64).await;
            }
            ChunkEvent::ChunkComplete { chunk_id } => {
                log::info!(
                    "Download {}: chunk {} completed",
                    task_id_owned,
                    chunk_id
                );
            }
            ChunkEvent::ChunkRetry {
                chunk_id,
                attempt,
                error,
            } => {
                log::warn!(
                    "Download {}: chunk {} retry #{}: {}",
                    task_id_owned,
                    chunk_id,
                    attempt,
                    error
                );
            }
            ChunkEvent::ChunkFailed { chunk_id, error } => {
                log::error!(
                    "Download {}: chunk {} failed: {}",
                    task_id_owned,
                    chunk_id,
                    error
                );
                // Don't fail the whole download — the engine handles failure threshold
            }
            ChunkEvent::Complete {
                total_bytes,
                elapsed_secs,
                avg_speed,
            } => {
                log::info!(
                    "Download {} complete: {} bytes in {:.1}s ({}/s)",
                    task_id_owned,
                    total_bytes,
                    elapsed_secs,
                    format_speed(avg_speed)
                );

                // Emit final progress (100%)
                let _ = app.emit(
                    "download-progress",
                    DownloadProgress {
                        task_id: task_id_owned.clone(),
                        percent: 100,
                        downloaded_bytes: total_bytes,
                        total_bytes,
                        speed: avg_speed,
                    },
                );

                // Update DB
                let _ =
                    db::update_download_progress(&task_id_owned, total_bytes as i64).await;
                let _ = db::update_download_status(&task_id_owned, "completed", None).await;

                // Emit status change
                let _ = app.emit(
                    "download-status-changed",
                    DownloadStatusChanged {
                        task_id: task_id_owned.clone(),
                        status: "completed".to_string(),
                        error: None,
                    },
                );
                let _ = app.emit("download-complete", task_id_owned.clone());
                break;
            }
            ChunkEvent::Paused { chunks_state: _ } => {
                let _ = db::update_download_status(&task_id_owned, "paused", None).await;
                let _ = app.emit(
                    "download-status-changed",
                    DownloadStatusChanged {
                        task_id: task_id_owned.clone(),
                        status: "paused".to_string(),
                        error: None,
                    },
                );
                // Cleanup registries
                cleanup_registries(&task_id_owned).await;
                return Err("Download paused".to_string());
            }
            ChunkEvent::Cancelled => {
                let _ = db::update_download_status(&task_id_owned, "cancelled", None).await;
                let _ = app.emit(
                    "download-status-changed",
                    DownloadStatusChanged {
                        task_id: task_id_owned.clone(),
                        status: "cancelled".to_string(),
                        error: None,
                    },
                );
                // Cleanup registries
                cleanup_registries(&task_id_owned).await;
                return Err("Download cancelled".to_string());
            }
        }
    }

    // Cleanup registries
    cleanup_registries(&task_id_owned).await;
    Ok(())
}

async fn cleanup_registries(task_id: &str) {
    let mut cancel_reg = DOWNLOAD_CANCEL_REGISTRY.lock().await;
    cancel_reg.remove(task_id);
    let mut pause_reg = DOWNLOAD_PAUSE_REGISTRY.lock().await;
    pause_reg.remove(task_id);
}

fn format_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1024.0 {
        format!("{:.0} B", bytes_per_sec)
    } else if bytes_per_sec < 1024.0 * 1024.0 {
        format!("{:.1} KB", bytes_per_sec / 1024.0)
    } else if bytes_per_sec < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", bytes_per_sec / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes_per_sec / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Spawn a download task.
/// Dispatches to chunked parallel download for files >= 10MB,
/// or single-stream download for smaller files.
pub(crate) async fn spawn_download_task(
    app: AppHandle,
    session: DownloadSession,
    config: DownloadConfig,
) {
    let task_id = session.id.clone();
    let file_size = session.file_size as u64;
    let destination = PathBuf::from(&session.local_path).join(&session.file_name);

    // Dispatch based on file size
    let result = if file_size >= CHUNKED_DOWNLOAD_THRESHOLD {
        // Multi-chunk parallel download via range-dl
        log::info!(
            "Download {}: using chunked download for {} bytes",
            task_id,
            file_size
        );
        download_file_chunked(
            &config,
            &session.object_key,
            &destination,
            &task_id,
            file_size,
            &app,
        )
        .await
    } else {
        // Single-stream download (original path for small files)
        log::info!(
            "Download {}: using single-stream for {} bytes",
            task_id,
            file_size
        );

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
                let _ =
                    db::update_download_status(&task_id, "failed", Some(&e.to_string())).await;
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

        let result = download_file_internal(
            &client,
            &config,
            &session.object_key,
            &destination,
            &task_id,
            file_size,
            &app,
            &cancelled,
            &paused,
        )
        .await;

        // Cleanup registries for single-stream path
        cleanup_registries(&task_id).await;

        result
    };

    // Handle errors (both paths)
    if let Err(e) = result {
        if !e.contains("paused") && !e.contains("cancelled") {
            let _ = db::update_download_status(&task_id, "failed", Some(&e)).await;
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
