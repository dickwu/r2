use libsql::{Builder, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::OnceLock;

static DB_CONNECTION: OnceLock<Connection> = OnceLock::new();

// Custom error type for database operations
type DbResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

// ============ Upload Session Structs ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadSession {
    pub id: String,
    pub file_path: String,
    pub file_size: i64,
    pub file_mtime: i64,
    pub object_key: String,
    pub bucket: String,
    pub account_id: String,
    pub upload_id: Option<String>,
    pub content_type: String,
    pub total_parts: i32,
    pub created_at: i64,
    pub updated_at: i64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedPart {
    pub session_id: String,
    pub part_number: i32,
    pub etag: String,
}

// ============ Multi-Account Structs ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,  // Cloudflare account_id
    pub name: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub id: i64,
    pub account_id: String,
    pub name: Option<String>,
    pub api_token: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bucket {
    pub id: i64,
    pub token_id: i64,
    pub name: String,
    pub public_domain: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Full configuration needed for R2 operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentConfig {
    pub account_id: String,
    pub account_name: Option<String>,
    pub token_id: i64,
    pub token_name: Option<String>,
    pub api_token: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub public_domain: Option<String>,
}

/// Initialize the database with required tables
pub async fn init_db(db_path: &Path) -> DbResult<()> {
    let db = Builder::new_local(db_path).build().await?;
    let conn = db.connect()?;
    
    // Enable foreign keys
    conn.execute("PRAGMA foreign_keys = ON;", ()).await?;
    
    conn.execute_batch(
        "
        -- Upload sessions tables
        CREATE TABLE IF NOT EXISTS upload_sessions (
            id TEXT PRIMARY KEY,
            file_path TEXT NOT NULL,
            file_size INTEGER NOT NULL,
            file_mtime INTEGER NOT NULL,
            object_key TEXT NOT NULL,
            bucket TEXT NOT NULL,
            account_id TEXT NOT NULL,
            upload_id TEXT,
            content_type TEXT NOT NULL,
            total_parts INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            status TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS completed_parts (
            session_id TEXT NOT NULL,
            part_number INTEGER NOT NULL,
            etag TEXT NOT NULL,
            PRIMARY KEY (session_id, part_number),
            FOREIGN KEY (session_id) REFERENCES upload_sessions(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_sessions_status ON upload_sessions(status);
        CREATE INDEX IF NOT EXISTS idx_sessions_file ON upload_sessions(file_path, file_size, file_mtime);

        -- Multi-account tables
        CREATE TABLE IF NOT EXISTS accounts (
            id TEXT PRIMARY KEY,
            name TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tokens (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            name TEXT,
            api_token TEXT NOT NULL,
            access_key_id TEXT NOT NULL,
            secret_access_key TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS buckets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            token_id INTEGER NOT NULL REFERENCES tokens(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            public_domain TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(token_id, name)
        );

        CREATE TABLE IF NOT EXISTS app_state (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_tokens_account ON tokens(account_id);
        CREATE INDEX IF NOT EXISTS idx_buckets_token ON buckets(token_id);

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
    ).await?;

    // Migration: Add last_modified column to directory_tree if it doesn't exist
    // SQLite doesn't have IF NOT EXISTS for columns, so we check and ignore errors
    let _ = conn.execute(
        "ALTER TABLE directory_tree ADD COLUMN last_modified TEXT",
        (),
    ).await;

    DB_CONNECTION.set(conn).map_err(|_| "Database already initialized")?;
    
    Ok(())
}

fn get_connection() -> DbResult<&'static Connection> {
    DB_CONNECTION.get().ok_or_else(|| "Database not initialized".into())
}

/// Create a new upload session
pub async fn create_session(session: &UploadSession) -> DbResult<()> {
    let conn = get_connection()?;
    conn.execute(
        "INSERT INTO upload_sessions 
         (id, file_path, file_size, file_mtime, object_key, bucket, account_id, 
          upload_id, content_type, total_parts, created_at, updated_at, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        libsql::params![
            session.id.clone(),
            session.file_path.clone(),
            session.file_size,
            session.file_mtime,
            session.object_key.clone(),
            session.bucket.clone(),
            session.account_id.clone(),
            session.upload_id.clone(),
            session.content_type.clone(),
            session.total_parts,
            session.created_at,
            session.updated_at,
            session.status.clone(),
        ],
    ).await?;
    Ok(())
}

/// Update session status
pub async fn update_session_status(session_id: &str, status: &str) -> DbResult<()> {
    let conn = get_connection()?;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE upload_sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
        libsql::params![status, now, session_id],
    ).await?;
    Ok(())
}

/// Find existing session for a file (by path, size, mtime and target key)
pub async fn find_resumable_session(
    file_path: &str,
    file_size: i64,
    file_mtime: i64,
    object_key: &str,
    bucket: &str,
    account_id: &str,
) -> DbResult<Option<UploadSession>> {
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT id, file_path, file_size, file_mtime, object_key, bucket, account_id,
                upload_id, content_type, total_parts, created_at, updated_at, status
         FROM upload_sessions 
         WHERE file_path = ?1 AND file_size = ?2 AND file_mtime = ?3 
               AND object_key = ?4 AND bucket = ?5 AND account_id = ?6
               AND status = 'uploading' AND upload_id IS NOT NULL",
        libsql::params![file_path, file_size, file_mtime, object_key, bucket, account_id]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(UploadSession {
            id: row.get(0)?,
            file_path: row.get(1)?,
            file_size: row.get(2)?,
            file_mtime: row.get(3)?,
            object_key: row.get(4)?,
            bucket: row.get(5)?,
            account_id: row.get(6)?,
            upload_id: row.get(7)?,
            content_type: row.get(8)?,
            total_parts: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
            status: row.get(12)?,
        }))
    } else {
        Ok(None)
    }
}

/// Get session by ID
pub async fn get_session(session_id: &str) -> DbResult<Option<UploadSession>> {
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT id, file_path, file_size, file_mtime, object_key, bucket, account_id,
                upload_id, content_type, total_parts, created_at, updated_at, status
         FROM upload_sessions WHERE id = ?1",
        libsql::params![session_id]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(UploadSession {
            id: row.get(0)?,
            file_path: row.get(1)?,
            file_size: row.get(2)?,
            file_mtime: row.get(3)?,
            object_key: row.get(4)?,
            bucket: row.get(5)?,
            account_id: row.get(6)?,
            upload_id: row.get(7)?,
            content_type: row.get(8)?,
            total_parts: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
            status: row.get(12)?,
        }))
    } else {
        Ok(None)
    }
}

/// Save a completed part
pub async fn save_completed_part(session_id: &str, part_number: i32, etag: &str) -> DbResult<()> {
    let conn = get_connection()?;
    conn.execute(
        "INSERT OR REPLACE INTO completed_parts (session_id, part_number, etag)
         VALUES (?1, ?2, ?3)",
        libsql::params![session_id, part_number, etag],
    ).await?;
    
    // Update session timestamp
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE upload_sessions SET updated_at = ?1 WHERE id = ?2",
        libsql::params![now, session_id],
    ).await?;
    
    Ok(())
}

/// Get all completed parts for a session
pub async fn get_completed_parts(session_id: &str) -> DbResult<Vec<CompletedPart>> {
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT session_id, part_number, etag FROM completed_parts 
         WHERE session_id = ?1 ORDER BY part_number",
        libsql::params![session_id]
    ).await?;
    
    let mut parts = Vec::new();
    while let Some(row) = rows.next().await? {
        parts.push(CompletedPart {
            session_id: row.get(0)?,
            part_number: row.get(1)?,
            etag: row.get(2)?,
        });
    }
    Ok(parts)
}

/// Delete session and its parts
pub async fn delete_session(session_id: &str) -> DbResult<()> {
    let conn = get_connection()?;
    conn.execute("DELETE FROM completed_parts WHERE session_id = ?1", libsql::params![session_id]).await?;
    conn.execute("DELETE FROM upload_sessions WHERE id = ?1", libsql::params![session_id]).await?;
    Ok(())
}

/// Get all pending/uploading sessions (for UI to show resumable uploads)
pub async fn get_pending_sessions() -> DbResult<Vec<UploadSession>> {
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT id, file_path, file_size, file_mtime, object_key, bucket, account_id,
                upload_id, content_type, total_parts, created_at, updated_at, status
         FROM upload_sessions 
         WHERE status IN ('pending', 'uploading')
         ORDER BY updated_at DESC",
        ()
    ).await?;
    
    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await? {
        sessions.push(UploadSession {
            id: row.get(0)?,
            file_path: row.get(1)?,
            file_size: row.get(2)?,
            file_mtime: row.get(3)?,
            object_key: row.get(4)?,
            bucket: row.get(5)?,
            account_id: row.get(6)?,
            upload_id: row.get(7)?,
            content_type: row.get(8)?,
            total_parts: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
            status: row.get(12)?,
        });
    }
    Ok(sessions)
}

