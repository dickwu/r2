//! R2 API commands for Tauri frontend

use crate::db::{self, CachedFile};
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

// ============ Cache Update Event ============

/// Event emitted when local cache is updated after R2 operations.
/// Frontend can listen to this to refresh affected views.
#[derive(Debug, Clone, Serialize)]
pub struct CacheUpdatedEvent {
    pub action: String,              // "delete" | "move" | "update"
    pub affected_paths: Vec<String>, // Parent folder paths that changed
}

/// Extract unique parent paths from a list of file keys
fn get_unique_parent_paths(keys: &[String]) -> Vec<String> {
    let mut paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    
    for key in keys {
        let parent = if let Some(last_slash) = key.rfind('/') {
            key[..=last_slash].to_string()
        } else {
            String::new() // Root
        };
        paths.insert(parent);
    }
    
    paths.into_iter().collect()
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

// ============ Sync Command ============

#[derive(Debug, Clone, Serialize)]
pub struct SyncResult {
    pub count: i32,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize)]
struct IndexingProgress {
    current: usize,
    total: usize,
}

/// Sync bucket: fetch all objects from R2, store in cache, and build directory tree.
/// This consolidates 3 frontend calls into 1 backend operation.
#[tauri::command]
pub async fn sync_bucket(
    config: R2ConfigInput,
    app: tauri::AppHandle,
) -> Result<SyncResult, String> {
    let r2_config: r2::R2Config = config.into();
    let bucket = r2_config.bucket.clone();
    let account_id = r2_config.account_id.clone();

    // Phase 1: Fetch all objects from R2
    let _ = app.emit("sync-phase", "fetching");

    let app_clone = app.clone();
    let progress_callback = Box::new(move |count: usize| {
        let _ = app_clone.emit("sync-progress", count);
    });

    let objects = r2::list_all_objects_recursive(&r2_config, Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to fetch objects: {}", e))?;

    let count = objects.len() as i32;

    // Phase 2: Store files in SQLite cache
    let _ = app.emit("sync-phase", "storing");

    let now = chrono::Utc::now().timestamp();
    let cached_files: Vec<CachedFile> = objects
        .into_iter()
        .map(|obj| CachedFile {
            bucket: bucket.clone(),
            account_id: account_id.clone(),
            key: obj.key,
            parent_path: String::new(), // Computed in store_all_files from key
            name: String::new(),        // Computed in store_all_files from key
            size: obj.size,
            last_modified: obj.last_modified,
            synced_at: now,
        })
        .collect();

    db::store_all_files(&bucket, &account_id, &cached_files)
        .await
        .map_err(|e| format!("Failed to store files: {}", e))?;

    // Phase 3: Build directory tree
    let _ = app.emit("sync-phase", "indexing");

    let app_clone = app.clone();
    let indexing_callback = move |current: usize, total: usize| {
        let _ = app_clone.emit("indexing-progress", IndexingProgress { current, total });
    };

    db::build_directory_tree(&bucket, &account_id, &cached_files, Some(indexing_callback))
        .await
        .map_err(|e| format!("Failed to build directory tree: {}", e))?;

    // Phase 4: Complete
    let _ = app.emit("sync-phase", "complete");

    Ok(SyncResult {
        count,
        timestamp: now,
    })
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
pub async fn delete_r2_object(
    config: R2ConfigInput,
    key: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    let r2_config: r2::R2Config = config.into();

    // Delete from R2
    r2::delete_object(&r2_config, &key)
        .await
        .map_err(|e| format!("Failed to delete object: {}", e))?;

    // Update local database cache
    let file_size = db::delete_cached_file(&bucket, &account_id, &key)
        .await
        .map_err(|e| format!("Failed to update file cache: {}", e))?;

    if file_size > 0 {
        db::update_directory_tree_for_delete(&bucket, &account_id, &key, file_size)
            .await
            .map_err(|e| format!("Failed to update directory tree: {}", e))?;
    }

    // Emit cache-updated event
    let _ = app.emit("cache-updated", CacheUpdatedEvent {
        action: "delete".to_string(),
        affected_paths: get_unique_parent_paths(&[key]),
    });

    Ok(())
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
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    let r2_config: r2::R2Config = config.into();
    let total = keys.len();

    if total == 0 {
        return Ok(BatchDeleteResult {
            deleted: 0,
            failed: 0,
            errors: vec![],
        });
    }

    // Get file sizes before deleting (for directory tree updates)
    let file_sizes = db::delete_cached_files_batch(&bucket, &account_id, &keys)
        .await
        .unwrap_or_default();

    let mut completed = 0;
    let mut failed = 0;
    let mut errors: Vec<String> = vec![];
    let mut deleted_keys: Vec<String> = vec![];

    const BATCH_SIZE: usize = 1000;

    for chunk in keys.chunks(BATCH_SIZE) {
        let batch_keys: Vec<String> = chunk.to_vec();
        let batch_count = batch_keys.len();

        match r2::delete_objects(&r2_config, batch_keys.clone()).await {
            Ok(_) => {
                completed += batch_count;
                deleted_keys.extend(batch_keys);
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

    // Update directory tree for successfully deleted files
    for key in &deleted_keys {
        if let Some(&size) = file_sizes.get(key) {
            let _ = db::update_directory_tree_for_delete(&bucket, &account_id, key, size).await;
        }
    }

    // Emit cache-updated event
    if !deleted_keys.is_empty() {
        let _ = app.emit("cache-updated", CacheUpdatedEvent {
            action: "delete".to_string(),
            affected_paths: get_unique_parent_paths(&deleted_keys),
        });
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
    app: tauri::AppHandle,
) -> Result<(), String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    let r2_config: r2::R2Config = config.into();

    // Rename in R2
    r2::rename_object(&r2_config, &old_key, &new_key)
        .await
        .map_err(|e| format!("Failed to rename object: {}", e))?;

    // Update local database cache
    if let Some((size, last_modified)) = db::move_cached_file(&bucket, &account_id, &old_key, &new_key)
        .await
        .map_err(|e| format!("Failed to update file cache: {}", e))?
    {
        db::update_directory_tree_for_move(&bucket, &account_id, &old_key, &new_key, size, &last_modified)
            .await
            .map_err(|e| format!("Failed to update directory tree: {}", e))?;
    }

    // Emit cache-updated event
    let _ = app.emit("cache-updated", CacheUpdatedEvent {
        action: "move".to_string(),
        affected_paths: get_unique_parent_paths(&[old_key, new_key]),
    });

    Ok(())
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
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
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
    // Track successful moves for database updates
    let successful_moves = Arc::new(tokio::sync::Mutex::new(Vec::<MoveOperation>::new()));

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
        let successful_moves = successful_moves.clone();
        let app = app.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit;

            if op.old_key == op.new_key {
                completed.fetch_add(1, Ordering::SeqCst);
            } else {
                match r2::rename_object(&config, &op.old_key, &op.new_key).await {
                    Ok(_) => {
                        completed.fetch_add(1, Ordering::SeqCst);
                        let mut moves = successful_moves.lock().await;
                        moves.push(op);
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

    // Update local database for successful moves
    let moves = successful_moves.lock().await;
    let mut affected_keys: Vec<String> = Vec::new();
    for op in moves.iter() {
        if let Ok(Some((size, last_modified))) = 
            db::move_cached_file(&bucket, &account_id, &op.old_key, &op.new_key).await
        {
            let _ = db::update_directory_tree_for_move(
                &bucket, &account_id, &op.old_key, &op.new_key, size, &last_modified
            ).await;
            affected_keys.push(op.old_key.clone());
            affected_keys.push(op.new_key.clone());
        }
    }

    // Emit cache-updated event
    if !affected_keys.is_empty() {
        let _ = app.emit("cache-updated", CacheUpdatedEvent {
            action: "move".to_string(),
            affected_paths: get_unique_parent_paths(&affected_keys),
        });
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

// ============ Upload Content ============

#[tauri::command]
pub async fn upload_r2_content(
    config: R2ConfigInput,
    key: String,
    content: String,
    content_type: Option<String>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    let r2_config: r2::R2Config = config.into();

    // Calculate content size before converting to bytes
    let content_bytes = content.into_bytes();
    let new_size = content_bytes.len() as i64;

    // Upload to R2
    let etag = r2::upload_content(
        &r2_config,
        &key,
        content_bytes,
        content_type.as_deref(),
    )
    .await
    .map_err(|e| format!("Failed to upload content: {}", e))?;

    // Update local database cache
    let last_modified = chrono::Utc::now().to_rfc3339();

    // Update the file record and get size delta
    let size_delta = db::update_cached_file(&bucket, &account_id, &key, new_size, &last_modified)
        .await
        .map_err(|e| format!("Failed to update file cache: {}", e))?;

    // Update directory tree (all ancestor folders)
    db::update_directory_tree_for_file(&bucket, &account_id, &key, size_delta, &last_modified)
        .await
        .map_err(|e| format!("Failed to update directory tree: {}", e))?;

    // Emit cache-updated event
    let _ = app.emit("cache-updated", CacheUpdatedEvent {
        action: "update".to_string(),
        affected_paths: get_unique_parent_paths(&[key]),
    });

    Ok(etag)
}
