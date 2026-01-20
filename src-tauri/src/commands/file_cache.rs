//! File caching commands for Tauri frontend

use crate::db::{self, CachedDirectoryNode, CachedFile};
use crate::r2;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use tauri::Emitter;

// ============ Response Types ============

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedFileResponse {
    pub key: String,
    pub size: i64,
    #[serde(rename = "lastModified")]
    pub last_modified: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DirectoryNodeResponse {
    pub path: String,
    #[serde(rename = "fileCount")]
    pub file_count: i32,
    #[serde(rename = "totalFileCount")]
    pub total_file_count: i32,
    pub size: i64,
    #[serde(rename = "totalSize")]
    pub total_size: i64,
    #[serde(rename = "lastModified")]
    pub last_modified: Option<String>,
    #[serde(rename = "lastUpdated")]
    pub last_updated: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResultResponse {
    pub files: Vec<CachedFileResponse>,
    #[serde(rename = "totalCount")]
    pub total_count: i32,
}

#[derive(Debug, Clone, Serialize)]
struct IndexingProgress {
    current: usize,
    total: usize,
}

// ============ Type Conversions ============

impl From<CachedFile> for CachedFileResponse {
    fn from(file: CachedFile) -> Self {
        CachedFileResponse {
            key: file.key,
            size: file.size,
            last_modified: file.last_modified,
        }
    }
}

impl From<CachedDirectoryNode> for DirectoryNodeResponse {
    fn from(node: CachedDirectoryNode) -> Self {
        DirectoryNodeResponse {
            path: node.path,
            file_count: node.file_count,
            total_file_count: node.total_file_count,
            size: node.size,
            total_size: node.total_size,
            last_modified: node.last_modified,
            last_updated: node.last_updated,
        }
    }
}

// ============ Helper Functions ============

async fn get_current_bucket_info() -> Result<(String, String), String> {
    let config = db::get_current_config()
        .await
        .map_err(|e| format!("Failed to get current config: {}", e))?
        .ok_or_else(|| "No active configuration".to_string())?;

    Ok((config.bucket, config.account_id))
}

// ============ Commands ============

#[tauri::command]
pub async fn store_all_files(files: Vec<r2::R2Object>, app: tauri::AppHandle) -> Result<(), String> {
    let (bucket, account_id) = get_current_bucket_info().await?;
    let now = chrono::Utc::now().timestamp();

    let _ = app.emit("sync-phase", "storing");

    let cached_files: Vec<CachedFile> = files
        .into_iter()
        .map(|f| CachedFile {
            bucket: bucket.clone(),
            account_id: account_id.clone(),
            key: f.key,
            parent_path: String::new(), // Computed in store_all_files from key
            name: String::new(),        // Computed in store_all_files from key
            size: f.size,
            last_modified: f.last_modified,
            synced_at: now,
        })
        .collect();

    db::store_all_files(&bucket, &account_id, &cached_files)
        .await
        .map_err(|e| format!("Failed to store files: {}", e))
}

#[tauri::command]
pub async fn get_all_cached_files() -> Result<Vec<CachedFileResponse>, String> {
    let (bucket, account_id) = get_current_bucket_info().await?;

    let files = db::get_all_cached_files(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to get cached files: {}", e))?;

    Ok(files.into_iter().map(|f| f.into()).collect())
}

#[tauri::command]
pub async fn search_cached_files(query: String) -> Result<SearchResultResponse, String> {
    let (bucket, account_id) = get_current_bucket_info().await?;

    let result = db::search_cached_files(&bucket, &account_id, &query)
        .await
        .map_err(|e| format!("Failed to search files: {}", e))?;

    Ok(SearchResultResponse {
        files: result.files.into_iter().map(|f| f.into()).collect(),
        total_count: result.total_count,
    })
}

#[tauri::command]
pub async fn calculate_folder_size(prefix: String) -> Result<i64, String> {
    let (bucket, account_id) = get_current_bucket_info().await?;

    db::calculate_folder_size(&bucket, &account_id, &prefix)
        .await
        .map_err(|e| format!("Failed to calculate folder size: {}", e))
}

#[tauri::command]
pub async fn build_directory_tree(app: tauri::AppHandle) -> Result<(), String> {
    let (bucket, account_id) = get_current_bucket_info().await?;

    let _ = app.emit("sync-phase", "indexing");

    let files = db::get_all_cached_files(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to get cached files: {}", e))?;

    let app_clone = app.clone();
    let progress_callback = move |current: usize, total: usize| {
        let _ = app_clone.emit("indexing-progress", IndexingProgress { current, total });
    };

    db::build_directory_tree(&bucket, &account_id, &files, Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to build directory tree: {}", e))?;

    let _ = app.emit("sync-phase", "complete");

    Ok(())
}

#[tauri::command]
pub async fn get_directory_node(path: String) -> Result<Option<DirectoryNodeResponse>, String> {
    let (bucket, account_id) = get_current_bucket_info().await?;

    let node = db::get_directory_node(&bucket, &account_id, &path)
        .await
        .map_err(|e| format!("Failed to get directory node: {}", e))?;

    Ok(node.map(|n| n.into()))
}

#[tauri::command]
pub async fn get_all_directory_nodes() -> Result<Vec<DirectoryNodeResponse>, String> {
    let (bucket, account_id) = get_current_bucket_info().await?;

    let nodes = db::get_all_directory_nodes(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to get directory nodes: {}", e))?;

    Ok(nodes.into_iter().map(|n| n.into()).collect())
}

#[tauri::command]
pub async fn clear_file_cache() -> Result<(), String> {
    let (bucket, account_id) = get_current_bucket_info().await?;

    db::clear_file_cache(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to clear cache: {}", e))
}

// ============ Folder Contents Command ============

#[derive(Debug, Serialize, Deserialize)]
pub struct FolderContentsResponse {
    pub files: Vec<CachedFileResponse>,
    pub folders: Vec<String>,
}

/// Get folder contents from cache (files at this level + immediate subfolders)
/// This is the cache equivalent of S3 ListObjectsV2 with delimiter="/"
#[tauri::command]
pub async fn get_folder_contents(prefix: Option<String>) -> Result<FolderContentsResponse, String> {
    let (bucket, account_id) = get_current_bucket_info().await?;
    let prefix_str = prefix.unwrap_or_default();

    let result = db::get_folder_contents(&bucket, &account_id, &prefix_str)
        .await
        .map_err(|e| format!("Failed to get folder contents: {}", e))?;

    Ok(FolderContentsResponse {
        files: result.files.into_iter().map(|f| f.into()).collect(),
        folders: result.folders,
    })
}

// ============ URL Fetch Command ============

/// Fetch a URL and return the content as base64-encoded bytes.
/// This bypasses Tauri HTTP plugin scope restrictions by using reqwest directly.
#[tauri::command]
pub async fn fetch_url_bytes(url: String) -> Result<String, String> {
    let client = reqwest::Client::new();
    
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch URL: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "HTTP {}: {}",
            response.status().as_u16(),
            response.status().canonical_reason().unwrap_or("Unknown error")
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    Ok(BASE64.encode(&bytes))
}
