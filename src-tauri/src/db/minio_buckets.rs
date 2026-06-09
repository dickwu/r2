use super::{get_connection, DbResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinioBucket {
    pub id: i64,
    pub account_id: String,
    pub name: String,
    pub public_domain_scheme: Option<String>,
    pub public_domain_host: Option<String>,
    pub is_public: bool,
    pub public_path_prefix: Option<String>,
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
        is_public INTEGER NOT NULL DEFAULT 0,
        public_path_prefix TEXT,
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
            "SELECT id, account_id, name, public_domain_scheme, public_domain_host, is_public, public_path_prefix, created_at, updated_at
             FROM minio_buckets WHERE account_id = ?1 ORDER BY name",
            turso::params![account_id],
        )
        .await?;

    let mut buckets = Vec::new();
    while let Some(row) = rows.next().await? {
        let is_public: i64 = row.get(5)?;
        buckets.push(MinioBucket {
            id: row.get(0)?,
            account_id: row.get(1)?,
            name: row.get(2)?,
            public_domain_scheme: row.get(3)?,
            public_domain_host: row.get(4)?,
            is_public: is_public != 0,
            public_path_prefix: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        });
    }
    Ok(buckets)
}

#[allow(clippy::type_complexity)]
pub async fn save_minio_buckets_for_account(
    account_id: &str,
    buckets: &[(String, Option<String>, Option<String>, bool, Option<String>)],
) -> DbResult<Vec<MinioBucket>> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "DELETE FROM minio_buckets WHERE account_id = ?1",
        turso::params![account_id],
    )
    .await?;

    let mut result = Vec::new();
    for (name, public_domain_scheme, public_domain_host, is_public, public_path_prefix) in buckets {
        conn.execute(
            "INSERT INTO minio_buckets (account_id, name, public_domain_scheme, public_domain_host, is_public, public_path_prefix, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            turso::params![
                account_id,
                name.clone(),
                public_domain_scheme.clone(),
                public_domain_host.clone(),
                *is_public as i64,
                public_path_prefix.clone(),
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
            is_public: *is_public,
            public_path_prefix: public_path_prefix.clone(),
            created_at: now,
            updated_at: now,
        });
    }

    Ok(result)
}
