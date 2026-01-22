use super::{get_connection, DbResult};
use serde::{Deserialize, Serialize};

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

/// Get SQL for creating upload session tables
pub fn get_table_sql() -> &'static str {
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
        FOREIGN KEY (session_id) REFERENCES upload_sessions(id)
    );

    CREATE INDEX IF NOT EXISTS idx_sessions_status ON upload_sessions(status);
    CREATE INDEX IF NOT EXISTS idx_sessions_file ON upload_sessions(file_path, file_size, file_mtime);
    "
}

// ============ Upload Session Functions ============

/// Create a new upload session
pub async fn create_session(session: &UploadSession) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT INTO upload_sessions 
         (id, file_path, file_size, file_mtime, object_key, bucket, account_id, 
          upload_id, content_type, total_parts, created_at, updated_at, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        turso::params![
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
    )
    .await?;
    Ok(())
}

/// Update session status
pub async fn update_session_status(session_id: &str, status: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE upload_sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
        turso::params![status, now, session_id],
    )
    .await?;
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
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, file_path, file_size, file_mtime, object_key, bucket, account_id,
                upload_id, content_type, total_parts, created_at, updated_at, status
         FROM upload_sessions 
         WHERE file_path = ?1 AND file_size = ?2 AND file_mtime = ?3 
               AND object_key = ?4 AND bucket = ?5 AND account_id = ?6
               AND status = 'uploading' AND upload_id IS NOT NULL",
            turso::params![file_path, file_size, file_mtime, object_key, bucket, account_id],
        )
        .await?;

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
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, file_path, file_size, file_mtime, object_key, bucket, account_id,
                upload_id, content_type, total_parts, created_at, updated_at, status
         FROM upload_sessions WHERE id = ?1",
            turso::params![session_id],
        )
        .await?;

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
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT INTO completed_parts (session_id, part_number, etag)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (session_id, part_number) DO UPDATE SET etag = ?3",
        turso::params![session_id, part_number, etag],
    )
    .await?;

    // Update session timestamp
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE upload_sessions SET updated_at = ?1 WHERE id = ?2",
        turso::params![now, session_id],
    )
    .await?;

    Ok(())
}

/// Get all completed parts for a session
pub async fn get_completed_parts(session_id: &str) -> DbResult<Vec<CompletedPart>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT session_id, part_number, etag FROM completed_parts 
         WHERE session_id = ?1 ORDER BY part_number",
            turso::params![session_id],
        )
        .await?;

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
    let conn = get_connection()?.lock().await;
    conn.execute(
        "DELETE FROM completed_parts WHERE session_id = ?1",
        turso::params![session_id],
    )
    .await?;
    conn.execute(
        "DELETE FROM upload_sessions WHERE id = ?1",
        turso::params![session_id],
    )
    .await?;
    Ok(())
}

/// Get all pending/uploading sessions (for UI to show resumable uploads)
pub async fn get_pending_sessions() -> DbResult<Vec<UploadSession>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, file_path, file_size, file_mtime, object_key, bucket, account_id,
                upload_id, content_type, total_parts, created_at, updated_at, status
         FROM upload_sessions 
         WHERE status IN ('pending', 'uploading')
         ORDER BY updated_at DESC",
            (),
        )
        .await?;

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
    let conn = get_connection()?.lock().await;
    let cutoff = chrono::Utc::now().timestamp() - (7 * 24 * 60 * 60);

    // Step 1: Query session IDs to delete (libsql doesn't support subqueries in WHERE)
    let mut rows = conn
        .query(
            "SELECT id FROM upload_sessions 
         WHERE (status = 'completed' OR status = 'failed' OR status = 'cancelled')
         AND updated_at < ?1",
            turso::params![cutoff],
        )
        .await?;

    let mut session_ids: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        session_ids.push(row.get(0)?);
    }

    // Step 2: Delete parts for each session
    for session_id in &session_ids {
        conn.execute(
            "DELETE FROM completed_parts WHERE session_id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
    }

    // Step 3: Delete the sessions
    for session_id in &session_ids {
        conn.execute(
            "DELETE FROM upload_sessions WHERE id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
    }

    Ok(session_ids.len())
}
