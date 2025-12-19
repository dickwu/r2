use rusqlite::{Connection, Result, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref DB_CONNECTION: Mutex<Option<Connection>> = Mutex::new(None);
}

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
pub fn init_db(db_path: &Path) -> Result<()> {
    let conn = Connection::open(db_path)?;
    
    // Enable foreign keys
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    
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
        "
    )?;

    let mut db = DB_CONNECTION.lock().unwrap();
    *db = Some(conn);
    
    Ok(())
}

fn with_connection<F, T>(f: F) -> Result<T>
where
    F: FnOnce(&Connection) -> Result<T>,
{
    let db = DB_CONNECTION.lock().unwrap();
    match db.as_ref() {
        Some(conn) => f(conn),
        None => Err(rusqlite::Error::InvalidQuery),
    }
}

/// Create a new upload session
pub fn create_session(session: &UploadSession) -> Result<()> {
    with_connection(|conn| {
        conn.execute(
            "INSERT INTO upload_sessions 
             (id, file_path, file_size, file_mtime, object_key, bucket, account_id, 
              upload_id, content_type, total_parts, created_at, updated_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                session.id,
                session.file_path,
                session.file_size,
                session.file_mtime,
                session.object_key,
                session.bucket,
                session.account_id,
                session.upload_id,
                session.content_type,
                session.total_parts,
                session.created_at,
                session.updated_at,
                session.status,
            ],
        )?;
        Ok(())
    })
}

/// Update session status
pub fn update_session_status(session_id: &str, status: &str) -> Result<()> {
    with_connection(|conn| {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE upload_sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now, session_id],
        )?;
        Ok(())
    })
}

/// Find existing session for a file (by path, size, mtime and target key)
pub fn find_resumable_session(
    file_path: &str,
    file_size: i64,
    file_mtime: i64,
    object_key: &str,
    bucket: &str,
    account_id: &str,
) -> Result<Option<UploadSession>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_size, file_mtime, object_key, bucket, account_id,
                    upload_id, content_type, total_parts, created_at, updated_at, status
             FROM upload_sessions 
             WHERE file_path = ?1 AND file_size = ?2 AND file_mtime = ?3 
                   AND object_key = ?4 AND bucket = ?5 AND account_id = ?6
                   AND status = 'uploading' AND upload_id IS NOT NULL"
        )?;
        
        let mut rows = stmt.query(params![file_path, file_size, file_mtime, object_key, bucket, account_id])?;
        
        if let Some(row) = rows.next()? {
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
    })
}

/// Get session by ID
pub fn get_session(session_id: &str) -> Result<Option<UploadSession>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_size, file_mtime, object_key, bucket, account_id,
                    upload_id, content_type, total_parts, created_at, updated_at, status
             FROM upload_sessions WHERE id = ?1"
        )?;
        
        let mut rows = stmt.query(params![session_id])?;
        
        if let Some(row) = rows.next()? {
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
    })
}

/// Save a completed part
pub fn save_completed_part(session_id: &str, part_number: i32, etag: &str) -> Result<()> {
    with_connection(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO completed_parts (session_id, part_number, etag)
             VALUES (?1, ?2, ?3)",
            params![session_id, part_number, etag],
        )?;
        
        // Update session timestamp
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE upload_sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;
        
        Ok(())
    })
}

/// Get all completed parts for a session
pub fn get_completed_parts(session_id: &str) -> Result<Vec<CompletedPart>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT session_id, part_number, etag FROM completed_parts 
             WHERE session_id = ?1 ORDER BY part_number"
        )?;
        
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(CompletedPart {
                session_id: row.get(0)?,
                part_number: row.get(1)?,
                etag: row.get(2)?,
            })
        })?;
        
        let mut parts = Vec::new();
        for part in rows {
            parts.push(part?);
        }
        Ok(parts)
    })
}

/// Delete session and its parts
pub fn delete_session(session_id: &str) -> Result<()> {
    with_connection(|conn| {
        conn.execute("DELETE FROM completed_parts WHERE session_id = ?1", params![session_id])?;
        conn.execute("DELETE FROM upload_sessions WHERE id = ?1", params![session_id])?;
        Ok(())
    })
}

/// Get all pending/uploading sessions (for UI to show resumable uploads)
pub fn get_pending_sessions() -> Result<Vec<UploadSession>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_size, file_mtime, object_key, bucket, account_id,
                    upload_id, content_type, total_parts, created_at, updated_at, status
             FROM upload_sessions 
             WHERE status IN ('pending', 'uploading')
             ORDER BY updated_at DESC"
        )?;
        
        let rows = stmt.query_map([], |row| {
            Ok(UploadSession {
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
            })
        })?;
        
        let mut sessions = Vec::new();
        for session in rows {
            sessions.push(session?);
        }
        Ok(sessions)
    })
}

