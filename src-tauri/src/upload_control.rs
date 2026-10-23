//! Process-wide controls shared by every file-upload command (R2's presigned
//! path and the AWS/MinIO/RustFS SDK path), added for issue #18:
//!
//! - a cap on how many files transfer at once, so a 100-file folder can no
//!   longer open 100 uploads (and up to 600 part requests) at the same time;
//! - the cancel registry `cancel_upload` flips, honoured while a file waits for
//!   a slot and between attempts;
//! - the retry policy: up to [`MAX_UPLOAD_ATTEMPTS`] attempts with exponential
//!   backoff and jitter, for transient failures only (network errors,
//!   timeouts, 408/429/5xx) — never for 403/404 and other permanent refusals.
//!
//! Replaying an upload attempt is safe: every attempt sends the same bytes of
//! the same local file to the same key (or the same part number of the same
//! multipart upload), so a request whose outcome was unknown is simply
//! overwritten with identical content. Multipart *completion* is not replayed.

use crate::providers::s3_client::{
    classify_storage_error, describe_s3_error, s3_error_class, StorageErrorClass,
};
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Most files that transfer at once, across every provider. The upload queue
/// in the frontend starts 3–5 files at a time; this is the backstop for any
/// caller that does not go through that queue.
pub(crate) const MAX_PARALLEL_FILE_UPLOADS: usize = 5;
/// Attempts per request, counting the first.
pub(crate) const MAX_UPLOAD_ATTEMPTS: u32 = 3;
/// The jitter cap doubles from here per consecutive failure (1 s, 2 s, 4 s …).
const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// Upper bound of the jitter cap.
const BACKOFF_MAX: Duration = Duration::from_secs(8);
/// Longest provider-requested Retry-After honoured inside one upload.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
/// How often a wait (for a slot, or between attempts) checks for a cancel.
const CANCEL_POLL: Duration = Duration::from_millis(100);
/// Fixed allowance of a single attempt on top of its transfer time.
const ATTEMPT_TIMEOUT_BASE: Duration = Duration::from_secs(120);
/// Slowest sustained throughput an attempt is allowed before it is treated as
/// stalled: 32 KiB/s. A 20 MiB part may take ~12 min, a 100 MiB PUT ~55 min.
const MIN_THROUGHPUT_BYTES_PER_SEC: u64 = 32 * 1024;

/// The error text every upload path returns for a cancelled upload; the
/// frontend recognises a cancel by the word "cancelled".
pub(crate) const CANCELLED_MESSAGE: &str = "Upload cancelled";
/// Emitted before an upload waits to retry a failed attempt.
pub(crate) const UPLOAD_RETRY_EVENT: &str = "upload-retry";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UploadRetryEvent {
    pub task_id: String,
    /// The attempt about to be made (2 or 3).
    pub attempt: u32,
    pub max_attempts: u32,
    pub delay_ms: u64,
    pub error: String,
}

fn upload_slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_PARALLEL_FILE_UPLOADS)))
        .clone()
}

/// Wait for one of the [`MAX_PARALLEL_FILE_UPLOADS`] file slots. Held for the
/// whole upload; a cancel while waiting returns [`CANCELLED_MESSAGE`].
pub(crate) async fn acquire_upload_slot(
    cancelled: &AtomicBool,
) -> Result<OwnedSemaphorePermit, String> {
    let acquire = upload_slots().acquire_owned();
    tokio::pin!(acquire);
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err(CANCELLED_MESSAGE.to_string());
        }
        tokio::select! {
            permit = &mut acquire => {
                return permit.map_err(|_| "Upload slots closed".to_string());
            }
            _ = tokio::time::sleep(CANCEL_POLL) => {}
        }
    }
}

type CancelRegistry = Mutex<HashMap<String, Arc<AtomicBool>>>;

fn cancel_registry() -> &'static CancelRegistry {
    static REGISTRY: OnceLock<CancelRegistry> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// A running upload's cancel flag, registered under its task id until dropped.
pub(crate) struct CancelRegistration {
    task_id: String,
    pub cancelled: Arc<AtomicBool>,
}

