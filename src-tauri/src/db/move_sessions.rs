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

/// Identity observed before any destination mutation. A zero size is an actual
/// HEAD result, never an unknown or cached size.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceIdentity {
    pub size: u64,
    pub etag: String,
    pub version_id: Option<String>,
}

/// Recovery state is independent of UI pause/error status. In particular,
/// retrying deletion must never re-run the destination transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveJournal {
    pub task_id: String,
    pub stage: String,
    pub source: SourceIdentity,
    pub source_scope: String,
    pub dest_scope: String,
    pub destination: Option<SourceIdentity>,
    #[serde(default)]
    pub retry: MoveRetrySchedule,
    #[serde(default)]
    pub metrics: MoveMetrics,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MoveMetrics {
    pub copy_started_at_ms: Option<i64>,
    pub copy_completed_at_ms: Option<i64>,
    pub verification_started_at_ms: Option<i64>,
    pub verification_completed_at_ms: Option<i64>,
    pub delete_started_at_ms: Option<i64>,
    pub delete_completed_at_ms: Option<i64>,
    pub copy_requests: u64,
    pub max_copy_in_flight: u32,
    pub retry_events: u32,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MoveRetrySchedule {
    pub next_attempt_at: Option<i64>,
    pub attempt_count: u32,
    pub last_error_class: Option<String>,
    pub phase: Option<String>,
}

async fn save_journal_on(conn: &turso::Connection, journal: &MoveJournal) -> DbResult<()> {
    // FULL applies to the critical decision record even though ordinary cache
    // writes use NORMAL. Do not proceed to source deletion if this write fails.
    conn.execute("PRAGMA synchronous = FULL", ()).await?;
    let result = conn
        .execute(
            "INSERT INTO move_journal (task_id, data) VALUES (?1, ?2)
         ON CONFLICT(task_id) DO UPDATE SET data = excluded.data",
            turso::params![journal.task_id.as_str(), serde_json::to_string(journal)?],
        )
        .await;
    let _ = conn.execute("PRAGMA synchronous = NORMAL", ()).await;
    result?;
    Ok(())
}

pub async fn save_move_journal(journal: &MoveJournal) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    // A task removed/cancelled by the user must not be resurrected by a late
    // upload completion and then delete its source.
    let mut rows = conn
        .query(
            "SELECT id FROM move_sessions WHERE id = ?1",
            turso::params![journal.task_id.as_str()],
        )
        .await?;
    if rows.next().await?.is_none() {
        return Err("Move task was removed; source retained".into());
    }
    save_journal_on(&conn, journal).await?;
    conn.execute(
        "DELETE FROM move_task_retries WHERE task_id = ?1",
        turso::params![journal.task_id.as_str()],
    )
    .await?;
    Ok(())
}

const MAX_PERSISTED_RETRIES: u32 = 20;
const MAX_MOVE_RETRY_DELAY_SECS: i64 = 30;

fn move_retry_delay_secs(attempt: u32) -> i64 {
    2_i64
        .pow(attempt.saturating_sub(1).min(5))
        .min(MAX_MOVE_RETRY_DELAY_SECS)
}

pub async fn schedule_move_retry(
    task_id: &str,
    phase: &str,
    error_class: &str,
) -> DbResult<Option<i64>> {
    let conn = get_connection()?.lock().await;
    let mut journal = match get_journal_on(&conn, task_id).await? {
        Some(journal) => journal,
        None => return Ok(None),
    };
    let attempt = journal.retry.attempt_count.saturating_add(1);
    if attempt > MAX_PERSISTED_RETRIES {
        journal.retry.attempt_count = attempt;
        journal.retry.last_error_class = Some(error_class.into());
        journal.retry.phase = Some(phase.into());
        journal.retry.next_attempt_at = None;
        save_journal_on(&conn, &journal).await?;
        return Ok(None);
    }
    let delay_secs = move_retry_delay_secs(attempt);
    let next_attempt_at = chrono::Utc::now().timestamp() + delay_secs;
    journal.retry = MoveRetrySchedule {
        next_attempt_at: Some(next_attempt_at),
        attempt_count: attempt,
        last_error_class: Some(error_class.into()),
        phase: Some(phase.into()),
    };
    save_journal_on(&conn, &journal).await?;
    Ok(Some(next_attempt_at))
}

