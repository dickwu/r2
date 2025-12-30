use crate::db::{self, CachedDirectoryNode, CachedFile};
use crate::r2;
use serde::{Deserialize, Serialize};
use tauri::Emitter;

// ============ R2 Commands ============

#[derive(Debug, Deserialize)]
pub struct R2ConfigInput {
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl From<R2ConfigInput> for r2::R2Config {
    fn from(input: R2ConfigInput) -> Self {
        r2::R2Config {
            account_id: input.account_id,
            bucket: input.bucket,
            access_key_id: input.access_key_id,
            secret_access_key: input.secret_access_key,
        }
    }
}

#[tauri::command]
pub async fn list_r2_buckets(account_id: String, access_key_id: String, secret_access_key: String) -> Result<Vec<r2::R2Bucket>, String> {
    let config = r2::R2Config {
        account_id,
        bucket: String::new(), // Not needed for listing buckets
        access_key_id,
        secret_access_key,
    };
    
    r2::list_buckets(&config)
        .await
        .map_err(|e| format!("Failed to list buckets: {}", e))
}

#[derive(Debug, Deserialize)]
pub struct ListObjectsInput {
    pub config: R2ConfigInput,
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    pub continuation_token: Option<String>,
    pub max_keys: Option<i32>,
}

#[tauri::command]
pub async fn list_r2_objects(input: ListObjectsInput) -> Result<r2::ListObjectsResult, String> {
    let config: r2::R2Config = input.config.into();
    
    r2::list_objects(
        &config,
        input.prefix.as_deref(),
        input.delimiter.as_deref(),
        input.continuation_token.as_deref(),
        input.max_keys,
    )
    .await
    .map_err(|e| format!("Failed to list objects: {}", e))
}

#[tauri::command]
pub async fn list_all_r2_objects(
    config: R2ConfigInput,
    app: tauri::AppHandle,
) -> Result<Vec<r2::R2Object>, String> {
    let r2_config: r2::R2Config = config.into();
    
    // Emit fetching phase at start
    let _ = app.emit("sync-phase", "fetching");
    
    // Create progress callback that emits Tauri events
    let app_clone = app.clone();
    let progress_callback = Box::new(move |count: usize| {
        let _ = app_clone.emit("sync-progress", count);
    });
    
    r2::list_all_objects_recursive(&r2_config, Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to list all objects: {}", e))
}

#[tauri::command]
pub async fn delete_r2_object(config: R2ConfigInput, key: String) -> Result<(), String> {
    let r2_config: r2::R2Config = config.into();
    
    r2::delete_object(&r2_config, &key)
        .await
        .map_err(|e| format!("Failed to delete object: {}", e))
}

#[tauri::command]
pub async fn rename_r2_object(config: R2ConfigInput, old_key: String, new_key: String) -> Result<(), String> {
    let r2_config: r2::R2Config = config.into();
    
    r2::rename_object(&r2_config, &old_key, &new_key)
        .await
        .map_err(|e| format!("Failed to rename object: {}", e))
}

#[tauri::command]
pub async fn generate_signed_url(config: R2ConfigInput, key: String, expires_in: Option<u64>) -> Result<String, String> {
    let r2_config: r2::R2Config = config.into();
    let expires_in_secs = expires_in.unwrap_or(3600); // Default 1 hour
    
    r2::generate_presigned_url(&r2_config, &key, expires_in_secs)
        .await
        .map_err(|e| format!("Failed to generate signed URL: {}", e))
}

// ============ Cache Commands ============

/// Frontend-friendly cached file format
#[derive(Debug, Serialize, Deserialize)]
pub struct CachedFileResponse {
    pub key: String,
    pub size: i64,
    #[serde(rename = "lastModified")]
    pub last_modified: String,
}

/// Frontend-friendly directory node format
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

/// Get current bucket and account_id from config
async fn get_current_bucket_info() -> Result<(String, String), String> {
    let config = db::get_current_config()
        .await
        .map_err(|e| format!("Failed to get current config: {}", e))?
        .ok_or_else(|| "No active configuration".to_string())?;
    
    Ok((config.bucket, config.account_id))
}

#[tauri::command]
pub async fn store_all_files(
    files: Vec<r2::R2Object>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let (bucket, account_id) = get_current_bucket_info().await?;
    let now = chrono::Utc::now().timestamp();
    
    // Emit storing phase
    let _ = app.emit("sync-phase", "storing");
    
    let cached_files: Vec<CachedFile> = files
        .into_iter()
        .map(|f| CachedFile {
            bucket: bucket.clone(),
            account_id: account_id.clone(),
            key: f.key,
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

/// Search result response with total count
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResultResponse {
    pub files: Vec<CachedFileResponse>,
    #[serde(rename = "totalCount")]
    pub total_count: i32,
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

/// Indexing progress payload
#[derive(Debug, Clone, Serialize)]
struct IndexingProgress {
    current: usize,
    total: usize,
}

#[tauri::command]
pub async fn build_directory_tree(app: tauri::AppHandle) -> Result<(), String> {
    let (bucket, account_id) = get_current_bucket_info().await?;
    
    // Emit indexing phase
    let _ = app.emit("sync-phase", "indexing");
    
    // Get all cached files
    let files = db::get_all_cached_files(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to get cached files: {}", e))?;
    
    // Build tree with progress callback
    let app_clone = app.clone();
    let progress_callback = move |current: usize, total: usize| {
        let _ = app_clone.emit("indexing-progress", IndexingProgress { current, total });
    };
    
    db::build_directory_tree(&bucket, &account_id, &files, Some(progress_callback))
        .await
        .map_err(|e| format!("Failed to build directory tree: {}", e))?;
    
    // Emit complete phase
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
