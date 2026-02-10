use super::{get_connection, DbResult};
use serde::{Deserialize, Serialize};

/// Move session status (string-based storage)
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MoveStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "downloading")]
    Downloading,
    #[serde(rename = "uploading")]
    Uploading,
    #[serde(rename = "finishing")]
    Finishing,
    #[serde(rename = "deleting")]
    Deleting,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl std::fmt::Display for MoveStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MoveStatus::Pending => write!(f, "pending"),
            MoveStatus::Downloading => write!(f, "downloading"),
            MoveStatus::Uploading => write!(f, "uploading"),
            MoveStatus::Finishing => write!(f, "finishing"),
            MoveStatus::Deleting => write!(f, "deleting"),
            MoveStatus::Paused => write!(f, "paused"),
            MoveStatus::Success => write!(f, "success"),
            MoveStatus::Error => write!(f, "error"),
            MoveStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl From<String> for MoveStatus {
    fn from(value: String) -> Self {
        match value.as_str() {
            "pending" => MoveStatus::Pending,
            "downloading" => MoveStatus::Downloading,
            "uploading" => MoveStatus::Uploading,
            "finishing" => MoveStatus::Finishing,
            "deleting" => MoveStatus::Deleting,
            "paused" => MoveStatus::Paused,
            "success" => MoveStatus::Success,
            "error" => MoveStatus::Error,
            "cancelled" => MoveStatus::Cancelled,
            _ => MoveStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveSession {
    pub id: String,
    pub source_key: String,
    pub dest_key: String,
    pub source_bucket: String,
    pub source_account_id: String,
    pub source_provider: String,
    pub dest_bucket: String,
    pub dest_account_id: String,
    pub dest_provider: String,
    pub delete_original: bool,
    pub file_size: i64,
    pub progress: i64,
    pub status: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Get SQL for creating move session tables
pub fn get_table_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS move_sessions (
        id TEXT PRIMARY KEY,
        source_key TEXT NOT NULL,
        dest_key TEXT NOT NULL,
        source_bucket TEXT NOT NULL,
        source_account_id TEXT NOT NULL,
        source_provider TEXT NOT NULL,
        dest_bucket TEXT NOT NULL,
        dest_account_id TEXT NOT NULL,
        dest_provider TEXT NOT NULL,
        delete_original INTEGER NOT NULL DEFAULT 1,
        file_size INTEGER,
        progress INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL DEFAULT 'pending',
        error TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_move_sessions_status ON move_sessions(status);
    CREATE INDEX IF NOT EXISTS idx_move_sessions_source ON move_sessions(source_bucket, source_account_id);

    CREATE TABLE IF NOT EXISTS move_upload_sessions (
        task_id TEXT PRIMARY KEY,
        upload_id TEXT NOT NULL,
        part_size INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS move_upload_parts (
        task_id TEXT NOT NULL,
        part_number INTEGER NOT NULL,
        etag TEXT NOT NULL,
        size INTEGER NOT NULL,
        PRIMARY KEY (task_id, part_number)
    );

    CREATE INDEX IF NOT EXISTS idx_move_upload_parts_task ON move_upload_parts(task_id);
    "
}

/// Create a new move session
#[allow(dead_code)]
pub async fn create_move_session(session: &MoveSession) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT INTO move_sessions
         (id, source_key, dest_key, source_bucket, source_account_id, source_provider,
          dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress,
          status, error, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        turso::params![
            session.id.clone(),
            session.source_key.clone(),
            session.dest_key.clone(),
            session.source_bucket.clone(),
            session.source_account_id.clone(),
            session.source_provider.clone(),
            session.dest_bucket.clone(),
            session.dest_account_id.clone(),
            session.dest_provider.clone(),
            if session.delete_original { 1 } else { 0 },
            session.file_size,
            session.progress,
            session.status.clone(),
            session.error.clone(),
            session.created_at,
            session.updated_at,
        ],
    )
    .await?;
    Ok(())
}

/// Batch create move sessions (faster for multiple sessions)
pub async fn create_move_sessions_batch(sessions: &[MoveSession]) -> DbResult<()> {
    if sessions.is_empty() {
        return Ok(());
    }

    let conn = get_connection()?.lock().await;

    // Use a transaction for batch insert
    conn.execute("BEGIN TRANSACTION", ()).await?;

    for session in sessions {
        if let Err(e) = conn
            .execute(
                "INSERT INTO move_sessions
                 (id, source_key, dest_key, source_bucket, source_account_id, source_provider,
                  dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress,
                  status, error, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                turso::params![
                    session.id.clone(),
                    session.source_key.clone(),
                    session.dest_key.clone(),
                    session.source_bucket.clone(),
                    session.source_account_id.clone(),
                    session.source_provider.clone(),
                    session.dest_bucket.clone(),
                    session.dest_account_id.clone(),
                    session.dest_provider.clone(),
                    if session.delete_original { 1 } else { 0 },
                    session.file_size,
                    session.progress,
                    session.status.clone(),
                    session.error.clone(),
                    session.created_at,
                    session.updated_at,
                ],
            )
            .await
        {
            // Rollback on error
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(e.into());
        }
    }

    conn.execute("COMMIT", ()).await?;
    Ok(())
}

/// Update move session progress
pub async fn update_move_progress(session_id: &str, progress: i64) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE move_sessions
         SET progress = CASE WHEN progress < ?1 THEN ?1 ELSE progress END,
             updated_at = ?2
         WHERE id = ?3",
        turso::params![progress, now, session_id],
    )
    .await?;
    Ok(())
}

/// Update move session status
pub async fn update_move_status(
    session_id: &str,
    status: &str,
    error: Option<&str>,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE move_sessions SET status = ?1, error = ?2, updated_at = ?3 WHERE id = ?4",
        turso::params![status, error, now, session_id],
    )
    .await?;
    Ok(())
}

/// Update move session status and progress atomically
pub async fn update_move_status_and_progress(
    session_id: &str,
    status: &str,
    progress: i64,
    error: Option<&str>,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE move_sessions
         SET status = ?1,
             progress = CASE WHEN progress < ?2 THEN ?2 ELSE progress END,
             error = ?3,
             updated_at = ?4
         WHERE id = ?5",
        turso::params![status, progress, error, now, session_id],
    )
    .await?;
    Ok(())
}

/// Save multipart upload session info for a move task
pub async fn save_move_upload_session(
    task_id: &str,
    upload_id: &str,
    part_size: i64,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT OR REPLACE INTO move_upload_sessions (task_id, upload_id, part_size)
         VALUES (?1, ?2, ?3)",
        turso::params![task_id, upload_id, part_size],
    )
    .await?;
    Ok(())
}

