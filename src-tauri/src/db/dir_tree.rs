//! Directory tree builder optimized for large file sets.
//!
//! Separates tree computation from database storage and supports:
//! - Async-friendly processing with periodic yields
//! - Batch database inserts for performance
//! - Progress reporting during build

use super::{get_connection, CachedFile, DbResult};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Batch size for database inserts
const DB_BATCH_SIZE: usize = 1000;
const DB_YIELD_EVERY_BATCHES: usize = 8;

/// Yield to runtime every N directories during computation
const YIELD_INTERVAL: usize = 1000;

/// Progress callback receives (current, total) counts
pub type ProgressCallback = Box<dyn FnMut(usize, usize) + Send>;

/// In-memory directory node for tree building
#[derive(Debug, Clone)]
struct DirNode {
    direct_files: Vec<FileInfo>,
    subdirs: BTreeSet<String>,
}

/// Minimal file info needed for aggregation
#[derive(Debug, Clone)]
struct FileInfo {
    size: i64,
    last_modified: String,
}

/// Computed directory statistics ready for DB storage
#[derive(Debug, Clone)]
pub struct ComputedNode {
    pub path: String,
    pub parent_path: String, // Parent folder path for fast child lookup
    pub file_count: i32,
    pub total_file_count: i32,
    pub size: i64,
    pub total_size: i64,
    pub last_modified: Option<String>,
}

/// Result of updating directory tree for a move operation.
#[derive(Debug, Clone)]
pub struct MoveTreeResult {
    pub removed_paths: Vec<String>,
    pub created_paths: Vec<String>,
}

/// Helper to compute parent path from a folder path
fn compute_parent_path(path: &str) -> String {
    // Path like "folder/" -> parent is ""
    // Path like "folder/sub/" -> parent is "folder/"
    // Path like "" (root) -> parent is ""
    if path.is_empty() {
        return String::new();
    }

    // Remove trailing slash, find last slash, include it
    let without_trailing = path.trim_end_matches('/');
    if let Some(last_slash) = without_trailing.rfind('/') {
        without_trailing[..=last_slash].to_string()
    } else {
        String::new() // Top-level folder, parent is root
    }
}

/// Builder for directory tree from file list.
///
/// Usage:
/// ```ignore
/// let builder = DirectoryTreeBuilder::new();
/// let nodes = builder.build(files, Some(progress_cb)).await;
/// DirectoryTreeBuilder::store(bucket, account_id, &nodes).await?;
/// ```
pub struct DirectoryTreeBuilder {
    dir_map: HashMap<String, DirNode>,
}

impl DirectoryTreeBuilder {
    pub fn new() -> Self {
        Self {
            dir_map: HashMap::new(),
        }
    }

    /// Build directory tree in memory from files.
    /// Returns computed nodes ready for database storage.
    pub async fn build(
        mut self,
        files: &[CachedFile],
        mut progress: Option<ProgressCallback>,
    ) -> Vec<ComputedNode> {
        // Phase 1: Extract directory structure
        self.extract_directories(files).await;

        // Phase 2: Compute aggregates bottom-up
        self.compute_aggregates(progress.as_mut()).await
    }

    /// Extract directory structure from file paths
    async fn extract_directories(&mut self, files: &[CachedFile]) {
        // Initialize root
        self.dir_map.insert(
            String::new(),
            DirNode {
                direct_files: Vec::new(),
                subdirs: BTreeSet::new(),
            },
        );

        for (idx, file) in files.iter().enumerate() {
            self.process_file(file);

            // Yield periodically to not starve runtime
            if idx > 0 && idx % YIELD_INTERVAL == 0 {
                tokio::task::yield_now().await;
            }
        }
    }

    /// Process single file, updating directory map
    fn process_file(&mut self, file: &CachedFile) {
        let parts: Vec<&str> = file.key.split('/').collect();
        let file_info = FileInfo {
            size: file.size,
            last_modified: file.last_modified.clone(),
        };

        // Root-level file
        if parts.len() == 1 {
            self.dir_map
                .get_mut("")
                .unwrap()
                .direct_files
                .push(file_info);
            return;
        }

        // Traverse directory levels
        for i in 0..parts.len() - 1 {
            let current_path = if i == 0 {
                format!("{}/", parts[0])
            } else {
                format!("{}/", parts[0..=i].join("/"))
            };

            let parent_path = if i > 0 {
                format!("{}/", parts[0..i].join("/"))
            } else {
                String::new()
            };

            // Ensure directory exists
            self.dir_map
                .entry(current_path.clone())
                .or_insert_with(|| DirNode {
                    direct_files: Vec::new(),
                    subdirs: BTreeSet::new(),
                });

            // Link to parent
            if let Some(parent) = self.dir_map.get_mut(&parent_path) {
                parent.subdirs.insert(current_path.clone());
            }

            // Add file to direct parent
            if i == parts.len() - 2 {
                self.dir_map
                    .get_mut(&current_path)
                    .unwrap()
                    .direct_files
                    .push(file_info.clone());
            }
        }
    }