impl Drop for CancelRegistration {
    fn drop(&mut self) {
        let mut registry = cancel_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A later upload may have reused the id; only remove our own flag.
        if registry
            .get(&self.task_id)
            .is_some_and(|flag| Arc::ptr_eq(flag, &self.cancelled))
        {
            registry.remove(&self.task_id);
        }
    }
}

pub(crate) fn register_upload(task_id: &str) -> CancelRegistration {
    let cancelled = Arc::new(AtomicBool::new(false));
    cancel_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(task_id.to_string(), cancelled.clone());
    CancelRegistration {
        task_id: task_id.to_string(),
        cancelled,
    }
}

/// Flag a registered upload as cancelled. Returns whether one was running.
pub(crate) fn cancel_registered_upload(task_id: &str) -> bool {
    let registry = cancel_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match registry.get(task_id) {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

/// Timeout of one attempt that sends `payload_bytes`. There is deliberately no
/// timeout on the upload as a whole: this only bounds a single request, scaled
/// to its size, so a stalled connection fails (and is retried) while a large
/// upload on a slow or shared link still has all the time it needs.
///
/// A client-wide read timeout cannot do this job: both reqwest and the AWS
/// SDK's HTTP client start it when the request is sent and stop it only at the
/// response headers, so it bounds the whole body upload.
pub(crate) fn attempt_timeout(payload_bytes: u64) -> Duration {
    ATTEMPT_TIMEOUT_BASE + Duration::from_secs(payload_bytes / MIN_THROUGHPUT_BYTES_PER_SEC)
}

/// One failed attempt: whether another attempt may help, and what to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttemptFailure {
    pub retryable: bool,
    pub message: String,
    pub retry_after: Option<Duration>,
}

impl AttemptFailure {
    #[cfg(test)]
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            retryable: true,
            message: message.into(),
            retry_after: None,
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            retryable: false,
            message: message.into(),
            retry_after: None,
        }
    }

    /// A non-success HTTP response: 408, 429 and 5xx (but 501) are transient.
    pub fn from_status(status: u16, message: impl Into<String>, retry_after: Option<&str>) -> Self {
        Self {
            retryable: is_transient_status(status),
            message: message.into(),
            retry_after: retry_after.and_then(parse_retry_after),
        }
    }

    /// A request that produced no response. Building the request (a bad URL or
    /// header) is permanent; connecting, sending, timing out and reading the
    /// response are network failures worth another attempt.
    pub fn from_reqwest(error: &reqwest::Error, message: impl Into<String>) -> Self {
        let retryable = match error.status() {
            Some(status) => is_transient_status(status.as_u16()),
            None => !error.is_builder() && !error.is_redirect(),
        };
        Self {
            retryable,
            message: message.into(),
            retry_after: None,
        }
    }
}

/// An AWS SDK upload request that failed, classified like a read (replaying
/// the same bytes is idempotent): throttling, 5xx, timeouts and dropped
/// connections are transient; auth, not-found and other refusals are not.
pub(crate) fn sdk_attempt_failure<E>(error: &SdkError<E, HttpResponse>) -> AttemptFailure
where
    E: std::error::Error + ProvideErrorMetadata + 'static,
{
    AttemptFailure {
        retryable: s3_error_class(error, false) == StorageErrorClass::Transient,
        message: describe_s3_error(error),
        retry_after: error
            .raw_response()
            .and_then(|r| r.headers().get("retry-after"))
            .and_then(parse_retry_after),
    }
}

/// Per-request timeouts for an SDK upload request carrying `payload_bytes`,
/// applied with `.customize().config_override(...)`. The shared S3 client's
/// 30 s read timeout and 120 s operation timeouts are meant for listings: the
/// SDK's HTTP client runs its read timeout across the whole request body, so
/// they would kill any part or PUT that needs longer than that — which several
/// concurrent uploads on one link routinely do. Keep the connect timeout and
/// bound the attempt by its size instead (see [`attempt_timeout`]).
pub(crate) fn sdk_upload_timeouts(payload_bytes: u64) -> aws_sdk_s3::config::Builder {
    let attempt = attempt_timeout(payload_bytes);
    aws_sdk_s3::config::Builder::new().timeout_config(
        TimeoutConfig::builder()
            .connect_timeout(Duration::from_secs(10))
            .disable_read_timeout()
            .operation_attempt_timeout(attempt)
            .operation_timeout(attempt)
            .build(),
    )
}

