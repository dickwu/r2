//! Index of the on-disk thumbnail cache.
//!
//! One row per object that has a thumbnail file. The row records the object
//! identity the thumbnail was rendered from (etag, or size plus last-modified)
//! so a lookup can tell, without any network or ffmpeg, whether the cached
//! file still shows the current object. The same rows drive eviction (least
//! recently used, unused for too long) and the size/count shown in Settings,
//! so the cache directory never has to be walked for either.
//!
//! The files themselves live under the app cache dir and are managed by
//! `crate::thumbnail_cache`; functions here only touch the table and return
//! the relative file paths a caller should delete.

use super::DbResult;
use std::collections::{HashMap, HashSet};
use turso::Connection;

pub fn get_table_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS thumbnail_cache (
        account_id TEXT NOT NULL,
        bucket TEXT NOT NULL,
        key TEXT NOT NULL,
        -- Identity of the object the thumbnail was rendered from.
        etag TEXT,
        size INTEGER,
        -- Normalized to RFC 3339 UTC so listings that format it differently match.
        last_modified TEXT,
        -- 'image' or 'video'
        kind TEXT NOT NULL,
        -- Path of the thumbnail file, relative to the thumbnail cache dir.
        file_path TEXT NOT NULL,
        bytes INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        last_used_at INTEGER NOT NULL,
        PRIMARY KEY (account_id, bucket, key)
    );
    CREATE INDEX IF NOT EXISTS idx_thumbnail_cache_lru ON thumbnail_cache(last_used_at);
    "
}

/// The object a thumbnail is for, as the listing describes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailObject {
    pub account_id: String,
    pub bucket: String,
    pub key: String,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub last_modified: Option<String>,
}

impl ThumbnailObject {
    /// Same object with its etag unquoted and its timestamp normalized.
    pub fn normalized(&self) -> Self {
        Self {
            account_id: self.account_id.clone(),
            bucket: self.bucket.clone(),
            key: self.key.clone(),
            etag: normalize_etag(self.etag.as_deref()),
            size: self.size,
            last_modified: normalize_last_modified(self.last_modified.as_deref()),
        }
    }
}

pub fn normalize_etag(etag: Option<&str>) -> Option<String> {
    let etag = etag?
        .trim()
        .trim_start_matches("W/")
        .trim_matches('"')
        .trim();
    (!etag.is_empty()).then(|| etag.to_string())
}

/// RFC 3339 UTC at second precision when the value parses as a date, the
/// trimmed text otherwise. S3 listings carry milliseconds, HEAD responses and
/// some SDKs do not; both name the same version of the object.
pub fn normalize_last_modified(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(value)
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(value))
        .map(|d| d.with_timezone(&chrono::Utc));
    Some(match parsed {
        Ok(date) => date.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        Err(_) => value.to_string(),
    })
}

/// A row of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailEntry {
    pub key: String,
    pub etag: Option<String>,
    pub size: Option<i64>,
    pub last_modified: Option<String>,
    pub kind: String,
    pub file_path: String,
    pub bytes: i64,
    pub last_used_at: i64,
}

/// Whether a cached thumbnail was rendered from the object the listing now
/// describes. Both sides are expected to be normalized. When both carry an
/// etag it decides; otherwise size and last-modified must both be known and
/// equal, so an object without metadata is never served a stale thumbnail.
pub fn identity_matches(
    entry_etag: Option<&str>,
    entry_size: Option<i64>,
    entry_last_modified: Option<&str>,
    etag: Option<&str>,
    size: Option<i64>,
    last_modified: Option<&str>,
) -> bool {
    if let (Some(a), Some(b)) = (entry_etag, etag) {
        // A size that is known on both sides must agree too: an etag alone
        // is not unique across multipart uploads of different content.
        return a == b && (entry_size.is_none() || size.is_none() || entry_size == size);
    }
    match (entry_size, size, entry_last_modified, last_modified) {
        (Some(a), Some(b), Some(c), Some(d)) => a == b && c == d,
        _ => false,
    }
}

const ENTRY_COLUMNS: &str = "key, etag, size, last_modified, kind, file_path, bytes, last_used_at";