/// Clean up old completed/failed sessions (older than 7 days)
pub fn cleanup_old_sessions() -> Result<usize> {
    with_connection(|conn| {
        let cutoff = chrono::Utc::now().timestamp() - (7 * 24 * 60 * 60);
        
        // First delete parts
        conn.execute(
            "DELETE FROM completed_parts WHERE session_id IN 
             (SELECT id FROM upload_sessions WHERE status IN ('completed', 'failed', 'cancelled') 
              AND updated_at < ?1)",
            params![cutoff],
        )?;
        
        // Then delete sessions
        let deleted = conn.execute(
            "DELETE FROM upload_sessions 
             WHERE status IN ('completed', 'failed', 'cancelled') AND updated_at < ?1",
            params![cutoff],
        )?;
        
        Ok(deleted)
    })
}

// ============ Account CRUD Functions ============

/// Create a new account
pub fn create_account(id: &str, name: Option<&str>) -> Result<Account> {
    with_connection(|conn| {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO accounts (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, now, now],
        )?;
        Ok(Account {
            id: id.to_string(),
            name: name.map(|s| s.to_string()),
            created_at: now,
            updated_at: now,
        })
    })
}

/// Get account by ID
#[allow(dead_code)]
pub fn get_account(id: &str) -> Result<Option<Account>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare("SELECT id, name, created_at, updated_at FROM accounts WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        
        if let Some(row) = rows.next()? {
            Ok(Some(Account {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
            }))
        } else {
            Ok(None)
        }
    })
}

/// List all accounts
pub fn list_accounts() -> Result<Vec<Account>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare("SELECT id, name, created_at, updated_at FROM accounts ORDER BY created_at")?;
        let rows = stmt.query_map([], |row| {
            Ok(Account {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        
        let mut accounts = Vec::new();
        for account in rows {
            accounts.push(account?);
        }
        Ok(accounts)
    })
}

/// Update account
pub fn update_account(id: &str, name: Option<&str>) -> Result<()> {
    with_connection(|conn| {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE accounts SET name = ?1, updated_at = ?2 WHERE id = ?3",
            params![name, now, id],
        )?;
        Ok(())
    })
}

/// Delete account (cascades to tokens and buckets)
pub fn delete_account(id: &str) -> Result<()> {
    with_connection(|conn| {
        conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])?;
        Ok(())
    })
}

// ============ Token CRUD Functions ============

/// Create a new token
pub fn create_token(
    account_id: &str,
    name: Option<&str>,
    api_token: &str,
    access_key_id: &str,
    secret_access_key: &str,
) -> Result<Token> {
    with_connection(|conn| {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO tokens (account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![account_id, name, api_token, access_key_id, secret_access_key, now, now],
        )?;
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
    })
}

/// Get token by ID
pub fn get_token(id: i64) -> Result<Option<Token>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at
             FROM tokens WHERE id = ?1"
        )?;
        let mut rows = stmt.query(params![id])?;
        
        if let Some(row) = rows.next()? {
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
    })
}

/// List tokens by account
pub fn list_tokens_by_account(account_id: &str) -> Result<Vec<Token>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at
             FROM tokens WHERE account_id = ?1 ORDER BY created_at"
        )?;
        let rows = stmt.query_map(params![account_id], |row| {
            Ok(Token {
                id: row.get(0)?,
                account_id: row.get(1)?,
                name: row.get(2)?,
                api_token: row.get(3)?,
                access_key_id: row.get(4)?,
                secret_access_key: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        
        let mut tokens = Vec::new();
        for token in rows {
            tokens.push(token?);
        }
        Ok(tokens)
    })
}

/// Update token
pub fn update_token(
    id: i64,
    name: Option<&str>,
    api_token: &str,
    access_key_id: &str,
    secret_access_key: &str,
) -> Result<()> {
    with_connection(|conn| {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE tokens SET name = ?1, api_token = ?2, access_key_id = ?3, secret_access_key = ?4, updated_at = ?5
             WHERE id = ?6",
            params![name, api_token, access_key_id, secret_access_key, now, id],
        )?;
        Ok(())
    })
}

/// Delete token (cascades to buckets)
pub fn delete_token(id: i64) -> Result<()> {
    with_connection(|conn| {
        conn.execute("DELETE FROM tokens WHERE id = ?1", params![id])?;
        Ok(())
    })
}

// ============ Bucket CRUD Functions ============

/// Create a new bucket
#[allow(dead_code)]
pub fn create_bucket(token_id: i64, name: &str, public_domain: Option<&str>) -> Result<Bucket> {
    with_connection(|conn| {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO buckets (token_id, name, public_domain, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![token_id, name, public_domain, now, now],
        )?;
        let id = conn.last_insert_rowid();
        Ok(Bucket {
            id,
            token_id,
            name: name.to_string(),
            public_domain: public_domain.map(|s| s.to_string()),
            created_at: now,
            updated_at: now,
        })
    })
}

