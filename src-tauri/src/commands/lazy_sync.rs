use crate::db::{self, CachedFile};
use crate::providers::aws;
use crate::providers::minio;
use crate::r2;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    // Provider-aware fields (all optional for backward compatibility)
    pub provider: Option<String>,
    pub endpoint_scheme: Option<String>,
    pub endpoint_host: Option<String>,
    pub force_path_style: Option<bool>,
    pub region: Option<String>,
    pub force_refresh: Option<bool>,
}

// ============ Provider-Aware Client Factory ============

async fn create_client_for_input(input: &LazyListInput) -> Result<aws_sdk_s3::Client, String> {
    let provider = input.provider.as_deref().unwrap_or("r2");
    match provider {
        "minio" | "rustfs" => {
            let config = minio::MinioConfig {
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
                endpoint_scheme: input
                    .endpoint_scheme
                    .clone()
                    .unwrap_or_else(|| "http".into()),
                endpoint_host: input.endpoint_host.clone().unwrap_or_default(),
                force_path_style: input.force_path_style.unwrap_or(true),
            };
            minio::create_minio_client(&config)
                .await
                .map_err(|e| format!("Failed to create {} client: {}", provider, e))
        }
        "aws" => {
            let config = aws::AwsConfig {
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
                region: input.region.clone().unwrap_or_else(|| "us-east-1".into()),
                endpoint_scheme: input.endpoint_scheme.clone(),
                endpoint_host: input.endpoint_host.clone(),
                force_path_style: input.force_path_style.unwrap_or(false),
            };
            aws::create_aws_client(&config)
                .await
                .map_err(|e| format!("Failed to create aws client: {}", e))
        }
        _ => {
            let config = r2::R2Config {
                account_id: input.account_id.clone(),
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
            };
            r2::create_r2_client(&config)
                .await
                .map_err(|e| format!("Failed to create r2 client: {}", e))
        }
    }
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

    if !input.force_refresh.unwrap_or(false) {
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
    }

    // Cache is stale or missing -- fetch from S3
    let client = create_client_for_input(&input).await?;

    // Paginate with delimiter to get immediate children only
    let mut all_files: Vec<CachedFile> = Vec::new();
    let mut all_folders: Vec<String> = Vec::new();
    let mut continuation_token: Option<String> = None;
    let mut page_count = 0;

    loop {
        let create_request = || {
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

            request
        };

        let response = {
            let _list_guard = S3_LIST_LOCK.lock().await;
            match create_request().send().await {
                Ok(response) => response,
                Err(first_error) => create_request().send().await.map_err(|retry_error| {
                    format!(
                        "S3 list failed after retry: {}; first attempt: {}",
                        retry_error, first_error
                    )
                })?,
            }
        };

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
static S3_LIST_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));
static BACKGROUND_RUN_ID: AtomicU64 = AtomicU64::new(0);
static BACKGROUND_SYNC_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

fn is_background_run_active(run_id: u64) -> bool {
    BACKGROUND_RUN_ID.load(Ordering::SeqCst) == run_id && !BACKGROUND_CANCEL.load(Ordering::SeqCst)
}

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
    let run_id = BACKGROUND_RUN_ID.fetch_add(1, Ordering::SeqCst) + 1;

    // Reset cancellation flag
    BACKGROUND_CANCEL.store(false, Ordering::SeqCst);

    // Spawn background task -- returns immediately
    tokio::spawn(async move {
        let result = run_background_sync(input, app.clone(), run_id).await;
        let is_active = is_background_run_active(run_id);

        match result {
            Ok(sync_result) if is_active && !sync_result.cancelled => {
                let _ = app.emit("background-sync-complete", sync_result);
            }
            Err(e) if is_active => {
                let _ = app.emit("background-sync-error", e);
            }
            _ => {}
        }
    });

    Ok(())
}