fn entry_from_row(row: &turso::Row) -> DbResult<ThumbnailEntry> {
    Ok(ThumbnailEntry {
        key: row.get(0)?,
        etag: row.get(1)?,
        size: row.get(2)?,
        last_modified: row.get(3)?,
        kind: row.get(4)?,
        file_path: row.get(5)?,
        bytes: row.get(6)?,
        last_used_at: row.get(7)?,
    })
}

/// The lookup result for an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Serve this file: it was rendered from the current object.
    Fresh(ThumbnailEntry),
    /// A thumbnail exists for an older version of the object.
    Stale(ThumbnailEntry),
    Missing,
}

pub async fn lookup_on(conn: &Connection, object: &ThumbnailObject) -> DbResult<Lookup> {
    let object = object.normalized();
    let mut rows = conn
        .query(
            &format!(
                "SELECT {ENTRY_COLUMNS} FROM thumbnail_cache WHERE account_id = ?1 AND bucket = ?2 AND key = ?3"
            ),
            turso::params![object.account_id.as_str(), object.bucket.as_str(), object.key.as_str()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(Lookup::Missing);
    };
    let entry = entry_from_row(&row)?;
    let fresh = identity_matches(
        entry.etag.as_deref(),
        entry.size,
        entry.last_modified.as_deref(),
        object.etag.as_deref(),
        object.size,
        object.last_modified.as_deref(),
    );
    Ok(if fresh {
        Lookup::Fresh(entry)
    } else {
        Lookup::Stale(entry)
    })
}

/// Record a hit. Only rows not used for `min_interval` seconds are written, so
/// scrolling a grid of cached tiles does not turn into a stream of writes.
pub async fn touch_on(
    conn: &Connection,
    object: &ThumbnailObject,
    now: i64,
    min_interval: i64,
) -> DbResult<()> {
    conn.execute(
        "UPDATE thumbnail_cache SET last_used_at = ?4
         WHERE account_id = ?1 AND bucket = ?2 AND key = ?3 AND last_used_at <= ?5",
        turso::params![
            object.account_id.as_str(),
            object.bucket.as_str(),
            object.key.as_str(),
            now,
            now - min_interval
        ],
    )
    .await?;
    Ok(())
}

/// Insert or replace the row for an object. Returns the file path of the row
/// it replaced when that file differs from the new one (the caller deletes it).
pub async fn upsert_on(
    conn: &Connection,
    object: &ThumbnailObject,
    kind: &str,
    file_path: &str,
    bytes: i64,
    now: i64,
) -> DbResult<Option<String>> {
    let object = object.normalized();
    let previous = match lookup_on(conn, &object).await? {
        Lookup::Fresh(entry) | Lookup::Stale(entry) => Some(entry.file_path),
        Lookup::Missing => None,
    };
    conn.execute(
        "INSERT INTO thumbnail_cache
            (account_id, bucket, key, etag, size, last_modified, kind, file_path, bytes, created_at, last_used_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
         ON CONFLICT (account_id, bucket, key) DO UPDATE SET
            etag = ?4, size = ?5, last_modified = ?6, kind = ?7, file_path = ?8,
            bytes = ?9, created_at = ?10, last_used_at = ?10",
        turso::params![
            object.account_id.as_str(),
            object.bucket.as_str(),
            object.key.as_str(),
            object.etag.clone(),
            object.size,
            object.last_modified.clone(),
            kind,
            file_path,
            bytes,
            now
        ],
    )
    .await?;
    Ok(previous.filter(|p| p != file_path))
}

/// Drop the rows of the given keys; returns their file paths.
pub async fn remove_keys_on(
    conn: &Connection,
    bucket: &str,
    account_id: &str,
    keys: &[&str],
) -> DbResult<Vec<String>> {
    let mut files = Vec::new();
    for chunk in keys.chunks(400) {
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 3)).collect();
        let mut params: Vec<turso::Value> =
            vec![account_id.to_string().into(), bucket.to_string().into()];
        params.extend(chunk.iter().map(|k| turso::Value::from(k.to_string())));
        let mut rows = conn
            .query(
                &format!(
                    "SELECT file_path FROM thumbnail_cache WHERE account_id = ?1 AND bucket = ?2 AND key IN ({})",
                    placeholders.join(", ")
                ),
                params.clone(),
            )
            .await?;
        let mut found = false;
        while let Some(row) = rows.next().await? {
            files.push(row.get::<String>(0)?);
            found = true;
        }
        drop(rows);
        if found {
            conn.execute(
                &format!(
                    "DELETE FROM thumbnail_cache WHERE account_id = ?1 AND bucket = ?2 AND key IN ({})",
                    placeholders.join(", ")
                ),
                params,
            )
            .await?;
        }
    }
    Ok(files)
}

