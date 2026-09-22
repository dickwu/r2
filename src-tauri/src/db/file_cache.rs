use super::DbResult;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

static LOCAL_MUTATION_TOKEN: AtomicU64 = AtomicU64::new(1);
const LOCAL_MUTATION_STALE_SECS: i64 = 300;

// ============ File Cache Structs ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFile {
    pub bucket: String,
    pub account_id: String,
    pub key: String,
    pub parent_path: String, // e.g., "" for root, "folder/" for files in folder/
    pub name: String,        // file name without path
    pub size: i64,
    pub last_modified: String,
    pub synced_at: i64,
}

/// Helper to extract parent path and name from a key
pub fn parse_key(key: &str) -> (String, String) {
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

    CREATE TABLE IF NOT EXISTS cached_files_staging (
        bucket TEXT NOT NULL, account_id TEXT NOT NULL, key TEXT NOT NULL,
        parent_path TEXT NOT NULL, name TEXT NOT NULL, size INTEGER NOT NULL,
        last_modified TEXT NOT NULL, synced_at INTEGER NOT NULL,
        PRIMARY KEY(bucket, account_id, key)
    );

    CREATE TABLE IF NOT EXISTS sync_meta (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        last_sync INTEGER NOT NULL,
        file_count INTEGER NOT NULL,
        generation INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (bucket, account_id)
    );

    CREATE TABLE IF NOT EXISTS bucket_content_revisions (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        revision INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (bucket, account_id)
    );

    -- Index for fast folder listing (exact match on parent_path)
    CREATE INDEX IF NOT EXISTS idx_cached_files_parent ON cached_files(bucket, account_id, parent_path);
    CREATE INDEX IF NOT EXISTS idx_directory_tree_parent ON directory_tree(bucket, account_id, parent_path);
    "
}

// ============ File Cache Functions ============

pub(crate) async fn content_revision_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
) -> DbResult<i64> {
    let mut rows = conn
        .query(
            "SELECT revision FROM bucket_content_revisions WHERE bucket = ?1 AND account_id = ?2",
            turso::params![bucket, account_id],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    })
}

fn local_mutation_barrier_key(bucket: &str, account_id: &str) -> String {
    format!("cache_mutation_in_progress:{account_id}:{bucket}")
}

fn sync_base_revision_key(bucket: &str, account_id: &str) -> String {
    format!("sync_base_revision:{account_id}:{bucket}")
}

fn sync_active_run_key(bucket: &str, account_id: &str) -> String {
    format!("sync_active_run:{account_id}:{bucket}")
}

fn next_sync_run_token() -> String {
    let id = LOCAL_MUTATION_TOKEN.fetch_add(1, Ordering::SeqCst);
    format!(
        "sync:{}:{}:{id}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalMutationBarrier {
    token: String,
    started_at: i64,
}

fn next_local_mutation_token() -> String {
    let id = LOCAL_MUTATION_TOKEN.fetch_add(1, Ordering::SeqCst);
    format!(
        "{}:{}:{id}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )
}

fn parse_barriers(value: &str) -> Vec<LocalMutationBarrier> {
    serde_json::from_str(value).unwrap_or_default()
}

pub async fn begin_local_cache_mutation(bucket: &str, account_id: &str) -> DbResult<String> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    let token = next_local_mutation_token();
    let key = local_mutation_barrier_key(bucket, account_id);
    let mut barriers = {
        let mut rows = conn
            .query(
                "SELECT value FROM app_state WHERE key = ?1",
                turso::params![key.clone()],
            )
            .await?;
        match rows.next().await? {
            Some(row) => parse_barriers(&row.get::<String>(0)?),
            None => Vec::new(),
        }
    };
    barriers.push(LocalMutationBarrier {
        token: token.clone(),
        started_at: chrono::Utc::now().timestamp(),
    });
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        turso::params![key, serde_json::to_string(&barriers)?],
    )
    .await?;
    Ok(token)
}

pub async fn finish_local_cache_mutation(
    bucket: &str,
    account_id: &str,
    token: &str,
) -> DbResult<()> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    let key = local_mutation_barrier_key(bucket, account_id);
    let mut rows = conn
        .query(
            "SELECT value FROM app_state WHERE key = ?1",
            turso::params![key.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(());
    };
    let mut barriers = parse_barriers(&row.get::<String>(0)?);
    drop(rows);
    barriers.retain(|barrier| barrier.token != token);
    if barriers.is_empty() {
        conn.execute("DELETE FROM app_state WHERE key = ?1", turso::params![key])
            .await?;
    } else {
        conn.execute(
            "UPDATE app_state SET value = ?2 WHERE key = ?1",
            turso::params![key, serde_json::to_string(&barriers)?],
        )
        .await?;
    }
    Ok(())
}

