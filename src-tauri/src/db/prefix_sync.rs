use super::DbResult;
use std::collections::BTreeSet;

pub fn get_table_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS prefix_sync_times (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        prefix TEXT NOT NULL,
        last_synced_at INTEGER NOT NULL,
        file_count INTEGER NOT NULL DEFAULT 0,
        folder_count INTEGER NOT NULL DEFAULT 0,
        generation INTEGER NOT NULL DEFAULT 0,
        -- When a listing last published this folder fresh; 0 after a stale
        -- publish. Unlike last_synced_at, local writes never reset it.
        listed_at INTEGER NOT NULL DEFAULT 0,
        -- Whether the cached rows are the folder's complete snapshot: a
        -- listing no local write overlapped published them, and every local
        -- write since was applied to them. Served first, stale, once a write
        -- has zeroed last_synced_at. A stale publish clears it; the marker a
        -- write leaves on a folder never listed does not set it.
        complete INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (bucket, account_id, prefix)
    );
    CREATE INDEX IF NOT EXISTS idx_prefix_sync ON prefix_sync_times(bucket, account_id, prefix);

    -- Advanced by every local cache write or invalidation that can change a
    -- folder's listing; a listing that overlapped a change publishes stale.
    CREATE TABLE IF NOT EXISTS prefix_mutation_generations (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        prefix TEXT NOT NULL,
        generation INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (bucket, account_id, prefix)
    );
    "
}

/// Publish a complete delimiter listing and its freshness record together.
/// Partial pages never call this function: a failure leaves the old snapshot
/// and marker unchanged, including a previously valid empty directory.
///
/// `listed_generation` is the folder's mutation generation captured before the
/// listing's first request. If a local write advanced it since, the pages may
/// predate that write: the rows are still written, but without a fresh marker
/// or the `complete` bit, so the next open lists live instead of serving them
/// first, and without `listed_at`, so a running full sync publishes its own
/// (journal-replayed) rows for the folder instead of these -- and, since this
/// listing may have seen changes the scan did not, does not vouch for them
/// either. Returns whether the listing was published fresh.
pub async fn replace_complete_prefix(
    bucket: &str,
    account_id: &str,
    prefix: &str,
    files: &[super::CachedFile],
    folders: &[String],
    listed_generation: i64,
) -> DbResult<bool> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    replace_complete_prefix_on(
        &conn,
        bucket,
        account_id,
        prefix,
        files,
        folders,
        listed_generation,
    )
    .await
}

pub(crate) async fn replace_complete_prefix_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefix: &str,
    files: &[super::CachedFile],
    folders: &[String],
    listed_generation: i64,
) -> DbResult<bool> {
    let now = chrono::Utc::now().timestamp();
    conn.execute("BEGIN TRANSACTION", ()).await?;
    let result = async {
        let fresh =
            mutation_generation_on(conn, bucket, account_id, prefix).await? == listed_generation;
        let listed_at = if fresh { now } else { 0 };
        if !fresh {
            super::file_cache::journal_stale_listing_on(conn, bucket, account_id, prefix).await?;
        }
        conn.execute("DELETE FROM cached_files WHERE bucket = ?1 AND account_id = ?2 AND parent_path = ?3",
            turso::params![bucket, account_id, prefix]).await?;
        for chunk in files.chunks(500) {
            let placeholders = chunk.iter().map(|_| "(?, ?, ?, ?, ?, ?, ?, ?)").collect::<Vec<_>>().join(",");
            let sql = format!("INSERT OR REPLACE INTO cached_files (bucket, account_id, key, parent_path, name, size, last_modified, synced_at) VALUES {placeholders}");
            let mut params: Vec<turso::Value> = Vec::with_capacity(chunk.len() * 8);
            for file in chunk {
                if file.parent_path != prefix { return Err("Listing includes a file outside the requested prefix".into()); }
                params.extend([bucket.to_string().into(), account_id.to_string().into(), file.key.clone().into(),
                    file.parent_path.clone().into(), file.name.clone().into(), file.size.into(), file.last_modified.clone().into(), now.into()]);
            }
            conn.execute(&sql, params).await?;
        }
        super::dir_tree::replace_prefix_children_on(conn, bucket, account_id, prefix, folders).await?;
        conn.execute(
            "INSERT INTO prefix_sync_times (bucket, account_id, prefix, last_synced_at, listed_at, file_count, folder_count, generation, complete)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, 1, ?7)
             ON CONFLICT (bucket, account_id, prefix) DO UPDATE SET
               last_synced_at = ?4,
               listed_at = ?4,
               file_count = ?5,
               folder_count = ?6,
               generation = prefix_sync_times.generation + 1,
               complete = ?7",
            turso::params![bucket, account_id, prefix, listed_at, files.len() as i64, folders.len() as i64, fresh as i64],
        ).await?;
        super::file_cache::bump_content_revision_on(conn, bucket, account_id).await?;
        Ok::<bool, Box<dyn std::error::Error + Send + Sync>>(fresh)
    }.await;
    match result {
        Ok(fresh) => {
            conn.execute("COMMIT", ()).await?;
            Ok(fresh)
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
}

/// The folder's mutation generation, to be captured before a listing sends
/// its first request. The row is created here so that bucket- and account-wide
/// invalidations, which advance every existing row, also reach this listing.
pub(crate) async fn capture_mutation_generation_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefix: &str,
) -> DbResult<i64> {
    conn.execute(
        "INSERT INTO prefix_mutation_generations (bucket, account_id, prefix, generation)
         VALUES (?1, ?2, ?3, 0)
         ON CONFLICT (bucket, account_id, prefix) DO NOTHING",
        turso::params![bucket, account_id, prefix],
    )
    .await?;
    mutation_generation_on(conn, bucket, account_id, prefix).await
}

async fn mutation_generation_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefix: &str,
) -> DbResult<i64> {
    let mut rows = conn
        .query(
            "SELECT generation FROM prefix_mutation_generations
             WHERE bucket = ?1 AND account_id = ?2 AND prefix = ?3",
            turso::params![bucket, account_id, prefix],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    })
}