/// Get multipart upload session info for a move task
pub async fn get_move_upload_session(task_id: &str) -> DbResult<Option<(String, i64)>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT upload_id, part_size FROM move_upload_sessions WHERE task_id = ?1",
            turso::params![task_id],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        Ok(Some((row.get(0)?, row.get(1)?)))
    } else {
        Ok(None)
    }
}

/// Delete multipart upload session info for a move task
pub async fn delete_move_upload_session(task_id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "DELETE FROM move_upload_sessions WHERE task_id = ?1",
        turso::params![task_id],
    )
    .await?;
    Ok(())
}

/// Save a completed multipart upload part
pub async fn save_move_upload_part(
    task_id: &str,
    part_number: i32,
    etag: &str,
    size: i64,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT OR REPLACE INTO move_upload_parts (task_id, part_number, etag, size)
         VALUES (?1, ?2, ?3, ?4)",
        turso::params![task_id, part_number, etag, size],
    )
    .await?;
    Ok(())
}

/// Get completed multipart upload parts for a move task
pub async fn get_move_upload_parts(task_id: &str) -> DbResult<Vec<(i32, String, i64)>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT part_number, etag, size FROM move_upload_parts WHERE task_id = ?1",
            turso::params![task_id],
        )
        .await?;

    let mut parts = Vec::new();
    while let Some(row) = rows.next().await? {
        parts.push((row.get(0)?, row.get(1)?, row.get(2)?));
    }
    Ok(parts)
}