pub async fn clear_move_retry(task_id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    if let Some(mut journal) = get_journal_on(&conn, task_id).await? {
        if journal.retry != MoveRetrySchedule::default() {
            journal.retry = MoveRetrySchedule::default();
            save_journal_on(&conn, &journal).await?;
        }
    }
    Ok(())
}

pub async fn schedule_move_task_retry(
    task_id: &str,
    phase: &str,
    error_class: &str,
) -> DbResult<Option<i64>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT attempt_count FROM move_task_retries WHERE task_id = ?1",
            turso::params![task_id],
        )
        .await?;
    let attempt = match rows.next().await? {
        Some(row) => row.get::<i64>(0)?.max(0) as u32,
        None => 0,
    }
    .saturating_add(1);
    if attempt > MAX_PERSISTED_RETRIES {
        conn.execute(
            "INSERT INTO move_task_retries (task_id, next_attempt_at, attempt_count, last_error_class, phase)
             VALUES (?1, NULL, ?2, ?3, ?4)
             ON CONFLICT(task_id) DO UPDATE SET next_attempt_at = NULL, attempt_count = excluded.attempt_count, last_error_class = excluded.last_error_class, phase = excluded.phase",
            turso::params![task_id, attempt as i64, error_class, phase],
        )
        .await?;
        return Ok(None);
    }
    let next_attempt_at = chrono::Utc::now().timestamp() + move_retry_delay_secs(attempt);
    conn.execute(
        "INSERT INTO move_task_retries (task_id, next_attempt_at, attempt_count, last_error_class, phase)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(task_id) DO UPDATE SET next_attempt_at = excluded.next_attempt_at, attempt_count = excluded.attempt_count, last_error_class = excluded.last_error_class, phase = excluded.phase",
        turso::params![task_id, next_attempt_at, attempt as i64, error_class, phase],
    )
    .await?;
    Ok(Some(next_attempt_at))
}

pub async fn clear_move_task_retry(task_id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "DELETE FROM move_task_retries WHERE task_id = ?1",
        turso::params![task_id],
    )
    .await?;
    Ok(())
}

async fn get_journal_on(conn: &turso::Connection, task_id: &str) -> DbResult<Option<MoveJournal>> {
    let mut rows = conn
        .query(
            "SELECT data FROM move_journal WHERE task_id = ?1",
            turso::params![task_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(serde_json::from_str(&row.get::<String>(0)?)?)),
        None => Ok(None),
    }
}

pub async fn get_move_journal(task_id: &str) -> DbResult<Option<MoveJournal>> {
    let conn = get_connection()?.lock().await;
    get_journal_on(&conn, task_id).await
}

