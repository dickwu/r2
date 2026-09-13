use crate::commands::cache_events::{get_unique_parent_paths, CacheUpdatedEvent};
use crate::db;
use tauri::{AppHandle, Emitter};

pub(crate) async fn update_cache_after_upload(
    app: &AppHandle,
    bucket: &str,
    account_id: &str,
    key: &str,
    new_size: i64,
    last_modified: &str,
) -> Result<(), String> {
    if db::cache_scope::current_scope().is_none() {
        db::cache_scope::invalidate_unscoped(account_id)
            .await
            .map_err(|e| e.to_string())?;
        let _ = app.emit(
            "cache-updated",
            CacheUpdatedEvent {
                action: "update".into(),
                affected_paths: get_unique_parent_paths(&[key.to_string()]),
            },
        );
        return Ok(());
    }

    // Insert or update the file in cache, returns (size_delta, is_new_file)
    let (size_delta, is_new_file) =
        db::update_cached_file(bucket, account_id, key, new_size, last_modified)
            .await
            .map_err(|e| format!("Failed to update file cache: {}", e))?;

    // Update directory tree - create nodes if needed (for new folders)
    db::update_directory_tree_for_file(
        bucket,
        account_id,
        key,
        size_delta,
        last_modified,
        is_new_file,
    )
    .await
    .map_err(|e| format!("Failed to update directory tree: {}", e))?;

    let _ = app.emit(
        "cache-updated",
        CacheUpdatedEvent {
            action: "update".to_string(),
            affected_paths: get_unique_parent_paths(&[key.to_string()]),
        },
    );

    Ok(())
}
