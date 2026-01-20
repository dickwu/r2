use serde::{Deserialize, Serialize};
use super::{get_connection, DbResult};

// ============ Token Struct ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub id: i64,
    pub account_id: String,
    pub name: Option<String>,
    pub api_token: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageProvider {
    R2,
    Aws,
    Minio,
    Rustfs,
}

/// Full configuration needed for storage operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentConfig {
    pub provider: StorageProvider,
    pub account_id: String,
    pub account_name: Option<String>,
    pub token_id: Option<i64>,
    pub token_name: Option<String>,
    pub api_token: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub public_domain: Option<String>,
    pub public_domain_scheme: Option<String>,
    pub region: Option<String>,
    pub endpoint_scheme: Option<String>,
    pub endpoint_host: Option<String>,
    pub force_path_style: Option<bool>,
}

/// Get SQL for creating token tables
pub fn get_table_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS tokens (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        account_id TEXT NOT NULL REFERENCES accounts(id),
        name TEXT,
        api_token TEXT NOT NULL,
        access_key_id TEXT NOT NULL,
        secret_access_key TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_tokens_account ON tokens(account_id);
    "
}

// ============ Token CRUD Functions ============

/// Create a new token
pub async fn create_token(
    account_id: &str,
    name: Option<&str>,
    api_token: &str,
    access_key_id: &str,
    secret_access_key: &str,
) -> DbResult<Token> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO tokens (account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        turso::params![account_id, name, api_token, access_key_id, secret_access_key, now, now],
    ).await?;
    let id = conn.last_insert_rowid();
    Ok(Token {
        id,
        account_id: account_id.to_string(),
        name: name.map(|s| s.to_string()),
        api_token: api_token.to_string(),
        access_key_id: access_key_id.to_string(),
        secret_access_key: secret_access_key.to_string(),
        created_at: now,
        updated_at: now,
    })
}

/// Get token by ID
pub async fn get_token(id: i64) -> DbResult<Option<Token>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT id, account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at
         FROM tokens WHERE id = ?1",
        turso::params![id]
    ).await?;
    
    if let Some(row) = rows.next().await? {
        Ok(Some(Token {
            id: row.get(0)?,
            account_id: row.get(1)?,
            name: row.get(2)?,
            api_token: row.get(3)?,
            access_key_id: row.get(4)?,
            secret_access_key: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        }))
    } else {
        Ok(None)
    }
}

/// List tokens by account
pub async fn list_tokens_by_account(account_id: &str) -> DbResult<Vec<Token>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn.query(
        "SELECT id, account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at
         FROM tokens WHERE account_id = ?1 ORDER BY created_at",
        turso::params![account_id]
    ).await?;
    
    let mut tokens = Vec::new();
    while let Some(row) = rows.next().await? {
        tokens.push(Token {
            id: row.get(0)?,
            account_id: row.get(1)?,
            name: row.get(2)?,
            api_token: row.get(3)?,
            access_key_id: row.get(4)?,
            secret_access_key: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        });
    }
    Ok(tokens)
}

/// Update token
pub async fn update_token(
    id: i64,
    name: Option<&str>,
    api_token: &str,
    access_key_id: &str,
    secret_access_key: &str,
) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE tokens SET name = ?1, api_token = ?2, access_key_id = ?3, secret_access_key = ?4, updated_at = ?5
         WHERE id = ?6",
        turso::params![name, api_token, access_key_id, secret_access_key, now, id],
    ).await?;
    Ok(())
}

/// Delete token (manually cascades to buckets)
pub async fn delete_token(id: i64) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    // Delete buckets first
    conn.execute("DELETE FROM buckets WHERE token_id = ?1", turso::params![id]).await?;
    // Then delete token
    conn.execute("DELETE FROM tokens WHERE id = ?1", turso::params![id]).await?;
    Ok(())
}

// ============ Combined Config Functions ============