async fn run_background_sync(
    input: LazyListInput,
    app: tauri::AppHandle,
    run_id: u64,
) -> Result<BackgroundSyncResult, String> {
    let _sync_guard = BACKGROUND_SYNC_LOCK.lock().await;

    let bucket = input.bucket.clone();
    let account_id = input.account_id.clone();

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            total_objects: 0,
            cancelled: true,
        });
    }

    // Begin sync (staging table)
    db::begin_sync(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to begin sync: {}", e))?;

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            total_objects: 0,
            cancelled: true,
        });
    }

    // Create S3 client (provider-aware)
    let client = create_client_for_input(&input).await?;

    // Spawn store task
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<CachedFile>>(8);
    let store_bucket = bucket.clone();
    let store_account_id = account_id.clone();
    let store_handle = tokio::spawn(async move {
        let mut stored_count: usize = 0;
        while let Some(batch) = rx.recv().await {
            if !is_background_run_active(run_id) {
                break;
            }

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
    let start_time = std::time::Instant::now();
    let use_delimiter_crawl = input.provider.as_deref() == Some("rustfs");

    let mut pending_prefixes: VecDeque<String> = VecDeque::from([String::new()]);
    let mut seen_prefixes: HashSet<String> = HashSet::from([String::new()]);

    while let Some(current_prefix) = pending_prefixes.pop_front() {
        let mut continuation_token: Option<String> = None;

        loop {
            if !is_background_run_active(run_id) {
                drop(tx);
                return Ok(BackgroundSyncResult {
                    total_objects: fetched_count,
                    cancelled: true,
                });
            }

            let create_request = || {
                let mut request = client.list_objects_v2().bucket(&bucket).max_keys(1000);

                if use_delimiter_crawl {
                    request = request.delimiter("/");
                    if !current_prefix.is_empty() {
                        request = request.prefix(&current_prefix);
                    }
                }

                if let Some(token) = &continuation_token {
                    request = request.continuation_token(token);
                }

                request
            };

            let response = {
                let _list_guard = S3_LIST_LOCK.lock().await;
                match create_request().send().await {
                    Ok(response) => response,
                    Err(first_error) => create_request().send().await.map_err(|retry_error| {
                        format!(
                            "S3 list failed after retry: {}; first attempt: {}",
                            retry_error, first_error
                        )
                    })?,
                }
            };

            if !is_background_run_active(run_id) {
                drop(tx);
                return Ok(BackgroundSyncResult {
                    total_objects: fetched_count,
                    cancelled: true,
                });
            }

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

            if use_delimiter_crawl {
                for cp in response.common_prefixes() {
                    if let Some(prefix) = cp.prefix() {
                        let prefix = prefix.to_string();
                        folder_keys.push(prefix.clone());
                        if seen_prefixes.insert(prefix.clone()) {
                            pending_prefixes.push_back(prefix);
                        }
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
                    estimated_total: {
                        let has_pending_prefixes =
                            use_delimiter_crawl && !pending_prefixes.is_empty();
                        if is_truncated || has_pending_prefixes {
                            None
                        } else {
                            Some(fetched_count)
                        }
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

        if !use_delimiter_crawl {
            break;
        }
    }

    // Wait for store task
    drop(tx);
    let stored_count = store_handle
        .await
        .map_err(|e| format!("Store task panicked: {}", e))?
        .map_err(|e| format!("Store failed: {}", e))?;

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            total_objects: fetched_count,
            cancelled: true,
        });
    }

    // Finish sync (swap staging -> live)
    db::finish_sync(&bucket, &account_id, stored_count)
        .await
        .map_err(|e| format!("Failed to finish sync: {}", e))?;

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            total_objects: fetched_count,
            cancelled: true,
        });
    }

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
    BACKGROUND_RUN_ID.fetch_add(1, Ordering::SeqCst);
    BACKGROUND_CANCEL.store(true, Ordering::SeqCst);
    Ok(())
}
