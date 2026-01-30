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
    let _ = db::update_move_status(task_id, status, error.as_deref()).await;
    let _ = app.emit(
        "move-status-changed",
        MoveStatusChanged {
            task_id: task_id.to_string(),
            status: status.to_string(),
            error,
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
    let _ = db::update_move_status_and_progress(task_id, status, progress, error.as_deref()).await;
    let _ = app.emit(
        "move-status-changed",
        MoveStatusChanged {
            task_id: task_id.to_string(),
            status: status.to_string(),
            error,
        },
    );
}
