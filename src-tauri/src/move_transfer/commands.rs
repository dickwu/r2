//! Move transfer Tauri commands

use crate::db::{self, MoveSession};
use chrono::Utc;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    #[serde(default)]
    pub overwrite: bool,
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
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "move-{}-{}-{}",
        Utc::now().timestamp_millis(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
        index
    )
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

    let mut destinations = std::collections::HashSet::new();
    for operation in &operations {
        if operation.overwrite {
            return Err(
                "Replacing existing destinations requires a separate explicit operation".into(),
            );
        }
        if operation.source_key.is_empty() || operation.dest_key.is_empty() {
            return Err("Move source and destination keys must not be empty".into());
        }
        if !destinations.insert(&operation.dest_key) {
            return Err("Multiple source objects map to the same destination key".into());
        }
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

    let source_cfg = build_move_config(&source_config)?;
    let dest_cfg = build_move_config(&dest_config)?;
    // Validate endpoints before writing tasks that could never run.
    super::planner::scope(&source_cfg)?;
    super::planner::scope(&dest_cfg)?;
    let keys = operations
        .iter()
        .map(|op| op.source_key.clone())
        .collect::<Vec<_>>();
    let cached_sizes =
        db::move_sessions::get_move_cached_sizes(&source_bucket, &source_account_id, &keys)
            .await
            .map_err(|e| format!("Failed to read cached sizes: {e}"))?;

    // Build all sessions first, then batch insert for speed
    let mut sessions_to_create: Vec<MoveSession> = Vec::with_capacity(operations.len());

    for (index, op) in operations.iter().enumerate() {
        let task_id = build_task_id(index);
        let file_size = cached_sizes.get(&op.source_key).copied().unwrap_or(0);

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
    defer_start: Option<bool>,
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
    if defer_start.unwrap_or(false) {
        return Ok(0);
    }
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
        .filter(|s| {
            matches!(
                s.status.as_str(),
                "downloading" | "uploading" | "finishing" | "deleting" | "pending"
            )
        })
        .map(|s| s.id.clone())
        .collect();

    // Pre-create/set pause flags so tasks that are pending or starting can still observe pause.
    {
        let mut registry = MOVE_PAUSE_REGISTRY.lock().unwrap();
        for task_id in &active_ids {
            let paused = registry
                .entry(task_id.clone())
                .or_insert_with(|| Arc::new(AtomicBool::new(true)));
            paused.store(true, Ordering::SeqCst);
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

    {
        let running = MOVE_CANCEL_REGISTRY.lock().unwrap();
        if paused_ids.iter().any(|id| running.contains_key(id)) {
            return Err(
                "Some moves are still stopping; resume after their current requests settle".into(),
            );
        }
    }

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
    let started_count = request_queue_run(&app, &source_bucket, &source_account_id).await;
    info!(
        "resume_all_moves: resumed {} for {}/{} started={}",
        resumed_count, source_bucket, source_account_id, started_count
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
                scope: None,
            },
        );
    } else {
        info!("pause_move: flagged active task {}", task_id);
    }
    Ok(())
}

/// Resume a paused move (set status to pending)
#[tauri::command]
pub async fn resume_move(
    app: AppHandle,
    task_id: String,
    source_config: Option<MoveConfigInput>,
    dest_config: Option<MoveConfigInput>,
) -> Result<(), String> {
    if MOVE_CANCEL_REGISTRY.lock().unwrap().contains_key(&task_id) {
        return Err("Move is still stopping; resume after its current request settles".into());
    }
    let session = db::move_sessions::get_move_session(&task_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Move task does not exist")?;
    match (source_config, dest_config) {
        (Some(source), Some(dest)) => {
            if source.provider != session.source_provider
                || source.account_id != session.source_account_id
                || source.bucket != session.source_bucket
                || dest.provider != session.dest_provider
                || dest.account_id != session.dest_account_id
                || dest.bucket != session.dest_bucket
            {
                return Err("Resume configuration does not match the stored move scope".into());
            }
            let source_cfg = build_move_config(&source)?;
            let dest_cfg = build_move_config(&dest)?;
            register_move_config(
                &source.provider,
                &source.account_id,
                &source.bucket,
                source_cfg,
            );
            register_move_config(&dest.provider, &dest.account_id, &dest.bucket, dest_cfg);
        }
        (None, None) => {}
        _ => return Err("Both source and destination credentials are required to resume".into()),
    }
    MOVE_PAUSE_REGISTRY.lock().unwrap().remove(&task_id);
    MOVE_CANCEL_REGISTRY.lock().unwrap().remove(&task_id);
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
            scope: None,
        },
    );

    request_queue_run(&app, &session.source_bucket, &session.source_account_id).await;
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
        let journal = db::move_sessions::get_move_journal(&task_id)
            .await
            .map_err(|e| e.to_string())?;
        let status = super::worker::recovery_failure_status(
            "cancelled: Move cancelled",
            journal.as_ref().map(|journal| journal.stage.as_str()),
        );
        let error = (status == "outcome_unknown").then(|| {
            "Cancellation cannot undo a possible remote commit; resume to reconcile this task"
                .to_string()
        });
        db::update_move_status(&task_id, status, error.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        let _ = app.emit(
            "move-status-changed",
            MoveStatusChanged {
                task_id: task_id.clone(),
                status: status.to_string(),
                error,
                scope: None,
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
            return Err(
                "Move is stopping; remove its task after the current request settles".into(),
            );
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
