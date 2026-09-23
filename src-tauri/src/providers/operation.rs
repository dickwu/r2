//! One retry owner for identity-bound reads and replayable multipart parts.
//! Publications (PUT/Copy/Complete/Delete) deliberately have no operation kind:
//! their uncertain results require reconciliation against a durable receipt.

use super::s3_client::{describe_s3_error, s3_error_class, StorageErrorClass};
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

pub(crate) const DATA_LIMIT: usize = 8;
const CONTROL_LIMIT: usize = 4;
const CONTROL_POLL: Duration = Duration::from_millis(10);
/// Longest wait between attempts spent inside an operation (as in v0.3.5).
/// A longer Retry-After is handed to the durable task scheduler instead of
/// holding the worker and whatever it has reserved.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
pub enum OperationKind {
    List,
    Head,
    Get,
    ListParts,
    UploadPart,
    UploadPartCopy,
}

impl OperationKind {
    fn is_data(self) -> bool {
        matches!(self, Self::Get | Self::UploadPart | Self::UploadPartCopy)
    }
}

/// Most attempts any caller may ask for, counting the first.
const MAX_ATTEMPTS: u32 = 6;

/// Full jitter: the wait after a failure is uniform in `[0, cap]`, where the
/// cap doubles from `initial` per consecutive failure up to `max`. A longer
/// Retry-After from the provider still wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Backoff {
    /// 250 ms, 500 ms, 1 s, 2 s, then 4 s.
    pub const DEFAULT: Self = Self {
        initial: Duration::from_millis(250),
        max: Duration::from_secs(4),
    };

    /// The jitter cap after the `failed_attempt`-th (0-based) failure in a row.
    pub fn cap(self, failed_attempt: u32) -> Duration {
        self.initial
            .saturating_mul(1 << failed_attempt.min(MAX_ATTEMPTS))
            .min(self.max)
    }
}

/// Scope and identity are caller-owned immutable labels, never credentials or
/// presigned URLs. The executor does not log either; the request factory must
/// enforce their corresponding version/ETag/range/precondition on every call.
pub struct OperationContext<'a> {
    pub kind: OperationKind,
    pub endpoint: &'a str,
    pub scope: &'a str,
    pub identity: &'a str,
    pub deadline: Instant,
    pub cancelled: &'a AtomicBool,
    pub paused: Option<&'a AtomicBool>,
    pub peer_endpoint: Option<&'a str>,
    pub max_attempts: u32,
    pub backoff: Backoff,
}

impl<'a> OperationContext<'a> {
    pub fn new(
        kind: OperationKind,
        endpoint: &'a str,
        scope: &'a str,
        identity: &'a str,
        deadline: Instant,
        cancelled: &'a AtomicBool,
    ) -> Self {
        Self {
            kind,
            endpoint,
            scope,
            identity,
            deadline,
            cancelled,
            paused: None,
            peer_endpoint: None,
            max_attempts: 3,
            backoff: Backoff::DEFAULT,
        }
    }

    pub fn with_pause(mut self, paused: &'a AtomicBool) -> Self {
        self.paused = Some(paused);
        self
    }

    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts.clamp(1, MAX_ATTEMPTS);
        self
    }

    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn with_peer_endpoint(mut self, endpoint: &'a str) -> Self {
        self.peer_endpoint = Some(endpoint);
        self
    }

    fn check(&self) -> Result<(), OperationError> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(OperationError::Cancelled)
        } else if self.paused.is_some_and(|p| p.load(Ordering::SeqCst)) {
            Err(OperationError::Paused)
        } else if Instant::now() >= self.deadline {
            Err(OperationError::Deadline { last: None })
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug)]
pub struct AttemptError {
    pub class: StorageErrorClass,
    pub message: String,
    pub retry_after: Duration,
}

