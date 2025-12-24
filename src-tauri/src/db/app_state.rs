use super::{get_connection, DbResult};

/// Get SQL for creating app_state table
pub fn get_table_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS app_state (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    "
}

// ============ App State Functions ============

/// Get app state value
#[allow(dead_code)]
pub async fn get_app_state(key: &str) -> DbResult<Option<String>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query("SELECT value FROM app_state WHERE key = ?1", turso::params![key]).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

/// Set app state value
pub async fn set_app_state(key: &str, value: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = ?2",
        turso::params![key, value],
    ).await?;
    Ok(())
}

/// Delete app state value
#[allow(dead_code)]
pub async fn delete_app_state(key: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute("DELETE FROM app_state WHERE key = ?1", turso::params![key]).await?;
    Ok(())
}
