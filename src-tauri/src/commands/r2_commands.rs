//! R2 API commands for Tauri frontend

use crate::commands::cache_events::{get_unique_parent_paths, CacheUpdatedEvent};
use crate::commands::delete_cache::{update_cache_after_batch_delete, update_cache_after_delete};
use crate::commands::move_cache::{update_cache_after_batch_move, update_cache_after_move};
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

    let result = r2::list_all_objects_recursive(&r2_config, Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to list all objects: {}", e))?;

    Ok(result.objects)
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
/// Uses a dedicated store task so fetching emits real-time progress without blocking on DB writes.
#[tauri::command]
pub async fn sync_bucket(
    config: R2ConfigInput,
    app: tauri::AppHandle,
) -> Result<SyncResult, String> {
    let r2_config: r2::R2Config = config.into();
    let bucket = r2_config.bucket.clone();
    let account_id = r2_config.account_id.clone();
    let now = chrono::Utc::now().timestamp();

    let _ = app.emit("sync-phase", "fetching");

    // Clear old cache
    db::begin_sync(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to clear cache: {}", e))?;

    // Spawn dedicated store task — DB writes happen here without blocking the fetch loop
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<CachedFile>>(8);
    let store_bucket = bucket.clone();
    let store_account_id = account_id.clone();
    let store_app = app.clone();
    let store_handle = tokio::spawn(async move {
        let mut stored_count: usize = 0;
        while let Some(batch) = rx.recv().await {
            let batch_len = batch.len();
            db::store_file_batch(&store_bucket, &store_account_id, &batch)
                .await
                .map_err(|e| format!("Failed to store files: {}", e))?;
            stored_count += batch_len;
            let _ = store_app.emit("store-progress", stored_count);
        }
        Ok::<usize, String>(stored_count)
    });

    // Fetch pages from S3 — progress emitted after every page
    let client = r2::create_r2_client(&r2_config)
        .await
        .map_err(|e| format!("Failed to create client: {}", e))?;

    let mut fetched_count: usize = 0;
    let mut folder_keys: Vec<String> = Vec::new();
    let mut continuation_token: Option<String> = None;

    loop {
        let mut request = client
            .list_objects_v2()
            .bucket(&r2_config.bucket)
            .max_keys(1000);

        if let Some(token) = &continuation_token {
            request = request.continuation_token(token);
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("Failed to list objects: {}", e))?;
        let is_truncated = response.is_truncated().unwrap_or(false);
        let next_token = response.next_continuation_token().map(|s| s.to_string());

        let mut batch: Vec<CachedFile> = Vec::new();
        for obj in response.contents() {
            if let Some(key) = obj.key() {
                let key = key.to_string();
                if key.ends_with('/') {
                    folder_keys.push(key);
                } else {
                    let (parent_path, name) = db::parse_key(&key);
                    batch.push(CachedFile {
                        bucket: bucket.clone(),
                        account_id: account_id.clone(),
                        key,
                        parent_path,
                        name,
                        size: obj.size().unwrap_or(0),
                        last_modified: obj
                            .last_modified()
                            .map(|dt| dt.to_string())
                            .unwrap_or_default(),
                        synced_at: now,
                    });
                }
            }
        }

        fetched_count += batch.len();
        let _ = app.emit("sync-progress", fetched_count);

        if !batch.is_empty() {
            tx.send(batch)
                .await
                .map_err(|_| "Store task crashed".to_string())?;
        }

        if !is_truncated {
            break;
        }
        continuation_token = next_token;
    }

    // All pages fetched — wait for store task to finish remaining batches
    drop(tx);
    let _ = app.emit("sync-phase", "storing");
    let stored_count = store_handle
        .await
        .map_err(|e| format!("Store task panicked: {}", e))?
        .map_err(|e| format!("Store failed: {}", e))?;

    db::finish_sync(&bucket, &account_id, stored_count)
        .await
        .map_err(|e| format!("Failed to update sync metadata: {}", e))?;

    // Build directory tree from DB
    let _ = app.emit("sync-phase", "indexing");

    let app_clone = app.clone();
    let indexing_callback = move |current: usize, total: usize| {
        let _ = app_clone.emit("indexing-progress", IndexingProgress { current, total });
    };

    db::build_directory_tree_from_db(
        &bucket,
        &account_id,
        &folder_keys,
        Some(indexing_callback),
    )
    .await
    .map_err(|e| format!("Failed to build directory tree: {}", e))?;

    let _ = app.emit("sync-phase", "complete");

    Ok(SyncResult {
        count: stored_count as i32,
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

    // Update cache and emit events (including paths-removed if any folders became empty)
    update_cache_after_delete(&app, &bucket, &account_id, &key).await?;

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

    // Update cache and emit events (including paths-removed if any folders became empty)
    if !deleted_keys.is_empty() {
        if let Err(e) =
            update_cache_after_batch_delete(&app, &bucket, &account_id, &deleted_keys).await
        {
            errors.push(e);
        }
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

    // Update cache and emit events (including paths-created/removed)
    update_cache_after_move(&app, &bucket, &account_id, &old_key, &new_key).await?;

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

    // Update local database for successful moves and emit events
    let operations: Vec<(String, String)> = {
        let moves = successful_moves.lock().await;
        moves
            .iter()
            .map(|op| (op.old_key.clone(), op.new_key.clone()))
            .collect()
    };
    if !operations.is_empty() {
        if let Err(e) = update_cache_after_batch_move(&app, &bucket, &account_id, &operations).await
        {
            let mut errs = errors.lock().await;
            errs.push(e);
        }
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
    let etag = r2::upload_content(&r2_config, &key, content_bytes, content_type.as_deref())
        .await
        .map_err(|e| format!("Failed to upload content: {}", e))?;

    // Update local database cache
    let last_modified = chrono::Utc::now().to_rfc3339();

    // Update the file record and get (size_delta, is_new_file)
    let (size_delta, is_new_file) =
        db::update_cached_file(&bucket, &account_id, &key, new_size, &last_modified)
            .await
            .map_err(|e| format!("Failed to update file cache: {}", e))?;

    // Update directory tree (all ancestor folders)
    db::update_directory_tree_for_file(
        &bucket,
        &account_id,
        &key,
        size_delta,
        &last_modified,
        is_new_file,
    )
    .await
    .map_err(|e| format!("Failed to update directory tree: {}", e))?;

    // Emit cache-updated event
    let _ = app.emit(
        "cache-updated",
        CacheUpdatedEvent {
            action: "update".to_string(),
            affected_paths: get_unique_parent_paths(&[key]),
        },
    );

    Ok(etag)
}