async fn invalidate_stale_barrier_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    key: &str,
) -> DbResult<()> {
    conn.execute(
        "DELETE FROM app_state WHERE key = ?1",
        turso::params![key.to_string()],
    )
    .await?;
    conn.execute(
        "DELETE FROM sync_meta WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;
    conn.execute(
        "DELETE FROM prefix_sync_times WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;
    bump_content_revision_on(conn, bucket, account_id).await?;
    Ok(())
}

pub(crate) async fn ensure_no_local_cache_mutation_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
) -> DbResult<()> {
    let key = local_mutation_barrier_key(bucket, account_id);
    let mut rows = conn
        .query(
            "SELECT value FROM app_state WHERE key = ?1 LIMIT 1",
            turso::params![key.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(());
    };
    let barriers = parse_barriers(&row.get::<String>(0)?);
    drop(rows);
    let now = chrono::Utc::now().timestamp();
    if barriers.is_empty()
        || barriers
            .iter()
            .all(|barrier| now.saturating_sub(barrier.started_at) > LOCAL_MUTATION_STALE_SECS)
    {
        invalidate_stale_barrier_on(conn, bucket, account_id, &key).await?;
        return Ok(());
    }
    Err("Cache mutation is in progress".into())
}

pub(crate) async fn bump_content_revision_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
) -> DbResult<i64> {
    conn.execute(
        "INSERT INTO bucket_content_revisions (bucket, account_id, revision)
         VALUES (?1, ?2, 1)
         ON CONFLICT(bucket, account_id) DO UPDATE SET
           revision = bucket_content_revisions.revision + 1",
        turso::params![bucket, account_id],
    )
    .await?;
    content_revision_on(conn, bucket, account_id).await
}

/// Get all cached files for a bucket
pub async fn get_all_cached_files(bucket: &str, account_id: &str) -> DbResult<Vec<CachedFile>> {
    let conn = super::cache_scope::read_connection(account_id).await?;
    let mut rows = conn
        .query(
            "SELECT bucket, account_id, key, parent_path, name, size, last_modified, synced_at
         FROM cached_files
         WHERE bucket = ?1 AND account_id = ?2
         ORDER BY key",
            turso::params![bucket, account_id],
        )
        .await?;

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
    let conn = super::cache_scope::read_connection(account_id).await?;
    let mut rows = conn
        .query(
            "SELECT size FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key = ?3",
            turso::params![bucket, account_id, key],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Delete a single cached file.
/// Returns the file's size for directory tree updates (negative delta).
/// None means the file was not found in cache.
pub async fn delete_cached_file(
    bucket: &str,
    account_id: &str,
    key: &str,
) -> DbResult<Option<i64>> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    conn.execute("BEGIN TRANSACTION", ()).await?;

    let result = async {
        let mut rows = conn
            .query(
                "SELECT size FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key = ?3",
                turso::params![bucket, account_id, key],
            )
            .await?;

        let size: i64 = if let Some(row) = rows.next().await? {
            row.get(0)?
        } else {
            return Ok(None);
        };
        drop(rows);

        conn.execute(
            "DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key = ?3",
            turso::params![bucket, account_id, key],
        )
        .await?;
        let (parent_path, _) = parse_key(key);
        super::prefix_sync::invalidate_prefixes_on(&conn, bucket, account_id, &[parent_path])
            .await?;
        bump_content_revision_on(&conn, bucket, account_id).await?;

        Ok(Some(size))
    }
    .await;
    match result {
        Ok(value) => {
            conn.execute("COMMIT", ()).await?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
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

    let conn = super::cache_scope::write_connection(account_id).await?;
    conn.execute("BEGIN TRANSACTION", ()).await?;
    let result = async {
        let mut file_sizes: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();

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

        let mut prefixes: Vec<String> = keys.iter().map(|key| parse_key(key).0).collect();
        prefixes.sort();
        prefixes.dedup();
        super::prefix_sync::invalidate_prefixes_on(&conn, bucket, account_id, &prefixes).await?;
        bump_content_revision_on(&conn, bucket, account_id).await?;

        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(file_sizes)
    }
    .await;
    match result {
        Ok(value) => {
            conn.execute("COMMIT", ()).await?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
}

/// Move/rename a cached file to a new key.
/// Returns (size, last_modified) for directory tree updates.
pub async fn move_cached_file(
    bucket: &str,
    account_id: &str,
    old_key: &str,
    new_key: &str,
) -> DbResult<Option<(i64, String)>> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    conn.execute("BEGIN TRANSACTION", ()).await?;

    let result = async {
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
            return Ok(None);
        };
        drop(rows);

        // Compute new parent_path and name
        let (old_parent_path, _) = parse_key(old_key);
        let (new_parent_path, new_name) = parse_key(new_key);
        let now = chrono::Utc::now().timestamp();

        // Update the file record with new key, parent_path, and name
        conn.execute(
        "UPDATE cached_files SET key = ?1, parent_path = ?2, name = ?3, synced_at = ?4
         WHERE bucket = ?5 AND account_id = ?6 AND key = ?7",
        turso::params![
            new_key,
            new_parent_path.clone(),
            new_name,
            now,
            bucket,
            account_id,
            old_key
        ],
    )
    .await?;

        let mut prefixes = vec![old_parent_path, new_parent_path];
        prefixes.sort();
        prefixes.dedup();
        super::prefix_sync::invalidate_prefixes_on(&conn, bucket, account_id, &prefixes).await?;
        bump_content_revision_on(&conn, bucket, account_id).await?;

        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(file_info)
    }
    .await;
    match result {
        Ok(value) => {
            conn.execute("COMMIT", ()).await?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
}

/// Update or insert a single cached file.
/// Returns (size_delta, is_new_file) for directory tree updates.
/// - size_delta: new_size - old_size (or new_size if new file)
/// - is_new_file: true if this was an insert, false if update
pub async fn update_cached_file(
    bucket: &str,
    account_id: &str,
    key: &str,
    new_size: i64,
    last_modified: &str,
) -> DbResult<(i64, bool)> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    conn.execute("BEGIN TRANSACTION", ()).await?;

    let result = async {
        // Get old size for delta calculation (if file exists)
        let mut rows = conn
            .query(
                "SELECT size FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key = ?3",
                turso::params![bucket, account_id, key],
            )
            .await?;

        let (old_size, is_new_file): (i64, bool) = if let Some(row) = rows.next().await? {
            (row.get(0)?, false)
        } else {
            (0, true)
        };
        drop(rows);

        let now = chrono::Utc::now().timestamp();
        let (parent_path, name) = parse_key(key);

        // Use INSERT OR REPLACE to handle both new files and updates
        conn.execute(
        "INSERT INTO cached_files (bucket, account_id, key, parent_path, name, size, last_modified, synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (bucket, account_id, key) DO UPDATE SET
           size = ?6, last_modified = ?7, synced_at = ?8",
        turso::params![
            bucket,
            account_id,
            key,
            parent_path.clone(),
            name,
            new_size,
            last_modified,
            now
        ],
    ).await?;

        super::prefix_sync::invalidate_prefixes_on(&conn, bucket, account_id, &[parent_path])
            .await?;
        bump_content_revision_on(&conn, bucket, account_id).await?;

        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((new_size - old_size, is_new_file))
    }
    .await;
    match result {
        Ok(value) => {
            conn.execute("COMMIT", ()).await?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
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
pub async fn search_cached_files(
    bucket: &str,
    account_id: &str,
    query: &str,
) -> DbResult<SearchResult> {
    let conn = super::cache_scope::read_connection(account_id).await?;

    // Split query into terms and create LIKE conditions for each
    let terms: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();

    if terms.is_empty() {
        return Ok(SearchResult {
            files: Vec::new(),
            total_count: 0,
        });
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
    let conn = super::cache_scope::read_connection(account_id).await?;
    let pattern = format!("{}%", prefix);

    let mut rows = conn
        .query(
            "SELECT COALESCE(SUM(size), 0) FROM cached_files
         WHERE bucket = ?1 AND account_id = ?2 AND key LIKE ?3",
            turso::params![bucket, account_id, pattern],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        let total: i64 = row.get(0)?;
        Ok(total)
    } else {
        Ok(0)
    }
}

/// Bucket-wide summary of cached contents.
#[derive(Debug, Clone)]
pub struct BucketSummary {
    pub total_files: i64,
    pub total_size: i64,
    pub last_modified: Option<String>,
    /// True when the numbers describe the whole bucket (directory tree root
    /// from a finished sync, or a sync_meta row proving a full sync completed).
    /// False for partial data gathered by lazy per-folder browsing.
    pub is_complete: bool,
}

/// Whether a full bucket sync has completed (sync_meta row exists). While a
/// full sync + incremental cache updates hold, the local cache is
/// authoritative for browsing this bucket.
pub async fn has_full_sync(bucket: &str, account_id: &str) -> DbResult<bool> {
    let conn = super::cache_scope::read_connection(account_id).await?;
    let mut meta_rows = conn
        .query(
            "SELECT 1 FROM sync_meta WHERE bucket = ?1 AND account_id = ?2",
            turso::params![bucket, account_id],
        )
        .await?;
    Ok(meta_rows.next().await?.is_some())
}

/// Get a bucket-wide summary (total file count + total size).
///
/// Resolution order:
/// 1. directory_tree root node — exact totals, but only trusted once a
///    sync_meta row proves a full sync completed (incremental upload/delete
///    deltas can create a partial root node before any full sync).
/// 2. SQL aggregate over cached_files — covers lazy-browsed partial data.
///    Exact-match on indexed columns, no LIKE.
pub async fn get_bucket_summary(bucket: &str, account_id: &str) -> DbResult<BucketSummary> {
    // sync_meta only exists after a full sync finished — it both gates the
    // directory-tree shortcut and distinguishes a genuinely empty bucket
    // (complete) from not-yet-synced lazy data in the aggregate fallback.
    let has_full_sync = has_full_sync(bucket, account_id).await?;

    if has_full_sync {
        if let Some(root) = get_directory_node(bucket, account_id, "").await? {
            return Ok(BucketSummary {
                total_files: root.total_file_count as i64,
                total_size: root.total_size,
                last_modified: root.last_modified,
                is_complete: true,
            });
        }
    }

    let conn = super::cache_scope::read_connection(account_id).await?;

    let mut rows = conn
        .query(
            "SELECT COUNT(*), COALESCE(SUM(size), 0), MAX(last_modified)
             FROM cached_files
             WHERE bucket = ?1 AND account_id = ?2",
            turso::params![bucket, account_id],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        Ok(BucketSummary {
            total_files: row.get(0)?,
            total_size: row.get(1)?,
            last_modified: row.get(2)?,
            is_complete: has_full_sync,
        })
    } else {
        Ok(BucketSummary {
            total_files: 0,
            total_size: 0,
            last_modified: None,
            is_complete: has_full_sync,
        })
    }
}

fn directory_node_from_row(row: &turso::Row) -> DbResult<CachedDirectoryNode> {
    Ok(CachedDirectoryNode {
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
    })
}

/// Get directory node by path
pub async fn get_directory_node(
    bucket: &str,
    account_id: &str,
    path: &str,
) -> DbResult<Option<CachedDirectoryNode>> {
    let conn = super::cache_scope::read_connection(account_id).await?;
    let mut rows = conn.query(
        "SELECT bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated
         FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2 AND path = ?3",
        turso::params![bucket, account_id, path]
    ).await?;

    if let Some(row) = rows.next().await? {
        Ok(Some(directory_node_from_row(&row)?))
    } else {
        Ok(None)
    }
}

/// Get directory nodes for many paths in one lock acquisition. Result is
/// aligned with `paths` (None where no node exists). Collapses the
/// per-subfolder query round-trips a folder view used to make.
pub async fn get_directory_nodes(
    bucket: &str,
    account_id: &str,
    paths: &[String],
) -> DbResult<Vec<Option<CachedDirectoryNode>>> {
    let conn = super::cache_scope::read_connection(account_id).await?;
    let mut nodes = Vec::with_capacity(paths.len());
    for path in paths {
        let mut rows = conn.query(
            "SELECT bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated
             FROM directory_tree
             WHERE bucket = ?1 AND account_id = ?2 AND path = ?3",
            turso::params![bucket, account_id, path.clone()]
        ).await?;

        match rows.next().await? {
            Some(row) => nodes.push(Some(directory_node_from_row(&row)?)),
            None => nodes.push(None),
        }
    }
    Ok(nodes)
}

/// Get all directory nodes for a bucket
pub async fn get_all_directory_nodes(
    bucket: &str,
    account_id: &str,
) -> DbResult<Vec<CachedDirectoryNode>> {
    let conn = super::cache_scope::read_connection(account_id).await?;
    let mut rows = conn.query(
        "SELECT bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated
         FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2
         ORDER BY path",
        turso::params![bucket, account_id]
    ).await?;

    let mut nodes = Vec::new();
    while let Some(row) = rows.next().await? {
        nodes.push(directory_node_from_row(&row)?);
    }
    Ok(nodes)
}

/// Stop a bucket's cache from claiming to be complete, without discarding it.
///
/// Browsing then falls back to per-prefix listing, which is slower but cannot
/// present a folder as empty on the strength of a completeness claim the cache
/// can no longer back up. The rows themselves are kept: they are still the
/// best answer available until the next sync.
#[allow(dead_code)]
pub async fn clear_full_sync_marker(bucket: &str, account_id: &str) -> DbResult<()> {
    let conn = super::cache_scope::clear_connection(account_id).await?;
    conn.execute(
        "DELETE FROM sync_meta WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;
    Ok(())
}

/// Clear all cached data for a bucket
pub async fn clear_file_cache(bucket: &str, account_id: &str) -> DbResult<()> {
    let conn = super::cache_scope::clear_connection(account_id).await?;

    conn.execute(
        "DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;

    conn.execute(
        "DELETE FROM directory_tree WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;

    conn.execute(
        "DELETE FROM sync_meta WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;

    Ok(())
}

/// Result of get_folder_contents
#[derive(Debug, Clone)]
pub struct FolderContents {
    pub files: Vec<CachedFile>,
    pub folders: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CachedFolderPage {
    pub files: Vec<CachedFile>,
    pub folders: Vec<String>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CachePageCursor {
    v: u8,
    bucket: String,
    account_id: String,
    prefix: String,
    snapshot: Option<String>,
    pos: CachePageCursorPosition,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind")]
enum CachePageCursorPosition {
    Folder { path: String },
    File { name: String, key: String },
}

impl CachePageCursor {
    fn parse(
        value: &str,
        bucket: &str,
        account_id: &str,
        prefix: &str,
        snapshot: Option<String>,
    ) -> DbResult<Self> {
        let cursor: Self = serde_json::from_str(value)?;
        if cursor.v != 1
            || cursor.bucket != bucket
            || cursor.account_id != account_id
            || cursor.prefix != prefix
            || cursor.snapshot != snapshot
        {
            return Err("Cache page cursor no longer matches this cache snapshot".into());
        }
        Ok(cursor)
    }

    fn folder(
        path: &str,
        bucket: &str,
        account_id: &str,
        prefix: &str,
        snapshot: Option<String>,
    ) -> DbResult<String> {
        Ok(serde_json::to_string(&Self {
            v: 1,
            bucket: bucket.to_string(),
            account_id: account_id.to_string(),
            prefix: prefix.to_string(),
            snapshot,
            pos: CachePageCursorPosition::Folder {
                path: path.to_string(),
            },
        })?)
    }

    fn file(
        file: &CachedFile,
        bucket: &str,
        account_id: &str,
        prefix: &str,
        snapshot: Option<String>,
    ) -> DbResult<String> {
        Ok(serde_json::to_string(&Self {
            v: 1,
            bucket: bucket.to_string(),
            account_id: account_id.to_string(),
            prefix: prefix.to_string(),
            snapshot,
            pos: CachePageCursorPosition::File {
                name: file.name.clone(),
                key: file.key.clone(),
            },
        })?)
    }
}

/// Get folder contents from cache (files at this level + immediate subfolders)
/// This mimics S3 ListObjectsV2 with delimiter="/" behavior
///
/// FAST: Uses exact match on parent_path (indexed) instead of LIKE patterns
pub async fn get_folder_contents(
    bucket: &str,
    account_id: &str,
    prefix: &str,
) -> DbResult<FolderContents> {
    let conn = super::cache_scope::read_connection(account_id).await?;

    folder_contents_on(&conn, bucket, account_id, prefix).await
}

pub(crate) async fn folder_contents_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefix: &str,
) -> DbResult<FolderContents> {
    // Query 1: Get files directly in this folder using EXACT MATCH on parent_path
    // This is O(1) index lookup instead of O(n) LIKE scan
    let mut rows = conn
        .query(
            "SELECT bucket, account_id, key, parent_path, name, size, last_modified, synced_at
         FROM cached_files
         WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3
         ORDER BY name",
            turso::params![bucket, account_id, prefix],
        )
        .await?;

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
    let mut rows = conn
        .query(
            "SELECT path FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3
         ORDER BY path",
            turso::params![bucket, account_id, prefix],
        )
        .await?;

    let mut folders = Vec::new();
    while let Some(row) = rows.next().await? {
        let folder: String = row.get(0)?;
        folders.push(folder);
    }

    Ok(FolderContents { files, folders })
}

fn cached_file_from_row(row: &turso::Row) -> DbResult<CachedFile> {
    Ok(CachedFile {
        bucket: row.get(0)?,
        account_id: row.get(1)?,
        key: row.get(2)?,
        parent_path: row.get(3)?,
        name: row.get(4)?,
        size: row.get(5)?,
        last_modified: row.get(6)?,
        synced_at: row.get(7)?,
    })
}

async fn has_cached_file_after(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefix: &str,
    cursor: Option<&CachedFile>,
) -> DbResult<bool> {
    let mut rows = if let Some(cursor) = cursor {
        conn.query(
            "SELECT 1 FROM cached_files
             WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3
               AND (name > ?4 OR (name = ?4 AND key > ?5))
             ORDER BY name, key
             LIMIT 1",
            turso::params![
                bucket,
                account_id,
                prefix,
                cursor.name.as_str(),
                cursor.key.as_str()
            ],
        )
        .await?
    } else {
        conn.query(
            "SELECT 1 FROM cached_files
             WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3
             ORDER BY name, key
             LIMIT 1",
            turso::params![bucket, account_id, prefix],
        )
        .await?
    };
    Ok(rows.next().await?.is_some())
}

pub(crate) async fn cached_folder_page_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefix: &str,
    cursor: Option<&str>,
    snapshot: Option<String>,
    page_size: usize,
) -> DbResult<CachedFolderPage> {
    if page_size == 0 {
        return Err("Cache page size must be greater than zero".into());
    }
    let cursor = cursor
        .map(|cursor| CachePageCursor::parse(cursor, bucket, account_id, prefix, snapshot.clone()))
        .transpose()?;
    let limit = page_size + 1;
    let mut folders = Vec::new();
    let mut files = Vec::new();
    let mut next_cursor = None;

    if !matches!(
        cursor.as_ref().map(|cursor| &cursor.pos),
        Some(CachePageCursorPosition::File { .. })
    ) {
        let folder_after = match &cursor {
            Some(CachePageCursor {
                pos: CachePageCursorPosition::Folder { path },
                ..
            }) => Some(path.as_str()),
            _ => None,
        };
        let mut rows = if let Some(path) = folder_after {
            conn.query(
                "SELECT path FROM directory_tree
                 WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3 AND path > ?4
                 ORDER BY path
                 LIMIT ?5",
                turso::params![bucket, account_id, prefix, path, limit as i64],
            )
            .await?
        } else {
            conn.query(
                "SELECT path FROM directory_tree
                 WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3
                 ORDER BY path
                 LIMIT ?4",
                turso::params![bucket, account_id, prefix, limit as i64],
            )
            .await?
        };
        while let Some(row) = rows.next().await? {
            folders.push(row.get::<String>(0)?);
        }
        if folders.len() > page_size {
            folders.truncate(page_size);
            next_cursor = folders
                .last()
                .map(|path| {
                    CachePageCursor::folder(path, bucket, account_id, prefix, snapshot.clone())
                })
                .transpose()?;
            return Ok(CachedFolderPage {
                files,
                folders,
                next_cursor,
            });
        }
        if folders.len() == page_size {
            if has_cached_file_after(conn, bucket, account_id, prefix, None).await? {
                next_cursor = folders
                    .last()
                    .map(|path| {
                        CachePageCursor::folder(path, bucket, account_id, prefix, snapshot.clone())
                    })
                    .transpose()?;
            }
            return Ok(CachedFolderPage {
                files,
                folders,
                next_cursor,
            });
        }
    }

    let remaining = page_size - folders.len();
    if remaining == 0 {
        return Ok(CachedFolderPage {
            files,
            folders,
            next_cursor,
        });
    }
    let file_after = match &cursor {
        Some(CachePageCursor {
            pos: CachePageCursorPosition::File { name, key },
            ..
        }) => Some((name.as_str(), key.as_str())),
        _ => None,
    };
    let mut rows = if let Some((name, key)) = file_after {
        conn.query(
            "SELECT bucket, account_id, key, parent_path, name, size, last_modified, synced_at
             FROM cached_files
             WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3
               AND (name > ?4 OR (name = ?4 AND key > ?5))
             ORDER BY name, key
             LIMIT ?6",
            turso::params![
                bucket,
                account_id,
                prefix,
                name,
                key,
                (remaining + 1) as i64
            ],
        )
        .await?
    } else {
        conn.query(
            "SELECT bucket, account_id, key, parent_path, name, size, last_modified, synced_at
             FROM cached_files
             WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3
             ORDER BY name, key
             LIMIT ?4",
            turso::params![bucket, account_id, prefix, (remaining + 1) as i64],
        )
        .await?
    };
    while let Some(row) = rows.next().await? {
        files.push(cached_file_from_row(&row)?);
    }
    if files.len() > remaining {
        files.truncate(remaining);
        next_cursor = files
            .last()
            .map(|file| CachePageCursor::file(file, bucket, account_id, prefix, snapshot.clone()))
            .transpose()?;
    } else if files.len() == remaining {
        if let Some(last_file) = files.last() {
            if has_cached_file_after(conn, bucket, account_id, prefix, Some(last_file)).await? {
                next_cursor = Some(CachePageCursor::file(
                    last_file,
                    bucket,
                    account_id,
                    prefix,
                    snapshot.clone(),
                )?);
            }
        }
    }
    Ok(CachedFolderPage {
        files,
        folders,
        next_cursor,
    })
}

// ============ Streaming Sync Functions ============
//
// Uses a staging table so the live cached_files table is untouched during sync.
// If the sync fails midway, old data is preserved — the staging table is just
// abandoned and cleaned up on the next begin_sync call.
// finish_sync atomically swaps staging → live in a single transaction.

async fn active_sync_run_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
) -> DbResult<String> {
    let mut rows = conn
        .query(
            "SELECT value FROM app_state WHERE key = ?1",
            turso::params![sync_active_run_key(bucket, account_id)],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get(0)?),
        None => Err("Full sync run is missing".into()),
    }
}

async fn ensure_sync_run_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    run_token: &str,
) -> DbResult<()> {
    let active = active_sync_run_on(conn, bucket, account_id).await?;
    if active != run_token {
        return Err("Full sync run is no longer active".into());
    }
    Ok(())
}

/// Step 1: prepare staging table for new sync data.
/// Old data in cached_files stays intact and queryable during the entire sync.
pub async fn begin_sync(bucket: &str, account_id: &str) -> DbResult<String> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    begin_sync_on(&conn, bucket, account_id).await
}

pub(crate) async fn begin_sync_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
) -> DbResult<String> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cached_files_staging (
            bucket TEXT NOT NULL,
            account_id TEXT NOT NULL,
            key TEXT NOT NULL,
            parent_path TEXT NOT NULL,
            name TEXT NOT NULL,
            size INTEGER NOT NULL,
            last_modified TEXT NOT NULL,
            synced_at INTEGER NOT NULL,
            PRIMARY KEY (bucket, account_id, key)
        )",
        (),
    )
    .await?;

    conn.execute(
        "DELETE FROM cached_files_staging WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;

    let base_revision = content_revision_on(conn, bucket, account_id).await?;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        turso::params![
            sync_base_revision_key(bucket, account_id),
            base_revision.to_string()
        ],
    )
    .await?;
    let run_token = next_sync_run_token();
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        turso::params![sync_active_run_key(bucket, account_id), run_token.clone()],
    )
    .await?;

    Ok(run_token)
}

/// Step 2: insert a batch of files into the STAGING table.
/// The live cached_files table is not modified.
pub async fn store_file_batch(
    bucket: &str,
    account_id: &str,
    run_token: &str,
    files: &[CachedFile],
) -> DbResult<()> {
    if files.is_empty() {
        return Ok(());
    }

    const BATCH_SIZE: usize = 1000;
    let conn = super::cache_scope::write_connection(account_id).await?;
    conn.execute("BEGIN TRANSACTION", ()).await?;

    let tx_result = async {
        ensure_sync_run_on(&conn, bucket, account_id, run_token).await?;
        for chunk in files.chunks(BATCH_SIZE) {
            if chunk.is_empty() {
                continue;
            }

            let placeholders: Vec<String> = chunk
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let base = i * 8;
                    format!(
                        "(?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{})",
                        base + 1, base + 2, base + 3, base + 4,
                        base + 5, base + 6, base + 7, base + 8
                    )
                })
                .collect();

            let sql = format!(
                "INSERT INTO cached_files_staging (bucket, account_id, key, parent_path, name, size, last_modified, synced_at) VALUES {}",
                placeholders.join(", ")
            );

            let mut params: Vec<turso::Value> = Vec::with_capacity(chunk.len() * 8);
            for file in chunk {
                let (parent_path, name) = if file.name.is_empty() {
                    parse_key(&file.key)
                } else {
                    (file.parent_path.clone(), file.name.clone())
                };

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

        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    }
    .await;

    if let Err(err) = tx_result {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(err);
    }

    conn.execute("COMMIT", ()).await?;
    Ok(())
}

/// Step 3: atomically swap staging data into the live table.
/// In one transaction: delete old live data → copy staging → rebuild tree,
/// publish skipped-prefix metadata, clear prefix markers, clean staging, and
/// update the sync generation. If this fails, old data is still intact.
#[allow(dead_code)]
pub async fn finish_sync(bucket: &str, account_id: &str, file_count: usize) -> DbResult<()> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    let run_token = active_sync_run_on(&conn, bucket, account_id).await?;
    drop(conn);
    finish_sync_with_metadata(bucket, account_id, &run_token, file_count, &[], &[]).await
}

pub async fn finish_sync_with_metadata(
    bucket: &str,
    account_id: &str,
    run_token: &str,
    file_count: usize,
    folder_keys: &[String],
    skipped_prefixes: &[String],
) -> DbResult<()> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    conn.execute("BEGIN TRANSACTION", ()).await?;
    let result = finish_sync_with_metadata_on(
        &conn,
        bucket,
        account_id,
        run_token,
        file_count,
        folder_keys,
        skipped_prefixes,
    )
    .await;
    match result {
        Ok(()) => {
            conn.execute("COMMIT", ()).await?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
}

pub(crate) async fn finish_sync_with_metadata_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    run_token: &str,
    file_count: usize,
    folder_keys: &[String],
    skipped_prefixes: &[String],
) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();
    ensure_sync_run_on(conn, bucket, account_id, run_token).await?;
    let base_key = sync_base_revision_key(bucket, account_id);
    let mut rows = conn
        .query(
            "SELECT value FROM app_state WHERE key = ?1",
            turso::params![base_key.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err("Full sync base revision is missing".into());
    };
    let base_revision: i64 = row.get::<String>(0)?.parse()?;
    let current_revision = content_revision_on(conn, bucket, account_id).await?;
    if current_revision != base_revision {
        return Err("Cache changed during full sync".into());
    }
    drop(rows);

    conn.execute(
        "DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;

    conn.execute(
        "INSERT INTO cached_files SELECT * FROM cached_files_staging WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;

    super::dir_tree::rebuild_directory_tree_on(conn, bucket, account_id, folder_keys).await?;

    conn.execute(
        "DELETE FROM prefix_sync_times WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;

    let skipped_key = format!("skipped_prefixes:{account_id}:{bucket}");
    if skipped_prefixes.is_empty() {
        conn.execute(
            "DELETE FROM app_state WHERE key = ?1",
            turso::params![skipped_key],
        )
        .await?;
    } else {
        let skipped_json = serde_json::to_string(skipped_prefixes)?;
        conn.execute(
            "INSERT INTO app_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            turso::params![skipped_key, skipped_json],
        )
        .await?;
    }

    conn.execute(
        "DELETE FROM cached_files_staging WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;
    conn.execute(
        "DELETE FROM app_state WHERE key = ?1",
        turso::params![sync_base_revision_key(bucket, account_id)],
    )
    .await?;
    conn.execute(
        "DELETE FROM app_state WHERE key = ?1",
        turso::params![sync_active_run_key(bucket, account_id)],
    )
    .await?;

    bump_content_revision_on(conn, bucket, account_id).await?;

    conn.execute(
        "INSERT INTO sync_meta (bucket, account_id, last_sync, file_count, generation)
         VALUES (?1, ?2, ?3, ?4, 1)
         ON CONFLICT (bucket, account_id) DO UPDATE SET
           last_sync = ?3,
           file_count = ?4,
           generation = sync_meta.generation + 1",
        turso::params![bucket, account_id, now, file_count as i32],
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use turso::Connection;

    async fn schema(conn: &Connection) {
        conn.execute_batch(super::super::app_state::get_table_sql())
            .await
            .unwrap();
        conn.execute_batch(get_table_sql()).await.unwrap();
        conn.execute_batch(super::super::prefix_sync::get_table_sql())
            .await
            .unwrap();
    }

    async fn fixture() -> (turso::Database, Connection) {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        schema(&conn).await;
        (db, conn)
    }

    async fn insert_file(conn: &Connection, key: &str, size: i64) {
        let (parent_path, name) = parse_key(key);
        conn.execute(
            "INSERT INTO cached_files (bucket, account_id, key, parent_path, name, size, last_modified, synced_at)
             VALUES ('bucket', 'account', ?1, ?2, ?3, ?4, '2026-09-22T00:00:00Z', 1)",
            turso::params![key, parent_path, name, size],
        )
        .await
        .unwrap();
    }

    async fn insert_folder(conn: &Connection, path: &str, parent_path: &str) {
        conn.execute(
            "INSERT INTO directory_tree
             (bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated)
             VALUES ('bucket', 'account', ?1, ?2, 0, 0, 0, 0, NULL, 1)",
            turso::params![path, parent_path],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cached_folder_pages_advance_with_database_cursors() {
        let (_db, conn) = fixture().await;
        for index in 0..1205 {
            insert_file(&conn, &format!("file-{index:04}.txt"), index).await;
        }

        let first = cached_folder_page_on(
            &conn,
            "bucket",
            "account",
            "",
            None,
            Some("prefix:1:1".into()),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(first.files.len(), 1000);
        assert_eq!(first.folders.len(), 0);
        assert!(first.next_cursor.is_some());
        let second = cached_folder_page_on(
            &conn,
            "bucket",
            "account",
            "",
            first.next_cursor.as_deref(),
            Some("prefix:1:1".into()),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(second.files.len(), 205);
        assert_eq!(second.files[0].key, "file-1000.txt");
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn cached_folder_pages_keep_folder_first_order_across_cursor_boundaries() {
        let (_db, conn) = fixture().await;
        insert_folder(&conn, "zeta/", "").await;
        insert_folder(&conn, "alpha/", "").await;
        insert_file(&conn, "alpha.txt", 1).await;
        insert_file(&conn, "zeta.txt", 2).await;

        let first = cached_folder_page_on(
            &conn,
            "bucket",
            "account",
            "",
            None,
            Some("prefix:1:1".into()),
            2,
        )
        .await
        .unwrap();
        assert_eq!(
            first.folders,
            vec!["alpha/".to_string(), "zeta/".to_string()]
        );
        assert!(first.files.is_empty());
        let second = cached_folder_page_on(
            &conn,
            "bucket",
            "account",
            "",
            first.next_cursor.as_deref(),
            Some("prefix:1:1".into()),
            2,
        )
        .await
        .unwrap();
        assert_eq!(
            second
                .files
                .iter()
                .map(|file| file.key.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha.txt", "zeta.txt"]
        );
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn full_sync_publish_aborts_when_local_revision_changes_during_scan() {
        let (_db, conn) = fixture().await;
        insert_file(&conn, "old.txt", 1).await;
        let run = begin_sync_on(&conn, "bucket", "account").await.unwrap();
        conn.execute(
            "INSERT INTO cached_files_staging(bucket,account_id,key,parent_path,name,size,last_modified,synced_at)
             VALUES ('bucket','account','new.txt','','new.txt',1,'',1)",
            (),
        )
        .await
        .unwrap();
        bump_content_revision_on(&conn, "bucket", "account")
            .await
            .unwrap();

        conn.execute("BEGIN TRANSACTION", ()).await.unwrap();
        let publish =
            finish_sync_with_metadata_on(&conn, "bucket", "account", &run, 1, &[], &[]).await;
        assert!(publish.is_err());
        conn.execute("ROLLBACK", ()).await.unwrap();

        let page = cached_folder_page_on(&conn, "bucket", "account", "", None, None, 10)
            .await
            .unwrap();
        assert_eq!(
            page.files
                .iter()
                .map(|file| file.key.as_str())
                .collect::<Vec<_>>(),
            vec!["old.txt"]
        );
        let mut rows = conn
            .query(
                "SELECT 1 FROM sync_meta WHERE bucket = 'bucket' AND account_id = 'account'",
                (),
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn cached_folder_cursor_rejects_snapshot_replacement_and_special_keys_round_trip() {
        let (_db, conn) = fixture().await;
        insert_file(&conn, "line\nbreak.txt", 1).await;
        insert_file(&conn, "plain.txt", 2).await;

        let first = cached_folder_page_on(
            &conn,
            "bucket",
            "account",
            "",
            None,
            Some("prefix:10:1".into()),
            1,
        )
        .await
        .unwrap();
        assert_eq!(first.files[0].key, "line\nbreak.txt");
        let second = cached_folder_page_on(
            &conn,
            "bucket",
            "account",
            "",
            first.next_cursor.as_deref(),
            Some("prefix:10:1".into()),
            1,
        )
        .await
        .unwrap();
        assert_eq!(second.files[0].key, "plain.txt");
        let replaced = cached_folder_page_on(
            &conn,
            "bucket",
            "account",
            "",
            first.next_cursor.as_deref(),
            Some("prefix:10:2".into()),
            1,
        )
        .await;
        assert!(replaced.is_err());

        let wrong_bucket = cached_folder_page_on(
            &conn,
            "other-bucket",
            "account",
            "",
            first.next_cursor.as_deref(),
            Some("prefix:10:1".into()),
            1,
        )
        .await;
        assert!(wrong_bucket.is_err());

        let wrong_account = cached_folder_page_on(
            &conn,
            "bucket",
            "other-account",
            "",
            first.next_cursor.as_deref(),
            Some("prefix:10:1".into()),
            1,
        )
        .await;
        assert!(wrong_account.is_err());

        let wrong_prefix = cached_folder_page_on(
            &conn,
            "bucket",
            "account",
            "other/",
            first.next_cursor.as_deref(),
            Some("prefix:10:1".into()),
            1,
        )
        .await;
        assert!(wrong_prefix.is_err());
    }

    #[tokio::test]
    async fn full_sync_publish_requires_current_run_and_base_revision() {
        let (_db, conn) = fixture().await;
        let old_run = begin_sync_on(&conn, "bucket", "account").await.unwrap();
        let current_run = begin_sync_on(&conn, "bucket", "account").await.unwrap();

        let old_run_guard = ensure_sync_run_on(&conn, "bucket", "account", &old_run).await;
        assert!(old_run_guard.is_err());
        ensure_sync_run_on(&conn, "bucket", "account", &current_run)
            .await
            .unwrap();

        conn.execute(
            "DELETE FROM app_state WHERE key = 'sync_base_revision:account:bucket'",
            (),
        )
        .await
        .unwrap();
        conn.execute("BEGIN TRANSACTION", ()).await.unwrap();
        let missing_base =
            finish_sync_with_metadata_on(&conn, "bucket", "account", &current_run, 0, &[], &[])
                .await;
        assert!(missing_base.is_err());
        conn.execute("ROLLBACK", ()).await.unwrap();
    }
}
