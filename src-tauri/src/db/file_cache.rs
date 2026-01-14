use serde::{Deserialize, Serialize};
use super::{get_connection, DbResult};

// ============ File Cache Structs ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFile {
    pub bucket: String,
    pub account_id: String,
    pub key: String,
    pub parent_path: String,  // e.g., "" for root, "folder/" for files in folder/
    pub name: String,         // file name without path
    pub size: i64,
    pub last_modified: String,
    pub synced_at: i64,
}

/// Helper to extract parent path and name from a key
fn parse_key(key: &str) -> (String, String) {
    if let Some(last_slash) = key.rfind('/') {
        let parent = &key[..=last_slash]; // Include trailing slash
        let name = &key[last_slash + 1..];
        (parent.to_string(), name.to_string())
    } else {
        // Root level file
        (String::new(), key.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDirectoryNode {
    pub bucket: String,
    pub account_id: String,
    pub path: String,
    pub parent_path: String,
    pub file_count: i32,
    pub total_file_count: i32,
    pub size: i64,
    pub total_size: i64,
    pub last_modified: Option<String>,
    pub last_updated: i64,
}

/// Get SQL for creating file cache tables
pub fn get_table_sql() -> &'static str {
    "
    -- File cache tables (replaces IndexedDB)
    -- Drop old table to recreate with new schema
    DROP TABLE IF EXISTS cached_files;
    
    CREATE TABLE IF NOT EXISTS cached_files (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        key TEXT NOT NULL,
        parent_path TEXT NOT NULL,  -- Parent folder path (empty string for root)
        name TEXT NOT NULL,          -- File name without path
        size INTEGER NOT NULL,
        last_modified TEXT NOT NULL,
        synced_at INTEGER NOT NULL,
        PRIMARY KEY (bucket, account_id, key)
    );

    -- Drop old directory_tree to recreate with new schema
    DROP TABLE IF EXISTS directory_tree;
    
    CREATE TABLE IF NOT EXISTS directory_tree (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        path TEXT NOT NULL,
        parent_path TEXT NOT NULL,  -- Parent folder path for fast child lookup
        file_count INTEGER NOT NULL,
        total_file_count INTEGER NOT NULL,
        size INTEGER NOT NULL,
        total_size INTEGER NOT NULL,
        last_modified TEXT,
        last_updated INTEGER NOT NULL,
        PRIMARY KEY (bucket, account_id, path)
    );

    CREATE TABLE IF NOT EXISTS sync_meta (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        last_sync INTEGER NOT NULL,
        file_count INTEGER NOT NULL,
        PRIMARY KEY (bucket, account_id)
    );

    -- Index for fast folder listing (exact match on parent_path)
    CREATE INDEX IF NOT EXISTS idx_cached_files_parent ON cached_files(bucket, account_id, parent_path);
    CREATE INDEX IF NOT EXISTS idx_directory_tree_parent ON directory_tree(bucket, account_id, parent_path);
    "
}

// ============ File Cache Functions ============

/// Store all files for a bucket (clears existing) - optimized with batch inserts
pub async fn store_all_files(bucket: &str, account_id: &str, files: &[CachedFile]) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    
    // Begin transaction for atomicity and performance
    conn.execute("BEGIN TRANSACTION", ()).await?;
    
    // Clear existing files for this bucket
    conn.execute(
        "DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    ).await?;
    
    // Batch insert files - SQLite supports multi-row INSERT
    // Process in chunks of 500 to avoid hitting limits
    const BATCH_SIZE: usize = 500;
    
    for chunk in files.chunks(BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        
        // Build multi-value INSERT statement (8 columns now)
        let placeholders: Vec<String> = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let base = i * 8;
                format!(
                    "(?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{})",
                    base + 1, base + 2, base + 3, base + 4, base + 5, base + 6, base + 7, base + 8
                )
            })
            .collect();
        
        let sql = format!(
            "INSERT INTO cached_files (bucket, account_id, key, parent_path, name, size, last_modified, synced_at) VALUES {}",
            placeholders.join(", ")
        );
        
        // Build parameters array
        let mut params: Vec<turso::Value> = Vec::with_capacity(chunk.len() * 8);
        for file in chunk {
            // Compute parent_path and name from key
            let (parent_path, name) = parse_key(&file.key);
            
            params.push(bucket.to_string().into());
            params.push(account_id.to_string().into());
            params.push(file.key.clone().into());
            params.push(parent_path.into());
            params.push(name.into());
            params.push(file.size.into());
            params.push(file.last_modified.clone().into());
            params.push(file.synced_at.into());
        }
        
        conn.execute(&sql, params).await?;
    }
    
    // Update sync metadata
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO sync_meta (bucket, account_id, last_sync, file_count)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (bucket, account_id) DO UPDATE SET last_sync = ?3, file_count = ?4",
        turso::params![bucket, account_id, now, files.len() as i32],
    ).await?;
    
    // Commit transaction
    conn.execute("COMMIT", ()).await?;
    
    Ok(())
}

