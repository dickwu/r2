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

/// Records the events a cache update reports, as (event, payload) pairs.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecordedCacheEvents(pub std::sync::Mutex<Vec<(String, serde_json::Value)>>);

#[cfg(test)]
impl CacheEventSink for RecordedCacheEvents {
    fn emit_cache_event<S: Serialize + Clone>(&self, event: &str, payload: S) {
        let payload = serde_json::to_value(payload).unwrap();
        self.0.lock().unwrap().push((event.to_string(), payload));
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
        // No scope was captured before this write (e.g. a Move finishing in
        // the background): its rows cannot be attributed to this namespace.
        db::file_cache::relist_unscoped_writes(bucket, account_id, &[key])
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
