use crate::db;
use log::{info, warn};
use tauri::{AppHandle, Emitter};

use super::types::MoveStatusChanged;

pub(crate) async fn update_move_status(
    app: &AppHandle,
    task_id: &str,
    status: &str,
    error: Option<String>,
) {
    match error.as_ref() {
        Some(err) => warn!("move_status: {} -> {} error={}", task_id, status, err),
        None => info!("move_status: {} -> {}", task_id, status),
    }
    if let Err(db_error) = db::update_move_status(task_id, status, error.as_deref()).await {
        log::error!("Could not persist move status for {task_id}: {db_error}");
        return;
    }
    let _ = app.emit(
        "move-status-changed",
        MoveStatusChanged {
            task_id: task_id.to_string(),
            status: status.to_string(),
            error,
            scope: db::move_sessions::get_move_session(task_id)
                .await
                .ok()
                .flatten()
                .map(Into::into),
        },
    );
}

pub(crate) async fn update_move_status_with_progress(
    app: &AppHandle,
    task_id: &str,
    status: &str,
    progress: i64,
    error: Option<String>,
) {
    match error.as_ref() {
        Some(err) => warn!("move_status: {} -> {} error={}", task_id, status, err),
        None => info!("move_status: {} -> {}", task_id, status),
    }
    if let Err(db_error) =
        db::update_move_status_and_progress(task_id, status, progress, error.as_deref()).await
    {
        log::error!("Could not persist move progress for {task_id}: {db_error}");
        return;
    }
    let _ = app.emit(
        "move-status-changed",
        MoveStatusChanged {
            task_id: task_id.to_string(),
            status: status.to_string(),
            error,
            scope: db::move_sessions::get_move_session(task_id)
                .await
                .ok()
                .flatten()
                .map(Into::into),
        },
    );
}