/// Advance the mutation generation of each prefix, creating rows as needed.
async fn advance_mutation_generations_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefixes: &BTreeSet<String>,
) -> DbResult<()> {
    let prefixes: Vec<&String> = prefixes.iter().collect();
    for chunk in prefixes.chunks(500) {
        let values = chunk
            .iter()
            .map(|_| "(?, ?, ?, 1)")
            .collect::<Vec<_>>()
            .join(",");
        let mut params: Vec<turso::Value> = Vec::with_capacity(chunk.len() * 3);
        for prefix in chunk {
            params.extend([
                bucket.to_string().into(),
                account_id.to_string().into(),
                (*prefix).clone().into(),
            ]);
        }
        conn.execute(
            &format!(
                "INSERT INTO prefix_mutation_generations (bucket, account_id, prefix, generation)
                 VALUES {values}
                 ON CONFLICT (bucket, account_id, prefix) DO UPDATE SET
                   generation = prefix_mutation_generations.generation + 1"
            ),
            params,
        )
        .await?;
    }
    Ok(())
}

/// Every folder whose listing shows a write to `keys`: each key's parent,
/// whose files change, and every ancestor, whose child folders may appear or
/// disappear.
pub(crate) fn listing_folders(keys: &[&str]) -> BTreeSet<String> {
    let mut folders = BTreeSet::new();
    for key in keys {
        let mut folder = super::parse_key(key).0;
        while folders.insert(folder.clone()) && !folder.is_empty() {
            folder = super::parse_key(folder.trim_end_matches('/')).0;
        }
    }
    folders
}

/// Record a local cache write to `keys` (upload, delete, move/rename) whose
/// rows the cache now holds. Their folders lose listing freshness, and every
/// folder whose listing can show the change advances its mutation generation,
/// whether or not the cache already had the keys.
pub(crate) async fn note_local_mutation_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    keys: &[&str],
) -> DbResult<()> {
    let parents: BTreeSet<String> = keys.iter().map(|key| super::parse_key(key).0).collect();
    expire_prefix_markers_on(conn, bucket, account_id, &parents).await?;
    advance_mutation_generations_on(conn, bucket, account_id, &listing_folders(keys)).await
}

/// Record a write to `keys` that the cache could not apply as rows (it ran
/// without a cache scope). No complete snapshot may vouch for the folders
/// that show it any more, the full index included: all of them re-list on
/// next open, and listings in flight publish stale.
pub(crate) async fn note_unapplied_write_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    keys: &[&str],
) -> DbResult<()> {
    let folders = listing_folders(keys);
    mark_folders_changed_on(conn, bucket, account_id, &folders).await?;
    advance_mutation_generations_on(conn, bucket, account_id, &folders).await
}

