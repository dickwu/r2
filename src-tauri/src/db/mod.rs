use turso::{Builder, Connection};
use std::path::Path;
use std::sync::OnceLock;
use tokio::sync::Mutex;

// Wrap Connection in Mutex to serialize database access
// turso 0.4.0-pre.19 has race conditions in its page cache when accessed concurrently
static DB_CONNECTION: OnceLock<Mutex<Connection>> = OnceLock::new();

// Custom error type for database operations
pub(crate) type DbResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

// Re-export submodules
pub mod accounts;
pub mod buckets;
pub mod sessions;
pub mod tokens;
pub mod file_cache;
pub mod app_state;
pub mod dir_tree;

// Re-export types
pub use accounts::Account;
pub use buckets::Bucket;
pub use sessions::UploadSession;
pub use tokens::{Token, CurrentConfig};
pub use file_cache::{CachedFile, CachedDirectoryNode};

// ============ Connection and Initialization ============

pub(crate) fn get_connection() -> DbResult<&'static Mutex<Connection>> {
    DB_CONNECTION.get().ok_or_else(|| "Database not initialized".into())
}

/// Initialize the database with required tables
pub async fn init_db(db_path: &Path) -> DbResult<()> {
    let db = Builder::new_local(db_path.to_str().unwrap()).build().await?;
    let conn = db.connect()?;
    
    // Enable foreign keys
    conn.execute("PRAGMA foreign_keys = ON;", ()).await?;
    
    conn.execute_batch(
        &format!(
            "{}{}{}",
            sessions::get_table_sql(),
            "
        -- Multi-account tables
        CREATE TABLE IF NOT EXISTS accounts (
            id TEXT PRIMARY KEY,
            name TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        ",
            tokens::get_table_sql()
        )
    ).await?;
    
    // Create buckets and app_state tables
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS buckets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            token_id INTEGER NOT NULL REFERENCES tokens(id),
            name TEXT NOT NULL,
            public_domain TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(token_id, name)
        );

        CREATE INDEX IF NOT EXISTS idx_buckets_token ON buckets(token_id);
        "
    ).await?;
    
    // Create app_state table
    conn.execute_batch(app_state::get_table_sql()).await?;
    
    // Create file cache tables
    conn.execute_batch(file_cache::get_table_sql()).await?;

    DB_CONNECTION.set(Mutex::new(conn)).map_err(|_| "Database already initialized")?;
    
    Ok(())
}

// Re-export session functions
pub use sessions::{
    create_session, update_session_status, find_resumable_session, get_session,
    save_completed_part, get_completed_parts, delete_session, get_pending_sessions,
    cleanup_old_sessions,
};

// Re-export token functions
pub use tokens::{
    create_token, get_token, list_tokens_by_account, update_token, delete_token,
    get_current_config, set_current_selection,
};

// Re-export app_state functions
pub use app_state::{get_app_state, set_app_state, delete_app_state};

// Re-export account functions
pub use accounts::{create_account, delete_account, list_accounts, update_account, has_accounts};
// Re-export bucket functions
pub use buckets::{create_bucket, delete_bucket, list_buckets_by_token, update_bucket, save_buckets_for_token};
// Re-export file cache functions
pub use file_cache::{
    store_all_files, get_all_cached_files, search_cached_files, calculate_folder_size, 
    get_directory_node, get_all_directory_nodes, clear_file_cache,
};
// Re-export directory tree builder
pub use dir_tree::{build_directory_tree};