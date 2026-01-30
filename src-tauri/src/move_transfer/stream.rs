use crate::db;
use crate::providers::{aws, minio};
use crate::r2;
use futures_util::{future::join_all, StreamExt};
use log::{debug, info};
use reqwest::{Body, Client};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};
use tokio::sync::Semaphore;

use super::config::MoveConfig;
use super::state::update_move_status;
use super::types::{MoveProgress, MAX_CONCURRENT_PARTS};
use crate::db::MoveSession;

const MULTIPART_THRESHOLD: u64 = 100 * 1024 * 1024;
const PART_SIZE: u64 = 20 * 1024 * 1024;

async fn generate_download_url(config: &MoveConfig, key: &str) -> Result<String, String> {
    match config {
        MoveConfig::R2(cfg) => r2::generate_presigned_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate R2 URL: {}", e)),
        MoveConfig::Aws(cfg) => aws::generate_presigned_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate AWS URL: {}", e)),
        MoveConfig::Minio(cfg) => minio::generate_presigned_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate MinIO URL: {}", e)),
        MoveConfig::Rustfs(cfg) => minio::generate_presigned_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate RustFS URL: {}", e)),
    }
}

async fn generate_upload_url(config: &MoveConfig, key: &str) -> Result<String, String> {
    match config {
        MoveConfig::R2(cfg) => r2::generate_presigned_put_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate R2 upload URL: {}", e)),
        MoveConfig::Aws(cfg) => aws::generate_presigned_put_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate AWS upload URL: {}", e)),
        MoveConfig::Minio(cfg) => minio::generate_presigned_put_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate MinIO upload URL: {}", e)),
        MoveConfig::Rustfs(cfg) => minio::generate_presigned_put_url(cfg, key, 3600)
            .await
            .map_err(|e| format!("Failed to generate RustFS upload URL: {}", e)),
    }
}

async fn resolve_source_size(
    client: &Client,
    download_url: &str,
    known_size: u64,
) -> Result<u64, String> {
    if known_size > 0 {
        return Ok(known_size);
    }

    let response = client
        .get(download_url)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .map_err(|e| format!("Size probe failed: {}", e))?;

    if response.status().as_u16() == 206 {
        if let Some(range) = response.headers().get(reqwest::header::CONTENT_RANGE) {
            if let Ok(range_str) = range.to_str() {
                if let Some(total) = range_str.split('/').nth(1) {
                    if total != "*" {
                        if let Ok(total) = total.parse::<u64>() {
                            let _ = response.bytes().await;
                            return Ok(total);
                        }
                    }
                }
            }
        }
    }

    let length = response.content_length().unwrap_or(0);
    let _ = response.bytes().await;
    Ok(length)
}

async fn fetch_range_bytes(
    client: &Client,
    download_url: &str,
    start: u64,
    end: u64,
) -> Result<Vec<u8>, String> {
    let response = client
        .get(download_url)
        .header("Range", format!("bytes={}-{}", start, end))
        .send()
        .await
        .map_err(|e| format!("Range request failed: {}", e))?;

    if response.status().as_u16() != 206 && !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("Range request failed: {} - {}", status, text));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read range bytes: {}", e))?;
    Ok(bytes.to_vec())
}

async fn initiate_multipart_upload(config: &MoveConfig, key: &str) -> Result<String, String> {
    match config {
        MoveConfig::R2(cfg) => r2::upload::initiate_multipart_upload(cfg, key, None)
            .await
            .map_err(|e| format!("Failed to initiate R2 multipart upload: {}", e)),
        MoveConfig::Aws(cfg) => aws::initiate_multipart_upload(cfg, key, None)
            .await
            .map_err(|e| format!("Failed to initiate AWS multipart upload: {}", e)),
        MoveConfig::Minio(cfg) => minio::initiate_multipart_upload(cfg, key, None)
            .await
            .map_err(|e| format!("Failed to initiate MinIO multipart upload: {}", e)),
        MoveConfig::Rustfs(cfg) => minio::initiate_multipart_upload(cfg, key, None)
            .await
            .map_err(|e| format!("Failed to initiate RustFS multipart upload: {}", e)),
    }
}