impl AttemptError {
    pub fn new(class: StorageErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            retry_after: Duration::ZERO,
        }
    }

    pub fn transient(message: impl Into<String>) -> Self {
        Self::new(StorageErrorClass::Transient, message)
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self::new(StorageErrorClass::Permanent, message)
    }

    pub fn with_retry_after(mut self, wait: Duration) -> Self {
        self.retry_after = wait;
        self
    }

    pub fn from_sdk<E>(error: &SdkError<E, HttpResponse>) -> Self
    where
        E: std::error::Error + ProvideErrorMetadata + 'static,
    {
        let retry_after = error
            .raw_response()
            .and_then(|r| r.headers().get("retry-after"))
            .and_then(parse_retry_after)
            .unwrap_or_default();
        // Service codes are useful diagnostics without serializing request
        // internals, headers, URLs or credentials into persisted task errors.
        // Anything else (a dropped connection, a timeout, an unreadable
        // response) keeps the cause chain describe_s3_error walks, which is
        // what names the failure; connector errors carry no credentials.
        let message = match (error.code(), error) {
            (Some(code), _) => code.to_owned(),
            (None, SdkError::ServiceError(_)) => "Storage request failed".to_owned(),
            (None, _) => describe_s3_error(error),
        };
        Self::new(s3_error_class(error, false), message).with_retry_after(retry_after)
    }
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            chrono::DateTime::parse_from_rfc2822(value).ok().map(|at| {
                Duration::from_secs((at.timestamp() - chrono::Utc::now().timestamp()).max(0) as u64)
            })
        })
}

#[derive(Debug)]
pub enum OperationError {
    Cancelled,
    Paused,
    Deadline { last: Option<AttemptError> },
    Failed { error: AttemptError, attempts: u32 },
}

impl OperationError {
    pub fn class(&self) -> StorageErrorClass {
        match self {
            Self::Failed { error, .. } => error.class,
            Self::Deadline { .. } => StorageErrorClass::Transient,
            Self::Cancelled | Self::Paused => StorageErrorClass::Permanent,
        }
    }
}

impl fmt::Display for OperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "cancelled: Storage operation cancelled"),
            Self::Paused => write!(f, "paused: Storage operation paused"),
            Self::Deadline { last } => {
                write!(f, "transient: Storage operation deadline exhausted")?;
                if let Some(error) = last {
                    write!(f, "; last attempt: {}", error.message)?;
                }
                Ok(())
            }
            Self::Failed { error, attempts } => write!(
                f,
                "{}: {} (after {attempts} attempts)",
                error.class.label(),
                error.message
            ),
        }
    }
}

impl std::error::Error for OperationError {}

struct EndpointBudget {
    data: Arc<Semaphore>,
    control: Arc<Semaphore>,
}