/// Clean up old completed/failed sessions (older than 7 days)
pub async fn cleanup_old_sessions() -> DbResult<usize> {
    let conn = get_connection()?;
    let cutoff = chrono::Utc::now().timestamp() - (7 * 24 * 60 * 60);
    
    // First delete parts
    conn.execute(
        "DELETE FROM completed_parts WHERE session_id IN 
         (SELECT id FROM upload_sessions WHERE status IN ('completed', 'failed', 'cancelled') 
          AND updated_at < ?1)",
        libsql::params![cutoff],
    ).await?;
    
    // Then delete sessions
    let deleted = conn.execute(
        "DELETE FROM upload_sessions 
         WHERE status IN ('completed', 'failed', 'cancelled') AND updated_at < ?1",
        libsql::params![cutoff],
    ).await?;
    
    Ok(deleted as usize)
}

// ============ Account CRUD Functions ============

/// Create a new account
pub async fn create_account(id: &str, name: Option<&str>) -> DbResult<Account> {
    let conn = get_connection()?;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO accounts (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
        libsql::params![id, name, now, now],
    ).await?;
    Ok(Account {
        id: id.to_string(),
        name: name.map(|s| s.to_string()),
        created_at: now,
        updated_at: now,
    })
}

/// Get account by ID
#[allow(dead_code)]
pub async fn get_account(id: &str) -> DbResult<Option<Account>> {
    let conn = get_connection()?;
    let mut rows = conn.query("SELECT id, name, created_at, updated_at FROM accounts WHERE id = ?1", libsql::params![id]).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(Account {
            id: row.get(0)?,
            name: row.get(1)?,
            created_at: row.get(2)?,
            updated_at: row.get(3)?,
        }))
    } else {
        Ok(None)
    }
}