/// Get all cached files for a bucket
pub async fn get_all_cached_files(bucket: &str, account_id: &str) -> DbResult<Vec<CachedFile>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT bucket, account_id, key, parent_path, name, size, last_modified, synced_at
         FROM cached_files
         WHERE bucket = ?1 AND account_id = ?2
         ORDER BY key",
        turso::params![bucket, account_id]
    ).await?;
    
    let mut files = Vec::new();
    while let Some(row) = rows.next().await? {
        files.push(CachedFile {
            bucket: row.get(0)?,
            account_id: row.get(1)?,
            key: row.get(2)?,
            parent_path: row.get(3)?,
            name: row.get(4)?,
            size: row.get(5)?,
            last_modified: row.get(6)?,
            synced_at: row.get(7)?,
        });
    }
    Ok(files)
}

/// Get a single file's size from cache (returns 0 if not found)
pub async fn get_cached_file_size(bucket: &str, account_id: &str, key: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT size FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key = ?3",
        turso::params![bucket, account_id, key]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Delete a single cached file.
/// Returns the file's size for directory tree updates (negative delta).
pub async fn delete_cached_file(
    bucket: &str,
    account_id: &str,
    key: &str,
) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    
    // Get file size before deleting
    let mut rows = conn.query(
        "SELECT size FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key = ?3",
        turso::params![bucket, account_id, key]
    ).await?;
    
    let size: i64 = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Ok(0); // File not in cache
    };
    drop(rows);
    
    // Delete the file record
    conn.execute(
        "DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key = ?3",
        turso::params![bucket, account_id, key],
    ).await?;
    
    Ok(size)
}

/// Delete multiple cached files in batch.
/// Returns a map of key -> size for directory tree updates.
pub async fn delete_cached_files_batch(
    bucket: &str,
    account_id: &str,
    keys: &[String],
) -> DbResult<std::collections::HashMap<String, i64>> {
    if keys.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    
    let conn = get_connection()?.lock().await;
    let mut file_sizes: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    
    // Get sizes for all files
    for chunk in keys.chunks(500) {
        let placeholders: Vec<String> = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 3))
            .collect();
        
        let sql = format!(
            "SELECT key, size FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key IN ({})",
            placeholders.join(", ")
        );
        
        let mut params: Vec<turso::Value> = Vec::new();
        params.push(bucket.to_string().into());
        params.push(account_id.to_string().into());
        for key in chunk {
            params.push(key.clone().into());
        }
        
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            let key: String = row.get(0)?;
            let size: i64 = row.get(1)?;
            file_sizes.insert(key, size);
        }
    }
    
    // Delete all files
    for chunk in keys.chunks(500) {
        let placeholders: Vec<String> = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 3))
            .collect();
        
        let sql = format!(
            "DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key IN ({})",
            placeholders.join(", ")
        );
        
        let mut params: Vec<turso::Value> = Vec::new();
        params.push(bucket.to_string().into());
        params.push(account_id.to_string().into());
        for key in chunk {
            params.push(key.clone().into());
        }
        
        conn.execute(&sql, params).await?;
    }
    
    Ok(file_sizes)
}