/// Read `len` bytes of the file at `offset`. Upload attempts read their bytes
/// afresh each time rather than keeping a copy around for a retry.
pub(crate) async fn read_file_range(
    file_path: &Path,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>, String> {
    let mut file = tokio::fs::File::open(file_path)
        .await
        .map_err(|e| format!("Failed to open file: {}", e))?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(|e| format!("Failed to seek: {}", e))?;
    }
    let len = usize::try_from(len).map_err(|_| "File part too large".to_string())?;
    let mut buffer = vec![0u8; len];
    file.read_exact(&mut buffer)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;
    Ok(buffer)
}

/// Whether an HTTP status means "not now" rather than "no".
pub(crate) fn is_transient_status(status: u16) -> bool {
    // Replaying identical bytes is idempotent, so an upload attempt is
    // classified like a read: 408/429/5xx retry, 401/403/404/409/412 do not.
    classify_storage_error(None, Some(status), false) == StorageErrorClass::Transient
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Exponential backoff with "equal jitter": after the `failed_attempt`-th
/// failure in a row (1-based) the cap is `BACKOFF_BASE * 2^(n-1)` up to
/// `BACKOFF_MAX`, and the wait is uniform in `[cap/2, cap]` (`jitter` in
/// `[0, 1]`), so retries of many files spread out but never fire immediately.
/// A provider's Retry-After wins when longer, up to `MAX_RETRY_AFTER`.
pub(crate) fn retry_delay(
    failed_attempt: u32,
    jitter: f64,
    retry_after: Option<Duration>,
) -> Duration {
    let exponent = failed_attempt.saturating_sub(1).min(16);
    let cap = BACKOFF_BASE.saturating_mul(1 << exponent).min(BACKOFF_MAX);
    let half = cap / 2;
    let backoff = half + half.mul_f64(jitter.clamp(0.0, 1.0));
    backoff.max(retry_after.unwrap_or_default().min(MAX_RETRY_AFTER))
}

fn jitter() -> f64 {
    f64::from(chrono::Utc::now().timestamp_subsec_nanos()) / 1_000_000_000.0
}

/// Sleep for `wait`, returning [`CANCELLED_MESSAGE`] as soon as `cancelled` is set.
pub(crate) async fn sleep_unless_cancelled(
    wait: Duration,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err(CANCELLED_MESSAGE.to_string());
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(());
        }
        tokio::time::sleep((deadline - now).min(CANCEL_POLL)).await;
    }
}

/// Run `attempt` (given its 1-based number) until it succeeds, fails
/// permanently, or [`MAX_UPLOAD_ATTEMPTS`] attempts have failed. Before each
/// retry `on_retry(next_attempt, delay, failure)` is called and the backoff is
/// slept; a cancel stops it at once. The attempt closure must rebuild its
/// request from scratch (re-presign, reopen the file) every time.
pub(crate) async fn retry_transient<T, F, Fut>(
    cancelled: &AtomicBool,
    mut on_retry: impl FnMut(u32, Duration, &AttemptFailure),
    mut attempt: F,
) -> Result<T, String>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, AttemptFailure>>,
{
    let mut number = 1;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err(CANCELLED_MESSAGE.to_string());
        }
        let failure = match attempt(number).await {
            Ok(value) => return Ok(value),
            Err(failure) => failure,
        };
        if cancelled.load(Ordering::SeqCst) {
            return Err(CANCELLED_MESSAGE.to_string());
        }
        if !failure.retryable || number >= MAX_UPLOAD_ATTEMPTS {
            return Err(if number > 1 {
                format!("{} (after {} attempts)", failure.message, number)
            } else {
                failure.message
            });
        }
        let delay = retry_delay(number, jitter(), failure.retry_after);
        log::warn!(
            "Upload attempt {}/{} failed, retrying in {} ms: {}",
            number,
            MAX_UPLOAD_ATTEMPTS,
            delay.as_millis(),
            failure.message
        );
        on_retry(number + 1, delay, &failure);
        sleep_unless_cancelled(delay, cancelled).await?;
        number += 1;
    }
}

