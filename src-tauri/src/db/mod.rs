use std::path::Path;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use turso::{Builder, Connection};

// Wrap Connection in Mutex to serialize database access
// turso 0.4.0-pre.19 has race conditions in its page cache when accessed concurrently
static DB_CONNECTION: OnceLock<Mutex<Connection>> = OnceLock::new();

// Custom error type for database operations
pub(crate) type DbResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

// Re-export submodules
pub mod accounts;
pub mod app_state;
pub mod aws_accounts;
pub mod aws_buckets;
pub mod buckets;
pub mod cache_scope;
pub mod dir_tree;
pub mod downloads;
pub mod file_cache;
pub mod minio_accounts;
pub mod minio_buckets;
pub mod move_sessions;
pub mod prefix_sync;
pub mod rustfs_accounts;
pub mod rustfs_buckets;
pub mod sessions;
pub mod tokens;

// Re-export types
pub use accounts::Account;
pub use aws_accounts::AwsAccount;
pub use aws_buckets::AwsBucket;
pub use buckets::Bucket;
pub use downloads::DownloadSession;
pub use file_cache::{CachedDirectoryNode, CachedFile};
pub use minio_accounts::MinioAccount;
pub use minio_buckets::MinioBucket;
pub use move_sessions::MoveSession;
pub use rustfs_accounts::RustfsAccount;
pub use rustfs_buckets::RustfsBucket;
pub use sessions::UploadSession;
pub use tokens::{CurrentConfig, Token};

// ============ Connection and Initialization ============

pub(crate) fn get_connection() -> DbResult<&'static Mutex<Connection>> {
    DB_CONNECTION
        .get()
        .ok_or_else(|| "Database not initialized".into())
}

/// Initialize the database with required tables
pub async fn init_db(db_path: &Path) -> DbResult<()> {
    let db = Builder::new_local(db_path.to_str().unwrap())
        .build()
        .await?;
    let conn = db.connect()?;
    migrate_on(&conn).await?;

    if DB_CONNECTION.set(Mutex::new(conn)).is_err() {
        // Unit tests in several modules each initialize the process-wide
        // in-memory database once; the first wins and the others reuse it.
        if cfg!(test) {
            return Ok(());
        }
        return Err("Database already initialized".into());
    }

    Ok(())
}

/// Add a column a database created by an older release may lack, and say
/// whether it was added now. Only the error for a column that already exists
/// (this step ran on an earlier start) is expected; any other failure is a
/// broken migration and is surfaced.
async fn add_column_on(conn: &Connection, table: &str, column: &str) -> DbResult<bool> {
    match conn
        .execute(&format!("ALTER TABLE {table} ADD COLUMN {column}"), ())
        .await
    {
        Ok(_) => Ok(true),
        Err(error) if error.to_string().contains("duplicate column name") => Ok(false),
        Err(error) => Err(format!("Failed to add {table} column {column}: {error}").into()),
    }
}

