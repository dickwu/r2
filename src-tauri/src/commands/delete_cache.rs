use crate::commands::cache_events::{
    get_unique_parent_paths, CacheUpdatedEvent, PathsRemovedEvent,
};
use crate::db;
use log::{error, info};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio::time::sleep;

struct DeleteCacheQueueState {
    pending: HashMap<(String, String), HashSet<String>>,
    scheduled: bool,
}

static DELETE_CACHE_QUEUE: OnceLock<Mutex<DeleteCacheQueueState>> = OnceLock::new();

fn delete_cache_queue() -> &'static Mutex<DeleteCacheQueueState> {
    DELETE_CACHE_QUEUE.get_or_init(|| {
        Mutex::new(DeleteCacheQueueState {
            pending: HashMap::new(),
            scheduled: false,
        })
    })
}

/// Update cache after a single file deletion.
/// Handles file cache, directory tree updates, and emits appropriate events.
pub(crate) async fn update_cache_after_delete(
    app: &AppHandle,
    bucket: &str,
    account_id: &str,
    key: &str,
) -> Result<(), String> {
    // Delete the file from cache and get its size
    let file_size = db::delete_cached_file(bucket, account_id, key)
        .await
        .map_err(|e| format!("Failed to update file cache: {}", e))?;

    if let Some(file_size) = file_size {
        // Update directory tree and get removed paths
        let removed_paths =
            db::update_directory_tree_for_delete(bucket, account_id, key, file_size)
                .await
                .map_err(|e| format!("Failed to update directory tree: {}", e))?;

        // Emit paths-removed event if any paths were removed
        if !removed_paths.is_empty() {
            let _ = app.emit("paths-removed", PathsRemovedEvent { removed_paths });
        }
    }

    // Emit cache-updated event
    let _ = app.emit(
        "cache-updated",
        CacheUpdatedEvent {
            action: "delete".to_string(),
            affected_paths: get_unique_parent_paths(&[key.to_string()]),
        },
    );

    Ok(())
}

/// Update cache after batch file deletion.
/// Handles file cache, directory tree updates, and emits appropriate events.
pub(crate) async fn update_cache_after_batch_delete(
    app: &AppHandle,
    bucket: &str,
    account_id: &str,
    deleted_keys: &[String],
) -> Result<(), String> {
    if deleted_keys.is_empty() {
        return Ok(());
    }

    // Delete files from cache and get their sizes
    let file_sizes = db::delete_cached_files_batch(bucket, account_id, deleted_keys)
        .await
        .map_err(|e| format!("Failed to update file cache: {}", e))?;

    let deleted_entries: Vec<(String, i64)> = deleted_keys
        .iter()
        .filter_map(|key| file_sizes.get(key).map(|size| (key.clone(), *size)))
        .collect();

    let all_removed_paths = match db::update_directory_tree_for_delete_batch(
        bucket,
        account_id,
        &deleted_entries,
    )
    .await
    {
        Ok(paths) => paths,
        Err(e) => {
            error!("delete_cache_batch: dir_tree update failed: {}", e);
            Vec::new()
        }
    };

    // Emit paths-removed event if any paths were removed
    if !all_removed_paths.is_empty() {
        let _ = app.emit(
            "paths-removed",
            PathsRemovedEvent {
                removed_paths: all_removed_paths,
            },
        );
    }

    // Emit cache-updated event
    let _ = app.emit(
        "cache-updated",
        CacheUpdatedEvent {
            action: "delete".to_string(),
            affected_paths: get_unique_parent_paths(deleted_keys),
        },
    );

    Ok(())
}

/// Queue cache updates for deletes to avoid duplicated directory calculations.
pub(crate) async fn queue_cache_after_delete(
    app: AppHandle,
    bucket: String,
    account_id: String,
    key: String,
) {
    let should_schedule = {
        let queue = delete_cache_queue();
        let mut state = queue.lock().await;
        state
            .pending
            .entry((bucket.clone(), account_id.clone()))
            .or_insert_with(HashSet::new)
            .insert(key);
        if state.scheduled {
            false
        } else {
            state.scheduled = true;
            true
        }
    };

    if !should_schedule {
        return;
    }

    tokio::spawn(async move {
        sleep(Duration::from_millis(300)).await;
        let batch = {
            let queue = delete_cache_queue();
            let mut state = queue.lock().await;
            state.scheduled = false;
            std::mem::take(&mut state.pending)
        };

        for ((bucket, account_id), keys) in batch {
            let key_list: Vec<String> = keys.into_iter().collect();
            info!(
                "delete_cache_batch: flushing {} keys for {}/{}",
                key_list.len(),
                account_id,
                bucket
            );
            if let Err(e) =
                update_cache_after_batch_delete(&app, &bucket, &account_id, &key_list).await
            {
                error!(
                    "delete_cache_batch: failed for {}/{}: {}",
                    account_id, bucket, e
                );
            }
        }
    });
}