/// Leave a zero marker on each folder, listed or not: a kept zero marker
/// means "changed since listed", so neither the folder's own listing nor a
/// fresh full index makes it fresh, and it re-lists on next open.
pub(crate) async fn mark_folders_changed_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    folders: &BTreeSet<String>,
) -> DbResult<()> {
    let folders: Vec<&String> = folders.iter().collect();
    for chunk in folders.chunks(500) {
        let values = chunk
            .iter()
            .map(|_| "(?, ?, ?, 0, 1)")
            .collect::<Vec<_>>()
            .join(",");
        let mut params: Vec<turso::Value> = Vec::with_capacity(chunk.len() * 3);
        for folder in chunk {
            params.extend([
                bucket.to_string().into(),
                account_id.to_string().into(),
                (*folder).clone().into(),
            ]);
        }
        conn.execute(
            &format!(
                "INSERT INTO prefix_sync_times (bucket, account_id, prefix, last_synced_at, generation)
                 VALUES {values}
                 ON CONFLICT (bucket, account_id, prefix) DO UPDATE SET
                   last_synced_at = 0,
                   generation = prefix_sync_times.generation + 1"
            ),
            params,
        )
        .await?;
    }
    Ok(())
}

/// Invalidate freshness for already complete prefixes after local mutation.
/// A successful DELETE only validates those keys, not the entire directory:
/// extending its LIST timestamp would hide external changes indefinitely.
/// Keep the marker so a known empty/stale snapshot remains distinguishable
/// from a never-listed prefix.
pub async fn touch_prefix_sync_times_if_exists(
    bucket: &str,
    account_id: &str,
    prefixes: &[String],
) -> DbResult<()> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    invalidate_prefixes_on(&conn, bucket, account_id, prefixes).await
}

pub(crate) async fn invalidate_prefixes_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefixes: &[String],
) -> DbResult<()> {
    let prefixes: BTreeSet<String> = prefixes.iter().cloned().collect();
    expire_prefix_markers_on(conn, bucket, account_id, &prefixes).await?;
    advance_mutation_generations_on(conn, bucket, account_id, &prefixes).await
}

async fn expire_prefix_markers_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefixes: &BTreeSet<String>,
) -> DbResult<()> {
    let prefixes: Vec<&String> = prefixes.iter().collect();
    for chunk in prefixes.chunks(500) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE prefix_sync_times SET last_synced_at = 0, generation = generation + 1 WHERE bucket = ? AND account_id = ? AND prefix IN ({placeholders})"
        );
        let mut params: Vec<turso::Value> =
            vec![bucket.to_string().into(), account_id.to_string().into()];
        params.extend(chunk.iter().map(|prefix| (*prefix).clone().into()));
        conn.execute(&sql, params).await?;
    }
    Ok(())
}

/// Advance every folder's mutation generation for a bucket, or for a whole
/// account when `bucket` is None, after an invalidation that cannot name them.
pub(crate) async fn advance_all_mutation_generations_on(
    conn: &turso::Connection,
    account_id: &str,
    bucket: Option<&str>,
) -> DbResult<()> {
    match bucket {
        // A primary-key prefix range.
        Some(bucket) => {
            conn.execute(
                "UPDATE prefix_mutation_generations SET generation = generation + 1
                 WHERE bucket = ?1 AND account_id = ?2",
                turso::params![bucket, account_id],
            )
            .await?
        }
        // Only on an account reset or the unscoped-write safety net, never
        // per mutation.
        None => {
            conn.execute(
                "UPDATE prefix_mutation_generations SET generation = generation + 1
                 WHERE account_id = ?1",
                turso::params![account_id],
            )
            .await?
        }
    };
    Ok(())
}

