use super::{get_connection, DbResult};
use serde::{Deserialize, Serialize};

/// Download session status (for future type-safe status handling)
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DownloadStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "downloading")]
    Downloading,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl std::fmt::Display for DownloadStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadStatus::Pending => write!(f, "pending"),
            DownloadStatus::Downloading => write!(f, "downloading"),
            DownloadStatus::Paused => write!(f, "paused"),
            DownloadStatus::Completed => write!(f, "completed"),
            DownloadStatus::Failed => write!(f, "failed"),
            DownloadStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl From<String> for DownloadStatus {
    fn from(s: String) -> Self {
        match s.as_str() {
            "pending" => DownloadStatus::Pending,
            "downloading" => DownloadStatus::Downloading,
            "paused" => DownloadStatus::Paused,
            "completed" => DownloadStatus::Completed,
            "failed" => DownloadStatus::Failed,
            "cancelled" => DownloadStatus::Cancelled,
            _ => DownloadStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadSession {
    pub id: String,
    pub object_key: String,
    pub file_name: String,
    pub file_size: i64,
    pub downloaded_bytes: i64,
    pub local_path: String,
    pub bucket: String,
    pub account_id: String,
    pub status: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Get SQL for creating download session tables
pub fn get_table_sql() -> &'static str {
    "
    -- Download sessions table
    CREATE TABLE IF NOT EXISTS download_sessions (
        id TEXT PRIMARY KEY,
        object_key TEXT NOT NULL,
        file_name TEXT NOT NULL,
        file_size INTEGER NOT NULL,
        downloaded_bytes INTEGER NOT NULL DEFAULT 0,
        local_path TEXT NOT NULL,
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'pending',
        error TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_download_sessions_status ON download_sessions(status);
    CREATE INDEX IF NOT EXISTS idx_download_sessions_bucket ON download_sessions(bucket, account_id);
    "
}

/// Create a new download session
pub async fn create_download_session(session: &DownloadSession) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT INTO download_sessions 
         (id, object_key, file_name, file_size, downloaded_bytes, local_path, 
          bucket, account_id, status, error, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        turso::params![
            session.id.clone(),
            session.object_key.clone(),
            session.file_name.clone(),
            session.file_size,
            session.downloaded_bytes,
            session.local_path.clone(),
            session.bucket.clone(),
            session.account_id.clone(),
            session.status.clone(),
            session.error.clone(),
            session.created_at,
            session.updated_at,
        ],
    )
    .await?;
    Ok(())
}

/// Update download session progress
pub async fn update_download_progress(session_id: &str, downloaded_bytes: i64) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE download_sessions SET downloaded_bytes = ?1, updated_at = ?2 WHERE id = ?3",
        turso::params![downloaded_bytes, now, session_id],
    )
    .await?;
    Ok(())
}

/// Update download session file size (when obtained from Content-Length header)
pub async fn update_download_file_size(session_id: &str, file_size: i64) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE download_sessions SET file_size = ?1, updated_at = ?2 WHERE id = ?3",
        turso::params![file_size, now, session_id],
    )
    .await?;
    Ok(())
}

/// Update download session status
pub async fn update_download_status(
    session_id: &str,
    status: &str,
    error: Option<&str>,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE download_sessions SET status = ?1, error = ?2, updated_at = ?3 WHERE id = ?4",
        turso::params![status, error, now, session_id],
    )
    .await?;
    Ok(())
}

/// Get download session by ID
#[allow(dead_code)]
pub async fn get_download_session(session_id: &str) -> DbResult<Option<DownloadSession>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, object_key, file_name, file_size, downloaded_bytes, local_path,
                bucket, account_id, status, error, created_at, updated_at
         FROM download_sessions WHERE id = ?1",
            turso::params![session_id],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        Ok(Some(DownloadSession {
            id: row.get(0)?,
            object_key: row.get(1)?,
            file_name: row.get(2)?,
            file_size: row.get(3)?,
            downloaded_bytes: row.get(4)?,
            local_path: row.get(5)?,
            bucket: row.get(6)?,
            account_id: row.get(7)?,
            status: row.get(8)?,
            error: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        }))
    } else {
        Ok(None)
    }
}

