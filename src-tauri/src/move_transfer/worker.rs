//! Move transfer worker - download to temp, upload to destination, optional delete

use crate::db::{self, MoveSession};
use crate::providers::conditional::Condition;
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
    head_identity_checked, plan_transfer, recovery_step, scope, verified_destination, RecoveryStep,
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

/// Only a positive capability answer may take a Move off the relay. A probe
/// that fails (network, a 403 on the probe prefix, anything) keeps the relay
/// that 0.3.5 used and is logged; it never fails the Move.
async fn capability_confirmed(
    task_id: &str,
    config: &MoveConfig,
    conditions: &[Condition],
) -> bool {
    for &condition in conditions {
        match config.supports_condition(condition).await {
            Ok(true) => {}
            Ok(false) => return false,
            Err(error) => {
                warn!(
                    "move_capability_probe_failed: {} condition={:?} error={}; relaying",
                    task_id, condition, error
                );
                return false;
            }
        }
    }
    true
}

/// The plan a Move runs: the planner's, with a single server-side copy on an
/// endpoint that is neither native AWS nor R2 only once its conditions are
/// confirmed. Large objects on such endpoints relay, as in v0.3.5: multipart
/// copy there must first be validated on the exact deployment (review NEXT-02).
async fn execution_plan(
    session: &MoveSession,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    size: u64,
) -> Result<TransferPlan, String> {
    let plan = plan_transfer(
        source_config,
        dest_config,
        &session.source_key,
        &session.dest_key,
        size,
    )?;
    if plan == TransferPlan::SingleCopy
        && !super::planner::native_aws(dest_config)
        && !matches!(dest_config, MoveConfig::R2(_))
        && !capability_confirmed(
            &session.id,
            dest_config,
            &[Condition::CopyCreate, Condition::CopySource],
        )
        .await
    {
        return Ok(TransferPlan::Relay);
    }
    Ok(plan)
}

