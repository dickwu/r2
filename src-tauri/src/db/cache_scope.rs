//! Cache provenance for an immutable storage configuration and account revision.
//! An operation captures once, before networking. Every DB lock checks that
//! receipt, so account edits fence old requests without holding a lock over I/O.
use super::{get_connection, DbResult};
use sha2::{Digest, Sha256};
use std::future::Future;
use tokio::sync::MutexGuard;
use turso::Connection;

#[derive(Clone)]
pub struct CacheConfig {
    pub provider: String,
    pub account_id: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: Option<String>,
    pub endpoint_scheme: Option<String>,
    pub endpoint_host: Option<String>,
    pub force_path_style: bool,
}

impl CacheConfig {
    pub fn from_current(config: &super::tokens::CurrentConfig) -> Self {
        let provider = match config.provider {
            super::tokens::StorageProvider::R2 => "r2",
            super::tokens::StorageProvider::Aws => "aws",
            super::tokens::StorageProvider::Minio => "minio",
            super::tokens::StorageProvider::Rustfs => "rustfs",
        };
        Self {
            provider: provider.into(),
            account_id: config.account_id.clone(),
            access_key_id: config.access_key_id.clone(),
            secret_access_key: config.secret_access_key.clone(),
            region: config.region.clone(),
            endpoint_scheme: config.endpoint_scheme.clone(),
            endpoint_host: config.endpoint_host.clone(),
            force_path_style: config.force_path_style.unwrap_or(provider != "aws"),
        }
    }

    /// Hash only effective S3 connection fields. Labels/public URLs are not a
    /// storage namespace. Length-prefixed serialization avoids delimiter aliases.
    pub fn fingerprint(&self) -> DbResult<String> {
        let (endpoint, region, path_style) = match self.provider.as_str() {
            "r2" => (
                format!("https://{}.r2.cloudflarestorage.com", self.account_id),
                "auto".to_string(),
                true,
            ),
            "aws" => (
                self.endpoint_host
                    .as_deref()
                    .filter(|host| !host.trim().is_empty())
                    .map(|host| {
                        normalize_endpoint(self.endpoint_scheme.as_deref().unwrap_or("https"), host)
                    })
                    .transpose()?
                    .unwrap_or_default(),
                self.region.clone().unwrap_or_else(|| "us-east-1".into()),
                self.force_path_style,
            ),
            "minio" | "rustfs" => (
                normalize_endpoint(
                    self.endpoint_scheme.as_deref().unwrap_or("http"),
                    self.endpoint_host.as_deref().unwrap_or_default(),
                )?,
                "us-east-1".to_string(),
                self.force_path_style,
            ),
            _ => return Err("Unknown storage provider for cache scope".into()),
        };
        let serialized = serde_json::to_vec(&(
            "cache-scope-v1",
            &self.provider,
            &self.account_id,
            endpoint,
            region,
            path_style,
            &self.access_key_id,
            &self.secret_access_key,
        ))?;
        Ok(format!("{:x}", Sha256::digest(serialized)))
    }
}

fn normalize_endpoint(scheme: &str, host: &str) -> DbResult<String> {
    let url = reqwest::Url::parse(&format!("{}://{}", scheme.trim(), host.trim()))
        .map_err(|_| "Invalid cache endpoint")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Invalid cache endpoint".into());
    }
    Ok(url.to_string().trim_end_matches('/').to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheScope {
    pub provider: String,
    pub account_id: String,
    pub fingerprint: String,
    pub revision: i64,
}

tokio::task_local! { static ACTIVE_SCOPE: CacheScope; }

pub async fn in_scope<F: Future>(scope: CacheScope, future: F) -> F::Output {
    ACTIVE_SCOPE.scope(scope, future).await
}

pub fn current_scope() -> Option<CacheScope> {
    ACTIVE_SCOPE.try_with(Clone::clone).ok()
}

impl CacheScope {
    pub async fn capture(config: &CacheConfig) -> DbResult<Self> {
        let conn = get_connection()?.lock().await;
        capture_on(&conn, config).await
    }
}

pub fn get_table_sql() -> &'static str {
    "CREATE TABLE IF NOT EXISTS cache_scopes (
        account_id TEXT PRIMARY KEY, provider TEXT NOT NULL,
        fingerprint TEXT NOT NULL, revision INTEGER NOT NULL
    );"
}

pub async fn prepare_cache_schema_on(conn: &Connection) -> DbResult<()> {
    for (table, required) in [
        ("cached_files", "parent_path"),
        ("directory_tree", "parent_path"),
    ] {
        let mut rows = conn
            .query(&format!("PRAGMA table_info({table})"), ())
            .await?;
        let mut names = Vec::new();
        while let Some(row) = rows.next().await? {
            names.push(row.get::<String>(1)?);
        }
        if !names.is_empty() && !names.iter().any(|name| name == required) {
            // Only disposable historical cache schemas are recreated. Current
            // schemas and all account/upload/user data are preserved.
            conn.execute(&format!("DROP TABLE {table}"), ()).await?;
            conn.execute("DELETE FROM app_state WHERE key='cache_scope_schema'", ())
                .await?;
        }
    }
    Ok(())
}