/// Create or upgrade every table on `conn` and drop state that only a running
/// process may hold. Idempotent: it runs on every start of every release's DB.
async fn migrate_on(conn: &Connection) -> DbResult<()> {
    // Enable foreign keys
    conn.execute("PRAGMA foreign_keys = ON;", ()).await?;

    // Performance tuning for large datasets (1M+ files)
    // These are non-fatal: journal_mode and mmap_size return result rows
    // which may fail with execute(); use let _ to ignore errors.
    let _ = conn.execute("PRAGMA journal_mode = WAL;", ()).await;
    let _ = conn.execute("PRAGMA synchronous = NORMAL;", ()).await;
    let _ = conn.execute("PRAGMA cache_size = -64000;", ()).await;
    let _ = conn.execute("PRAGMA temp_store = MEMORY;", ()).await;
    let _ = conn.execute("PRAGMA mmap_size = 268435456;", ()).await;

    conn.execute_batch(&format!(
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
    ))
    .await?;

    // Create buckets and app_state tables
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS buckets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            token_id INTEGER NOT NULL REFERENCES tokens(id),
            name TEXT NOT NULL,
            public_domain TEXT,
            public_domain_scheme TEXT,
            is_public INTEGER NOT NULL DEFAULT 0,
            public_path_prefix TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(token_id, name)
        );

        CREATE INDEX IF NOT EXISTS idx_buckets_token ON buckets(token_id);
        CREATE INDEX IF NOT EXISTS idx_buckets_unique ON buckets(token_id, name);
        ",
    )
    .await?;

    // Backfill public_domain_scheme for existing R2 buckets (idempotent)
    add_column_on(conn, "buckets", "public_domain_scheme TEXT").await?;
    let _ = conn
        .execute(
            "UPDATE buckets SET public_domain_scheme = 'https' WHERE public_domain_scheme IS NULL",
            (),
        )
        .await;

    // Add explicit public-access flag + R2 public path prefix for existing DBs
    // (idempotent: the ADD COLUMN reports a duplicate once the column exists).
    add_column_on(conn, "buckets", "is_public INTEGER NOT NULL DEFAULT 0").await?;
    add_column_on(conn, "buckets", "public_path_prefix TEXT").await?;
    // Buckets that already carried a public domain were served publicly before
    // this flag existed — preserve that behavior on upgrade.
    let _ = conn
        .execute(
            "UPDATE buckets SET is_public = 1 WHERE public_domain IS NOT NULL AND public_domain <> ''",
            (),
        )
        .await;

    // Provider-specific tables
    conn.execute_batch(&format!(
        "{}{}",
        aws_accounts::get_table_sql(),
        aws_buckets::get_table_sql()
    ))
    .await?;

    conn.execute_batch(&format!(
        "{}{}",
        minio_accounts::get_table_sql(),
        minio_buckets::get_table_sql()
    ))
    .await?;

    conn.execute_batch(&format!(
        "{}{}",
        rustfs_accounts::get_table_sql(),
        rustfs_buckets::get_table_sql()
    ))
    .await?;

    // Add the explicit public-access flag to S3-family bucket tables for
    // existing DBs (idempotent). Backfill: any bucket that already had a custom
    // public domain host was served publicly before the flag existed.
    for table in ["aws_buckets", "minio_buckets", "rustfs_buckets"] {
        add_column_on(conn, table, "is_public INTEGER NOT NULL DEFAULT 0").await?;
        add_column_on(conn, table, "public_path_prefix TEXT").await?;
        let _ = conn
            .execute(
                &format!(
                    "UPDATE {table} SET is_public = 1 WHERE public_domain_host IS NOT NULL AND public_domain_host <> ''"
                ),
                (),
            )
            .await;
    }

    // Create app_state table
    conn.execute_batch(app_state::get_table_sql()).await?;

    // Create file cache tables
    cache_scope::prepare_cache_schema_on(conn).await?;
    conn.execute_batch(file_cache::get_table_sql()).await?;

    // Create download sessions table
    conn.execute_batch(downloads::get_table_sql()).await?;

    // Create move sessions tables (including task-level retry scheduling)
    conn.execute_batch(move_sessions::get_table_sql()).await?;

    // Create prefix sync times table (for lazy sync)
    conn.execute_batch(prefix_sync::get_table_sql()).await?;
    add_column_on(conn, "sync_meta", "generation INTEGER NOT NULL DEFAULT 0").await?;
    add_column_on(
        conn,
        "prefix_sync_times",
        "generation INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    add_column_on(
        conn,
        "prefix_sync_times",
        "listed_at INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    // Markers from before this column: one that was ever fresh came from a
    // listing, so its rows are that folder's snapshot. One that never was may
    // have been left by a write on a folder never listed, and is not trusted.
    if add_column_on(
        conn,
        "prefix_sync_times",
        "complete INTEGER NOT NULL DEFAULT 0",
    )
    .await?
    {
        conn.execute(
            "UPDATE prefix_sync_times SET complete = 1 WHERE last_synced_at > 0 OR listed_at > 0",
            (),
        )
        .await?;
    }
    cache_scope::initialize_on(conn).await?;
    file_cache::clear_interrupted_work_on(conn).await?;
    Ok(())
}

/// Initialize the process-wide database for unit tests that exercise code
/// paths reaching it through `get_connection`.
#[cfg(test)]
pub(crate) async fn init_test_db() {
    static INITIALIZED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    INITIALIZED
        .get_or_init(|| async { init_db(Path::new(":memory:")).await.unwrap() })
        .await;
}

