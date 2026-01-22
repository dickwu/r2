use crate::commands::cache_events::{
    get_unique_parent_paths, CacheUpdatedEvent, PathsCreatedEvent, PathsRemovedEvent,
};
use crate::db;
use std::collections::HashSet;
use tauri::{AppHandle, Emitter};

/// Update cache after a single file move/rename.
/// Handles file cache, directory tree updates, and emits appropriate events.
pub(crate) async fn update_cache_after_move(
    app: &AppHandle,
    bucket: &str,
    account_id: &str,
    old_key: &str,
    new_key: &str,
) -> Result<(), String> {
    if old_key == new_key {
        return Ok(());
    }

    if let Some((size, last_modified)) = db::move_cached_file(bucket, account_id, old_key, new_key)
        .await
        .map_err(|e| format!("Failed to update file cache: {}", e))?
    {
        let move_result = db::update_directory_tree_for_move(
            bucket,
            account_id,
            old_key,
            new_key,
            size,
            &last_modified,
        )
        .await
        .map_err(|e| format!("Failed to update directory tree: {}", e))?;

        if !move_result.removed_paths.is_empty() {
            let _ = app.emit(
                "paths-removed",
                PathsRemovedEvent {
                    removed_paths: move_result.removed_paths,
                },
            );
        }

        if !move_result.created_paths.is_empty() {
            let _ = app.emit(
                "paths-created",
                PathsCreatedEvent {
                    created_paths: move_result.created_paths,
                },
            );
        }
    }

    // Always emit cache-updated so the UI refreshes affected paths
    let _ = app.emit(
        "cache-updated",
        CacheUpdatedEvent {
            action: "move".to_string(),
            affected_paths: get_unique_parent_paths(&[old_key.to_string(), new_key.to_string()]),
        },
    );

    Ok(())
}

/// Update cache after batch move/rename operations.
/// Handles file cache, directory tree updates, and emits appropriate events.
pub(crate) async fn update_cache_after_batch_move(
    app: &AppHandle,
    bucket: &str,
    account_id: &str,
    operations: &[(String, String)],
) -> Result<(), String> {
    if operations.is_empty() {
        return Ok(());
    }

    let mut affected_keys: Vec<String> = Vec::new();
    let mut removed_paths: HashSet<String> = HashSet::new();
    let mut created_paths: HashSet<String> = HashSet::new();

    for (old_key, new_key) in operations {
        if old_key == new_key {
            continue;
        }

        if let Some((size, last_modified)) =
            db::move_cached_file(bucket, account_id, old_key, new_key)
                .await
                .map_err(|e| format!("Failed to update file cache: {}", e))?
        {
            let move_result = db::update_directory_tree_for_move(
                bucket,
                account_id,
                old_key,
                new_key,
                size,
                &last_modified,
            )
            .await
            .map_err(|e| format!("Failed to update directory tree: {}", e))?;

            for path in move_result.removed_paths {
                removed_paths.insert(path);
            }
            for path in move_result.created_paths {
                created_paths.insert(path);
            }

            affected_keys.push(old_key.clone());
            affected_keys.push(new_key.clone());
        }
    }

    if !removed_paths.is_empty() {
        let mut removed: Vec<String> = removed_paths.into_iter().collect();
        removed.sort();
        let _ = app.emit(
            "paths-removed",
            PathsRemovedEvent {
                removed_paths: removed,
            },
        );
    }

    if !created_paths.is_empty() {
        let mut created: Vec<String> = created_paths.into_iter().collect();
        created.sort();
        let _ = app.emit(
            "paths-created",
            PathsCreatedEvent {
                created_paths: created,
            },
        );
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

    Ok(())
}