pub async fn initialize_on(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(get_table_sql()).await?;
    let mut rows = conn
        .query(
            "SELECT value FROM app_state WHERE key = 'cache_scope_schema'",
            (),
        )
        .await?;
    if rows
        .next()
        .await?
        .is_some_and(|row| row.get::<String>(0).ok().as_deref() == Some("1"))
    {
        return Ok(());
    }
    drop(rows);
    conn.execute("BEGIN TRANSACTION", ()).await?;
    let result = async {
        // Pre-migration rows have no provable namespace; invalidate both data
        // and their completeness markers, once. Future restarts preserve them.
        for table in [
            "cached_files",
            "cached_files_staging",
            "directory_tree",
            "prefix_sync_times",
            "sync_meta",
            "cache_scopes",
        ] {
            conn.execute(&format!("DELETE FROM {table}"), ()).await?;
        }
        conn.execute(
            "DELETE FROM app_state WHERE substr(key,1,17) = 'skipped_prefixes:'",
            (),
        )
        .await?;
        conn.execute(
            "INSERT INTO app_state (key,value) VALUES ('cache_scope_schema','1') ON CONFLICT(key) DO UPDATE SET value='1'",
            (),
        )
        .await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    }
    .await;
    finish_transaction(conn, result).await
}

pub async fn finish_transaction(conn: &Connection, result: DbResult<()>) -> DbResult<()> {
    match result {
        Ok(()) => {
            conn.execute("COMMIT", ()).await?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
}

async fn stored_scope_on(conn: &Connection, account_id: &str) -> DbResult<Option<CacheScope>> {
    let mut rows = conn
        .query(
            "SELECT provider,fingerprint,revision FROM cache_scopes WHERE account_id=?1",
            turso::params![account_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(CacheScope {
            provider: row.get(0)?,
            account_id: account_id.into(),
            fingerprint: row.get(1)?,
            revision: row.get(2)?,
        })),
        None => Ok(None),
    }
}

async fn saved_fingerprints_on(
    conn: &Connection,
    provider: &str,
    account_id: &str,
) -> DbResult<Vec<String>> {
    let sql = match provider {
        "aws" => {
            "SELECT access_key_id,secret_access_key,region,endpoint_scheme,endpoint_host,force_path_style FROM aws_accounts WHERE id=?1"
        }
        "minio" => {
            "SELECT access_key_id,secret_access_key,'us-east-1',endpoint_scheme,endpoint_host,force_path_style FROM minio_accounts WHERE id=?1"
        }
        "rustfs" => {
            "SELECT access_key_id,secret_access_key,'us-east-1',endpoint_scheme,endpoint_host,force_path_style FROM rustfs_accounts WHERE id=?1"
        }
        "r2" => {
            "SELECT access_key_id,secret_access_key,'auto',NULL,NULL,1 FROM tokens WHERE account_id=?1"
        }
        _ => return Err("Unknown saved storage provider".into()),
    };
    let mut rows = conn.query(sql, turso::params![account_id]).await?;
    let mut fingerprints = Vec::new();
    while let Some(row) = rows.next().await? {
        fingerprints.push(
            CacheConfig {
                provider: provider.into(),
                account_id: account_id.into(),
                access_key_id: row.get(0)?,
                secret_access_key: row.get(1)?,
                region: row.get(2)?,
                endpoint_scheme: row.get(3)?,
                endpoint_host: row.get(4)?,
                force_path_style: row.get::<i64>(5)? != 0,
            }
            .fingerprint()?,
        );
    }
    Ok(fingerprints)
}

async fn capture_on(conn: &Connection, config: &CacheConfig) -> DbResult<CacheScope> {
    let fingerprint = config.fingerprint()?;
    if !saved_fingerprints_on(conn, &config.provider, &config.account_id)
        .await?
        .contains(&fingerprint)
    {
        return Err("Storage account changed; reload its current configuration".into());
    }
    if let Some(existing) = stored_scope_on(conn, &config.account_id).await? {
        if existing.provider == config.provider && existing.fingerprint == fingerprint {
            return Ok(existing);
        }
    }
    conn.execute("BEGIN TRANSACTION", ()).await?;
    let result = async {
        invalidate_account_on(conn, &config.account_id).await?;
        conn.execute("INSERT INTO cache_scopes(account_id,provider,fingerprint,revision) VALUES (?1,?2,?3,1)
            ON CONFLICT(account_id) DO UPDATE SET provider=excluded.provider,fingerprint=excluded.fingerprint,revision=cache_scopes.revision+1",
            turso::params![config.account_id.as_str(), config.provider.as_str(), fingerprint.as_str()]).await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    }.await;
    finish_transaction(conn, result).await?;
    stored_scope_on(conn, &config.account_id)
        .await?
        .ok_or_else(|| "Missing bound cache scope".into())
}

/// Called inside the same transaction as an account/token update. A label-only
/// edit keeps the cache warm; effective connection changes advance the fence.
pub async fn account_updated_on(
    conn: &Connection,
    provider: &str,
    account_id: &str,
) -> DbResult<()> {
    let fingerprints = saved_fingerprints_on(conn, provider, account_id).await?;
    if let Some(existing) = stored_scope_on(conn, account_id).await? {
        if existing.provider == provider && fingerprints.contains(&existing.fingerprint) {
            return Ok(());
        }
    }
    invalidate_account_on(conn, account_id).await?;
    conn.execute("INSERT INTO cache_scopes(account_id,provider,fingerprint,revision) VALUES (?1,?2,'',1)
        ON CONFLICT(account_id) DO UPDATE SET provider=excluded.provider,fingerprint='',revision=cache_scopes.revision+1",
        turso::params![account_id, provider]).await?;
    Ok(())
}

pub async fn invalidate_account_on(conn: &Connection, account_id: &str) -> DbResult<()> {
    for table in [
        "cached_files",
        "cached_files_staging",
        "directory_tree",
        "prefix_sync_times",
        "sync_meta",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE account_id=?1"),
            turso::params![account_id],
        )
        .await?;
    }
    let prefix = format!("skipped_prefixes:{account_id}:");
    conn.execute(
        "DELETE FROM app_state WHERE substr(key,1,?1)=?2",
        turso::params![prefix.len() as i64, prefix],
    )
    .await?;
    // A listing still in flight for this account must not publish as fresh.
    super::prefix_sync::advance_all_mutation_generations_on(conn, account_id, None).await?;
    Ok(())
}

async fn invalidate_unscoped_on(conn: &Connection, account_id: &str) -> DbResult<()> {
    conn.execute("BEGIN TRANSACTION", ()).await?;
    let result = invalidate_account_on(conn, account_id).await;
    finish_transaction(conn, result).await
}

pub async fn invalidate_unscoped(account_id: &str) -> DbResult<()> {
    let conn = get_connection()?.lock().await;
    invalidate_unscoped_on(&conn, account_id).await
}

pub async fn validate_on(conn: &Connection, scope: &CacheScope) -> DbResult<()> {
    if stored_scope_on(conn, &scope.account_id).await?.as_ref() != Some(scope)
        || !saved_fingerprints_on(conn, &scope.provider, &scope.account_id)
            .await?
            .contains(&scope.fingerprint)
    {
        return Err("Storage cache scope changed while this operation was running".into());
    }
    Ok(())
}

async fn check_context_on(conn: &Connection, account_id: &str, mutation: bool) -> DbResult<()> {
    if let Some(scope) = current_scope() {
        if scope.account_id != account_id {
            return Err("Cache account does not match operation scope".into());
        }
        return validate_on(conn, &scope).await;
    }
    if mutation {
        // Older upload/move notifications carry only account_id. Their delayed
        // content cannot be attributed to the current endpoint. Invalidate
        // instead of publishing potentially foreign file rows.
        invalidate_unscoped_on(conn, account_id).await?;
        return Err("Cache origin unavailable; invalidated cache for a scoped refresh".into());
    }
    Err("Cache reads require the current storage configuration".into())
}

pub async fn read_connection(account_id: &str) -> DbResult<MutexGuard<'static, Connection>> {
    let conn = get_connection()?.lock().await;
    check_context_on(&conn, account_id, false).await?;
    Ok(conn)
}