    /// Compute aggregates bottom-up (deepest directories first)
    async fn compute_aggregates(
        &self,
        mut progress: Option<&mut ProgressCallback>,
    ) -> Vec<ComputedNode> {
        // Sort by depth (deepest first)
        let mut sorted_paths: Vec<_> = self.dir_map.keys().cloned().collect();
        sorted_paths.sort_by_key(|p| std::cmp::Reverse(p.matches('/').count()));

        let total = sorted_paths.len();
        let mut results: Vec<ComputedNode> = Vec::with_capacity(total);
        let mut aggregates: HashMap<String, (i32, i64, Option<String>)> = HashMap::new();

        // Report initial progress
        if let Some(ref mut cb) = progress {
            cb(0, total);
        }

        for (idx, path) in sorted_paths.iter().enumerate() {
            let node = self.dir_map.get(path).unwrap();

            // Direct stats
            let direct_count = node.direct_files.len() as i32;
            let direct_size: i64 = node.direct_files.iter().map(|f| f.size).sum();
            let direct_last_mod: Option<&str> = node
                .direct_files
                .iter()
                .map(|f| f.last_modified.as_str())
                .max();

            // Aggregate from subdirs
            let mut sub_count: i32 = 0;
            let mut sub_size: i64 = 0;
            let mut sub_last_mod: Option<String> = None;

            for subdir in &node.subdirs {
                if let Some(&(count, size, ref last_mod)) = aggregates.get(subdir) {
                    sub_count += count;
                    sub_size += size;
                    if let Some(ref lm) = last_mod {
                        sub_last_mod = match sub_last_mod {
                            None => Some(lm.clone()),
                            Some(ref curr) if lm > curr => Some(lm.clone()),
                            other => other,
                        };
                    }
                }
            }

            // Combined last_modified
            let last_modified = match (direct_last_mod, sub_last_mod) {
                (Some(d), Some(s)) => Some(if d > s.as_str() { d.to_string() } else { s }),
                (Some(d), None) => Some(d.to_string()),
                (None, Some(s)) => Some(s),
                (None, None) => None,
            };

            let total_count = direct_count + sub_count;
            let total_size = direct_size + sub_size;

            // Store for parent aggregation
            aggregates.insert(
                path.clone(),
                (total_count, total_size, last_modified.clone()),
            );

            results.push(ComputedNode {
                path: path.clone(),
                parent_path: compute_parent_path(path),
                file_count: direct_count,
                total_file_count: total_count,
                size: direct_size,
                total_size,
                last_modified,
            });

            // Progress and yield
            if (idx + 1) % 100 == 0 || idx + 1 == total {
                if let Some(ref mut cb) = progress {
                    cb(idx + 1, total);
                }
            }
            if idx > 0 && idx % YIELD_INTERVAL == 0 {
                tokio::task::yield_now().await;
            }
        }

        results
    }