/// Get current configuration (combines token + bucket info)
pub async fn get_current_config() -> DbResult<Option<CurrentConfig>> {
    let conn = get_connection()?.lock().await;
    let mut rows = conn
        .query("SELECT value FROM app_state WHERE key = 'current_provider'", ())
        .await?;

    let provider = if let Some(row) = rows.next().await? {
        let value: String = row.get(0)?;
        match value.as_str() {
            "aws" => StorageProvider::Aws,
            "minio" => StorageProvider::Minio,
            "rustfs" => StorageProvider::Rustfs,
            _ => StorageProvider::R2,
        }
    } else {
        StorageProvider::R2
    };

    match provider {
        StorageProvider::R2 => {
            // Get current token ID
            let mut rows = conn
                .query("SELECT value FROM app_state WHERE key = 'current_token_id'", ())
                .await?;

            let token_id: i64 = if let Some(row) = rows.next().await? {
                let value: String = row.get(0)?;
                value.parse().map_err(|_| "Invalid token ID")?
            } else {
                return Ok(None);
            };

            // Get current bucket
            let mut rows = conn
                .query("SELECT value FROM app_state WHERE key = 'current_bucket'", ())
                .await?;

            let bucket_name: String = if let Some(row) = rows.next().await? {
                row.get(0)?
            } else {
                return Ok(None);
            };

            // Get token with account info
            let mut rows = conn
                .query(
                    "SELECT t.id, t.account_id, t.name, t.api_token, t.access_key_id, t.secret_access_key,
                            a.name as account_name
                     FROM tokens t
                     JOIN accounts a ON t.account_id = a.id
                     WHERE t.id = ?1",
                    turso::params![token_id],
                )
                .await?;

            if let Some(row) = rows.next().await? {
                // Get bucket's public domain
                let mut bucket_rows = conn
                    .query(
                        "SELECT public_domain, public_domain_scheme FROM buckets WHERE token_id = ?1 AND name = ?2",
                        turso::params![token_id, bucket_name.clone()],
                    )
                    .await?;
                let (public_domain, public_domain_scheme): (Option<String>, Option<String>) =
                    if let Some(bucket_row) = bucket_rows.next().await? {
                        (bucket_row.get(0)?, bucket_row.get(1)?)
                    } else {
                        (None, None)
                    };

                Ok(Some(CurrentConfig {
                    provider: StorageProvider::R2,
                    account_id: row.get(1)?,
                    account_name: row.get(6)?,
                    token_id: Some(row.get(0)?),
                    token_name: row.get(2)?,
                    api_token: Some(row.get(3)?),
                    access_key_id: row.get(4)?,
                    secret_access_key: row.get(5)?,
                    bucket: bucket_name,
                    public_domain,
                    public_domain_scheme,
                    region: None,
                    endpoint_scheme: None,
                    endpoint_host: None,
                    force_path_style: None,
                }))
            } else {
                Ok(None)
            }
        }
        StorageProvider::Aws => {
            let mut rows = conn
                .query("SELECT value FROM app_state WHERE key = 'current_aws_account_id'", ())
                .await?;
            let account_id: String = if let Some(row) = rows.next().await? {
                row.get(0)?
            } else {
                return Ok(None);
            };

            let mut rows = conn
                .query("SELECT value FROM app_state WHERE key = 'current_aws_bucket'", ())
                .await?;
            let bucket_name: String = if let Some(row) = rows.next().await? {
                row.get(0)?
            } else {
                return Ok(None);
            };

            let mut rows = conn
                .query(
                    "SELECT id, name, access_key_id, secret_access_key, region, endpoint_scheme, endpoint_host, force_path_style
                     FROM aws_accounts WHERE id = ?1",
                    turso::params![account_id.as_str()],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                let force_value: i64 = row.get(7)?;

                let mut bucket_rows = conn
                    .query(
                        "SELECT public_domain_scheme, public_domain_host FROM aws_buckets WHERE account_id = ?1 AND name = ?2",
                        turso::params![account_id.as_str(), bucket_name.as_str()],
                    )
                    .await?;
                let (public_domain_scheme, public_domain_host): (Option<String>, Option<String>) =
                    if let Some(bucket_row) = bucket_rows.next().await? {
                        (bucket_row.get(0)?, bucket_row.get(1)?)
                    } else {
                        (None, None)
                    };

                Ok(Some(CurrentConfig {
                    provider: StorageProvider::Aws,
                    account_id: row.get(0)?,
                    account_name: row.get(1)?,
                    token_id: None,
                    token_name: None,
                    api_token: None,
                    access_key_id: row.get(2)?,
                    secret_access_key: row.get(3)?,
                    bucket: bucket_name,
                    public_domain: public_domain_host,
                    public_domain_scheme,
                    region: Some(row.get(4)?),
                    endpoint_scheme: Some(row.get(5)?),
                    endpoint_host: row.get(6)?,
                    force_path_style: Some(force_value != 0),
                }))
            } else {
                Ok(None)
            }
        }
        StorageProvider::Minio => {
            let mut rows = conn
                .query("SELECT value FROM app_state WHERE key = 'current_minio_account_id'", ())
                .await?;
            let account_id: String = if let Some(row) = rows.next().await? {
                row.get(0)?
            } else {
                return Ok(None);
            };

            let mut rows = conn
                .query("SELECT value FROM app_state WHERE key = 'current_minio_bucket'", ())
                .await?;
            let bucket_name: String = if let Some(row) = rows.next().await? {
                row.get(0)?
            } else {
                return Ok(None);
            };

            let mut rows = conn
                .query(
                    "SELECT id, name, access_key_id, secret_access_key, endpoint_scheme, endpoint_host, force_path_style
                     FROM minio_accounts WHERE id = ?1",
                    turso::params![account_id.as_str()],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                let force_value: i64 = row.get(6)?;

                let mut bucket_rows = conn
                    .query(
                        "SELECT public_domain_scheme, public_domain_host FROM minio_buckets WHERE account_id = ?1 AND name = ?2",
                        turso::params![account_id.as_str(), bucket_name.as_str()],
                    )
                    .await?;
                let (public_domain_scheme, public_domain_host): (Option<String>, Option<String>) =
                    if let Some(bucket_row) = bucket_rows.next().await? {
                        (bucket_row.get(0)?, bucket_row.get(1)?)
                    } else {
                        (None, None)
                    };

                Ok(Some(CurrentConfig {
                    provider: StorageProvider::Minio,
                    account_id: row.get(0)?,
                    account_name: row.get(1)?,
                    token_id: None,
                    token_name: None,
                    api_token: None,
                    access_key_id: row.get(2)?,
                    secret_access_key: row.get(3)?,
                    bucket: bucket_name,
                    public_domain: public_domain_host,
                    public_domain_scheme,
                    region: None,
                    endpoint_scheme: Some(row.get(4)?),
                    endpoint_host: Some(row.get(5)?),
                    force_path_style: Some(force_value != 0),
                }))
            } else {
                Ok(None)
            }
        }
        StorageProvider::Rustfs => {
            let mut rows = conn
                .query("SELECT value FROM app_state WHERE key = 'current_rustfs_account_id'", ())
                .await?;
            let account_id: String = if let Some(row) = rows.next().await? {
                row.get(0)?
            } else {
                return Ok(None);
            };

            let mut rows = conn
                .query("SELECT value FROM app_state WHERE key = 'current_rustfs_bucket'", ())
                .await?;
            let bucket_name: String = if let Some(row) = rows.next().await? {
                row.get(0)?
            } else {
                return Ok(None);
            };

            let mut rows = conn
                .query(
                    "SELECT id, name, access_key_id, secret_access_key, endpoint_scheme, endpoint_host, force_path_style
                     FROM rustfs_accounts WHERE id = ?1",
                    turso::params![account_id.as_str()],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                let force_value: i64 = row.get(6)?;

                let mut bucket_rows = conn
                    .query(
                        "SELECT public_domain_scheme, public_domain_host FROM rustfs_buckets WHERE account_id = ?1 AND name = ?2",
                        turso::params![account_id.as_str(), bucket_name.as_str()],
                    )
                    .await?;
                let (public_domain_scheme, public_domain_host): (Option<String>, Option<String>) =
                    if let Some(bucket_row) = bucket_rows.next().await? {
                        (bucket_row.get(0)?, bucket_row.get(1)?)
                    } else {
                        (None, None)
                    };

                Ok(Some(CurrentConfig {
                    provider: StorageProvider::Rustfs,
                    account_id: row.get(0)?,
                    account_name: row.get(1)?,
                    token_id: None,
                    token_name: None,
                    api_token: None,
                    access_key_id: row.get(2)?,
                    secret_access_key: row.get(3)?,
                    bucket: bucket_name,
                    public_domain: public_domain_host,
                    public_domain_scheme,
                    region: None,
                    endpoint_scheme: Some(row.get(4)?),
                    endpoint_host: Some(row.get(5)?),
                    force_path_style: Some(force_value != 0),
                }))
            } else {
                Ok(None)
            }
        }
    }
}

