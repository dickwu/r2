//! R2 Tauri commands

use super::types::R2Config;
use super::upload::upload_file;
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

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