/// Clear all prefix sync times for a bucket (used when switching accounts or full re-sync).
#[allow(dead_code)]
pub async fn clear_prefix_sync_times(bucket: &str, account_id: &str) -> DbResult<()> {
    let conn = super::cache_scope::clear_connection(account_id).await?;
    conn.execute(
        "DELETE FROM prefix_sync_times WHERE bucket = ?1 AND account_id = ?2",
        turso::params![bucket, account_id],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (turso::Database, turso::Connection) {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch(get_table_sql()).await.unwrap();
        conn.execute_batch(super::super::app_state::get_table_sql())
            .await
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE cached_files (bucket TEXT, account_id TEXT, key TEXT, parent_path TEXT, name TEXT, size INTEGER, last_modified TEXT, synced_at INTEGER, PRIMARY KEY(bucket, account_id, key));
             CREATE TABLE directory_tree (bucket TEXT, account_id TEXT, path TEXT, parent_path TEXT, file_count INTEGER, total_file_count INTEGER, size INTEGER, total_size INTEGER, last_modified TEXT, last_updated INTEGER, PRIMARY KEY(bucket, account_id, path));
             CREATE TABLE bucket_content_revisions (bucket TEXT, account_id TEXT, revision INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(bucket, account_id));",
        ).await.unwrap();
        (db, conn)
    }

    fn file(key: &str) -> super::super::CachedFile {
        let (parent_path, name) = super::super::parse_key(key);
        super::super::CachedFile {
            bucket: "bucket".into(),
            account_id: "account".into(),
            key: key.into(),
            parent_path,
            name,
            size: 3,
            last_modified: String::new(),
            synced_at: 1,
        }
    }

    async fn publish(
        conn: &turso::Connection,
        prefix: &str,
        files: &[super::super::CachedFile],
        folders: &[String],
    ) -> DbResult<bool> {
        let generation = capture_mutation_generation_on(conn, "bucket", "account", prefix)
            .await
            .unwrap();
        replace_complete_prefix_on(
            conn, "bucket", "account", prefix, files, folders, generation,
        )
        .await
    }

    async fn count(conn: &turso::Connection, table: &str) -> i64 {
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    #[tokio::test]
    async fn complete_empty_directory_keeps_a_completeness_marker() {
        let (_db, conn) = fixture().await;
        publish(&conn, "", &[file("old.txt")], &["gone/".into()])
            .await
            .unwrap();
        publish(&conn, "", &[], &[]).await.unwrap();
        assert_eq!(count(&conn, "cached_files").await, 0);
        assert_eq!(count(&conn, "directory_tree").await, 0);
        assert_eq!(count(&conn, "prefix_sync_times").await, 1);
        let mut rows = conn
            .query("SELECT file_count, folder_count FROM prefix_sync_times", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 0);
        assert_eq!(row.get::<i64>(1).unwrap(), 0);
    }

    #[tokio::test]
    async fn failed_snapshot_rolls_back_files_directories_and_freshness() {
        let (_db, conn) = fixture().await;
        publish(&conn, "", &[file("old.txt")], &["kept/".into()])
            .await
            .unwrap();
        conn.execute("UPDATE prefix_sync_times SET last_synced_at = 123", ())
            .await
            .unwrap();
        let result = publish(&conn, "", &[file("new.txt")], &["invalid/nested/".into()]).await;
        assert!(result.is_err());
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
            "old.txt"
        );
        let mut rows = conn
            .query("SELECT path FROM directory_tree", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "kept/"
        );
        let mut rows = conn
            .query("SELECT last_synced_at FROM prefix_sync_times", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            123
        );
    }

    #[tokio::test]
    async fn batched_folders_preserve_existing_aggregates_and_remove_absent_children() {
        let (_db, conn) = fixture().await;
        publish(&conn, "", &[], &["kept/".into(), "gone/".into()])
            .await
            .unwrap();
        conn.execute(
            "UPDATE directory_tree SET total_size = 99 WHERE path = 'kept/'",
            (),
        )
        .await
        .unwrap();
        let mut folders: Vec<String> = (0..1001).map(|index| format!("folder-{index}/")).collect();
        folders.push("kept/".into());
        publish(&conn, "", &[], &folders).await.unwrap();
        assert_eq!(count(&conn, "directory_tree").await, 1002);
        let mut rows = conn
            .query(
                "SELECT total_size FROM directory_tree WHERE path = 'kept/'",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            99
        );
    }
    #[tokio::test]
    async fn local_mutation_preserves_completeness_without_extending_directory_ttl() {
        let (_db, conn) = fixture().await;
        publish(&conn, "known/", &[], &[]).await.unwrap();
        invalidate_prefixes_on(
            &conn,
            "bucket",
            "account",
            &["known/".into(), "never-listed/".into()],
        )
        .await
        .unwrap();
        assert_eq!(count(&conn, "prefix_sync_times").await, 1);
        let mut rows = conn
            .query(
                "SELECT prefix, last_synced_at, generation, complete FROM prefix_sync_times",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "known/");
        assert_eq!(row.get::<i64>(1).unwrap(), 0);
        assert_eq!(row.get::<i64>(2).unwrap(), 2);
        // The rows stay the folder's snapshot: served first, stale.
        assert_eq!(row.get::<i64>(3).unwrap(), 1);
    }

    async fn marker(conn: &turso::Connection, prefix: &str) -> Option<i64> {
        let mut rows = conn
            .query(
                "SELECT last_synced_at FROM prefix_sync_times WHERE prefix = ?1",
                turso::params![prefix],
            )
            .await
            .unwrap();
        rows.next()
            .await
            .unwrap()
            .map(|row| row.get::<i64>(0).unwrap())
    }

    async fn complete(conn: &turso::Connection, prefix: &str) -> bool {
        let mut rows = conn
            .query(
                "SELECT complete FROM prefix_sync_times WHERE prefix = ?1",
                turso::params![prefix],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap() != 0
    }

    #[tokio::test]
    async fn listing_that_overlaps_a_local_write_publishes_rows_without_a_fresh_marker() {
        let (_db, conn) = fixture().await;
        // The folder was never listed or cached: the write must still count.
        let listed = capture_mutation_generation_on(&conn, "bucket", "account", "dir/")
            .await
            .unwrap();
        let root_listed = capture_mutation_generation_on(&conn, "bucket", "account", "")
            .await
            .unwrap();
        note_local_mutation_on(&conn, "bucket", "account", &["dir/sub/new.txt"])
            .await
            .unwrap();

        // "dir/" is an ancestor of the write: its child folders may change.
        let fresh = replace_complete_prefix_on(
            &conn,
            "bucket",
            "account",
            "dir/",
            &[file("dir/k.txt")],
            &[],
            listed,
        )
        .await
        .unwrap();
        assert!(!fresh);
        assert_eq!(marker(&conn, "dir/").await, Some(0));
        // Its rows may predate the write: they are no snapshot to serve first.
        assert!(!complete(&conn, "dir/").await);
        assert_eq!(count(&conn, "cached_files").await, 1);
        let root_fresh =
            replace_complete_prefix_on(&conn, "bucket", "account", "", &[], &[], root_listed)
                .await
                .unwrap();
        assert!(!root_fresh);

        // A listing that starts after the write publishes fresh again.
        assert!(publish(&conn, "dir/", &[file("dir/k.txt")], &[])
            .await
            .unwrap());
        assert!(marker(&conn, "dir/").await.unwrap() > 0);
        assert!(complete(&conn, "dir/").await);

        // Plain invalidation advances never-listed folders too.
        let listed = capture_mutation_generation_on(&conn, "bucket", "account", "other/")
            .await
            .unwrap();
        invalidate_prefixes_on(&conn, "bucket", "account", &["other/".into()])
            .await
            .unwrap();
        let fresh =
            replace_complete_prefix_on(&conn, "bucket", "account", "other/", &[], &[], listed)
                .await
                .unwrap();
        assert!(!fresh);
    }

    #[tokio::test]
    async fn account_and_bucket_invalidations_reach_listings_in_flight() {
        let (_db, conn) = fixture().await;
        let listed = capture_mutation_generation_on(&conn, "bucket", "account", "")
            .await
            .unwrap();
        let other_bucket = capture_mutation_generation_on(&conn, "other", "account", "")
            .await
            .unwrap();
        let other_account = capture_mutation_generation_on(&conn, "bucket", "stranger", "")
            .await
            .unwrap();
        advance_all_mutation_generations_on(&conn, "account", Some("bucket"))
            .await
            .unwrap();
        assert_ne!(
            mutation_generation_on(&conn, "bucket", "account", "")
                .await
                .unwrap(),
            listed
        );
        assert_eq!(
            mutation_generation_on(&conn, "other", "account", "")
                .await
                .unwrap(),
            other_bucket
        );
        advance_all_mutation_generations_on(&conn, "account", None)
            .await
            .unwrap();
        assert_ne!(
            mutation_generation_on(&conn, "other", "account", "")
                .await
                .unwrap(),
            other_bucket
        );
        assert_eq!(
            mutation_generation_on(&conn, "bucket", "stranger", "")
                .await
                .unwrap(),
            other_account
        );
    }
}
