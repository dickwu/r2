//! Download metadata sidecar file for resume support.
//! Written alongside the .part file as a JSON backup of chunk states.
//! SQLite is the primary persistence; this is a recovery fallback.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::types::ChunkState;

#[derive(Debug, Serialize, Deserialize)]
pub struct DownloadMeta {
    pub file_size: u64,
    pub chunks: Vec<ChunkState>,
}

impl DownloadMeta {
    /// Save metadata to a JSON file.
    pub async fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize meta: {}", e))?;
        tokio::fs::write(path, json)
            .await
            .map_err(|e| format!("Failed to write meta file: {}", e))
    }

    /// Load metadata from a JSON file.
    pub async fn load(path: &Path) -> Result<Self, String> {
        let json = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("Failed to read meta file: {}", e))?;
        serde_json::from_str(&json).map_err(|e| format!("Failed to parse meta: {}", e))
    }
}