/// List all accounts
pub async fn list_accounts() -> DbResult<Vec<Account>> {
    let conn = get_connection()?;
    let mut rows = conn.query("SELECT id, name, created_at, updated_at FROM accounts ORDER BY created_at", ()).await?;
    
    let mut accounts = Vec::new();
    while let Some(row) = rows.next().await? {
        accounts.push(Account {
            id: row.get(0)?,
            name: row.get(1)?,
            created_at: row.get(2)?,
            updated_at: row.get(3)?,
        });
    }
    Ok(accounts)
}

/// Update account
pub async fn update_account(id: &str, name: Option<&str>) -> DbResult<()> {
    let conn = get_connection()?;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE accounts SET name = ?1, updated_at = ?2 WHERE id = ?3",
        libsql::params![name, now, id],
    ).await?;
    Ok(())
}

/// Delete account (cascades to tokens and buckets)
pub async fn delete_account(id: &str) -> DbResult<()> {
    let conn = get_connection()?;
    conn.execute("DELETE FROM accounts WHERE id = ?1", libsql::params![id]).await?;
    Ok(())
}

// ============ Token CRUD Functions ============

/// Create a new token
pub async fn create_token(
    account_id: &str,
    name: Option<&str>,
    api_token: &str,
    access_key_id: &str,
    secret_access_key: &str,
) -> DbResult<Token> {
    let conn = get_connection()?;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO tokens (account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        libsql::params![account_id, name, api_token, access_key_id, secret_access_key, now, now],
    ).await?;
    let id = conn.last_insert_rowid();
    Ok(Token {
        id,
        account_id: account_id.to_string(),
        name: name.map(|s| s.to_string()),
        api_token: api_token.to_string(),
        access_key_id: access_key_id.to_string(),
        secret_access_key: secret_access_key.to_string(),
        created_at: now,
        updated_at: now,
    })
}