/// Get bucket by ID
#[allow(dead_code)]
pub fn get_bucket(id: i64) -> Result<Option<Bucket>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, token_id, name, public_domain, created_at, updated_at FROM buckets WHERE id = ?1"
        )?;
        let mut rows = stmt.query(params![id])?;
        
        if let Some(row) = rows.next()? {
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
    })
}

/// List buckets by token
pub fn list_buckets_by_token(token_id: i64) -> Result<Vec<Bucket>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, token_id, name, public_domain, created_at, updated_at
             FROM buckets WHERE token_id = ?1 ORDER BY name"
        )?;
        let rows = stmt.query_map(params![token_id], |row| {
            Ok(Bucket {
                id: row.get(0)?,
                token_id: row.get(1)?,
                name: row.get(2)?,
                public_domain: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        
        let mut buckets = Vec::new();
        for bucket in rows {
            buckets.push(bucket?);
        }
        Ok(buckets)
    })
}

/// Update bucket
pub fn update_bucket(id: i64, public_domain: Option<&str>) -> Result<()> {
    with_connection(|conn| {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE buckets SET public_domain = ?1, updated_at = ?2 WHERE id = ?3",
            params![public_domain, now, id],
        )?;
        Ok(())
    })
}

/// Delete bucket
pub fn delete_bucket(id: i64) -> Result<()> {
    with_connection(|conn| {
        conn.execute("DELETE FROM buckets WHERE id = ?1", params![id])?;
        Ok(())
    })
}

/// Save multiple buckets for a token (replace existing)
pub fn save_buckets_for_token(token_id: i64, buckets: &[(String, Option<String>)]) -> Result<Vec<Bucket>> {
    with_connection(|conn| {
        let now = chrono::Utc::now().timestamp();
        
        // Delete existing buckets for this token
        conn.execute("DELETE FROM buckets WHERE token_id = ?1", params![token_id])?;
        
        // Insert new buckets
        let mut result = Vec::new();
        for (name, public_domain) in buckets {
            conn.execute(
                "INSERT INTO buckets (token_id, name, public_domain, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![token_id, name, public_domain.as_deref(), now, now],
            )?;
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
    })
}

// ============ App State Functions ============

/// Get app state value
#[allow(dead_code)]
pub fn get_app_state(key: &str) -> Result<Option<String>> {
    with_connection(|conn| {
        let mut stmt = conn.prepare("SELECT value FROM app_state WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    })
}

/// Set app state value
pub fn set_app_state(key: &str, value: &str) -> Result<()> {
    with_connection(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO app_state (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    })
}

/// Delete app state value
#[allow(dead_code)]
pub fn delete_app_state(key: &str) -> Result<()> {
    with_connection(|conn| {
        conn.execute("DELETE FROM app_state WHERE key = ?1", params![key])?;
        Ok(())
    })
}

// ============ Combined Config Functions ============

/// Get current configuration (combines token + bucket info)
pub fn get_current_config() -> Result<Option<CurrentConfig>> {
    with_connection(|conn| {
        // Get current token ID
        let mut stmt = conn.prepare("SELECT value FROM app_state WHERE key = 'current_token_id'")?;
        let mut rows = stmt.query([])?;
        
        let token_id: i64 = if let Some(row) = rows.next()? {
            let value: String = row.get(0)?;
            value.parse().map_err(|_| rusqlite::Error::InvalidQuery)?
        } else {
            return Ok(None);
        };
        
        // Get current bucket
        let mut stmt = conn.prepare("SELECT value FROM app_state WHERE key = 'current_bucket'")?;
        let mut rows = stmt.query([])?;
        
        let bucket_name: String = if let Some(row) = rows.next()? {
            row.get(0)?
        } else {
            return Ok(None);
        };
        
        // Get token with account info
        let mut stmt = conn.prepare(
            "SELECT t.id, t.account_id, t.name, t.api_token, t.access_key_id, t.secret_access_key,
                    a.name as account_name
             FROM tokens t
             JOIN accounts a ON t.account_id = a.id
             WHERE t.id = ?1"
        )?;
        let mut rows = stmt.query(params![token_id])?;
        
        if let Some(row) = rows.next()? {
            // Get bucket's public domain
            let mut bucket_stmt = conn.prepare(
                "SELECT public_domain FROM buckets WHERE token_id = ?1 AND name = ?2"
            )?;
            let mut bucket_rows = bucket_stmt.query(params![token_id, &bucket_name])?;
            let public_domain: Option<String> = if let Some(bucket_row) = bucket_rows.next()? {
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
    })
}

/// Set current token and bucket
pub fn set_current_selection(token_id: i64, bucket_name: &str) -> Result<()> {
    with_connection(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO app_state (key, value) VALUES ('current_token_id', ?1)",
            params![token_id.to_string()],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO app_state (key, value) VALUES ('current_bucket', ?1)",
            params![bucket_name],
        )?;
        Ok(())
    })
}

/// Check if any accounts exist
pub fn has_accounts() -> Result<bool> {
    with_connection(|conn| {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))?;
        Ok(count > 0)
    })
}

