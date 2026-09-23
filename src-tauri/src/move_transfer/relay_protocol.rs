//! The relay HTTP contract, resource reservations, and replayable payloads.
//! A signed URL is only an address: each read still proves range and identity.
use crate::providers::operation::Backoff;
use crate::providers::resources::{ByteLease, DiskLease, ResourceKind};
use aws_sdk_s3::primitives::{ByteStream, SdkBody};
use futures_util::StreamExt;
use reqwest::{header, Client, Response, StatusCode};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) const MIB: u64 = 1024 * 1024;
const BUFFER_BUDGET_MIB: u32 = 256;
const MEMORY_PART_LIMIT: u64 = 128 * MIB;
const STREAM_RESERVATION_MIB: u32 = 2;
/// At most two provider-sized (<= 5 GiB) part caches may exist process-wide.
const SPOOL_SLOTS: usize = 2;
pub(crate) const MAX_ATTEMPTS: usize = 3;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(crate) struct ReadError {
    pub message: String,
    pub retryable: bool,
    pub retry_after: Option<Duration>,
}

impl ReadError {
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            retry_after: None,
        }
    }
    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            retry_after: None,
        }
    }
}

/// Checks both flags while queuing, requesting, reading, and backing off.
/// Dropping a mutating request never proves rollback; the caller journals that phase first.
pub(crate) async fn interruptible<T>(
    cancelled: &AtomicBool,
    paused: &AtomicBool,
    future: impl Future<Output = T>,
) -> Result<T, String> {
    tokio::pin!(future);
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err("cancelled: Move cancelled".into());
        }
        if paused.load(Ordering::SeqCst) {
            return Err("paused: Move paused".into());
        }
        tokio::select! {
            result = &mut future => return Ok(result),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

pub(crate) fn attempt_timeout(bytes: u64) -> Duration {
    // A progressing large part gets time proportional to size; idle bodies have a separate bound.
    Duration::from_secs(30 + bytes.div_ceil(128 * 1024))
}

/// Deadline for a replayable relay operation: every one of its MAX_ATTEMPTS
/// attempts keeps a full attempt_timeout, plus the executor's backoff between
/// them. One attempt's timeout would leave a stalled first attempt no retry.
pub(crate) fn operation_budget(bytes: u64) -> Duration {
    let attempts = MAX_ATTEMPTS as u32;
    let backoff: Duration = (0..attempts - 1).map(|n| Backoff::DEFAULT.cap(n)).sum();
    attempt_timeout(bytes) * attempts + backoff
}

pub(crate) fn validate_response(
    response: &Response,
    range: Option<(u64, u64)>,
    total: u64,
    etag: &str,
) -> Result<u64, ReadError> {
    let status = response.status();
    if status == StatusCode::PRECONDITION_FAILED {
        return Err(ReadError::permanent("conflict: source identity changed"));
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(ReadError::permanent(format!(
            "needs_auth: source returned HTTP {}",
            status.as_u16()
        )));
    }
    if status == StatusCode::TOO_MANY_REQUESTS
        || (status.is_server_error() && status != StatusCode::NOT_IMPLEMENTED)
    {
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);
        return Err(ReadError {
            message: format!("Source temporarily unavailable: HTTP {}", status.as_u16()),
            retryable: true,
            retry_after,
        });
    }
    let expected = match range {
        Some((start, end)) => {
            if start > end || end >= total {
                return Err(ReadError::permanent("Invalid requested range"));
            }
            if status != StatusCode::PARTIAL_CONTENT {
                return Err(ReadError::permanent(format!(
                    "Range protocol error: expected HTTP 206, got {}",
                    status.as_u16()
                )));
            }
            let expected_range = format!("bytes {start}-{end}/{total}");
            if response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                != Some(expected_range.as_str())
            {
                return Err(ReadError::permanent(format!(
                    "Range protocol error: expected Content-Range {expected_range}"
                )));
            }
            end - start + 1
        }
        None => {
            if status != StatusCode::OK || response.headers().contains_key(header::CONTENT_RANGE) {
                return Err(ReadError::permanent(format!(
                    "Full-read protocol error: expected HTTP 200, got {}",
                    status.as_u16()
                )));
            }
            total
        }
    };
    if let Some(length) = response.headers().get(header::CONTENT_LENGTH) {
        if length.to_str().ok().and_then(|v| v.parse::<u64>().ok()) != Some(expected) {
            return Err(ReadError::permanent(
                "Read protocol error: wrong Content-Length",
            ));
        }
    }
    if etag.is_empty()
        || response
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            != Some(etag)
    {
        return Err(ReadError::permanent(
            "conflict: source response ETag differs from the recorded identity",
        ));
    }
    Ok(expected)
}

