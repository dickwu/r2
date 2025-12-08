use rusqlite::{Connection, Result, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref DB_CONNECTION: Mutex<Option<Connection>> = Mutex::new(None);
}

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

/// Initialize the database with required tables
pub fn init_db(db_path: &Path) -> Result<()> {
    let conn = Connection::open(db_path)?;
    
    conn.execute_batch(
        "
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

/// Update session's upload_id (after initiating multipart upload)
pub fn update_session_upload_id(session_id: &str, upload_id: &str) -> Result<()> {
    with_connection(|conn| {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE upload_sessions SET upload_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![upload_id, now, session_id],
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