/// Called before a retry with the attempt about to be made, the backoff, and
/// the failure being retried.
pub(crate) type RetryReporter = Arc<dyn Fn(u32, Duration, &AttemptFailure) + Send + Sync>;

/// What a provider upload needs from the command that runs it: the cancel
/// flag, and where to report a retry.
#[derive(Clone)]
pub(crate) struct UploadHooks {
    pub cancelled: Arc<AtomicBool>,
    pub on_retry: RetryReporter,
}

impl UploadHooks {
    /// Hooks that emit [`UPLOAD_RETRY_EVENT`] for `task_id` through `app`.
    pub fn for_task(app: &tauri::AppHandle, task_id: &str, cancelled: Arc<AtomicBool>) -> Self {
        let app = app.clone();
        let task_id = task_id.to_string();
        Self {
            cancelled,
            on_retry: Arc::new(move |attempt, delay, failure| {
                emit_retry(&app, &task_id, attempt, delay, failure);
            }),
        }
    }

    /// Run `attempt` under [`retry_transient`] with these hooks.
    pub async fn retry<T, F, Fut>(&self, attempt: F) -> Result<T, String>
    where
        F: FnMut(u32) -> Fut,
        Fut: Future<Output = Result<T, AttemptFailure>>,
    {
        let on_retry = self.on_retry.clone();
        retry_transient(
            &self.cancelled,
            move |next, delay, failure| on_retry(next, delay, failure),
            attempt,
        )
        .await
    }
}