/// Delete multipart upload parts for a move task
pub async fn delete_move_upload_parts(task_id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "DELETE FROM move_upload_parts WHERE task_id = ?1",
        turso::params![task_id],
    )
    .await?;
    Ok(())
}

/// Get move session by ID
#[allow(dead_code)]
pub async fn get_move_session(session_id: &str) -> DbResult<Option<MoveSession>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, source_key, dest_key, source_bucket, source_account_id, source_provider,
                dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress,
                status, error, created_at, updated_at
         FROM move_sessions WHERE id = ?1",
            turso::params![session_id],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        Ok(Some(MoveSession {
            id: row.get(0)?,
            source_key: row.get(1)?,
            dest_key: row.get(2)?,
            source_bucket: row.get(3)?,
            source_account_id: row.get(4)?,
            source_provider: row.get(5)?,
            dest_bucket: row.get(6)?,
            dest_account_id: row.get(7)?,
            dest_provider: row.get(8)?,
            delete_original: row.get::<i64>(9)? != 0,
            file_size: row.get(10)?,
            progress: row.get(11)?,
            status: row.get(12)?,
            error: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
        }))
    } else {
        Ok(None)
    }
}

/// Get all move sessions for a specific source bucket
pub async fn get_move_sessions_for_source(
    source_bucket: &str,
    source_account_id: &str,
) -> DbResult<Vec<MoveSession>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, source_key, dest_key, source_bucket, source_account_id, source_provider,
                dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress,
                status, error, created_at, updated_at
         FROM move_sessions
         WHERE source_bucket = ?1 AND source_account_id = ?2
         ORDER BY updated_at DESC",
            turso::params![source_bucket, source_account_id],
        )
        .await?;

    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await? {
        sessions.push(MoveSession {
            id: row.get(0)?,
            source_key: row.get(1)?,
            dest_key: row.get(2)?,
            source_bucket: row.get(3)?,
            source_account_id: row.get(4)?,
            source_provider: row.get(5)?,
            dest_bucket: row.get(6)?,
            dest_account_id: row.get(7)?,
            dest_provider: row.get(8)?,
            delete_original: row.get::<i64>(9)? != 0,
            file_size: row.get(10)?,
            progress: row.get(11)?,
            status: row.get(12)?,
            error: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
        });
    }
    Ok(sessions)
}

/// Get pending move sessions for a source bucket (ordered by created_at)
#[allow(dead_code)]
pub async fn get_pending_moves(
    source_bucket: &str,
    source_account_id: &str,
    limit: i64,
) -> DbResult<Vec<MoveSession>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, source_key, dest_key, source_bucket, source_account_id, source_provider,
                dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress,
                status, error, created_at, updated_at
         FROM move_sessions
         WHERE source_bucket = ?1 AND source_account_id = ?2 AND status = 'pending'
         ORDER BY created_at ASC
         LIMIT ?3",
            turso::params![source_bucket, source_account_id, limit],
        )
        .await?;

    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await? {
        sessions.push(MoveSession {
            id: row.get(0)?,
            source_key: row.get(1)?,
            dest_key: row.get(2)?,
            source_bucket: row.get(3)?,
            source_account_id: row.get(4)?,
            source_provider: row.get(5)?,
            dest_bucket: row.get(6)?,
            dest_account_id: row.get(7)?,
            dest_provider: row.get(8)?,
            delete_original: row.get::<i64>(9)? != 0,
            file_size: row.get(10)?,
            progress: row.get(11)?,
            status: row.get(12)?,
            error: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
        });
    }
    Ok(sessions)
}