/// Older builds downgraded a missing completion to transferring. A saved
/// MPU and our marker identify a candidate only; bytes still must be verified.
#[allow(clippy::too_many_arguments)]
async fn reconcile_legacy_multipart(
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    session: &MoveSession,
    journal: &mut MoveJournal,
    identity: crate::db::move_sessions::SourceIdentity,
    head: &aws_sdk_s3::operation::head_object::HeadObjectOutput,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<bool, String> {
    if journal.stage != "transferring"
        || head
            .metadata()
            .and_then(|metadata| metadata.get(TRANSFER_MARKER))
            .map(String::as_str)
            != Some(session.id.as_str())
        || db::get_move_upload_session(&session.id)
            .await
            .map_err(|e| e.to_string())?
            .is_none()
    {
        return Ok(false);
    }
    super::stream::reconcile_uploaded_destination(
        source_config,
        dest_config,
        session,
        journal,
        identity,
        head,
        cancelled,
        paused,
    )
    .await?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn move_file_internal(
    client: &Client,
    session: &MoveSession,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    app: Option<&AppHandle>,
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
    let mut stored = get_move_journal(&session.id)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(journal) = stored.clone() {
        if journal.source_scope != source_scope || journal.dest_scope != dest_scope {
            return Err(
                "conflict: Storage endpoint or tenant changed since this move began".into(),
            );
        }
        match recovery_step(&journal.stage) {
            RecoveryStep::Complete => return Ok(None),
            RecoveryStep::Transfer => {}
            RecoveryStep::ReconcileDestination => {
                let observed = super::stream::protocol::interruptible(
                    cancelled,
                    paused,
                    head_identity_checked(dest_config, &session.dest_key, cancelled, paused),
                )
                .await??;
                if let Some((identity, head)) = observed {
                    verified_destination(
                        &journal,
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
                            cancelled,
                            paused,
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
                if journal.stage != "outcome_unknown" {
                    return Err(
                        "outcome_unknown: Verified destination no longer exists; source retained"
                            .into(),
                    );
                }
                let mut recovered = journal.clone();
                if super::stream::recover_absent_multipart(
                    source_config,
                    dest_config,
                    session,
                    &mut recovered,
                    cancelled,
                    paused,
                )
                .await?
                {
                    return Ok(Some(MoveUploadResult {
                        uploaded_size: recovered.source.size,
                        delete_original: session.delete_original,
                    }));
                }
                // The upload ID is proven missing, and a fresh HEAD confirmed
                // absence. Its atomic reset permits only a new conditional upload.
                stored = Some(recovered);
            }
            _ => {
                return Err(
                    "needs_action: Unrecognized move recovery phase; source retained".into(),
                );
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
        head_identity_checked(source_config, &session.source_key, cancelled, paused),
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
            source_scope: source_scope.clone(),
            dest_scope: dest_scope.clone(),
            destination: None,
            retry: Default::default(),
            metrics: Default::default(),
        },
    };
    if let Some((identity, head)) = super::stream::protocol::interruptible(
        cancelled,
        paused,
        head_identity_checked(dest_config, &session.dest_key, cancelled, paused),
    )
    .await??
    {
        if reconcile_legacy_multipart(
            source_config,
            dest_config,
            session,
            &mut journal,
            identity,
            &head,
            cancelled,
            paused,
        )
        .await?
        {
            return Ok(Some(MoveUploadResult {
                uploaded_size: journal.source.size,
                delete_original: session.delete_original,
            }));
        }
        return Err("conflict: Destination already exists; choose another name. Existing objects are never overwritten by a move.".into());
    }
    save_move_journal(&journal)
        .await
        .map_err(|e| format!("Cannot persist source identity: {e}"))?;
    let plan = execution_plan(session, source_config, dest_config, journal.source.size).await?;
    let uploaded_size = match plan {
        TransferPlan::SingleCopy | TransferPlan::MultipartCopy => {
            super::server_copy::copy(
                plan,
                session,
                dest_config,
                &source_head,
                &mut journal,
                app,
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
        head_identity_checked(dest_config, &session.dest_key, cancelled, paused),
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
            cancelled,
            paused,
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

/// Task statuses a failure message can lead with (`<status>: …`); anything
/// else is `error`.
pub(crate) const FAILURE_STATUSES: [&str; 7] = [
    "paused",
    "cancelled",
    "needs_auth",
    "conflict",
    "outcome_unknown",
    "needs_action",
    "delete_pending",
];

fn failure_status(error: &str) -> &'static str {
    for status in FAILURE_STATUSES {
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

fn retry_phase(error: &str, fallback: &str) -> String {
    error
        .split(':')
        .nth(1)
        .map(str::trim)
        .filter(|phase| !phase.is_empty() && !phase.contains(' '))
        .unwrap_or(fallback)
        .to_string()
}

fn schedule_retry_wakeup(app: AppHandle, session: &MoveSession, next_attempt_at: i64) {
    let task_id = session.id.clone();
    let source_bucket = session.source_bucket.clone();
    let source_account_id = session.source_account_id.clone();
    let delay = (next_attempt_at - chrono::Utc::now().timestamp()).max(0) as u64;
    tokio::spawn(async move {
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        }
        let ready = match (
            db::move_sessions::get_move_session(&task_id).await,
            get_move_journal(&task_id).await,
        ) {
            (Ok(Some(session)), Ok(Some(journal))) => {
                session.status == "pending"
                    && journal.retry.next_attempt_at == Some(next_attempt_at)
                    && next_attempt_at <= chrono::Utc::now().timestamp()
            }
            _ => false,
        };
        if ready {
            schedule_queue_continuation(app, source_bucket, source_account_id);
        }
    });
}

fn schedule_retry_wakeup_for_source(
    app: AppHandle,
    source_bucket: String,
    source_account_id: String,
    next_attempt_at: i64,
) {
    let delay = (next_attempt_at - chrono::Utc::now().timestamp()).max(0) as u64;
    tokio::spawn(async move {
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        }
        match db::move_sessions::get_next_move_retry_attempt_for_source(
            &source_bucket,
            &source_account_id,
        )
        .await
        {
            Ok(Some(next)) if next <= chrono::Utc::now().timestamp() => {
                schedule_queue_continuation(app, source_bucket, source_account_id);
            }
            _ => {}
        }
    });
}

async fn report_failure(app: &AppHandle, session: &MoveSession, error: String) {
    let phase = get_move_journal(&session.id).await;
    let status = match &phase {
        Ok(journal) => recovery_failure_status(
            &error,
            journal.as_ref().map(|journal| journal.stage.as_str()),
        ),
        Err(_) => "needs_action",
    };
    let transient_retry = error.starts_with("transient:") || error.contains(": transient:");
    let journal_for_retry = phase.as_ref().ok().and_then(|journal| journal.as_ref());
    let error = if status == "outcome_unknown" {
        format!(
            "outcome_unknown: {error}. A remote mutation may have committed; its recovery record was retained."
        )
    } else {
        error
    };
    if transient_retry {
        let phase_name = journal_for_retry
            .map(|journal| retry_phase(&error, &journal.stage))
            .unwrap_or_else(|| retry_phase(&error, "preflight"));
        let scheduled = if journal_for_retry.is_some() {
            db::move_sessions::schedule_move_retry(&session.id, &phase_name, "transient").await
        } else {
            db::move_sessions::schedule_move_task_retry(&session.id, &phase_name, "transient").await
        };
        match scheduled {
            Ok(Some(next_attempt_at)) => {
                let message = format!("{error}; retry scheduled at {next_attempt_at}");
                update_move_status(app, &session.id, "pending", Some(message)).await;
                if journal_for_retry.is_some() {
                    schedule_retry_wakeup(app.clone(), session, next_attempt_at);
                } else {
                    schedule_retry_wakeup_for_source(
                        app.clone(),
                        session.source_bucket.clone(),
                        session.source_account_id.clone(),
                        next_attempt_at,
                    );
                }
                return;
            }
            Ok(None) => {}
            Err(schedule_error) => {
                update_move_status(
                    app,
                    &session.id,
                    "needs_action",
                    Some(format!(
                        "Cannot persist retry schedule: {schedule_error}; {error}"
                    )),
                )
                .await;
                return;
            }
        }
    }
    update_move_status(app, &session.id, status, Some(error)).await;
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
        Some(&app),
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
                    let _ = db::move_sessions::clear_move_retry(&session.id).await;
                    let _ = db::move_sessions::clear_move_task_retry(&session.id).await;
                    update_move_status_with_progress(&app, &session.id, "success", 100, None).await
                }
                Err(error) => report_failure(&app, &session, error).await,
            }
        }
        Ok(None) => {
            let _ = db::move_sessions::clear_move_retry(&session.id).await;
            let _ = db::move_sessions::clear_move_task_retry(&session.id).await;
            update_move_status_with_progress(&app, &session.id, "success", 100, None).await
        }
        Err(error) => {
            error!("move_failed: {} error={}", task_id, error);
            report_failure(&app, &session, error).await;
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
                if let Ok(Some(next_attempt_at)) =
                    db::move_sessions::get_next_move_retry_attempt_for_source(
                        source_bucket,
                        source_account_id,
                    )
                    .await
                {
                    schedule_retry_wakeup_for_source(
                        app.clone(),
                        source_bucket.to_string(),
                        source_account_id.to_string(),
                        next_attempt_at,
                    );
                }
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
    #[tokio::test]
    async fn legacy_transferring_multipart_verifies_bytes_before_adopting_destination() {
        use super::*;
        use crate::move_transfer::stream::tests::{fixture_config, journal_fixture, test_db_guard};
        use crate::test_s3::{serve, Response};
        let _guard = test_db_guard().await;
        for (our_marker, changed) in [(true, false), (true, true), (false, false)] {
            let (session, mut journal) = journal_fixture("legacy-multipart", 8).await;
            journal.stage = "transferring".into();
            save_move_journal(&journal).await.unwrap();
            let marker = if our_marker {
                session.id.clone()
            } else {
                "foreign-task".into()
            };
            let fixture = serve(move |request| {
                let marker = marker.clone();
                async move {
                    if request.method == "HEAD" {
                        return Response::empty(200)
                            .header("etag", "\"destination\"")
                            .header("content-length", 8)
                            .header("x-amz-meta-r2-move-task", marker);
                    }
                    let source = request.path.split('?').next().unwrap().ends_with("/source");
                    Response::xml(
                        200,
                        if changed && !source {
                            "modified"
                        } else {
                            "original"
                        },
                    )
                    .header(
                        "etag",
                        if source {
                            "\"source\""
                        } else {
                            "\"destination\""
                        },
                    )
                }
            })
            .await;
            let config = fixture_config(&fixture.endpoint);
            let (identity, head) =
                crate::move_transfer::planner::head_identity(&config, &session.dest_key)
                    .await
                    .unwrap()
                    .unwrap();
            let result = reconcile_legacy_multipart(
                &config,
                &config,
                &session,
                &mut journal,
                identity,
                &head,
                &AtomicBool::new(false),
                &AtomicBool::new(false),
            )
            .await;
            if !our_marker {
                assert!(!result.unwrap());
                assert_eq!(journal.stage, "transferring");
                assert_eq!(fixture.requests.lock().unwrap().len(), 1);
            } else if changed {
                assert!(result.unwrap_err().starts_with("conflict:"));
                assert_eq!(
                    get_move_journal(&session.id).await.unwrap().unwrap().stage,
                    "transferring"
                );
            } else {
                assert!(result.unwrap());
                assert_eq!(
                    get_move_journal(&session.id).await.unwrap().unwrap().stage,
                    "copied"
                );
            }
            assert!(fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| matches!(request.method.as_str(), "GET" | "HEAD")));
        }
    }

    #[test]
    fn a_failed_copy_capability_probe_relays_the_move_instead_of_failing_it() {
        // Polling a whole unoptimised relay Move, SDK calls included, takes a
        // little more than a test thread's 2 MiB of stack (the relay code at
        // e0a6051 needs the same), so it runs on a thread with the 8 MiB a
        // main thread gets.
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(copy_probe_denied_move_relays())
            })
            .unwrap()
            .join()
            .unwrap();
    }

    async fn copy_probe_denied_move_relays() {
        use super::*;
        use crate::move_transfer::stream::tests::{fixture_config, journal_fixture, test_db_guard};
        use crate::test_s3::{serve, Response};
        let _guard = test_db_guard().await;
        // journal_fixture also opens the shared test database; the Move under
        // test is a fresh one without a journal.
        let (template, _) = journal_fixture("copy-probe-denied", 8).await;
        let session = MoveSession {
            id: format!("{}-fresh", template.id),
            progress: 0,
            status: "pending".into(),
            ..template
        };
        db::move_sessions::create_move_session(&session)
            .await
            .unwrap();
        let written = Arc::new(AtomicBool::new(false));
        let fixture = serve({
            let written = written.clone();
            let marker = session.id.clone();
            move |request| {
                let written = written.clone();
                let marker = marker.clone();
                async move {
                    let path = request.path.split('?').next().unwrap().to_string();
                    if path.contains("/.r2-operation-checks/") {
                        // The deployment denies CopyObject on the probe prefix;
                        // every other probe step behaves like S3.
                        if request.headers.contains_key("x-amz-copy-source") {
                            return Response::xml(
                                403,
                                "<Error><Code>AccessDenied</Code><Message>Copy denied</Message></Error>",
                            );
                        }
                        return match request.method.as_str() {
                            "PUT" if request.headers.contains_key("if-none-match") => {
                                Response::xml(
                                    412,
                                    "<Error><Code>PreconditionFailed</Code><Message>exists</Message></Error>",
                                )
                            }
                            "PUT" => Response::empty(200).header("etag", "\"probe\""),
                            "HEAD" => Response::empty(200)
                                .header("etag", "\"probe\"")
                                .header("content-length", 8),
                            _ => Response::empty(204),
                        };
                    }
                    if path.ends_with("/source") {
                        return Response::xml(200, "original").header("etag", "\"source\"");
                    }
                    if request.method == "PUT" {
                        written.store(true, Ordering::SeqCst);
                        return Response::empty(200).header("etag", "\"destination\"");
                    }
                    if written.load(Ordering::SeqCst) {
                        Response::empty(200)
                            .header("etag", "\"destination\"")
                            .header("content-length", 8)
                            .header("x-amz-meta-r2-move-task", marker)
                    } else {
                        Response::empty(404)
                    }
                }
            }
        })
        .await;
        let config = fixture_config(&fixture.endpoint);
        let moved = move_file_internal(
            &shared_http_client().unwrap(),
            &session,
            &config,
            &config,
            None,
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap()
        .expect("the move copies its source");
        assert_eq!(moved.uploaded_size, 8);
        let journal = get_move_journal(&session.id).await.unwrap().unwrap();
        assert_eq!(journal.stage, "copied");
        assert_eq!(journal.destination.unwrap().etag, "\"destination\"");
        let requests = fixture.requests.lock().unwrap();
        let relayed: Vec<_> = requests
            .iter()
            .filter(|request| request.method == "PUT" && request.path.contains("/destination"))
            .collect();
        assert_eq!(relayed.len(), 1);
        assert_eq!(relayed[0].body, b"original");
        assert_eq!(
            relayed[0].headers.get("if-none-match").map(String::as_str),
            Some("*")
        );
        assert!(!requests.iter().any(|request| {
            request.headers.contains_key("x-amz-copy-source")
                && !request.path.contains("/.r2-operation-checks/")
        }));
    }

    #[tokio::test]
    async fn large_compatible_moves_relay_even_where_part_copy_conditions_hold() {
        use super::*;
        use crate::move_transfer::planner::SINGLE_COPY_LIMIT;
        use crate::move_transfer::stream::tests::fixture_config;
        use crate::test_s3::{serve, Response};
        // An endpoint that enforces every condition the capability probes try.
        let fixture = serve(|request| async move {
            let refused = || {
                Response::xml(
                    412,
                    "<Error><Code>PreconditionFailed</Code><Message>condition</Message></Error>",
                )
            };
            let conditional = request.headers.contains_key("x-amz-copy-source")
                || request.headers.contains_key("if-none-match");
            match request.method.as_str() {
                "PUT" if conditional => refused(),
                "PUT" if request.path.contains("uploadId=") => {
                    Response::empty(200).header("etag", "\"part\"")
                }
                "PUT" => Response::empty(200).header("etag", "\"probe\""),
                "POST" if request.path.contains("uploadId=") => refused(),
                "POST" => Response::xml(
                    200,
                    "<InitiateMultipartUploadResult><UploadId>probe-upload</UploadId></InitiateMultipartUploadResult>",
                ),
                "GET" => Response::xml(
                    200,
                    "<ListPartsResult><IsTruncated>false</IsTruncated></ListPartsResult>",
                ),
                "HEAD" => Response::empty(200)
                    .header("etag", "\"probe\"")
                    .header("content-length", 8),
                _ => Response::empty(204),
            }
        })
        .await;
        let config = fixture_config(&fixture.endpoint);
        let session = MoveSession {
            id: "compatible-large-move".into(),
            source_key: "source".into(),
            dest_key: "destination".into(),
            source_bucket: "bucket".into(),
            source_account_id: "account".into(),
            source_provider: "minio".into(),
            dest_bucket: "bucket".into(),
            dest_account_id: "account".into(),
            dest_provider: "minio".into(),
            delete_original: true,
            file_size: 0,
            progress: 0,
            status: "pending".into(),
            error: None,
            created_at: 0,
            updated_at: 0,
        };
        // Compatible multipart copy is not validated on any deployment yet
        // (review NEXT-02), so a large object relays as in v0.3.5, without
        // probing at all.
        assert_eq!(
            execution_plan(&session, &config, &config, SINGLE_COPY_LIMIT + 1)
                .await
                .unwrap(),
            TransferPlan::Relay
        );
        assert!(fixture.requests.lock().unwrap().is_empty());
        // A confirmed single conditional copy is still used.
        assert_eq!(
            execution_plan(&session, &config, &config, SINGLE_COPY_LIMIT)
                .await
                .unwrap(),
            TransferPlan::SingleCopy
        );
        // Native AWS keeps its conditional multipart copy.
        let aws = |bucket: &str| {
            MoveConfig::Aws(crate::providers::aws::AwsConfig {
                bucket: bucket.into(),
                access_key_id: "a".into(),
                secret_access_key: "s".into(),
                region: "us-east-1".into(),
                endpoint_scheme: None,
                endpoint_host: None,
                force_path_style: false,
            })
        };
        assert_eq!(
            execution_plan(&session, &aws("a"), &aws("b"), SINGLE_COPY_LIMIT + 1)
                .await
                .unwrap(),
            TransferPlan::MultipartCopy
        );
    }
}