/// Get all download sessions for a specific bucket
pub async fn get_download_sessions_for_bucket(
    bucket: &str,
    account_id: &str,
) -> DbResult<Vec<DownloadSession>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, object_key, file_name, file_size, downloaded_bytes, local_path,
                bucket, account_id, status, error, created_at, updated_at
         FROM download_sessions 
         WHERE bucket = ?1 AND account_id = ?2
         ORDER BY updated_at DESC",
            turso::params![bucket, account_id],
        )
        .await?;

    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await? {
        sessions.push(DownloadSession {
            id: row.get(0)?,
            object_key: row.get(1)?,
            file_name: row.get(2)?,
            file_size: row.get(3)?,
            downloaded_bytes: row.get(4)?,
            local_path: row.get(5)?,
            bucket: row.get(6)?,
            account_id: row.get(7)?,
            status: row.get(8)?,
            error: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        });
    }
    Ok(sessions)
}

/// Delete a download session
pub async fn delete_download_session(session_id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "DELETE FROM download_sessions WHERE id = ?1",
        turso::params![session_id],
    )
    .await?;
    Ok(())
}

/// Get pending download sessions for a bucket (ordered by created_at)
pub async fn get_pending_downloads(
    bucket: &str,
    account_id: &str,
    limit: i64,
) -> DbResult<Vec<DownloadSession>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, object_key, file_name, file_size, downloaded_bytes, local_path,
                bucket, account_id, status, error, created_at, updated_at
         FROM download_sessions 
         WHERE bucket = ?1 AND account_id = ?2 AND status = 'pending'
         ORDER BY created_at ASC
         LIMIT ?3",
            turso::params![bucket, account_id, limit],
        )
        .await?;

    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await? {
        sessions.push(DownloadSession {
            id: row.get(0)?,
            object_key: row.get(1)?,
            file_name: row.get(2)?,
            file_size: row.get(3)?,
            downloaded_bytes: row.get(4)?,
            local_path: row.get(5)?,
            bucket: row.get(6)?,
            account_id: row.get(7)?,
            status: row.get(8)?,
            error: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        });
    }
    Ok(sessions)
}

/// Count active (downloading) sessions for a bucket
pub async fn count_active_downloads(bucket: &str, account_id: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM download_sessions 
         WHERE bucket = ?1 AND account_id = ?2 AND status = 'downloading'",
            turso::params![bucket, account_id],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Set all downloading tasks to paused (for pause all)
pub async fn pause_all_downloads(bucket: &str, account_id: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE download_sessions SET status = 'paused', updated_at = ?1
         WHERE bucket = ?2 AND account_id = ?3 AND status IN ('downloading', 'pending')",
        turso::params![now, bucket, account_id],
    )
    .await?;

    // Return count of updated rows
    let mut rows = conn.query("SELECT changes()", turso::params![]).await?;
    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Set all paused tasks to pending (for start all)
pub async fn resume_all_downloads(bucket: &str, account_id: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE download_sessions SET status = 'pending', updated_at = ?1 
         WHERE bucket = ?2 AND account_id = ?3 AND status = 'paused'",
        turso::params![now, bucket, account_id],
    )
    .await?;

    // Return count of updated rows
    let mut rows = conn.query("SELECT changes()", turso::params![]).await?;
    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Delete all finished downloads (completed, failed, cancelled)
pub async fn delete_finished_downloads(bucket: &str, account_id: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "DELETE FROM download_sessions 
         WHERE bucket = ?1 AND account_id = ?2 
         AND status IN ('completed', 'failed', 'cancelled')",
        turso::params![bucket, account_id],
    )
    .await?;

    let mut rows = conn.query("SELECT changes()", turso::params![]).await?;
    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Delete all downloads for a bucket (only call when no active downloads)
pub async fn delete_all_downloads(bucket: &str, account_id: &str) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "DELETE FROM download_sessions WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;

    let mut rows = conn.query("SELECT changes()", turso::params![]).await?;
    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Ok(0)
    }
}

/// Clean up old completed/cancelled sessions (older than 7 days)
#[allow(dead_code)]
pub async fn cleanup_old_download_sessions() -> DbResult<usize> {
    let conn = get_connection()?.lock().await;
    let cutoff = chrono::Utc::now().timestamp() - (7 * 24 * 60 * 60);

    // Query session IDs to delete
    let mut rows = conn
        .query(
            "SELECT id FROM download_sessions 
         WHERE (status = 'completed' OR status = 'cancelled')
         AND updated_at < ?1",
            turso::params![cutoff],
        )
        .await?;

    let mut session_ids: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        session_ids.push(row.get(0)?);
    }

    // Delete the sessions
    for session_id in &session_ids {
        conn.execute(
            "DELETE FROM download_sessions WHERE id = ?1",
            turso::params![session_id.clone()],
        )
        .await?;
    }

    Ok(session_ids.len())
}