async fn upload_part(
    config: &MoveConfig,
    key: &str,
    upload_id: &str,
    part_number: i32,
    data: Vec<u8>,
) -> Result<String, String> {
    match config {
        MoveConfig::R2(cfg) => r2::upload::upload_part(cfg, key, upload_id, part_number, data)
            .await
            .map_err(|e| format!("Failed to upload R2 part: {}", e)),
        MoveConfig::Aws(cfg) => aws::upload_part(cfg, key, upload_id, part_number, data)
            .await
            .map_err(|e| format!("Failed to upload AWS part: {}", e)),
        MoveConfig::Minio(cfg) => minio::upload_part(cfg, key, upload_id, part_number, data)
            .await
            .map_err(|e| format!("Failed to upload MinIO part: {}", e)),
        MoveConfig::Rustfs(cfg) => minio::upload_part(cfg, key, upload_id, part_number, data)
            .await
            .map_err(|e| format!("Failed to upload RustFS part: {}", e)),
    }
}

async fn complete_multipart_upload(
    config: &MoveConfig,
    key: &str,
    upload_id: &str,
    parts: Vec<(i32, String)>,
) -> Result<(), String> {
    match config {
        MoveConfig::R2(cfg) => r2::upload::complete_multipart_upload(cfg, key, upload_id, parts)
            .await
            .map_err(|e| format!("Failed to complete R2 multipart upload: {}", e)),
        MoveConfig::Aws(cfg) => aws::complete_multipart_upload(cfg, key, upload_id, parts)
            .await
            .map_err(|e| format!("Failed to complete AWS multipart upload: {}", e)),
        MoveConfig::Minio(cfg) => minio::complete_multipart_upload(cfg, key, upload_id, parts)
            .await
            .map_err(|e| format!("Failed to complete MinIO multipart upload: {}", e)),
        MoveConfig::Rustfs(cfg) => minio::complete_multipart_upload(cfg, key, upload_id, parts)
            .await
            .map_err(|e| format!("Failed to complete RustFS multipart upload: {}", e)),
    }
}

async fn abort_multipart_upload(config: &MoveConfig, key: &str, upload_id: &str) {
    let _ = match config {
        MoveConfig::R2(cfg) => r2::upload::abort_multipart_upload(cfg, key, upload_id).await,
        MoveConfig::Aws(cfg) => aws::abort_multipart_upload(cfg, key, upload_id).await,
        MoveConfig::Minio(cfg) => minio::abort_multipart_upload(cfg, key, upload_id).await,
        MoveConfig::Rustfs(cfg) => minio::abort_multipart_upload(cfg, key, upload_id).await,
    };
}