struct TempPart(PathBuf);
impl Drop for TempPart {
    fn drop(&mut self) {
        // Part files are local caches, never the only copy; their source remains remote until verified.
        if let Err(error) = std::fs::remove_file(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Failed to remove relay part cache: {error}");
            }
        }
    }
}

pub(crate) struct Payload {
    pub body: SdkBody,
    pub len: u64,
    // Retain both until the upload (including any retries) has settled.
    _file: Option<TempPart>,
    _reservation: PayloadReservation,
}

/// Relay memory and part-cache slots, shared by every relay in the process.
///
/// A read is admitted here before it asks for an endpoint request slot, never
/// while holding one: payloads that already hold memory still need a slot for
/// their UploadPart, so a read waiting for memory inside a slot can stall both
/// until a deadline expires. The order is always budget, then slot.
#[derive(Clone)]
pub(crate) struct RelayBudget {
    bytes: Arc<Semaphore>,
    spool_slots: Arc<Semaphore>,
}

impl RelayBudget {
    pub(crate) fn shared() -> Self {
        static SHARED: OnceLock<RelayBudget> = OnceLock::new();
        SHARED
            .get_or_init(|| Self::with_capacity(BUFFER_BUDGET_MIB, SPOOL_SLOTS))
            .clone()
    }

    pub(crate) fn with_capacity(budget_mib: u32, spool_slots: usize) -> Self {
        Self {
            bytes: Arc::new(Semaphore::new(budget_mib as usize)),
            spool_slots: Arc::new(Semaphore::new(spool_slots)),
        }
    }

    /// Reserves what one read of `range` will hold before any request is made:
    /// the spool decision, a part-cache slot for a spooled part, its share of
    /// the byte budget and the accounting lease. Only cancellation or pause
    /// ends the wait.
    pub(crate) async fn reserve(
        &self,
        range: Option<(u64, u64)>,
        total: u64,
        cancelled: &AtomicBool,
        paused: &AtomicBool,
    ) -> Result<PayloadReservation, ReadError> {
        let expected = range.map(|(s, e)| e - s + 1).unwrap_or(total);
        let spool = expected > MEMORY_PART_LIMIT;
        let weight = if spool {
            STREAM_RESERVATION_MIB
        } else {
            expected.div_ceil(MIB).max(1) as u32
        };
        let spool_slot = if spool {
            Some(
                interruptible(cancelled, paused, self.spool_slots.clone().acquire_owned())
                    .await
                    .map_err(ReadError::permanent)?
                    .map_err(|_| ReadError::permanent("Relay part cache budget closed"))?,
            )
        } else {
            None
        };
        let bytes = interruptible(
            cancelled,
            paused,
            self.bytes.clone().acquire_many_owned(weight),
        )
        .await
        .map_err(ReadError::permanent)?
        .map_err(|_| ReadError::permanent("Relay byte budget closed"))?;
        let lease = ByteLease::new(
            if spool {
                ResourceKind::RelaySpool
            } else {
                ResourceKind::RelayBuffer
            },
            expected,
        );
        Ok(PayloadReservation {
            range,
            total,
            expected,
            spool,
            _bytes: bytes,
            _spool_slot: spool_slot,
            _lease: lease,
        })
    }
}

