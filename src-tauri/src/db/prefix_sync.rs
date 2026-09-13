use super::DbResult;

pub fn get_table_sql() -> &'static str {
    "
    CREATE TABLE IF NOT EXISTS prefix_sync_times (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        prefix TEXT NOT NULL,
        last_synced_at INTEGER NOT NULL,
        file_count INTEGER NOT NULL DEFAULT 0,
        folder_count INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (bucket, account_id, prefix)
    );
    CREATE INDEX IF NOT EXISTS idx_prefix_sync ON prefix_sync_times(bucket, account_id, prefix);
    "
}

/// Publish a complete delimiter listing and its freshness record together.
/// Partial pages never call this function: a failure leaves the old snapshot
/// and marker unchanged, including a previously valid empty directory.
pub async fn replace_complete_prefix(
    bucket: &str,
    account_id: &str,
    prefix: &str,
    files: &[super::CachedFile],
    folders: &[String],
) -> DbResult<()> {
    let conn = super::cache_scope::write_connection(account_id).await?;
    replace_complete_prefix_on(&conn, bucket, account_id, prefix, files, folders).await
}

pub(crate) async fn replace_complete_prefix_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefix: &str,
    files: &[super::CachedFile],
    folders: &[String],
) -> DbResult<()> {
    let now = chrono::Utc::now().timestamp();
    conn.execute("BEGIN TRANSACTION", ()).await?;
    let result = async {
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
            "INSERT INTO prefix_sync_times (bucket, account_id, prefix, last_synced_at, file_count, folder_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (bucket, account_id, prefix) DO UPDATE SET last_synced_at = ?4, file_count = ?5, folder_count = ?6",
            turso::params![bucket, account_id, prefix, now, files.len() as i64, folders.len() as i64],
        ).await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    }.await;
    if let Err(error) = result {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(error);
    }
    conn.execute("COMMIT", ()).await?;
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

async fn invalidate_prefixes_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    prefixes: &[String],
) -> DbResult<()> {
    for chunk in prefixes.chunks(500) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("UPDATE prefix_sync_times SET last_synced_at = 0 WHERE bucket = ? AND account_id = ? AND prefix IN ({placeholders})");
        let mut params: Vec<turso::Value> =
            vec![bucket.to_string().into(), account_id.to_string().into()];
        params.extend(chunk.iter().map(|prefix| prefix.clone().into()));
        conn.execute(&sql, params).await?;
    }
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
        conn.execute_batch(
            "CREATE TABLE cached_files (bucket TEXT, account_id TEXT, key TEXT, parent_path TEXT, name TEXT, size INTEGER, last_modified TEXT, synced_at INTEGER, PRIMARY KEY(bucket, account_id, key));
             CREATE TABLE directory_tree (bucket TEXT, account_id TEXT, path TEXT, parent_path TEXT, file_count INTEGER, total_file_count INTEGER, size INTEGER, total_size INTEGER, last_modified TEXT, last_updated INTEGER, PRIMARY KEY(bucket, account_id, path));",
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
        replace_complete_prefix_on(
            &conn,
            "bucket",
            "account",
            "",
            &[file("old.txt")],
            &["gone/".into()],
        )
        .await
        .unwrap();
        replace_complete_prefix_on(&conn, "bucket", "account", "", &[], &[])
            .await
            .unwrap();
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
        replace_complete_prefix_on(
            &conn,
            "bucket",
            "account",
            "",
            &[file("old.txt")],
            &["kept/".into()],
        )
        .await
        .unwrap();
        conn.execute("UPDATE prefix_sync_times SET last_synced_at = 123", ())
            .await
            .unwrap();
        let result = replace_complete_prefix_on(
            &conn,
            "bucket",
            "account",
            "",
            &[file("new.txt")],
            &["invalid/nested/".into()],
        )
        .await;
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
        replace_complete_prefix_on(
            &conn,
            "bucket",
            "account",
            "",
            &[],
            &["kept/".into(), "gone/".into()],
        )
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
        replace_complete_prefix_on(&conn, "bucket", "account", "", &[], &folders)
            .await
            .unwrap();
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
        replace_complete_prefix_on(&conn, "bucket", "account", "known/", &[], &[])
            .await
            .unwrap();
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
            .query("SELECT prefix, last_synced_at FROM prefix_sync_times", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "known/");
        assert_eq!(row.get::<i64>(1).unwrap(), 0);
    }
}
