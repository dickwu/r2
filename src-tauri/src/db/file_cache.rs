use serde::{Deserialize, Serialize};
use super::{get_connection, DbResult};

// ============ File Cache Structs ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFile {
    pub bucket: String,
    pub account_id: String,
    pub key: String,
    pub size: i64,
    pub last_modified: String,
    pub synced_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDirectoryNode {
    pub bucket: String,
    pub account_id: String,
    pub path: String,
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
    CREATE TABLE IF NOT EXISTS cached_files (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        key TEXT NOT NULL,
        size INTEGER NOT NULL,
        last_modified TEXT NOT NULL,
        synced_at INTEGER NOT NULL,
        PRIMARY KEY (bucket, account_id, key)
    );

    CREATE TABLE IF NOT EXISTS directory_tree (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        path TEXT NOT NULL,
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

    CREATE INDEX IF NOT EXISTS idx_cached_files_prefix ON cached_files(bucket, account_id, key);
    CREATE INDEX IF NOT EXISTS idx_directory_tree_lookup ON directory_tree(bucket, account_id, path);
    "
}

// ============ File Cache Functions ============

/// Store all files for a bucket (clears existing)
pub async fn store_all_files(bucket: &str, account_id: &str, files: &[CachedFile]) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    
    // Clear existing files for this bucket
    conn.execute(
        "DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    ).await?;
    
    // Insert new files
    for file in files {
        conn.execute(
            "INSERT INTO cached_files (bucket, account_id, key, size, last_modified, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            turso::params![
                bucket,
                account_id,
                file.key.clone(),
                file.size,
                file.last_modified.clone(),
                file.synced_at,
            ],
        ).await?;
    }
    
    // Update sync metadata
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO sync_meta (bucket, account_id, last_sync, file_count)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (bucket, account_id) DO UPDATE SET last_sync = ?3, file_count = ?4",
        turso::params![bucket, account_id, now, files.len() as i32],
    ).await?;
    
    Ok(())
}

/// Get all cached files for a bucket
pub async fn get_all_cached_files(bucket: &str, account_id: &str) -> DbResult<Vec<CachedFile>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT bucket, account_id, key, size, last_modified, synced_at
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
            size: row.get(3)?,
            last_modified: row.get(4)?,
            synced_at: row.get(5)?,
        });
    }
    Ok(files)
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