pub async fn write_connection(account_id: &str) -> DbResult<MutexGuard<'static, Connection>> {
    let conn = get_connection()?.lock().await;
    check_context_on(&conn, account_id, true).await?;
    Ok(conn)
}

pub async fn clear_connection(account_id: &str) -> DbResult<MutexGuard<'static, Connection>> {
    let conn = get_connection()?.lock().await;
    if current_scope().is_some() {
        check_context_on(&conn, account_id, false).await?;
    }
    Ok(conn)
}

pub async fn validate_app_state_on(conn: &Connection, key: &str, mutation: bool) -> DbResult<()> {
    if let Some(suffix) = key.strip_prefix("skipped_prefixes:") {
        let account_id = suffix
            .split_once(':')
            .ok_or("Invalid skipped-prefix cache key")?
            .0;
        check_context_on(conn, account_id, mutation).await?;
    }
    Ok(())
}

#[allow(dead_code)]
pub struct PrefixSnapshot {
    pub prefix_time: Option<i64>,
    pub full_sync: bool,
    pub skipped_prefixes: Option<Vec<String>>,
    pub contents: super::file_cache::FolderContents,
}

#[allow(dead_code)]
pub struct PrefixPageSnapshot {
    pub prefix_time: Option<i64>,
    pub full_time: Option<i64>,
    /// When the rows were last known current. None once the folder changed
    /// after it was listed: neither its marker nor the full index vouch then.
    pub freshness_time: Option<i64>,
    pub full_sync: bool,
    pub snapshot_token: Option<String>,
    pub skipped_prefixes: Option<Vec<String>>,
    pub page: super::file_cache::CachedFolderPage,
}

/// Capture the folder's mutation generation before a listing's first request.
pub async fn capture_prefix_generation(
    scope: &CacheScope,
    bucket: &str,
    prefix: &str,
) -> DbResult<i64> {
    let conn = get_connection()?.lock().await;
    validate_on(&conn, scope).await?;
    super::prefix_sync::capture_mutation_generation_on(&conn, bucket, &scope.account_id, prefix)
        .await
}

#[allow(dead_code)]
pub async fn read_prefix_snapshot(
    scope: &CacheScope,
    bucket: &str,
    prefix: &str,
) -> DbResult<PrefixSnapshot> {
    let conn = get_connection()?.lock().await;
    read_prefix_snapshot_on(&conn, scope, bucket, prefix).await
}