/// Fetch all cached sizes with bounded IN queries instead of a query per move.
/// These sizes are display hints; transfer identity always comes from HEAD.
pub async fn get_move_cached_sizes(
    bucket: &str,
    account_id: &str,
    keys: &[String],
) -> DbResult<std::collections::HashMap<String, i64>> {
    let conn = get_connection()?.lock().await;
    let mut result = std::collections::HashMap::new();
    for batch in keys.chunks(400) {
        let placeholders = (3..batch.len() + 3)
            .map(|n| format!("?{n}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT key, size FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key IN ({placeholders})"
        );
        let mut params: Vec<turso::Value> =
            vec![bucket.to_string().into(), account_id.to_string().into()];
        params.extend(batch.iter().cloned().map(turso::Value::from));
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            result.insert(row.get(0)?, row.get(1)?);
        }
    }
    Ok(result)
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

    CREATE TABLE IF NOT EXISTS move_task_retries (
        task_id TEXT PRIMARY KEY,
        next_attempt_at INTEGER,
        attempt_count INTEGER NOT NULL DEFAULT 0,
        last_error_class TEXT,
        phase TEXT
    );

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

    CREATE TABLE IF NOT EXISTS move_journal (
        task_id TEXT PRIMARY KEY,
        data TEXT NOT NULL
    );
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

/// Reset only the MPU proven missing by the caller, in the same durable
/// transaction as its parts and recovery phase. A concurrent replacement or
/// task removal must not erase a newer upload or resurrect a removed task.
pub async fn reset_missing_move_upload(
    expected_upload_id: &str,
    journal: &MoveJournal,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    reset_missing_upload_on(&conn, expected_upload_id, journal).await
}

async fn reset_missing_upload_on(
    conn: &turso::Connection,
    expected_upload_id: &str,
    journal: &MoveJournal,
) -> DbResult<()> {
    if journal.stage != "transferring" || journal.destination.is_some() {
        return Err("Missing upload reset requires an uncommitted transfer".into());
    }
    conn.execute("PRAGMA synchronous = FULL", ()).await?;
    let result: DbResult<()> = async {
        conn.execute("BEGIN IMMEDIATE", ()).await?;
        let mut rows = conn.query(
            "SELECT u.upload_id FROM move_upload_sessions u INNER JOIN move_sessions m ON m.id = u.task_id WHERE u.task_id = ?1",
            turso::params![journal.task_id.as_str()],
        ).await?;
        let upload_id = match rows.next().await? {
            Some(row) => row.get::<String>(0)?,
            None => return Err("Move task or multipart upload was removed; recovery record retained".into()),
        };
        drop(rows);
        if upload_id != expected_upload_id {
            return Err("Multipart upload changed during reconciliation; newer upload retained".into());
        }
        let current = get_journal_on(conn, &journal.task_id).await?
            .ok_or("Move recovery journal was removed; multipart upload retained")?;
        if !matches!(current.stage.as_str(), "transferring" | "outcome_unknown")
            || current.destination.is_some()
            || current.task_id != journal.task_id
            || current.source != journal.source
            || current.source_scope != journal.source_scope
            || current.dest_scope != journal.dest_scope
        {
            return Err("Move recovery journal advanced during reconciliation; current receipt retained".into());
        }
        conn.execute("DELETE FROM move_upload_parts WHERE task_id = ?1", turso::params![journal.task_id.as_str()]).await?;
        conn.execute("DELETE FROM move_upload_sessions WHERE task_id = ?1", turso::params![journal.task_id.as_str()]).await?;
        conn.execute(
            "INSERT INTO move_journal (task_id, data) VALUES (?1, ?2) ON CONFLICT(task_id) DO UPDATE SET data = excluded.data",
            turso::params![journal.task_id.as_str(), serde_json::to_string(journal)?],
        ).await?;
        conn.execute("COMMIT", ()).await?;
        Ok(())
    }.await;
    if result.is_err() {
        let _ = conn.execute("ROLLBACK", ()).await;
    }
    let _ = conn.execute("PRAGMA synchronous = NORMAL", ()).await;
    result
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
    get_pending_moves_for_source_on(&conn, source_bucket, source_account_id, limit).await
}

/// Pending tasks of one source, with whichever retry records they have.
const PENDING_RETRY_JOIN: &str = "FROM move_sessions s
         LEFT JOIN move_task_retries r ON r.task_id = s.id
         LEFT JOIN move_journal j ON j.task_id = s.id
         WHERE s.source_bucket = ?1 AND s.source_account_id = ?2
         AND s.status = 'pending'";
/// When a pending task may run next: a preflight retry row wins over the
/// journal's schedule, and neither means now. Evaluated in SQL so choosing
/// work is one bounded statement, not two queries and a JSON parse per
/// pending task under the global DB lock.
const NEXT_RETRY_AT: &str =
    "COALESCE(r.next_attempt_at, json_extract(j.data, '$.retry.next_attempt_at'))";

async fn get_pending_moves_for_source_on(
    conn: &turso::Connection,
    source_bucket: &str,
    source_account_id: &str,
    limit: i64,
) -> DbResult<Vec<MoveSession>> {
    let mut rows = conn
        .query(
            &format!(
                "SELECT s.id, s.source_key, s.dest_key, s.source_bucket, s.source_account_id,
                s.source_provider, s.dest_bucket, s.dest_account_id, s.dest_provider,
                s.delete_original, s.file_size, s.progress, s.status, s.error, s.created_at,
                s.updated_at
         {PENDING_RETRY_JOIN}
         AND COALESCE({NEXT_RETRY_AT}, ?3) <= ?3
         ORDER BY s.created_at ASC
         LIMIT ?4"
            ),
            turso::params![
                source_bucket,
                source_account_id,
                chrono::Utc::now().timestamp(),
                limit
            ],
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
/// Finishing retains a worker slot, so slow deletion cannot create an unbounded queue.
pub async fn count_active_moves(source_bucket: &str, source_account_id: &str) -> DbResult<i64> {
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
         WHERE status IN ('pending', 'downloading', 'uploading', 'finishing', 'deleting', 'paused', 'delete_pending', 'outcome_unknown', 'needs_auth', 'conflict', 'needs_action')
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

pub async fn get_next_move_retry_attempt_for_source(
    source_bucket: &str,
    source_account_id: &str,
) -> DbResult<Option<i64>> {
    let conn = get_connection()?.lock().await;
    get_next_move_retry_attempt_for_source_on(&conn, source_bucket, source_account_id).await
}

async fn get_next_move_retry_attempt_for_source_on(
    conn: &turso::Connection,
    source_bucket: &str,
    source_account_id: &str,
) -> DbResult<Option<i64>> {
    let mut rows = conn
        .query(
            &format!("SELECT MIN({NEXT_RETRY_AT}) {PENDING_RETRY_JOIN}"),
            turso::params![source_bucket, source_account_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<Option<i64>>(0)?),
        None => Ok(None),
    }
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
         WHERE status IN ('pending', 'downloading', 'uploading', 'finishing', 'deleting', 'delete_pending', 'outcome_unknown')",
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
        "DELETE FROM move_task_retries WHERE task_id = ?1",
        turso::params![session_id],
    )
    .await?;
    conn.execute(
        "DELETE FROM move_journal WHERE task_id = ?1",
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
             AND status IN ('success', 'cancelled')",
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
            "DELETE FROM move_journal WHERE task_id = ?1",
            turso::params![session_id.as_str()],
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
    delete_all_moves_on(&conn, source_bucket, source_account_id).await
}

async fn delete_all_moves_on(
    conn: &turso::Connection,
    source_bucket: &str,
    source_account_id: &str,
) -> DbResult<i64> {
    // First, get the IDs of all sessions (libsql doesn't support subqueries in IN clauses)
    let mut rows = conn
        .query(
            "SELECT id, status FROM move_sessions WHERE source_bucket = ?1 AND source_account_id = ?2",
            turso::params![source_bucket, source_account_id],
        )
        .await?;

    let mut session_ids: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        let status: String = row.get(1)?;
        if matches!(
            status.as_str(),
            "downloading" | "uploading" | "finishing" | "deleting"
        ) {
            return Err("Cannot clear all moves while moves are active".into());
        }
        session_ids.push(row.get(0)?);
    }

    // Validate the entire set under the same database lock before deleting
    // anything. A stale UI must not discard a just-published recovery receipt.
    for id in &session_ids {
        if get_journal_on(conn, id).await?.is_some_and(|journal| {
            matches!(
                journal.stage.as_str(),
                "copied" | "delete_pending" | "delete_unknown" | "outcome_unknown"
            )
        }) {
            return Err("Resolve moves that need attention before clearing all tasks; their recovery records were retained".into());
        }
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
            "DELETE FROM move_journal WHERE task_id = ?1",
            turso::params![session_id.as_str()],
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
            "DELETE FROM move_journal WHERE task_id = ?1",
            turso::params![session_id.as_str()],
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
    use super::*;

    #[tokio::test]
    async fn bulk_clear_rejects_unresolved_receipts_before_deleting_any_task() {
        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(get_table_sql()).await.unwrap();
        for id in ["finished", "recover"] {
            conn.execute("INSERT INTO move_sessions (id, source_key, dest_key, source_bucket, source_account_id, source_provider, dest_bucket, dest_account_id, dest_provider, created_at, updated_at, status) VALUES (?1, 'x', 'y', 'source', 'account', 'r2', 'target', 'account', 'r2', 0, 0, 'cancelled')",turso::params![id]).await.unwrap();
        }
        let mut journal = MoveJournal {
            task_id: "recover".into(),
            stage: "copied".into(),
            source: SourceIdentity {
                size: 4,
                etag: "source".into(),
                version_id: None,
            },
            source_scope: "source".into(),
            dest_scope: "target".into(),
            destination: None,
            retry: Default::default(),
            metrics: Default::default(),
        };
        for phase in [
            "copied",
            "delete_pending",
            "delete_unknown",
            "outcome_unknown",
        ] {
            journal.stage = phase.into();
            save_journal_on(&conn, &journal).await.unwrap();
            assert!(delete_all_moves_on(&conn, "source", "account")
                .await
                .is_err());
            let mut rows = conn
                .query("SELECT count(*) FROM move_sessions", ())
                .await
                .unwrap();
            assert_eq!(
                rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
                2
            );
            assert_eq!(
                get_journal_on(&conn, "recover")
                    .await
                    .unwrap()
                    .unwrap()
                    .stage,
                phase
            );
        }
        journal.stage = "complete".into();
        save_journal_on(&conn, &journal).await.unwrap();
        assert_eq!(
            delete_all_moves_on(&conn, "source", "account")
                .await
                .unwrap(),
            2
        );
        assert!(get_journal_on(&conn, "recover").await.unwrap().is_none());
    }

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

    #[test]
    fn retry_schedule_covers_five_minute_outage_with_capped_delays() {
        let delays = (1..=MAX_PERSISTED_RETRIES)
            .map(move_retry_delay_secs)
            .collect::<Vec<_>>();
        assert_eq!(&delays[..6], &[1, 2, 4, 8, 16, 30]);
        assert!(delays
            .iter()
            .all(|delay| *delay <= MAX_MOVE_RETRY_DELAY_SECS));
        assert!(delays.iter().sum::<i64>() >= 5 * 60);
    }

    #[tokio::test]
    async fn preflight_retry_schedule_filters_and_restores_future_wake() {
        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(get_table_sql()).await.unwrap();
        for (id, created_at) in [("future", 0_i64), ("ready", 1_i64)] {
            conn.execute("INSERT INTO move_sessions (id, source_key, dest_key, source_bucket, source_account_id, source_provider, dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress, created_at, updated_at, status) VALUES (?1, 'source', 'dest', 'bucket', 'account', 'minio', 'bucket', 'account', 'minio', 0, 8, 0, ?2, ?2, 'pending')",turso::params![id, created_at]).await.unwrap();
        }
        conn.execute(
            "INSERT INTO move_task_retries (task_id, next_attempt_at, attempt_count, last_error_class, phase) VALUES ('future', ?1, 1, 'transient', 'preflight')",
            turso::params![chrono::Utc::now().timestamp() + 60],
        )
        .await
        .unwrap();
        let ready = get_pending_moves_for_source_on(&conn, "bucket", "account", 1)
            .await
            .unwrap();
        assert_eq!(
            ready.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["ready"]
        );
        assert!(
            get_next_move_retry_attempt_for_source_on(&conn, "bucket", "account")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn retry_schedule_is_backward_compatible_and_filters_future_pending_tasks() {
        let legacy = r#"{
            "task_id":"move",
            "stage":"transferring",
            "source":{"size":8,"etag":"source","version_id":null},
            "source_scope":"source",
            "dest_scope":"dest",
            "destination":null
        }"#;
        let journal: MoveJournal = serde_json::from_str(legacy).unwrap();
        assert_eq!(journal.retry.attempt_count, 0);
        assert_eq!(journal.retry.next_attempt_at, None);
        assert_eq!(journal.retry.last_error_class, None);
        assert_eq!(journal.retry.phase, None);

        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(get_table_sql()).await.unwrap();
        for (id, created_at) in [("future", 0_i64), ("ready", 1_i64)] {
            conn.execute("INSERT INTO move_sessions (id, source_key, dest_key, source_bucket, source_account_id, source_provider, dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress, created_at, updated_at, status) VALUES (?1, 'source', 'dest', 'bucket', 'account', 'minio', 'bucket', 'account', 'minio', 0, 8, 0, ?2, ?2, 'pending')",turso::params![id, created_at]).await.unwrap();
        }
        let mut future = journal.clone();
        future.task_id = "future".into();
        future.retry = MoveRetrySchedule {
            next_attempt_at: Some(chrono::Utc::now().timestamp() + 60),
            attempt_count: 2,
            last_error_class: Some("transient".into()),
            phase: Some("UploadPartCopy".into()),
        };
        save_journal_on(&conn, &future).await.unwrap();
        let ready = get_pending_moves_for_source_on(&conn, "bucket", "account", 1)
            .await
            .unwrap();
        assert_eq!(
            get_next_move_retry_attempt_for_source_on(&conn, "bucket", "account")
                .await
                .unwrap(),
            future.retry.next_attempt_at
        );
        assert_eq!(
            ready.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["ready"]
        );
    }

    #[tokio::test]
    async fn pending_selection_skips_deferred_tasks_in_creation_order_past_the_limit() {
        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(get_table_sql()).await.unwrap();
        let now = chrono::Utc::now().timestamp();
        let insert = |id: String, bucket: &'static str, status: &'static str, created_at: i64| {
            let conn = conn.clone();
            async move {
                conn.execute("INSERT INTO move_sessions (id, source_key, dest_key, source_bucket, source_account_id, source_provider, dest_bucket, dest_account_id, dest_provider, delete_original, file_size, progress, created_at, updated_at, status) VALUES (?1, 'source', 'dest', ?2, 'account', 'minio', 'target', 'account', 'minio', 0, 8, 0, ?3, ?3, ?4)", turso::params![id, bucket, created_at, status]).await.unwrap();
            }
        };
        let task_retry = |id: &'static str, next: Option<i64>| {
            let conn = conn.clone();
            async move {
                conn.execute("INSERT INTO move_task_retries (task_id, next_attempt_at, attempt_count, last_error_class, phase) VALUES (?1, ?2, 1, 'transient', 'preflight')", turso::params![id, next]).await.unwrap();
            }
        };
        let journal_retry = |id: &'static str, next: i64| {
            let conn = conn.clone();
            async move {
                let journal = MoveJournal {
                    task_id: id.into(),
                    stage: "transferring".into(),
                    source: SourceIdentity {
                        size: 8,
                        etag: "source".into(),
                        version_id: None,
                    },
                    source_scope: "source".into(),
                    dest_scope: "dest".into(),
                    destination: None,
                    retry: MoveRetrySchedule {
                        next_attempt_at: Some(next),
                        attempt_count: 1,
                        last_error_class: Some("transient".into()),
                        phase: Some("GET".into()),
                    },
                    metrics: Default::default(),
                };
                save_journal_on(&conn, &journal).await.unwrap();
            }
        };
        // Deferred tasks come first in creation order, so a selection that
        // honours the limit before filtering would return none of them.
        for (created_at, id) in [
            "deferred-task-retry",
            "deferred-journal",
            "exhausted-task-retry-deferred-journal",
            "due-task-retry-over-deferred-journal",
            "due-journal",
            "plain-1",
            "plain-2",
            "plain-3",
            "plain-4",
        ]
        .into_iter()
        .enumerate()
        {
            insert(id.into(), "bucket", "pending", created_at as i64).await;
        }
        for filler in 0..40 {
            insert(
                format!("later-{filler:02}"),
                "bucket",
                "pending",
                100 + filler,
            )
            .await;
        }
        insert("other-status".into(), "bucket", "paused", -2).await;
        insert("other-source".into(), "elsewhere", "pending", -1).await;
        task_retry("deferred-task-retry", Some(now + 60)).await;
        journal_retry("deferred-journal", now + 120).await;
        task_retry("exhausted-task-retry-deferred-journal", None).await;
        journal_retry("exhausted-task-retry-deferred-journal", now + 30).await;
        task_retry("due-task-retry-over-deferred-journal", Some(now - 10)).await;
        journal_retry("due-task-retry-over-deferred-journal", now + 90).await;
        journal_retry("due-journal", now - 5).await;
        task_retry("other-status", Some(now - 100)).await;
        task_retry("other-source", Some(now - 100)).await;

        let ids = |sessions: Vec<MoveSession>| {
            sessions
                .into_iter()
                .map(|session| session.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(
                get_pending_moves_for_source_on(&conn, "bucket", "account", 4)
                    .await
                    .unwrap()
            ),
            [
                "due-task-retry-over-deferred-journal",
                "due-journal",
                "plain-1",
                "plain-2"
            ]
        );
        let all = ids(
            get_pending_moves_for_source_on(&conn, "bucket", "account", 1000)
                .await
                .unwrap(),
        );
        assert_eq!(all.len(), 46);
        assert_eq!(
            all[..6],
            [
                "due-task-retry-over-deferred-journal",
                "due-journal",
                "plain-1",
                "plain-2",
                "plain-3",
                "plain-4"
            ]
        );
        assert_eq!(all[6], "later-00");
        assert_eq!(all[45], "later-39");
        // The earliest scheduled retry of any pending task on this source,
        // due or not; other sources and statuses do not count.
        assert_eq!(
            get_next_move_retry_attempt_for_source_on(&conn, "bucket", "account")
                .await
                .unwrap(),
            Some(now - 10)
        );
        assert_eq!(
            get_next_move_retry_attempt_for_source_on(&conn, "elsewhere", "account")
                .await
                .unwrap(),
            Some(now - 100)
        );
        assert_eq!(
            get_next_move_retry_attempt_for_source_on(&conn, "nothing", "account")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn copied_receipt_survives_session_pause_and_reopen() {
        let path = std::env::temp_dir().join(format!(
            "r2-move-journal-{}-{}.db",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let database = turso::Builder::new_local(path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(get_table_sql()).await.unwrap();
        let identity = SourceIdentity {
            size: 42,
            etag: "frozen".into(),
            version_id: Some("v1".into()),
        };
        let mut journal = MoveJournal {
            task_id: "move".into(),
            stage: "copied".into(),
            source: identity.clone(),
            source_scope: "source".into(),
            dest_scope: "dest".into(),
            destination: Some(identity),
            retry: Default::default(),
            metrics: Default::default(),
        };
        save_journal_on(&conn, &journal).await.unwrap();
        conn.execute("INSERT INTO move_sessions (id, source_key, dest_key, source_bucket, source_account_id, source_provider, dest_bucket, dest_account_id, dest_provider, created_at, updated_at, status) VALUES ('move', 'x', 'y', 'a', 'a', 'r2', 'b', 'b', 'r2', 0, 0, 'error')", ()).await.unwrap();
        // UI retry is deliberately independent of the copy/delete receipt.
        conn.execute(
            "UPDATE move_sessions SET status = 'paused' WHERE id = 'move'",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "UPDATE move_sessions SET status = 'pending' WHERE id = 'move'",
            (),
        )
        .await
        .unwrap();
        journal.stage = "delete_pending".into();
        save_journal_on(&conn, &journal).await.unwrap();
        drop(conn);
        drop(database);
        let database = turso::Builder::new_local(path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        let recovered = get_journal_on(&conn, "move").await.unwrap().unwrap();
        assert_eq!(recovered.stage, "delete_pending");
        assert_eq!(recovered.source, journal.source);
        assert_eq!(recovered.destination, journal.destination);
        conn.execute(
            "UPDATE move_journal SET data = 'corrupt' WHERE task_id = 'move'",
            (),
        )
        .await
        .unwrap();
        assert!(get_journal_on(&conn, "move").await.is_err());
        drop(conn);
        drop(database);
        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn expired_upload_reset_retains_newer_upload_and_removed_task_receipts() {
        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(get_table_sql()).await.unwrap();
        conn.execute("INSERT INTO move_sessions (id, source_key, dest_key, source_bucket, source_account_id, source_provider, dest_bucket, dest_account_id, dest_provider, created_at, updated_at, status) VALUES ('move', 'source', 'dest', 'bucket', 'account', 'minio', 'bucket', 'account', 'minio', 0, 0, 'error')", ()).await.unwrap();
        let mut journal = MoveJournal {
            task_id: "move".into(),
            stage: "outcome_unknown".into(),
            source: SourceIdentity {
                size: 8,
                etag: "source".into(),
                version_id: None,
            },
            source_scope: "source".into(),
            dest_scope: "dest".into(),
            destination: None,
            retry: Default::default(),
            metrics: Default::default(),
        };
        save_journal_on(&conn, &journal).await.unwrap();
        conn.execute(
            "INSERT INTO move_upload_sessions VALUES ('move', 'new-upload', 5242880)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO move_upload_parts VALUES ('move', 1, 'part', 8)",
            (),
        )
        .await
        .unwrap();
        journal.stage = "transferring".into();
        assert!(reset_missing_upload_on(&conn, "old-upload", &journal)
            .await
            .is_err());
        assert_eq!(
            get_journal_on(&conn, "move").await.unwrap().unwrap().stage,
            "outcome_unknown"
        );
        let mut parts = conn
            .query("SELECT count(*) FROM move_upload_parts", ())
            .await
            .unwrap();
        assert_eq!(
            parts.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1
        );
        drop(parts);
        conn.execute("DELETE FROM move_sessions WHERE id = 'move'", ())
            .await
            .unwrap();
        assert!(reset_missing_upload_on(&conn, "new-upload", &journal)
            .await
            .is_err());
        assert_eq!(
            get_journal_on(&conn, "move").await.unwrap().unwrap().stage,
            "outcome_unknown"
        );
    }
    #[tokio::test]
    async fn expired_upload_reset_rejects_an_advanced_journal_even_when_upload_id_matches() {
        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(get_table_sql()).await.unwrap();
        conn.execute("INSERT INTO move_sessions (id, source_key, dest_key, source_bucket, source_account_id, source_provider, dest_bucket, dest_account_id, dest_provider, created_at, updated_at, status) VALUES ('move', 'source', 'dest', 'bucket', 'account', 'minio', 'bucket', 'account', 'minio', 0, 0, 'error')", ()).await.unwrap();
        conn.execute(
            "INSERT INTO move_upload_sessions VALUES ('move', 'same-upload', 5242880)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO move_upload_parts VALUES ('move', 1, 'part', 8)",
            (),
        )
        .await
        .unwrap();
        let requested = MoveJournal {
            task_id: "move".into(),
            stage: "transferring".into(),
            source: SourceIdentity {
                size: 8,
                etag: "source".into(),
                version_id: None,
            },
            source_scope: "source".into(),
            dest_scope: "dest".into(),
            destination: None,
            retry: Default::default(),
            metrics: Default::default(),
        };
        for scenario in ["copied", "receipt", "source-change"] {
            let mut current = requested.clone();
            match scenario {
                "copied" => current.stage = "copied".into(),
                "receipt" => {
                    current.stage = "outcome_unknown".into();
                    current.destination = Some(SourceIdentity {
                        size: 8,
                        etag: "published".into(),
                        version_id: None,
                    });
                }
                _ => current.source.etag = "new-source".into(),
            }
            save_journal_on(&conn, &current).await.unwrap();
            assert!(
                reset_missing_upload_on(&conn, "same-upload", &requested)
                    .await
                    .is_err(),
                "{scenario}"
            );
            let stored = get_journal_on(&conn, "move").await.unwrap().unwrap();
            assert_eq!(stored.stage, current.stage);
            assert_eq!(stored.destination, current.destination);
            assert_eq!(stored.source, current.source);
            let mut parts = conn
                .query("SELECT count(*) FROM move_upload_parts", ())
                .await
                .unwrap();
            assert_eq!(
                parts.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
                1
            );
        }
    }
}