pub(crate) fn emit_retry(
    app: &tauri::AppHandle,
    task_id: &str,
    attempt: u32,
    delay: Duration,
    failure: &AttemptFailure,
) {
    use tauri::Emitter;
    let _ = app.emit(
        UPLOAD_RETRY_EVENT,
        UploadRetryEvent {
            task_id: task_id.to_string(),
            attempt,
            max_attempts: MAX_UPLOAD_ATTEMPTS,
            delay_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            error: failure.message.clone(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn transient_statuses_retry_and_refusals_do_not() {
        for status in [408, 429, 500, 502, 503, 504, 520, 522] {
            assert!(is_transient_status(status), "{status} should retry");
        }
        for status in [400, 401, 403, 404, 409, 412, 501] {
            assert!(!is_transient_status(status), "{status} should not retry");
        }
    }

    #[test]
    fn status_failure_reads_retry_after_seconds() {
        let failure = AttemptFailure::from_status(429, "Upload failed: 429", Some("7"));
        assert!(failure.retryable);
        assert_eq!(failure.retry_after, Some(Duration::from_secs(7)));
        let failure = AttemptFailure::from_status(403, "Upload failed: 403", Some("junk"));
        assert!(!failure.retryable);
        assert_eq!(failure.retry_after, None);
    }

    #[test]
    fn backoff_grows_exponentially_within_jitter_bounds() {
        for (failed, cap) in [(1, 1_000), (2, 2_000), (3, 4_000), (4, 8_000), (9, 8_000)] {
            let low = retry_delay(failed, 0.0, None);
            let high = retry_delay(failed, 1.0, None);
            assert_eq!(low, Duration::from_millis(cap / 2), "attempt {failed}");
            assert_eq!(high, Duration::from_millis(cap), "attempt {failed}");
            let mid = retry_delay(failed, 0.5, None);
            assert!(low <= mid && mid <= high);
        }
        // Out-of-range jitter is clamped rather than producing a longer wait.
        assert_eq!(retry_delay(1, 7.0, None), Duration::from_secs(1));
    }

    #[test]
    fn retry_after_wins_when_longer_but_is_capped() {
        assert_eq!(
            retry_delay(1, 0.0, Some(Duration::from_secs(5))),
            Duration::from_secs(5)
        );
        assert_eq!(
            retry_delay(1, 0.0, Some(Duration::from_secs(600))),
            MAX_RETRY_AFTER
        );
        assert_eq!(
            retry_delay(3, 1.0, Some(Duration::from_millis(10))),
            Duration::from_secs(4)
        );
    }

    #[test]
    fn attempt_timeout_scales_with_payload() {
        assert_eq!(attempt_timeout(0), ATTEMPT_TIMEOUT_BASE);
        let part = attempt_timeout(20 * 1024 * 1024);
        assert_eq!(part, ATTEMPT_TIMEOUT_BASE + Duration::from_secs(640));
        assert!(attempt_timeout(100 * 1024 * 1024) > part);
    }

    #[tokio::test(start_paused = true)]
    async fn transient_failures_are_retried_up_to_the_limit() {
        let cancelled = AtomicBool::new(false);
        let calls = AtomicU32::new(0);
        let mut retries = Vec::new();
        let result: Result<(), String> = retry_transient(
            &cancelled,
            |next, _, _| retries.push(next),
            |n| {
                calls.fetch_add(1, Ordering::SeqCst);
                async move { Err(AttemptFailure::transient(format!("boom {n}"))) }
            },
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), MAX_UPLOAD_ATTEMPTS);
        assert_eq!(retries, vec![2, 3]);
        assert_eq!(result.unwrap_err(), "boom 3 (after 3 attempts)");
    }

    #[tokio::test(start_paused = true)]
    async fn a_transient_failure_then_success_succeeds() {
        let cancelled = AtomicBool::new(false);
        let result = retry_transient(
            &cancelled,
            |_, _, _| {},
            |n| async move {
                if n < 2 {
                    Err(AttemptFailure::transient("reset"))
                } else {
                    Ok(n)
                }
            },
        )
        .await;
        assert_eq!(result, Ok(2));
    }

    #[tokio::test(start_paused = true)]
    async fn permanent_failures_are_not_retried() {
        let cancelled = AtomicBool::new(false);
        let calls = AtomicU32::new(0);
        let result: Result<(), String> = retry_transient(
            &cancelled,
            |_, _, _| panic!("a permanent failure must not be retried"),
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(AttemptFailure::from_status(403, "AccessDenied", None)) }
            },
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.unwrap_err(), "AccessDenied");
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_backoff_stops_retrying() {
        let cancelled = AtomicBool::new(false);
        let calls = AtomicU32::new(0);
        let result: Result<(), String> = retry_transient(
            &cancelled,
            |_, _, _| cancelled.store(true, Ordering::SeqCst),
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(AttemptFailure::transient("timeout")) }
            },
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.unwrap_err(), CANCELLED_MESSAGE);
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_for_a_slot_honours_cancel() {
        let held: Vec<_> = futures_util::future::join_all(
            (0..MAX_PARALLEL_FILE_UPLOADS).map(|_| upload_slots().acquire_owned()),
        )
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let waiter = tokio::spawn(async move { acquire_upload_slot(&flag).await.map(|_| ()) });
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!waiter.is_finished(), "all slots are held");
        cancelled.store(true, Ordering::SeqCst);
        assert_eq!(waiter.await.unwrap(), Err(CANCELLED_MESSAGE.to_string()));
        drop(held);
        let free = AtomicBool::new(false);
        assert!(acquire_upload_slot(&free).await.is_ok());
    }

    #[test]
    fn cancel_registry_flags_and_releases_only_its_own_entry() {
        let first = register_upload("task-registry-test");
        assert!(cancel_registered_upload("task-registry-test"));
        assert!(first.cancelled.load(Ordering::SeqCst));
        let second = register_upload("task-registry-test");
        drop(first);
        assert!(cancel_registered_upload("task-registry-test"));
        assert!(second.cancelled.load(Ordering::SeqCst));
        drop(second);
        assert!(!cancel_registered_upload("task-registry-test"));
    }
}
