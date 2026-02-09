//! Move transfer worker - download to temp, upload to destination, optional delete

use crate::db::{self, MoveSession};
use crate::providers::{aws, minio};
use crate::r2::{self};
use log::{debug, error, info, warn};
use reqwest::Client;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};

use super::config::MoveConfig;
use super::finishing::{run_cache_operations, run_delete_original};
use super::state::{update_move_status, update_move_status_with_progress};
use super::stream::stream_transfer_without_temp;
use super::types::{MoveProgress, MoveStatusChanged, MAX_CONCURRENT_MOVES};

// Global cancel/pause registry for move tasks (using std::sync::Mutex for Send compatibility)
lazy_static::lazy_static! {
    pub(crate) static ref MOVE_CANCEL_REGISTRY: Mutex<HashMap<String, Arc<AtomicBool>>> =
        Mutex::new(HashMap::new());
    pub(crate) static ref MOVE_PAUSE_REGISTRY: Mutex<HashMap<String, Arc<AtomicBool>>> =
        Mutex::new(HashMap::new());
    static ref MOVE_CONFIG_REGISTRY: Mutex<HashMap<String, MoveConfig>> = Mutex::new(HashMap::new());
    static ref MOVE_QUEUE_SENDERS: Mutex<HashMap<String, mpsc::Sender<QueueSignal>>> =
        Mutex::new(HashMap::new());
}

enum QueueSignal {
    Continue,
    RunOnce { respond: oneshot::Sender<i64> },
}

fn config_key(provider: &str, account_id: &str, bucket: &str) -> String {
    format!("{}:{}:{}", provider, account_id, bucket)
}

pub(crate) fn register_move_config(
    provider: &str,
    account_id: &str,
    bucket: &str,
    config: MoveConfig,
) {
    let key = config_key(provider, account_id, bucket);
    let mut registry = MOVE_CONFIG_REGISTRY.lock().unwrap();
    registry.insert(key, config);
}

fn get_move_config(provider: &str, account_id: &str, bucket: &str) -> Option<MoveConfig> {
    let key = config_key(provider, account_id, bucket);
    let registry = MOVE_CONFIG_REGISTRY.lock().unwrap();
    registry.get(&key).cloned()
}

fn queue_key(source_bucket: &str, source_account_id: &str) -> String {
    format!("{}:{}", source_account_id, source_bucket)
}

fn get_or_create_queue_sender(
    app: &AppHandle,
    source_bucket: &str,
    source_account_id: &str,
) -> mpsc::Sender<QueueSignal> {
    let key = queue_key(source_bucket, source_account_id);
    let mut senders = MOVE_QUEUE_SENDERS.lock().unwrap();
    if let Some(sender) = senders.get(&key) {
        return sender.clone();
    }
    let (sender, receiver) = mpsc::channel(8);
    let app_clone = app.clone();
    let source_bucket = source_bucket.to_string();
    let source_account_id = source_account_id.to_string();
    tokio::spawn(async move {
        run_queue_worker(app_clone, source_bucket, source_account_id, receiver).await;
    });
    senders.insert(key, sender.clone());
    sender
}

/// Result of move_file_internal indicating upload completion
struct MoveUploadResult {
    uploaded_size: u64,
    delete_original: bool,
}

async fn try_server_side_copy(
    session: &MoveSession,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
) -> Option<MoveUploadResult> {
    info!(
        "move_copy_try: {} {}/{} -> {}/{}",
        session.id,
        session.source_account_id,
        session.source_bucket,
        session.dest_account_id,
        session.dest_bucket
    );
    let copy_result = match (source_config, dest_config) {
        (MoveConfig::R2(_), MoveConfig::R2(dest_cfg)) => r2::copy_object_between_buckets(
            dest_cfg,
            &session.source_bucket,
            &session.source_key,
            &session.dest_key,
        )
        .await
        .map_err(|e| format!("R2 copy failed: {}", e)),
        (MoveConfig::Aws(_), MoveConfig::Aws(dest_cfg)) => aws::copy_object_between_buckets(
            dest_cfg,
            &session.source_bucket,
            &session.source_key,
            &session.dest_key,
        )
        .await
        .map_err(|e| format!("AWS copy failed: {}", e)),
        (MoveConfig::Minio(_), MoveConfig::Minio(dest_cfg))
        | (MoveConfig::Rustfs(_), MoveConfig::Rustfs(dest_cfg)) => {
            minio::copy_object_between_buckets(
                dest_cfg,
                &session.source_bucket,
                &session.source_key,
                &session.dest_key,
            )
            .await
            .map_err(|e| format!("MinIO copy failed: {}", e))
        }
        _ => return None,
    };

    let file_size = if session.file_size > 0 {
        session.file_size as u64
    } else {
        db::get_cached_file_size(
            &session.source_bucket,
            &session.source_account_id,
            &session.source_key,
        )
        .await
        .unwrap_or(0) as u64
    };

    match copy_result {
        Ok(()) => {
            info!(
                "move_copy_finish: {} size={} delete_original={}",
                session.id, file_size, session.delete_original
            );
            Some(MoveUploadResult {
                uploaded_size: file_size,
                delete_original: session.delete_original,
            })
        }
        Err(err) => {
            warn!(
                "move_copy_failed: {} error={}, fallback to stream",
                session.id, err
            );
            None
        }
    }
}

