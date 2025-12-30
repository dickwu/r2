//! Directory tree builder optimized for large file sets.
//!
//! Separates tree computation from database storage and supports:
//! - Async-friendly processing with periodic yields
//! - Batch database inserts for performance
//! - Progress reporting during build

use std::collections::{BTreeSet, HashMap};
use super::{get_connection, DbResult, CachedFile};

/// Batch size for database inserts
const DB_BATCH_SIZE: usize = 500;

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
    pub file_count: i32,
    pub total_file_count: i32,
    pub size: i64,
    pub total_size: i64,
    pub last_modified: Option<String>,
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
        self.dir_map.insert(String::new(), DirNode {
            direct_files: Vec::new(),
            subdirs: BTreeSet::new(),
        });

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
            self.dir_map.get_mut("").unwrap().direct_files.push(file_info);
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
            self.dir_map.entry(current_path.clone()).or_insert_with(|| DirNode {
                direct_files: Vec::new(),
                subdirs: BTreeSet::new(),
            });

            // Link to parent
            if let Some(parent) = self.dir_map.get_mut(&parent_path) {
                parent.subdirs.insert(current_path.clone());
            }

            // Add file to direct parent
            if i == parts.len() - 2 {
                self.dir_map.get_mut(&current_path).unwrap().direct_files.push(file_info.clone());
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
        sorted_paths.sort_by(|a, b| {
            b.matches('/').count().cmp(&a.matches('/').count())
        });

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
            let direct_last_mod: Option<&str> = node.direct_files
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
            aggregates.insert(path.clone(), (total_count, total_size, last_modified.clone()));

            results.push(ComputedNode {
                path: path.clone(),
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
    pub async fn store(
        bucket: &str,
        account_id: &str,
        nodes: &[ComputedNode],
    ) -> DbResult<()> {
        let conn = get_connection()?.lock().await;
        let now = chrono::Utc::now().timestamp();

        // Clear existing
        conn.execute(
            "DELETE FROM directory_tree WHERE bucket = ?1 AND account_id = ?2",
            turso::params![bucket, account_id],
        ).await?;

        // Batch insert
        for chunk in nodes.chunks(DB_BATCH_SIZE) {
            if chunk.is_empty() {
                continue;
            }

            let placeholders: Vec<String> = chunk
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let b = i * 9;
                    format!(
                        "(?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{})",
                        b + 1, b + 2, b + 3, b + 4, b + 5, b + 6, b + 7, b + 8, b + 9
                    )
                })
                .collect();

            let sql = format!(
                "INSERT INTO directory_tree 
                 (bucket, account_id, path, file_count, total_file_count, size, total_size, last_modified, last_updated)
                 VALUES {}",
                placeholders.join(", ")
            );

            let mut params: Vec<turso::Value> = Vec::with_capacity(chunk.len() * 9);
            for node in chunk {
                params.push(bucket.to_string().into());
                params.push(account_id.to_string().into());
                params.push(node.path.clone().into());
                params.push(node.file_count.into());
                params.push(node.total_file_count.into());
                params.push(node.size.into());
                params.push(node.total_size.into());
                params.push(node.last_modified.clone().map(|s| s.into()).unwrap_or(turso::Value::Null));
                params.push(now.into());
            }

            conn.execute(&sql, params).await?;
        }

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
    let progress: Option<ProgressCallback> = progress_callback.map(|f| Box::new(f) as ProgressCallback);
    
    let builder = DirectoryTreeBuilder::new();
    let nodes = builder.build(files, progress).await;
    DirectoryTreeBuilder::store(bucket, account_id, &nodes).await
}
