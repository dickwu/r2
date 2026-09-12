//! Move transfer worker - download to temp, upload to destination, optional delete

use crate::db::{self, MoveSession};
use log::{debug, error, info, warn};
use reqwest::Client;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};

use super::config::MoveConfig;
use super::finishing::{run_cache_operations, run_delete_original};
use super::planner::{
    head_identity, plan_transfer, recovery_step, scope, verified_destination, RecoveryStep,
    TransferPlan, TRANSFER_MARKER,
};
use super::state::{update_move_status, update_move_status_with_progress};
use super::stream::{shared_http_client, stream_transfer_without_temp};
use super::types::{MoveProgress, MoveStatusChanged, MAX_CONCURRENT_MOVES};
use crate::db::move_sessions::{get_move_journal, save_move_journal, MoveJournal};

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

fn check_control(cancelled: &AtomicBool, paused: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::SeqCst) {
        return Err("cancelled: Move cancelled; source retained".into());
    }
    if paused.load(Ordering::SeqCst) {
        return Err("paused: Move paused; source retained".into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn move_file_internal(
    client: &Client,
    session: &MoveSession,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    app: &AppHandle,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
) -> Result<Option<MoveUploadResult>, String> {
    check_control(cancelled, paused)?;
    if source_config.bucket() != session.source_bucket
        || dest_config.bucket() != session.dest_bucket
    {
        return Err("conflict: Move configuration no longer matches its stored bucket".into());
    }
    let initial_plan = plan_transfer(
        source_config,
        dest_config,
        &session.source_key,
        &session.dest_key,
        0,
    )?;
    if initial_plan == TransferPlan::NoOp {
        return Ok(None);
    }
    if initial_plan == TransferPlan::RejectConflict {
        return Err("conflict: Source and destination may identify the same object under different credentials".into());
    }
    let source_scope = scope(source_config)?;
    let dest_scope = scope(dest_config)?;
    let stored = get_move_journal(&session.id)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(journal) = &stored {
        if journal.source_scope != source_scope || journal.dest_scope != dest_scope {
            return Err(
                "conflict: Storage endpoint or tenant changed since this move began".into(),
            );
        }
        match recovery_step(&journal.stage) {
            RecoveryStep::Complete => return Ok(None),
            RecoveryStep::Transfer => {}
            RecoveryStep::ReconcileDestination => {
                let Some((identity, head)) = super::stream::protocol::interruptible(
                    cancelled,
                    paused,
                    head_identity(dest_config, &session.dest_key),
                )
                .await??
                else {
                    return Err("outcome_unknown: Destination publication cannot yet be confirmed; source retained".into());
                };
                verified_destination(
                    journal,
                    &identity,
                    head.metadata()
                        .and_then(|m| m.get(TRANSFER_MARKER))
                        .map(String::as_str),
                )?;
                if journal.destination.is_none() {
                    super::planner::verify_unknown_content(
                        source_config,
                        dest_config,
                        &session.source_key,
                        &session.dest_key,
                        &journal.source,
                        &identity,
                    )
                    .await?;
                }
                let mut reconciled = journal.clone();
                reconciled.destination = Some(identity);
                if reconciled.stage == "outcome_unknown" {
                    reconciled.stage = "copied".into();
                }
                save_move_journal(&reconciled)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(Some(MoveUploadResult {
                    uploaded_size: journal.source.size,
                    delete_original: session.delete_original,
                }));
            }
            _ => {
                return Err(
                    "needs_action: Unrecognized move recovery phase; source retained".into(),
                )
            }
        }
    }
    if stored.is_none()
        && (session.progress >= 100
            || db::get_move_upload_session(&session.id)
                .await
                .map_err(|e| e.to_string())?
                .is_some())
    {
        return Err("needs_action: This older move has no frozen source identity; inspect its destination before starting a new move".into());
    }
    let (source_identity, source_head) = super::stream::protocol::interruptible(
        cancelled,
        paused,
        head_identity(source_config, &session.source_key),
    )
    .await??
    .ok_or_else(|| "not_found: Move source does not exist".to_string())?;
    let mut journal = match stored {
        Some(journal) => {
            if journal.source != source_identity {
                return Err(
                    "conflict: Move source changed; existing uploaded parts cannot be reused"
                        .into(),
                );
            }
            journal
        }
        None => MoveJournal {
            task_id: session.id.clone(),
            stage: "transferring".into(),
            source: source_identity,
            source_scope,
            dest_scope,
            destination: None,
        },
    };
    if super::stream::protocol::interruptible(
        cancelled,
        paused,
        head_identity(dest_config, &session.dest_key),
    )
    .await??
    .is_some()
    {
        return Err("conflict: Destination already exists; choose another name. Existing objects are never overwritten by a move.".into());
    }
    save_move_journal(&journal)
        .await
        .map_err(|e| format!("Cannot persist source identity: {e}"))?;
    let mut plan = plan_transfer(
        source_config,
        dest_config,
        &session.source_key,
        &session.dest_key,
        journal.source.size,
    )?;
    if plan == TransferPlan::SingleCopy
        && !super::planner::native_aws(dest_config)
        && !matches!(dest_config, MoveConfig::R2(_))
    {
        use crate::providers::conditional::Condition;
        if !dest_config
            .supports_condition(Condition::CopyCreate)
            .await?
            || !dest_config
                .supports_condition(Condition::CopySource)
                .await?
        {
            plan = TransferPlan::Relay;
        }
    }
    let uploaded_size = match plan {
        TransferPlan::SingleCopy | TransferPlan::MultipartCopy => {
            super::server_copy::copy(
                plan,
                session,
                dest_config,
                &source_head,
                &mut journal,
                cancelled,
                paused,
            )
            .await?
        }
        TransferPlan::Relay => {
            stream_transfer_without_temp(
                client,
                session,
                source_config,
                dest_config,
                app,
                cancelled,
                paused,
            )
            .await?
        }
        _ => return Err("conflict: Invalid transfer plan".into()),
    };
    // The protocol engine may have persisted a response identity. Reload it
    // before checking the currently visible destination.
    let mut journal = get_move_journal(&session.id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Missing move recovery journal")?;
    let (destination, head) = super::stream::protocol::interruptible(
        cancelled,
        paused,
        head_identity(dest_config, &session.dest_key),
    )
    .await??
    .ok_or("outcome_unknown: Destination not visible after upload")?;
    verified_destination(
        &journal,
        &destination,
        head.metadata()
            .and_then(|m| m.get(TRANSFER_MARKER))
            .map(String::as_str),
    )?;
    if journal.destination.is_none() {
        super::planner::verify_unknown_content(
            source_config,
            dest_config,
            &session.source_key,
            &session.dest_key,
            &journal.source,
            &destination,
        )
        .await?;
    }
    if uploaded_size != journal.source.size {
        return Err("conflict: Uploaded length differs from the frozen source".into());
    }
    journal.destination = Some(destination);
    journal.stage = "copied".into();
    save_move_journal(&journal)
        .await
        .map_err(|e| format!("Cannot persist verified destination: {e}"))?;
    check_control(cancelled, paused)?;
    Ok(Some(MoveUploadResult {
        uploaded_size,
        delete_original: session.delete_original,
    }))
}

fn failure_status(error: &str) -> &'static str {
    for status in [
        "paused",
        "cancelled",
        "needs_auth",
        "conflict",
        "outcome_unknown",
        "needs_action",
        "delete_pending",
    ] {
        if error.starts_with(&format!("{status}:")) {
            return status;
        }
    }
    "error"
}

pub(crate) fn recovery_failure_status(error: &str, phase: Option<&str>) -> &'static str {
    let status = failure_status(error);
    if matches!(phase, Some("outcome_unknown" | "delete_unknown"))
        && matches!(status, "cancelled" | "paused" | "error")
    {
        "outcome_unknown"
    } else if status == "cancelled" && phase == Some("delete_pending") {
        "delete_pending"
    } else if matches!(status, "error" | "cancelled")
        && matches!(phase, Some("copied" | "delete_pending"))
    {
        "needs_action"
    } else {
        status
    }
}

async fn report_failure(app: &AppHandle, task_id: &str, error: String) {
    let phase = get_move_journal(task_id).await;
    let status = match &phase {
        Ok(journal) => recovery_failure_status(
            &error,
            journal.as_ref().map(|journal| journal.stage.as_str()),
        ),
        Err(_) => "needs_action",
    };
    let error = if status == "outcome_unknown" {
        format!("outcome_unknown: {error}. A remote mutation may have committed; its recovery record was retained.")
    } else {
        error
    };
    update_move_status(app, task_id, status, Some(error)).await;
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

    let client = match shared_http_client() {
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

    match result {
        Ok(Some(upload_result)) => {
            // Finishing shares the bounded worker lifetime. No unbounded detached
            // delete/cache futures can accumulate behind a slow endpoint.
            update_move_status_with_progress(&app, &session.id, "finishing", 100, None).await;
            let _ = app.emit(
                "move-progress",
                MoveProgress {
                    task_id: session.id.clone(),
                    phase: "uploading".into(),
                    percent: 100,
                    transferred_bytes: upload_result.uploaded_size,
                    total_bytes: upload_result.uploaded_size,
                    speed: 0.0,
                },
            );
            run_cache_operations(
                app.clone(),
                session.id.clone(),
                session.dest_bucket.clone(),
                session.dest_account_id.clone(),
                session.dest_key.clone(),
                upload_result.uploaded_size,
            )
            .await;
            let finished = if upload_result.delete_original {
                run_delete_original(
                    &app,
                    &session,
                    &source_config,
                    &dest_config,
                    &cancelled,
                    &paused,
                )
                .await
            } else {
                async {
                    check_control(&cancelled, &paused)?;
                    let mut journal = get_move_journal(&session.id)
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or("Missing move journal")?;
                    journal.stage = "complete".into();
                    save_move_journal(&journal).await.map_err(|e| e.to_string())
                }
                .await
            };
            match finished {
                Ok(()) => {
                    update_move_status_with_progress(&app, &session.id, "success", 100, None).await
                }
                Err(error) => report_failure(&app, &session.id, error).await,
            }
        }
        Ok(None) => update_move_status_with_progress(&app, &session.id, "success", 100, None).await,
        Err(error) => {
            error!("move_failed: {} error={}", task_id, error);
            report_failure(&app, &task_id, error).await;
        }
    }
    cleanup_registries(&task_id);
    schedule_queue_continuation(app, source_bucket, source_account_id);
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
                if MOVE_CANCEL_REGISTRY
                    .lock()
                    .unwrap()
                    .contains_key(&next_session.id)
                {
                    continue;
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
                        scope: None,
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

#[cfg(test)]
mod recovery_tests {
    use super::recovery_failure_status;
    #[test]
    fn cancellation_does_not_turn_an_unknown_commit_into_a_finished_task() {
        for phase in ["outcome_unknown", "delete_unknown"] {
            assert_eq!(
                recovery_failure_status("cancelled: Move cancelled", Some(phase)),
                "outcome_unknown"
            );
            assert_eq!(
                recovery_failure_status("paused: Move paused", Some(phase)),
                "outcome_unknown"
            );
        }
        assert_eq!(
            recovery_failure_status("cancelled: Move cancelled", Some("transferring")),
            "cancelled"
        );
        assert_eq!(
            recovery_failure_status("Cannot persist deletion", Some("delete_pending")),
            "needs_action"
        );
        assert_eq!(
            recovery_failure_status("cancelled: Move cancelled", Some("copied")),
            "needs_action"
        );
        assert_eq!(
            recovery_failure_status("cancelled: Move cancelled", Some("delete_pending")),
            "delete_pending"
        );
    }
}