#[allow(dead_code)]
async fn read_prefix_snapshot_on(
    conn: &Connection,
    scope: &CacheScope,
    bucket: &str,
    prefix: &str,
) -> DbResult<PrefixSnapshot> {
    validate_on(conn, scope).await?;
    let mut rows = conn.query("SELECT last_synced_at FROM prefix_sync_times WHERE bucket=?1 AND account_id=?2 AND prefix=?3", turso::params![bucket, scope.account_id.as_str(), prefix]).await?;
    let prefix_time = rows.next().await?.map(|row| row.get(0)).transpose()?;
    let mut rows = conn
        .query(
            "SELECT file_count FROM sync_meta WHERE bucket=?1 AND account_id=?2",
            turso::params![bucket, scope.account_id.as_str()],
        )
        .await?;
    let full_sync = rows.next().await?.is_some();
    let mut rows = conn
        .query(
            "SELECT value FROM app_state WHERE key=?1",
            turso::params![format!("skipped_prefixes:{}:{bucket}", scope.account_id)],
        )
        .await?;
    let skipped_prefixes = match rows.next().await? {
        None => Some(Vec::new()),
        Some(row) => serde_json::from_str(&row.get::<String>(0)?).ok(),
    };
    let contents =
        super::file_cache::folder_contents_on(conn, bucket, &scope.account_id, prefix).await?;
    Ok(PrefixSnapshot {
        prefix_time,
        full_sync,
        skipped_prefixes,
        contents,
    })
}

