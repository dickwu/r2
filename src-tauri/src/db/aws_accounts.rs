use super::{get_connection, DbResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsAccount {
    pub id: String,
    pub name: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
    pub endpoint_scheme: String,
    pub endpoint_host: Option<String>,
    pub force_path_style: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn get_table_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS aws_accounts (
        id TEXT PRIMARY KEY,
        name TEXT,
        access_key_id TEXT NOT NULL,
        secret_access_key TEXT NOT NULL,
        region TEXT NOT NULL,
        endpoint_scheme TEXT NOT NULL,
        endpoint_host TEXT,
        force_path_style INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_aws_accounts_created ON aws_accounts(created_at);
    "
}

async fn generate_id(conn: &turso::Connection) -> DbResult<String> {
    let mut rows = conn.query("SELECT lower(hex(randomblob(16)))", ()).await?;
    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Err("Failed to generate AWS account id".into())
    }
}

pub async fn create_aws_account(
    name: Option<&str>,
    access_key_id: &str,
    secret_access_key: &str,
    region: &str,
    endpoint_scheme: &str,
    endpoint_host: Option<&str>,
    force_path_style: bool,
) -> DbResult<AwsAccount> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    let id = generate_id(&conn).await?;
    let force_value = if force_path_style { 1 } else { 0 };

    conn.execute(
        "INSERT INTO aws_accounts (id, name, access_key_id, secret_access_key, region, endpoint_scheme, endpoint_host, force_path_style, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        turso::params![
            id.as_str(),
            name,
            access_key_id,
            secret_access_key,
            region,
            endpoint_scheme,
            endpoint_host,
            force_value,
            now,
            now
        ],
    ).await?;

    Ok(AwsAccount {
        id,
        name: name.map(|s| s.to_string()),
        access_key_id: access_key_id.to_string(),
        secret_access_key: secret_access_key.to_string(),
        region: region.to_string(),
        endpoint_scheme: endpoint_scheme.to_string(),
        endpoint_host: endpoint_host.map(|s| s.to_string()),
        force_path_style,
        created_at: now,
        updated_at: now,
    })
}

pub async fn list_aws_accounts() -> DbResult<Vec<AwsAccount>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, name, access_key_id, secret_access_key, region, endpoint_scheme, endpoint_host, force_path_style, created_at, updated_at
             FROM aws_accounts ORDER BY created_at",
            (),
        )
        .await?;

    let mut accounts = Vec::new();
    while let Some(row) = rows.next().await? {
        let force_value: i64 = row.get(7)?;
        accounts.push(AwsAccount {
            id: row.get(0)?,
            name: row.get(1)?,
            access_key_id: row.get(2)?,
            secret_access_key: row.get(3)?,
            region: row.get(4)?,
            endpoint_scheme: row.get(5)?,
            endpoint_host: row.get(6)?,
            force_path_style: force_value != 0,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        });
    }
    Ok(accounts)
}

pub async fn update_aws_account(
    id: &str,
    name: Option<&str>,
    access_key_id: &str,
    secret_access_key: &str,
    region: &str,
    endpoint_scheme: &str,
    endpoint_host: Option<&str>,
    force_path_style: bool,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    let force_value = if force_path_style { 1 } else { 0 };

    conn.execute(
        "UPDATE aws_accounts
         SET name = ?1, access_key_id = ?2, secret_access_key = ?3, region = ?4,
             endpoint_scheme = ?5, endpoint_host = ?6, force_path_style = ?7, updated_at = ?8
         WHERE id = ?9",
        turso::params![
            name,
            access_key_id,
            secret_access_key,
            region,
            endpoint_scheme,
            endpoint_host,
            force_value,
            now,
            id
        ],
    )
    .await?;

    Ok(())
}

pub async fn delete_aws_account(id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;

    conn.execute(
        "DELETE FROM aws_buckets WHERE account_id = ?1",
        turso::params![id],
    )
    .await?;

    conn.execute("DELETE FROM aws_accounts WHERE id = ?1", turso::params![id])
        .await?;

    Ok(())
}
