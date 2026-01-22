//! Download Tauri commands

use crate::db::{self, DownloadSession};
use crate::providers::{aws, minio, rustfs};
use chrono::Utc;
use serde::Deserialize;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter};

use super::types::{DownloadBatchOperation, DownloadStatusChanged, DownloadTaskDeleted};
use super::worker::{
    get_pending_sessions_to_start, spawn_download_task, DownloadConfig, DOWNLOAD_CANCEL_REGISTRY,
    DOWNLOAD_PAUSE_REGISTRY,
};

#[derive(Debug, Deserialize)]
pub struct DownloadConfigInput {
    pub provider: String,
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: Option<String>,
    pub endpoint_scheme: Option<String>,
    pub endpoint_host: Option<String>,
    pub force_path_style: Option<bool>,
}

fn build_download_config(input: &DownloadConfigInput) -> Result<DownloadConfig, String> {
    match input.provider.as_str() {
        "aws" => {
            let region = input
                .region
                .as_ref()
                .ok_or_else(|| "AWS region is required".to_string())?
                .to_string();
            Ok(DownloadConfig::Aws(aws::AwsConfig {
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
                region,
                endpoint_scheme: input.endpoint_scheme.clone(),
                endpoint_host: input.endpoint_host.clone(),
                force_path_style: input.force_path_style.unwrap_or(false),
            }))
        }
        "minio" => {
            let endpoint_scheme = input
                .endpoint_scheme
                .clone()
                .unwrap_or_else(|| "https".to_string());
            let endpoint_host = input
                .endpoint_host
                .clone()
                .ok_or_else(|| "MinIO endpoint host is required".to_string())?;
            Ok(DownloadConfig::Minio(minio::MinioConfig {
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
                endpoint_scheme,
                endpoint_host,
                force_path_style: input.force_path_style.unwrap_or(true),
            }))
        }
        "rustfs" => {
            let endpoint_scheme = input
                .endpoint_scheme
                .clone()
                .unwrap_or_else(|| "https".to_string());
            let endpoint_host = input
                .endpoint_host
                .clone()
                .ok_or_else(|| "RustFS endpoint host is required".to_string())?;
            Ok(DownloadConfig::Rustfs(rustfs::RustfsConfig {
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
                endpoint_scheme,
                endpoint_host,
                force_path_style: true,
            }))
        }
        "r2" => Ok(DownloadConfig::R2(crate::r2::R2Config {
            account_id: input.account_id.clone(),
            bucket: input.bucket.clone(),
            access_key_id: input.access_key_id.clone(),
            secret_access_key: input.secret_access_key.clone(),
        })),
        _ => Err(format!("Unsupported provider: {}", input.provider)),
    }
}

/// Create a download session in the database
/// If file_size is 0, looks up the size from file cache
#[tauri::command]
pub async fn create_download_task(
    task_id: String,
    object_key: String,
    file_name: String,
    file_size: i64,
    local_path: String,
    bucket: String,
    account_id: String,
) -> Result<(), String> {
    // If file_size is 0, try to get it from the file cache
    let actual_file_size = if file_size == 0 {
        db::get_cached_file_size(&bucket, &account_id, &object_key)
            .await
            .unwrap_or(0)
    } else {
        file_size
    };

    let now = Utc::now().timestamp();
    let session = DownloadSession {
        id: task_id,
        object_key,
        file_name,
        file_size: actual_file_size,
        downloaded_bytes: 0,
        local_path,
        bucket,
        account_id,
        status: "pending".to_string(),
        error: None,
        created_at: now,
        updated_at: now,
    };

    db::create_download_session(&session)
        .await
        .map_err(|e| format!("Failed to create download session: {}", e))
}

/// Process the download queue - start pending downloads up to MAX_CONCURRENT_DOWNLOADS
#[tauri::command]
pub async fn start_download_queue(
    app: AppHandle,
    config: DownloadConfigInput,
) -> Result<i64, String> {
    let download_config = build_download_config(&config)?;

    // Get sessions to start (this updates their status in DB and emits events)
    let sessions = get_pending_sessions_to_start(&app, &config.bucket, &config.account_id).await?;
    let started_count = sessions.len() as i64;

    // Spawn download tasks for each session
    for session in sessions {
        let app_clone = app.clone();
        let config_clone = download_config.clone();
        tokio::spawn(async move {
            spawn_download_task(app_clone, session, config_clone).await;
        });
    }

    Ok(started_count)
}

/// Start all paused downloads
#[tauri::command]
pub async fn start_all_downloads(
    app: AppHandle,
    config: DownloadConfigInput,
) -> Result<i64, String> {
    // First, set all paused tasks to pending in DB
    let resumed_count = db::resume_all_downloads(&config.bucket, &config.account_id)
        .await
        .map_err(|e| format!("Failed to resume downloads: {}", e))?;

    // Emit batch operation event for UI to reload
    let _ = app.emit(
        "download-batch-operation",
        DownloadBatchOperation {
            operation: "resume_all".to_string(),
            bucket: config.bucket.clone(),
            account_id: config.account_id.clone(),
        },
    );

    // Then get sessions to start and spawn tasks
    let download_config = build_download_config(&config)?;
    let sessions = get_pending_sessions_to_start(&app, &config.bucket, &config.account_id).await?;

    for session in sessions {
        let app_clone = app.clone();
        let config_clone = download_config.clone();
        tokio::spawn(async move {
            spawn_download_task(app_clone, session, config_clone).await;
        });
    }

    Ok(resumed_count)
}