/// Build directory tree from files (same algorithm as indexeddb.ts)
pub async fn build_directory_tree(bucket: &str, account_id: &str, files: &[CachedFile]) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    
    // Clear existing tree for this bucket
    conn.execute(
        "DELETE FROM directory_tree WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    ).await?;
    
    // Build directory map
    use std::collections::{HashMap, HashSet};
    let mut dir_map: HashMap<String, (Vec<&CachedFile>, HashSet<String>)> = HashMap::new();
    
    // Initialize root directory
    dir_map.insert(String::new(), (Vec::new(), HashSet::new()));
    
    // Extract all unique directories from file paths
    for file in files {
        let parts: Vec<&str> = file.key.split('/').collect();
        
        // Handle root-level files (no directory)
        if parts.len() == 1 {
            dir_map.get_mut("").unwrap().0.push(file);
            continue;
        }
        
        // Traverse each directory level
        for i in 0..parts.len() - 1 {
            // Build path from root to current level
            let current_path = if i == 0 {
                format!("{}/", parts[0])
            } else {
                format!("{}/", parts[0..=i].join("/"))
            };
            
            let prev_path = if i > 0 {
                format!("{}/", parts[0..i].join("/"))
            } else {
                String::new()
            };
            
            if !dir_map.contains_key(&current_path) {
                dir_map.insert(current_path.clone(), (Vec::new(), HashSet::new()));
            }
            
            // Track parent-child relationship
            let parent_dir = prev_path;
            if let Some(parent) = dir_map.get_mut(&parent_dir) {
                parent.1.insert(current_path.clone());
            }
            
            // Add file to its direct parent directory
            if i == parts.len() - 2 {
                dir_map.get_mut(&current_path).unwrap().0.push(file);
            }
        }
    }
    
    // Calculate sizes and counts (bottom-up)
    let mut sorted_dirs: Vec<_> = dir_map.keys().cloned().collect();
    sorted_dirs.sort_by(|a, b| b.split('/').count().cmp(&a.split('/').count()));
    
    let mut node_map: HashMap<String, CachedDirectoryNode> = HashMap::new();
    let now = chrono::Utc::now().timestamp();
    
    for path in sorted_dirs {
        let (files, subdirs) = dir_map.get(&path).unwrap();
        
        // Direct files in this directory
        let direct_size: i64 = files.iter().map(|f| f.size).sum();
        let direct_count = files.len() as i32;
        
        // Find max last_modified from direct files
        let direct_last_modified: Option<&str> = files.iter()
            .map(|f| f.last_modified.as_str())
            .max();
        
        // Aggregate from subdirectories
        let mut sub_size: i64 = 0;
        let mut sub_count: i32 = 0;
        let mut sub_last_modified: Option<String> = None;
        for subdir in subdirs {
            if let Some(sub_node) = node_map.get(subdir) {
                sub_size += sub_node.total_size;
                sub_count += sub_node.total_file_count;
                // Track max last_modified from subdirs
                if let Some(ref sub_lm) = sub_node.last_modified {
                    sub_last_modified = match sub_last_modified {
                        None => Some(sub_lm.clone()),
                        Some(ref current) if sub_lm > current => Some(sub_lm.clone()),
                        other => other,
                    };
                }
            }
        }
        
        // Combine direct and subdirectory last_modified (take the max)
        let last_modified = match (direct_last_modified, sub_last_modified) {
            (Some(d), Some(s)) => Some(if d > s.as_str() { d.to_string() } else { s }),
            (Some(d), None) => Some(d.to_string()),
            (None, Some(s)) => Some(s),
            (None, None) => None,
        };
        
        let node = CachedDirectoryNode {
            bucket: bucket.to_string(),
            account_id: account_id.to_string(),
            path: path.clone(),
            file_count: direct_count,
            total_file_count: direct_count + sub_count,
            size: direct_size,
            total_size: direct_size + sub_size,
            last_modified: last_modified.clone(),
            last_updated: now,
        };
        
        // Store in database
        conn.execute(
            "INSERT INTO directory_tree 
             (bucket, account_id, path, file_count, total_file_count, size, total_size, last_modified, last_updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            turso::params![
                node.bucket.clone(),
                node.account_id.clone(),
                node.path.clone(),
                node.file_count,
                node.total_file_count,
                node.size,
                node.total_size,
                last_modified,
                node.last_updated,
            ],
        ).await?;
        
        node_map.insert(path, node);
    }
    
    Ok(())
}

/// Get directory node by path
pub async fn get_directory_node(bucket: &str, account_id: &str, path: &str) -> DbResult<Option<CachedDirectoryNode>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT bucket, account_id, path, file_count, total_file_count, size, total_size, last_modified, last_updated
         FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2 AND path = ?3",
        turso::params![bucket, account_id, path]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(CachedDirectoryNode {
            bucket: row.get(0)?,
            account_id: row.get(1)?,
            path: row.get(2)?,
            file_count: row.get(3)?,
            total_file_count: row.get(4)?,
            size: row.get(5)?,
            total_size: row.get(6)?,
            last_modified: row.get(7)?,
            last_updated: row.get(8)?,
        }))
    } else {
        Ok(None)
    }
}

/// Get all directory nodes for a bucket
pub async fn get_all_directory_nodes(bucket: &str, account_id: &str) -> DbResult<Vec<CachedDirectoryNode>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT bucket, account_id, path, file_count, total_file_count, size, total_size, last_modified, last_updated
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
            file_count: row.get(3)?,
            total_file_count: row.get(4)?,
            size: row.get(5)?,
            total_size: row.get(6)?,
            last_modified: row.get(7)?,
            last_updated: row.get(8)?,
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