// Re-export session functions
pub use sessions::{
    cleanup_old_sessions, create_session, delete_session, find_resumable_session,
    get_completed_parts, get_pending_sessions, get_session, save_completed_part,
    update_session_status,
};

// Re-export token functions
pub use tokens::{
    create_token, delete_token, get_current_config, get_token, list_tokens_by_account,
    set_current_aws_selection, set_current_minio_selection, set_current_rustfs_selection,
    set_current_selection, update_token,
};

// Re-export app_state functions
pub use app_state::set_app_state;

// Re-export account functions
pub use accounts::{create_account, delete_account, has_accounts, list_accounts, update_account};
// Re-export bucket functions
pub use buckets::{delete_bucket, list_buckets_by_token, save_buckets_for_token, update_bucket};
// Re-export AWS provider functions
pub use aws_accounts::{
    create_aws_account, delete_aws_account, list_aws_accounts, update_aws_account,
};
pub use aws_buckets::{list_aws_buckets_by_account, save_aws_buckets_for_account};
// Re-export MinIO provider functions
pub use minio_accounts::{
    create_minio_account, delete_minio_account, list_minio_accounts, update_minio_account,
};
pub use minio_buckets::{list_minio_buckets_by_account, save_minio_buckets_for_account};
// Re-export RustFS provider functions
pub use rustfs_accounts::{
    create_rustfs_account, delete_rustfs_account, list_rustfs_accounts, update_rustfs_account,
};
pub use rustfs_buckets::{list_rustfs_buckets_by_account, save_rustfs_buckets_for_account};
// Re-export file cache functions
pub use file_cache::{
    begin_local_cache_mutation, begin_sync, calculate_folder_size, clear_file_cache,
    delete_cached_file, delete_cached_files_batch, finish_local_cache_mutation,
    finish_sync_with_metadata, get_all_cached_files, get_all_directory_nodes, get_bucket_summary,
    get_cached_file_size, get_directory_node, get_directory_nodes, get_folder_contents,
    move_cached_file, parse_key, search_cached_files, store_file_batch, update_cached_file,
};
// Re-export directory tree builder
pub use dir_tree::{
    build_directory_tree_from_db, update_directory_tree_for_delete,
    update_directory_tree_for_delete_batch, update_directory_tree_for_file,
    update_directory_tree_for_move,
};
// Re-export download session functions
pub use downloads::{
    count_active_downloads, create_download_session, create_download_sessions_batch,
    delete_all_downloads, delete_download_session, delete_finished_downloads,
    get_download_sessions_for_bucket, get_pending_downloads, pause_all_downloads,
    resume_all_downloads, update_download_file_size, update_download_progress,
    update_download_status,
};
// Re-export move session functions
pub use move_sessions::{
    count_active_moves, count_in_progress_moves, create_move_sessions_batch, delete_all_moves,
    delete_finished_moves, delete_move_session, delete_move_upload_parts,
    get_all_active_move_sessions, get_move_sessions_for_source, get_move_upload_parts,
    get_move_upload_session, get_pending_moves_for_source, pause_all_moves, resume_all_moves,
    save_move_upload_part, save_move_upload_session, update_move_progress, update_move_status,
    update_move_status_and_progress,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// DDL v0.3.5's init_db ran, frozen at that release.
    const V0_3_5_SCHEMA: &str = include_str!("testdata/v0.3.5-schema.sql");

    /// What a v0.3.5 user's database holds: accounts, bucket settings, a synced
    /// and a lazily listed cache (one folder still fresh, one written to since
    /// it was listed), resumable transfers and app settings.
    const V0_3_5_ROWS: &str = "
        INSERT INTO accounts (id, name, created_at, updated_at) VALUES ('acct', 'Main', 1, 1);
        INSERT INTO tokens (id, account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at)
            VALUES (1, 'acct', 'rw', 'api', 'ak', 'sk', 1, 1);
        INSERT INTO buckets (token_id, name, public_domain, public_domain_scheme, is_public, public_path_prefix, created_at, updated_at)
            VALUES (1, 'photos', 'cdn.example.com', 'https', 1, 'assets', 1, 1);
        INSERT INTO minio_accounts (id, name, access_key_id, secret_access_key, endpoint_scheme, endpoint_host, force_path_style, created_at, updated_at)
            VALUES ('minio', NULL, 'mk', 'ms', 'http', 'minio.local:9000', 1, 1, 1);
        INSERT INTO minio_buckets (account_id, name, public_domain_scheme, public_domain_host, is_public, public_path_prefix, created_at, updated_at)
            VALUES ('minio', 'data', NULL, NULL, 0, NULL, 1, 1);
        INSERT INTO cached_files VALUES
            ('photos', 'acct', 'a/1.jpg', 'a/', '1.jpg', 10, '2025-01-01', 5),
            ('photos', 'acct', 'root.txt', '', 'root.txt', 3, '2025-01-02', 5);
        INSERT INTO directory_tree VALUES
            ('photos', 'acct', '', '', 1, 2, 3, 13, '2025-01-02', 5),
            ('photos', 'acct', 'a/', '', 1, 1, 10, 10, '2025-01-01', 5);
        INSERT INTO sync_meta (bucket, account_id, last_sync, file_count) VALUES ('photos', 'acct', 5, 2);
        INSERT INTO prefix_sync_times (bucket, account_id, prefix, last_synced_at, file_count, folder_count)
            VALUES ('photos', 'acct', 'a/', 6, 1, 0), ('photos', 'acct', 'b/', 0, 0, 0);
        INSERT INTO cache_scopes (account_id, provider, fingerprint, revision) VALUES ('acct', 'r2', 'fp', 3);
        INSERT INTO upload_sessions (id, file_path, file_size, file_mtime, object_key, bucket, account_id, upload_id, content_type, total_parts, created_at, updated_at, status)
            VALUES ('up', '/tmp/big.bin', 100, 1, 'big.bin', 'photos', 'acct', 'mpu', 'application/octet-stream', 2, 1, 1, 'uploading');
        INSERT INTO completed_parts (session_id, part_number, etag) VALUES ('up', 1, 'etag-1');
        INSERT INTO download_sessions (id, object_key, file_name, file_size, downloaded_bytes, local_path, bucket, account_id, status, created_at, updated_at)
            VALUES ('down', 'a/1.jpg', '1.jpg', 10, 4, '/tmp/1.jpg', 'photos', 'acct', 'paused', 1, 1);
        INSERT INTO move_sessions (id, source_key, dest_key, source_bucket, source_account_id, source_provider, dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress, status, created_at, updated_at)
            VALUES ('mv', 'a/1.jpg', 'b/1.jpg', 'photos', 'acct', 'r2', 'data', 'minio', 'minio', 1, 10, 50, 'paused', 1, 1);
        INSERT INTO move_upload_sessions (task_id, upload_id, part_size) VALUES ('mv', 'mv-upload', 8);
        INSERT INTO move_upload_parts (task_id, part_number, etag, size) VALUES ('mv', 1, 'mv-etag', 8);
        INSERT INTO move_journal (task_id, data) VALUES ('mv', '{}');
        INSERT INTO app_state (key, value) VALUES
            ('cache_scope_schema', '1'),
            ('current_config', 'acct'),
            ('skipped_prefixes:acct:photos', '[\"broken/\"]');
    ";

    /// Every v0.3.5 row, read through v0.3.5's own columns.
    const V0_3_5_DATA: [&str; 23] = [
        "SELECT * FROM upload_sessions",
        "SELECT * FROM completed_parts",
        "SELECT * FROM accounts",
        "SELECT * FROM tokens",
        "SELECT * FROM buckets",
        "SELECT * FROM aws_accounts",
        "SELECT * FROM aws_buckets",
        "SELECT * FROM minio_accounts",
        "SELECT * FROM minio_buckets",
        "SELECT * FROM rustfs_accounts",
        "SELECT * FROM rustfs_buckets",
        "SELECT * FROM app_state ORDER BY key",
        "SELECT * FROM cached_files ORDER BY key",
        "SELECT * FROM directory_tree ORDER BY path",
        "SELECT * FROM cached_files_staging",
        "SELECT bucket, account_id, last_sync, file_count FROM sync_meta",
        "SELECT * FROM download_sessions",
        "SELECT * FROM move_sessions",
        "SELECT * FROM move_upload_sessions",
        "SELECT * FROM move_upload_parts",
        "SELECT * FROM move_journal",
        "SELECT bucket, account_id, prefix, last_synced_at, file_count, folder_count FROM prefix_sync_times",
        "SELECT * FROM cache_scopes",
    ];

    async fn rows(conn: &Connection, sql: &str) -> Vec<String> {
        let mut rows = conn.query(sql, ()).await.unwrap();
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let values: Vec<String> = (0..row.column_count())
                .map(|index| format!("{:?}", row.get_value(index).unwrap()))
                .collect();
            out.push(values.join("|"));
        }
        out
    }

    async fn data(conn: &Connection) -> Vec<Vec<String>> {
        let mut data = Vec::new();
        for sql in V0_3_5_DATA {
            data.push(rows(conn, sql).await);
        }
        data
    }

    async fn schema(conn: &Connection) -> Vec<String> {
        rows(
            conn,
            "SELECT type, name, sql FROM sqlite_master ORDER BY type, name",
        )
        .await
    }

    async fn open(path: &Path) -> (turso::Database, Connection) {
        let db = Builder::new_local(path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        (db, conn)
    }

    #[tokio::test]
    async fn v0_3_5_database_migrates_idempotently_and_keeps_its_data() {
        let path = std::env::temp_dir().join(format!(
            "r2-migrate-v0.3.5-{}-{}.db",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let (db, conn) = open(&path).await;
        conn.execute_batch(V0_3_5_SCHEMA).await.unwrap();
        conn.execute_batch(V0_3_5_ROWS).await.unwrap();
        let before = data(&conn).await;
        assert_eq!(before[12].len(), 2, "the fixture holds cached rows");

        migrate_on(&conn).await.unwrap();

        assert_eq!(data(&conn).await, before);
        assert_eq!(
            rows(&conn, "SELECT generation FROM sync_meta").await,
            vec!["Integer(0)".to_string()]
        );
        // A marker that was ever fresh came from a listing: its rows are that
        // folder's snapshot. One that never was is not trusted as one.
        assert_eq!(
            rows(
                &conn,
                "SELECT prefix, generation, listed_at, complete FROM prefix_sync_times ORDER BY prefix"
            )
            .await,
            vec![
                "Text(\"a/\")|Integer(0)|Integer(0)|Integer(1)".to_string(),
                "Text(\"b/\")|Integer(0)|Integer(0)|Integer(0)".into()
            ]
        );
        let names = rows(
            &conn,
            "SELECT name FROM sqlite_master WHERE type IN ('table', 'index') ORDER BY name",
        )
        .await;
        for added in [
            "bucket_content_revisions",
            "idx_cached_files_parent_key",
            "idx_directory_tree_parent_path",
            "move_task_retries",
            "prefix_mutation_generations",
            "sync_mutation_journal",
        ] {
            assert!(names.contains(&format!("Text(\"{added}\")")), "{added}");
        }
        for superseded in ["idx_cached_files_parent", "idx_directory_tree_parent"] {
            assert!(
                !names.contains(&format!("Text(\"{superseded}\")")),
                "{superseded}"
            );
        }
        // The migrated cache serves pages through the new keyset index.
        let page = file_cache::cached_folder_page_on(&conn, "photos", "acct", "a/", None, None, 10)
            .await
            .unwrap();
        assert_eq!(page.files[0].key, "a/1.jpg");

        // A second start on the same database changes nothing.
        let migrated_schema = schema(&conn).await;
        migrate_on(&conn).await.unwrap();
        assert_eq!(schema(&conn).await, migrated_schema);
        assert_eq!(data(&conn).await, before);

        drop(conn);
        drop(db);
        let (db, conn) = open(&path).await;
        migrate_on(&conn).await.unwrap();
        assert_eq!(schema(&conn).await, migrated_schema);
        assert_eq!(data(&conn).await, before);
        drop(conn);
        drop(db);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[tokio::test]
    async fn column_migrations_tolerate_only_an_existing_column() {
        let db = Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE sample (id INTEGER)", ())
            .await
            .unwrap();

        assert!(add_column_on(&conn, "sample", "extra TEXT").await.unwrap());
        assert!(!add_column_on(&conn, "sample", "extra TEXT").await.unwrap());
        // Any other failure, such as a table that was never created, surfaces.
        let missing = add_column_on(&conn, "absent", "extra TEXT").await;
        assert!(missing
            .unwrap_err()
            .to_string()
            .contains("no such table: absent"));
    }
}