#[allow(clippy::too_many_arguments)]
async fn stream_single_put(
    client: &Client,
    download_url: &str,
    upload_url: &str,
    total_bytes: u64,
    session: &MoveSession,
    app: &AppHandle,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
) -> Result<u64, String> {
    info!(
        "single_put_start: {} total_bytes={}",
        session.id, total_bytes
    );
    let response = client
        .get(download_url)
        .send()
        .await
        .map_err(|e| format!("Download request failed: {}", e))?;

    if !response.status().is_success() && response.status().as_u16() != 206 {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("Download failed: {} - {}", status, text));
    }

    let transferred = Arc::new(AtomicU64::new(0));
    let transferred_for_stream = transferred.clone();
    let abort_reason = Arc::new(AtomicU8::new(0)); // 0 none, 1 paused, 2 cancelled
    let abort_reason_for_stream = abort_reason.clone();
    let cancelled = cancelled.clone();
    let paused = paused.clone();
    let start_time = std::time::Instant::now();
    let task_id = session.id.clone();
    let app_handle = app.clone();
    let next_log_percent = Arc::new(AtomicU8::new(10));
    let next_log_percent_for_stream = next_log_percent.clone();
    let next_db_percent = Arc::new(AtomicU8::new(5));
    let next_db_percent_for_stream = next_db_percent.clone();
    let persist_99_sent = Arc::new(AtomicBool::new(false));
    let persist_99_sent_for_stream = persist_99_sent.clone();

    let stream = response.bytes_stream().map(move |chunk_result| {
        if cancelled.load(Ordering::SeqCst) {
            abort_reason_for_stream.store(2, Ordering::SeqCst);
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        if paused.load(Ordering::SeqCst) {
            abort_reason_for_stream.store(1, Ordering::SeqCst);
            return Err(io::Error::new(io::ErrorKind::Interrupted, "paused"));
        }

        let chunk = chunk_result.map_err(io::Error::other)?;
        let new_total = transferred_for_stream.fetch_add(chunk.len() as u64, Ordering::SeqCst)
            + chunk.len() as u64;

        let percent = if total_bytes > 0 {
            std::cmp::min(
                ((new_total as f64 / total_bytes as f64) * 100.0).round() as u32,
                100,
            )
        } else {
            0
        };
        let display_percent = if percent >= 100 { 99 } else { percent };
        let elapsed = start_time.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            new_total as f64 / elapsed
        } else {
            0.0
        };

        let next_threshold = next_log_percent_for_stream.load(Ordering::SeqCst);
        if display_percent >= next_threshold as u32
            && next_log_percent_for_stream
                .compare_exchange(
                    next_threshold,
                    next_threshold.saturating_add(10),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
        {
            debug!(
                "single_put_progress: {} percent={} bytes={}",
                task_id, display_percent, new_total
            );
        }

        let mut should_persist = false;
        let next_db_threshold = next_db_percent_for_stream.load(Ordering::SeqCst);
        if display_percent >= next_db_threshold as u32
            && next_db_percent_for_stream
                .compare_exchange(
                    next_db_threshold,
                    next_db_threshold.saturating_add(5),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
        {
            should_persist = true;
        }
        if display_percent == 99
            && persist_99_sent_for_stream
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            should_persist = true;
        }

        let _ = app_handle.emit(
            "move-progress",
            MoveProgress {
                task_id: task_id.clone(),
                phase: "uploading".to_string(),
                percent: display_percent,
                transferred_bytes: new_total,
                total_bytes,
                speed,
            },
        );

        if should_persist {
            let task_id_update = task_id.clone();
            tokio::spawn(async move {
                let _ = db::update_move_progress(&task_id_update, display_percent as i64).await;
            });
        }

        Ok(chunk)
    });

    let body = Body::wrap_stream(stream);
    let mut request = client.put(upload_url).body(body);
    if total_bytes > 0 {
        request = request.header("content-length", total_bytes);
    }

    info!(
        "single_put_upload_start: {} total_bytes={}",
        session.id, total_bytes
    );
    let upload_response = request.send().await;
    if let Err(err) = upload_response {
        match abort_reason.load(Ordering::SeqCst) {
            1 => {
                update_move_status(app, &session.id, "paused", None).await;
                return Err("Move paused".to_string());
            }
            2 => {
                update_move_status(app, &session.id, "cancelled", None).await;
                return Err("Move cancelled".to_string());
            }
            _ => {
                update_move_status(app, &session.id, "error", Some(err.to_string())).await;
                return Err(format!("Upload request failed: {}", err));
            }
        }
    }

    let response = upload_response.unwrap();
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        update_move_status(app, &session.id, "error", Some(text.clone())).await;
        return Err(format!("Upload failed: {} - {}", status, text));
    }
    debug!(
        "single_put_upload_done: {} status={}",
        session.id,
        response.status()
    );

    let final_bytes = transferred.load(Ordering::SeqCst);
    info!(
        "move_upload_finish: {} bytes={} total_bytes={}",
        session.id, final_bytes, total_bytes
    );

    Ok(if total_bytes > 0 {
        total_bytes
    } else {
        final_bytes
    })
}