/// What a listing or sync changed among the cached thumbnails.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// Objects whose listing metadata no longer matches their thumbnail. The
    /// rows are dropped; the entries carry the new metadata (from the cached
    /// listing) so the caller can regenerate them.
    pub changed: Vec<ThumbnailEntry>,
    /// Keys of objects the listing no longer has.
    pub removed: Vec<String>,
    /// Files to delete.
    pub files: Vec<String>,
}

impl Reconciled {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }
}

/// Which cached thumbnails a listing covers, and whether a key missing from
/// the listing means the object is gone.
pub enum Scope<'a> {
    /// The direct children of one folder, from a complete delimiter listing.
    Folder(&'a str),
    /// A whole-bucket scan, except under these skipped prefixes.
    Bucket { skipped_prefixes: &'a [String] },
    /// Just these keys (single-object writes); missing keys are not removed.
    Keys(&'a [&'a str]),
}

fn in_scope(scope: &Scope<'_>, key: &str) -> bool {
    match scope {
        Scope::Folder(prefix) => super::file_cache::parse_key(key).0 == *prefix,
        Scope::Bucket { skipped_prefixes } => {
            !skipped_prefixes.iter().any(|p| key.starts_with(p.as_str()))
        }
        Scope::Keys(keys) => keys.contains(&key),
    }
}

/// Compare the cached thumbnails in `scope` against `cached_files`, the
/// listing the app just published: drop thumbnails of changed objects (and
/// report their new metadata) and of objects the listing no longer has.
pub async fn reconcile_on(
    conn: &Connection,
    bucket: &str,
    account_id: &str,
    scope: Scope<'_>,
) -> DbResult<Reconciled> {
    // The index is small (bounded by the cache size limit), so read the
    // bucket's rows and filter here rather than build per-scope SQL.
    let mut entries: Vec<ThumbnailEntry> = Vec::new();
    let mut rows = conn
        .query(
            &format!(
                "SELECT {ENTRY_COLUMNS} FROM thumbnail_cache WHERE account_id = ?1 AND bucket = ?2"
            ),
            turso::params![account_id, bucket],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let entry = entry_from_row(&row)?;
        if in_scope(&scope, &entry.key) {
            entries.push(entry);
        }
    }
    drop(rows);
    if entries.is_empty() {
        return Ok(Reconciled::default());
    }

    let mut listed: HashMap<String, (i64, String)> = HashMap::new();
    for chunk in entries.chunks(400) {
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 3)).collect();
        let mut params: Vec<turso::Value> =
            vec![bucket.to_string().into(), account_id.to_string().into()];
        params.extend(chunk.iter().map(|e| turso::Value::from(e.key.clone())));
        let mut rows = conn
            .query(
                &format!(
                    "SELECT key, size, last_modified FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND key IN ({})",
                    placeholders.join(", ")
                ),
                params,
            )
            .await?;
        while let Some(row) = rows.next().await? {
            listed.insert(row.get(0)?, (row.get(1)?, row.get(2)?));
        }
    }

    let removable = !matches!(scope, Scope::Keys(_));
    let mut out = Reconciled::default();
    for entry in entries {
        match listed.get(&entry.key) {
            Some((size, last_modified)) => {
                let last_modified = normalize_last_modified(Some(last_modified));
                // The cached listing has no etag: compare size and time.
                let same = identity_matches(
                    None,
                    entry.size,
                    entry.last_modified.as_deref(),
                    None,
                    Some(*size),
                    last_modified.as_deref(),
                );
                if !same {
                    out.files.push(entry.file_path.clone());
                    out.changed.push(ThumbnailEntry {
                        size: Some(*size),
                        last_modified,
                        etag: None,
                        ..entry
                    });
                }
            }
            None if removable => {
                out.files.push(entry.file_path.clone());
                out.removed.push(entry.key);
            }
            None => {}
        }
    }
    let gone: Vec<&str> = out
        .changed
        .iter()
        .map(|e| e.key.as_str())
        .chain(out.removed.iter().map(String::as_str))
        .collect();
    if !gone.is_empty() {
        remove_keys_on(conn, bucket, account_id, &gone).await?;
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailStats {
    pub count: i64,
    pub bytes: i64,
}

pub async fn stats_on(conn: &Connection) -> DbResult<ThumbnailStats> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM thumbnail_cache",
            (),
        )
        .await?;
    let row = rows.next().await?.ok_or("no stats row")?;
    Ok(ThumbnailStats {
        count: row.get(0)?,
        bytes: row.get(1)?,
    })
}

