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