/// Result of a single part upload operation
struct PartUploadResult {
    part_number: i32,
    etag: String,
}

#[allow(clippy::too_many_arguments)]
async fn stream_multipart(
    client: &Client,
    session: &MoveSession,
    download_url: &str,
    dest_config: &MoveConfig,
    total_bytes: u64,
    app: &AppHandle,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
) -> Result<u64, String> {
    info!(
        "multipart_start: {} total_bytes={}",
        session.id, total_bytes
    );
    let (upload_id, mut part_size) = match db::get_move_upload_session(&session.id).await {
        Ok(Some((upload_id, part_size))) => (upload_id, part_size as u64),
        _ => {
            let upload_id = initiate_multipart_upload(dest_config, &session.dest_key).await?;
            let part_size = PART_SIZE;
            let _ = db::save_move_upload_session(&session.id, &upload_id, part_size as i64).await;
            (upload_id, part_size)
        }
    };

    if part_size == 0 {
        part_size = PART_SIZE;
    }

    let total_parts = total_bytes.div_ceil(part_size) as i32;
    info!(
        "multipart_plan: {} total_parts={} part_size={}",
        session.id, total_parts, part_size
    );
    let mut completed_parts: HashMap<i32, String> = HashMap::new();
    let mut completed_sizes: HashMap<i32, i64> = HashMap::new();

    if let Ok(parts) = db::get_move_upload_parts(&session.id).await {
        for (part_number, etag, size) in parts {
            completed_parts.insert(part_number, etag);
            completed_sizes.insert(part_number, size);
        }
    }

    let uploaded_bytes = Arc::new(AtomicU64::new(
        completed_sizes.values().map(|s| *s as u64).sum(),
    ));
    let start_time = std::time::Instant::now();

    // Collect parts that need to be uploaded
    let pending_parts: Vec<i32> = (1..=total_parts)
        .filter(|p| !completed_parts.contains_key(p))
        .collect();

    if pending_parts.is_empty() {
        info!(
            "multipart_resume_complete: {} all parts already uploaded",
            session.id
        );
        // All parts already completed, just finalize
        let mut parts: Vec<(i32, String)> = completed_parts.into_iter().collect();
        parts.sort_by_key(|(part_number, _)| *part_number);
        complete_multipart_upload(dest_config, &session.dest_key, &upload_id, parts).await?;
        let _ = db::delete_move_upload_parts(&session.id).await;
        let _ = db::delete_move_upload_session(&session.id).await;
        return Ok(total_bytes);
    }

    // Semaphore to limit concurrent part uploads
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_PARTS));
    let error_flag = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::with_capacity(pending_parts.len());

    for part_number in pending_parts {
        // Check cancel/pause before spawning
        if cancelled.load(Ordering::SeqCst) {
            error_flag.store(true, Ordering::SeqCst);
            break;
        }
        if paused.load(Ordering::SeqCst) {
            error_flag.store(true, Ordering::SeqCst);
            break;
        }

        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let client = client.clone();
        let download_url = download_url.to_string();
        let dest_config = dest_config.clone();
        let dest_key = session.dest_key.clone();
        let upload_id = upload_id.clone();
        let session_id = session.id.clone();
        let task_id = session.id.clone();
        let app = app.clone();
        let cancelled = cancelled.clone();
        let paused = paused.clone();
        let uploaded_bytes = uploaded_bytes.clone();
        let error_flag = error_flag.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit; // Hold permit until done

            // Check cancel/pause
            if cancelled.load(Ordering::SeqCst) || paused.load(Ordering::SeqCst) {
                return Err("Interrupted".to_string());
            }

            // Skip if error already occurred
            if error_flag.load(Ordering::SeqCst) {
                return Err("Aborted due to earlier error".to_string());
            }

            let start = (part_number as u64 - 1) * part_size;
            let end = std::cmp::min(start + part_size - 1, total_bytes - 1);
            let part_timer = Instant::now();
            debug!(
                "multipart_part_start: {} part={} range={}..={}",
                task_id, part_number, start, end
            );

            // Download part
            let download_timer = Instant::now();
            let bytes = match fetch_range_bytes(&client, &download_url, start, end).await {
                Ok(b) => b,
                Err(e) => {
                    error_flag.store(true, Ordering::SeqCst);
                    return Err(e);
                }
            };
            debug!(
                "multipart_part_download_done: {} part={} bytes={} elapsed_ms={}",
                task_id,
                part_number,
                bytes.len(),
                download_timer.elapsed().as_millis()
            );

            // Check again before upload
            if cancelled.load(Ordering::SeqCst) || paused.load(Ordering::SeqCst) {
                return Err("Interrupted".to_string());
            }

            // Upload part
            let upload_timer = Instant::now();
            let etag = match upload_part(
                &dest_config,
                &dest_key,
                &upload_id,
                part_number,
                bytes.clone(),
            )
            .await
            {
                Ok(e) => e,
                Err(e) => {
                    error_flag.store(true, Ordering::SeqCst);
                    return Err(e);
                }
            };
            debug!(
                "multipart_part_upload_done: {} part={} elapsed_ms={}",
                task_id,
                part_number,
                upload_timer.elapsed().as_millis()
            );

            // Save part to DB
            let part_size_bytes = bytes.len() as u64;
            let _ =
                db::save_move_upload_part(&session_id, part_number, &etag, part_size_bytes as i64)
                    .await;

            // Update progress atomically
            let new_uploaded =
                uploaded_bytes.fetch_add(part_size_bytes, Ordering::SeqCst) + part_size_bytes;
            let percent = if total_bytes > 0 {
                std::cmp::min(
                    ((new_uploaded as f64 / total_bytes as f64) * 100.0).round() as u32,
                    100,
                )
            } else {
                0
            };
            let display_percent = if percent >= 100 { 99 } else { percent };

            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                new_uploaded as f64 / elapsed
            } else {
                0.0
            };

            // Update DB progress BEFORE emitting event (throttled - only every 5%)
            // This avoids race condition where frontend shows progress but DB hasn't updated
            if display_percent % 5 == 0 || display_percent == 99 {
                let _ = db::update_move_progress(&session_id, display_percent as i64).await;
            }

            let _ = app.emit(
                "move-progress",
                MoveProgress {
                    task_id: task_id.clone(),
                    phase: "uploading".to_string(),
                    percent: display_percent,
                    transferred_bytes: new_uploaded,
                    total_bytes,
                    speed,
                },
            );
            debug!(
                "multipart_part_done: {} part={} total_elapsed_ms={}",
                task_id,
                part_number,
                part_timer.elapsed().as_millis()
            );

            Ok(PartUploadResult { part_number, etag })
        });

        handles.push(handle);
    }

    // Wait for all parts to complete
    let results: Vec<_> = join_all(handles).await;

    // Check for cancel/pause
    if cancelled.load(Ordering::SeqCst) {
        update_move_status(app, &session.id, "cancelled", None).await;
        abort_multipart_upload(dest_config, &session.dest_key, &upload_id).await;
        let _ = db::delete_move_upload_parts(&session.id).await;
        let _ = db::delete_move_upload_session(&session.id).await;
        return Err("Move cancelled".to_string());
    }
    if paused.load(Ordering::SeqCst) {
        update_move_status(app, &session.id, "paused", None).await;
        return Err("Move paused".to_string());
    }

    // Collect results and check for errors
    for result in results {
        match result {
            Ok(Ok(part_result)) => {
                completed_parts.insert(part_result.part_number, part_result.etag);
            }
            Ok(Err(e)) if e.contains("Interrupted") => {
                // Cancel/pause handled above
            }
            Ok(Err(e)) => {
                update_move_status(app, &session.id, "error", Some(e.clone())).await;
                return Err(e);
            }
            Err(e) => {
                let err_msg = format!("Part upload task failed: {}", e);
                update_move_status(app, &session.id, "error", Some(err_msg.clone())).await;
                return Err(err_msg);
            }
        }
    }

    // Verify all parts completed
    let mut parts: Vec<(i32, String)> = completed_parts.into_iter().collect();
    parts.sort_by_key(|(part_number, _)| *part_number);

    if parts.len() != total_parts as usize {
        update_move_status(
            app,
            &session.id,
            "error",
            Some("Missing upload parts".to_string()),
        )
        .await;
        return Err("Missing upload parts".to_string());
    }

    // Complete multipart upload
    if let Err(err) =
        complete_multipart_upload(dest_config, &session.dest_key, &upload_id, parts).await
    {
        update_move_status(app, &session.id, "error", Some(err.clone())).await;
        return Err(err);
    }
    info!("multipart_complete: {}", session.id);

    // Cleanup
    let _ = db::delete_move_upload_parts(&session.id).await;
    let _ = db::delete_move_upload_session(&session.id).await;

    info!(
        "move_upload_finish: {} bytes={} total_bytes={}",
        session.id, total_bytes, total_bytes
    );

    Ok(total_bytes)
}