/// Move/rename a cached file to a new key.
/// Returns (size, last_modified) for directory tree updates.
pub async fn move_cached_file(
    bucket: &str,
    account_id: &str,
    old_key: &str,
    new_key: &str,
) -> DbResult<Option<(i64, String)>> {
    let conn = get_connection()?.lock().await;
    
    // Get file info
    let mut rows = conn.query(
        "SELECT size, last_modified FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key = ?3",
        turso::params![bucket, account_id, old_key]
    ).await?;
    
    let file_info = if let Some(row) = rows.next().await? {
        let size: i64 = row.get(0)?;
        let last_modified: String = row.get(1)?;
        Some((size, last_modified))
    } else {
        return Ok(None); // File not in cache
    };
    drop(rows);
    
    // Compute new parent_path and name
    let (new_parent_path, new_name) = parse_key(new_key);
    let now = chrono::Utc::now().timestamp();
    
    // Update the file record with new key, parent_path, and name
    conn.execute(
        "UPDATE cached_files SET key = ?1, parent_path = ?2, name = ?3, synced_at = ?4
         WHERE bucket = ?5 AND account_id = ?6 AND key = ?7",
        turso::params![new_key, new_parent_path, new_name, now, bucket, account_id, old_key],
    ).await?;
    
    Ok(file_info)
}

/// Update a single cached file's size and last_modified.
/// Returns the size difference (new_size - old_size) for directory tree updates.
pub async fn update_cached_file(
    bucket: &str,
    account_id: &str,
    key: &str,
    new_size: i64,
    last_modified: &str,
) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    
    // Get old size for delta calculation
    let mut rows = conn.query(
        "SELECT size FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key = ?3",
        turso::params![bucket, account_id, key]
    ).await?;
    
    let old_size: i64 = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        0
    };
    drop(rows);
    
    let now = chrono::Utc::now().timestamp();
    
    // Update the file record
    conn.execute(
        "UPDATE cached_files SET size = ?1, last_modified = ?2, synced_at = ?3
         WHERE bucket = ?4 AND account_id = ?5 AND key = ?6",
        turso::params![new_size, last_modified, now, bucket, account_id, key],
    ).await?;
    
    Ok(new_size - old_size)
}

/// Search result with total count
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub files: Vec<CachedFile>,
    pub total_count: i32,
}

/// Search cached files by key pattern (case-insensitive)
/// Supports multiple terms separated by spaces (AND search)
/// e.g., "test name" matches files containing both "test" AND "name"
pub async fn search_cached_files(bucket: &str, account_id: &str, query: &str) -> DbResult<SearchResult> {
    let conn = get_connection()?.lock().await;
    
    // Split query into terms and create LIKE conditions for each
    let terms: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();
    
    if terms.is_empty() {
        return Ok(SearchResult { files: Vec::new(), total_count: 0 });
    }
    
    // Build WHERE clause with AND for each term
    let like_conditions: Vec<String> = terms
        .iter()
        .enumerate()
        .map(|(i, _)| format!("LOWER(key) LIKE ?{}", i + 3))
        .collect();
    
    let where_clause = like_conditions.join(" AND ");
    let sql = format!(
        "SELECT bucket, account_id, key, parent_path, name, size, last_modified, synced_at
         FROM cached_files
         WHERE bucket = ?1 AND account_id = ?2 AND {}
         ORDER BY key",
        where_clause
    );
    
    // Build params: bucket, account_id, then patterns for each term
    let mut params: Vec<turso::Value> = Vec::new();
    params.push(bucket.to_string().into());
    params.push(account_id.to_string().into());
    for term in &terms {
        params.push(format!("%{}%", term.to_lowercase()).into());
    }
    
    let mut rows = conn.query(&sql, params).await?;
    
    let mut files = Vec::new();
    while let Some(row) = rows.next().await? {
        files.push(CachedFile {
            bucket: row.get(0)?,
            account_id: row.get(1)?,
            key: row.get(2)?,
            parent_path: row.get(3)?,
            name: row.get(4)?,
            size: row.get(5)?,
            last_modified: row.get(6)?,
            synced_at: row.get(7)?,
        });
    }
    
    let total_count = files.len() as i32;
    Ok(SearchResult { files, total_count })
}