    /// Store computed nodes to database with batch inserts.
    /// Clears existing tree first.
    pub async fn store(bucket: &str, account_id: &str, nodes: &[ComputedNode]) -> DbResult<()> {
        let now = chrono::Utc::now().timestamp();
        let conn = get_connection()?.lock().await;
        conn.execute("BEGIN TRANSACTION", ()).await?;

        let tx_result = async {
            conn.execute(
                "DELETE FROM directory_tree WHERE bucket = ?1 AND account_id = ?2",
                turso::params![bucket, account_id],
            )
            .await?;

            // Batch insert (10 columns with parent_path)
            for (batch_idx, chunk) in nodes.chunks(DB_BATCH_SIZE).enumerate() {
                if chunk.is_empty() {
                    continue;
                }

                let placeholders: Vec<String> = chunk
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        let b = i * 10;
                        format!(
                            "(?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{})",
                            b + 1,
                            b + 2,
                            b + 3,
                            b + 4,
                            b + 5,
                            b + 6,
                            b + 7,
                            b + 8,
                            b + 9,
                            b + 10
                        )
                    })
                    .collect();

                let sql = format!(
                    "INSERT INTO directory_tree 
                     (bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated)
                     VALUES {}",
                    placeholders.join(", ")
                );

                let mut params: Vec<turso::Value> = Vec::with_capacity(chunk.len() * 10);
                for node in chunk {
                    params.push(bucket.to_string().into());
                    params.push(account_id.to_string().into());
                    params.push(node.path.clone().into());
                    params.push(node.parent_path.clone().into());
                    params.push(node.file_count.into());
                    params.push(node.total_file_count.into());
                    params.push(node.size.into());
                    params.push(node.total_size.into());
                    params.push(
                        node.last_modified
                            .clone()
                            .map(|s| s.into())
                            .unwrap_or(turso::Value::Null),
                    );
                    params.push(now.into());
                }

                conn.execute(&sql, params).await?;

                if (batch_idx + 1) % DB_YIELD_EVERY_BATCHES == 0 {
                    tokio::task::yield_now().await;
                }
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
}

impl Default for DirectoryTreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience function matching old API signature
pub async fn build_directory_tree<F>(
    bucket: &str,
    account_id: &str,
    files: &[CachedFile],
    progress_callback: Option<F>,
) -> DbResult<()>
where
    F: FnMut(usize, usize) + Send + 'static,
{
    let progress: Option<ProgressCallback> =
        progress_callback.map(|f| Box::new(f) as ProgressCallback);

    let builder = DirectoryTreeBuilder::new();
    let nodes = builder.build(files, progress).await;
    DirectoryTreeBuilder::store(bucket, account_id, &nodes).await
}

/// Helper to get all ancestor paths for a file key (including root "")
fn get_ancestor_paths(key: &str) -> (String, Vec<String>) {
    let mut paths: Vec<String> = Vec::new();

    // Get the parent path of the file
    let parent_path = if let Some(last_slash) = key.rfind('/') {
        key[..=last_slash].to_string()
    } else {
        String::new() // Root level file
    };

    // Add all ancestor paths
    let mut current = parent_path.clone();
    paths.push(current.clone());

    while !current.is_empty() {
        let without_trailing = current.trim_end_matches('/');
        if let Some(last_slash) = without_trailing.rfind('/') {
            current = without_trailing[..=last_slash].to_string();
        } else {
            current = String::new(); // Root
        }
        paths.push(current.clone());
    }

    (parent_path, paths)
}

// ============ Core Directory Delta Function ============

/// Apply a delta to directory tree for a single file operation.
/// This is the core function used by delete, move, and update operations.
/// Uses UPSERT to create directory nodes if they don't exist (for new file uploads).
///
/// - `file_count_delta`: -1 for delete, +1 for add, 0 for size-only change
/// - `size_delta`: negative for delete/move-from, positive for add/move-to/size-increase
/// - `last_modified`: Some() to update timestamp (add/update), None to skip (delete)
async fn apply_directory_delta(
    bucket: &str,
    account_id: &str,
    key: &str,
    file_count_delta: i32,
    size_delta: i64,
    last_modified: Option<&str>,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();

    let (parent_path, paths_to_update) = get_ancestor_paths(key);

    for path in &paths_to_update {
        let is_direct_parent = path == &parent_path;
        let path_str = path.as_str();
        let node_parent_path = compute_parent_path(path_str);

        match (is_direct_parent, last_modified) {
            // Direct parent with last_modified update (add/update file)
            (true, Some(lm)) => {
                conn.execute(
                    "INSERT INTO directory_tree (bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?6, ?7, ?8)
                     ON CONFLICT (bucket, account_id, path) DO UPDATE SET
                       file_count = file_count + ?5,
                       total_file_count = total_file_count + ?5,
                       size = size + ?6,
                       total_size = total_size + ?6,
                       last_modified = CASE 
                           WHEN directory_tree.last_modified IS NULL OR ?7 > directory_tree.last_modified THEN ?7 
                           ELSE directory_tree.last_modified 
                       END,
                       last_updated = ?8",
                    turso::params![bucket, account_id, path_str, node_parent_path, file_count_delta, size_delta, lm, now],
                ).await?;
            }
            // Direct parent without last_modified update (delete)
            (true, None) => {
                conn.execute(
                    "INSERT INTO directory_tree (bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?6, NULL, ?7)
                     ON CONFLICT (bucket, account_id, path) DO UPDATE SET
                       file_count = file_count + ?5,
                       total_file_count = total_file_count + ?5,
                       size = size + ?6,
                       total_size = total_size + ?6,
                       last_updated = ?7",
                    turso::params![bucket, account_id, path_str, node_parent_path, file_count_delta, size_delta, now],
                ).await?;
            }
            // Ancestor with last_modified update (add/update file)
            (false, Some(lm)) => {
                conn.execute(
                    "INSERT INTO directory_tree (bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5, 0, ?6, ?7, ?8)
                     ON CONFLICT (bucket, account_id, path) DO UPDATE SET
                       total_file_count = total_file_count + ?5,
                       total_size = total_size + ?6,
                       last_modified = CASE 
                           WHEN directory_tree.last_modified IS NULL OR ?7 > directory_tree.last_modified THEN ?7 
                           ELSE directory_tree.last_modified 
                       END,
                       last_updated = ?8",
                    turso::params![bucket, account_id, path_str, node_parent_path, file_count_delta, size_delta, lm, now],
                ).await?;
            }
            // Ancestor without last_modified update (delete)
            (false, None) => {
                conn.execute(
                    "INSERT INTO directory_tree (bucket, account_id, path, parent_path, file_count, total_file_count, size, total_size, last_modified, last_updated)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5, 0, ?6, NULL, ?7)
                     ON CONFLICT (bucket, account_id, path) DO UPDATE SET
                       total_file_count = total_file_count + ?5,
                       total_size = total_size + ?6,
                       last_updated = ?7",
                    turso::params![bucket, account_id, path_str, node_parent_path, file_count_delta, size_delta, now],
                ).await?;
            }
        }
    }

    Ok(())
}

// ============ Public API Functions ============

/// Update directory tree when a file is deleted.
/// Decreases file_count and sizes for the file's parent and all ancestors.
/// Returns a list of paths that were removed because they became empty.
pub async fn update_directory_tree_for_delete(
    bucket: &str,
    account_id: &str,
    key: &str,
    file_size: i64,
) -> DbResult<Vec<String>> {
    // Apply the delta first
    apply_directory_delta(bucket, account_id, key, -1, -file_size, None).await?;

    // Check for and remove empty paths
    remove_empty_paths(bucket, account_id, key).await
}

/// Update directory tree for a batch of file deletions.
/// Applies deltas for all files, then removes empty paths once for all affected ancestors.
pub async fn update_directory_tree_for_delete_batch(
    bucket: &str,
    account_id: &str,
    deleted: &[(String, i64)],
) -> DbResult<Vec<String>> {
    if deleted.is_empty() {
        return Ok(Vec::new());
    }

    let mut affected_paths: HashSet<String> = HashSet::new();

    for (key, file_size) in deleted {
        apply_directory_delta(bucket, account_id, key, -1, -file_size, None).await?;

        let (_, paths) = get_ancestor_paths(key);
        for path in paths.into_iter().filter(|p| !p.is_empty()) {
            affected_paths.insert(path);
        }
    }

    if affected_paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut paths: Vec<String> = affected_paths.into_iter().collect();
    paths.sort_by_key(|path| std::cmp::Reverse(path.len()));

    let conn = get_connection()?.lock().await;

    let placeholders: Vec<String> = (0..paths.len()).map(|i| format!("?{}", i + 3)).collect();
    let select_sql = format!(
        "SELECT path, total_file_count FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2 AND path IN ({})",
        placeholders.join(", ")
    );

    let mut params: Vec<turso::Value> = Vec::with_capacity(paths.len() + 2);
    params.push(bucket.to_string().into());
    params.push(account_id.to_string().into());
    for path in &paths {
        params.push(path.clone().into());
    }

    let mut rows = conn.query(&select_sql, params).await?;
    let mut removed_paths: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        let path: String = row.get(0)?;
        let total_count: i32 = row.get(1)?;
        if total_count <= 0 {
            removed_paths.push(path);
        }
    }
    drop(rows);

