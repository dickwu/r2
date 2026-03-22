//! Download event types and payloads

use serde::Serialize;

/// Maximum concurrent downloads per bucket
pub const MAX_CONCURRENT_DOWNLOADS: i64 = 5;

/// Progress event payload for downloads
#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub task_id: String,
    pub percent: u32,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed: f64, // bytes per second
}

/// Status change event payload
#[derive(Debug, Clone, Serialize)]
pub struct DownloadStatusChanged {
    pub task_id: String,
    pub status: String,
    pub error: Option<String>,
}

/// Task deleted event payload
#[derive(Debug, Clone, Serialize)]
pub struct DownloadTaskDeleted {
    pub task_id: String,
}

/// Batch operation event payload (for clear finished/all, pause all, resume all)
#[derive(Debug, Clone, Serialize)]
pub struct DownloadBatchOperation {
    pub operation: String, // "clear_finished" | "clear_all" | "pause_all" | "resume_all"
    pub bucket: String,
    pub account_id: String,
}

/// Chunk-level progress event payload (aggregated, emitted once per file per 200ms)
#[derive(Debug, Clone, Serialize)]
pub struct DownloadChunkProgressEvent {
    pub task_id: String,
    pub chunks: Vec<ChunkProgressInfo>,
    pub aggregate_speed: f64,
    pub aggregate_downloaded: u64,
    pub total_bytes: u64,
}

/// Per-chunk progress info within an aggregated event
#[derive(Debug, Clone, Serialize)]
pub struct ChunkProgressInfo {
    pub chunk_id: u16,
    pub start: u64,
    pub end: u64,
    pub downloaded_bytes: u64,
    pub speed: f64,
    pub status: String,
}
