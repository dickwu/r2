use crate::commands::batch_delete::run_batch_delete;
use crate::commands::batch_move::{
    fallback_batch_id, run_batch_move, BatchMoveResult, MoveOperation,
};
use crate::commands::delete_cache::{update_cache_after_batch_delete, update_cache_after_delete};
use crate::commands::move_cache::{update_cache_after_batch_move, update_cache_after_move};
use crate::commands::upload_cache::{update_cache_after_upload, CacheEventSink};
use crate::db::{self, CachedFile};
use crate::providers::minio;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::Emitter;

#[derive(Debug, Clone, Deserialize)]
pub struct MinioConfigInput {
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint_scheme: String,
    pub endpoint_host: String,
    pub force_path_style: bool,
}

impl MinioConfigInput {
    /// The legacy RustFS adapter uses the MinIO commands, so the namespace is
    /// a MinIO account's or, failing that, a RustFS account's.
    fn cache_configs(&self) -> [db::cache_scope::CacheConfig; 2] {
        let minio = db::cache_scope::CacheConfig {
            provider: "minio".into(),
            account_id: self.account_id.clone(),
            access_key_id: self.access_key_id.clone(),
            secret_access_key: self.secret_access_key.clone(),
            region: None,
            endpoint_scheme: Some(self.endpoint_scheme.clone()),
            endpoint_host: Some(self.endpoint_host.clone()),
            force_path_style: self.force_path_style,
        };
        let rustfs = db::cache_scope::CacheConfig {
            provider: "rustfs".into(),
            ..minio.clone()
        };
        [minio, rustfs]
    }
}