    if removed_paths.is_empty() {
        return Ok(Vec::new());
    }

    let delete_placeholders: Vec<String> = (0..removed_paths.len())
        .map(|i| format!("?{}", i + 3))
        .collect();
    let delete_sql = format!(
        "DELETE FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2 AND path IN ({})",
        delete_placeholders.join(", ")
    );

    let mut delete_params: Vec<turso::Value> = Vec::with_capacity(removed_paths.len() + 2);
    delete_params.push(bucket.to_string().into());
    delete_params.push(account_id.to_string().into());
    for path in &removed_paths {
        delete_params.push(path.clone().into());
    }

    conn.execute(&delete_sql, delete_params).await?;

    Ok(removed_paths)
}

/// Detect which ancestor paths for a key do not yet exist in directory_tree.
/// Returns the list of paths that would be newly created.
async fn detect_new_paths(bucket: &str, account_id: &str, key: &str) -> DbResult<Vec<String>> {
    let conn = get_connection()?.lock().await;
    let (_, paths_to_check) = get_ancestor_paths(key);

    let mut created_paths: Vec<String> = Vec::new();

    for path in paths_to_check.iter().filter(|p| !p.is_empty()) {
        let path_str = path.as_str();
        let mut rows = conn
            .query(
                "SELECT 1 FROM directory_tree WHERE bucket = ?1 AND account_id = ?2 AND path = ?3",
                turso::params![bucket, account_id, path_str],
            )
            .await?;

        if rows.next().await?.is_none() {
            created_paths.push(path.clone());
        }
    }

    Ok(created_paths)
}