async fn move_file_internal(
    client: &Client,
    session: &MoveSession,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    app: &AppHandle,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
) -> Result<Option<MoveUploadResult>, String> {
    // Try server-side copy first (finishing handled in background)
    if let Some(upload_result) = try_server_side_copy(session, source_config, dest_config).await {
        return Ok(Some(upload_result));
    }

    let uploaded_size = stream_transfer_without_temp(
        client,
        session,
        source_config,
        dest_config,
        app,
        cancelled,
        paused,
    )
    .await?;

    if cancelled.load(Ordering::SeqCst) {
        update_move_status(app, &session.id, "cancelled", None).await;
        return Err("Move cancelled".to_string());
    }

    // Upload complete - return result for background processing
    Ok(Some(MoveUploadResult {
        uploaded_size,
        delete_original: session.delete_original,
    }))
}

/// Spawn a move task
pub(crate) async fn spawn_move_task(
    app: AppHandle,
    session: MoveSession,
    source_config: MoveConfig,
    dest_config: MoveConfig,
) {
    let task_id = session.id.clone();
    let source_bucket = session.source_bucket.clone();
    let source_account_id = session.source_account_id.clone();
    info!(
        "spawn_move_task: {} {} -> {} delete_original={} size={}",
        task_id, session.source_key, session.dest_key, session.delete_original, session.file_size
    );

    // Reuse any pre-existing flags set by pause/cancel commands to avoid races.
    let cancelled = {
        let mut cancel_registry = MOVE_CANCEL_REGISTRY.lock().unwrap();
        cancel_registry
            .entry(task_id.clone())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    };
    let paused = {
        let mut pause_registry = MOVE_PAUSE_REGISTRY.lock().unwrap();
        pause_registry
            .entry(task_id.clone())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    };

    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            update_move_status(&app, &task_id, "error", Some(e.to_string())).await;
            cleanup_registries(&task_id);
            schedule_queue_continuation(app, source_bucket, source_account_id);
            return;
        }
    };

    let result = move_file_internal(
        &client,
        &session,
        &source_config,
        &dest_config,
        &app,
        &cancelled,
        &paused,
    )
    .await;

    let mut scheduled_continuation = false;

    match result {
        Ok(Some(upload_result)) => {
            // Upload succeeded - mark finishing and emit 100% progress once
            update_move_status_with_progress(&app, &session.id, "finishing", 100, None).await;
            let _ = app.emit(
                "move-progress",
                MoveProgress {
                    task_id: session.id.clone(),
                    phase: "uploading".to_string(),
                    percent: 100,
                    transferred_bytes: upload_result.uploaded_size,
                    total_bytes: upload_result.uploaded_size,
                    speed: 0.0,
                },
            );
            info!(
                "move_upload_complete: {} uploaded_size={} delete_original={}",
                task_id, upload_result.uploaded_size, upload_result.delete_original
            );

            if upload_result.delete_original {
                // Update status to "deleting" immediately, run delete in background
                update_move_status_with_progress(&app, &session.id, "deleting", 100, None).await;
                let app_for_cache = app.clone();
                let app_for_delete = app.clone();
                let task_id_cleanup = task_id.clone();
                let dest_bucket_clone = session.dest_bucket.clone();
                let dest_account_id_clone = session.dest_account_id.clone();
                let dest_key_clone = session.dest_key.clone();
                let uploaded_size = upload_result.uploaded_size;
                let session_for_delete = session.clone();
                let source_config_for_delete = source_config.clone();

                schedule_queue_continuation(
                    app.clone(),
                    source_bucket.clone(),
                    source_account_id.clone(),
                );
                scheduled_continuation = true;

                // Spawn cache update + delete original in background, cleanup after both
                tokio::spawn(async move {
                    let cache_future = run_cache_operations(
                        app_for_cache,
                        task_id_cleanup.clone(),
                        dest_bucket_clone,
                        dest_account_id_clone,
                        dest_key_clone,
                        uploaded_size,
                    );
                    let delete_future = run_delete_original(
                        app_for_delete,
                        session_for_delete,
                        source_config_for_delete,
                    );
                    let _ = tokio::join!(cache_future, delete_future);
                    cleanup_registries(&task_id_cleanup);
                });
            } else {
                // No delete needed - stay in finishing until cache update completes
                let app_for_cache = app.clone();
                let app_for_status = app.clone();
                let task_id_cleanup = session.id.clone();
                let dest_bucket_clone = session.dest_bucket.clone();
                let dest_account_id_clone = session.dest_account_id.clone();
                let dest_key_clone = session.dest_key.clone();
                let uploaded_size = upload_result.uploaded_size;

                schedule_queue_continuation(
                    app.clone(),
                    source_bucket.clone(),
                    source_account_id.clone(),
                );
                scheduled_continuation = true;

                // Spawn cache update in background (non-blocking), cleanup after
                tokio::spawn(async move {
                    run_cache_operations(
                        app_for_cache,
                        task_id_cleanup.clone(),
                        dest_bucket_clone,
                        dest_account_id_clone,
                        dest_key_clone,
                        uploaded_size,
                    )
                    .await;
                    update_move_status_with_progress(
                        &app_for_status,
                        &task_id_cleanup,
                        "success",
                        100,
                        None,
                    )
                    .await;
                    cleanup_registries(&task_id_cleanup);
                });
            }
        }
        Ok(None) => {
            // Server-side copy completed everything, nothing more to do
            info!("move_server_side_copy_done: {}", task_id);
            cleanup_registries(&task_id);
        }
        Err(e) => {
            if !e.contains("paused") && !e.contains("cancelled") {
                let err = e.clone();
                update_move_status(&app, &task_id, "error", Some(err)).await;
                error!("move_failed: {} error={}", task_id, e);
            }
            cleanup_registries(&task_id);
        }
    }

    // After upload completes, IMMEDIATELY schedule next pending tasks
    // Post-upload operations (cache update, delete) run in parallel
    if !scheduled_continuation {
        schedule_queue_continuation(app, source_bucket, source_account_id);
    }
}

