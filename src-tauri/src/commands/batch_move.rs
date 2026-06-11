//! Shared batch move/rename executor used by all provider command modules.
//!
//! Runs rename (copy + delete) operations with adaptive parallelism and emits
//! throttled, enriched `batch-move-progress` events consumed by the transfer
//! panel and the folder rename modal.

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::Emitter;
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Deserialize)]
pub struct MoveOperation {
    pub old_key: String,
    pub new_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchMoveResult {
    pub moved: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchMoveProgress {
    pub batch_id: String,
    /// Processed operations (succeeded + failed), kept for backwards compatibility.
    pub completed: usize,
    pub total: usize,
    pub failed: usize,
    /// Most recently processed object key.
    pub current_key: String,
    pub ops_per_sec: f64,
    pub eta_ms: u64,
    pub done: bool,
}

/// Minimum interval between progress event emissions per batch.
const EMIT_INTERVAL_MS: u64 = 100;

/// Scale rename parallelism with batch size: small batches stay gentle,
/// large folder renames fan out wider (server-side copies are cheap).
pub(crate) fn batch_concurrency(total: usize) -> usize {
    (total / 25).clamp(6, 12)
}

/// Fallback batch id when the frontend does not provide one.
pub(crate) fn fallback_batch_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("batch-{}", millis)
}

pub(crate) struct BatchMoveOutcome {
    pub moved: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    /// (old_key, new_key) pairs that succeeded, for cache updates.
    pub successful: Vec<(String, String)>,
}

struct BatchProgressTracker {
    batch_id: String,
    total: usize,
    started: Instant,
    last_emit_ms: AtomicU64,
}

impl BatchProgressTracker {
    fn new(batch_id: String, total: usize) -> Self {
        Self {
            batch_id,
            total,
            started: Instant::now(),
            last_emit_ms: AtomicU64::new(0),
        }
    }

    fn payload(
        &self,
        processed: usize,
        failed: usize,
        current_key: &str,
        done: bool,
    ) -> BatchMoveProgress {
        let elapsed = self.started.elapsed().as_secs_f64();
        let ops_per_sec = if elapsed > 0.0 {
            processed as f64 / elapsed
        } else {
            0.0
        };
        let remaining = self.total.saturating_sub(processed);
        let eta_ms = if ops_per_sec > 0.0 {
            ((remaining as f64 / ops_per_sec) * 1000.0) as u64
        } else {
            0
        };
        BatchMoveProgress {
            batch_id: self.batch_id.clone(),
            completed: processed,
            total: self.total,
            failed,
            current_key: current_key.to_string(),
            ops_per_sec,
            eta_ms,
            done,
        }
    }

    /// Emit at most once per EMIT_INTERVAL_MS. Final (`done`) events bypass the throttle.
    fn emit(&self, app: &tauri::AppHandle, processed: usize, failed: usize, current_key: &str) {
        let now_ms = self.started.elapsed().as_millis() as u64;
        let last = self.last_emit_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) < EMIT_INTERVAL_MS {
            return;
        }
        if self
            .last_emit_ms
            .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let _ = app.emit(
            "batch-move-progress",
            self.payload(processed, failed, current_key, false),
        );
    }

    fn emit_final(&self, app: &tauri::AppHandle, processed: usize, failed: usize) {
        let _ = app.emit(
            "batch-move-progress",
            self.payload(processed, failed, "", true),
        );
    }
}

/// Execute a batch of rename operations through `rename_one` with bounded
/// parallelism, emitting throttled progress events along the way.
pub(crate) async fn run_batch_move<F, Fut>(
    app: &tauri::AppHandle,
    batch_id: String,
    operations: Vec<MoveOperation>,
    rename_one: F,
) -> BatchMoveOutcome
where
    F: Fn(MoveOperation) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send,
{
    let total = operations.len();
    if total == 0 {
        return BatchMoveOutcome {
            moved: 0,
            failed: 0,
            errors: vec![],
            successful: vec![],
        };
    }

    let semaphore = Arc::new(Semaphore::new(batch_concurrency(total)));
    let succeeded = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    let successful = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
    let tracker = Arc::new(BatchProgressTracker::new(batch_id, total));

    let mut handles = Vec::with_capacity(total);

    for op in operations {
        let permit = match semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break, // semaphore closed — cannot happen in practice
        };
        let succeeded = succeeded.clone();
        let failed = failed.clone();
        let errors = errors.clone();
        let successful = successful.clone();
        let tracker = tracker.clone();
        let rename_one = rename_one.clone();
        let app = app.clone();

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let key = op.old_key.clone();

            if op.old_key == op.new_key {
                succeeded.fetch_add(1, Ordering::SeqCst);
            } else {
                match rename_one(op.clone()).await {
                    Ok(()) => {
                        succeeded.fetch_add(1, Ordering::SeqCst);
                        successful.lock().await.push((op.old_key, op.new_key));
                    }
                    Err(e) => {
                        failed.fetch_add(1, Ordering::SeqCst);
                        errors.lock().await.push(format!("{}: {}", key, e));
                    }
                }
            }

            let processed = succeeded.load(Ordering::SeqCst) + failed.load(Ordering::SeqCst);
            if processed < tracker.total {
                tracker.emit(&app, processed, failed.load(Ordering::SeqCst), &key);
            }
        }));
    }

    for handle in handles {
        let _ = handle.await;
    }

    let moved = succeeded.load(Ordering::SeqCst);
    let failed_count = failed.load(Ordering::SeqCst);
    tracker.emit_final(app, moved + failed_count, failed_count);

    let successful = {
        let guard = successful.lock().await;
        guard.clone()
    };
    let errors = {
        let guard = errors.lock().await;
        guard.clone()
    };

    BatchMoveOutcome {
        moved,
        failed: failed_count,
        errors,
        successful,
    }
}
