use serde::{Deserialize, Serialize};
use super::{get_connection, DbResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinioBucket {
    pub id: i64,
    pub account_id: String,
    pub name: String,
    pub public_domain_scheme: Option<String>,
    pub public_domain_host: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn get_table_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS minio_buckets (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        account_id TEXT NOT NULL REFERENCES minio_accounts(id),
        name TEXT NOT NULL,
        public_domain_scheme TEXT,
        public_domain_host TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        UNIQUE(account_id, name)
    );

    CREATE INDEX IF NOT EXISTS idx_minio_buckets_account ON minio_buckets(account_id);
    CREATE INDEX IF NOT EXISTS idx_minio_buckets_unique ON minio_buckets(account_id, name);
    "
}

pub async fn list_minio_buckets_by_account(account_id: &str) -> DbResult<Vec<MinioBucket>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query(
            "SELECT id, account_id, name, public_domain_scheme, public_domain_host, created_at, updated_at
             FROM minio_buckets WHERE account_id = ?1 ORDER BY name",
            turso::params![account_id],
        )
        .await?;

    let mut buckets = Vec::new();
    while let Some(row) = rows.next().await? {
        buckets.push(MinioBucket {
            id: row.get(0)?,
            account_id: row.get(1)?,
            name: row.get(2)?,
            public_domain_scheme: row.get(3)?,
            public_domain_host: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        });
    }
    Ok(buckets)
}

pub async fn save_minio_buckets_for_account(
    account_id: &str,
    buckets: &[(String, Option<String>, Option<String>)],
) -> DbResult<Vec<MinioBucket>> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "DELETE FROM minio_buckets WHERE account_id = ?1",
        turso::params![account_id],
    ).await?;

    let mut result = Vec::new();
    for (name, public_domain_scheme, public_domain_host) in buckets {
        conn.execute(
            "INSERT INTO minio_buckets (account_id, name, public_domain_scheme, public_domain_host, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            turso::params![
                account_id,
                name.clone(),
                public_domain_scheme.clone(),
                public_domain_host.clone(),
                now,
                now
            ],
        ).await?;

        let id = conn.last_insert_rowid();
        result.push(MinioBucket {
            id,
            account_id: account_id.to_string(),
            name: name.clone(),
            public_domain_scheme: public_domain_scheme.clone(),
            public_domain_host: public_domain_host.clone(),
            created_at: now,
            updated_at: now,
        });
    }

    Ok(result)
}