/// Check all ancestor paths and remove any that have become empty (total_file_count == 0).
/// Returns the list of removed paths.
async fn remove_empty_paths(bucket: &str, account_id: &str, key: &str) -> DbResult<Vec<String>> {
    let conn = get_connection()?.lock().await;
    let (_, paths_to_check) = get_ancestor_paths(key);

    let mut removed_paths: Vec<String> = Vec::new();

    // Check each path from deepest to shallowest (skip root "")
    for path in paths_to_check.iter().filter(|p| !p.is_empty()) {
        let path_str = path.as_str();

        // Check if this path is now empty
        let mut rows = conn
            .query(
                "SELECT total_file_count FROM directory_tree WHERE bucket = ?1 AND account_id = ?2 AND path = ?3",
                turso::params![bucket, account_id, path_str],
            )
            .await?;

        if let Some(row) = rows.next().await? {
            let total_count: i32 = row.get(0)?;

            if total_count <= 0 {
                // Remove this empty directory node
                conn.execute(
                    "DELETE FROM directory_tree WHERE bucket = ?1 AND account_id = ?2 AND path = ?3",
                    turso::params![bucket, account_id, path_str],
                ).await?;

                removed_paths.push(path.clone());
            }
        }
    }

    Ok(removed_paths)
}

/// Update directory tree for a file move/rename operation.
/// Decreases counts/sizes in old location, increases in new location.
pub async fn update_directory_tree_for_move(
    bucket: &str,
    account_id: &str,
    old_key: &str,
    new_key: &str,
    file_size: i64,
    last_modified: &str,
) -> DbResult<MoveTreeResult> {
    // Get parent paths to check if it's a same-folder rename
    let (old_parent, _) = get_ancestor_paths(old_key);
    let (new_parent, _) = get_ancestor_paths(new_key);

    // If moving within same folder, no directory tree changes needed
    if old_parent == new_parent {
        return Ok(MoveTreeResult {
            removed_paths: Vec::new(),
            created_paths: Vec::new(),
        });
    }

    // Detect which paths will be newly created for the new location
    let created_paths = detect_new_paths(bucket, account_id, new_key).await?;

    // Decrease in old location
    apply_directory_delta(bucket, account_id, old_key, -1, -file_size, None).await?;

    // Remove empty paths left behind
    let removed_paths = remove_empty_paths(bucket, account_id, old_key).await?;

    // Increase in new location
    apply_directory_delta(
        bucket,
        account_id,
        new_key,
        1,
        file_size,
        Some(last_modified),
    )
    .await?;

    Ok(MoveTreeResult {
        removed_paths,
        created_paths,
    })
}

/// Update directory tree when a file is uploaded or updated.
/// For new files: increments file counts AND updates sizes.
/// For existing files: only updates sizes (delta).
///
/// - `is_new_file`: true if the file was just created, false if overwriting existing
/// - `size_delta`: size change (new_size - old_size, or new_size if new file)
pub async fn update_directory_tree_for_file(
    bucket: &str,
    account_id: &str,
    key: &str,
    size_delta: i64,
    last_modified: &str,
    is_new_file: bool,
) -> DbResult<()> {
    let file_count_delta = if is_new_file { 1 } else { 0 };
    apply_directory_delta(
        bucket,
        account_id,
        key,
        file_count_delta,
        size_delta,
        Some(last_modified),
    )
    .await
}