/// Set current token and bucket
pub async fn set_current_selection(token_id: i64, bucket_name: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_provider', 'r2')
         ON CONFLICT (key) DO UPDATE SET value = 'r2'",
        (),
    ).await?;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_token_id', ?1)
         ON CONFLICT (key) DO UPDATE SET value = ?1",
        turso::params![token_id.to_string()],
    ).await?;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_bucket', ?1)
         ON CONFLICT (key) DO UPDATE SET value = ?1",
        turso::params![bucket_name],
    ).await?;
    Ok(())
}

pub async fn set_current_aws_selection(account_id: &str, bucket_name: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_provider', 'aws')
         ON CONFLICT (key) DO UPDATE SET value = 'aws'",
        (),
    ).await?;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_aws_account_id', ?1)
         ON CONFLICT (key) DO UPDATE SET value = ?1",
        turso::params![account_id],
    ).await?;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_aws_bucket', ?1)
         ON CONFLICT (key) DO UPDATE SET value = ?1",
        turso::params![bucket_name],
    ).await?;
    Ok(())
}

pub async fn set_current_minio_selection(account_id: &str, bucket_name: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_provider', 'minio')
         ON CONFLICT (key) DO UPDATE SET value = 'minio'",
        (),
    ).await?;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_minio_account_id', ?1)
         ON CONFLICT (key) DO UPDATE SET value = ?1",
        turso::params![account_id],
    ).await?;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_minio_bucket', ?1)
         ON CONFLICT (key) DO UPDATE SET value = ?1",
        turso::params![bucket_name],
    ).await?;
    Ok(())
}

pub async fn set_current_rustfs_selection(account_id: &str, bucket_name: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_provider', 'rustfs')
         ON CONFLICT (key) DO UPDATE SET value = 'rustfs'",
        (),
    ).await?;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_rustfs_account_id', ?1)
         ON CONFLICT (key) DO UPDATE SET value = ?1",
        turso::params![account_id],
    ).await?;
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('current_rustfs_bucket', ?1)
         ON CONFLICT (key) DO UPDATE SET value = ?1",
        turso::params![bucket_name],
    ).await?;
    Ok(())
}
