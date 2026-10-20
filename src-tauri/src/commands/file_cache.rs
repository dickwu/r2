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

#[derive(Debug, Serialize, Deserialize)]
pub struct BucketSummaryResponse {
    #[serde(rename = "totalFiles")]
    pub total_files: i64,
    #[serde(rename = "totalSize")]
    pub total_size: i64,
    #[serde(rename = "lastModified")]
    pub last_modified: Option<String>,
    #[serde(rename = "isComplete")]
    pub is_complete: bool,
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

async fn with_current_cache_scope<T, F>(
    operation: impl FnOnce(String, String) -> F,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    let config = db::get_current_config()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("No active configuration")?;
    let scope =
        db::cache_scope::CacheScope::capture(&db::cache_scope::CacheConfig::from_current(&config))
            .await
            .map_err(|e| e.to_string())?;
    db::cache_scope::in_scope(scope, operation(config.bucket, config.account_id)).await
}

// ============ Commands ============

#[tauri::command]
pub async fn store_all_files(
    files: Vec<r2::R2Object>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _ = (files, app);
    Err("Unscoped cache imports are no longer accepted; run a scoped bucket sync".into())
}

#[tauri::command]
pub async fn get_all_cached_files() -> Result<Vec<CachedFileResponse>, String> {
    with_current_cache_scope(|bucket, account_id| async move {
        let files = db::get_all_cached_files(&bucket, &account_id)
            .await
            .map_err(|e| format!("Failed to get cached files: {}", e))?;

        Ok(files.into_iter().map(|f| f.into()).collect())
    })
    .await
}

#[tauri::command]
pub async fn search_cached_files(query: String) -> Result<SearchResultResponse, String> {
    with_current_cache_scope(|bucket, account_id| async move {
        let result = db::search_cached_files(&bucket, &account_id, &query)
            .await
            .map_err(|e| format!("Failed to search files: {}", e))?;

        Ok(SearchResultResponse {
            files: result.files.into_iter().map(|f| f.into()).collect(),
            total_count: result.total_count,
        })
    })
    .await
}

/// Bucket-wide summary (total files + size) for the status bar.
/// Cheap: single-row directory-tree lookup after a full sync, falling back to
/// one SQL aggregate over the partial lazy cache — never a LIKE scan.
#[tauri::command]
pub async fn get_bucket_summary() -> Result<BucketSummaryResponse, String> {
    with_current_cache_scope(|bucket, account_id| async move {
        let summary = db::get_bucket_summary(&bucket, &account_id)
            .await
            .map_err(|e| format!("Failed to get bucket summary: {}", e))?;

        Ok(BucketSummaryResponse {
            total_files: summary.total_files,
            total_size: summary.total_size,
            last_modified: summary.last_modified,
            is_complete: summary.is_complete,
        })
    })
    .await
}

#[tauri::command]
pub async fn calculate_folder_size(prefix: String) -> Result<i64, String> {
    with_current_cache_scope(|bucket, account_id| async move {
        db::calculate_folder_size(&bucket, &account_id, &prefix)
            .await
            .map_err(|e| format!("Failed to calculate folder size: {}", e))
    })
    .await
}

#[tauri::command]
pub async fn build_directory_tree(app: tauri::AppHandle) -> Result<(), String> {
    let config = db::get_current_config()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("No active configuration")?;
    let scope =
        db::cache_scope::CacheScope::capture(&db::cache_scope::CacheConfig::from_current(&config))
            .await
            .map_err(|e| e.to_string())?;
    let bucket = config.bucket;
    let account_id = config.account_id;

    let _ = app.emit("sync-phase", "indexing");

    let app_clone = app.clone();
    let progress_callback = move |current: usize, total: usize| {
        let _ = app_clone.emit("indexing-progress", IndexingProgress { current, total });
    };

    // Build from DB instead of loading all files into memory
    db::cache_scope::in_scope(
        scope,
        db::build_directory_tree_from_db(&bucket, &account_id, &[], Some(progress_callback)),
    )
    .await
    .map_err(|e| format!("Failed to build directory tree: {}", e))?;

    let _ = app.emit("sync-phase", "complete");

    Ok(())
}

#[tauri::command]
pub async fn get_directory_node(path: String) -> Result<Option<DirectoryNodeResponse>, String> {
    with_current_cache_scope(|bucket, account_id| async move {
        let node = db::get_directory_node(&bucket, &account_id, &path)
            .await
            .map_err(|e| format!("Failed to get directory node: {}", e))?;

        Ok(node.map(|n| n.into()))
    })
    .await
}

/// Batch lookup: one IPC + one config resolve for a whole folder view's
/// subfolder nodes, instead of one round trip per subfolder. Result aligns
/// with `paths` (null where the tree has no node yet).
#[tauri::command]
pub async fn get_directory_nodes(
    paths: Vec<String>,
) -> Result<Vec<Option<DirectoryNodeResponse>>, String> {
    with_current_cache_scope(|bucket, account_id| async move {
        let nodes = db::get_directory_nodes(&bucket, &account_id, &paths)
            .await
            .map_err(|e| format!("Failed to get directory nodes: {}", e))?;

        Ok(nodes
            .into_iter()
            .map(|node| node.map(|n| n.into()))
            .collect())
    })
    .await
}

#[tauri::command]
pub async fn get_all_directory_nodes() -> Result<Vec<DirectoryNodeResponse>, String> {
    with_current_cache_scope(|bucket, account_id| async move {
        let nodes = db::get_all_directory_nodes(&bucket, &account_id)
            .await
            .map_err(|e| format!("Failed to get directory nodes: {}", e))?;

        Ok(nodes.into_iter().map(|n| n.into()).collect())
    })
    .await
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
    with_current_cache_scope(|bucket, account_id| async move {
        let prefix_str = prefix.unwrap_or_default();

        let result = db::get_folder_contents(&bucket, &account_id, &prefix_str)
            .await
            .map_err(|e| format!("Failed to get folder contents: {}", e))?;

        Ok(FolderContentsResponse {
            files: result.files.into_iter().map(|f| f.into()).collect(),
            folders: result.folders,
        })
    })
    .await
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
            response
                .status()
                .canonical_reason()
                .unwrap_or("Unknown error")
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    Ok(BASE64.encode(&bytes))
}

/// What an object is, going by its content: the type its first bytes
/// identify, as `mimetype-detector` reads them, and the Content-Type it was
/// stored with.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedType {
    /// MIME type identified from the content.
    pub mime: String,
    /// Extension that type is stored under, without the dot ("" if unknown).
    pub extension: String,
    /// Human-readable name of the type.
    pub name: String,
    /// The Content-Type header the object was stored with.
    pub content_type: Option<String>,
}