/// Evict rows unused since `now - max_age` and then the least recently used
/// rows until the total is at most `target_bytes`. Returns the file paths.
pub async fn evict_on(
    conn: &Connection,
    max_bytes: i64,
    target_bytes: i64,
    max_age: i64,
    now: i64,
) -> DbResult<Vec<String>> {
    let mut files = Vec::new();
    let mut doomed: Vec<(String, String, String)> = Vec::new();
    let mut total = stats_on(conn).await?.bytes;
    let over = total > max_bytes;
    let mut rows = conn
        .query(
            "SELECT account_id, bucket, key, file_path, bytes, last_used_at FROM thumbnail_cache ORDER BY last_used_at ASC",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let last_used_at: i64 = row.get(5)?;
        let aged = last_used_at < now - max_age;
        if !aged && !(over && total > target_bytes) {
            break;
        }
        let bytes: i64 = row.get(4)?;
        total -= bytes;
        files.push(row.get(3)?);
        doomed.push((row.get(0)?, row.get(1)?, row.get(2)?));
    }
    drop(rows);
    for (account_id, bucket, key) in &doomed {
        conn.execute(
            "DELETE FROM thumbnail_cache WHERE account_id = ?1 AND bucket = ?2 AND key = ?3",
            turso::params![account_id.as_str(), bucket.as_str(), key.as_str()],
        )
        .await?;
    }
    Ok(files)
}

/// Drop every row; returns the stats before clearing.
pub async fn clear_on(conn: &Connection) -> DbResult<ThumbnailStats> {
    let before = stats_on(conn).await?;
    conn.execute("DELETE FROM thumbnail_cache", ()).await?;
    Ok(before)
}