impl From<MinioConfigInput> for minio::MinioConfig {
    fn from(input: MinioConfigInput) -> Self {
        minio::MinioConfig {
            bucket: input.bucket,
            access_key_id: input.access_key_id,
            secret_access_key: input.secret_access_key,
            endpoint_scheme: input.endpoint_scheme,
            endpoint_host: input.endpoint_host,
            force_path_style: input.force_path_style,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListObjectsInput {
    pub config: MinioConfigInput,
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    pub continuation_token: Option<String>,
    pub max_keys: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncResult {
    pub count: i32,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderLoadProgress {
    pub pages: usize,
    pub items: usize,
}

#[tauri::command]
pub async fn list_minio_buckets(
    _account_id: String,
    access_key_id: String,
    secret_access_key: String,
    endpoint_scheme: String,
    endpoint_host: String,
    force_path_style: bool,
) -> Result<Vec<minio::MinioBucket>, String> {
    let config = minio::MinioConfig {
        bucket: String::new(),
        access_key_id,
        secret_access_key,
        endpoint_scheme,
        endpoint_host,
        force_path_style,
    };

    minio::list_buckets(&config)
        .await
        .map_err(|e| format!("Failed to list buckets: {}", e))
}

#[tauri::command]
pub async fn list_minio_objects(
    input: ListObjectsInput,
) -> Result<minio::ListObjectsResult, String> {
    let config: minio::MinioConfig = input.config.into();

    minio::list_objects(
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
pub async fn list_all_minio_objects(
    config: MinioConfigInput,
    app: tauri::AppHandle,
) -> Result<Vec<minio::MinioObject>, String> {
    let minio_config: minio::MinioConfig = config.into();

    let _ = app.emit("sync-phase", "fetching");

    let app_clone = app.clone();
    let progress_callback = Box::new(move |count: usize| {
        let _ = app_clone.emit("sync-progress", count);
    });

    let result = minio::list_all_objects_recursive(&minio_config, Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to list all objects: {}", e))?;

    Ok(result.objects)
}

#[tauri::command]
pub async fn sync_minio_bucket(
    config: MinioConfigInput,
    app: tauri::AppHandle,
) -> Result<SyncResult, String> {
    let [minio_config, rustfs_config] = config.cache_configs();
    let scope = match db::cache_scope::CacheScope::capture(&minio_config).await {
        Ok(scope) => scope,
        // The legacy RustFS adapter intentionally uses the MinIO protocol command.
        Err(_) => db::cache_scope::CacheScope::capture(&rustfs_config)
            .await
            .map_err(|e| e.to_string())?,
    };
    db::cache_scope::in_scope(scope, sync_minio_bucket_scoped(config, app)).await
}

async fn sync_minio_bucket_scoped(
    config: MinioConfigInput,
    app: tauri::AppHandle,
) -> Result<SyncResult, String> {
    let account_id = config.account_id.clone();
    let minio_config: minio::MinioConfig = config.into();
    let bucket = minio_config.bucket.clone();
    let now = chrono::Utc::now().timestamp();

    let _ = app.emit("sync-phase", "fetching");

    let sync_run = db::begin_sync(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to clear cache: {}", e))?;

    // Spawn dedicated store task
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<CachedFile>>(8);
    let store_bucket = bucket.clone();
    let store_account_id = account_id.clone();
    let store_app = app.clone();
    let store_sync_run = sync_run.clone();
    let store_scope = db::cache_scope::current_scope().ok_or("Missing sync cache scope")?;
    let store_handle = tokio::spawn(db::cache_scope::in_scope(store_scope, async move {
        let mut stored_count: usize = 0;
        while let Some(batch) = rx.recv().await {
            let batch_len = batch.len();
            db::store_file_batch(&store_bucket, &store_account_id, &store_sync_run, &batch)
                .await
                .map_err(|e| format!("Failed to store files: {}", e))?;
            stored_count += batch_len;
            let _ = store_app.emit("store-progress", stored_count);
        }
        Ok::<usize, String>(stored_count)
    }));

    let client = minio::create_minio_client(&minio_config)
        .await
        .map_err(|e| format!("Failed to create client: {}", e))?;

    let mut fetched_count: usize = 0;
    let mut folder_keys: Vec<String> = Vec::new();
    let mut continuation_token: Option<String> = None;

    loop {
        let mut request = client
            .list_objects_v2()
            .bucket(&minio_config.bucket)
            .max_keys(1000);

        if let Some(token) = &continuation_token {
            request = request.continuation_token(token);
        }

        let response = request.send().await.map_err(|e| {
            format!(
                "Failed to list objects: {}",
                crate::providers::s3_client::describe_s3_error(&e)
            )
        })?;
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

    drop(tx);
    let _ = app.emit("sync-phase", "storing");
    let stored_count = store_handle
        .await
        .map_err(|e| format!("Store task panicked: {}", e))?
        .map_err(|e| format!("Store failed: {}", e))?;

    let _ = app.emit("sync-phase", "indexing");

    db::finish_sync_with_metadata(
        &bucket,
        &account_id,
        &sync_run,
        stored_count,
        &folder_keys,
        &[],
    )
    .await
    .map_err(|e| format!("Failed to publish sync cache: {}", e))?;

    let _ = app.emit("sync-phase", "complete");

    Ok(SyncResult {
        count: stored_count as i32,
        timestamp: now,
    })
}

#[tauri::command]
pub async fn list_folder_minio_objects(
    config: MinioConfigInput,
    prefix: Option<String>,
    app: tauri::AppHandle,
) -> Result<minio::ListObjectsResult, String> {
    let minio_config: minio::MinioConfig = config.into();

    let _ = app.emit("folder-load-phase", "loading");

    let app_clone = app.clone();
    let progress_callback = Box::new(move |pages: usize, items: usize| {
        let _ = app_clone.emit("folder-load-progress", FolderLoadProgress { pages, items });
    });

    let result =
        minio::list_folder_objects(&minio_config, prefix.as_deref(), Some(progress_callback))
            .await
            .map_err(|e| format!("Failed to list folder objects: {}", e))?;

    let _ = app.emit("folder-load-phase", "complete");

    Ok(result)
}

#[tauri::command]
pub async fn delete_minio_object(
    config: MinioConfigInput,
    key: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    delete_minio_object_with(config, key, &app).await
}

pub(crate) async fn delete_minio_object_with(
    config: MinioConfigInput,
    key: String,
    app: &impl CacheEventSink,
) -> Result<(), String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    // Captured before the write, as a sync captures its scope before listing.
    let scope = db::cache_scope::capture_write_scope(config.cache_configs()).await;
    let minio_config: minio::MinioConfig = config.into();

    minio::delete_object(&minio_config, &key)
        .await
        .map_err(|e| format!("Failed to delete object: {}", e))?;

    // Update cache and emit events (including paths-removed if any folders became empty)
    let update = update_cache_after_delete(app, &bucket, &account_id, &key);
    db::cache_scope::in_optional_scope(scope, update).await?;

    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchDeleteResult {
    pub deleted: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

#[tauri::command]
pub async fn batch_delete_minio_objects(
    config: MinioConfigInput,
    keys: Vec<String>,
    app: tauri::AppHandle,
) -> Result<BatchDeleteResult, String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    // Captured before the write, as a sync captures its scope before listing.
    let scope = db::cache_scope::capture_write_scope(config.cache_configs()).await;
    let minio_config: minio::MinioConfig = config.into();
    let total = keys.len();

    if total == 0 {
        return Ok(BatchDeleteResult {
            deleted: 0,
            failed: 0,
            errors: vec![],
        });
    }

    let mut outcome = run_batch_delete(&app, keys, |batch| {
        let cfg = minio_config.clone();
        async move {
            minio::delete_objects(&cfg, batch)
                .await
                .map(|_| ())
                .map_err(|e| format!("Batch delete failed: {}", e))
        }
    })
    .await;

    // Update cache and emit events (including paths-removed if any folders became empty)
    if !outcome.deleted_keys.is_empty() {
        let update =
            update_cache_after_batch_delete(&app, &bucket, &account_id, &outcome.deleted_keys);
        if let Err(e) = db::cache_scope::in_optional_scope(scope, update).await {
            outcome.errors.push(e);
        }
    }

    Ok(BatchDeleteResult {
        deleted: outcome.completed,
        failed: outcome.failed,
        errors: outcome.errors,
    })
}

#[tauri::command]
pub async fn rename_minio_object(
    config: MinioConfigInput,
    old_key: String,
    new_key: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    // Captured before the write, as a sync captures its scope before listing.
    let scope = db::cache_scope::capture_write_scope(config.cache_configs()).await;
    let minio_config: minio::MinioConfig = config.into();

    minio::rename_object(&minio_config, &old_key, &new_key)
        .await
        .map_err(|e| format!("Failed to rename object: {}", e))?;

    // Update cache and emit events (including paths-created/removed)
    let update = update_cache_after_move(&app, &bucket, &account_id, &old_key, &new_key);
    db::cache_scope::in_optional_scope(scope, update).await?;

    Ok(())
}

#[tauri::command]
pub async fn batch_move_minio_objects(
    config: MinioConfigInput,
    operations: Vec<MoveOperation>,
    batch_id: Option<String>,
    app: tauri::AppHandle,
) -> Result<BatchMoveResult, String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    // Captured before the write, as a sync captures its scope before listing.
    let scope = db::cache_scope::capture_write_scope(config.cache_configs()).await;
    let minio_config: minio::MinioConfig = config.into();
    let batch_id = batch_id.unwrap_or_else(fallback_batch_id);

    let rename = move |op: MoveOperation| {
        let config = minio_config.clone();
        async move {
            minio::rename_object(&config, &op.old_key, &op.new_key)
                .await
                .map_err(|e| e.to_string())
        }
    };

    let outcome = run_batch_move(&app, batch_id, operations, rename).await;

    let mut errors = outcome.errors;
    if !outcome.successful.is_empty() {
        let update = update_cache_after_batch_move(&app, &bucket, &account_id, &outcome.successful);
        if let Err(e) = db::cache_scope::in_optional_scope(scope, update).await {
            errors.push(e);
        }
    }

    let final_completed = outcome.moved;
    let final_failed = outcome.failed;
    let final_errors = errors;

    Ok(BatchMoveResult {
        moved: final_completed,
        failed: final_failed,
        errors: final_errors,
    })
}

#[tauri::command]
pub async fn generate_minio_signed_url(
    config: MinioConfigInput,
    key: String,
    expires_in: Option<u64>,
) -> Result<String, String> {
    let minio_config: minio::MinioConfig = config.into();
    let expires_in_secs = expires_in.unwrap_or(3600);

    minio::generate_presigned_url(&minio_config, &key, expires_in_secs)
        .await
        .map_err(|e| format!("Failed to generate signed URL: {}", e))
}

#[tauri::command]
pub async fn upload_minio_content(
    config: MinioConfigInput,
    key: String,
    content: String,
    content_type: Option<String>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    upload_minio_content_with(config, key, content, content_type, &app).await
}

pub(crate) async fn upload_minio_content_with(
    config: MinioConfigInput,
    key: String,
    content: String,
    content_type: Option<String>,
    app: &impl CacheEventSink,
) -> Result<String, String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    // Captured before the write, as a sync captures its scope before listing.
    let scope = db::cache_scope::capture_write_scope(config.cache_configs()).await;
    let minio_config: minio::MinioConfig = config.into();

    let content_bytes = content.into_bytes();
    let new_size = content_bytes.len() as i64;

    let etag = minio::upload_content(&minio_config, &key, content_bytes, content_type.as_deref())
        .await
        .map_err(|e| format!("Failed to upload content: {}", e))?;

    let last_modified = chrono::Utc::now().to_rfc3339();

    let update =
        update_cache_after_upload(app, &bucket, &account_id, &key, new_size, &last_modified);
    db::cache_scope::in_optional_scope(scope, update).await?;

    Ok(etag)
}

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

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn upload_minio_file(
    app: tauri::AppHandle,
    task_id: String,
    file_path: String,
    key: String,
    content_type: Option<String>,
    account_id: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
    endpoint_scheme: String,
    endpoint_host: String,
    force_path_style: bool,
) -> Result<UploadResult, String> {
    let input = MinioConfigInput {
        account_id,
        bucket,
        access_key_id,
        secret_access_key,
        endpoint_scheme,
        endpoint_host,
        force_path_style,
    };
    // Captured before the write, as a sync captures its scope before listing.
    let scope = db::cache_scope::capture_write_scope(input.cache_configs()).await;
    let account_id = input.account_id.clone();
    let config: minio::MinioConfig = input.into();

    let path = PathBuf::from(&file_path);
    if !path.exists() {
        return Ok(UploadResult {
            task_id,
            success: false,
            error: Some(format!("File not found: {}", file_path)),
            upload_id: None,
        });
    }

    let file_size = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("Failed to get file metadata: {}", e))?
        .len();

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

    let result = minio::upload_file(
        &config,
        &key,
        &path,
        content_type.as_deref(),
        Some(progress_callback),
    )
    .await;

    match result {
        Ok(upload_id_or_etag) => {
            let _ = app.emit(
                "upload-progress",
                UploadProgress {
                    task_id: task_id.clone(),
                    percent: 100,
                    uploaded_bytes: file_size,
                    total_bytes: file_size,
                },
            );

            let last_modified = chrono::Utc::now().to_rfc3339();
            let update = update_cache_after_upload(
                &app,
                &config.bucket,
                &account_id,
                &key,
                file_size as i64,
                &last_modified,
            );
            if let Err(err) = db::cache_scope::in_optional_scope(scope, update).await {
                log::warn!("Failed to update cache after upload: {}", err);
            }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::upload_cache::RecordedCacheEvents;
    use crate::db::cache_scope::{self, CacheConfig, CacheScope};
    use crate::test_s3::{serve, Fixture, Response};

    /// A MinIO account whose endpoint is an in-process S3 fixture that accepts
    /// every PUT and DELETE, registered in the app database like a saved one.
    async fn saved_account(account_id: &str) -> (Fixture, MinioConfigInput, CacheConfig) {
        crate::db::init_test_db().await;
        let fixture = serve(|request| async move {
            match request.method.as_str() {
                "PUT" => Response::empty(200).header("etag", "\"fixture\""),
                "DELETE" => Response::empty(204),
                _ => Response::empty(404),
            }
        })
        .await;
        let host = fixture
            .endpoint
            .strip_prefix("http://")
            .unwrap()
            .to_string();
        crate::db::get_connection()
            .unwrap()
            .lock()
            .await
            .execute(
                "INSERT INTO minio_accounts (id, access_key_id, secret_access_key, endpoint_scheme, endpoint_host, force_path_style, created_at, updated_at)
                 VALUES (?1, 'fixture', 'fixture-secret', 'http', ?2, 1, 0, 0)",
                turso::params![account_id, host.clone()],
            )
            .await
            .unwrap();
        let input = MinioConfigInput {
            account_id: account_id.into(),
            bucket: "journal".into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            endpoint_scheme: "http".into(),
            endpoint_host: host.clone(),
            force_path_style: true,
        };
        let cache_config = CacheConfig {
            provider: "minio".into(),
            account_id: account_id.into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            region: None,
            endpoint_scheme: Some("http".into()),
            endpoint_host: Some(host),
            force_path_style: true,
        };
        (fixture, input, cache_config)
    }

    fn scanned(account_id: &str, key: &str, size: i64) -> CachedFile {
        let (parent_path, name) = db::parse_key(key);
        CachedFile {
            bucket: "journal".into(),
            account_id: account_id.into(),
            key: key.into(),
            parent_path,
            name,
            size,
            last_modified: "scan".into(),
            synced_at: 1,
        }
    }

    /// What a sync command does up to its publish: capture its scope, begin
    /// the run, and stage what its scan listed.
    async fn start_sync(
        cache_config: &CacheConfig,
        account_id: &str,
        listed: &[CachedFile],
    ) -> (CacheScope, String) {
        let scope = CacheScope::capture(cache_config).await.unwrap();
        let run = cache_scope::in_scope(scope.clone(), async {
            let run = db::begin_sync("journal", account_id).await.unwrap();
            db::store_file_batch("journal", account_id, &run, listed)
                .await
                .unwrap();
            run
        })
        .await;
        (scope, run)
    }

    async fn publish(scope: &CacheScope, account_id: &str, run: &str) -> Result<(), String> {
        cache_scope::in_scope(
            scope.clone(),
            db::finish_sync_with_metadata("journal", account_id, run, 2, &[], &[]),
        )
        .await
        .map_err(|error| error.to_string())
    }

    async fn folder(scope: &CacheScope, prefix: &str) -> cache_scope::PrefixPageSnapshot {
        cache_scope::read_prefix_page(scope, "journal", prefix, None, 100)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn deletes_and_uploads_through_the_commands_reach_a_running_sync() {
        const ACCOUNT: &str = "command-journal-account";
        let (fixture, input, cache_config) = saved_account(ACCOUNT).await;
        // The scan listed both files before the user deleted one of them.
        let (scope, run) = start_sync(
            &cache_config,
            ACCOUNT,
            &[
                scanned(ACCOUNT, "gone.txt", 2),
                scanned(ACCOUNT, "keep.txt", 1),
            ],
        )
        .await;
        let events = RecordedCacheEvents::default();

        delete_minio_object_with(input.clone(), "gone.txt".into(), &events)
            .await
            .unwrap();
        upload_minio_content_with(input, "new.txt".into(), "hello".into(), None, &events)
            .await
            .unwrap();
        publish(&scope, ACCOUNT, &run).await.unwrap();

        let requests: Vec<_> = fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                let path = request.path.split('?').next().unwrap_or_default();
                format!("{} {path}", request.method)
            })
            .collect();
        assert_eq!(
            requests,
            vec!["DELETE /journal/gone.txt", "PUT /journal/new.txt"]
        );
        let root = folder(&scope, "").await;
        assert!(root.full_sync);
        assert_eq!(
            root.page
                .files
                .iter()
                .map(|file| (file.key.as_str(), file.size))
                .collect::<Vec<_>>(),
            vec![("keep.txt", 1), ("new.txt", 5)]
        );
        let emitted: Vec<_> = events
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(event, _)| event.clone())
            .collect();
        assert_eq!(emitted, vec!["cache-updated", "cache-updated"]);
    }

    #[tokio::test]
    async fn an_unattributed_write_keeps_the_sync_running_but_unvouched() {
        const ACCOUNT: &str = "move-journal-account";
        let (_fixture, _input, cache_config) = saved_account(ACCOUNT).await;
        let (scope, run) =
            start_sync(&cache_config, ACCOUNT, &[scanned(ACCOUNT, "keep.txt", 1)]).await;
        let events = RecordedCacheEvents::default();

        // The Move pipeline reports its destination write with no cache scope.
        update_cache_after_upload(&events, "journal", ACCOUNT, "moved/in.txt", 9, "now")
            .await
            .unwrap();
        publish(&scope, ACCOUNT, &run).await.unwrap();

        let root = folder(&scope, "").await;
        assert!(root.full_sync);
        assert_eq!(
            root.page
                .files
                .iter()
                .map(|file| file.key.as_str())
                .collect::<Vec<_>>(),
            vec!["keep.txt"]
        );
        // Neither the destination nor its parent, whose child folders the
        // write changed, may be served as fresh: both re-list on open.
        assert_eq!(root.freshness_time, None);
        assert_eq!(folder(&scope, "moved/").await.freshness_time, None);
    }
}