fn endpoint_budget(endpoint: &str) -> Arc<EndpointBudget> {
    static BUDGETS: OnceLock<Mutex<HashMap<String, Weak<EndpointBudget>>>> = OnceLock::new();
    let mut budgets = BUDGETS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(budget) = budgets.get(endpoint).and_then(Weak::upgrade) {
        return budget;
    }
    budgets.retain(|_, value| value.strong_count() > 0);
    let budget = Arc::new(EndpointBudget {
        data: Arc::new(Semaphore::new(DATA_LIMIT)),
        control: Arc::new(Semaphore::new(CONTROL_LIMIT)),
    });
    budgets.insert(endpoint.to_owned(), Arc::downgrade(&budget));
    budget
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct OperationMetrics {
    pub attempts: u64,
    pub retries: u64,
    pub active: u64,
    pub queued: u64,
    pub queue_us: u64,
    pub network_us: u64,
    pub backoff_us: u64,
}

static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static RETRIES: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicU64 = AtomicU64::new(0);
static QUEUED: AtomicU64 = AtomicU64::new(0);
static QUEUE_US: AtomicU64 = AtomicU64::new(0);
static NETWORK_US: AtomicU64 = AtomicU64::new(0);
static BACKOFF_US: AtomicU64 = AtomicU64::new(0);

pub fn metrics() -> OperationMetrics {
    OperationMetrics {
        attempts: ATTEMPTS.load(Ordering::Relaxed),
        retries: RETRIES.load(Ordering::Relaxed),
        active: ACTIVE.load(Ordering::Relaxed),
        queued: QUEUED.load(Ordering::Relaxed),
        queue_us: QUEUE_US.load(Ordering::Relaxed),
        network_us: NETWORK_US.load(Ordering::Relaxed),
        backoff_us: BACKOFF_US.load(Ordering::Relaxed),
    }
}

struct PhaseTimer {
    start: Instant,
    elapsed: &'static AtomicU64,
    gauge: Option<&'static AtomicU64>,
}

impl PhaseTimer {
    fn new(elapsed: &'static AtomicU64, gauge: Option<&'static AtomicU64>) -> Self {
        if let Some(gauge) = gauge {
            gauge.fetch_add(1, Ordering::Relaxed);
        }
        Self {
            start: Instant::now(),
            elapsed,
            gauge,
        }
    }
}

impl Drop for PhaseTimer {
    fn drop(&mut self) {
        self.elapsed.fetch_add(
            self.start.elapsed().as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        if let Some(gauge) = self.gauge {
            gauge.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

async fn bounded<T>(
    context: &OperationContext<'_>,
    future: impl Future<Output = T>,
) -> Result<T, OperationError> {
    tokio::pin!(future);
    loop {
        context.check()?;
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(context.deadline) => return Err(OperationError::Deadline { last: None }),
            _ = tokio::time::sleep(CONTROL_POLL) => {},
            result = &mut future => {
                context.check()?;
                return Ok(result);
            }
        }
    }
}

/// Keep the registry's budget owner alive for the entire admitted request.
/// An owned semaphore permit alone does not retain EndpointBudget, and the
/// weak registry would otherwise create a second independent limit.
pub(crate) struct OperationPermit {
    _permits: Vec<OwnedSemaphorePermit>,
    _budgets: Vec<Arc<EndpointBudget>>,
}

async fn acquire_operation_permits(
    context: &OperationContext<'_>,
    attempt: u32,
) -> Result<OperationPermit, OperationError> {
    let budget = endpoint_budget(context.endpoint);
    let mut plan = vec![(budget, 1)];
    if context.kind.is_data() {
        if let Some(peer) = context.peer_endpoint {
            if peer == context.endpoint {
                plan[0].1 = 2;
            } else {
                plan.push((endpoint_budget(peer), 1));
                if context.endpoint > peer {
                    plan.swap(0, 1);
                }
            }
        }
    }
    let mut permits = Vec::with_capacity(plan.len());
    for (owner, count) in &plan {
        let semaphore = if context.kind.is_data() {
            &owner.data
        } else {
            &owner.control
        };
        permits.push(
            bounded(context, semaphore.clone().acquire_many_owned(*count))
                .await?
                .map_err(|_| OperationError::Failed {
                    error: AttemptError::permanent("Endpoint budget closed"),
                    attempts: attempt,
                })?,
        );
    }
    Ok(OperationPermit {
        _permits: permits,
        _budgets: plan.into_iter().map(|(owner, _)| owner).collect(),
    })
}

/// The factory is invoked afresh after admission for every attempt. A GET
/// factory must validate the headers and finish reading the body before Ok.
/// A part factory must reconstruct its immutable payload or source condition.
pub async fn execute<T, F, Fut>(
    context: &OperationContext<'_>,
    mut send: F,
) -> Result<T, OperationError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, AttemptError>>,
{
    context.check()?;
    if context.endpoint.is_empty()
        || context.peer_endpoint.is_some_and(str::is_empty)
        || context.scope.is_empty()
        || (context.kind.is_data() && context.identity.is_empty())
    {
        return Err(OperationError::Failed {
            error: AttemptError::permanent(
                "Replayable operation requires an endpoint, scope and frozen data identity",
            ),
            attempts: 0,
        });
    }
    let attempts = context.max_attempts.clamp(1, MAX_ATTEMPTS);
    for attempt in 0..attempts {
        context.check()?;
        let queue = PhaseTimer::new(&QUEUE_US, Some(&QUEUED));
        let permits = acquire_operation_permits(context, attempt).await?;
        drop(queue);
        // A ready permit must not win a race with cancellation/deadline and
        // cause a new request to be constructed after the operation stopped.
        context.check()?;
        ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        if attempt > 0 {
            RETRIES.fetch_add(1, Ordering::Relaxed);
        }
        let network = PhaseTimer::new(&NETWORK_US, Some(&ACTIVE));
        let result = bounded(context, send()).await;
        drop(network);
        drop(permits);
        match result? {
            Ok(value) => return Ok(value),
            Err(error) => {
                if error.class != StorageErrorClass::Transient || attempt + 1 >= attempts {
                    return Err(OperationError::Failed {
                        error,
                        attempts: attempt + 1,
                    });
                }
                context.check()?;
                let cap_ms =
                    u64::try_from(context.backoff.cap(attempt).as_millis()).unwrap_or(u64::MAX);
                let jitter = u64::from(chrono::Utc::now().timestamp_subsec_nanos())
                    % cap_ms.saturating_add(1);
                let wait = error.retry_after.max(Duration::from_millis(jitter));
                // A wait past the deadline cannot end in another attempt, and
                // a long one must not hold the worker. Hand both back now:
                // long waits belong to the durable task scheduler.
                if wait > MAX_RETRY_WAIT
                    || wait >= context.deadline.saturating_duration_since(Instant::now())
                {
                    return Err(OperationError::Deadline { last: Some(error) });
                }
                let backoff = PhaseTimer::new(&BACKOFF_US, None);
                bounded(context, tokio::time::sleep(wait)).await?;
                drop(backoff);
                context.check()?;
            }
        }
    }
    unreachable!("each attempt returns or retries within the bound")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn context<'a>(endpoint: &'a str, cancelled: &'a AtomicBool) -> OperationContext<'a> {
        OperationContext::new(
            OperationKind::Get,
            endpoint,
            "bucket/prefix",
            "etag-1/range-0-3",
            Instant::now() + Duration::from_secs(30),
            cancelled,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn transient_read_retries_the_same_identity_and_complete_body() {
        let cancelled = AtomicBool::new(false);
        let ctx = context("retry-body", &cancelled);
        let calls = AtomicUsize::new(0);
        let value = execute(&ctx, || async {
            assert_eq!(ctx.identity, "etag-1/range-0-3");
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(AttemptError::transient("body interrupted after two bytes"));
            }
            Ok(vec![1, 2, 3, 4])
        })
        .await
        .unwrap();
        assert_eq!(value, vec![1, 2, 3, 4]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn authentication_and_identity_failures_are_never_retried() {
        for class in [
            StorageErrorClass::NeedsAuth,
            StorageErrorClass::Conflict,
            StorageErrorClass::Permanent,
        ] {
            let cancelled = AtomicBool::new(false);
            let ctx = context("permanent", &cancelled);
            let calls = AtomicUsize::new(0);
            let result = execute::<(), _, _>(&ctx, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(AttemptError::new(class, "denied or changed identity"))
            })
            .await;
            assert_eq!(result.unwrap_err().class(), class);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_during_backoff_never_constructs_another_request() {
        let cancelled = AtomicBool::new(false);
        let ctx = context("cancel-backoff", &cancelled);
        let calls = AtomicUsize::new(0);
        let operation = execute::<(), _, _>(&ctx, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(AttemptError::transient("503").with_retry_after(Duration::from_secs(5)))
        });
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            cancelled.store(true, Ordering::SeqCst);
        };
        let (result, _) = tokio::join!(operation, cancel);
        assert!(matches!(result, Err(OperationError::Cancelled)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn permanently_unsendable_sdk_body_is_not_retried() {
        use aws_sdk_s3::error::ConnectorError;
        use aws_sdk_s3::operation::get_object::GetObjectError;
        let cancelled = AtomicBool::new(false);
        let ctx = context("unsendable", &cancelled);
        let calls = AtomicUsize::new(0);
        let result = execute::<(), _, _>(&ctx, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            let error: SdkError<GetObjectError, HttpResponse> = SdkError::dispatch_failure(
                ConnectorError::user("request body cannot be replayed".into()),
            );
            Err(AttemptError::from_sdk(&error))
        })
        .await
        .unwrap_err();
        assert_eq!(result.class(), StorageErrorClass::Permanent);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_connection_keeps_its_description_in_the_attempt_error() {
        use super::super::s3_client::describe_s3_error;
        use aws_sdk_s3::error::{ConnectorError, ErrorMetadata};
        use aws_sdk_s3::operation::get_object::GetObjectError;
        use aws_sdk_s3::primitives::SdkBody;
        type GetError = SdkError<GetObjectError, HttpResponse>;
        let reset: GetError =
            SdkError::dispatch_failure(ConnectorError::io("connection reset by peer".into()));
        let cancelled = AtomicBool::new(false);
        let ctx = context("connection-reset", &cancelled);
        let calls = AtomicUsize::new(0);
        let error = execute::<(), _, _>(&ctx, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(AttemptError::from_sdk(&reset))
        })
        .await
        .unwrap_err();
        // A dropped connection is retried, and what the task finally shows
        // names the failure rather than a placeholder.
        assert_eq!(error.class(), StorageErrorClass::Transient);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let text = error.to_string();
        assert_eq!(
            text,
            format!(
                "transient: {} (after 3 attempts)",
                describe_s3_error(&reset)
            )
        );
        assert!(text.contains("connection reset by peer"), "{text}");

        let timed_out: GetError = SdkError::timeout_error("connect took too long");
        let attempt = AttemptError::from_sdk(&timed_out);
        assert_eq!(attempt.class, StorageErrorClass::Transient);
        assert_eq!(attempt.message, describe_s3_error(&timed_out));
        assert!(attempt.message.contains("connect took too long"));

        // A service error keeps only its code: the message could carry
        // request internals into persisted task errors.
        let service = |status: u16, metadata: ErrorMetadata| -> GetError {
            SdkError::service_error(
                GetObjectError::generic(metadata),
                HttpResponse::new(status.try_into().unwrap(), SdkBody::empty()),
            )
        };
        let denied = service(
            403,
            ErrorMetadata::builder()
                .code("AccessDenied")
                .message("Access Denied")
                .build(),
        );
        assert_eq!(AttemptError::from_sdk(&denied).message, "AccessDenied");
        // A code-less service error (a CDN's bare 520) has nothing to name.
        let bare = service(520, ErrorMetadata::builder().build());
        assert_eq!(
            AttemptError::from_sdk(&bare).class,
            StorageErrorClass::Transient
        );
        assert_eq!(
            AttemptError::from_sdk(&bare).message,
            "Storage request failed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn truncated_sdk_success_response_is_retried_as_a_complete_read() {
        use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
        use aws_sdk_s3::primitives::SdkBody;
        let cancelled = AtomicBool::new(false);
        let ctx = context("truncated-xml", &cancelled);
        let calls = AtomicUsize::new(0);
        execute(&ctx, || async {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let raw = HttpResponse::new(200_u16.try_into().unwrap(), SdkBody::empty());
                let error: SdkError<ListObjectsV2Error, HttpResponse> =
                    SdkError::response_error("truncated XML body", raw);
                assert_eq!(
                    s3_error_class(&error, true),
                    StorageErrorClass::OutcomeUnknown
                );
                Err(AttemptError::from_sdk(&error))
            } else {
                Ok(())
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_and_total_deadline_include_backoff() {
        let cancelled = AtomicBool::new(false);
        let ctx = context("retry-after", &cancelled);
        let calls = AtomicUsize::new(0);
        let start = Instant::now();
        execute(&ctx, || async {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(AttemptError::transient("503").with_retry_after(Duration::from_secs(5)))
            } else {
                Ok(())
            }
        })
        .await
        .unwrap();
        assert!(start.elapsed() >= Duration::from_secs(5));
        let mut ctx = context("deadline-backoff", &cancelled);
        ctx.deadline = Instant::now() + Duration::from_secs(1);
        let calls = AtomicUsize::new(0);
        let result = execute::<(), _, _>(&ctx, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(AttemptError::transient("503").with_retry_after(Duration::from_secs(5)))
        })
        .await;
        assert!(matches!(result, Err(OperationError::Deadline { .. })));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_wait_beyond_the_deadline_returns_at_once_with_the_last_error() {
        let cancelled = AtomicBool::new(false);
        let ctx = context("retry-after-past-deadline", &cancelled);
        let calls = AtomicUsize::new(0);
        let start = Instant::now();
        let result = execute::<(), _, _>(&ctx, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(AttemptError::transient("SlowDown").with_retry_after(Duration::from_secs(60)))
        })
        .await;
        // The durable task scheduler owns a wait this long; the worker must
        // not be held until the 30 s deadline first.
        assert!(start.elapsed() < Duration::from_secs(1));
        match result {
            Err(OperationError::Deadline { last: Some(error) }) => {
                assert_eq!(error.message, "SlowDown");
                assert_eq!(error.retry_after, Duration::from_secs(60));
            }
            other => panic!("expected a deadline carrying the last attempt, got {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_waits_over_thirty_seconds_go_to_the_task_scheduler() {
        let cancelled = AtomicBool::new(false);
        let mut ctx = context("retry-after-cap", &cancelled);
        ctx.deadline = Instant::now() + Duration::from_secs(300);
        let calls = AtomicUsize::new(0);
        let start = Instant::now();
        let result = execute::<(), _, _>(&ctx, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(AttemptError::transient("SlowDown").with_retry_after(Duration::from_secs(60)))
        })
        .await;
        // v0.3.5 refused any wait over 30 s: a hidden long sleep would hold
        // the worker and its relay memory; the durable retry does not.
        assert!(start.elapsed() < Duration::from_secs(1));
        match result {
            Err(OperationError::Deadline { last: Some(error) }) => {
                assert_eq!(error.retry_after, Duration::from_secs(60));
            }
            other => panic!("expected a deadline carrying the last attempt, got {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let calls = AtomicUsize::new(0);
        let start = Instant::now();
        execute(&ctx, || async {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(AttemptError::transient("SlowDown").with_retry_after(Duration::from_secs(5)))
            } else {
                Ok(())
            }
        })
        .await
        .unwrap();
        assert!(start.elapsed() >= Duration::from_secs(5));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_or_expired_queue_never_dispatches() {
        let cancelled = AtomicBool::new(false);
        let ctx = context("queue-cancel", &cancelled);
        let budget = endpoint_budget(ctx.endpoint);
        let _all = budget.data.acquire_many(DATA_LIMIT as u32).await.unwrap();
        let calls = AtomicUsize::new(0);
        let operation = execute(&ctx, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            cancelled.store(true, Ordering::SeqCst);
        };
        let (result, _) = tokio::join!(operation, cancel);
        assert!(matches!(result, Err(OperationError::Cancelled)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        cancelled.store(false, Ordering::SeqCst);
        let result = execute(&ctx, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert!(matches!(result, Err(OperationError::Deadline { .. })));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_and_deadline_interrupt_inflight_body() {
        let cancelled = AtomicBool::new(false);
        let ctx = context("network-cancel", &cancelled);
        let operation = execute::<(), _, _>(&ctx, || async { std::future::pending().await });
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            cancelled.store(true, Ordering::SeqCst);
        };
        let (result, _) = tokio::join!(operation, cancel);
        assert!(matches!(result, Err(OperationError::Cancelled)));
        cancelled.store(false, Ordering::SeqCst);
        let result = execute::<(), _, _>(&ctx, || async { std::future::pending().await }).await;
        assert!(matches!(result, Err(OperationError::Deadline { .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn endpoint_and_control_budgets_are_independent() {
        let cancelled = AtomicBool::new(false);
        let mut ctx = context("busy-data", &cancelled);
        let budget = endpoint_budget(ctx.endpoint);
        let _all = budget.data.acquire_many(DATA_LIMIT as u32).await.unwrap();
        ctx.kind = OperationKind::Head;
        execute(&ctx, || async { Ok(()) }).await.unwrap();
        ctx.kind = OperationKind::Get;
        ctx.endpoint = "other-endpoint";
        execute(&ctx, || async { Ok(()) }).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn data_admission_is_shared_across_scopes_on_one_endpoint() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut jobs = tokio::task::JoinSet::new();
        for bucket in 0..16 {
            let active = active.clone();
            let maximum = maximum.clone();
            jobs.spawn(async move {
                let cancelled = AtomicBool::new(false);
                let scope = format!("bucket-{bucket}");
                let mut ctx = context("shared-capacity", &cancelled);
                ctx.scope = &scope;
                execute(&ctx, || async {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
                .unwrap();
            });
        }
        while let Some(result) = jobs.join_next().await {
            result.unwrap();
        }
        assert_eq!(maximum.load(Ordering::SeqCst), DATA_LIMIT);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn expired_or_paused_operations_do_not_construct_requests() {
        let cancelled = AtomicBool::new(false);
        let paused = AtomicBool::new(false);
        let mut ctx = context("not-dispatched", &cancelled).with_pause(&paused);
        let calls = AtomicUsize::new(0);
        ctx.deadline = Instant::now();
        let result = execute(&ctx, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .await;
        assert!(matches!(result, Err(OperationError::Deadline { .. })));
        ctx.deadline = Instant::now() + Duration::from_secs(30);
        paused.store(true, Ordering::SeqCst);
        let result = execute(&ctx, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .await;
        assert!(matches!(result, Err(OperationError::Paused)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn paired_streams_on_one_endpoint_reserve_two_slots_atomically() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut jobs = tokio::task::JoinSet::new();
        for _ in 0..12 {
            let active = active.clone();
            let maximum = maximum.clone();
            jobs.spawn(async move {
                let cancelled = AtomicBool::new(false);
                let ctx = context("same-endpoint-pair", &cancelled)
                    .with_peer_endpoint("same-endpoint-pair");
                execute(&ctx, || async {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
                .unwrap();
            });
        }
        while let Some(result) = jobs.join_next().await {
            result.unwrap();
        }
        assert_eq!(maximum.load(Ordering::SeqCst), DATA_LIMIT / 2);
    }

    #[tokio::test(start_paused = true)]
    async fn opposing_stream_comparisons_use_one_endpoint_order() {
        let mut jobs = tokio::task::JoinSet::new();
        for index in 0..20 {
            jobs.spawn(async move {
                let cancelled = AtomicBool::new(false);
                let (endpoint, peer) = if index % 2 == 0 {
                    ("pair-A", "pair-B")
                } else {
                    ("pair-B", "pair-A")
                };
                let ctx = context(endpoint, &cancelled).with_peer_endpoint(peer);
                execute(&ctx, || async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(())
                })
                .await
                .unwrap();
            });
        }
        while let Some(result) = jobs.join_next().await {
            result.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_paired_admission_releases_the_first_endpoint() {
        let cancelled = AtomicBool::new(false);
        let first = endpoint_budget("cancel-pair-A");
        let second = endpoint_budget("cancel-pair-B");
        let _held = second.data.acquire_many(DATA_LIMIT as u32).await.unwrap();
        let ctx = context("cancel-pair-A", &cancelled).with_peer_endpoint("cancel-pair-B");
        let calls = AtomicUsize::new(0);
        let operation = execute(&ctx, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            cancelled.store(true, Ordering::SeqCst);
        };
        let (result, _) = tokio::join!(operation, cancel);
        assert!(matches!(result, Err(OperationError::Cancelled)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(first.data.available_permits(), DATA_LIMIT);
    }
}