/// Cleanup registries for a task
fn cleanup_registries(task_id: &str) {
    {
        let mut cancel_registry = MOVE_CANCEL_REGISTRY.lock().unwrap();
        cancel_registry.remove(task_id);
    }
    {
        let mut pause_registry = MOVE_PAUSE_REGISTRY.lock().unwrap();
        pause_registry.remove(task_id);
    }
}

async fn run_queue_worker(
    app: AppHandle,
    source_bucket: String,
    source_account_id: String,
    mut receiver: mpsc::Receiver<QueueSignal>,
) {
    while let Some(signal) = receiver.recv().await {
        let mut responders = Vec::new();
        if let QueueSignal::RunOnce { respond } = signal {
            responders.push(respond);
        }
        while let Ok(next_signal) = receiver.try_recv() {
            if let QueueSignal::RunOnce { respond } = next_signal {
                responders.push(respond);
            }
        }
        let started = continue_move_queue(&app, &source_bucket, &source_account_id).await;
        for respond in responders {
            let _ = respond.send(started);
        }
    }
}

/// Schedule queue continuation for a source
fn schedule_queue_continuation(app: AppHandle, source_bucket: String, source_account_id: String) {
    let sender = get_or_create_queue_sender(&app, &source_bucket, &source_account_id);
    if sender.try_send(QueueSignal::Continue).is_err() {
        debug!(
            "schedule_queue_continuation: skip pending {}/{}",
            source_account_id, source_bucket
        );
    }
}

/// Request a single queue run and return number started
pub(crate) async fn request_queue_run(
    app: &AppHandle,
    source_bucket: &str,
    source_account_id: &str,
) -> i64 {
    let sender = get_or_create_queue_sender(app, source_bucket, source_account_id);
    let (respond, receiver) = oneshot::channel();
    if sender.send(QueueSignal::RunOnce { respond }).await.is_err() {
        return 0;
    }
    receiver.await.unwrap_or(0)
}