/// Get pending move sessions for a source (ordered by created_at)
pub async fn get_pending_moves_for_source(
    source_bucket: &str,
    source_account_id: &str,
    limit: i64,
) -> DbResult<Vec<MoveSession>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, source_key, dest_key, source_bucket, source_account_id, source_provider,
                dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress,
                status, error, created_at, updated_at
         FROM move_sessions
         WHERE source_bucket = ?1 AND source_account_id = ?2
         AND status = 'pending'
         ORDER BY created_at ASC
         LIMIT ?3",
            turso::params![source_bucket, source_account_id, limit],
        )
        .await?;

    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await? {
        sessions.push(MoveSession {
            id: row.get(0)?,
            source_key: row.get(1)?,
            dest_key: row.get(2)?,
            source_bucket: row.get(3)?,
            source_account_id: row.get(4)?,
            source_provider: row.get(5)?,
            dest_bucket: row.get(6)?,
            dest_account_id: row.get(7)?,
            dest_provider: row.get(8)?,
            delete_original: row.get::<i64>(9)? != 0,
            file_size: row.get(10)?,
            progress: row.get(11)?,
            status: row.get(12)?,
            error: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
        });
    }
    Ok(sessions)
}

/// Count active move sessions for a source bucket (for queue slot calculation)
/// Excludes tasks at 100% progress (deleting/finalizing) to allow new tasks to start immediately
pub async fn count_active_moves(source_bucket: &str, source_account_id: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM move_sessions
         WHERE source_bucket = ?1 AND source_account_id = ?2
         AND status IN ('downloading', 'uploading')
         AND progress < 100",
            turso::params![source_bucket, source_account_id],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Count any in-progress moves (for clear_all validation - includes deleting)
pub async fn count_in_progress_moves(
    source_bucket: &str,
    source_account_id: &str,
) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM move_sessions
         WHERE source_bucket = ?1 AND source_account_id = ?2
         AND status IN ('downloading', 'uploading', 'finishing', 'deleting')",
            turso::params![source_bucket, source_account_id],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Get all active or pending move sessions across all accounts
pub async fn get_all_active_move_sessions() -> DbResult<Vec<MoveSession>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, source_key, dest_key, source_bucket, source_account_id, source_provider,
            dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress,
            status, error, created_at, updated_at
         FROM move_sessions
         WHERE status IN ('pending', 'downloading', 'uploading', 'finishing', 'deleting', 'paused')
         ORDER BY updated_at DESC",
            turso::params![],
        )
        .await?;

    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await? {
        sessions.push(MoveSession {
            id: row.get(0)?,
            source_key: row.get(1)?,
            dest_key: row.get(2)?,
            source_bucket: row.get(3)?,
            source_account_id: row.get(4)?,
            source_provider: row.get(5)?,
            dest_bucket: row.get(6)?,
            dest_account_id: row.get(7)?,
            dest_provider: row.get(8)?,
            delete_original: row.get::<i64>(9)? != 0,
            file_size: row.get(10)?,
            progress: row.get(11)?,
            status: row.get(12)?,
            error: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
        });
    }
    Ok(sessions)
}

/// Pause all active moves
pub async fn pause_all_moves(source_bucket: &str, source_account_id: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE move_sessions SET status = 'paused', updated_at = ?1
         WHERE source_bucket = ?2 AND source_account_id = ?3
         AND status IN ('downloading', 'uploading', 'finishing', 'deleting', 'pending')",
        turso::params![now, source_bucket, source_account_id],
    )
    .await?;

    let mut rows = conn.query("SELECT changes()", turso::params![]).await?;
    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// On app startup, pause all non-terminal moves so queues default to paused.
pub async fn pause_stale_moves_on_startup() -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE move_sessions SET status = 'paused', updated_at = ?1
         WHERE status IN ('pending', 'downloading', 'uploading', 'finishing', 'deleting')",
        turso::params![now],
    )
    .await?;

    let mut rows = conn.query("SELECT changes()", turso::params![]).await?;
    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Resume all paused moves
pub async fn resume_all_moves(source_bucket: &str, source_account_id: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE move_sessions SET status = 'pending', updated_at = ?1
         WHERE source_bucket = ?2 AND source_account_id = ?3 AND status = 'paused'",
        turso::params![now, source_bucket, source_account_id],
    )
    .await?;

    let mut rows = conn.query("SELECT changes()", turso::params![]).await?;
    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Delete a move session
