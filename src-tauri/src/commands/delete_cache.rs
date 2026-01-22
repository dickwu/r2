use crate::commands::cache_events::{
    get_unique_parent_paths, CacheUpdatedEvent, PathsRemovedEvent,
};
use crate::db;
use tauri::{AppHandle, Emitter};

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

    // Collect all removed paths
    let mut all_removed_paths: Vec<String> = Vec::new();

    // Update directory tree for each deleted file
    for key in deleted_keys {
        if let Some(&size) = file_sizes.get(key) {
            match db::update_directory_tree_for_delete(bucket, account_id, key, size).await {
                Ok(removed_paths) => {
                    // Add paths that haven't been added yet
                    for path in removed_paths {
                        if !all_removed_paths.contains(&path) {
                            all_removed_paths.push(path);
                        }
                    }
                }
                Err(e) => {
                    // Log but don't fail the entire operation
                    eprintln!("Failed to update directory tree for {}: {}", key, e);
                }
            }
        }
    }

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