/// Get token by ID
pub async fn get_token(id: i64) -> DbResult<Option<Token>> {
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT id, account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at
         FROM tokens WHERE id = ?1",
        libsql::params![id]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(Token {
            id: row.get(0)?,
            account_id: row.get(1)?,
            name: row.get(2)?,
            api_token: row.get(3)?,
            access_key_id: row.get(4)?,
            secret_access_key: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        }))
    } else {
        Ok(None)
    }
}

/// List tokens by account
pub async fn list_tokens_by_account(account_id: &str) -> DbResult<Vec<Token>> {
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT id, account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at
         FROM tokens WHERE account_id = ?1 ORDER BY created_at",
        libsql::params![account_id]
    ).await?;
    
    let mut tokens = Vec::new();
    while let Some(row) = rows.next().await? {
        tokens.push(Token {
            id: row.get(0)?,
            account_id: row.get(1)?,
            name: row.get(2)?,
            api_token: row.get(3)?,
            access_key_id: row.get(4)?,
            secret_access_key: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        });
    }
    Ok(tokens)
}

/// Update token
pub async fn update_token(
    id: i64,
    name: Option<&str>,
    api_token: &str,
    access_key_id: &str,
    secret_access_key: &str,
) -> DbResult<()> {
    let conn = get_connection()?;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE tokens SET name = ?1, api_token = ?2, access_key_id = ?3, secret_access_key = ?4, updated_at = ?5
         WHERE id = ?6",
        libsql::params![name, api_token, access_key_id, secret_access_key, now, id],
    ).await?;
    Ok(())
}

/// Delete token (cascades to buckets)
pub async fn delete_token(id: i64) -> DbResult<()> {
    let conn = get_connection()?;
    conn.execute("DELETE FROM tokens WHERE id = ?1", libsql::params![id]).await?;
    Ok(())
}

// ============ Bucket CRUD Functions ============

/// Create a new bucket
#[allow(dead_code)]
pub async fn create_bucket(token_id: i64, name: &str, public_domain: Option<&str>) -> DbResult<Bucket> {
    let conn = get_connection()?;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO buckets (token_id, name, public_domain, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![token_id, name, public_domain, now, now],
    ).await?;
    let id = conn.last_insert_rowid();
    Ok(Bucket {
        id,
        token_id,
        name: name.to_string(),
        public_domain: public_domain.map(|s| s.to_string()),
        created_at: now,
        updated_at: now,
    })
}

/// Get bucket by ID
#[allow(dead_code)]
pub async fn get_bucket(id: i64) -> DbResult<Option<Bucket>> {
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT id, token_id, name, public_domain, created_at, updated_at FROM buckets WHERE id = ?1",
        libsql::params![id]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(Bucket {
            id: row.get(0)?,
            token_id: row.get(1)?,
            name: row.get(2)?,
            public_domain: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        }))
    } else {
        Ok(None)
    }
}

/// List buckets by token
pub async fn list_buckets_by_token(token_id: i64) -> DbResult<Vec<Bucket>> {
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT id, token_id, name, public_domain, created_at, updated_at
         FROM buckets WHERE token_id = ?1 ORDER BY name",
        libsql::params![token_id]
    ).await?;
    
    let mut buckets = Vec::new();
    while let Some(row) = rows.next().await? {
        buckets.push(Bucket {
            id: row.get(0)?,
            token_id: row.get(1)?,
            name: row.get(2)?,
            public_domain: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        });
    }
    Ok(buckets)
}

