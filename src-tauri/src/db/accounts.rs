use super::{get_connection, DbResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String, // Cloudflare account_id
    pub name: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Create a new account
pub async fn create_account(id: &str, name: Option<&str>) -> DbResult<Account> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO accounts (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
        turso::params![id, name, now, now],
    )
    .await?;
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
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, name, created_at, updated_at FROM accounts WHERE id = ?1",
            turso::params![id],
        )
        .await?;

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
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, name, created_at, updated_at FROM accounts ORDER BY created_at",
            (),
        )
        .await?;

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
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE accounts SET name = ?1, updated_at = ?2 WHERE id = ?3",
        turso::params![name, now, id],
    )
    .await?;
    Ok(())
}

/// Delete account (manually cascades to tokens and buckets)
pub async fn delete_account(id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;

    // Get all tokens for this account
    let mut rows = conn
        .query(
            "SELECT id FROM tokens WHERE account_id = ?1",
            turso::params![id],
        )
        .await?;

    let mut token_ids: Vec<i64> = Vec::new();
    while let Some(row) = rows.next().await? {
        token_ids.push(row.get(0)?);
    }

    // Delete buckets for each token
    for token_id in &token_ids {
        conn.execute(
            "DELETE FROM buckets WHERE token_id = ?1",
            turso::params![*token_id],
        )
        .await?;
    }

    // Delete tokens
    conn.execute(
        "DELETE FROM tokens WHERE account_id = ?1",
        turso::params![id],
    )
    .await?;

    // Delete account
    conn.execute("DELETE FROM accounts WHERE id = ?1", turso::params![id])
        .await?;
    Ok(())
}

/// Check if any accounts exist
pub async fn has_accounts() -> DbResult<bool> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query("SELECT COUNT(*) FROM accounts", ()).await?;
    if let Some(row) = rows.next().await? {
        let count: i64 = row.get(0)?;
        Ok(count > 0)
    } else {
        Ok(false)
    }
}
