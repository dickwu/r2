use crate::commands::cache_events::{get_unique_parent_paths, CacheUpdatedEvent};
use crate::db;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// Where a cache update reports the folders it changed. The app forwards the
/// events to the webview; tests record them.
pub(crate) trait CacheEventSink {
    fn emit_cache_event<S: Serialize + Clone>(&self, event: &str, payload: S);
}

impl CacheEventSink for AppHandle {
    fn emit_cache_event<S: Serialize + Clone>(&self, event: &str, payload: S) {
        let _ = self.emit(event, payload);
    }
}

pub(crate) async fn update_cache_after_upload(
    app: &impl CacheEventSink,
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
        app.emit_cache_event(
            "cache-updated",
            CacheUpdatedEvent {
                action: "update".into(),
                affected_paths: get_unique_parent_paths(&[key.to_string()]),
            },
        );
        return Ok(());
    }

    let mutation_token = db::begin_local_cache_mutation(bucket, account_id)
        .await
        .map_err(|e| format!("Failed to start cache mutation: {}", e))?;
    let mutation_result = async {
        let (size_delta, is_new_file) =
            db::update_cached_file(bucket, account_id, key, new_size, last_modified)
                .await
                .map_err(|e| format!("Failed to update file cache: {}", e))?;
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
        Ok::<(), String>(())
    }
    .await;
    let finish_result = db::finish_local_cache_mutation(bucket, account_id, &mutation_token)
        .await
        .map_err(|e| format!("Failed to finish cache mutation: {}", e));
    if let Err(error) = mutation_result {
        let _ = finish_result;
        return Err(error);
    }
    finish_result?;

    app.emit_cache_event(
        "cache-updated",
        CacheUpdatedEvent {
            action: "update".to_string(),
            affected_paths: get_unique_parent_paths(&[key.to_string()]),
        },
    );

    Ok(())
}