pub async fn read_prefix_page(
    scope: &CacheScope,
    bucket: &str,
    prefix: &str,
    cursor: Option<&str>,
    page_size: usize,
) -> DbResult<PrefixPageSnapshot> {
    let conn = get_connection()?.lock().await;
    conn.execute("BEGIN TRANSACTION", ()).await?;
    let result = read_prefix_page_on(&conn, scope, bucket, prefix, cursor, page_size).await;
    match result {
        Ok(value) => {
            conn.execute("COMMIT", ()).await?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
}

async fn read_prefix_page_on(
    conn: &Connection,
    scope: &CacheScope,
    bucket: &str,
    prefix: &str,
    cursor: Option<&str>,
    page_size: usize,
) -> DbResult<PrefixPageSnapshot> {
    validate_on(conn, scope).await?;
    super::file_cache::ensure_no_local_cache_mutation_on(conn, bucket, &scope.account_id).await?;
    let mut rows = conn.query("SELECT last_synced_at,generation FROM prefix_sync_times WHERE bucket=?1 AND account_id=?2 AND prefix=?3", turso::params![bucket, scope.account_id.as_str(), prefix]).await?;
    let prefix_row = match rows.next().await? {
        Some(row) => Some((row.get::<i64>(0)?, row.get::<i64>(1)?)),
        None => None,
    };
    let prefix_marker = prefix_row.filter(|(time, _)| *time > 0);
    // A kept zero marker: the folder changed locally after it was listed, or
    // its last listing overlapped such a change.
    let prefix_changed = prefix_row.is_some() && prefix_marker.is_none();
    let prefix_time = prefix_marker.map(|(time, _)| time);
    let mut rows = conn
        .query(
            "SELECT last_sync,generation FROM sync_meta WHERE bucket=?1 AND account_id=?2",
            turso::params![bucket, scope.account_id.as_str()],
        )
        .await?;
    let full_marker = match rows.next().await? {
        Some(row) => Some((row.get::<i64>(0)?, row.get::<i64>(1)?)),
        None => None,
    };
    let full_time = full_marker.map(|(time, _)| time);
    let freshness_time = if prefix_changed {
        None
    } else {
        prefix_time.or(full_time)
    };
    let full_sync = full_marker.is_some();
    let content_revision =
        super::file_cache::content_revision_on(conn, bucket, &scope.account_id).await?;
    let snapshot_token = prefix_marker
        .map(|(time, generation)| format!("prefix:{time}:{generation}:{content_revision}"))
        .or_else(|| {
            full_marker
                .map(|(time, generation)| format!("full:{time}:{generation}:{content_revision}"))
        });
    let mut rows = conn
        .query(
            "SELECT value FROM app_state WHERE key=?1",
            turso::params![format!("skipped_prefixes:{}:{bucket}", scope.account_id)],
        )
        .await?;
    let skipped_prefixes = match rows.next().await? {
        None => Some(Vec::new()),
        Some(row) => serde_json::from_str(&row.get::<String>(0)?).ok(),
    };
    let page = super::file_cache::cached_folder_page_on(
        conn,
        bucket,
        &scope.account_id,
        prefix,
        cursor,
        snapshot_token.clone(),
        page_size,
    )
    .await?;
    Ok(PrefixPageSnapshot {
        prefix_time,
        full_time,
        freshness_time,
        full_sync,
        snapshot_token,
        skipped_prefixes,
        page,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::{oneshot, Mutex};

    fn config(host: &str) -> CacheConfig {
        CacheConfig {
            provider: "minio".into(),
            account_id: "account".into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
            region: None,
            endpoint_scheme: Some("https".into()),
            endpoint_host: Some(host.into()),
            force_path_style: true,
        }
    }

    async fn schema(conn: &Connection) {
        conn.execute_batch(super::super::app_state::get_table_sql())
            .await
            .unwrap();
        prepare_cache_schema_on(conn).await.unwrap();
        conn.execute_batch(super::super::file_cache::get_table_sql())
            .await
            .unwrap();
        conn.execute_batch(super::super::prefix_sync::get_table_sql())
            .await
            .unwrap();
        conn.execute_batch(super::super::minio_accounts::get_table_sql())
            .await
            .unwrap();
        initialize_on(conn).await.unwrap();
    }

    async fn fixture() -> (turso::Database, Connection) {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        schema(&conn).await;
        conn.execute("INSERT INTO minio_accounts(id,access_key_id,secret_access_key,endpoint_scheme,endpoint_host,force_path_style,created_at,updated_at) VALUES ('account','key','secret','https','a.example',1,0,0)", ()).await.unwrap();
        (db, conn)
    }

    fn file(key: &str) -> super::super::CachedFile {
        super::super::CachedFile {
            bucket: "bucket".into(),
            account_id: "account".into(),
            key: key.into(),
            parent_path: "".into(),
            name: key.into(),
            size: 1,
            last_modified: "".into(),
            synced_at: 1,
        }
    }

    /// Publish a complete root listing whose generation was captured first.
    async fn list_root(conn: &Connection, files: &[super::super::CachedFile]) -> DbResult<()> {
        let generation = super::super::prefix_sync::capture_mutation_generation_on(
            conn, "bucket", "account", "",
        )
        .await?;
        super::super::prefix_sync::replace_complete_prefix_on(
            conn,
            "bucket",
            "account",
            "",
            files,
            &[],
            generation,
        )
        .await
        .map(|_| ())
    }

    async fn publish_on(conn: &Connection, scope: CacheScope, key: &str) -> DbResult<()> {
        in_scope(scope, async {
            check_context_on(conn, "account", true).await?;
            list_root(conn, &[file(key)]).await
        })
        .await
    }

    async fn pin_prefix_time(conn: &Connection, time: i64) {
        conn.execute(
            "UPDATE prefix_sync_times SET last_synced_at = ?1 WHERE bucket = 'bucket' AND account_id = 'account' AND prefix = ''",
            turso::params![time],
        )
        .await
        .unwrap();
    }

    #[test]
    fn effective_config_fingerprints_are_normalized_and_do_not_reveal_credentials() {
        let a = config("EXAMPLE.test:443/");
        assert_eq!(
            a.fingerprint().unwrap(),
            config("example.test").fingerprint().unwrap()
        );
        let mut changed = a.clone();
        changed.secret_access_key = "rotated".into();
        assert_ne!(a.fingerprint().unwrap(), changed.fingerprint().unwrap());
        let hash = a.fingerprint().unwrap();
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains("secret"));
        let mut aws = a;
        aws.provider = "aws".into();
        aws.region = Some("us-east-1".into());
        changed = aws.clone();
        changed.region = Some("us-west-2".into());
        assert_ne!(aws.fingerprint().unwrap(), changed.fingerprint().unwrap());
    }

    #[tokio::test]
    async fn prefix_page_cursor_rejects_same_second_replacement_generation() {
        let (_db, conn) = fixture().await;
        let scope = capture_on(&conn, &config("a.example")).await.unwrap();
        in_scope(scope.clone(), async {
            list_root(&conn, &[file("a.txt"), file("b.txt")]).await
        })
        .await
        .unwrap();
        pin_prefix_time(&conn, 123).await;
        let first = read_prefix_page_on(&conn, &scope, "bucket", "", None, 1)
            .await
            .unwrap();
        let cursor = first.page.next_cursor.clone().unwrap();
        assert_eq!(first.snapshot_token.as_deref(), Some("prefix:123:1:1"));

        in_scope(scope.clone(), async {
            list_root(&conn, &[file("c.txt"), file("d.txt")]).await
        })
        .await
        .unwrap();
        pin_prefix_time(&conn, 123).await;
        let replaced = read_prefix_page_on(&conn, &scope, "bucket", "", Some(&cursor), 1).await;
        assert!(replaced.is_err());
    }

    #[tokio::test]
    async fn prefix_page_rejects_in_progress_local_mutation_barrier() {
        let (_db, conn) = fixture().await;
        let scope = capture_on(&conn, &config("a.example")).await.unwrap();
        in_scope(scope.clone(), async {
            list_root(&conn, &[file("a.txt"), file("b.txt")]).await
        })
        .await
        .unwrap();
        let first = read_prefix_page_on(&conn, &scope, "bucket", "", None, 1)
            .await
            .unwrap();
        let old_cursor = first.page.next_cursor.clone().unwrap();

        conn.execute(
            "INSERT INTO app_state(key,value) VALUES ('cache_mutation_in_progress:account:bucket',?1)",
            turso::params![serde_json::json!([
                {"token":"a","started_at": chrono::Utc::now().timestamp()},
                {"token":"b","started_at": chrono::Utc::now().timestamp()}
            ]).to_string()],
        )
        .await
        .unwrap();
        conn.execute(
            "UPDATE cached_files SET size = 99 WHERE bucket = 'bucket' AND account_id = 'account' AND key = 'b.txt'",
            (),
        )
        .await
        .unwrap();
        let blocked = read_prefix_page_on(&conn, &scope, "bucket", "", Some(&old_cursor), 10).await;
        assert!(blocked.is_err());

        conn.execute(
            "UPDATE app_state SET value = ?1 WHERE key = 'cache_mutation_in_progress:account:bucket'",
            turso::params![serde_json::json!([
                {"token":"b","started_at": chrono::Utc::now().timestamp()}
            ]).to_string()],
        )
        .await
        .unwrap();
        let still_blocked =
            read_prefix_page_on(&conn, &scope, "bucket", "", Some(&old_cursor), 10).await;
        assert!(still_blocked.is_err());

        conn.execute(
            "DELETE FROM app_state WHERE key = 'cache_mutation_in_progress:account:bucket'",
            (),
        )
        .await
        .unwrap();
        super::super::file_cache::bump_content_revision_on(&conn, "bucket", "account")
            .await
            .unwrap();
        let stale_cursor =
            read_prefix_page_on(&conn, &scope, "bucket", "", Some(&old_cursor), 10).await;
        assert!(stale_cursor.is_err());
    }

    #[tokio::test]
    async fn stale_local_mutation_barrier_invalidates_markers_and_recovers_reads() {
        let (_db, conn) = fixture().await;
        let scope = capture_on(&conn, &config("a.example")).await.unwrap();
        in_scope(scope.clone(), async {
            list_root(&conn, &[file("a.txt")]).await
        })
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO sync_meta(bucket,account_id,last_sync,file_count,generation) VALUES ('bucket','account',123,1,1)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO app_state(key,value) VALUES ('cache_mutation_in_progress:account:bucket',?1)",
            turso::params![serde_json::json!([
                {"token":"stale","started_at": 1}
            ]).to_string()],
        )
        .await
        .unwrap();

        let recovered = read_prefix_page_on(&conn, &scope, "bucket", "", None, 10)
            .await
            .unwrap();
        assert!(recovered.prefix_time.is_none());
        assert!(recovered.full_time.is_none());
        assert!(recovered.snapshot_token.is_none());

        let mut rows = conn
            .query(
                "SELECT 1 FROM app_state WHERE key = 'cache_mutation_in_progress:account:bucket'",
                (),
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_none());
        let mut rows = conn
            .query(
                "SELECT 1 FROM sync_meta WHERE bucket = 'bucket' AND account_id = 'account'",
                (),
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn prefix_cursor_rejects_atomic_full_sync_swap() {
        let (_db, conn) = fixture().await;
        let scope = capture_on(&conn, &config("a.example")).await.unwrap();
        in_scope(scope.clone(), async {
            list_root(&conn, &[file("a.txt"), file("b.txt")]).await
        })
        .await
        .unwrap();
        pin_prefix_time(&conn, 123).await;
        let first = read_prefix_page_on(&conn, &scope, "bucket", "", None, 1)
            .await
            .unwrap();
        let old_cursor = first.page.next_cursor.clone().unwrap();
        assert_eq!(first.snapshot_token.as_deref(), Some("prefix:123:1:1"));

        conn.execute("BEGIN TRANSACTION", ()).await.unwrap();
        conn.execute(
            "DELETE FROM cached_files WHERE bucket = 'bucket' AND account_id = 'account'",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO cached_files(bucket,account_id,key,parent_path,name,size,last_modified,synced_at)
             VALUES ('bucket','account','c.txt','','c.txt',1,'',1),
                    ('bucket','account','d.txt','','d.txt',1,'',1)",
            (),
        )
        .await
        .unwrap();
        super::super::dir_tree::rebuild_directory_tree_on(&conn, "bucket", "account", &[])
            .await
            .unwrap();
        conn.execute(
            "DELETE FROM prefix_sync_times WHERE bucket = 'bucket' AND account_id = 'account'",
            (),
        )
        .await
        .unwrap();
        super::super::file_cache::bump_content_revision_on(&conn, "bucket", "account")
            .await
            .unwrap();
        conn.execute(
            "INSERT INTO sync_meta(bucket,account_id,last_sync,file_count,generation)
             VALUES ('bucket','account',456,2,1)",
            (),
        )
        .await
        .unwrap();
        conn.execute("COMMIT", ()).await.unwrap();

        let old_page = read_prefix_page_on(&conn, &scope, "bucket", "", Some(&old_cursor), 1).await;
        assert!(old_page.is_err());

        let full = read_prefix_page_on(&conn, &scope, "bucket", "", None, 1)
            .await
            .unwrap();
        assert_eq!(full.prefix_time, None);
        assert_eq!(full.full_time, Some(456));
        assert_eq!(full.snapshot_token.as_deref(), Some("full:456:1:2"));
        let mut keys = full
            .page
            .files
            .iter()
            .map(|file| file.key.clone())
            .collect::<Vec<_>>();
        if let Some(cursor) = full.page.next_cursor.as_deref() {
            let second = read_prefix_page_on(&conn, &scope, "bucket", "", Some(cursor), 10)
                .await
                .unwrap();
            keys.extend(second.page.files.iter().map(|file| file.key.clone()));
        }
        assert!(keys.iter().any(|key| key == "c.txt"));
        assert!(!keys.iter().any(|key| key == "a.txt"));
    }

    #[tokio::test]
    async fn prefix_marker_zero_falls_back_to_fresh_full_sync_snapshot() {
        let (_db, conn) = fixture().await;
        let scope = capture_on(&conn, &config("a.example")).await.unwrap();
        conn.execute(
            "INSERT INTO cached_files(bucket,account_id,key,parent_path,name,size,last_modified,synced_at)
             VALUES ('bucket','account','a.txt','','a.txt',1,'',1),
                    ('bucket','account','b.txt','','b.txt',1,'',1)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO sync_meta(bucket,account_id,last_sync,file_count,generation)
             VALUES ('bucket','account',456,2,3)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO prefix_sync_times(bucket,account_id,prefix,last_synced_at,generation)
             VALUES ('bucket','account','',0,9)",
            (),
        )
        .await
        .unwrap();

        let first = read_prefix_page_on(&conn, &scope, "bucket", "", None, 1)
            .await
            .unwrap();

        assert!(first.full_sync);
        assert!(first.prefix_time.is_none());
        assert_eq!(first.full_time, Some(456));
        assert_eq!(first.snapshot_token.as_deref(), Some("full:456:3:0"));
    }

    #[tokio::test]
    async fn prefix_page_does_not_pin_full_sync_only_index() {
        let (_db, conn) = fixture().await;
        let scope = capture_on(&conn, &config("a.example")).await.unwrap();
        conn.execute(
            "INSERT INTO cached_files(bucket,account_id,key,parent_path,name,size,last_modified,synced_at)
             VALUES ('bucket','account','a.txt','','a.txt',1,'',1),
                    ('bucket','account','b.txt','','b.txt',1,'',1)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO sync_meta(bucket,account_id,last_sync,file_count,generation)
             VALUES ('bucket','account',123,2,7)",
            (),
        )
        .await
        .unwrap();

        let first = read_prefix_page_on(&conn, &scope, "bucket", "", None, 1)
            .await
            .unwrap();

        assert!(first.full_sync);
        assert!(first.prefix_time.is_none());
        assert_eq!(first.full_time, Some(123));
        assert_eq!(first.snapshot_token.as_deref(), Some("full:123:7:0"));
        assert!(first.page.next_cursor.is_some());
    }

    #[tokio::test]
    async fn endpoint_edit_fences_inflight_publish_and_preserves_new_namespace() {
        let (_db, conn) = fixture().await;
        let scope_a = capture_on(&conn, &config("a.example")).await.unwrap();
        publish_on(&conn, scope_a.clone(), "from-a").await.unwrap();
        let shared = Arc::new(Mutex::new(conn));
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let old_db = shared.clone();
        let old_scope = scope_a.clone();
        let old_request = tokio::spawn(async move {
            started_tx.send(()).unwrap();
            release_rx.await.unwrap(); // network A completes after account edit
            let conn = old_db.lock().await;
            publish_on(&conn, old_scope, "late-a").await
        });
        started_rx.await.unwrap();
        let scope_b = {
            let conn = shared.lock().await;
            super::super::minio_accounts::update_minio_account_on(
                &conn,
                "account",
                None,
                "key",
                "secret",
                "https",
                "b.example",
                true,
            )
            .await
            .unwrap();
            assert!(capture_on(&conn, &config("a.example")).await.is_err());
            assert!(read_prefix_snapshot_on(&conn, &scope_a, "bucket", "")
                .await
                .is_err());
            let scope_b = capture_on(&conn, &config("b.example")).await.unwrap();
            let empty = read_prefix_snapshot_on(&conn, &scope_b, "bucket", "")
                .await
                .unwrap();
            assert!(empty.contents.files.is_empty());
            assert!(empty.prefix_time.is_none());
            assert!(!empty.full_sync);
            publish_on(&conn, scope_b.clone(), "from-b").await.unwrap();
            scope_b
        };
        release_tx.send(()).unwrap();
        assert!(old_request.await.unwrap().is_err());
        let conn = shared.lock().await;
        let snapshot = read_prefix_snapshot_on(&conn, &scope_b, "bucket", "")
            .await
            .unwrap();
        assert_eq!(snapshot.contents.files[0].key, "from-b");
        // An old A task also cannot regain authority after A -> B -> A.
        super::super::minio_accounts::update_minio_account_on(
            &conn,
            "account",
            None,
            "key",
            "secret",
            "https",
            "a.example",
            true,
        )
        .await
        .unwrap();
        let new_a = capture_on(&conn, &config("a.example")).await.unwrap();
        assert_ne!(new_a.revision, scope_a.revision);
        assert!(publish_on(&conn, scope_a, "obsolete-a").await.is_err());
    }

    #[tokio::test]
    async fn label_edits_keep_warm_cache_and_failed_invalidation_rolls_back_account() {
        let (_db, conn) = fixture().await;
        let scope = capture_on(&conn, &config("a.example")).await.unwrap();
        publish_on(&conn, scope.clone(), "warm").await.unwrap();
        super::super::minio_accounts::update_minio_account_on(
            &conn,
            "account",
            Some("Renamed"),
            "key",
            "secret",
            "https",
            "A.EXAMPLE:443/",
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            capture_on(&conn, &config("a.example")).await.unwrap(),
            scope
        );
        assert_eq!(
            read_prefix_snapshot_on(&conn, &scope, "bucket", "")
                .await
                .unwrap()
                .contents
                .files[0]
                .key,
            "warm"
        );
        conn.execute("DROP TABLE directory_tree", ()).await.unwrap();
        assert!(super::super::minio_accounts::update_minio_account_on(
            &conn,
            "account",
            None,
            "key",
            "changed",
            "https",
            "b.example",
            true
        )
        .await
        .is_err());
        // Config update and preceding cache deletes were rolled back together.
        let mut rows = conn
            .query(
                "SELECT endpoint_host,secret_access_key FROM minio_accounts WHERE id='account'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "A.EXAMPLE:443/");
        assert_eq!(row.get::<String>(1).unwrap(), "secret");
        let mut rows = conn
            .query("SELECT key FROM cached_files", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "warm"
        );
    }

    #[tokio::test]
    async fn unknown_legacy_cache_is_invalidated_once_and_known_cache_survives_restart() {
        let path = std::env::temp_dir().join(format!(
            "r2-cache-scope-{}-{}.db",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let db = turso::Builder::new_local(path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        schema(&conn).await;
        conn.execute("INSERT INTO minio_accounts(id,access_key_id,secret_access_key,endpoint_scheme,endpoint_host,force_path_style,created_at,updated_at) VALUES ('account','key','secret','https','a.example',1,0,0)", ()).await.unwrap();
        conn.execute("DELETE FROM app_state WHERE key='cache_scope_schema'", ())
            .await
            .unwrap();
        list_root(&conn, &[file("unproven")]).await.unwrap();
        conn.execute("INSERT INTO sync_meta(bucket,account_id,last_sync,file_count) VALUES ('bucket','account',1,1)", ()).await.unwrap();
        initialize_on(&conn).await.unwrap();
        let scope = capture_on(&conn, &config("a.example")).await.unwrap();
        let snapshot = read_prefix_snapshot_on(&conn, &scope, "bucket", "")
            .await
            .unwrap();
        assert!(snapshot.contents.files.is_empty());
        assert!(snapshot.prefix_time.is_none());
        assert!(!snapshot.full_sync);
        publish_on(&conn, scope.clone(), "proven").await.unwrap();
        drop(conn);
        drop(db);
        let db = turso::Builder::new_local(path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        schema(&conn).await;
        assert_eq!(
            capture_on(&conn, &config("a.example")).await.unwrap(),
            scope
        );
        assert_eq!(
            read_prefix_snapshot_on(&conn, &scope, "bucket", "")
                .await
                .unwrap()
                .contents
                .files[0]
                .key,
            "proven"
        );
        drop(conn);
        drop(db);
        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn switching_tokens_rejects_old_and_unscoped_reads() {
        let (_db, conn) = fixture().await;
        conn.execute("CREATE TABLE accounts(id TEXT PRIMARY KEY)", ())
            .await
            .unwrap();
        conn.execute("INSERT INTO accounts(id) VALUES ('account')", ())
            .await
            .unwrap();
        conn.execute_batch(super::super::tokens::get_table_sql())
            .await
            .unwrap();
        conn.execute("INSERT INTO tokens(account_id,api_token,access_key_id,secret_access_key,created_at,updated_at) VALUES ('account','','wide','wide-secret',0,0),('account','','narrow','narrow-secret',0,0)", ()).await.unwrap();
        let mut wide = config("ignored.example");
        wide.provider = "r2".into();
        wide.access_key_id = "wide".into();
        wide.secret_access_key = "wide-secret".into();
        let scope_wide = capture_on(&conn, &wide).await.unwrap();
        publish_on(&conn, scope_wide.clone(), "wide-only")
            .await
            .unwrap();
        assert!(check_context_on(&conn, "account", false).await.is_err());
        let mut narrow = wide;
        narrow.access_key_id = "narrow".into();
        narrow.secret_access_key = "narrow-secret".into();
        let scope_narrow = capture_on(&conn, &narrow).await.unwrap();
        assert!(read_prefix_snapshot_on(&conn, &scope_wide, "bucket", "")
            .await
            .is_err());
        assert!(read_prefix_snapshot_on(&conn, &scope_narrow, "bucket", "")
            .await
            .unwrap()
            .contents
            .files
            .is_empty());
        // A pending wide-token metadata read cannot run inside the new scope.
        assert!(
            in_scope(scope_wide, check_context_on(&conn, "account", false))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn direct_account_deletion_revokes_a_previously_valid_scope() {
        let (_db, conn) = fixture().await;
        conn.execute("CREATE TABLE accounts(id TEXT PRIMARY KEY)", ())
            .await
            .unwrap();
        conn.execute("INSERT INTO accounts(id) VALUES ('account')", ())
            .await
            .unwrap();
        conn.execute_batch(super::super::tokens::get_table_sql())
            .await
            .unwrap();
        conn.execute("INSERT INTO tokens(account_id,api_token,access_key_id,secret_access_key,created_at,updated_at) VALUES ('account','','key','secret',0,0)", ()).await.unwrap();
        let mut r2 = config("ignored.example");
        r2.provider = "r2".into();
        let scope = capture_on(&conn, &r2).await.unwrap();
        publish_on(&conn, scope.clone(), "before-delete")
            .await
            .unwrap();
        // The R2 account command deletes tokens directly, without calling each
        // token-update helper. Stored scope metadata alone is not authority.
        conn.execute("DELETE FROM tokens WHERE account_id='account'", ())
            .await
            .unwrap();
        conn.execute("DELETE FROM accounts WHERE id='account'", ())
            .await
            .unwrap();
        assert_eq!(
            stored_scope_on(&conn, "account").await.unwrap().as_ref(),
            Some(&scope)
        );
        assert!(read_prefix_snapshot_on(&conn, &scope, "bucket", "")
            .await
            .is_err());
        assert!(publish_on(&conn, scope, "after-delete").await.is_err());
        assert!(capture_on(&conn, &r2).await.is_err());
    }
}
