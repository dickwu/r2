use super::{get_connection, DbResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinioAccount {
    pub id: String,
    pub name: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint_scheme: String,
    pub endpoint_host: String,
    pub force_path_style: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn get_table_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS minio_accounts (
        id TEXT PRIMARY KEY,
        name TEXT,
        access_key_id TEXT NOT NULL,
        secret_access_key TEXT NOT NULL,
        endpoint_scheme TEXT NOT NULL,
        endpoint_host TEXT NOT NULL,
        force_path_style INTEGER NOT NULL DEFAULT 1,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_minio_accounts_created ON minio_accounts(created_at);
    "
}

async fn generate_id(conn: &turso::Connection) -> DbResult<String> {
    let mut rows = conn.query("SELECT lower(hex(randomblob(16)))", ()).await?;
    if let Some(row) = rows.next().await? {
        Ok(row.get(0)?)
    } else {
        Err("Failed to generate MinIO account id".into())
    }
}

pub async fn create_minio_account(
    name: Option<&str>,
    access_key_id: &str,
    secret_access_key: &str,
    endpoint_scheme: &str,
    endpoint_host: &str,
    force_path_style: bool,
) -> DbResult<MinioAccount> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    let id = generate_id(&conn).await?;
    let force_value = if force_path_style { 1 } else { 0 };

    conn.execute(
        "INSERT INTO minio_accounts (id, name, access_key_id, secret_access_key, endpoint_scheme, endpoint_host, force_path_style, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        turso::params![
            id.as_str(),
            name,
            access_key_id,
            secret_access_key,
            endpoint_scheme,
            endpoint_host,
            force_value,
            now,
            now
        ],
    ).await?;

    Ok(MinioAccount {
        id,
        name: name.map(|s| s.to_string()),
        access_key_id: access_key_id.to_string(),
        secret_access_key: secret_access_key.to_string(),
        endpoint_scheme: endpoint_scheme.to_string(),
        endpoint_host: endpoint_host.to_string(),
        force_path_style,
        created_at: now,
        updated_at: now,
    })
}

pub async fn list_minio_accounts() -> DbResult<Vec<MinioAccount>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, name, access_key_id, secret_access_key, endpoint_scheme, endpoint_host, force_path_style, created_at, updated_at
             FROM minio_accounts ORDER BY created_at",
            (),
        )
        .await?;

    let mut accounts = Vec::new();
    while let Some(row) = rows.next().await? {
        let force_value: i64 = row.get(6)?;
        accounts.push(MinioAccount {
            id: row.get(0)?,
            name: row.get(1)?,
            access_key_id: row.get(2)?,
            secret_access_key: row.get(3)?,
            endpoint_scheme: row.get(4)?,
            endpoint_host: row.get(5)?,
            force_path_style: force_value != 0,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        });
    }
    Ok(accounts)
}

pub async fn update_minio_account(
    id: &str,
    name: Option<&str>,
    access_key_id: &str,
    secret_access_key: &str,
    endpoint_scheme: &str,
    endpoint_host: &str,
    force_path_style: bool,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    let force_value = if force_path_style { 1 } else { 0 };

    conn.execute(
        "UPDATE minio_accounts
         SET name = ?1, access_key_id = ?2, secret_access_key = ?3,
             endpoint_scheme = ?4, endpoint_host = ?5, force_path_style = ?6, updated_at = ?7
         WHERE id = ?8",
        turso::params![
            name,
            access_key_id,
            secret_access_key,
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

pub async fn delete_minio_account(id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;

    conn.execute(
        "DELETE FROM minio_buckets WHERE account_id = ?1",
        turso::params![id],
    )
    .await?;

    conn.execute(
        "DELETE FROM minio_accounts WHERE id = ?1",
        turso::params![id],
    )
    .await?;

    Ok(())
}
