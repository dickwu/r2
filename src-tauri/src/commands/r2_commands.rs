//! R2 API commands for Tauri frontend

use crate::r2;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::Semaphore;

// ============ Types ============

#[derive(Debug, Deserialize)]
pub struct R2ConfigInput {
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl From<R2ConfigInput> for r2::R2Config {
    fn from(input: R2ConfigInput) -> Self {
        r2::R2Config {
            account_id: input.account_id,
            bucket: input.bucket,
            access_key_id: input.access_key_id,
            secret_access_key: input.secret_access_key,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListObjectsInput {
    pub config: R2ConfigInput,
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    pub continuation_token: Option<String>,
    pub max_keys: Option<i32>,
}

// ============ List Commands ============

#[tauri::command]
pub async fn list_r2_buckets(
    account_id: String,
    access_key_id: String,
    secret_access_key: String,
) -> Result<Vec<r2::R2Bucket>, String> {
    let config = r2::R2Config {
        account_id,
        bucket: String::new(),
        access_key_id,
        secret_access_key,
    };

    r2::list_buckets(&config)
        .await
        .map_err(|e| format!("Failed to list buckets: {}", e))
}

#[tauri::command]
pub async fn list_r2_objects(input: ListObjectsInput) -> Result<r2::ListObjectsResult, String> {
    let config: r2::R2Config = input.config.into();

    r2::list_objects(
        &config,
        input.prefix.as_deref(),
        input.delimiter.as_deref(),
        input.continuation_token.as_deref(),
        input.max_keys,
    )
    .await
    .map_err(|e| format!("Failed to list objects: {}", e))
}

#[tauri::command]
pub async fn list_all_r2_objects(
    config: R2ConfigInput,
    app: tauri::AppHandle,
) -> Result<Vec<r2::R2Object>, String> {
    let r2_config: r2::R2Config = config.into();

    let _ = app.emit("sync-phase", "fetching");

    let app_clone = app.clone();
    let progress_callback = Box::new(move |count: usize| {
        let _ = app_clone.emit("sync-progress", count);
    });

    r2::list_all_objects_recursive(&r2_config, Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to list all objects: {}", e))
}

// ============ Folder List Command ============

#[derive(Debug, Clone, Serialize)]
pub struct FolderLoadProgress {
    pub pages: usize,
    pub items: usize,
}

#[tauri::command]
pub async fn list_folder_r2_objects(
    config: R2ConfigInput,
    prefix: Option<String>,
    app: tauri::AppHandle,
) -> Result<r2::ListObjectsResult, String> {
    let r2_config: r2::R2Config = config.into();

    let _ = app.emit("folder-load-phase", "loading");

    let app_clone = app.clone();
    let progress_callback = Box::new(move |pages: usize, items: usize| {
        let _ = app_clone.emit("folder-load-progress", FolderLoadProgress { pages, items });
    });

    let result = r2::list_folder_objects(&r2_config, prefix.as_deref(), Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to list folder objects: {}", e))?;

    let _ = app.emit("folder-load-phase", "complete");

    Ok(result)
}

// ============ Delete Commands ============

#[tauri::command]
pub async fn delete_r2_object(config: R2ConfigInput, key: String) -> Result<(), String> {
    let r2_config: r2::R2Config = config.into();

    r2::delete_object(&r2_config, &key)
        .await
        .map_err(|e| format!("Failed to delete object: {}", e))
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchDeleteProgress {
    pub completed: usize,
    pub total: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchDeleteResult {
    pub deleted: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

#[tauri::command]
pub async fn batch_delete_r2_objects(
    config: R2ConfigInput,
    keys: Vec<String>,
    app: tauri::AppHandle,
) -> Result<BatchDeleteResult, String> {
    let r2_config: r2::R2Config = config.into();
    let total = keys.len();

    if total == 0 {
        return Ok(BatchDeleteResult {
            deleted: 0,
            failed: 0,
            errors: vec![],
        });
    }

    let mut completed = 0;
    let mut failed = 0;
    let mut errors: Vec<String> = vec![];

    const BATCH_SIZE: usize = 1000;

    for chunk in keys.chunks(BATCH_SIZE) {
        let batch_keys: Vec<String> = chunk.to_vec();
        let batch_count = batch_keys.len();

        match r2::delete_objects(&r2_config, batch_keys).await {
            Ok(_) => {
                completed += batch_count;
            }
            Err(e) => {
                failed += batch_count;
                errors.push(format!("Batch delete failed: {}", e));
            }
        }

        let _ = app.emit(
            "batch-delete-progress",
            BatchDeleteProgress {
                completed,
                total,
                failed,
            },
        );
    }

    Ok(BatchDeleteResult {
        deleted: completed,
        failed,
        errors,
    })
}

// ============ Rename/Move Commands ============

#[tauri::command]
pub async fn rename_r2_object(
    config: R2ConfigInput,
    old_key: String,
    new_key: String,
) -> Result<(), String> {
    let r2_config: r2::R2Config = config.into();

    r2::rename_object(&r2_config, &old_key, &new_key)
        .await
        .map_err(|e| format!("Failed to rename object: {}", e))
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchMoveProgress {
    pub completed: usize,
    pub total: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MoveOperation {
    pub old_key: String,
    pub new_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchMoveResult {
    pub moved: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

#[tauri::command]
pub async fn batch_move_r2_objects(
    config: R2ConfigInput,
    operations: Vec<MoveOperation>,
    app: tauri::AppHandle,
) -> Result<BatchMoveResult, String> {
    let r2_config: r2::R2Config = config.into();
    let total = operations.len();

    if total == 0 {
        return Ok(BatchMoveResult {
            moved: 0,
            failed: 0,
            errors: vec![],
        });
    }

    const CONCURRENCY: usize = 6;
    let semaphore = Arc::new(Semaphore::new(CONCURRENCY));
    let completed = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));

    let mut handles = Vec::new();

    for op in operations {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        let config = r2_config.clone();
        let completed = completed.clone();
        let failed = failed.clone();
        let errors = errors.clone();
        let app = app.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit;

            if op.old_key == op.new_key {
                completed.fetch_add(1, Ordering::SeqCst);
            } else {
                match r2::rename_object(&config, &op.old_key, &op.new_key).await {
                    Ok(_) => {
                        completed.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(e) => {
                        failed.fetch_add(1, Ordering::SeqCst);
                        let mut errs = errors.lock().await;
                        errs.push(format!("{}: {}", op.old_key, e));
                    }
                }
            }

            let current_completed = completed.load(Ordering::SeqCst);
            let current_failed = failed.load(Ordering::SeqCst);
            let _ = app.emit(
                "batch-move-progress",
                BatchMoveProgress {
                    completed: current_completed + current_failed,
                    total,
                    failed: current_failed,
                },
            );
        });

        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    let final_completed = completed.load(Ordering::SeqCst);
    let final_failed = failed.load(Ordering::SeqCst);
    let final_errors = errors.lock().await.clone();

    Ok(BatchMoveResult {
        moved: final_completed,
        failed: final_failed,
        errors: final_errors,
    })
}

// ============ Signed URL ============

#[tauri::command]
pub async fn generate_signed_url(
    config: R2ConfigInput,
    key: String,
    expires_in: Option<u64>,
) -> Result<String, String> {
    let r2_config: r2::R2Config = config.into();
    let expires_in_secs = expires_in.unwrap_or(3600);

    r2::generate_presigned_url(&r2_config, &key, expires_in_secs)
        .await
        .map_err(|e| format!("Failed to generate signed URL: {}", e))
}
