use super::{get_connection, DbResult};

pub fn get_table_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS prefix_sync_times (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        prefix TEXT NOT NULL,
        last_synced_at INTEGER NOT NULL,
        file_count INTEGER NOT NULL DEFAULT 0,
        folder_count INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (bucket, account_id, prefix)
    );
    CREATE INDEX IF NOT EXISTS idx_prefix_sync ON prefix_sync_times(bucket, account_id, prefix);
    "
}

/// Get the last sync time for a prefix. Returns None if never synced.
pub async fn get_prefix_sync_time(
    bucket: &str,
    account_id: &str,
    prefix: &str,
) -> DbResult<Option<i64>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT last_synced_at FROM prefix_sync_times
             WHERE bucket = ?1 AND account_id = ?2 AND prefix = ?3",
            turso::params![bucket, account_id, prefix],
        )
        .await?;

    if let Some(row) = rows.next().await? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

/// Update the sync time for a prefix after a successful lazy sync.
pub async fn set_prefix_sync_time(
    bucket: &str,
    account_id: &str,
    prefix: &str,
    file_count: i32,
    folder_count: i32,
) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT INTO prefix_sync_times (bucket, account_id, prefix, last_synced_at, file_count, folder_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (bucket, account_id, prefix) DO UPDATE SET
           last_synced_at = ?4, file_count = ?5, folder_count = ?6",
        turso::params![bucket, account_id, prefix, now, file_count, folder_count],
    )
    .await?;
    Ok(())
}

/// Clear all prefix sync times for a bucket (used when switching accounts or full re-sync).
#[allow(dead_code)]
pub async fn clear_prefix_sync_times(bucket: &str, account_id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "DELETE FROM prefix_sync_times WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;
    Ok(())
}
