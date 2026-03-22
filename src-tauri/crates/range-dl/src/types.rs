use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Callback that generates a fresh presigned URL on each invocation.
/// Called N times upfront (once per chunk) before spawning chunk tasks.
/// The caller (e.g., Tauri worker) provides this using provider-specific presigning logic;
/// the range-dl crate itself is provider-agnostic.
pub type UrlProvider =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync>;

/// Configuration for range-based parallel downloads.
#[derive(Debug, Clone)]
pub struct RangeDownloadConfig {
    /// Minimum chunk size in bytes (default: 8 MB).
    /// Files smaller than this use single-stream download (no chunking overhead).
    pub min_chunk_size: u64,
    /// Maximum concurrent chunks per file (default: 8, hard cap: 64).
    pub max_chunks: u16,
    /// Write buffer size per chunk in bytes (default: 2 MB).
    pub write_buffer_size: usize,
    /// Maximum retry attempts per chunk (default: 3).
    pub max_retries: u8,
    /// Base duration for exponential retry backoff (default: 1s → 1s, 2s, 4s).
    pub retry_backoff_base: Duration,
    /// Connection timeout per chunk HTTP request (default: 30s).
    pub connect_timeout: Duration,
}

impl Default for RangeDownloadConfig {
    fn default() -> Self {
        Self {
            min_chunk_size: 8 * 1024 * 1024,       // 8 MB
            max_chunks: 8,
            write_buffer_size: 2 * 1024 * 1024,     // 2 MB
            max_retries: 3,
            retry_backoff_base: Duration::from_secs(1),
            connect_timeout: Duration::from_secs(30),
        }
    }
}

impl RangeDownloadConfig {
    /// Determine the max_chunks ceiling based on file size tier.
    pub fn chunks_ceiling_for_size(&self, file_size: u64) -> u16 {
        let tier_max = if file_size < 10 * 1024 * 1024 {
            1 // < 10 MB → single stream
        } else if file_size < 100 * 1024 * 1024 {
            4 // 10–100 MB
        } else if file_size < 1024 * 1024 * 1024 {
            8 // 100 MB – 1 GB
        } else {
            16 // > 1 GB
        };
        tier_max.min(self.max_chunks)
    }
}

/// Status of an individual chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChunkStatus {
    Pending,
    Downloading,
    Complete,
    Failed,
    Paused,
}

/// Persisted state of a single chunk (used for resume).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkState {
    pub chunk_id: u16,
    /// Inclusive start byte of this chunk's assigned range.
    pub start: u64,
    /// Exclusive end byte (range is [start, end)).
    pub end: u64,
    /// Bytes successfully written to disk so far.
    pub downloaded_bytes: u64,
    pub status: ChunkStatus,
}

impl ChunkState {
    /// Total size of this chunk's assigned range.
    pub fn total_bytes(&self) -> u64 {
        self.end - self.start
    }

    /// Remaining bytes to download.
    pub fn remaining(&self) -> u64 {
        self.total_bytes().saturating_sub(self.downloaded_bytes)
    }

    /// The byte offset to resume from.
    pub fn resume_offset(&self) -> u64 {
        self.start + self.downloaded_bytes
    }
}

/// Events emitted by the download engine.
#[derive(Debug, Clone, Serialize)]
pub enum ChunkEvent {
    /// Aggregated progress for all chunks in a file (emitted at most once per throttle interval).
    Progress {
        chunks: Vec<ChunkProgress>,
        aggregate_speed: f64,
        aggregate_downloaded: u64,
        total_bytes: u64,
    },
    /// A single chunk completed its range.
    ChunkComplete { chunk_id: u16 },
    /// A chunk encountered an error and is retrying.
    ChunkRetry {
        chunk_id: u16,
        attempt: u8,
        error: String,
    },
    /// A chunk permanently failed.
    ChunkFailed { chunk_id: u16, error: String },
    /// All chunks completed — the file is assembled and ready.
    Complete {
        total_bytes: u64,
        elapsed_secs: f64,
        avg_speed: f64,
    },
    /// Download was paused. Contains chunk states for resume.
    Paused { chunks_state: Vec<ChunkState> },
    /// Download was cancelled.
    Cancelled,
}

/// Per-chunk progress snapshot (included in aggregated Progress events).
#[derive(Debug, Clone, Serialize)]
pub struct ChunkProgress {
    pub chunk_id: u16,
    pub start: u64,
    pub end: u64,
    pub downloaded_bytes: u64,
    pub speed: f64,
    pub status: ChunkStatus,
}

/// Signals for controlling a running download.
pub struct DownloadControl {
    /// Cancel the download. Fires once; .part file will be deleted.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Pause the download. Set to `true` to pause; chunks flush buffers and exit cleanly.
    pub pause: tokio::sync::watch::Sender<bool>,
}

/// Describes where to download a file.
#[derive(Debug, Clone)]
pub struct DownloadTarget {
    /// Total file size in bytes (must be known upfront for chunking).
    pub file_size: u64,
    /// Destination path for the final file (without .part extension — the engine adds it).
    pub destination: PathBuf,
}

/// Classification of errors for retry decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Transient network error — safe to retry.
    Network,
    /// Disk full — abort immediately, no retry.
    DiskFull,
    /// Permission denied — abort immediately.
    PermissionDenied,
    /// HTTP 401/403 — URL may be expired, retry with fresh URL.
    AuthExpired,
    /// Other error — retry once, then abort.
    Other,
}

pub fn classify_error(err: &str) -> ErrorKind {
    let lower = err.to_lowercase();
    if lower.contains("no space left")
        || lower.contains("disk full")
        || lower.contains("storage full")
        || lower.contains("not enough space")
    {
        ErrorKind::DiskFull
    } else if lower.contains("permission denied") || lower.contains("access denied") {
        ErrorKind::PermissionDenied
    } else if lower.contains("403") || lower.contains("401") || lower.contains("forbidden") {
        ErrorKind::AuthExpired
    } else if lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("reset")
        || lower.contains("broken pipe")
        || lower.contains("dns")
    {
        ErrorKind::Network
    } else {
        ErrorKind::Other
    }
}