/// The most the detector reads; asking for more would only cost bandwidth.
const DETECT_BYTES: usize = 3072;

fn detect_client() -> Result<&'static reqwest::Client, String> {
    // Shared so the grid, which asks once per extension-less tile, reuses
    // connections instead of opening (and handshaking) one per object.
    // Bounded, so a stalled endpoint fails the preview instead of leaving it
    // waiting on the identification.
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(|e| format!("Cannot create HTTP client: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Identify the object at `url` from its first bytes: one ranged GET for at
/// most `DETECT_BYTES`, sniffed with `mimetype-detector`. It goes through
/// reqwest like `fetch_url_bytes`, because the HTTP plugin's URL scope only
/// admits hosts on their default port (self-hosted MinIO/RustFS listen on
/// others).
#[tauri::command]
pub async fn detect_url_type(url: String) -> Result<DetectedType, String> {
    let mut response = detect_client()?
        .get(&url)
        .header(
            reqwest::header::RANGE,
            format!("bytes=0-{}", DETECT_BYTES - 1),
        )
        .send()
        .await
        .map_err(|e| format!("Failed to fetch URL: {}", e))?;

    let status = response.status();
    let mut head = Vec::with_capacity(DETECT_BYTES);
    let content_type = if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // Only an empty object has no first byte; the detector names it.
        None
    } else if status.is_success() {
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        // A server that ignores the range sends the whole object: stop at
        // what the detector reads and drop the rest unread.
        while head.len() < DETECT_BYTES {
            match response
                .chunk()
                .await
                .map_err(|e| format!("Failed to read response body: {}", e))?
            {
                Some(chunk) => head.extend_from_slice(&chunk),
                None => break,
            }
        }
        head.truncate(DETECT_BYTES);
        // The cut may land inside a multi-byte character, and the detector
        // only takes text that is valid UTF-8 to its last byte.
        if let Err(err) = std::str::from_utf8(&head) {
            if head.len() == DETECT_BYTES && err.error_len().is_none() {
                head.truncate(err.valid_up_to());
            }
        }
        content_type
    } else {
        return Err(format!(
            "HTTP {}: {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("Unknown error")
        ));
    };

    let detected = mimetype_detector::detect(&head);
    Ok(DetectedType {
        mime: detected.mime().to_owned(),
        extension: detected.extension().trim_start_matches('.').to_owned(),
        name: detected.name().to_owned(),
        content_type,
    })
}

#[cfg(test)]
mod tests {
    use super::{detect_url_type, DETECT_BYTES};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    /// Answers one request with `head` then `body` and closes its side (the
    /// shared client would otherwise pool the connection); resolves to the
    /// request head, lower-cased.
    async fn serve_once(
        head: &'static str,
        body: Vec<u8>,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/bkt/6c4c4cb6?X-Amz-Signature=abc",
            listener.local_addr().unwrap()
        );
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 2048];
            while !request.windows(4).any(|s| s == b"\r\n\r\n") {
                let count = socket.read(&mut buffer).await.unwrap_or(0);
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&body).await;
            let _ = socket.shutdown().await;
            while socket.read(&mut buffer).await.unwrap_or(0) > 0 {}
            String::from_utf8_lossy(&request).to_lowercase()
        });
        (url, task)
    }

    #[tokio::test]
    async fn identifies_the_content_from_its_first_bytes() {
        let (url, server) = serve_once(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\n\
             Content-Range: bytes 0-15/4096\r\nContent-Length: 16\r\n\r\n",
            PNG_SIGNATURE.to_vec(),
        )
        .await;
        let detected = detect_url_type(url).await.unwrap();
        assert_eq!(detected.mime, "image/png");
        assert_eq!(detected.extension, "png");
        assert_eq!(
            detected.content_type.as_deref(),
            Some("application/octet-stream")
        );
        let request = server.await.unwrap();
        assert!(
            request.starts_with("get /bkt/6c4c4cb6?x-amz-signature=abc "),
            "{request}"
        );
        assert!(request.contains("\r\nrange: bytes=0-3071\r\n"), "{request}");
    }

    #[tokio::test]
    async fn text_cut_inside_a_character_by_the_range_is_still_text() {
        // Three-byte characters after one ASCII byte: the range ends inside one.
        let text = format!(
            "a{}",
            "存储桶中的对象没有扩展名时也应该可以预览。".repeat(100)
        );
        let body = text.as_bytes()[..DETECT_BYTES].to_vec();
        assert!(std::str::from_utf8(&body).is_err());
        let (url, server) = serve_once(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\n\
             Content-Range: bytes 0-3071/6301\r\nContent-Length: 3072\r\n\r\n",
            body,
        )
        .await;
        let detected = detect_url_type(url).await.unwrap();
        assert!(detected.mime.starts_with("text/plain"), "{}", detected.mime);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_empty_object_is_detected_as_empty() {
        let (url, server) = serve_once(
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Type: application/xml\r\n\
             Content-Length: 0\r\n\r\n",
            Vec::new(),
        )
        .await;
        let detected = detect_url_type(url).await.unwrap();
        assert_eq!(detected.mime, "application/x-empty");
        assert_eq!(detected.content_type, None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reports_an_error_status_instead_of_sniffing_the_error_body() {
        let (url, server) = serve_once(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/xml\r\n\
             Content-Length: 8\r\n\r\n",
            b"<Error/>".to_vec(),
        )
        .await;
        assert_eq!(
            detect_url_type(url).await.unwrap_err(),
            "HTTP 403: Forbidden"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reads_no_further_than_the_detector_when_the_range_is_ignored() {
        // The server announces the whole object but sends only part of it:
        // reading past the detector's bytes would fail on the early close.
        let mut body = PNG_SIGNATURE.to_vec();
        body.resize(DETECT_BYTES * 4, 0);
        let (url, server) = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n\
             Content-Length: 1000000000\r\n\r\n",
            body,
        )
        .await;
        let detected = tokio::time::timeout(Duration::from_secs(10), detect_url_type(url))
            .await
            .expect("returned without the rest of the body")
            .unwrap();
        assert_eq!(detected.mime, "image/png");
        server.await.unwrap();
    }
}
