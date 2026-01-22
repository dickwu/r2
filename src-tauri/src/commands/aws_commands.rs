use crate::commands::cache_events::{get_unique_parent_paths, CacheUpdatedEvent};
use crate::commands::delete_cache::{update_cache_after_batch_delete, update_cache_after_delete};
use crate::commands::upload_cache::update_cache_after_upload;
use crate::db::{self, CachedFile};
use crate::providers::aws;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::Semaphore;

#[derive(Debug, Deserialize)]
pub struct AwsConfigInput {
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
    pub endpoint_scheme: Option<String>,
    pub endpoint_host: Option<String>,
    pub force_path_style: bool,
}

impl From<AwsConfigInput> for aws::AwsConfig {
    fn from(input: AwsConfigInput) -> Self {
        aws::AwsConfig {
            bucket: input.bucket,
            access_key_id: input.access_key_id,
            secret_access_key: input.secret_access_key,
            region: input.region,
            endpoint_scheme: input.endpoint_scheme,
            endpoint_host: input.endpoint_host,
            force_path_style: input.force_path_style,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListObjectsInput {
    pub config: AwsConfigInput,
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
struct IndexingProgress {
    current: usize,
    total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderLoadProgress {
    pub pages: usize,
    pub items: usize,
}

#[tauri::command]
pub async fn list_aws_buckets(
    _account_id: String,
    access_key_id: String,
    secret_access_key: String,
    region: String,
    endpoint_scheme: Option<String>,
    endpoint_host: Option<String>,
    force_path_style: bool,
) -> Result<Vec<aws::AwsBucket>, String> {
    let config = aws::AwsConfig {
        bucket: String::new(),
        access_key_id,
        secret_access_key,
        region,
        endpoint_scheme,
        endpoint_host,
        force_path_style,
    };

    aws::list_buckets(&config)
        .await
        .map_err(|e| format!("Failed to list buckets: {}", e))
}

#[tauri::command]
pub async fn list_aws_objects(input: ListObjectsInput) -> Result<aws::ListObjectsResult, String> {
    let config: aws::AwsConfig = input.config.into();

    aws::list_objects(
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
pub async fn list_all_aws_objects(
    config: AwsConfigInput,
    app: tauri::AppHandle,
) -> Result<Vec<aws::AwsObject>, String> {
    let aws_config: aws::AwsConfig = config.into();

    let _ = app.emit("sync-phase", "fetching");

    let app_clone = app.clone();
    let progress_callback = Box::new(move |count: usize| {
        let _ = app_clone.emit("sync-progress", count);
    });

    aws::list_all_objects_recursive(&aws_config, Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to list all objects: {}", e))
}

#[tauri::command]
pub async fn sync_aws_bucket(
    config: AwsConfigInput,
    app: tauri::AppHandle,
) -> Result<SyncResult, String> {
    let account_id = config.account_id.clone();
    let aws_config: aws::AwsConfig = config.into();
    let bucket = aws_config.bucket.clone();

    let _ = app.emit("sync-phase", "fetching");

    let app_clone = app.clone();
    let progress_callback = Box::new(move |count: usize| {
        let _ = app_clone.emit("sync-progress", count);
    });

    let objects = aws::list_all_objects_recursive(&aws_config, Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to fetch objects: {}", e))?;

    let count = objects.len() as i32;

    let _ = app.emit("sync-phase", "storing");

    let now = chrono::Utc::now().timestamp();
    let cached_files: Vec<CachedFile> = objects
        .into_iter()
        .map(|obj| CachedFile {
            bucket: bucket.clone(),
            account_id: account_id.clone(),
            key: obj.key,
            parent_path: String::new(),
            name: String::new(),
            size: obj.size,
            last_modified: obj.last_modified,
            synced_at: now,
        })
        .collect();

    db::store_all_files(&bucket, &account_id, &cached_files)
        .await
        .map_err(|e| format!("Failed to store files: {}", e))?;

    let _ = app.emit("sync-phase", "indexing");

    let app_clone = app.clone();
    let indexing_callback = move |current: usize, total: usize| {
        let _ = app_clone.emit("indexing-progress", IndexingProgress { current, total });
    };

    db::build_directory_tree(&bucket, &account_id, &cached_files, Some(indexing_callback))
        .await
        .map_err(|e| format!("Failed to build directory tree: {}", e))?;

    let _ = app.emit("sync-phase", "complete");

    Ok(SyncResult {
        count,
        timestamp: now,
    })
}

#[tauri::command]
pub async fn list_folder_aws_objects(
    config: AwsConfigInput,
    prefix: Option<String>,
    app: tauri::AppHandle,
) -> Result<aws::ListObjectsResult, String> {
    let aws_config: aws::AwsConfig = config.into();

    let _ = app.emit("folder-load-phase", "loading");

    let app_clone = app.clone();
    let progress_callback = Box::new(move |pages: usize, items: usize| {
        let _ = app_clone.emit("folder-load-progress", FolderLoadProgress { pages, items });
    });

    let result = aws::list_folder_objects(&aws_config, prefix.as_deref(), Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to list folder objects: {}", e))?;

    let _ = app.emit("folder-load-phase", "complete");

    Ok(result)
}

#[tauri::command]
pub async fn delete_aws_object(
    config: AwsConfigInput,
    key: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    let aws_config: aws::AwsConfig = config.into();

    aws::delete_object(&aws_config, &key)
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
pub async fn batch_delete_aws_objects(
    config: AwsConfigInput,
    keys: Vec<String>,
    app: tauri::AppHandle,
) -> Result<BatchDeleteResult, String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    let aws_config: aws::AwsConfig = config.into();
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

        match aws::delete_objects(&aws_config, batch_keys.clone()).await {
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
pub async fn rename_aws_object(
    config: AwsConfigInput,
    old_key: String,
    new_key: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    let aws_config: aws::AwsConfig = config.into();

    aws::rename_object(&aws_config, &old_key, &new_key)
        .await
        .map_err(|e| format!("Failed to rename object: {}", e))?;

    if let Some((size, last_modified)) =
        db::move_cached_file(&bucket, &account_id, &old_key, &new_key)
            .await
            .map_err(|e| format!("Failed to update file cache: {}", e))?
    {
        db::update_directory_tree_for_move(
            &bucket,
            &account_id,
            &old_key,
            &new_key,
            size,
            &last_modified,
        )
        .await
        .map_err(|e| format!("Failed to update directory tree: {}", e))?;
    }

    let _ = app.emit(
        "cache-updated",
        CacheUpdatedEvent {
            action: "move".to_string(),
            affected_paths: get_unique_parent_paths(&[old_key, new_key]),
        },
    );

    Ok(())
}

#[tauri::command]
pub async fn batch_move_aws_objects(
    config: AwsConfigInput,
    operations: Vec<MoveOperation>,
    app: tauri::AppHandle,
) -> Result<BatchMoveResult, String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    let aws_config: aws::AwsConfig = config.into();
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
    let successful_moves = Arc::new(tokio::sync::Mutex::new(Vec::<MoveOperation>::new()));

    let mut handles = Vec::new();

    for op in operations {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        let config = aws_config.clone();
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
                match aws::rename_object(&config, &op.old_key, &op.new_key).await {
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

    let moves = successful_moves.lock().await;
    let mut affected_keys: Vec<String> = Vec::new();
    for op in moves.iter() {
        if let Ok(Some((size, last_modified))) =
            db::move_cached_file(&bucket, &account_id, &op.old_key, &op.new_key).await
        {
            let _ = db::update_directory_tree_for_move(
                &bucket,
                &account_id,
                &op.old_key,
                &op.new_key,
                size,
                &last_modified,
            )
            .await;
            affected_keys.push(op.old_key.clone());
            affected_keys.push(op.new_key.clone());
        }
    }

    if !affected_keys.is_empty() {
        let _ = app.emit(
            "cache-updated",
            CacheUpdatedEvent {
                action: "move".to_string(),
                affected_paths: get_unique_parent_paths(&affected_keys),
            },
        );
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

#[tauri::command]
pub async fn generate_aws_signed_url(
    config: AwsConfigInput,
    key: String,
    expires_in: Option<u64>,
) -> Result<String, String> {
    let aws_config: aws::AwsConfig = config.into();
    let expires_in_secs = expires_in.unwrap_or(3600);

    aws::generate_presigned_url(&aws_config, &key, expires_in_secs)
        .await
        .map_err(|e| format!("Failed to generate signed URL: {}", e))
}

#[tauri::command]
pub async fn upload_aws_content(
    config: AwsConfigInput,
    key: String,
    content: String,
    content_type: Option<String>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let bucket = config.bucket.clone();
    let account_id = config.account_id.clone();
    let aws_config: aws::AwsConfig = config.into();

    let content_bytes = content.into_bytes();
    let new_size = content_bytes.len() as i64;

    let etag = aws::upload_content(&aws_config, &key, content_bytes, content_type.as_deref())
        .await
        .map_err(|e| format!("Failed to upload content: {}", e))?;

    let last_modified = chrono::Utc::now().to_rfc3339();

    let (size_delta, is_new_file) =
        db::update_cached_file(&bucket, &account_id, &key, new_size, &last_modified)
            .await
            .map_err(|e| format!("Failed to update file cache: {}", e))?;

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

    let _ = app.emit(
        "cache-updated",
        CacheUpdatedEvent {
            action: "update".to_string(),
            affected_paths: get_unique_parent_paths(&[key]),
        },
    );

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
pub async fn upload_aws_file(
    app: tauri::AppHandle,
    task_id: String,
    file_path: String,
    key: String,
    content_type: Option<String>,
    account_id: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
    region: String,
    endpoint_scheme: Option<String>,
    endpoint_host: Option<String>,
    force_path_style: bool,
) -> Result<UploadResult, String> {
    let config = aws::AwsConfig {
        bucket,
        access_key_id,
        secret_access_key,
        region,
        endpoint_scheme,
        endpoint_host,
        force_path_style,
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

    let result = aws::upload_file(
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
            if let Err(err) = update_cache_after_upload(
                &app,
                &config.bucket,
                &account_id,
                &key,
                file_size as i64,
                &last_modified,
            )
            .await
            {
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