/// Update bucket
pub async fn update_bucket(id: i64, public_domain: Option<&str>) -> DbResult<()> {
    let conn = get_connection()?;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE buckets SET public_domain = ?1, updated_at = ?2 WHERE id = ?3",
        libsql::params![public_domain, now, id],
    ).await?;
    Ok(())
}

/// Delete bucket
pub async fn delete_bucket(id: i64) -> DbResult<()> {
    let conn = get_connection()?;
    conn.execute("DELETE FROM buckets WHERE id = ?1", libsql::params![id]).await?;
    Ok(())
}

/// Save multiple buckets for a token (replace existing)
pub async fn save_buckets_for_token(token_id: i64, buckets: &[(String, Option<String>)]) -> DbResult<Vec<Bucket>> {
    let conn = get_connection()?;
    let now = chrono::Utc::now().timestamp();
    
    // Delete existing buckets for this token
    conn.execute("DELETE FROM buckets WHERE token_id = ?1", libsql::params![token_id]).await?;
    
    // Insert new buckets
    let mut result = Vec::new();
    for (name, public_domain) in buckets {
        conn.execute(
            "INSERT INTO buckets (token_id, name, public_domain, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![token_id, name.clone(), public_domain.clone(), now, now],
        ).await?;
        let id = conn.last_insert_rowid();
        result.push(Bucket {
            id,
            token_id,
            name: name.clone(),
            public_domain: public_domain.clone(),
            created_at: now,
            updated_at: now,
        });
    }
    Ok(result)
}

// ============ App State Functions ============

/// Get app state value
#[allow(dead_code)]
pub async fn get_app_state(key: &str) -> DbResult<Option<String>> {
    let conn = get_connection()?;
    let mut rows = conn.query("SELECT value FROM app_state WHERE key = ?1", libsql::params![key]).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

/// Set app state value
pub async fn set_app_state(key: &str, value: &str) -> DbResult<()> {
    let conn = get_connection()?;
    conn.execute(
        "INSERT OR REPLACE INTO app_state (key, value) VALUES (?1, ?2)",
        libsql::params![key, value],
    ).await?;
    Ok(())
}

/// Delete app state value
#[allow(dead_code)]
pub async fn delete_app_state(key: &str) -> DbResult<()> {
    let conn = get_connection()?;
    conn.execute("DELETE FROM app_state WHERE key = ?1", libsql::params![key]).await?;
    Ok(())
}

// ============ Combined Config Functions ============

/// Get current configuration (combines token + bucket info)
pub async fn get_current_config() -> DbResult<Option<CurrentConfig>> {
    let conn = get_connection()?;
    
    // Get current token ID
    let mut rows = conn.query("SELECT value FROM app_state WHERE key = 'current_token_id'", ()).await?;
    
    let token_id: i64 = if let Some(row) = rows.next().await? {
        let value: String = row.get(0)?;
        value.parse().map_err(|_| "Invalid token ID")?
    } else {
        return Ok(None);
    };
    
    // Get current bucket
    let mut rows = conn.query("SELECT value FROM app_state WHERE key = 'current_bucket'", ()).await?;
    
    let bucket_name: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Ok(None);
    };
    
    // Get token with account info
    let mut rows = conn.query(
        "SELECT t.id, t.account_id, t.name, t.api_token, t.access_key_id, t.secret_access_key,
                a.name as account_name
         FROM tokens t
         JOIN accounts a ON t.account_id = a.id
         WHERE t.id = ?1",
        libsql::params![token_id]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        // Get bucket's public domain
        let mut bucket_rows = conn.query(
            "SELECT public_domain FROM buckets WHERE token_id = ?1 AND name = ?2",
            libsql::params![token_id, bucket_name.clone()]
        ).await?;
        let public_domain: Option<String> = if let Some(bucket_row) = bucket_rows.next().await? {
            bucket_row.get(0)?
        } else {
            None
        };
        
        Ok(Some(CurrentConfig {
            account_id: row.get(1)?,
            account_name: row.get(6)?,
            token_id: row.get(0)?,
            token_name: row.get(2)?,
            api_token: row.get(3)?,
            access_key_id: row.get(4)?,
            secret_access_key: row.get(5)?,
            bucket: bucket_name,
            public_domain,
        }))
    } else {
        Ok(None)
    }
}