pub async fn delete_move_session(session_id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "DELETE FROM move_upload_parts WHERE task_id = ?1",
        turso::params![session_id],
    )
    .await?;
    conn.execute(
        "DELETE FROM move_upload_sessions WHERE task_id = ?1",
        turso::params![session_id],
    )
    .await?;
    conn.execute(
        "DELETE FROM move_sessions WHERE id = ?1",
        turso::params![session_id],
    )
    .await?;
    Ok(())
}

/// Delete all finished moves (success, error, cancelled)
pub async fn delete_finished_moves(source_bucket: &str, source_account_id: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;

    // First, get the IDs of finished sessions (libsql doesn't support subqueries in IN clauses)
    let mut rows = conn
        .query(
            "SELECT id FROM move_sessions
             WHERE source_bucket = ?1 AND source_account_id = ?2
             AND status IN ('success', 'error', 'cancelled')",
            turso::params![source_bucket, source_account_id],
        )
        .await?;

    let mut session_ids: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        session_ids.push(row.get(0)?);
    }

    // Delete related records and sessions for each ID
    for session_id in &session_ids {
        conn.execute(
            "DELETE FROM move_upload_parts WHERE task_id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
        conn.execute(
            "DELETE FROM move_upload_sessions WHERE task_id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
        conn.execute(
            "DELETE FROM move_sessions WHERE id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
    }

    Ok(session_ids.len() as i64)
}

/// Delete all move sessions for a source bucket (only when no active moves)
pub async fn delete_all_moves(source_bucket: &str, source_account_id: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;

    // First, get the IDs of all sessions (libsql doesn't support subqueries in IN clauses)
    let mut rows = conn
        .query(
            "SELECT id FROM move_sessions WHERE source_bucket = ?1 AND source_account_id = ?2",
            turso::params![source_bucket, source_account_id],
        )
        .await?;

    let mut session_ids: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        session_ids.push(row.get(0)?);
    }

    // Delete related records and sessions for each ID
    for session_id in &session_ids {
        conn.execute(
            "DELETE FROM move_upload_parts WHERE task_id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
        conn.execute(
            "DELETE FROM move_upload_sessions WHERE task_id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
        conn.execute(
            "DELETE FROM move_sessions WHERE id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
    }

    Ok(session_ids.len() as i64)
}

/// Clean up old finished sessions (older than 7 days)
#[allow(dead_code)]
pub async fn cleanup_old_move_sessions() -> DbResult<usize> {
    let conn = get_connection()?.lock().await;
    let cutoff = chrono::Utc::now().timestamp() - (7 * 24 * 60 * 60);

    // First, get the IDs of old finished sessions (libsql doesn't support subqueries in IN clauses)
    let mut rows = conn
        .query(
            "SELECT id FROM move_sessions
             WHERE status IN ('success', 'error', 'cancelled')
             AND updated_at < ?1",
            turso::params![cutoff],
        )
        .await?;

    let mut session_ids: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        session_ids.push(row.get(0)?);
    }

    // Delete related records and sessions for each ID
    for session_id in &session_ids {
        conn.execute(
            "DELETE FROM move_upload_parts WHERE task_id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
        conn.execute(
            "DELETE FROM move_upload_sessions WHERE task_id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
        conn.execute(
            "DELETE FROM move_sessions WHERE id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
    }

    Ok(session_ids.len())
}

#[cfg(test)]
mod tests {
    use super::get_table_sql;

    #[test]
    fn move_sessions_sql_contains_table_and_indexes() {
        let sql = get_table_sql();
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS move_sessions"));
        assert!(sql.contains("idx_move_sessions_status"));
        assert!(sql.contains("idx_move_sessions_source"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS move_upload_sessions"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS move_upload_parts"));
        assert!(sql.contains("idx_move_upload_parts_task"));
    }
}