/// Continue processing the move queue by starting next pending tasks
async fn continue_move_queue(app: &AppHandle, source_bucket: &str, source_account_id: &str) -> i64 {
    debug!(
        "continue_move_queue: {}/{}",
        source_account_id, source_bucket
    );
    match get_pending_sessions_to_start(source_bucket, source_account_id).await {
        Ok((next_sessions, slots_available)) => {
            if next_sessions.is_empty() || slots_available <= 0 {
                debug!(
                    "continue_move_queue: no pending sessions for {}/{}",
                    source_account_id, source_bucket
                );
                return 0;
            }
            let mut started = 0;
            let mut blocked_missing_source = 0;
            let mut blocked_missing_dest = 0;
            for next_session in next_sessions {
                if started >= slots_available {
                    break;
                }
                let source_config = match get_move_config(
                    &next_session.source_provider,
                    &next_session.source_account_id,
                    &next_session.source_bucket,
                ) {
                    Some(cfg) => cfg,
                    None => {
                        blocked_missing_source += 1;
                        warn!(
                            "queue_blocked_missing_source: task={} {}/{} provider={}",
                            next_session.id,
                            next_session.source_account_id,
                            next_session.source_bucket,
                            next_session.source_provider
                        );
                        continue;
                    }
                };
                let dest_config = match get_move_config(
                    &next_session.dest_provider,
                    &next_session.dest_account_id,
                    &next_session.dest_bucket,
                ) {
                    Some(cfg) => cfg,
                    None => {
                        blocked_missing_dest += 1;
                        warn!(
                            "queue_blocked_missing_dest: task={} {}/{} provider={}",
                            next_session.id,
                            next_session.dest_account_id,
                            next_session.dest_bucket,
                            next_session.dest_provider
                        );
                        continue;
                    }
                };

                if let Err(e) = db::update_move_status(&next_session.id, "downloading", None).await
                {
                    error!(
                        "queue_start_failed_status_update: task={} error={}",
                        next_session.id, e
                    );
                    continue;
                }
                let _ = app.emit(
                    "move-status-changed",
                    MoveStatusChanged {
                        task_id: next_session.id.clone(),
                        status: "downloading".to_string(),
                        error: None,
                    },
                );

                let app_clone = app.clone();
                tokio::spawn(async move {
                    spawn_move_task(app_clone, next_session, source_config, dest_config).await;
                });
                started += 1;
            }

            if blocked_missing_source > 0 || blocked_missing_dest > 0 {
                info!(
                    "queue_waiting_for_configs: {}/{} started={} blocked_source={} blocked_dest={}",
                    source_account_id,
                    source_bucket,
                    started,
                    blocked_missing_source,
                    blocked_missing_dest
                );
            }
            started
        }
        Err(err) => {
            error!(
                "continue_move_queue: failed for {}/{} error={}",
                source_account_id, source_bucket, err
            );
            0
        }
    }
}

/// Internal queue helper - returns pending candidates plus available worker slots.
pub(crate) async fn get_pending_sessions_to_start(
    source_bucket: &str,
    source_account_id: &str,
) -> Result<(Vec<MoveSession>, i64), String> {
    let active_count = db::count_active_moves(source_bucket, source_account_id)
        .await
        .map_err(|e| format!("Failed to count active moves: {}", e))?;

    let slots_available = MAX_CONCURRENT_MOVES - active_count;
    info!(
        "queue_check: {}/{} active={} slots={}",
        source_account_id, source_bucket, active_count, slots_available
    );
    if slots_available <= 0 {
        debug!(
            "queue_check: no slots for {}/{}",
            source_account_id, source_bucket
        );
        return Ok((Vec::new(), 0));
    }

    // Scan ahead so tasks waiting for different destination account configs do not block
    // other ready tasks at the front of the queue.
    let scan_limit = std::cmp::max(slots_available * 20, slots_available);
    let pending = db::get_pending_moves_for_source(source_bucket, source_account_id, scan_limit)
        .await
        .map_err(|e| format!("Failed to get pending moves: {}", e))?;

    if !pending.is_empty() {
        let ids: Vec<&str> = pending.iter().map(|s| s.id.as_str()).collect();
        debug!(
            "queue_pick: {}/{} picked {} (slots={}) [{}]",
            source_account_id,
            source_bucket,
            pending.len(),
            slots_available,
            ids.join(", ")
        );
    }

    Ok((pending, slots_available))
}