/// Every file path the index references (for the orphan sweep at startup).
pub async fn file_paths_on(conn: &Connection) -> DbResult<HashSet<String>> {
    let mut out = HashSet::new();
    let mut rows = conn
        .query("SELECT file_path FROM thumbnail_cache", ())
        .await?;
    while let Some(row) = rows.next().await? {
        out.insert(row.get(0)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use turso::Builder;

    async fn conn() -> Connection {
        let db = Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        super::super::migrate_on(&conn).await.unwrap();
        // Keep the database alive for the connection's lifetime.
        std::mem::forget(db);
        conn
    }

    fn obj(key: &str, size: i64, lm: &str) -> ThumbnailObject {
        ThumbnailObject {
            account_id: "acct".into(),
            bucket: "photos".into(),
            key: key.into(),
            etag: None,
            size: Some(size),
            last_modified: Some(lm.into()),
        }
    }

    async fn list(conn: &Connection, key: &str, size: i64, lm: &str) {
        let (parent, name) = super::super::file_cache::parse_key(key);
        conn.execute(
            "INSERT OR REPLACE INTO cached_files (bucket, account_id, key, parent_path, name, size, last_modified, synced_at)
             VALUES ('photos', 'acct', ?1, ?2, ?3, ?4, ?5, 1)",
            turso::params![key, parent, name, size, lm],
        )
        .await
        .unwrap();
    }

    #[test]
    fn identity_prefers_etag_and_needs_full_metadata_otherwise() {
        assert!(identity_matches(
            Some("a"),
            Some(1),
            None,
            Some("a"),
            Some(1),
            None
        ));
        assert!(!identity_matches(
            Some("a"),
            Some(1),
            None,
            Some("b"),
            Some(1),
            None
        ));
        assert!(!identity_matches(
            Some("a"),
            Some(1),
            None,
            Some("a"),
            Some(2),
            None
        ));
        assert!(identity_matches(
            None,
            Some(1),
            Some("t"),
            Some("x"),
            Some(1),
            Some("t")
        ));
        assert!(!identity_matches(
            None,
            Some(1),
            Some("t"),
            None,
            Some(1),
            Some("u")
        ));
        assert!(!identity_matches(None, None, None, None, None, None));
        assert!(!identity_matches(None, Some(1), None, None, Some(1), None));
    }

    #[test]
    fn metadata_is_normalized() {
        assert_eq!(normalize_etag(Some("\"abc\"")), Some("abc".into()));
        assert_eq!(normalize_etag(Some("W/\"abc\"")), Some("abc".into()));
        assert_eq!(normalize_etag(Some("  ")), None);
        let a = normalize_last_modified(Some("2026-11-03T12:00:00.000Z"));
        let b = normalize_last_modified(Some("2026-11-03T07:00:00-05:00"));
        let c = normalize_last_modified(Some("Tue, 03 Nov 2026 12:00:00 GMT"));
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a.as_deref(), Some("2026-11-03T12:00:00Z"));
        assert_eq!(normalize_last_modified(Some("odd")), Some("odd".into()));
    }

    #[tokio::test]
    async fn lookup_matches_then_goes_stale_when_the_object_changes() {
        let conn = conn().await;
        let o = obj("a/1.jpg", 10, "2026-11-01T00:00:00.000Z");
        assert_eq!(lookup_on(&conn, &o).await.unwrap(), Lookup::Missing);
        assert_eq!(
            upsert_on(&conn, &o, "image", "ab/old.webp", 100, 5)
                .await
                .unwrap(),
            None
        );

        // Same object, timestamp formatted differently: served from cache.
        let same = obj("a/1.jpg", 10, "2026-11-01T00:00:00Z");
        assert!(
            matches!(lookup_on(&conn, &same).await.unwrap(), Lookup::Fresh(e) if e.file_path == "ab/old.webp")
        );

        // Overwritten object: stale, and replacing it hands back the old file.
        let changed = obj("a/1.jpg", 11, "2026-11-02T00:00:00Z");
        assert!(matches!(
            lookup_on(&conn, &changed).await.unwrap(),
            Lookup::Stale(_)
        ));
        assert_eq!(
            upsert_on(&conn, &changed, "image", "cd/new.webp", 120, 6)
                .await
                .unwrap(),
            Some("ab/old.webp".into())
        );
        assert!(
            matches!(lookup_on(&conn, &changed).await.unwrap(), Lookup::Fresh(e) if e.file_path == "cd/new.webp")
        );
        assert_eq!(
            stats_on(&conn).await.unwrap(),
            ThumbnailStats {
                count: 1,
                bytes: 120
            }
        );
    }

    #[tokio::test]
    async fn reconcile_reports_changed_and_deleted_objects_in_scope() {
        let conn = conn().await;
        for (key, file) in [
            ("a/same.jpg", "f1"),
            ("a/changed.jpg", "f2"),
            ("a/gone.jpg", "f3"),
            ("b/other.jpg", "f4"),
        ] {
            upsert_on(
                &conn,
                &obj(key, 10, "2026-11-01T00:00:00Z"),
                "image",
                file,
                10,
                1,
            )
            .await
            .unwrap();
        }
        list(&conn, "a/same.jpg", 10, "2026-11-01T00:00:00.000Z").await;
        list(&conn, "a/changed.jpg", 20, "2026-11-03T00:00:00.000Z").await;

        let out = reconcile_on(&conn, "photos", "acct", Scope::Folder("a/"))
            .await
            .unwrap();
        assert_eq!(out.removed, vec!["a/gone.jpg".to_string()]);
        assert_eq!(out.changed.len(), 1);
        assert_eq!(out.changed[0].key, "a/changed.jpg");
        assert_eq!(out.changed[0].size, Some(20));
        assert_eq!(
            out.changed[0].last_modified.as_deref(),
            Some("2026-11-03T00:00:00Z")
        );
        let mut files = out.files.clone();
        files.sort();
        assert_eq!(files, vec!["f2".to_string(), "f3".into()]);

        // The unchanged object is still served; the other folder is untouched.
        assert!(matches!(
            lookup_on(&conn, &obj("a/same.jpg", 10, "2026-11-01T00:00:00Z"))
                .await
                .unwrap(),
            Lookup::Fresh(_)
        ));
        assert!(matches!(
            lookup_on(&conn, &obj("b/other.jpg", 10, "2026-11-01T00:00:00Z"))
                .await
                .unwrap(),
            Lookup::Fresh(_)
        ));
        assert_eq!(stats_on(&conn).await.unwrap().count, 2);

        // Key scope never removes a key the cache simply does not list.
        let keys = ["b/other.jpg"];
        let out = reconcile_on(&conn, "photos", "acct", Scope::Keys(&keys))
            .await
            .unwrap();
        assert!(out.is_empty());

        // A whole-bucket scan removes it, except under a skipped prefix.
        let skipped = vec!["b/".to_string()];
        let out = reconcile_on(
            &conn,
            "photos",
            "acct",
            Scope::Bucket {
                skipped_prefixes: &skipped,
            },
        )
        .await
        .unwrap();
        assert!(out.is_empty());
        let out = reconcile_on(
            &conn,
            "photos",
            "acct",
            Scope::Bucket {
                skipped_prefixes: &[],
            },
        )
        .await
        .unwrap();
        assert_eq!(out.removed, vec!["b/other.jpg".to_string()]);
    }

    #[tokio::test]
    async fn deleting_objects_drops_their_thumbnails() {
        let conn = conn().await;
        upsert_on(&conn, &obj("x.jpg", 1, "t"), "image", "x", 1, 1)
            .await
            .unwrap();
        upsert_on(&conn, &obj("y.mp4", 1, "t"), "video", "y", 1, 1)
            .await
            .unwrap();
        let files = remove_keys_on(&conn, "photos", "acct", &["x.jpg", "nope"])
            .await
            .unwrap();
        assert_eq!(files, vec!["x".to_string()]);
        assert_eq!(
            lookup_on(&conn, &obj("x.jpg", 1, "t")).await.unwrap(),
            Lookup::Missing
        );
        assert_eq!(stats_on(&conn).await.unwrap().count, 1);
    }

    #[tokio::test]
    async fn eviction_drops_aged_then_least_recently_used() {
        let conn = conn().await;
        for (i, used) in [(0, 10), (1, 500), (2, 600), (3, 700)] {
            let o = obj(&format!("k{i}"), 1, "t");
            upsert_on(&conn, &o, "image", &format!("f{i}"), 100, used)
                .await
                .unwrap();
        }
        touch_on(&conn, &obj("k1", 1, "t"), 800, 60).await.unwrap();
        // Under the limit: only the row unused for longer than max_age goes.
        let files = evict_on(&conn, 1000, 900, 200, 800).await.unwrap();
        assert_eq!(files, vec!["f0".to_string()]);
        // Over the limit: least recently used first (k1 was touched at 800).
        let files = evict_on(&conn, 250, 200, 10_000, 800).await.unwrap();
        assert_eq!(files, vec!["f2".to_string()]);
        assert_eq!(
            stats_on(&conn).await.unwrap(),
            ThumbnailStats {
                count: 2,
                bytes: 200
            }
        );

        // touch_on is throttled.
        touch_on(&conn, &obj("k1", 1, "t"), 820, 60).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT last_used_at FROM thumbnail_cache WHERE key = 'k1'",
                (),
            )
            .await
            .unwrap();
        let used: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(used, 800);
    }

    #[tokio::test]
    async fn clearing_empties_the_index() {
        let conn = conn().await;
        upsert_on(&conn, &obj("x.jpg", 1, "t"), "image", "x", 7, 1)
            .await
            .unwrap();
        assert_eq!(
            clear_on(&conn).await.unwrap(),
            ThumbnailStats { count: 1, bytes: 7 }
        );
        assert_eq!(
            stats_on(&conn).await.unwrap(),
            ThumbnailStats { count: 0, bytes: 0 }
        );
        assert!(file_paths_on(&conn).await.unwrap().is_empty());
    }
}
