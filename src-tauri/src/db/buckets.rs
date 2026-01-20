use serde::{Deserialize, Serialize};
use super::{get_connection, DbResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bucket {
    pub id: i64,
    pub token_id: i64,
    pub name: String,
    pub public_domain: Option<String>,
    pub public_domain_scheme: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Create a new bucket
#[allow(dead_code)]
pub async fn create_bucket(
    token_id: i64,
    name: &str,
    public_domain: Option<&str>,
    public_domain_scheme: Option<&str>,
) -> DbResult<Bucket> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO buckets (token_id, name, public_domain, public_domain_scheme, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        turso::params![token_id, name, public_domain, public_domain_scheme, now, now],
    ).await?;
    let id = conn.last_insert_rowid();
    Ok(Bucket {
        id,
        token_id,
        name: name.to_string(),
        public_domain: public_domain.map(|s| s.to_string()),
        public_domain_scheme: public_domain_scheme.map(|s| s.to_string()),
        created_at: now,
        updated_at: now,
    })
}

/// Get bucket by ID
#[allow(dead_code)]
pub async fn get_bucket(id: i64) -> DbResult<Option<Bucket>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT id, token_id, name, public_domain, public_domain_scheme, created_at, updated_at FROM buckets WHERE id = ?1",
        turso::params![id]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(Bucket {
            id: row.get(0)?,
            token_id: row.get(1)?,
            name: row.get(2)?,
            public_domain: row.get(3)?,
            public_domain_scheme: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        }))
    } else {
        Ok(None)
    }
}

/// List buckets by token
pub async fn list_buckets_by_token(token_id: i64) -> DbResult<Vec<Bucket>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT id, token_id, name, public_domain, public_domain_scheme, created_at, updated_at
         FROM buckets WHERE token_id = ?1 ORDER BY name",
        turso::params![token_id]
    ).await?;
    
    let mut buckets = Vec::new();
    while let Some(row) = rows.next().await? {
        buckets.push(Bucket {
            id: row.get(0)?,
            token_id: row.get(1)?,
            name: row.get(2)?,
            public_domain: row.get(3)?,
            public_domain_scheme: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        });
    }
    Ok(buckets)
}

/// Update bucket
pub async fn update_bucket(
    id: i64,
    public_domain: Option<&str>,
    public_domain_scheme: Option<&str>,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE buckets SET public_domain = ?1, public_domain_scheme = ?2, updated_at = ?3 WHERE id = ?4",
        turso::params![public_domain, public_domain_scheme, now, id],
    ).await?;
    Ok(())
}

/// Delete bucket
pub async fn delete_bucket(id: i64) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute("DELETE FROM buckets WHERE id = ?1", turso::params![id]).await?;
    Ok(())
}

/// Save multiple buckets for a token (replace existing)
pub async fn save_buckets_for_token(
    token_id: i64,
    buckets: &[(String, Option<String>, Option<String>)],
) -> DbResult<Vec<Bucket>> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    
    // Delete existing buckets for this token
    conn.execute("DELETE FROM buckets WHERE token_id = ?1", turso::params![token_id]).await?;
    
    // Insert new buckets
    let mut result = Vec::new();
    for (name, public_domain, public_domain_scheme) in buckets {
        conn.execute(
            "INSERT INTO buckets (token_id, name, public_domain, public_domain_scheme, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            turso::params![
                token_id,
                name.clone(),
                public_domain.clone(),
                public_domain_scheme.clone(),
                now,
                now
            ],
        ).await?;
        let id = conn.last_insert_rowid();
        result.push(Bucket {
            id,
            token_id,
            name: name.clone(),
            public_domain: public_domain.clone(),
            public_domain_scheme: public_domain_scheme.clone(),
            created_at: now,
            updated_at: now,
        });
    }
    Ok(result)
}