/// Calculate folder size by prefix
pub async fn calculate_folder_size(bucket: &str, account_id: &str, prefix: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    let pattern = format!("{}%", prefix);
    
    let mut rows = conn.query(
        "SELECT COALESCE(SUM(size), 0) FROM cached_files
         WHERE bucket = ?1 AND account_id = ?2 AND key LIKE ?3",
        turso::params![bucket, account_id, pattern]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        let total: i64 = row.get(0)?;
        Ok(total)
    } else {
        Ok(0)
    }
}

/// Get directory node by path
pub async fn get_directory_node(bucket: &str, account_id: &str, path: &str) -> DbResult<Option<CachedDirectoryNode>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated
         FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2 AND path = ?3",
        turso::params![bucket, account_id, path]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(CachedDirectoryNode {
            bucket: row.get(0)?,
            account_id: row.get(1)?,
            path: row.get(2)?,
            parent_path: row.get(3)?,
            file_count: row.get(4)?,
            total_file_count: row.get(5)?,
            size: row.get(6)?,
            total_size: row.get(7)?,
            last_modified: row.get(8)?,
            last_updated: row.get(9)?,
        }))
    } else {
        Ok(None)
    }
}

/// Get all directory nodes for a bucket
pub async fn get_all_directory_nodes(bucket: &str, account_id: &str) -> DbResult<Vec<CachedDirectoryNode>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated
         FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2
         ORDER BY path",
        turso::params![bucket, account_id]
    ).await?;
    
    let mut nodes = Vec::new();
    while let Some(row) = rows.next().await? {
        nodes.push(CachedDirectoryNode {
            bucket: row.get(0)?,
            account_id: row.get(1)?,
            path: row.get(2)?,
            parent_path: row.get(3)?,
            file_count: row.get(4)?,
            total_file_count: row.get(5)?,
            size: row.get(6)?,
            total_size: row.get(7)?,
            last_modified: row.get(8)?,
            last_updated: row.get(9)?,
        });
    }
    Ok(nodes)
}

/// Clear all cached data for a bucket
pub async fn clear_file_cache(bucket: &str, account_id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    
    conn.execute(
        "DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    ).await?;
    
    conn.execute(
        "DELETE FROM directory_tree WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    ).await?;
    
    conn.execute(
        "DELETE FROM sync_meta WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    ).await?;
    
    Ok(())
}

/// Result of get_folder_contents
#[derive(Debug, Clone)]
pub struct FolderContents {
    pub files: Vec<CachedFile>,
    pub folders: Vec<String>,
}

/// Get folder contents from cache (files at this level + immediate subfolders)
/// This mimics S3 ListObjectsV2 with delimiter="/" behavior
/// 
/// FAST: Uses exact match on parent_path (indexed) instead of LIKE patterns
pub async fn get_folder_contents(bucket: &str, account_id: &str, prefix: &str) -> DbResult<FolderContents> {
    let conn = get_connection()?.lock().await;
    
    // Query 1: Get files directly in this folder using EXACT MATCH on parent_path
    // This is O(1) index lookup instead of O(n) LIKE scan
    let mut rows = conn.query(
        "SELECT bucket, account_id, key, parent_path, name, size, last_modified, synced_at
         FROM cached_files
         WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3
         ORDER BY name",
        turso::params![bucket, account_id, prefix]
    ).await?;
    
    let mut files = Vec::new();
    while let Some(row) = rows.next().await? {
        files.push(CachedFile {
            bucket: row.get(0)?,
            account_id: row.get(1)?,
            key: row.get(2)?,
            parent_path: row.get(3)?,
            name: row.get(4)?,
            size: row.get(5)?,
            last_modified: row.get(6)?,
            synced_at: row.get(7)?,
        });
    }
    
    // Query 2: Get immediate child folders from directory_tree using EXACT MATCH on parent_path
    // This is O(1) index lookup instead of O(n) LIKE scan
    let mut rows = conn.query(
        "SELECT path FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3
         ORDER BY path",
        turso::params![bucket, account_id, prefix]
    ).await?;
    
    let mut folders = Vec::new();
    while let Some(row) = rows.next().await? {
        let folder: String = row.get(0)?;
        folders.push(folder);
    }
    
    Ok(FolderContents {
        files,
        folders,
    })
}
