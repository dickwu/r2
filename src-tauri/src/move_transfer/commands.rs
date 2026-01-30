//! Move transfer Tauri commands

use crate::db::{self, MoveSession};
use chrono::Utc;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter};

use super::config::MoveConfig;
use super::types::{MoveBatchOperation, MoveStatusChanged, MoveTaskDeleted};
use super::worker::{
    register_move_config, request_queue_run, MOVE_CANCEL_REGISTRY, MOVE_PAUSE_REGISTRY,
};

#[derive(Debug, Deserialize)]
pub struct MoveConfigInput {
    pub provider: String,
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: Option<String>,
    pub endpoint_scheme: Option<String>,
    pub endpoint_host: Option<String>,
    pub force_path_style: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct MoveOperationInput {
    pub source_key: String,
    pub dest_key: String,
}

#[derive(Debug, Serialize)]
pub struct StartMoveResult {
    pub created: i64,
}

fn build_move_config(input: &MoveConfigInput) -> Result<MoveConfig, String> {
    match input.provider.as_str() {
        "aws" => {
            let region = input
                .region
                .as_ref()
                .ok_or_else(|| "AWS region is required".to_string())?
                .to_string();
            Ok(MoveConfig::Aws(crate::providers::aws::AwsConfig {
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
                region,
                endpoint_scheme: input.endpoint_scheme.clone(),
                endpoint_host: input.endpoint_host.clone(),
                force_path_style: input.force_path_style.unwrap_or(false),
            }))
        }
        "minio" => {
            let endpoint_scheme = input
                .endpoint_scheme
                .clone()
                .unwrap_or_else(|| "https".to_string());
            let endpoint_host = input
                .endpoint_host
                .clone()
                .ok_or_else(|| "MinIO endpoint host is required".to_string())?;
            Ok(MoveConfig::Minio(crate::providers::minio::MinioConfig {
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
                endpoint_scheme,
                endpoint_host,
                force_path_style: input.force_path_style.unwrap_or(true),
            }))
        }
        "rustfs" => {
            let endpoint_scheme = input
                .endpoint_scheme
                .clone()
                .unwrap_or_else(|| "https".to_string());
            let endpoint_host = input
                .endpoint_host
                .clone()
                .ok_or_else(|| "RustFS endpoint host is required".to_string())?;
            Ok(MoveConfig::Rustfs(crate::providers::minio::MinioConfig {
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
                endpoint_scheme,
                endpoint_host,
                force_path_style: true,
            }))
        }
        "r2" => Ok(MoveConfig::R2(crate::r2::R2Config {
            account_id: input.account_id.clone(),
            bucket: input.bucket.clone(),
            access_key_id: input.access_key_id.clone(),
            secret_access_key: input.secret_access_key.clone(),
        })),
        _ => Err(format!("Unsupported provider: {}", input.provider)),
    }
}

fn build_task_id(index: usize) -> String {
    format!("move-{}-{}", Utc::now().timestamp_millis(), index)
}

/// Create move sessions and start processing queue
#[tauri::command]
pub async fn start_batch_move(
    app: AppHandle,
    source_config: MoveConfigInput,
    dest_config: MoveConfigInput,
    operations: Vec<MoveOperationInput>,
    delete_original: bool,
) -> Result<StartMoveResult, String> {
    if operations.is_empty() {
        return Ok(StartMoveResult { created: 0 });
    }

    let source_bucket = source_config.bucket.clone();
    let source_account_id = source_config.account_id.clone();
    let now = Utc::now().timestamp();
    info!(
        "start_batch_move: {} ops from {}/{} to {}/{} delete_original={}",
        operations.len(),
        source_config.provider,
        source_bucket,
        dest_config.provider,
        dest_config.bucket,
        delete_original
    );

    // Build all sessions first, then batch insert for speed
    let mut sessions_to_create: Vec<MoveSession> = Vec::with_capacity(operations.len());

    for (index, op) in operations.iter().enumerate() {
        let task_id = build_task_id(index);
        let file_size =
            db::get_cached_file_size(&source_bucket, &source_account_id, &op.source_key)
                .await
                .unwrap_or(0);

        sessions_to_create.push(MoveSession {
            id: task_id,
            source_key: op.source_key.clone(),
            dest_key: op.dest_key.clone(),
            source_bucket: source_bucket.clone(),
            source_account_id: source_account_id.clone(),
            source_provider: source_config.provider.clone(),
            dest_bucket: dest_config.bucket.clone(),
            dest_account_id: dest_config.account_id.clone(),
            dest_provider: dest_config.provider.clone(),
            delete_original,
            file_size,
            progress: 0,
            status: "pending".to_string(),
            error: None,
            created_at: now,
            updated_at: now,
        });
    }

    // Batch insert all sessions in a single transaction
    db::create_move_sessions_batch(&sessions_to_create)
        .await
        .map_err(|e| format!("Failed to create move sessions: {}", e))?;
    info!(
        "start_batch_move: created {} sessions for {}/{}",
        sessions_to_create.len(),
        source_bucket,
        source_account_id
    );

    let source_cfg = build_move_config(&source_config)?;
    let dest_cfg = build_move_config(&dest_config)?;
    register_move_config(
        &source_config.provider,
        &source_config.account_id,
        &source_config.bucket,
        source_cfg,
    );
    register_move_config(
        &dest_config.provider,
        &dest_config.account_id,
        &dest_config.bucket,
        dest_cfg,
    );
    let started = request_queue_run(&app, &source_bucket, &source_account_id).await;
    info!(
        "start_batch_move: queued {} sessions for {}/{} started={}",
        operations.len(),
        source_bucket,
        source_account_id,
        started
    );

    Ok(StartMoveResult {
        created: operations.len() as i64,
    })
}

/// Start pending moves (used after resume)
#[tauri::command]
pub async fn start_move_queue(
    app: AppHandle,
    source_config: MoveConfigInput,
    dest_config: MoveConfigInput,
) -> Result<i64, String> {
    let source_bucket = source_config.bucket.clone();
    let source_account_id = source_config.account_id.clone();
    info!(
        "start_move_queue: source {}/{} dest {}/{}",
        source_config.provider, source_bucket, dest_config.provider, dest_config.bucket
    );
    let source_cfg = build_move_config(&source_config)?;
    let dest_cfg = build_move_config(&dest_config)?;
    register_move_config(
        &source_config.provider,
        &source_config.account_id,
        &source_config.bucket,
        source_cfg,
    );
    register_move_config(
        &dest_config.provider,
        &dest_config.account_id,
        &dest_config.bucket,
        dest_cfg,
    );
    let started_count = request_queue_run(&app, &source_bucket, &source_account_id).await;
    info!(
        "start_move_queue: starting {} sessions for {}/{}",
        started_count, source_bucket, source_account_id
    );

    Ok(started_count)
}

/// Pause all active moves
#[tauri::command]
pub async fn pause_all_moves(
    app: AppHandle,
    source_bucket: String,
    source_account_id: String,
) -> Result<i64, String> {
    // Get task IDs for this bucket/account before setting pause flags
    let active_sessions = db::get_move_sessions_for_source(&source_bucket, &source_account_id)
        .await
        .map_err(|e| format!("Failed to get sessions: {}", e))?;

    let active_ids: Vec<String> = active_sessions
        .iter()
        .filter(|s| s.status == "downloading" || s.status == "uploading" || s.status == "pending")
        .map(|s| s.id.clone())
        .collect();

    // Only set pause flags for tasks belonging to this bucket/account
    {
        let registry = MOVE_PAUSE_REGISTRY.lock().unwrap();
        for task_id in &active_ids {
            if let Some(paused) = registry.get(task_id) {
                paused.store(true, Ordering::SeqCst);
            }
        }
    }

    let paused_count = db::pause_all_moves(&source_bucket, &source_account_id)
        .await
        .map_err(|e| format!("Failed to pause moves: {}", e))?;
    info!(
        "pause_all_moves: paused {} for {}/{}",
        paused_count, source_bucket, source_account_id
    );

    let _ = app.emit(
        "move-batch-operation",
        MoveBatchOperation {
            operation: "pause_all".to_string(),
            source_bucket: source_bucket.clone(),
            source_account_id: source_account_id.clone(),
        },
    );

    Ok(paused_count)
}

/// Resume all paused moves (set to pending)
#[tauri::command]
pub async fn resume_all_moves(
    app: AppHandle,
    source_bucket: String,
    source_account_id: String,
) -> Result<i64, String> {
    // Get paused task IDs before resuming
    let paused_sessions = db::get_move_sessions_for_source(&source_bucket, &source_account_id)
        .await
        .map_err(|e| format!("Failed to get sessions: {}", e))?;

    let paused_ids: Vec<String> = paused_sessions
        .iter()
        .filter(|s| s.status == "paused")
        .map(|s| s.id.clone())
        .collect();

    // Clear pause flags for tasks that will be resumed
    {
        let registry = MOVE_PAUSE_REGISTRY.lock().unwrap();
        for task_id in &paused_ids {
            if let Some(paused) = registry.get(task_id) {
                paused.store(false, Ordering::SeqCst);
            }
        }
    }

    let resumed_count = db::resume_all_moves(&source_bucket, &source_account_id)
        .await
        .map_err(|e| format!("Failed to resume moves: {}", e))?;
    info!(
        "resume_all_moves: resumed {} for {}/{}",
        resumed_count, source_bucket, source_account_id
    );

    let _ = app.emit(
        "move-batch-operation",
        MoveBatchOperation {
            operation: "resume_all".to_string(),
            source_bucket: source_bucket.clone(),
            source_account_id: source_account_id.clone(),
        },
    );

    Ok(resumed_count)
}

/// Pause a single move
#[tauri::command]
pub async fn pause_move(app: AppHandle, task_id: String) -> Result<(), String> {
    let found = {
        let registry = MOVE_PAUSE_REGISTRY.lock().unwrap();
        if let Some(paused) = registry.get(&task_id) {
            paused.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    };
    if !found {
        let _ = db::update_move_status(&task_id, "paused", None).await;
        let _ = app.emit(
            "move-status-changed",
            MoveStatusChanged {
                task_id: task_id.clone(),
                status: "paused".to_string(),
                error: None,
            },
        );
    } else {
        info!("pause_move: flagged active task {}", task_id);
    }
    Ok(())
}

/// Resume a paused move (set status to pending)
#[tauri::command]
pub async fn resume_move(app: AppHandle, task_id: String) -> Result<(), String> {
    db::update_move_status(&task_id, "pending", None)
        .await
        .map_err(|e| format!("Failed to resume move: {}", e))?;
    info!("resume_move: task {}", task_id);

    let _ = app.emit(
        "move-status-changed",
        MoveStatusChanged {
            task_id: task_id.clone(),
            status: "pending".to_string(),
            error: None,
        },
    );

    Ok(())
}

/// Cancel a move
#[tauri::command]
pub async fn cancel_move(app: AppHandle, task_id: String) -> Result<(), String> {
    let found = {
        let registry = MOVE_CANCEL_REGISTRY.lock().unwrap();
        if let Some(cancelled) = registry.get(&task_id) {
            cancelled.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    };
    if !found {
        let _ = db::update_move_status(&task_id, "cancelled", None).await;
        let _ = app.emit(
            "move-status-changed",
            MoveStatusChanged {
                task_id: task_id.clone(),
                status: "cancelled".to_string(),
                error: None,
            },
        );
    } else {
        info!("cancel_move: flagged active task {}", task_id);
    }
    Ok(())
}

/// Delete a move task from the database
#[tauri::command]
pub async fn delete_move_task(app: AppHandle, task_id: String) -> Result<(), String> {
    {
        let registry = MOVE_CANCEL_REGISTRY.lock().unwrap();
        if let Some(cancelled) = registry.get(&task_id) {
            cancelled.store(true, Ordering::SeqCst);
        }
    }

    db::delete_move_session(&task_id)
        .await
        .map_err(|e| format!("Failed to delete move task: {}", e))?;
    info!("delete_move_task: {}", task_id);

    let _ = app.emit(
        "move-task-deleted",
        MoveTaskDeleted {
            task_id: task_id.clone(),
        },
    );

    Ok(())
}

/// Get all move sessions for a source bucket
#[tauri::command]
pub async fn get_move_tasks(
    source_bucket: String,
    source_account_id: String,
) -> Result<Vec<MoveSession>, String> {
    db::get_move_sessions_for_source(&source_bucket, &source_account_id)
        .await
        .map_err(|e| format!("Failed to get move tasks: {}", e))
}

/// Get all active move tasks across all accounts (for global progress display)
#[tauri::command]
pub async fn get_all_active_move_tasks() -> Result<Vec<MoveSession>, String> {
    db::get_all_active_move_sessions()
        .await
        .map_err(|e| format!("Failed to get all active move tasks: {}", e))
}

/// Clear finished move tasks
#[tauri::command]
pub async fn clear_finished_moves(
    app: AppHandle,
    source_bucket: String,
    source_account_id: String,
) -> Result<i64, String> {
    let deleted_count = db::delete_finished_moves(&source_bucket, &source_account_id)
        .await
        .map_err(|e| format!("Failed to clear finished moves: {}", e))?;
    info!(
        "clear_finished_moves: deleted {} for {}/{}",
        deleted_count, source_bucket, source_account_id
    );

    let _ = app.emit(
        "move-batch-operation",
        MoveBatchOperation {
            operation: "clear_finished".to_string(),
            source_bucket: source_bucket.clone(),
            source_account_id: source_account_id.clone(),
        },
    );

    Ok(deleted_count)
}

/// Clear all move tasks (only when no active moves)
#[tauri::command]
pub async fn clear_all_moves(
    app: AppHandle,
    source_bucket: String,
    source_account_id: String,
) -> Result<i64, String> {
    // Use count_in_progress_moves to include deleting tasks
    let active_count = db::count_in_progress_moves(&source_bucket, &source_account_id)
        .await
        .map_err(|e| format!("Failed to count active moves: {}", e))?;

    if active_count > 0 {
        warn!(
            "clear_all_moves: blocked, {} active for {}/{}",
            active_count, source_bucket, source_account_id
        );
        return Err("Cannot clear all moves while moves are active".to_string());
    }

    let deleted_count = db::delete_all_moves(&source_bucket, &source_account_id)
        .await
        .map_err(|e| format!("Failed to clear all moves: {}", e))?;
    info!(
        "clear_all_moves: deleted {} for {}/{}",
        deleted_count, source_bucket, source_account_id
    );

    let _ = app.emit(
        "move-batch-operation",
        MoveBatchOperation {
            operation: "clear_all".to_string(),
            source_bucket: source_bucket.clone(),
            source_account_id: source_account_id.clone(),
        },
    );

    Ok(deleted_count)
}