/// Pause all active downloads
#[tauri::command]
pub async fn pause_all_downloads(
    app: AppHandle,
    bucket: String,
    account_id: String,
) -> Result<i64, String> {
    // Set pause flag for all active downloads
    {
        let registry = DOWNLOAD_PAUSE_REGISTRY.lock().await;
        for (_, paused) in registry.iter() {
            paused.store(true, Ordering::SeqCst);
        }
    }

    // Also update DB directly for any that might not be in registry
    let paused_count = db::pause_all_downloads(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to pause downloads: {}", e))?;

    // Emit batch operation event for UI to reload
    let _ = app.emit(
        "download-batch-operation",
        DownloadBatchOperation {
            operation: "pause_all".to_string(),
            bucket: bucket.clone(),
            account_id: account_id.clone(),
        },
    );

    Ok(paused_count)
}

/// Pause a single download
#[tauri::command]
pub async fn pause_download(app: AppHandle, task_id: String) -> Result<(), String> {
    let registry = DOWNLOAD_PAUSE_REGISTRY.lock().await;
    if let Some(paused) = registry.get(&task_id) {
        paused.store(true, Ordering::SeqCst);
        // Event will be emitted by download_file_internal when it detects pause
    } else {
        // If not actively downloading, just update status in DB and emit event
        let _ = db::update_download_status(&task_id, "paused", None).await;
        let _ = app.emit(
            "download-status-changed",
            DownloadStatusChanged {
                task_id: task_id.clone(),
                status: "paused".to_string(),
                error: None,
            },
        );
    }
    Ok(())
}

/// Resume a single paused download (set status to pending)
#[tauri::command]
pub async fn resume_download(app: AppHandle, task_id: String) -> Result<(), String> {
    db::update_download_status(&task_id, "pending", None)
        .await
        .map_err(|e| format!("Failed to resume download: {}", e))?;

    // Emit status change event
    let _ = app.emit(
        "download-status-changed",
        DownloadStatusChanged {
            task_id: task_id.clone(),
            status: "pending".to_string(),
            error: None,
        },
    );

    Ok(())
}

/// Cancel a download (removes the partial file)
#[tauri::command]
pub async fn cancel_download(app: AppHandle, task_id: String) -> Result<(), String> {
    let registry = DOWNLOAD_CANCEL_REGISTRY.lock().await;
    if let Some(cancelled) = registry.get(&task_id) {
        cancelled.store(true, Ordering::SeqCst);
        // Event will be emitted by download_file_internal when it detects cancel
    } else {
        // If not actively downloading, just update status in DB and emit event
        let _ = db::update_download_status(&task_id, "cancelled", None).await;
        let _ = app.emit(
            "download-status-changed",
            DownloadStatusChanged {
                task_id: task_id.clone(),
                status: "cancelled".to_string(),
                error: None,
            },
        );
    }
    Ok(())
}

/// Delete a download task from the database
#[tauri::command]
pub async fn delete_download_task(app: AppHandle, task_id: String) -> Result<(), String> {
    // Cancel if active
    {
        let registry = DOWNLOAD_CANCEL_REGISTRY.lock().await;
        if let Some(cancelled) = registry.get(&task_id) {
            cancelled.store(true, Ordering::SeqCst);
        }
    }

    // Delete from DB
    db::delete_download_session(&task_id)
        .await
        .map_err(|e| format!("Failed to delete download task: {}", e))?;

    // Emit delete event
    let _ = app.emit(
        "download-task-deleted",
        DownloadTaskDeleted {
            task_id: task_id.clone(),
        },
    );

    Ok(())
}

/// Get all download sessions for a bucket
#[tauri::command]
pub async fn get_download_tasks(
    bucket: String,
    account_id: String,
) -> Result<Vec<DownloadSession>, String> {
    db::get_download_sessions_for_bucket(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to get download tasks: {}", e))
}

/// Clear all finished download tasks (completed, failed, cancelled)
#[tauri::command]
pub async fn clear_finished_downloads(
    app: AppHandle,
    bucket: String,
    account_id: String,
) -> Result<i64, String> {
    let deleted_count = db::delete_finished_downloads(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to clear finished downloads: {}", e))?;

    // Emit batch operation event for UI
    let _ = app.emit(
        "download-batch-operation",
        DownloadBatchOperation {
            operation: "clear_finished".to_string(),
            bucket: bucket.clone(),
            account_id: account_id.clone(),
        },
    );

    Ok(deleted_count)
}

/// Clear all download tasks (only when no active downloads)
#[tauri::command]
pub async fn clear_all_downloads(
    app: AppHandle,
    bucket: String,
    account_id: String,
) -> Result<i64, String> {
    // Check if there are active downloads
    let active_count = db::count_active_downloads(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to count active downloads: {}", e))?;

    if active_count > 0 {
        return Err("Cannot clear all downloads while downloads are active".to_string());
    }

    let deleted_count = db::delete_all_downloads(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to clear all downloads: {}", e))?;

    // Emit batch operation event for UI
    let _ = app.emit(
        "download-batch-operation",
        DownloadBatchOperation {
            operation: "clear_all".to_string(),
            bucket: bucket.clone(),
            account_id: account_id.clone(),
        },
    );

    Ok(deleted_count)
}

/// Select a folder for downloading files using native dialog
#[tauri::command]
pub async fn select_download_folder(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();

    app.dialog()
        .file()
        .set_title("Select Download Folder")
        .pick_folder(move |folder_path| {
            let result = folder_path.map(|p| p.to_string());
            let _ = tx.send(result);
        });

    rx.await.map_err(|_| "Dialog was closed".to_string())
}
