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
    if db::cache_scope::current_scope().is_none() {
        db::cache_scope::invalidate_unscoped(account_id)
            .await
            .map_err(|e| e.to_string())?;
        let _ = app.emit(
            "cache-updated",
            CacheUpdatedEvent {
                action: "move".into(),
                affected_paths: get_unique_parent_paths(&[
                    old_key.to_string(),
                    new_key.to_string(),
                ]),
            },
        );
        return Ok(());
    }

    if old_key == new_key {
        return Ok(());
    }

    let mutation_token = db::begin_local_cache_mutation(bucket, account_id)
        .await
        .map_err(|e| format!("Failed to start cache mutation: {}", e))?;
    let mutation_result = async {
        let move_result = if let Some((size, last_modified)) =
            db::move_cached_file(bucket, account_id, old_key, new_key)
                .await
                .map_err(|e| format!("Failed to update file cache: {}", e))?
        {
            Some(
                db::update_directory_tree_for_move(
                    bucket,
                    account_id,
                    old_key,
                    new_key,
                    size,
                    &last_modified,
                )
                .await
                .map_err(|e| format!("Failed to update directory tree: {}", e))?,
            )
        } else {
            None
        };
        Ok::<_, String>(move_result)
    }
    .await;
    let finish_result = db::finish_local_cache_mutation(bucket, account_id, &mutation_token)
        .await
        .map_err(|e| format!("Failed to finish cache mutation: {}", e));
    let move_result = match mutation_result {
        Ok(value) => {
            finish_result?;
            value
        }
        Err(error) => {
            let _ = finish_result;
            return Err(error);
        }
    };

    if let Some(move_result) = move_result {
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
    if db::cache_scope::current_scope().is_none() {
        db::cache_scope::invalidate_unscoped(account_id)
            .await
            .map_err(|e| e.to_string())?;
        let _ = app.emit(
            "cache-updated",
            CacheUpdatedEvent {
                action: "move".into(),
                affected_paths: get_unique_parent_paths(
                    &operations
                        .iter()
                        .flat_map(|(a, b)| [a.clone(), b.clone()])
                        .collect::<Vec<_>>(),
                ),
            },
        );
        return Ok(());
    }

    if operations.is_empty() {
        return Ok(());
    }

    let mutation_token = db::begin_local_cache_mutation(bucket, account_id)
        .await
        .map_err(|e| format!("Failed to start cache mutation: {}", e))?;
    let mutation_result = async {
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
        Ok::<_, String>((affected_keys, removed_paths, created_paths))
    }
    .await;
    let finish_result = db::finish_local_cache_mutation(bucket, account_id, &mutation_token)
        .await
        .map_err(|e| format!("Failed to finish cache mutation: {}", e));
    let (affected_keys, removed_paths, created_paths) = match mutation_result {
        Ok(value) => {
            finish_result?;
            value
        }
        Err(error) => {
            let _ = finish_result;
            return Err(error);
        }
    };

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
