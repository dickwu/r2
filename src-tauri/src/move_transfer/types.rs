//! Move transfer event types and status definitions

use serde::{Deserialize, Serialize};

/// Maximum concurrent move tasks per source bucket
pub const MAX_CONCURRENT_MOVES: i64 = 5;

/// Maximum concurrent part uploads for multipart transfers
pub const MAX_CONCURRENT_PARTS: usize = 4;

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MoveStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "downloading")]
    Downloading,
    #[serde(rename = "uploading")]
    Uploading,
    #[serde(rename = "finishing")]
    Finishing,
    #[serde(rename = "deleting")]
    DeletingOriginal,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl std::fmt::Display for MoveStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MoveStatus::Pending => write!(f, "pending"),
            MoveStatus::Downloading => write!(f, "downloading"),
            MoveStatus::Uploading => write!(f, "uploading"),
            MoveStatus::Finishing => write!(f, "finishing"),
            MoveStatus::DeletingOriginal => write!(f, "deleting"),
            MoveStatus::Paused => write!(f, "paused"),
            MoveStatus::Success => write!(f, "success"),
            MoveStatus::Error => write!(f, "error"),
            MoveStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl From<String> for MoveStatus {
    fn from(value: String) -> Self {
        match value.as_str() {
            "pending" => MoveStatus::Pending,
            "downloading" => MoveStatus::Downloading,
            "uploading" => MoveStatus::Uploading,
            "finishing" => MoveStatus::Finishing,
            "deleting" => MoveStatus::DeletingOriginal,
            "paused" => MoveStatus::Paused,
            "success" => MoveStatus::Success,
            "error" => MoveStatus::Error,
            "cancelled" => MoveStatus::Cancelled,
            _ => MoveStatus::Pending,
        }
    }
}

/// Progress event payload for move tasks
#[derive(Debug, Clone, Serialize)]
pub struct MoveProgress {
    pub task_id: String,
    pub phase: String,
    pub percent: u32,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub speed: f64, // bytes per second
}

/// Status change event payload
#[derive(Debug, Clone, Serialize)]
pub struct MoveStatusChanged {
    pub task_id: String,
    pub status: String,
    pub error: Option<String>,
}

/// Task deleted event payload
#[derive(Debug, Clone, Serialize)]
pub struct MoveTaskDeleted {
    pub task_id: String,
}

/// Batch operation event payload
#[derive(Debug, Clone, Serialize)]
pub struct MoveBatchOperation {
    pub operation: String, // "clear_finished" | "clear_all" | "pause_all" | "resume_all"
    pub source_bucket: String,
    pub source_account_id: String,
}

#[cfg(test)]
mod tests {
    use super::MoveStatus;

    #[test]
    fn move_status_display_matches_expected_strings() {
        assert_eq!(MoveStatus::Pending.to_string(), "pending");
        assert_eq!(MoveStatus::Downloading.to_string(), "downloading");
        assert_eq!(MoveStatus::Uploading.to_string(), "uploading");
        assert_eq!(MoveStatus::Finishing.to_string(), "finishing");
        assert_eq!(MoveStatus::DeletingOriginal.to_string(), "deleting");
        assert_eq!(MoveStatus::Paused.to_string(), "paused");
        assert_eq!(MoveStatus::Success.to_string(), "success");
        assert_eq!(MoveStatus::Error.to_string(), "error");
        assert_eq!(MoveStatus::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn move_status_from_string_defaults_to_pending() {
        let status: MoveStatus = "unknown".to_string().into();
        assert_eq!(status, MoveStatus::Pending);
    }
}