/// Admission for one relay read. It survives failed read attempts and moves
/// into the Payload only when a read succeeds.
pub(crate) struct PayloadReservation {
    range: Option<(u64, u64)>,
    total: u64,
    expected: u64,
    spool: bool,
    _bytes: OwnedSemaphorePermit,
    _spool_slot: Option<OwnedSemaphorePermit>,
    _lease: ByteLease,
}

/// A validated body read under a reservation that still belongs to the caller.
pub(crate) struct FetchedBody {
    body: SdkBody,
    len: u64,
    file: Option<TempPart>,
}

impl PayloadReservation {
    /// One read attempt of the reserved range, run inside an executor attempt.
    /// Large parts use a replayable file while ordinary parts move their single
    /// Vec into ByteStream; a failure drops only this attempt's file.
    pub(crate) async fn fetch(
        &self,
        client: &Client,
        url: &str,
        etag: &str,
        cancelled: &AtomicBool,
        paused: &AtomicBool,
    ) -> Result<FetchedBody, ReadError> {
        let expected = self.expected;
        let mut request = client
            .get(url)
            .header(header::IF_MATCH, etag)
            .header(header::ACCEPT_ENCODING, "identity");
        if let Some((start, end)) = self.range {
            request = request.header(header::RANGE, format!("bytes={start}-{end}"));
        }
        let response = interruptible(
            cancelled,
            paused,
            tokio::time::timeout(IDLE_TIMEOUT, request.send()),
        )
        .await
        .map_err(ReadError::permanent)?
        .map_err(|_| ReadError::transient("Source response headers timed out"))?
        .map_err(|e| ReadError::transient(format!("Source request failed: {}", e.without_url())))?;
        if response.status() == StatusCode::FORBIDDEN {
            let mut chunks = response.bytes_stream();
            let mut text = Vec::new();
            while let Some(chunk) = interruptible(
                cancelled,
                paused,
                tokio::time::timeout(IDLE_TIMEOUT, chunks.next()),
            )
            .await
            .map_err(ReadError::permanent)?
            .map_err(|_| {
                ReadError::permanent("needs_auth: source authorization response stalled")
            })? {
                let chunk = chunk.map_err(|_| {
                    ReadError::permanent("needs_auth: source authorization response failed")
                })?;
                if text.len() + chunk.len() > 64 * 1024 {
                    break;
                }
                text.extend_from_slice(&chunk);
            }
            let text = String::from_utf8_lossy(&text);
            if text.contains("<Code>RequestExpired</Code>")
                || (text.contains("<Code>AccessDenied</Code>")
                    && text.contains("<Message>Request has expired</Message>"))
            {
                return Err(ReadError::transient(
                    "Signed source URL expired; renew the signature",
                ));
            }
            return Err(ReadError::permanent("needs_auth: source returned HTTP 403"));
        }
        validate_response(&response, self.range, self.total, etag)?;

        let mut temp = None;
        let mut file = None;
        let mut disk_lease = None;
        let mut buffer = Vec::new();
        if self.spool {
            static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let temp_dir = std::env::temp_dir();
            disk_lease = Some(
                DiskLease::reserve(&temp_dir, expected, || {
                    crate::mount::available_space(&temp_dir)
                })
                .map_err(|e| {
                    ReadError::permanent(format!("Cannot reserve relay part cache disk space: {e}"))
                })?,
            );
            let path = temp_dir.join(format!(
                "r2-relay-{}-{now}-{}.part",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = tokio::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            file = Some(options.open(&path).await.map_err(|e| {
                ReadError::permanent(format!("Cannot create relay part cache: {e}"))
            })?);
            temp = Some(TempPart(path));
        } else {
            buffer
                .try_reserve_exact(expected as usize)
                .map_err(|e| ReadError::permanent(format!("Cannot reserve relay buffer: {e}")))?;
        }
        let mut body = response.bytes_stream();
        let mut received = 0_u64;
        while let Some(chunk) = interruptible(
            cancelled,
            paused,
            tokio::time::timeout(IDLE_TIMEOUT, body.next()),
        )
        .await
        .map_err(ReadError::permanent)?
        .map_err(|_| ReadError::transient("Source response body stalled"))?
        {
            let chunk = chunk.map_err(|e| {
                ReadError::transient(format!("Source response body failed: {}", e.without_url()))
            })?;
            received += chunk.len() as u64;
            if received > expected {
                return Err(ReadError::permanent(
                    "Read protocol error: response exceeds expected length",
                ));
            }
            if let Some(file) = &mut file {
                interruptible(cancelled, paused, file.write_all(&chunk))
                    .await
                    .map_err(ReadError::permanent)?
                    .map_err(|e| {
                        ReadError::permanent(format!("Cannot write relay part cache: {e}"))
                    })?;
            } else {
                buffer.extend_from_slice(&chunk);
            }
        }
        if received != expected {
            return Err(ReadError::permanent(format!(
                "Read protocol error: expected {expected} bytes, received {received}"
            )));
        }
        let body = if let Some(mut file) = file {
            file.flush()
                .await
                .map_err(|e| ReadError::permanent(format!("Cannot flush relay part cache: {e}")))?;
            drop(file);
            drop(disk_lease.take());
            ByteStream::from_path(&temp.as_ref().expect("spooled payload has a path").0)
                .await
                .map_err(|e| ReadError::permanent(format!("Cannot reopen relay part cache: {e}")))?
        } else {
            ByteStream::from(buffer)
        };
        Ok(FetchedBody {
            body: body.into_inner(),
            len: received,
            file: temp,
        })
    }

    pub(crate) fn into_payload(self, fetched: FetchedBody) -> Payload {
        Payload {
            body: fetched.body,
            len: fetched.len,
            _file: fetched.file,
            _reservation: self,
        }
    }
}

/// Reserve and read in one step, for a caller that does not retry through the
/// executor. A retrying caller reserves first and fetches inside its attempts.
#[cfg(test)]
pub(crate) async fn fetch_payload(
    client: &Client,
    url: &str,
    range: Option<(u64, u64)>,
    total: u64,
    etag: &str,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<Payload, ReadError> {
    let reservation = RelayBudget::shared()
        .reserve(range, total, cancelled, paused)
        .await?;
    let fetched = reservation
        .fetch(client, url, etag, cancelled, paused)
        .await?;
    Ok(reservation.into_payload(fetched))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn fixture(response: &'static [u8]) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/source", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let count = socket.read(&mut request).await.unwrap();
            socket.write_all(response).await.unwrap();
            String::from_utf8_lossy(&request[..count]).into_owned()
        });
        (url, task)
    }

    async fn read(response: &'static [u8]) -> Result<Payload, ReadError> {
        let (url, task) = fixture(response).await;
        let result = fetch_payload(
            &Client::new(),
            &url,
            Some((2, 3)),
            4,
            "\"v1\"",
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await;
        let request = task.await.unwrap().to_ascii_lowercase();
        assert!(request.contains("range: bytes=2-3"));
        assert!(request.contains("if-match: \"v1\""));
        result
    }

    #[tokio::test]
    async fn reservation_outlives_a_failed_read_and_moves_into_the_payload() {
        let budget = RelayBudget::with_capacity(1, 1);
        let (cancelled, paused) = (AtomicBool::new(false), AtomicBool::new(false));
        let reservation = budget
            .reserve(Some((2, 3)), 4, &cancelled, &paused)
            .await
            .unwrap();
        assert_eq!(budget.bytes.available_permits(), 0);
        let (url, task) = fixture(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n").await;
        let failed = reservation
            .fetch(&Client::new(), &url, "\"v1\"", &cancelled, &paused)
            .await
            .err()
            .unwrap();
        assert!(failed.retryable);
        task.await.unwrap();
        assert_eq!(budget.bytes.available_permits(), 0);
        let (url, task) = fixture(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-3/4\r\nContent-Length: 2\r\nETag: \"v1\"\r\n\r\ncd").await;
        let fetched = reservation
            .fetch(&Client::new(), &url, "\"v1\"", &cancelled, &paused)
            .await
            .unwrap();
        assert!(task.await.unwrap().contains("bytes=2-3"));
        let payload = reservation.into_payload(fetched);
        assert_eq!(payload.len, 2);
        assert_eq!(budget.bytes.available_permits(), 0);
        drop(payload);
        assert_eq!(budget.bytes.available_permits(), 1);
    }

    #[tokio::test]
    async fn rejects_ignored_range_before_consuming_full_object() {
        let result =
            read(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nETag: \"v1\"\r\n\r\nabcd").await;
        assert!(result.err().unwrap().message.contains("expected HTTP 206"));
    }

    #[tokio::test]
    async fn validates_exact_range_total_length_and_identity() {
        let payload = read(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-3/4\r\nContent-Length: 2\r\nETag: \"v1\"\r\n\r\ncd").await.unwrap();
        assert_eq!(
            ByteStream::new(payload.body)
                .collect()
                .await
                .unwrap()
                .into_bytes()
                .as_ref(),
            b"cd"
        );
        for response in [
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-1/4\r\nContent-Length: 2\r\nETag: \"v1\"\r\n\r\nab".as_slice(),
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-3/8\r\nContent-Length: 2\r\nETag: \"v1\"\r\n\r\ncd".as_slice(),
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-3/4\r\nContent-Length: 4\r\nETag: \"v1\"\r\n\r\ncdef".as_slice(),
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-3/4\r\nContent-Length: 2\r\nETag: \"v2\"\r\n\r\ncd".as_slice(),
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-3/4\r\nETag: \"v1\"\r\nConnection: close\r\n\r\nc".as_slice(),
        ] { assert!(read(response).await.is_err()); }
    }

    #[tokio::test]
    async fn permanent_auth_and_conflict_do_not_retry() {
        let auth = read(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await
            .err()
            .unwrap();
        assert!(!auth.retryable);
        assert!(auth.message.starts_with("needs_auth:"));
        let conflict = read(b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\n\r\n")
            .await
            .err()
            .unwrap();
        assert!(!conflict.retryable);
        let transient =
            read(b"HTTP/1.1 503 Unavailable\r\nRetry-After: 2\r\nContent-Length: 0\r\n\r\n")
                .await
                .err()
                .unwrap();
        assert!(transient.retryable);
        assert_eq!(transient.retry_after, Some(Duration::from_secs(2)));
    }

    #[tokio::test]
    async fn cancellation_interrupts_waiting_for_response_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/source", listener.local_addr().unwrap());
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let task = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            signal.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            fetch_payload(
                &Client::new(),
                &url,
                Some((0, 1)),
                2,
                "\"v1\"",
                &cancelled,
                &AtomicBool::new(false),
            ),
        )
        .await
        .unwrap();
        assert!(result.err().unwrap().message.contains("cancelled"));
        task.abort();
    }

    #[tokio::test]
    async fn zero_byte_full_read_requires_real_empty_body() {
        let (url, task) =
            fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nETag: \"v1\"\r\n\r\n").await;
        let payload = fetch_payload(
            &Client::new(),
            &url,
            None,
            0,
            "\"v1\"",
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!(payload.len, 0);
        task.await.unwrap();
    }
    #[tokio::test]
    async fn expiration_can_renew_but_expired_credentials_require_auth() {
        let expired = read(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n<Error><Code>RequestExpired</Code></Error>").await.err().unwrap();
        assert!(expired.retryable);
        let credentials = read(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n<Error><Code>ExpiredToken</Code></Error>").await.err().unwrap();
        assert!(!credentials.retryable);
    }
}
