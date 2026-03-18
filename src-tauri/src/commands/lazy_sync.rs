use crate::db::{self, CachedFile};
use crate::r2;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use tauri::Emitter;

// ============ Types ============

#[derive(Debug, Deserialize)]
pub struct LazyListInput {
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub prefix: String, // "" for root, "folder/" for subfolder
}

#[derive(Debug, Clone, Serialize)]
pub struct LazyListResult {
    pub files: Vec<LazyFileItem>,
    pub folders: Vec<String>,
    pub prefix: String,
    pub from_cache: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LazyFileItem {
    pub key: String,
    pub name: String,
    pub size: i64,
    pub last_modified: String,
}

// ============ list_prefix Command ============

/// Lazy-list a single prefix using delimiter="/".
/// If cache is fresh (< 60s), serves from SQLite. Otherwise hits S3.
#[tauri::command]
pub async fn list_prefix(
    input: LazyListInput,
    app: tauri::AppHandle,
) -> Result<LazyListResult, String> {
    let bucket = &input.bucket;
    let account_id = &input.account_id;
    let prefix = &input.prefix;

    // Check if prefix was recently synced (within 60 seconds)
    let cached_time = db::prefix_sync::get_prefix_sync_time(bucket, account_id, prefix)
        .await
        .map_err(|e| format!("DB error: {}", e))?;

    let now = chrono::Utc::now().timestamp();
    const STALE_THRESHOLD_SECS: i64 = 60;

    if let Some(synced_at) = cached_time {
        if now - synced_at < STALE_THRESHOLD_SECS {
            // Serve from cache
            let contents = db::get_folder_contents(bucket, account_id, prefix)
                .await
                .map_err(|e| format!("DB error: {}", e))?;

            return Ok(LazyListResult {
                files: contents
                    .files
                    .into_iter()
                    .map(|f| LazyFileItem {
                        name: f.name,
                        key: f.key,
                        size: f.size,
                        last_modified: f.last_modified,
                    })
                    .collect(),
                folders: contents.folders,
                prefix: prefix.clone(),
                from_cache: true,
            });
        }
    }

    // Cache is stale or missing -- fetch from S3
    let r2_config = r2::R2Config {
        account_id: input.account_id.clone(),
        bucket: input.bucket.clone(),
        access_key_id: input.access_key_id.clone(),
        secret_access_key: input.secret_access_key.clone(),
    };

    let client = r2::create_r2_client(&r2_config)
        .await
        .map_err(|e| format!("Failed to create client: {}", e))?;

    // Paginate with delimiter to get immediate children only
    let mut all_files: Vec<CachedFile> = Vec::new();
    let mut all_folders: Vec<String> = Vec::new();
    let mut continuation_token: Option<String> = None;
    let mut page_count = 0;

    loop {
        let mut request = client
            .list_objects_v2()
            .bucket(bucket)
            .delimiter("/")
            .max_keys(1000);

        if !prefix.is_empty() {
            request = request.prefix(prefix);
        }

        if let Some(token) = &continuation_token {
            request = request.continuation_token(token);
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("S3 list failed: {}", e))?;

        page_count += 1;

        // Collect files (objects at this level)
        for obj in response.contents() {
            if let Some(key) = obj.key() {
                let key = key.to_string();
                if key.ends_with('/') {
                    continue; // Skip folder marker objects
                }
                let (parent_path, name) = db::parse_key(&key);
                all_files.push(CachedFile {
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

        // Collect folders (common prefixes)
        for cp in response.common_prefixes() {
            if let Some(p) = cp.prefix() {
                all_folders.push(p.to_string());
            }
        }

        // Emit progress for multi-page prefixes
        if page_count > 1 {
            let _ = app.emit(
                "folder-load-progress",
                serde_json::json!({
                    "pages": page_count,
                    "items": all_files.len() + all_folders.len(),
                }),
            );
        }

        let is_truncated = response.is_truncated().unwrap_or(false);
        if !is_truncated {
            break;
        }
        continuation_token = response.next_continuation_token().map(|s| s.to_string());
    }

    // Cache results in SQLite
    db::upsert_prefix_files(bucket, account_id, prefix, &all_files)
        .await
        .map_err(|e| format!("Failed to cache files: {}", e))?;

    // Upsert folder entries into directory_tree for this prefix
    for folder in &all_folders {
        db::ensure_directory_node(bucket, account_id, folder)
            .await
            .map_err(|e| format!("Failed to upsert directory node: {}", e))?;
    }

    // Record sync time
    db::prefix_sync::set_prefix_sync_time(
        bucket,
        account_id,
        prefix,
        all_files.len() as i32,
        all_folders.len() as i32,
    )
    .await
    .map_err(|e| format!("Failed to record sync time: {}", e))?;

    let result = LazyListResult {
        files: all_files
            .into_iter()
            .map(|f| LazyFileItem {
                name: f.name,
                key: f.key,
                size: f.size,
                last_modified: f.last_modified,
            })
            .collect(),
        folders: all_folders,
        prefix: prefix.clone(),
        from_cache: false,
    };

    Ok(result)
}

// ============ Background Sync (Task 3) ============

// Global cancellation token for background sync (one per app)
static BACKGROUND_CANCEL: LazyLock<Arc<AtomicBool>> =
    LazyLock::new(|| Arc::new(AtomicBool::new(false)));

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundSyncProgress {
    pub objects_fetched: usize,
    pub estimated_total: Option<usize>,
    pub is_running: bool,
    pub speed: f64, // objects/second
}

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundSyncResult {
    pub total_objects: usize,
    pub cancelled: bool,
}

#[tauri::command]
pub async fn start_background_sync(
    input: LazyListInput,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Reset cancellation flag
    BACKGROUND_CANCEL.store(false, Ordering::SeqCst);

    // Spawn background task -- returns immediately
    tokio::spawn(async move {
        let result = run_background_sync(input, app.clone()).await;
        match result {
            Ok(sync_result) => {
                let _ = app.emit("background-sync-complete", sync_result);
            }
            Err(e) => {
                let _ = app.emit("background-sync-error", e);
            }
        }
    });

    Ok(())
}

async fn run_background_sync(
    input: LazyListInput,
    app: tauri::AppHandle,
) -> Result<BackgroundSyncResult, String> {
    let r2_config = r2::R2Config {
        account_id: input.account_id.clone(),
        bucket: input.bucket.clone(),
        access_key_id: input.access_key_id.clone(),
        secret_access_key: input.secret_access_key.clone(),
    };

    let bucket = r2_config.bucket.clone();
    let account_id = r2_config.account_id.clone();

    // Begin sync (staging table)
    db::begin_sync(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to begin sync: {}", e))?;

    // Create S3 client
    let client = r2::create_r2_client(&r2_config)
        .await
        .map_err(|e| format!("Failed to create client: {}", e))?;

    // Spawn store task
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<CachedFile>>(8);
    let store_bucket = bucket.clone();
    let store_account_id = account_id.clone();
    let store_handle = tokio::spawn(async move {
        let mut stored_count: usize = 0;
        while let Some(batch) = rx.recv().await {
            let batch_len = batch.len();
            db::store_file_batch(&store_bucket, &store_account_id, &batch)
                .await
                .map_err(|e| format!("Failed to store files: {}", e))?;
            stored_count += batch_len;
        }
        Ok::<usize, String>(stored_count)
    });

    // Fetch loop with progress emission
    let mut fetched_count: usize = 0;
    let mut folder_keys: Vec<String> = Vec::new();
    let mut continuation_token: Option<String> = None;
    let start_time = std::time::Instant::now();

    loop {
        // Check cancellation
        if BACKGROUND_CANCEL.load(Ordering::SeqCst) {
            drop(tx);
            return Ok(BackgroundSyncResult {
                total_objects: fetched_count,
                cancelled: true,
            });
        }

        let mut request = client
            .list_objects_v2()
            .bucket(&bucket)
            .max_keys(1000);

        if let Some(token) = &continuation_token {
            request = request.continuation_token(token);
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("S3 list failed: {}", e))?;

        let is_truncated = response.is_truncated().unwrap_or(false);
        let next_token = response.next_continuation_token().map(|s| s.to_string());
        let now = chrono::Utc::now().timestamp();

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

        // Calculate speed
        let elapsed = start_time.elapsed().as_secs_f64().max(0.001);
        let speed = fetched_count as f64 / elapsed;

        // Emit progress every page
        let _ = app.emit(
            "background-sync-progress",
            BackgroundSyncProgress {
                objects_fetched: fetched_count,
                estimated_total: if is_truncated {
                    None // S3 doesn't provide total count
                } else {
                    Some(fetched_count)
                },
                is_running: true,
                speed,
            },
        );

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

    // Wait for store task
    drop(tx);
    let stored_count = store_handle
        .await
        .map_err(|e| format!("Store task panicked: {}", e))?
        .map_err(|e| format!("Store failed: {}", e))?;

    // Finish sync (swap staging -> live)
    db::finish_sync(&bucket, &account_id, stored_count)
        .await
        .map_err(|e| format!("Failed to finish sync: {}", e))?;

    // Build directory tree
    db::build_directory_tree_from_db(&bucket, &account_id, &folder_keys, None::<fn(usize, usize)>)
        .await
        .map_err(|e| format!("Failed to build tree: {}", e))?;

    Ok(BackgroundSyncResult {
        total_objects: fetched_count,
        cancelled: false,
    })
}

#[tauri::command]
pub async fn cancel_background_sync() -> Result<(), String> {
    BACKGROUND_CANCEL.store(true, Ordering::SeqCst);
    Ok(())
}