pub(crate) async fn stream_transfer_without_temp(
    client: &Client,
    session: &MoveSession,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    app: &AppHandle,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
) -> Result<u64, String> {
    let prepare_start = Instant::now();
    info!(
        "move_prepare_start: {} source_key={} dest_key={}",
        session.id, session.source_key, session.dest_key
    );
    let download_url = generate_download_url(source_config, &session.source_key).await?;
    let upload_url = generate_upload_url(dest_config, &session.dest_key).await?;

    let known_size = if session.file_size > 0 {
        session.file_size as u64
    } else {
        db::get_cached_file_size(
            &session.source_bucket,
            &session.source_account_id,
            &session.source_key,
        )
        .await
        .unwrap_or(0) as u64
    };

    let total_bytes = resolve_source_size(client, &download_url, known_size).await?;
    update_move_status(app, &session.id, "uploading", None).await;
    info!(
        "move_prepare_done: {} total_bytes={} multipart={}",
        session.id,
        total_bytes,
        total_bytes >= MULTIPART_THRESHOLD
    );
    info!(
        "move_prepare_time: {} duration_ms={}",
        session.id,
        prepare_start.elapsed().as_millis()
    );

    let _ = app.emit(
        "move-progress",
        MoveProgress {
            task_id: session.id.clone(),
            phase: "uploading".to_string(),
            percent: 0,
            transferred_bytes: 0,
            total_bytes,
            speed: 0.0,
        },
    );

    let upload_start = Instant::now();
    let result = if total_bytes >= MULTIPART_THRESHOLD {
        stream_multipart(
            client,
            session,
            &download_url,
            dest_config,
            total_bytes,
            app,
            cancelled,
            paused,
        )
        .await
    } else {
        stream_single_put(
            client,
            &download_url,
            &upload_url,
            total_bytes,
            session,
            app,
            cancelled,
            paused,
        )
        .await
    };

    let elapsed_ms = upload_start.elapsed().as_millis();
    match &result {
        Ok(bytes) => info!(
            "move_upload_time: {} bytes={} elapsed_ms={}",
            session.id, bytes, elapsed_ms
        ),
        Err(err) => info!(
            "move_upload_time: {} elapsed_ms={} error={}",
            session.id, elapsed_ms, err
        ),
    }

    result
}
