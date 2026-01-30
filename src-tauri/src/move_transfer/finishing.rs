//! Post-upload finishing tasks (cache update, delete source)

use crate::commands::delete_cache::queue_cache_after_delete;
use crate::commands::upload_cache::update_cache_after_upload;
use crate::db::MoveSession;
use crate::providers::{aws, minio};
use crate::r2;
use chrono::Utc;
use log::{error, info};
use tauri::AppHandle;

use super::config::MoveConfig;
use super::state::{update_move_status, update_move_status_with_progress};

async fn delete_source_object(config: &MoveConfig, key: &str) -> Result<(), String> {
    match config {
        MoveConfig::R2(cfg) => r2::delete_object(cfg, key)
            .await
            .map_err(|e| format!("Failed to delete R2 object: {}", e)),
        MoveConfig::Aws(cfg) => aws::delete_object(cfg, key)
            .await
            .map_err(|e| format!("Failed to delete AWS object: {}", e)),
        MoveConfig::Minio(cfg) => minio::delete_object(cfg, key)
            .await
            .map_err(|e| format!("Failed to delete MinIO object: {}", e)),
        MoveConfig::Rustfs(cfg) => minio::delete_object(cfg, key)
            .await
            .map_err(|e| format!("Failed to delete RustFS object: {}", e)),
    }
}

/// Run post-upload cache operations in background (non-blocking)
pub(crate) async fn run_cache_operations(
    app: AppHandle,
    task_id: String,
    dest_bucket: String,
    dest_account_id: String,
    dest_key: String,
    uploaded_size: u64,
) {
    info!(
        "finishing_cache_start: {} {}/{} key={} size={}",
        task_id, dest_account_id, dest_bucket, dest_key, uploaded_size
    );
    let last_modified = Utc::now().to_rfc3339();
    let _ = update_cache_after_upload(
        &app,
        &dest_bucket,
        &dest_account_id,
        &dest_key,
        uploaded_size as i64,
        &last_modified,
    )
    .await;
    info!(
        "finishing_cache_done: {} {}/{} key={}",
        task_id, dest_account_id, dest_bucket, dest_key
    );
}

/// Run delete original operation in background, then update status
pub(crate) async fn run_delete_original(
    app: AppHandle,
    session: MoveSession,
    source_config: MoveConfig,
) {
    info!(
        "finishing_delete_start: {} {}/{} key={}",
        session.id, session.source_account_id, session.source_bucket, session.source_key
    );
    // Delete original
    if let Err(e) = delete_source_object(&source_config, &session.source_key).await {
        update_move_status(&app, &session.id, "error", Some(e)).await;
        error!(
            "finishing_delete_failed: {} {}/{} key={}",
            session.id, session.source_account_id, session.source_bucket, session.source_key
        );
        return;
    }

    // Mark as complete immediately (cache updates are queued)
    update_move_status_with_progress(&app, &session.id, "success", 100, None).await;

    // Queue source cache updates to avoid repeated directory calculations
    queue_cache_after_delete(
        app.clone(),
        session.source_bucket.clone(),
        session.source_account_id.clone(),
        session.source_key.clone(),
    )
    .await;
    info!(
        "finishing_delete_done: {} {}/{} key={}",
        session.id, session.source_account_id, session.source_bucket, session.source_key
    );
}