/// Set current token and bucket
pub async fn set_current_selection(token_id: i64, bucket_name: &str) -> DbResult<()> {
    let conn = get_connection()?;
    conn.execute(
        "INSERT OR REPLACE INTO app_state (key, value) VALUES ('current_token_id', ?1)",
        libsql::params![token_id.to_string()],
    ).await?;
    conn.execute(
        "INSERT OR REPLACE INTO app_state (key, value) VALUES ('current_bucket', ?1)",
        libsql::params![bucket_name],
    ).await?;
    Ok(())
}

/// Check if any accounts exist
pub async fn has_accounts() -> DbResult<bool> {
    let conn = get_connection()?;
    let mut rows = conn.query("SELECT COUNT(*) FROM accounts", ()).await?;
    if let Some(row) = rows.next().await? {
        let count: i64 = row.get(0)?;
        Ok(count > 0)
    } else {
        Ok(false)
    }
}

// ============ File Cache Functions ============

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

/// Store all files for a bucket (clears existing)
pub async fn store_all_files(bucket: &str, account_id: &str, files: &[CachedFile]) -> DbResult<()> {
    let conn = get_connection()?;
    
    // Clear existing files for this bucket
    conn.execute(
        "DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2",
        libsql::params![bucket, account_id],
    ).await?;
    
    // Insert new files
    for file in files {
        conn.execute(
            "INSERT INTO cached_files (bucket, account_id, key, size, last_modified, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            libsql::params![
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
        "INSERT OR REPLACE INTO sync_meta (bucket, account_id, last_sync, file_count)
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![bucket, account_id, now, files.len() as i32],
    ).await?;
    
    Ok(())
}

/// Get all cached files for a bucket
pub async fn get_all_cached_files(bucket: &str, account_id: &str) -> DbResult<Vec<CachedFile>> {
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT bucket, account_id, key, size, last_modified, synced_at
         FROM cached_files
         WHERE bucket = ?1 AND account_id = ?2
         ORDER BY key",
        libsql::params![bucket, account_id]
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
    let conn = get_connection()?;
    let pattern = format!("{}%", prefix);
    
    let mut rows = conn.query(
        "SELECT COALESCE(SUM(size), 0) FROM cached_files
         WHERE bucket = ?1 AND account_id = ?2 AND key LIKE ?3",
        libsql::params![bucket, account_id, pattern]
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
    let conn = get_connection()?;
    
    // Clear existing tree for this bucket
    conn.execute(
        "DELETE FROM directory_tree WHERE bucket = ?1 AND account_id = ?2",
        libsql::params![bucket, account_id],
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
            libsql::params![
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
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT bucket, account_id, path, file_count, total_file_count, size, total_size, last_modified, last_updated
         FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2 AND path = ?3",
        libsql::params![bucket, account_id, path]
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
    let conn = get_connection()?;
    let mut rows = conn.query(
        "SELECT bucket, account_id, path, file_count, total_file_count, size, total_size, last_modified, last_updated
         FROM directory_tree
         WHERE bucket = ?1 AND account_id = ?2
         ORDER BY path",
        libsql::params![bucket, account_id]
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
    let conn = get_connection()?;
    
    conn.execute(
        "DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2",
        libsql::params![bucket, account_id],
    ).await?;
    
    conn.execute(
        "DELETE FROM directory_tree WHERE bucket = ?1 AND account_id = ?2",
        libsql::params![bucket, account_id],
    ).await?;
    
    conn.execute(
        "DELETE FROM sync_meta WHERE bucket = ?1 AND account_id = ?2",
        libsql::params![bucket, account_id],
    ).await?;
    
    Ok(())
}
