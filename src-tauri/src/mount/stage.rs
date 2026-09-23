//! Write-back staging for writable mounts.
//!
//! nfsserve 0.11 has no close/commit VFS hook and reports every successful
//! WRITE as FILE_SYNC. Callers must persist bytes and the stage manifest
//! before returning success; remote publication remains asynchronous. Uploading per
//! write would publish a torn object and cost a round trip per megabyte, so a
//! dirty file is written to a local staging file first and uploaded once the
//! client has gone quiet. Timing the upload is therefore entirely our problem,
//! and the policy that decides it lives here as pure functions.
//!
//! Nothing is ever buffered in memory: the staging file is the buffer, and the
//! upload streams from it.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::providers::resources::{ByteLease, DiskLease, ResourceKind};

use super::{stage_commit, stage_wal};

/// How long a file must go untouched before it is uploaded. Long enough to span
/// the gap between the WRITEs of one copy, short enough that a user who drops a
/// file in and looks at the bucket sees it there.
pub const FLUSH_DEBOUNCE: Duration = Duration::from_secs(2);
/// How often the flusher looks for work.
pub const FLUSH_SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// How long a failed upload is left alone before the scanner tries again.
pub const FLUSH_RETRY_COOLDOWN: Duration = Duration::from_secs(15);
/// Uploads in flight at once for one mount.
pub const MAX_CONCURRENT_FLUSHES: usize = 3;
/// Attempts inside a single flush before it is marked failed.
pub const UPLOAD_ATTEMPTS: u32 = 3;
/// Largest object that will be pulled down to be modified in place.
///
/// A write to the middle of an existing object has to become a full rewrite —
/// S3 has no partial update — so the whole object is staged first. Past this
/// size the download costs more than the edit is worth and the write is refused
/// rather than silently taking minutes.
pub const RMW_DOWNLOAD_CAP: u64 = 8 * 1024 * 1024 * 1024;

/// Upload thresholds, matching the app's own upload path so a file written
/// through a mount is chunked exactly like one uploaded from the file list.
pub const MULTIPART_THRESHOLD: u64 = 100 * 1024 * 1024;
pub const PART_SIZE: u64 = 20 * 1024 * 1024;
/// Parts uploaded concurrently within one multipart flush.
pub const PART_CONCURRENCY: usize = 4;
const CHECKPOINT_WAL_BYTES: u64 = 32 * 1024 * 1024;
const CHECKPOINT_RECORDS: u64 = 256;

/// Where a flush is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushState {
    Idle,
    Uploading,
    /// Requires explicit recovery after a permanent or uncertain failure.
    Paused,
    /// The last upload failed; the scanner leaves this stage alone until the
    /// cooldown expires so a broken bucket cannot spin the flusher.
    Failed {
        retry_after: Instant,
    },
}

/// Whether the scanner should start uploading a stage in this state.
///
/// Kept separate from [`Stage`] so the policy is testable without touching the
/// filesystem.
pub fn should_flush(
    dirty: bool,
    flush_requested: bool,
    state: FlushState,
    idle_for: Duration,
    now: Instant,
) -> bool {
    if !dirty {
        return false;
    }
    match state {
        FlushState::Uploading | FlushState::Paused => false,
        FlushState::Failed { retry_after } if now < retry_after => false,
        _ => flush_requested || idle_for >= FLUSH_DEBOUNCE,
    }
}

/// Whether a stage may be dropped from the registry.
///
/// Only a stage whose content is already in the bucket and that has no upload
/// in flight can go. Dropping any other would delete the only copy of a write:
/// the staging file is not a cache of the object, it *is* the object until the
/// upload succeeds.
pub fn can_evict(dirty: bool, state: FlushState) -> bool {
    !dirty && state == FlushState::Idle
}

/// Whether a finished upload leaves the stage clean.
///
/// A write that landed while the upload was in flight advanced the generation
/// counter, which means the object now in the bucket is already stale. The
/// stage stays dirty and the next tick uploads it again.
pub fn upload_settles_stage(uploaded_gen: u64, current_gen: u64) -> bool {
    uploaded_gen == current_gen
}

/// What has to happen before a write can land in a new stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageInit {
    /// Nothing to preserve — start from an empty file.
    Empty,
    /// The object has content a partial write must not destroy, so it is
    /// downloaded first (read-modify-write).
    Download,
    /// Too large to stage locally.
    TooLarge,
}

/// How a stage for an object of `object_size` bytes has to be primed.
pub fn stage_init(object_size: u64) -> StageInit {
    if object_size == 0 {
        StageInit::Empty
    } else if object_size > RMW_DOWNLOAD_CAP {
        StageInit::TooLarge
    } else {
        StageInit::Download
    }
}

/// Number of parts a multipart upload of `size` bytes is split into.
pub fn planned_part_size(size: u64) -> u64 {
    // Round up to a MiB while staying within S3's 10,000 part limit.
    let mib = 1024 * 1024;
    PART_SIZE.max(size.div_ceil(10_000).div_ceil(mib) * mib)
}

pub fn part_count(size: u64) -> u64 {
    size.div_ceil(planned_part_size(size)).max(1)
}

/// Byte range of part `index` (zero-based) of a `size`-byte upload.
pub fn part_range(size: u64, index: u64) -> (u64, u64) {
    let part_size = planned_part_size(size);
    let start = index.saturating_mul(part_size);
    let end = start.saturating_add(part_size).min(size);
    (start, end.saturating_sub(start))
}

/// One dirty file, backed by a real file on disk.
///
/// Every operation on a given file is serialized by the mutex the filesystem
/// keeps this behind, so the open handle needs no further synchronisation.
pub struct Stage {
    /// Kept open for the life of the stage: a copy arrives as hundreds of
    /// sequential writes and reopening per write would dominate the cost.
    file: File,
    path: PathBuf,
    stage_lease: ByteLease,
    snapshot_lease: ByteLease,
    wal_lease: ByteLease,
    wal_tail_repaired: bool,
    /// Key this stage belongs to, so the flusher does not have to re-resolve
    /// the inode — and stays correct if the inode is re-keyed by a rename.
    pub key: String,
    pub size: u64,
    pub mtime_secs: u32,
    pub dirty: bool,
    /// Bumped by every write. Captured before an upload so a write that lands
    /// mid-upload is noticed instead of being lost.
    pub dirty_gen: u64,
    applied_gen: u64,
    /// Set just before a WAL append and cleared once that record is committed
    /// and applied. While set, the WAL may hold a record the data file does
    /// not reflect — a failed commit or apply, or a cancelled request — and
    /// the next access replays it. Otherwise nothing is ever replayed outside
    /// restore, so a write or read costs O(record), not a pass over the WAL.
    needs_replay: bool,
    /// Set when an fsync of the data file failed. Linux can mark the pages a
    /// failed write-back dropped as clean, so a later fsync of the same file
    /// may succeed without them: until the WAL above the last manifest has
    /// been re-applied through a fresh handle, nothing is acknowledged, no
    /// manifest is written and no read is served from this file.
    data_unsynced: bool,
    /// Generation and checkpoint LSN of the last manifest written — the last
    /// state an fsync proved. Recovery replays the WAL above them, and so does
    /// clearing `data_unsynced`.
    manifest_gen: u64,
    manifest_checkpoint: u64,
    /// False while content written outside the WAL (the pre-edit download of
    /// a read-modify-write) has not yet been proven durable by a manifest.
    /// The WAL cannot rebuild that content, so a failed fsync then is final.
    base_proven: bool,
    pub checkpoint_lsn: u64,
    pub next_lsn: u64,
    records_since_checkpoint: u64,
    bytes_since_checkpoint: u64,
    pub first_dirty_at: Option<i64>,
    pub last_write: Instant,
    /// Set by the `utimes` a client sends at the end of a copy — the closest
    /// thing NFSv3 offers to a close notification — to skip the debounce.
    pub flush_requested: bool,
    /// Staged size the last queued-transfer event reported, so the progress
    /// row's total can follow a growing copy without an event per write.
    pub reported_size: u64,
    pub state: FlushState,
    /// Tombstone: this stage has been unpublished and its backing file deleted.
    ///
    /// A task can be holding a handle from before the stage was dropped, and
    /// writing into a file that no longer exists would silently lose the write.
    /// Whoever unpublishes a stage sets this under the stage's own lock and
    /// removes the map entry under the same map lock, so anyone who locks a
    /// stage and finds it set knows the handle is stale and the map is the
    /// place to look again.
    pub evicted: bool,
    pub last_error: Option<String>,
    pub snapshot: Option<UploadSnapshot>,
    pub publication_guard: Option<PublicationGuard>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PublicationGuard {
    Absent,
    Match { etag: String },
}
impl PublicationGuard {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Absent => true,
            Self::Match { etag } => !etag.trim().is_empty(),
        }
    }
    pub fn matches(&self, etag: Option<&str>) -> bool {
        match self {
            Self::Absent => etag.is_none(),
            Self::Match { etag: expected } => self.is_valid() && etag == Some(expected.as_str()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadSnapshot {
    pub path: PathBuf,
    pub generation: u64,
    pub size: u64,
    #[serde(default)]
    pub publication_guard: Option<PublicationGuard>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MultipartJournal {
    pub upload_id: Option<String>,
    pub part_size: u64,
    pub parts: std::collections::BTreeMap<i32, String>,
    pub completing: bool,
    #[serde(default)]
    pub precondition: Option<PublicationGuard>,
    #[serde(default)]
    pub published_etag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRecovery {
    pub key: String,
    pub size: u64,
    pub mtime_secs: u32,
    pub generation: u64,
    pub dirty: bool,
    pub state: String,
    pub error: Option<String>,
    pub path: PathBuf,
    pub snapshot: Option<UploadSnapshot>,
    #[serde(default)]
    pub publication_guard: Option<PublicationGuard>,
    #[serde(default)]
    pub checkpoint_lsn: u64,
    #[serde(default)]
    pub first_dirty_at: Option<i64>,
    #[serde(skip)]
    pub wal_bytes: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
enum DurableChange {
    Write { offset: u64, data: Vec<u8> },
    Resize { size: u64 },
}

#[derive(Serialize, Deserialize)]
struct WriteIntent {
    state: StageRecovery,
    change: DurableChange,
}

async fn replay_write(root: &Path, intent_path: &Path) -> std::io::Result<StageRecovery> {
    let metadata = tokio::fs::symlink_metadata(intent_path).await?;
    // A single NFS WRITE is bounded to 1 MiB; JSON byte arrays need at most
    // four bytes per byte plus a small identity record.
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 8 * 1024 * 1024
    {
        return Err(std::io::Error::other("Invalid durable write intent"));
    }
    let mut intent: WriteIntent = serde_json::from_slice(&tokio::fs::read(intent_path).await?)
        .map_err(std::io::Error::other)?;
    let name = intent
        .state
        .path
        .file_name()
        .ok_or_else(|| std::io::Error::other("Missing stage identity"))?;
    intent.state.path = root.join(name);
    let metadata = tokio::fs::symlink_metadata(&intent.state.path).await?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("Invalid stage data file"));
    }
    // Rewriting the manifest from the intent would lift a quarantine.
    let manifest_path = intent.state.path.with_extension("stage.json");
    if let Ok(bytes) = tokio::fs::read(&manifest_path).await {
        if serde_json::from_slice::<StageRecovery>(&bytes)
            .is_ok_and(|manifest| manifest.state == "unreadable")
        {
            return Err(std::io::Error::other(
                "This file is quarantined for review; its interrupted write is kept",
            ));
        }
    }
    let mut file = OpenOptions::from(stage_commit::sync_open_options())
        .open(&intent.state.path)
        .await?;
    match intent.change {
        DurableChange::Write { offset, data } => {
            file.seek(SeekFrom::Start(offset)).await?;
            file.write_all(&data).await?;
        }
        DurableChange::Resize { size } => file.set_len(size).await?,
    }
    file.flush().await?;
    file.sync_all().await?;
    stage_commit::record_file_sync_bytes(intent.state.size);
    if file.metadata().await?.len() != intent.state.size {
        return Err(std::io::Error::other("Replayed write size mismatch"));
    }
    write_json_atomic(
        &intent.state.path.with_extension("stage.json"),
        &intent.state,
    )
    .await?;
    tokio::fs::remove_file(intent_path).await?;
    sync_parent(intent_path).await?;
    Ok(intent.state)
}

/// Atomic replacement + file and directory sync: a successful FILE_SYNC NFS
/// WRITE must survive process restart with both content and its object mapping.
pub async fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary).await?;
    file.write_all(&bytes).await?;
    file.sync_all().await?;
    stage_commit::record_file_sync_bytes(bytes.len() as u64);
    drop(file);
    tokio::fs::rename(&temporary, path).await?;
    sync_parent(path).await
}

/// Makes the directory entry for `path` durable. A no-op on Windows: NTFS
/// journals directory changes itself, and std cannot open a directory
/// handle there to flush it.
pub async fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent).await?.sync_all().await?;
        stage_commit::record_parent_sync();
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub async fn replay_write_intents(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let mut errors = stage_wal::replay_all(root).await?;
    let mut dir = tokio::fs::read_dir(root).await.map_err(|e| e.to_string())?;
    while let Some(entry) = dir.next_entry().await.map_err(|e| e.to_string())? {
        if entry.file_name().to_string_lossy().ends_with(".write.json") {
            if let Err(error) = replay_write(root, &entry.path()).await {
                let path = entry.path();
                // A WAL replay error for the same data file uses this key
                // too; restore keeps one message per key, so keep both.
                match errors.iter_mut().find(|(existing, _)| *existing == path) {
                    Some((_, message)) => {
                        message.push_str("; ");
                        message.push_str(&error.to_string());
                    }
                    None => errors.push((path, error.to_string())),
                }
            }
        }
    }
    Ok(errors)
}

pub fn unreadable_record(path: PathBuf, key: String, error: String) -> StageRecovery {
    StageRecovery {
        key,
        size: 0,
        mtime_secs: 0,
        generation: 0,
        dirty: false,
        state: "unreadable".into(),
        error: Some(error),
        path,
        snapshot: None,
        publication_guard: None,
        checkpoint_lsn: 0,
        first_dirty_at: None,
        wal_bytes: None,
    }
}

/// A stage known only from its WAL records, which were not applied. Nothing
/// about it is claimed beyond its key; restore quarantines it.
fn pending_replay_record(path: PathBuf, key: String) -> StageRecovery {
    StageRecovery {
        dirty: true,
        state: "replay_pending".into(),
        error: None,
        ..unreadable_record(path, key, String::new())
    }
}

async fn read_recovery_record(
    root: &Path,
    path: &Path,
    write: bool,
) -> Result<StageRecovery, String> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|e| e.to_string())?;
    let limit = if write { 8 * 1024 * 1024 } else { 1024 * 1024 };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err("Invalid recovery record file".into());
    }
    let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
    let mut record = if write {
        serde_json::from_slice::<WriteIntent>(&bytes)
            .map_err(|e| e.to_string())?
            .state
    } else {
        serde_json::from_slice::<StageRecovery>(&bytes).map_err(|e| e.to_string())?
    };
    if record.key.is_empty() {
        return Err("Recovery record has no object key".into());
    }
    record.path = root.join(record.path.file_name().ok_or("Stage data path missing")?);
    if write {
        let metadata = tokio::fs::symlink_metadata(&record.path)
            .await
            .map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Invalid stage data file".into());
        }
        record.state = "replay_pending".into();
    }
    Ok(record)
}

pub async fn recovery_entries(root: &Path) -> Result<Vec<StageRecovery>, String> {
    let mut entries = Vec::new();
    let mut pending = std::collections::HashMap::new();
    let mut write_errors = std::collections::HashMap::new();
    let mut paths = Vec::new();
    let mut dir = match tokio::fs::read_dir(root).await {
        Ok(dir) => dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(error.to_string()),
    };
    while let Some(entry) = dir.next_entry().await.map_err(|e| e.to_string())? {
        paths.push(entry.path());
    }
    let wal_index = stage_wal::recovery_index(root).await?;
    let damage = wal_index.damage();
    if let Some(damage) = &damage {
        // Restore sets it aside; until then it is listed like the copies
        // below, so the folder is kept and the damage is visible.
        entries.push(unreadable_record(
            stage_wal::root_wal_path(root),
            String::new(),
            damage.clone(),
        ));
    }
    for path in paths.iter().filter(|path| {
        path.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with(stage_wal::DAMAGED_WAL_PREFIX)
        })
    }) {
        entries.push(unreadable_record(
            path.clone(),
            String::new(),
            "Damaged staging WAL kept for export and review".into(),
        ));
    }
    for path in paths
        .iter()
        .filter(|path| path.to_string_lossy().ends_with(".write.json"))
    {
        match read_recovery_record(root, path, true).await {
            Ok(record) => {
                pending.insert(record.path.clone(), record);
            }
            Err(error) => {
                if tokio::fs::try_exists(path).await.unwrap_or(true) {
                    let name = path
                        .file_name()
                        .ok_or("Recovery filename missing")?
                        .to_string_lossy();
                    let manifest = path.with_file_name(format!(
                        "{}.stage.json",
                        name.trim_end_matches(".write.json")
                    ));
                    write_errors.insert(manifest, (path.clone(), error));
                }
            }
        }
    }
    for path in paths
        .iter()
        .filter(|path| path.to_string_lossy().ends_with(".stage.json"))
    {
        let mut record = match read_recovery_record(root, path, false).await {
            Ok(record) => record,
            Err(error) => {
                if tokio::fs::try_exists(path).await.unwrap_or(true) {
                    entries.push(unreadable_record(path.clone(), String::new(), error));
                }
                continue;
            }
        };
        if let Some((_, error)) = write_errors.remove(path) {
            entries.push(unreadable_record(
                path.clone(),
                record.key,
                format!("Interrupted write is unreadable: {error}"),
            ));
            continue;
        }
        if let Some(record) = pending.remove(&record.path) {
            entries.push(record);
            continue;
        }
        if wal_index.is_discarded(&record.path) {
            // Deleted by the user; a crash interrupted removing its files.
            continue;
        }
        if record.state == "unreadable" {
            // Quarantined by an earlier recovery (a damaged WAL was set aside).
            record.dirty = true;
            entries.push(record);
            continue;
        }
        if let Some(damage) = &damage {
            if wal_index.is_affected(&record.path, record.checkpoint_lsn, record.generation) {
                entries.push(unreadable_record(path.clone(), record.key, damage.clone()));
                continue;
            }
        }
        record.wal_bytes = Some(
            wal_index
                .uncheckpointed_bytes_for_after(&record.path, record.checkpoint_lsn)
                .map_err(|e| e.to_string())?,
        );
        if let Some(summary) = wal_index
            .summary_for_after(&record.path, record.checkpoint_lsn, record.generation)
            .map_err(|e| e.to_string())?
        {
            if record.key != summary.record.key {
                entries.push(unreadable_record(
                    path.clone(),
                    record.key,
                    "WAL key does not match durable stage manifest".into(),
                ));
                continue;
            }
            // The WAL holds acknowledged records the data file may not have:
            // replay folds them into the manifest, so any still here were not
            // applied. Report the manifest as it is — advancing it would pass
            // old bytes off as the acknowledged content — and let restore
            // quarantine it with the replay error.
            record.state = "replay_pending".into();
            record.dirty = true;
            entries.push(record);
            continue;
        }
        if !record.dirty {
            continue;
        }
        let validation = async {
            validate_data_file(&record.path, record.size).await?;
            if let Some(snapshot) = &mut record.snapshot {
                snapshot.path = root.join(
                    snapshot
                        .path
                        .file_name()
                        .ok_or_else(|| std::io::Error::other("Snapshot identity missing"))?,
                );
                validate_data_file(&snapshot.path, snapshot.size).await?;
            }
            Ok::<(), std::io::Error>(())
        }
        .await;
        match validation {
            Ok(()) => entries.push(record),
            Err(error) => {
                if tokio::fs::try_exists(path).await.unwrap_or(true) {
                    entries.push(unreadable_record(
                        path.clone(),
                        record.key,
                        error.to_string(),
                    ));
                }
            }
        }
    }
    // Acknowledged records whose data file survived without a manifest: a
    // successful replay would have rebuilt the manifest, so they still wait.
    let listed: std::collections::HashSet<&PathBuf> = paths.iter().collect();
    for (name, key) in wal_index.stages() {
        let data_path = root.join(name);
        if listed.contains(&data_path)
            && !listed.contains(&data_path.with_extension("stage.json"))
            && !listed.contains(&data_path.with_extension("write.json"))
        {
            match &damage {
                Some(damage) if wal_index.is_affected(&data_path, 0, 0) => entries.push(
                    unreadable_record(data_path, key.to_string(), damage.clone()),
                ),
                _ => entries.push(pending_replay_record(data_path, key.to_string())),
            }
        }
    }
    entries.extend(pending.into_values());
    entries.extend(
        write_errors
            .into_values()
            .map(|(path, error)| unreadable_record(path, String::new(), error)),
    );
    Ok(entries)
}

async fn validate_data_file(path: &Path, expected_size: u64) -> std::io::Result<()> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != expected_size {
        return Err(std::io::Error::other(
            "Stage content does not match its durable manifest",
        ));
    }
    Ok(())
}

impl UploadSnapshot {
    pub fn token(&self) -> String {
        self.path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
    pub fn journal_path(&self) -> PathBuf {
        self.path.with_extension("upload.json")
    }
    pub async fn journal(&self) -> std::io::Result<MultipartJournal> {
        match tokio::fs::read(self.journal_path()).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MultipartJournal {
                part_size: planned_part_size(self.size),
                precondition: self.publication_guard.clone(),
                ..Default::default()
            }),
            Err(e) => Err(e),
        }
    }
    pub async fn save_journal(&self, journal: &MultipartJournal) -> std::io::Result<()> {
        write_json_atomic(&self.journal_path(), journal).await
    }
    pub async fn remove(&self) {
        let _ = tokio::fs::remove_file(&self.path).await;
        let _ = tokio::fs::remove_file(self.journal_path()).await;
    }
}

/// Test hook: `remove_files` of the stage at a data path stops dead after
/// that many of its steps, leaving the disk as a crash there would.
#[cfg(test)]
static REMOVE_CRASHES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, u32>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn crash_remove_after(path: &Path, steps: u32) {
    REMOVE_CRASHES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), steps);
}

#[cfg(test)]
fn remove_stops_after(path: &Path, step: u32) -> bool {
    REMOVE_CRASHES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(path)
        == Some(&step)
}

#[cfg(not(test))]
fn remove_stops_after(_path: &Path, _step: u32) -> bool {
    false
}

/// Test hook: the next durable change of the stage at a data path fails after
/// its WAL record is committed and before it reaches the data file.
#[cfg(test)]
static FAILING_APPLIES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
fn fail_next_apply(path: &Path) {
    FAILING_APPLIES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(path.to_path_buf());
}

#[cfg(test)]
fn apply_fails(path: &Path) -> bool {
    FAILING_APPLIES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(path)
}

/// Test hook: unlinking one of these paths fails.
#[cfg(test)]
static FAILING_REMOVALS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
fn fail_removal_of(path: &Path) {
    FAILING_REMOVALS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(path.to_path_buf());
}

/// Deletes `path`; true if it is gone afterwards, whether or not it existed.
async fn remove_if_present(path: &Path) -> bool {
    #[cfg(test)]
    if FAILING_REMOVALS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .contains(path)
    {
        return false;
    }
    match tokio::fs::remove_file(path).await {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

async fn copy_file_native_with_lease(
    source: PathBuf,
    destination: PathBuf,
    bytes: u64,
    lease: DiskLease,
) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        let result: std::io::Result<()> = (|| {
            std::fs::copy(&source, &destination)?;
            stage_commit::sync_open_options()
                .open(&destination)?
                .sync_all()?;
            Ok(())
        })();
        drop(lease);
        result
    })
    .await
    .map_err(std::io::Error::other)??;
    stage_commit::record_file_sync_bytes(bytes);
    Ok(())
}

impl Stage {
    /// Creates an empty staging file, replacing any leftover from a previous
    /// session.
    pub async fn create(path: PathBuf, key: String, mtime_secs: u32) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .await?;

        Ok(Self {
            file,
            path,
            stage_lease: ByteLease::new(ResourceKind::Stage, 0),
            snapshot_lease: ByteLease::new(ResourceKind::Snapshot, 0),
            wal_lease: ByteLease::new(ResourceKind::Wal, 0),
            wal_tail_repaired: true,
            key,
            size: 0,
            mtime_secs,
            dirty: false,
            dirty_gen: 0,
            applied_gen: 0,
            needs_replay: false,
            data_unsynced: false,
            manifest_gen: 0,
            manifest_checkpoint: 0,
            base_proven: true,
            checkpoint_lsn: 0,
            next_lsn: 1,
            records_since_checkpoint: 0,
            bytes_since_checkpoint: 0,
            first_dirty_at: None,
            last_write: Instant::now(),
            flush_requested: false,
            reported_size: 0,
            state: FlushState::Idle,
            evicted: false,
            last_error: None,
            snapshot: None,
            publication_guard: None,
        })
    }

    pub async fn restore(record: StageRecovery) -> std::io::Result<Self> {
        let file = OpenOptions::from(stage_commit::sync_open_options())
            .open(&record.path)
            .await?;
        if file.metadata().await?.len() != record.size {
            return Err(std::io::Error::other(
                "Stage size does not match its durable journal",
            ));
        }
        let wal_bytes = match record.wal_bytes {
            Some(bytes) => bytes,
            None => stage_wal::uncheckpointed_bytes_for(&record.path, record.checkpoint_lsn)
                .await
                .unwrap_or(0),
        };
        let snapshot_bytes = record
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.size)
            .unwrap_or(0);
        Ok(Self {
            file,
            path: record.path,
            stage_lease: ByteLease::new(ResourceKind::Stage, record.size),
            snapshot_lease: ByteLease::new(ResourceKind::Snapshot, snapshot_bytes),
            wal_lease: ByteLease::new(ResourceKind::Wal, wal_bytes),
            wal_tail_repaired: false,
            key: record.key,
            size: record.size,
            mtime_secs: record.mtime_secs,
            dirty: record.dirty,
            dirty_gen: record.generation,
            applied_gen: record.generation,
            // Restore replays the whole folder before any stage is rebuilt, and
            // recovery_entries hands over only stages with nothing left to apply.
            needs_replay: false,
            data_unsynced: false,
            manifest_gen: record.generation,
            manifest_checkpoint: record.checkpoint_lsn,
            base_proven: true,
            checkpoint_lsn: record.checkpoint_lsn,
            next_lsn: record.checkpoint_lsn.saturating_add(1),
            records_since_checkpoint: 0,
            bytes_since_checkpoint: 0,
            first_dirty_at: record.first_dirty_at,
            last_write: Instant::now(),
            flush_requested: true,
            reported_size: record.size,
            // This path is an explicit user recovery request. Retry repaired
            // credentials, but never release a pending rename target fence.
            state: if record.error.as_deref()
                == Some("Destination replacement pending; retained for recovery")
            {
                FlushState::Paused
            } else {
                FlushState::Idle
            },
            evicted: false,
            last_error: record.error,
            snapshot: record.snapshot,
            publication_guard: record.publication_guard,
        })
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.path.with_extension("stage.json")
    }

    /// Fsyncs the data file through the handle every write went through. A
    /// failure is never dropped: it marks the file unsynced (see
    /// `data_unsynced`) as well as being returned.
    async fn sync_data(&mut self) -> std::io::Result<()> {
        let result = async {
            stage_commit::injected_sync_failure(&self.path)?;
            self.file.sync_all().await
        }
        .await;
        match result {
            Ok(()) => {
                stage_commit::record_file_sync_bytes(self.size);
                Ok(())
            }
            Err(error) => {
                self.data_unsynced = true;
                Err(error)
            }
        }
    }

    pub async fn persist(&mut self) -> std::io::Result<()> {
        // The manifest must never claim a record the data file lacks: bring
        // the file level with the WAL first, then prove it with an fsync.
        self.replay_pending_write().await?;
        self.file.flush().await?;
        self.sync_data().await?;
        let state = match self.state {
            FlushState::Uploading => "uploading",
            FlushState::Paused => "paused",
            FlushState::Failed { .. } => "failed",
            FlushState::Idle => "waiting",
        };
        write_json_atomic(
            &self.manifest_path(),
            &StageRecovery {
                key: self.key.clone(),
                size: self.size,
                mtime_secs: self.mtime_secs,
                generation: self.dirty_gen,
                dirty: self.dirty,
                state: state.to_string(),
                error: self.last_error.clone(),
                path: self.path.clone(),
                snapshot: self.snapshot.clone(),
                publication_guard: self.publication_guard.clone(),
                checkpoint_lsn: self.checkpoint_lsn,
                first_dirty_at: self.first_dirty_at,
                wal_bytes: None,
            },
        )
        .await?;
        self.manifest_gen = self.dirty_gen;
        self.manifest_checkpoint = self.checkpoint_lsn;
        self.base_proven = true;
        Ok(())
    }

    /// Brings the data file level with the WAL after an interrupted change or
    /// a failed data-file fsync.
    ///
    /// O(1) unless one of those happened: restore already replayed the folder,
    /// and a change that completed applied its own record.
    pub async fn replay_pending_write(&mut self) -> std::io::Result<()> {
        if !self.needs_replay && !self.data_unsynced {
            return Ok(());
        }
        // The interrupted apply may still have a write in flight on this
        // handle: let it land, and learn whether a write-back through this
        // handle failed before it is swapped for a fresh one below.
        if self.data_unsynced {
            // Proven or not, a write still in flight must land before the
            // re-apply below rewrites those bytes through another handle.
            let _ = self.file.flush().await;
        } else {
            let _ = self.sync_data().await;
        }
        if self.data_unsynced && !self.base_proven {
            return Err(std::io::Error::other(
                "Staged content that is not in the WAL could not be made durable",
            ));
        }
        let path = self.path.with_extension("write.json");
        if tokio::fs::try_exists(&path).await? {
            let record = replay_write(
                self.path
                    .parent()
                    .ok_or_else(|| std::io::Error::other("Missing staging folder"))?,
                &path,
            )
            .await?;
            self.size = record.size;
            self.stage_lease.resize(self.size);
            self.dirty_gen = record.generation;
            self.applied_gen = record.generation;
            self.checkpoint_lsn = record.checkpoint_lsn;
            self.next_lsn = self.checkpoint_lsn.saturating_add(1);
            self.records_since_checkpoint = 0;
            self.bytes_since_checkpoint = 0;
            self.dirty = record.dirty;
            self.mtime_secs = record.mtime_secs;
            self.publication_guard = record.publication_guard;
            self.first_dirty_at = record.first_dirty_at;
            self.last_write = Instant::now();
            self.manifest_gen = record.generation;
            self.manifest_checkpoint = record.checkpoint_lsn;
        }
        let (checkpoint_floor, generation_floor) = if self.data_unsynced {
            // Pages written since the last proven manifest may be gone: apply
            // every record above it again, as crash recovery would.
            (self.manifest_checkpoint, self.manifest_gen)
        } else {
            (self.checkpoint_lsn, self.applied_gen)
        };
        if let Some(summary) =
            stage_wal::replay_file_after_generation(&self.path, checkpoint_floor, generation_floor)
                .await?
        {
            if self.key != summary.record.key {
                return Err(std::io::Error::other(
                    "WAL key does not match durable stage manifest",
                ));
            }
            self.key = summary.record.key;
            self.size = summary.record.resulting_size;
            self.stage_lease.resize(self.size);
            self.dirty_gen = summary.record.generation;
            self.applied_gen = summary.record.generation;
            self.next_lsn = summary.next_lsn;
            self.records_since_checkpoint = 0;
            self.bytes_since_checkpoint = 0;
            self.dirty = true;
            self.mtime_secs = summary.record.mtime_secs;
            self.first_dirty_at
                .get_or_insert(summary.record.dirty_at_ms);
            self.last_write = Instant::now();
        }
        // An unacknowledged record may have been dropped as a torn tail or by
        // a rewrite, so recount what this stage really owns in the WAL.
        self.refresh_wal_lease().await;
        self.file = OpenOptions::from(stage_commit::sync_open_options())
            .open(&self.path)
            .await?;
        self.needs_replay = false;
        // Everything up to the manifest was proven by its fsync, and every
        // record above it has just been written and fsynced through handles
        // that never saw the failure.
        self.data_unsynced = false;
        Ok(())
    }

    async fn durable_change(
        &mut self,
        change: DurableChange,
        size: u64,
        mtime: u32,
    ) -> std::io::Result<()> {
        self.replay_pending_write().await?;
        if self.checkpoint_lsn == 0 && self.dirty_gen == 0 {
            self.write_recovery_manifest().await?;
        }
        let generation = self.dirty_gen.saturating_add(1);
        let lsn = self.next_lsn;
        let (op, offset, payload) = match &change {
            DurableChange::Write { offset, data } => {
                (stage_wal::WalOp::Write, *offset, data.clone())
            }
            DurableChange::Resize { .. } => (stage_wal::WalOp::Truncate, 0, Vec::new()),
        };
        let wal_path = stage_wal::wal_path(&self.path);
        // Held until this record's commit returns; refused while an earlier
        // fsync failure of the WAL has not been repaired by a rewrite.
        let admitted = stage_wal::begin_append(&wal_path).await?;
        if !self.wal_tail_repaired {
            stage_wal::repair_tail(&wal_path).await?;
            self.wal_tail_repaired = true;
        }
        let dirty_at_ms = self
            .first_dirty_at
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        let record = stage_wal::WalRecord {
            lsn,
            generation,
            op,
            offset,
            resulting_size: size,
            mtime_secs: mtime,
            dirty_at_ms,
            data_name: stage_wal::data_name(&self.path)?,
            key: self.key.clone(),
            payload,
        };
        let wal_parent = wal_path
            .parent()
            .ok_or_else(|| std::io::Error::other("Missing WAL folder"))?;
        let wal_record_bytes = stage_wal::estimated_record_len(&record)?;
        let wal_growth = DiskLease::reserve(wal_parent, wal_record_bytes, || {
            super::available_space(wal_parent)
        })?;
        let data_growth = match record.op {
            stage_wal::WalOp::Write => record.payload.len() as u64,
            stage_wal::WalOp::Truncate | stage_wal::WalOp::Discard => {
                size.saturating_sub(self.size)
            }
        };
        let data_growth = if data_growth > 0 {
            Some(DiskLease::reserve(&self.path, data_growth, || {
                super::available_space(&self.path)
            })?)
        } else {
            None
        };

        self.needs_replay = true;
        self.wal_tail_repaired = false;
        match stage_wal::append_record_unchecked(&wal_path, &record).await {
            Ok(assigned_lsn) => {
                self.wal_tail_repaired = true;
                self.wal_lease
                    .resize(self.wal_lease.bytes().saturating_add(wal_record_bytes));
                stage_commit::commit(vec![wal_path.clone()]).await?;
                stage_wal::note_committed(&wal_path, assigned_lsn).await;
                drop(admitted);
                drop(wal_growth);
                self.next_lsn = assigned_lsn.saturating_add(1);
                self.records_since_checkpoint = self.records_since_checkpoint.saturating_add(1);
                self.bytes_since_checkpoint =
                    self.bytes_since_checkpoint.saturating_add(wal_record_bytes);
            }
            Err(error) => return Err(error),
        }
        // Once the WAL record is durable, an older in-flight upload must no
        // longer be allowed to mark this stage clean, even if data I/O now
        // fails. Recovery will replay the WAL record.
        self.dirty = true;
        self.dirty_gen = generation;
        self.first_dirty_at.get_or_insert(dirty_at_ms);
        self.last_write = Instant::now();
        #[cfg(test)]
        if apply_fails(&self.path) {
            return Err(std::io::Error::other("injected apply failure"));
        }
        match change {
            DurableChange::Write { offset, data } => {
                self.file.seek(SeekFrom::Start(offset)).await?;
                self.file.write_all(&data).await?;
                self.file.set_len(size).await?;
            }
            DurableChange::Resize { size } => self.file.set_len(size).await?,
        }
        self.file.flush().await?;
        self.needs_replay = false;
        drop(data_growth);
        self.size = size;
        self.stage_lease.resize(size);
        debug_assert_eq!(self.stage_lease.bytes(), self.size);
        self.mtime_secs = mtime;
        self.applied_gen = generation;
        if self.should_checkpoint().await {
            self.checkpoint_durable().await?;
        }
        Ok(())
    }

    pub async fn write_durable(
        &mut self,
        offset: u64,
        data: &[u8],
        mtime: u32,
    ) -> std::io::Result<()> {
        if data.len() > 1024 * 1024 {
            return Err(std::io::Error::other(
                "NFS write exceeds the advertised request limit",
            ));
        }
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| std::io::Error::other("Write offset overflow"))?;
        self.durable_change(
            DurableChange::Write {
                offset,
                data: data.to_vec(),
            },
            self.size.max(end),
            mtime,
        )
        .await
    }

    async fn write_recovery_manifest(&mut self) -> std::io::Result<()> {
        self.sync_data().await?;
        let state = match self.state {
            FlushState::Uploading => "uploading",
            FlushState::Paused => "paused",
            FlushState::Failed { .. } => "failed",
            FlushState::Idle => "waiting",
        };
        write_json_atomic(
            &self.manifest_path(),
            &StageRecovery {
                key: self.key.clone(),
                size: self.size,
                mtime_secs: self.mtime_secs,
                generation: self.dirty_gen,
                dirty: self.dirty,
                state: state.to_string(),
                error: self.last_error.clone(),
                path: self.path.clone(),
                snapshot: self.snapshot.clone(),
                publication_guard: self.publication_guard.clone(),
                checkpoint_lsn: self.checkpoint_lsn,
                first_dirty_at: self.first_dirty_at,
                wal_bytes: None,
            },
        )
        .await?;
        self.manifest_gen = self.dirty_gen;
        self.manifest_checkpoint = self.checkpoint_lsn;
        self.base_proven = true;
        Ok(())
    }

    pub async fn truncate_durable(&mut self, size: u64, mtime: u32) -> std::io::Result<()> {
        self.durable_change(DurableChange::Resize { size }, size, mtime)
            .await
    }

    /// Called with this stage locked, never under the stage-registry lock.
    /// A separate inode prevents subsequent write/truncate from changing an
    /// in-flight request body or a resumed multipart part.
    pub async fn upload_snapshot(&mut self) -> std::io::Result<UploadSnapshot> {
        self.replay_pending_write().await?;
        if let Some(snapshot) = &self.snapshot {
            return Ok(snapshot.clone());
        }
        self.checkpoint_durable().await?;
        let path = self
            .path
            .with_extension(format!("g{}.snapshot", self.dirty_gen));
        let snapshot_parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("Missing snapshot folder"))?;
        let snapshot_growth = DiskLease::reserve(snapshot_parent, self.size, || {
            super::available_space(snapshot_parent)
        })?;
        copy_file_native_with_lease(self.path.clone(), path.clone(), self.size, snapshot_growth)
            .await?;
        let snapshot = UploadSnapshot {
            path,
            generation: self.dirty_gen,
            size: self.size,
            publication_guard: self.publication_guard.clone(),
        };
        self.snapshot = Some(snapshot.clone());
        self.snapshot_lease.resize(snapshot.size);
        self.persist().await?;
        Ok(snapshot)
    }

    /// Deletes the stage for good: REMOVE, a truncating CREATE, a rename over
    /// it, a failed prime, or eviction once uploaded.
    ///
    /// Ordered so that a crash at any point never brings the file back. A
    /// stage with content the bucket may lack, or a WAL record not yet
    /// applied, first gets a durable discard record: from then on recovery
    /// neither restores it nor rebuilds its manifest from the WAL. The data
    /// file goes before the manifest, so even without a discard a crash
    /// leaves nothing to replay into. The stage's WAL records are not touched
    /// here — the caller may hold the mount's stage lock, and a discard makes
    /// them inert — they are handed to the WAL's dead-byte account, which
    /// compacts them away in the background once enough have piled up.
    ///
    /// For callers that cannot report a failure (eviction of an uploaded
    /// stage): the durable removal of `try_remove_files` first, then, if that
    /// cannot record the removal, whatever can still be done safely.
    pub async fn remove_files(&mut self) {
        let Err(error) = self.try_remove_files().await else {
            return;
        };
        log::warn!(
            "mount: could not record the removal of \"{}\" in the staging WAL: {}",
            self.key,
            error
        );
        if let Err(error) = self.remove_files_inner(true).await {
            log::error!(
                "mount: the staged copy of \"{}\" could not be deleted durably: {}",
                self.key,
                error
            );
        }
    }

    /// `remove_files` for a REMOVE, a truncating CREATE or a rename over the
    /// file: it succeeds only once the deletion is durable — a discard record
    /// committed for content the bucket may lack — so the client is only
    /// acknowledged then. On error nothing was removed and the stage is whole.
    pub async fn try_remove_files(&mut self) -> std::io::Result<()> {
        self.remove_files_inner(false).await
    }

    async fn remove_files_inner(&mut self, best_effort: bool) -> std::io::Result<()> {
        let wal_path = stage_wal::wal_path(&self.path);
        let mut dead = self.wal_lease.bytes();
        let needs_discard = self.dirty || self.needs_replay;
        let mut discarded = false;
        if needs_discard {
            match self.append_discard().await {
                Ok(bytes) => {
                    dead = dead.saturating_add(bytes);
                    discarded = true;
                }
                Err(error) if !best_effort => return Err(error),
                Err(error) => log::warn!(
                    "mount: could not record the removal of \"{}\" in the staging WAL: {}",
                    self.key,
                    error
                ),
            }
        }
        if remove_stops_after(&self.path, 1) {
            return Ok(());
        }
        // The data file goes first: it is what the WAL would replay into. If
        // it stays, so does everything else, so a crash finds the stage whole
        // rather than half deleted and rebuildable from the WAL.
        if !remove_if_present(&self.path).await {
            if needs_discard && !discarded {
                return Err(std::io::Error::other(
                    "The staged copy could not be deleted, nor its removal recorded",
                ));
            }
            // Recorded as discarded, or already in the bucket: the leftover
            // cannot come back, it is merely garbage in the staging folder.
            return Ok(());
        }
        let _ = remove_if_present(&self.path.with_extension("write.json")).await;
        if remove_stops_after(&self.path, 2) {
            return Ok(());
        }
        if let Some(snapshot) = &self.snapshot {
            snapshot.remove().await;
        }
        let _ = remove_if_present(&self.manifest_path()).await;
        if remove_stops_after(&self.path, 3) {
            return Ok(());
        }
        let synced = sync_parent(&self.path).await;
        if remove_stops_after(&self.path, 4) {
            return Ok(());
        }
        if needs_discard && !discarded {
            // Only best effort gets here: without a discard, the data file's
            // unlink is the deletion, and it is only durable once synced.
            synced?;
        }
        self.stage_lease.resize(0);
        self.snapshot_lease.resize(0);
        self.wal_lease.resize(0);
        if dead > 0 {
            stage_wal::note_reclaimable(&wal_path, dead).await;
        }
        Ok(())
    }

    /// Makes "this stage was deleted" durable in the WAL before any of its
    /// files go; returns the record's size.
    async fn append_discard(&mut self) -> std::io::Result<u64> {
        let wal_path = stage_wal::wal_path(&self.path);
        let admitted = stage_wal::begin_append(&wal_path).await?;
        let record = stage_wal::WalRecord {
            lsn: self.next_lsn,
            generation: self.dirty_gen,
            op: stage_wal::WalOp::Discard,
            offset: 0,
            resulting_size: 0,
            mtime_secs: self.mtime_secs,
            dirty_at_ms: 0,
            data_name: stage_wal::data_name(&self.path)?,
            key: self.key.clone(),
            payload: Vec::new(),
        };
        let wal_parent = wal_path
            .parent()
            .ok_or_else(|| std::io::Error::other("Missing WAL folder"))?;
        let bytes = stage_wal::estimated_record_len(&record)?;
        let _growth = DiskLease::reserve(wal_parent, bytes, || super::available_space(wal_parent))?;
        let lsn = stage_wal::append_record_unchecked(&wal_path, &record).await?;
        stage_commit::commit(vec![wal_path.clone()]).await?;
        stage_wal::note_committed(&wal_path, lsn).await;
        drop(admitted);
        Ok(bytes)
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes at an absolute offset, extending the file if needed.
    ///
    /// The flush is not optional: the uploader opens the same path through a
    /// separate handle, so buffered bytes it cannot see would upload as a hole.
    pub async fn write_at(&mut self, offset: u64, data: &[u8]) -> std::io::Result<()> {
        // Not in the WAL: only a manifest written after an fsync proves it.
        self.base_proven = false;
        self.file.seek(SeekFrom::Start(offset)).await?;
        self.file.write_all(data).await?;
        self.file.flush().await?;
        self.size = self.size.max(offset.saturating_add(data.len() as u64));
        self.stage_lease.resize(self.size);
        Ok(())
    }

    /// Reads up to `count` bytes at `offset`, stopping at end of file.
    pub async fn read_at(&mut self, offset: u64, count: usize) -> std::io::Result<Vec<u8>> {
        self.replay_pending_write().await?;
        if offset >= self.size || count == 0 {
            return Ok(Vec::new());
        }
        let available = (self.size - offset).min(count as u64) as usize;
        let mut buffer = vec![0u8; available];

        self.file.seek(SeekFrom::Start(offset)).await?;
        let mut filled = 0;
        while filled < available {
            let read = self.file.read(&mut buffer[filled..]).await?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        buffer.truncate(filled);
        Ok(buffer)
    }

    #[cfg(test)]
    pub async fn truncate(&mut self, size: u64) -> std::io::Result<()> {
        self.file.set_len(size).await?;
        self.file.flush().await?;
        self.size = size;
        self.stage_lease.resize(size);
        Ok(())
    }

    /// Records that the content changed and restarts the debounce window.
    #[cfg(test)]
    pub fn mark_dirty(&mut self, mtime_secs: u32) {
        self.dirty = true;
        self.dirty_gen = self.dirty_gen.wrapping_add(1);
        self.last_write = Instant::now();
        self.mtime_secs = mtime_secs;
        // More content means the copy is not over after all.
        self.flush_requested = false;
    }

    pub fn is_due(&self, now: Instant) -> bool {
        should_flush(
            self.dirty,
            self.flush_requested,
            self.state,
            now.saturating_duration_since(self.last_write),
            now,
        )
    }

    pub async fn reservation_bytes(&self) -> u64 {
        let snapshot = self.snapshot.as_ref().map(|s| s.size).unwrap_or(0);
        self.size.saturating_add(snapshot)
    }

    pub fn release_snapshot_accounting(&mut self) {
        self.snapshot_lease.resize(0);
    }

    async fn should_checkpoint(&self) -> bool {
        self.records_since_checkpoint >= CHECKPOINT_RECORDS
            || self.bytes_since_checkpoint >= CHECKPOINT_WAL_BYTES
    }

    async fn checkpoint_durable(&mut self) -> std::io::Result<()> {
        self.replay_pending_write().await?;
        let previous = self.checkpoint_lsn;
        self.checkpoint_lsn = self.next_lsn.saturating_sub(1);
        // persist proves the data file with an fsync before the manifest
        // records the new checkpoint; no record becomes reclaimable unless
        // it did.
        if let Err(error) = self.persist().await {
            self.checkpoint_lsn = previous;
            return Err(error);
        }
        // O(1): the records stay in the WAL until a threshold compaction,
        // which runs in the background, never on an upload or a write.
        let reclaimed = self.wal_lease.bytes();
        self.records_since_checkpoint = 0;
        self.bytes_since_checkpoint = 0;
        self.wal_lease.resize(0);
        if reclaimed > 0 {
            stage_wal::note_reclaimable(&stage_wal::wal_path(&self.path), reclaimed).await;
        }
        Ok(())
    }

    async fn refresh_wal_lease(&mut self) {
        let wal_bytes = stage_wal::uncheckpointed_bytes_for(&self.path, self.checkpoint_lsn)
            .await
            .unwrap_or(0);
        self.wal_lease.resize(wal_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_interrupted_local_write_replays_before_stage_recovery() {
        let root = std::env::temp_dir().join(format!(
            "r2-write-replay-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let mut stage = Stage::create(path.clone(), "original/key".into(), 1)
            .await
            .unwrap();
        stage.write_durable(0, b"old", 1).await.unwrap();
        let intent = WriteIntent {
            state: StageRecovery {
                key: "original/key".into(),
                size: 8,
                mtime_secs: 2,
                generation: 2,
                dirty: true,
                state: "waiting".into(),
                error: None,
                path: path.clone(),
                snapshot: None,
                publication_guard: None,
                checkpoint_lsn: 0,
                first_dirty_at: None,
                wal_bytes: None,
            },
            change: DurableChange::Write {
                offset: 0,
                data: b"complete".to_vec(),
            },
        };
        write_json_atomic(&path.with_extension("write.json"), &intent)
            .await
            .unwrap();
        // Simulate a crash after part of the file update but before its manifest.
        stage.file.seek(SeekFrom::Start(0)).await.unwrap();
        stage.file.write_all(b"comp").await.unwrap();
        stage.file.sync_all().await.unwrap();
        drop(stage);
        let inventory = recovery_entries(&root).await.unwrap();
        assert_eq!(inventory[0].state, "replay_pending");
        assert_eq!(
            tokio::fs::read(&path).await.unwrap(),
            b"comp",
            "inventory is read-only"
        );
        replay_write_intents(&root).await.unwrap();
        let records = recovery_entries(&root).await.unwrap();
        assert_eq!(records[0].key, "original/key");
        assert_eq!(records[0].generation, 2);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"complete");
        assert!(!tokio::fs::try_exists(path.with_extension("write.json"))
            .await
            .unwrap());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_durable_resize_restores_the_exact_acknowledged_size() {
        let root = std::env::temp_dir().join(format!(
            "r2-resize-replay-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let mut stage = Stage::create(root.join("record.data"), "key".into(), 1)
            .await
            .unwrap();
        stage.write_durable(0, b"content", 1).await.unwrap();
        stage.truncate_durable(2, 2).await.unwrap();
        drop(stage);
        // Restore order, as restore_stages does it: replay, then inventory.
        replay_write_intents(&root).await.unwrap();
        let records = recovery_entries(&root).await.unwrap();
        let mut restored = Stage::restore(records.into_iter().next().unwrap())
            .await
            .unwrap();
        assert_eq!(restored.read_at(0, 100).await.unwrap(), b"co");
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_wal_synced_before_data_recovers_the_acknowledged_write() {
        let root = std::env::temp_dir().join(format!(
            "r2-wal-before-data-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        drop(stage);
        let wal = stage_wal::wal_path(&path);
        stage_wal::append_record(
            &wal,
            &stage_wal::WalRecord {
                lsn: 1,
                generation: 1,
                op: stage_wal::WalOp::Write,
                offset: 0,
                resulting_size: 3,
                mtime_secs: 1,
                dirty_at_ms: 1,
                data_name: stage_wal::data_name(&path).unwrap(),
                key: "key".into(),
                payload: b"abc".to_vec(),
            },
        )
        .await
        .unwrap();
        stage_commit::commit(vec![wal]).await.unwrap();

        replay_write_intents(&root).await.unwrap();
        let record = recovery_entries(&root).await.unwrap().remove(0);
        let mut restored = Stage::restore(record).await.unwrap();
        assert_eq!(restored.read_at(0, 10).await.unwrap(), b"abc");
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn data_synced_before_manifest_still_recovers_from_wal() {
        let root = std::env::temp_dir().join(format!(
            "r2-data-before-manifest-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        let wal = stage_wal::wal_path(&path);
        stage_wal::append_record(
            &wal,
            &stage_wal::WalRecord {
                lsn: 1,
                generation: 1,
                op: stage_wal::WalOp::Write,
                offset: 0,
                resulting_size: 3,
                mtime_secs: 1,
                dirty_at_ms: 1,
                data_name: stage_wal::data_name(&path).unwrap(),
                key: "key".into(),
                payload: b"abc".to_vec(),
            },
        )
        .await
        .unwrap();
        stage_commit::commit(vec![wal]).await.unwrap();
        stage.write_at(0, b"abc").await.unwrap();
        stage.file.sync_all().await.unwrap();
        drop(stage);

        replay_write_intents(&root).await.unwrap();
        let record = recovery_entries(&root).await.unwrap().remove(0);
        let mut restored = Stage::restore(record).await.unwrap();
        assert_eq!(restored.read_at(0, 10).await.unwrap(), b"abc");
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn manifest_checkpoint_before_wal_truncate_does_not_replay_old_records() {
        let root = std::env::temp_dir().join(format!(
            "r2-manifest-before-wal-truncate-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        let wal = stage_wal::wal_path(&path);
        stage_wal::append_record(
            &wal,
            &stage_wal::WalRecord {
                lsn: 1,
                generation: 1,
                op: stage_wal::WalOp::Write,
                offset: 0,
                resulting_size: 3,
                mtime_secs: 1,
                dirty_at_ms: 1,
                data_name: stage_wal::data_name(&path).unwrap(),
                key: "key".into(),
                payload: b"abc".to_vec(),
            },
        )
        .await
        .unwrap();
        stage_commit::commit(vec![wal.clone()]).await.unwrap();
        stage.write_at(0, b"abc").await.unwrap();
        stage.file.sync_all().await.unwrap();
        stage.dirty = true;
        stage.dirty_gen = 1;
        stage.checkpoint_lsn = 1;
        stage.next_lsn = 2;
        stage.first_dirty_at = Some(chrono::Utc::now().timestamp_millis());
        stage.persist().await.unwrap();
        drop(stage);

        let record = recovery_entries(&root).await.unwrap().remove(0);
        assert_eq!(record.checkpoint_lsn, 1);
        let mut restored = Stage::restore(record).await.unwrap();
        assert_eq!(restored.read_at(0, 10).await.unwrap(), b"abc");
        assert!(tokio::fs::try_exists(wal).await.unwrap());
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn one_write_across_many_stages_does_not_checkpoint_each_stage() {
        let root = std::env::temp_dir().join(format!(
            "r2-many-stage-no-checkpoint-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let mut stages = Vec::new();
        for index in 0..100usize {
            let path = root.join(format!("{index}.data"));
            let mut stage = Stage::create(path, format!("key/{index}"), 1)
                .await
                .unwrap();
            stage.write_durable(0, &[index as u8], 1).await.unwrap();
            assert_eq!(
                stage.checkpoint_lsn, 0,
                "single-write stage checkpointed early"
            );
            assert_eq!(stage.records_since_checkpoint, 1);
            stages.push(stage);
        }
        assert!(!tokio::fs::try_exists(root.join(".stage.wal.highwater"))
            .await
            .unwrap());
        drop(stages);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn shared_wal_accounting_tracks_owned_records_not_metadata_per_stage() {
        let root = std::env::temp_dir().join(format!(
            "r2-shared-wal-accounting-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let mut first = Stage::create(root.join("first.data"), "first".into(), 1)
            .await
            .unwrap();
        let mut second = Stage::create(root.join("second.data"), "second".into(), 1)
            .await
            .unwrap();
        first.write_durable(0, b"first", 1).await.unwrap();
        second.write_durable(0, b"second", 1).await.unwrap();
        let wal_len = tokio::fs::metadata(root.join(".stage.wal"))
            .await
            .unwrap()
            .len();
        let owned_wal_bytes = first
            .wal_lease
            .bytes()
            .saturating_add(second.wal_lease.bytes());
        assert_eq!(owned_wal_bytes, wal_len);
        assert!(first.wal_lease.bytes() < wal_len);
        assert!(second.wal_lease.bytes() < wal_len);
        assert_eq!(first.reservation_bytes().await, first.size);
        assert_eq!(second.reservation_bytes().await, second.size);
        drop(first);
        drop(second);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn recovery_entries_scans_shared_wal_once_for_many_stages() {
        let root = std::env::temp_dir().join(format!(
            "r2-recovery-one-scan-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        for index in 0..100usize {
            let path = root.join(format!("{index}.data"));
            let mut stage = Stage::create(path, format!("key/{index}"), 1)
                .await
                .unwrap();
            stage.write_durable(0, &[index as u8], 1).await.unwrap();
            stage.persist().await.unwrap();
        }
        let wal = root.join(".stage.wal");
        let before = stage_wal::wal_read_count(&wal);
        let entries = recovery_entries(&root).await.unwrap();
        let after_entries = stage_wal::wal_read_count(&wal);
        assert_eq!(entries.len(), 100);
        assert_eq!(after_entries.saturating_sub(before), 1);
        for record in entries {
            let restored = Stage::restore(record).await.unwrap();
            drop(restored);
        }
        assert_eq!(stage_wal::wal_read_count(&wal), after_entries);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn sequential_and_random_repeated_writes_recover_exact_bytes() {
        let root = std::env::temp_dir().join(format!(
            "r2-repeated-writes-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        let mut expected = vec![0u8; 8192];
        for index in 0..32usize {
            let offset = (index * 173) % 4096;
            let len = 17 + (index * 29) % 511;
            let payload = vec![(index as u8).wrapping_mul(7).wrapping_add(3); len];
            stage
                .write_durable(offset as u64, &payload, index as u32 + 1)
                .await
                .unwrap();
            expected[offset..offset + len].copy_from_slice(&payload);
        }
        for index in 0..32usize {
            let offset = (8191usize.wrapping_sub(index * 197)) % 4096;
            let len = 1 + (index * 31) % 257;
            let payload = vec![(index as u8).wrapping_mul(11).wrapping_add(5); len];
            stage
                .write_durable(offset as u64, &payload, index as u32 + 33)
                .await
                .unwrap();
            expected[offset..offset + len].copy_from_slice(&payload);
        }
        let size = stage.size as usize;
        let expected = expected[..size].to_vec();
        drop(stage);

        replay_write_intents(&root).await.unwrap();
        let record = recovery_entries(&root).await.unwrap().remove(0);
        let mut restored = Stage::restore(record).await.unwrap();
        assert_eq!(restored.read_at(0, size).await.unwrap(), expected);
        assert_eq!(restored.dirty_gen, 64);
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    /// Puts bytes into a data file behind the stage's back — how the page
    /// cache reads after Linux dropped the pages of a failed write-back.
    async fn overwrite(path: &Path, offset: u64, bytes: &[u8]) {
        let mut file = OpenOptions::new().write(true).open(path).await.unwrap();
        file.seek(SeekFrom::Start(offset)).await.unwrap();
        file.write_all(bytes).await.unwrap();
        file.sync_all().await.unwrap();
    }

    async fn recover_single(root: &Path) -> Stage {
        stage_wal::forget_append_state(&root.join(".stage.wal")).await;
        let errors = replay_write_intents(root).await.unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        let record = recovery_entries(root).await.unwrap().remove(0);
        assert_ne!(record.state, "unreadable", "{:?}", record.error);
        Stage::restore(record).await.unwrap()
    }

    #[tokio::test]
    async fn a_failed_data_fsync_is_never_followed_by_a_trusting_checkpoint() {
        let root = std::env::temp_dir().join(format!(
            "r2-data-fsync-checkpoint-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        stage.write_durable(0, b"aaaa", 1).await.unwrap();
        stage.checkpoint_durable().await.unwrap();
        stage.write_durable(0, b"bbbb", 2).await.unwrap();

        stage_commit::fail_next_syncs(&path, 1);
        assert!(stage.checkpoint_durable().await.is_err());
        // The failed write-back dropped the acknowledged pages.
        overwrite(&path, 0, b"aaaa").await;
        // A later fsync that happens to succeed proves nothing on its own.
        stage.checkpoint_durable().await.unwrap();
        drop(stage);

        let mut restored = recover_single(&root).await;
        assert_eq!(
            restored.read_at(0, 16).await.unwrap(),
            b"bbbb",
            "acknowledged bytes were checkpointed away"
        );
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn no_write_is_acknowledged_while_the_data_file_is_unproven() {
        let root = std::env::temp_dir().join(format!(
            "r2-data-fsync-refuse-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        stage.write_durable(0, b"aaaa", 1).await.unwrap();
        stage.checkpoint_durable().await.unwrap();
        stage.write_durable(0, b"bbbb", 2).await.unwrap();
        stage_commit::fail_next_syncs(&path, 1);
        assert!(stage.checkpoint_durable().await.is_err());
        overwrite(&path, 0, b"aaaa").await;

        // The WAL must be re-applied through a handle that saw no failure
        // before anything else is acknowledged; here that fails too.
        stage_commit::fail_next_syncs(&path, 1);
        assert!(
            stage.write_durable(4, b"cccc", 3).await.is_err(),
            "a write was acknowledged on top of unproven data"
        );
        stage.write_durable(4, b"cccc", 3).await.unwrap();
        assert_eq!(stage.read_at(0, 16).await.unwrap(), b"bbbbcccc");
        drop(stage);
        let mut restored = recover_single(&root).await;
        assert_eq!(restored.read_at(0, 16).await.unwrap(), b"bbbbcccc");
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_data_fsync_before_a_replay_is_never_shrugged_off() {
        let root = std::env::temp_dir().join(format!(
            "r2-data-fsync-replay-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        stage.write_durable(0, b"aaaa", 1).await.unwrap();
        stage.checkpoint_durable().await.unwrap();
        stage.write_durable(0, b"bbbb", 2).await.unwrap();
        // The next change is committed but never reaches the data file...
        fail_next_apply(&path);
        assert!(stage.write_durable(4, b"cccc", 3).await.is_err());
        // ...and the write-back of the acknowledged one failed as well.
        stage_commit::fail_next_syncs(&path, 1);
        overwrite(&path, 0, b"aaaa").await;

        assert_eq!(stage.read_at(0, 16).await.unwrap(), b"bbbbcccc");
        drop(stage);
        let mut restored = recover_single(&root).await;
        assert_eq!(restored.read_at(0, 16).await.unwrap(), b"bbbbcccc");
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn downloaded_content_that_never_proved_durable_is_never_built_on() {
        let root = std::env::temp_dir().join(format!(
            "r2-primed-fsync-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        // The read-modify-write download lands outside the WAL.
        stage.write_at(0, b"downloaded").await.unwrap();
        stage_commit::fail_next_syncs(&path, 1);
        assert!(stage.write_durable(0, b"D", 2).await.is_err());
        // The WAL cannot rebuild that content, so no later fsync may vouch
        // for it: every write on top of it is refused.
        assert!(stage.write_durable(0, b"D", 2).await.is_err());
        assert!(stage.read_at(0, 16).await.is_err());
        assert!(!tokio::fs::try_exists(path.with_extension("stage.json"))
            .await
            .unwrap());
        drop(stage);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn persist_never_claims_a_record_the_data_file_lacks() {
        let root = std::env::temp_dir().join(format!(
            "r2-persist-unapplied-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        stage.write_durable(0, b"aaaa", 1).await.unwrap();
        fail_next_apply(&path);
        assert!(stage.write_durable(0, b"bbbb", 2).await.is_err());
        stage.persist().await.unwrap();
        drop(stage);

        let manifest: StageRecovery = serde_json::from_slice(
            &tokio::fs::read(path.with_extension("stage.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        let data = tokio::fs::read(&path).await.unwrap();
        assert!(
            manifest.generation < 2 || data == b"bbbb",
            "manifest claims generation {} over {:?}",
            manifest.generation,
            String::from_utf8_lossy(&data)
        );
        let mut restored = recover_single(&root).await;
        let recovered = restored.read_at(0, 8).await.unwrap();
        assert_eq!(recovered, data, "recovery disagrees with the manifest");
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn uploads_and_removals_below_the_threshold_never_rewrite_the_wal() {
        let root = std::env::temp_dir().join(format!(
            "r2-no-sync-compaction-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let wal = root.join(".stage.wal");
        let mut stages = Vec::new();
        for index in 0..8usize {
            let mut stage =
                Stage::create(root.join(format!("{index}.data")), format!("{index}"), 1)
                    .await
                    .unwrap();
            stage
                .write_durable(0, &[index as u8; 2048], 1)
                .await
                .unwrap();
            stages.push(stage);
        }
        let reads = stage_wal::wal_read_count(&wal);
        let mut length = tokio::fs::metadata(&wal).await.unwrap().len();
        for (index, mut stage) in stages.into_iter().enumerate() {
            if index % 2 == 0 {
                // Uploaded, then evicted.
                stage.upload_snapshot().await.unwrap();
                stage.dirty = false;
            }
            // The other half is deleted while still dirty (REMOVE).
            stage.remove_files().await;
            let now = tokio::fs::metadata(&wal).await.map_or(0, |m| m.len());
            assert!(now >= length, "an upload or removal rewrote the WAL");
            length = now;
        }
        assert_eq!(
            stage_wal::wal_read_count(&wal),
            reads,
            "an upload or removal re-read the whole WAL"
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn removed_and_checkpointed_bytes_are_reclaimed_once_past_the_threshold() {
        let root = std::env::temp_dir().join(format!(
            "r2-remove-wal-growth-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let wal = root.join(".stage.wal");
        stage_wal::set_compaction_floor(&wal, 64 * 1024);
        let mut kept = Stage::create(root.join("kept.data"), "kept".into(), 1)
            .await
            .unwrap();
        kept.write_durable(0, b"kept", 1).await.unwrap();
        let reads = stage_wal::wal_read_count(&wal);
        for index in 0..40usize {
            let path = root.join(format!("{index}.data"));
            let mut stage = Stage::create(path, format!("key/{index}"), 1)
                .await
                .unwrap();
            for chunk in 0..3u64 {
                stage
                    .write_durable(chunk * 1024, &[index as u8; 1024], 1)
                    .await
                    .unwrap();
            }
            if index % 2 == 0 {
                stage.upload_snapshot().await.unwrap();
            }
            stage.remove_files().await;
            assert_eq!(stage.wal_lease.bytes(), 0);
        }
        // Past the threshold the dead records are compacted away — off the
        // caller's locks and amortized, never once per removal or upload —
        // so what is left dead is bounded by the threshold.
        stage_wal::compact_if_due(&wal).await.unwrap();
        let compactions = stage_wal::wal_read_count(&wal) - reads;
        assert!(
            (1..=4).contains(&compactions),
            "{compactions} WAL rewrites for 40 files"
        );
        assert!(
            tokio::fs::metadata(&wal).await.unwrap().len() <= kept.wal_lease.bytes() + 64 * 1024
        );
        // Whatever is due goes: only the live stage's records are left...
        stage_wal::set_compaction_floor(&wal, 0);
        stage_wal::compact_if_due(&wal).await.unwrap();
        assert_eq!(
            tokio::fs::metadata(&wal).await.unwrap().len(),
            kept.wal_lease.bytes(),
            "only the live stage's records are left"
        );
        // ...and once everything is dead, no WAL at all.
        kept.remove_files().await;
        stage_wal::compact_if_due(&wal).await.unwrap();
        assert_eq!(tokio::fs::metadata(&wal).await.map_or(0, |m| m.len()), 0);
        drop(kept);
        assert!(recovery_entries(&root).await.unwrap().is_empty());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn evicting_an_uploaded_stage_leaves_the_wal_alone() {
        let root = std::env::temp_dir().join(format!(
            "r2-evict-no-wal-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let wal = stage_wal::wal_path(&path);
        let mut other = Stage::create(root.join("other.data"), "other".into(), 1)
            .await
            .unwrap();
        other.write_durable(0, b"other", 1).await.unwrap();
        let mut stage = Stage::create(path, "key".into(), 1).await.unwrap();
        stage.write_durable(0, b"uploaded", 1).await.unwrap();
        stage.upload_snapshot().await.unwrap();
        // What a settled upload leaves behind before evict_stage runs.
        stage.dirty = false;
        stage.snapshot = None;
        let before = tokio::fs::read(&wal).await.unwrap();
        let reads = stage_wal::wal_read_count(&wal);
        stage.remove_files().await;
        assert_eq!(stage_wal::wal_read_count(&wal), reads);
        assert_eq!(tokio::fs::read(&wal).await.unwrap(), before);
        drop(stage);
        drop(other);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_removal_that_cannot_be_made_durable_leaves_the_stage_whole() {
        let root = std::env::temp_dir().join(format!(
            "r2-remove-not-durable-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let wal = stage_wal::wal_path(&path);
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        stage
            .write_durable(0, b"not uploaded yet", 1)
            .await
            .unwrap();
        // Neither the discard (a failed fsync, or a full disk — twice: the
        // durable attempt and the best-effort retry) nor the unlink of the
        // data file the WAL replays into goes through.
        stage_commit::fail_next_syncs(&wal, 2);
        fail_removal_of(&path);
        stage.remove_files().await;
        assert!(
            tokio::fs::try_exists(path.with_extension("stage.json"))
                .await
                .unwrap(),
            "the manifest went while the data file it describes stayed"
        );
        // The stage lives on and keeps acknowledging writes. The discard its
        // failed commit left in the WAL — made durable by the rewrite the next
        // append runs — must not void them.
        stage.write_durable(16, b"!", 2).await.unwrap();
        drop(stage);
        let mut restored = recover_single(&root).await;
        assert_eq!(restored.read_at(0, 64).await.unwrap(), b"not uploaded yet!");
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_remove_is_acknowledged_only_once_its_deletion_is_durable() {
        let root = std::env::temp_dir().join(format!(
            "r2-remove-acknowledged-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let wal = stage_wal::wal_path(&path);
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        stage
            .write_durable(0, b"not uploaded yet", 1)
            .await
            .unwrap();
        stage_commit::fail_next_syncs(&wal, 1);
        assert!(
            stage.try_remove_files().await.is_err(),
            "a REMOVE was acknowledged without a durable discard"
        );
        assert!(tokio::fs::try_exists(&path).await.unwrap());
        assert!(tokio::fs::try_exists(path.with_extension("stage.json"))
            .await
            .unwrap());
        assert_eq!(stage.read_at(0, 64).await.unwrap(), b"not uploaded yet");
        // Once the WAL is healthy again the removal goes through, even if an
        // unlink fails: the durable discard already settles it.
        fail_removal_of(&path);
        stage.try_remove_files().await.unwrap();
        drop(stage);
        stage_wal::forget_append_state(&wal).await;
        assert!(replay_write_intents(&root).await.unwrap().is_empty());
        assert!(recovery_entries(&root)
            .await
            .unwrap()
            .iter()
            .all(|entry| entry.key != "key"));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn records_of_a_stage_removed_without_its_discard_are_still_reclaimed() {
        let root = std::env::temp_dir().join(format!(
            "r2-remove-no-discard-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let wal = stage_wal::wal_path(&path);
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        stage.write_durable(0, b"deleted", 1).await.unwrap();
        // A full disk: the discard cannot be written, the unlinks can.
        stage_commit::fail_next_syncs(&wal, 1);
        stage.remove_files().await;
        assert!(!tokio::fs::try_exists(&path).await.unwrap());
        drop(stage);
        // With data file and manifest gone the records are dead without it.
        let mut next = Stage::create(root.join("next.data"), "next".into(), 1)
            .await
            .unwrap();
        next.write_durable(0, b"next", 1).await.unwrap();
        assert!(stage_wal::compact_now(&wal).await.unwrap());
        assert_eq!(
            tokio::fs::metadata(&wal).await.unwrap().len(),
            next.wal_lease.bytes()
        );
        drop(next);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_crash_at_any_step_of_a_remove_never_brings_the_file_back() {
        for crash_after in 1..=5u32 {
            let root = std::env::temp_dir().join(format!(
                "r2-remove-crash-{crash_after}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            ));
            let gone_path = root.join("gone.data");
            let wal = stage_wal::wal_path(&gone_path);
            let mut gone = Stage::create(gone_path.clone(), "gone".into(), 1)
                .await
                .unwrap();
            let mut kept = Stage::create(root.join("kept.data"), "kept".into(), 1)
                .await
                .unwrap();
            gone.write_durable(0, b"deleted by the user", 1)
                .await
                .unwrap();
            kept.write_durable(0, b"still wanted", 1).await.unwrap();
            crash_remove_after(&gone_path, crash_after);
            gone.remove_files().await;
            drop(gone);
            drop(kept);
            stage_wal::forget_append_state(&wal).await;

            let errors = replay_write_intents(&root).await.unwrap();
            assert!(errors.is_empty(), "step {crash_after}: {errors:?}");
            let entries = recovery_entries(&root).await.unwrap();
            assert!(
                entries.iter().all(|entry| entry.key != "gone"),
                "a crash after step {crash_after} brought the deleted file back"
            );
            let kept = entries.iter().find(|entry| entry.key == "kept").unwrap();
            let mut restored = Stage::restore(kept.clone()).await.unwrap();
            assert_eq!(restored.read_at(0, 64).await.unwrap(), b"still wanted");
            drop(restored);
            tokio::fs::remove_dir_all(root).await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_failed_wal_fsync_refuses_every_ack_until_the_wal_is_rewritten() {
        let root = std::env::temp_dir().join(format!(
            "r2-wal-fsync-poison-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let a_path = root.join("a.data");
        let b_path = root.join("b.data");
        let wal = stage_wal::wal_path(&a_path);
        let mut a = Stage::create(a_path.clone(), "a".into(), 1).await.unwrap();
        let mut b = Stage::create(b_path.clone(), "b".into(), 1).await.unwrap();
        a.write_durable(0, b"one", 1).await.unwrap();
        b.write_durable(0, b"bee", 1).await.unwrap();

        stage_commit::fail_next_syncs(&wal, 1);
        assert!(
            a.write_durable(3, b"two", 2).await.is_err(),
            "no reply without a durable fsync"
        );
        // A later fsync of the same file may succeed without the pages the
        // failed one lost, so no stage in the folder is acknowledged until
        // the WAL is rewritten. Make that rewrite fail once.
        stage_commit::fail_next_syncs(&wal.with_extension("wal.tmp"), 1);
        assert!(
            b.write_durable(3, b"sting", 2).await.is_err(),
            "a write was acknowledged on a poisoned WAL"
        );
        // The rewrite also re-fsyncs every data file with records in the WAL;
        // until that works too, the poison stays.
        stage_commit::fail_next_syncs(&a_path, 1);
        assert!(b.write_durable(3, b"sting", 2).await.is_err());
        // The rewrite succeeds on the next attempt and writes resume.
        b.write_durable(3, b"sting", 2).await.unwrap();
        a.write_durable(6, b"three", 3).await.unwrap();
        let a_live = a.read_at(0, 64).await.unwrap();
        let b_live = b.read_at(0, 64).await.unwrap();
        assert_eq!(&a_live[..3], b"one");
        assert_eq!(&a_live[6..], b"three");
        assert_eq!(b_live, b"beesting");
        drop(a);
        drop(b);
        stage_wal::forget_append_state(&wal).await;

        assert!(replay_write_intents(&root).await.unwrap().is_empty());
        let entries = recovery_entries(&root).await.unwrap();
        for (key, live) in [("a", a_live), ("b", b_live)] {
            let entry = entries.iter().find(|entry| entry.key == key).unwrap();
            let mut restored = Stage::restore(entry.clone()).await.unwrap();
            assert_eq!(restored.read_at(0, 64).await.unwrap(), live, "{key}");
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_v0_3_5_folder_with_a_pending_json_intent_upgrades_and_keeps_writing() {
        let root = std::env::temp_dir().join(format!(
            "r2-v035-upgrade-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let path = root.join("legacy.data");
        tokio::fs::write(&path, b"abc").await.unwrap();
        // v0.3.5 on-disk shapes: no checkpoint_lsn / first_dirty_at, no WAL,
        // and a crash between the JSON intent and its manifest.
        let state = |size: u64, generation: u64| {
            serde_json::json!({
                "key": "legacy/key", "size": size, "mtime_secs": 1,
                "generation": generation, "dirty": true, "state": "waiting",
                "error": null, "path": path, "snapshot": null,
                "publication_guard": null
            })
        };
        write_json_atomic(&path.with_extension("stage.json"), &state(3, 1))
            .await
            .unwrap();
        write_json_atomic(
            &path.with_extension("write.json"),
            &serde_json::json!({
                "state": state(6, 2),
                "change": { "Write": { "offset": 3, "data": [100, 101, 102] } }
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            recovery_entries(&root).await.unwrap()[0].state,
            "replay_pending"
        );
        assert!(replay_write_intents(&root).await.unwrap().is_empty());
        let record = recovery_entries(&root).await.unwrap().remove(0);
        assert_eq!(record.generation, 2);
        let mut restored = Stage::restore(record).await.unwrap();
        assert_eq!(restored.read_at(0, 16).await.unwrap(), b"abcdef");
        // Post-upgrade writes go to the WAL and stack on the legacy generation.
        restored.write_durable(6, b"ghi", 2).await.unwrap();
        assert_eq!(restored.dirty_gen, 3);
        drop(restored);
        stage_wal::forget_append_state(&stage_wal::wal_path(&path)).await;

        assert!(replay_write_intents(&root).await.unwrap().is_empty());
        let record = recovery_entries(&root).await.unwrap().remove(0);
        assert_eq!(record.generation, 3);
        let mut restored = Stage::restore(record).await.unwrap();
        assert_eq!(restored.read_at(0, 16).await.unwrap(), b"abcdefghi");
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_stage_whose_replay_fails_is_quarantined_under_its_restore_key() {
        let root = std::env::temp_dir().join(format!(
            "r2-replay-failure-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let a_path = root.join("a.data");
        let b_path = root.join("b.data");
        let mut a = Stage::create(a_path.clone(), "a".into(), 1).await.unwrap();
        let mut b = Stage::create(b_path.clone(), "b".into(), 1).await.unwrap();
        a.write_durable(0, b"old!", 1).await.unwrap();
        a.checkpoint_durable().await.unwrap();
        a.write_durable(0, b"new!", 2).await.unwrap();
        b.write_durable(0, b"bbbb", 1).await.unwrap();
        drop(a);
        drop(b);
        // Crash before the acknowledged overwrite reached a's data file. The
        // size is unchanged, so no length check can notice the old bytes.
        tokio::fs::write(&a_path, b"old!").await.unwrap();
        stage_wal::fail_replay_of(&a_path, true);

        // restore_stages: replay, then quarantine every replay_pending record
        // with the error found under `<data>.write.json`.
        let errors: std::collections::HashMap<_, _> = replay_write_intents(&root)
            .await
            .unwrap()
            .into_iter()
            .collect();
        let entries = recovery_entries(&root).await.unwrap();
        let a_entry = entries.iter().find(|entry| entry.key == "a").unwrap();
        assert_eq!(
            a_entry.state, "replay_pending",
            "a stage with unapplied acknowledged records must not be restorable"
        );
        assert_eq!(
            a_entry.generation, 1,
            "never advanced past unapplied records"
        );
        assert_eq!(a_entry.checkpoint_lsn, 1);
        let keyed = errors.get(&a_entry.path.with_extension("write.json"));
        assert!(
            keyed.is_some_and(|error| error.contains("injected replay failure")),
            "restore_stages must find the replay error: {errors:?}"
        );
        let b_entry = entries.iter().find(|entry| entry.key == "b").unwrap();
        assert!(!["replay_pending", "unreadable"].contains(&b_entry.state.as_str()));
        let mut b_restored = Stage::restore(b_entry.clone()).await.unwrap();
        assert_eq!(b_restored.read_at(0, 8).await.unwrap(), b"bbbb");
        drop(b_restored);
        assert_eq!(tokio::fs::read(&a_path).await.unwrap(), b"old!");

        // Nothing was checkpointed away: once replay works the write is back.
        stage_wal::fail_replay_of(&a_path, false);
        assert!(replay_write_intents(&root).await.unwrap().is_empty());
        let a_entry = recovery_entries(&root)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.key == "a")
            .unwrap();
        let mut a_restored = Stage::restore(a_entry).await.unwrap();
        assert_eq!(a_restored.read_at(0, 8).await.unwrap(), b"new!");
        drop(a_restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_manifestless_stage_whose_replay_fails_is_reported_not_dropped() {
        let root = std::env::temp_dir().join(format!(
            "r2-manifestless-replay-failure-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        drop(Stage::create(path.clone(), "key".into(), 1).await.unwrap());
        let wal = stage_wal::wal_path(&path);
        stage_wal::append_record(
            &wal,
            &stage_wal::WalRecord {
                lsn: 1,
                generation: 1,
                op: stage_wal::WalOp::Write,
                offset: 0,
                resulting_size: 3,
                mtime_secs: 1,
                dirty_at_ms: 1,
                data_name: stage_wal::data_name(&path).unwrap(),
                key: "key".into(),
                payload: b"abc".to_vec(),
            },
        )
        .await
        .unwrap();
        stage_commit::commit(vec![wal]).await.unwrap();
        stage_wal::fail_replay_of(&path, true);

        let errors: std::collections::HashMap<_, _> = replay_write_intents(&root)
            .await
            .unwrap()
            .into_iter()
            .collect();
        assert!(errors.contains_key(&path.with_extension("write.json")));
        let entries = recovery_entries(&root).await.unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.path == path)
            .expect("acknowledged records without a manifest must be listed");
        assert_eq!(entry.state, "replay_pending");
        assert_eq!(entry.key, "key");
        stage_wal::fail_replay_of(&path, false);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    async fn append_to_wal(wal: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new().append(true).open(wal).await.unwrap();
        file.write_all(bytes).await.unwrap();
        file.sync_all().await.unwrap();
    }

    #[tokio::test]
    async fn a_zero_filled_wal_tail_after_power_loss_keeps_every_acknowledged_write() {
        let root = std::env::temp_dir().join(format!(
            "r2-zero-tail-recovery-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let wal = stage_wal::wal_path(&path);
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        stage.write_durable(0, b"acknowledged", 1).await.unwrap();
        stage.write_durable(12, b" twice", 2).await.unwrap();
        drop(stage);
        // The filesystem grew the WAL for an append whose data never landed.
        append_to_wal(&wal, &[0u8; 8192]).await;
        stage_wal::forget_append_state(&wal).await;

        let errors = replay_write_intents(&root).await.unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        let record = recovery_entries(&root).await.unwrap().remove(0);
        assert_ne!(record.state, "unreadable");
        let mut restored = Stage::restore(record).await.unwrap();
        assert_eq!(
            restored.read_at(0, 64).await.unwrap(),
            b"acknowledged twice"
        );
        // Writing again cuts the dead tail first, so the next crash recovers
        // the new record too instead of finding it behind invalid bytes.
        restored.write_durable(18, b"!", 3).await.unwrap();
        drop(restored);
        stage_wal::forget_append_state(&wal).await;
        replay_write_intents(&root).await.unwrap();
        let record = recovery_entries(&root).await.unwrap().remove(0);
        let mut restored = Stage::restore(record).await.unwrap();
        assert_eq!(
            restored.read_at(0, 64).await.unwrap(),
            b"acknowledged twice!"
        );
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_power_loss_hole_before_unacknowledged_records_is_cut_and_writes_continue() {
        let root = std::env::temp_dir().join(format!(
            "r2-power-loss-hole-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let wal = stage_wal::wal_path(&path);
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        stage.write_durable(0, b"acked", 1).await.unwrap();
        drop(stage);
        let acked_len = tokio::fs::metadata(&wal).await.unwrap().len() as usize;
        // A batch whose commit never returned: its unsynced bytes reached the
        // disk out of order, the first as zeros, the second intact.
        let name = stage_wal::data_name(&path).unwrap();
        let unacked = |generation: u64, offset: u64, payload: &[u8]| stage_wal::WalRecord {
            lsn: 0,
            generation,
            op: stage_wal::WalOp::Write,
            offset,
            resulting_size: offset + payload.len() as u64,
            mtime_secs: 2,
            dirty_at_ms: 1,
            data_name: name.clone(),
            key: "key".into(),
            payload: payload.to_vec(),
        };
        let lost = unacked(2, 5, b"-one");
        stage_wal::append_record(&wal, &lost).await.unwrap();
        stage_wal::append_record(&wal, &unacked(3, 9, b"-two"))
            .await
            .unwrap();
        let lost_len = stage_wal::estimated_record_len(&lost).unwrap() as usize;
        let mut bytes = tokio::fs::read(&wal).await.unwrap();
        bytes[acked_len..acked_len + lost_len].fill(0);
        tokio::fs::write(&wal, &bytes).await.unwrap();

        let mut restored = recover_single(&root).await;
        assert_eq!(restored.read_at(0, 64).await.unwrap(), b"acked");
        restored.write_durable(5, b"!", 2).await.unwrap();
        drop(restored);
        let mut restored = recover_single(&root).await;
        assert_eq!(restored.read_at(0, 64).await.unwrap(), b"acked!");
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    async fn damaged_wal_copies(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut dir = tokio::fs::read_dir(root).await.unwrap();
        while let Some(entry) = dir.next_entry().await.unwrap() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".stage.wal.damaged")
            {
                found.push(entry.path());
            }
        }
        found
    }

    #[tokio::test]
    async fn a_damaged_acknowledged_record_quarantines_only_its_stage_and_sets_the_wal_aside() {
        let root = std::env::temp_dir().join(format!(
            "r2-acked-damage-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let a_path = root.join("a.data");
        let b_path = root.join("b.data");
        let wal = stage_wal::wal_path(&a_path);
        let mut a = Stage::create(a_path.clone(), "a".into(), 1).await.unwrap();
        let mut b = Stage::create(b_path.clone(), "b".into(), 1).await.unwrap();
        a.write_durable(0, &[1u8; 4096], 1).await.unwrap();
        b.write_durable(0, &[2u8; 4096], 1).await.unwrap();
        a.write_durable(4096, &[3u8; 4096], 2).await.unwrap();
        drop(a);
        drop(b);
        // Media damage inside a's first record, long after it was acknowledged.
        let mut bytes = tokio::fs::read(&wal).await.unwrap();
        bytes[1024] ^= 0xff;
        tokio::fs::write(&wal, &bytes).await.unwrap();
        stage_wal::forget_append_state(&wal).await;

        replay_write_intents(&root)
            .await
            .expect("damage never fails the restore");
        let set_aside = damaged_wal_copies(&root).await;
        assert_eq!(set_aside.len(), 1, "the damaged WAL is kept for export");
        assert_eq!(tokio::fs::read(&set_aside[0]).await.unwrap(), bytes);
        let entries = recovery_entries(&root).await.unwrap();
        let a_entry = entries.iter().find(|entry| entry.key == "a").unwrap();
        assert_eq!(a_entry.state, "unreadable");
        assert!(a_entry.error.as_deref().unwrap().contains("damaged"));
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == set_aside[0] && entry.state == "unreadable"),
            "the set-aside WAL is listed for export"
        );
        let b_entry = entries.iter().find(|entry| entry.key == "b").unwrap();
        assert!(!["unreadable", "replay_pending"].contains(&b_entry.state.as_str()));
        let mut b = Stage::restore(b_entry.clone()).await.unwrap();
        assert_eq!(b.read_at(0, 8192).await.unwrap(), vec![2u8; 4096]);

        // The mount stays writable: a fresh WAL takes the next writes.
        b.write_durable(4096, b"more", 2).await.unwrap();
        let mut c = Stage::create(root.join("c.data"), "c".into(), 1)
            .await
            .unwrap();
        c.write_durable(0, b"new file", 1).await.unwrap();
        drop(b);
        drop(c);
        stage_wal::forget_append_state(&wal).await;
        assert!(replay_write_intents(&root).await.unwrap().is_empty());
        let entries = recovery_entries(&root).await.unwrap();
        let content = |key: &str| {
            let entry = entries.iter().find(|entry| entry.key == key).unwrap();
            assert_ne!(entry.state, "unreadable", "{key}");
            entry.clone()
        };
        let mut b = Stage::restore(content("b")).await.unwrap();
        let mut expected = vec![2u8; 4096];
        expected.extend_from_slice(b"more");
        assert_eq!(b.read_at(0, 8192).await.unwrap(), expected);
        let mut c = Stage::restore(content("c")).await.unwrap();
        assert_eq!(c.read_at(0, 64).await.unwrap(), b"new file");
        let a_entry = entries.iter().find(|entry| entry.key == "a").unwrap();
        assert_eq!(a_entry.state, "unreadable", "a stays quarantined");
        drop(b);
        drop(c);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_replay_keeps_a_damaged_wal_in_place_until_it_can_replay() {
        let root = std::env::temp_dir().join(format!(
            "r2-damage-failed-replay-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let a_path = root.join("a.data");
        let b_path = root.join("b.data");
        let wal = stage_wal::wal_path(&a_path);
        let mut a = Stage::create(a_path.clone(), "a".into(), 1).await.unwrap();
        let mut b = Stage::create(b_path.clone(), "b".into(), 1).await.unwrap();
        a.write_durable(0, &[1u8; 4096], 1).await.unwrap();
        b.write_durable(0, b"old!", 1).await.unwrap();
        b.checkpoint_durable().await.unwrap();
        b.write_durable(0, b"new!", 2).await.unwrap();
        drop(a);
        drop(b);
        // c is known only from the WAL: its manifest never made it.
        let c_path = root.join("c.data");
        tokio::fs::write(&c_path, b"").await.unwrap();
        stage_wal::append_record(
            &wal,
            &stage_wal::WalRecord {
                lsn: 0,
                generation: 1,
                op: stage_wal::WalOp::Write,
                offset: 0,
                resulting_size: 3,
                mtime_secs: 1,
                dirty_at_ms: 1,
                data_name: "c.data".into(),
                key: "c".into(),
                payload: b"ccc".to_vec(),
            },
        )
        .await
        .unwrap();
        // Media damage inside a's acknowledged record...
        let mut bytes = tokio::fs::read(&wal).await.unwrap();
        bytes[1024] ^= 0xff;
        tokio::fs::write(&wal, &bytes).await.unwrap();
        // ...and b's acknowledged overwrite never reached its data file, whose
        // size still matches its manifest, so no size check can notice.
        overwrite(&b_path, 0, b"old!").await;
        stage_wal::fail_replay_of(&b_path, true);
        stage_wal::fail_replay_of(&c_path, true);
        stage_wal::forget_append_state(&wal).await;

        let errors: std::collections::HashMap<_, _> = replay_write_intents(&root)
            .await
            .unwrap()
            .into_iter()
            .collect();
        let entries = recovery_entries(&root).await.unwrap();
        let b_entry = entries.iter().find(|entry| entry.key == "b").unwrap();
        assert_eq!(
            b_entry.state, "replay_pending",
            "b would be restored, and uploaded, with its old bytes"
        );
        assert!(errors
            .get(&b_path.with_extension("write.json"))
            .is_some_and(|error| error.contains("injected replay failure")));
        assert!(
            damaged_wal_copies(&root).await.is_empty(),
            "b's acknowledged records exist only in this WAL"
        );
        let a_entry = entries.iter().find(|entry| entry.key == "a").unwrap();
        assert_eq!(a_entry.state, "unreadable");
        let c_entry = entries
            .iter()
            .find(|entry| entry.key == "c")
            .expect("a stage known only from the WAL is listed, not dropped");
        assert_eq!(c_entry.state, "replay_pending");

        // Once b and c replay, the WAL is set aside and both come back whole.
        stage_wal::fail_replay_of(&b_path, false);
        stage_wal::fail_replay_of(&c_path, false);
        stage_wal::forget_append_state(&wal).await;
        replay_write_intents(&root).await.unwrap();
        assert_eq!(damaged_wal_copies(&root).await.len(), 1);
        let entries = recovery_entries(&root).await.unwrap();
        let b_entry = entries.iter().find(|entry| entry.key == "b").unwrap();
        assert!(!["unreadable", "replay_pending"].contains(&b_entry.state.as_str()));
        let mut b = Stage::restore(b_entry.clone()).await.unwrap();
        assert_eq!(b.read_at(0, 8).await.unwrap(), b"new!");
        let c_entry = entries.iter().find(|entry| entry.key == "c").unwrap();
        let mut c = Stage::restore(c_entry.clone()).await.unwrap();
        assert_eq!(c.read_at(0, 8).await.unwrap(), b"ccc");
        let a_entry = entries.iter().find(|entry| entry.key == "a").unwrap();
        assert_eq!(a_entry.state, "unreadable");
        drop(b);
        drop(c);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn proven_damage_found_while_mounted_never_blocks_writes() {
        let root = std::env::temp_dir().join(format!(
            "r2-live-damage-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let wal = stage_wal::wal_path(&path);
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        stage.write_durable(0, &[1u8; 4096], 1).await.unwrap();
        stage.write_durable(4096, &[2u8; 4096], 2).await.unwrap();
        let mut bytes = tokio::fs::read(&wal).await.unwrap();
        bytes[1024] ^= 0xff;
        tokio::fs::write(&wal, &bytes).await.unwrap();
        // The next append has to look at the tail and finds the damage.
        stage_wal::forget_append_state(&wal).await;
        stage.write_durable(8192, b"more", 3).await.unwrap();
        let after = tokio::fs::read(&wal).await.unwrap();
        assert_eq!(
            &after[..bytes.len()],
            bytes.as_slice(),
            "damage is never cut"
        );
        drop(stage);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn sequential_writes_reads_and_uploads_never_rescan_the_shared_wal() {
        let root = std::env::temp_dir().join(format!(
            "r2-no-wal-rescan-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join("record.data");
        let wal = stage_wal::wal_path(&path);
        let mut stage = Stage::create(path.clone(), "key".into(), 1).await.unwrap();
        let chunk = 4096usize;
        let writes = 200usize;
        let first = vec![1u8; chunk];
        stage.write_durable(0, &first, 1).await.unwrap();
        let mut expected = first;
        // Whatever the first write needed, every later write and read must be
        // O(record): the WAL is shared by the whole folder and can be huge.
        let reads_after_first_write = stage_wal::wal_read_count(&wal);
        for index in 1..writes {
            let offset = (index * chunk) as u64;
            let payload = vec![(index % 251) as u8; chunk];
            stage.write_durable(offset, &payload, 1).await.unwrap();
            assert_eq!(stage.read_at(offset, chunk).await.unwrap(), payload);
            expected.extend_from_slice(&payload);
        }
        assert_eq!(
            stage.checkpoint_lsn, 0,
            "stay below the checkpoint threshold"
        );
        assert_eq!(
            stage_wal::wal_read_count(&wal),
            reads_after_first_write,
            "a write or read rescanned the WAL"
        );
        // An upload checkpoints in O(1): below the compaction threshold it
        // neither replays nor rewrites the WAL.
        stage.upload_snapshot().await.unwrap();
        assert_eq!(stage_wal::wal_read_count(&wal), reads_after_first_write);
        drop(stage);

        replay_write_intents(&root).await.unwrap();
        let record = recovery_entries(&root).await.unwrap().remove(0);
        let mut restored = Stage::restore(record).await.unwrap();
        assert_eq!(restored.read_at(0, expected.len()).await.unwrap(), expected);
        drop(restored);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "local WAL ACK benchmark; run intentionally on the target filesystem"]
    async fn wal_ack_latency_matrix() {
        let sizes = [4 * 1024usize, 128 * 1024, 1024 * 1024];
        let concurrency = [1usize, 10, 100];
        for size in sizes {
            for files in concurrency {
                let root = std::env::temp_dir().join(format!(
                    "r2-wal-bench-{}-{}-{}-{}",
                    std::process::id(),
                    size,
                    files,
                    chrono::Utc::now().timestamp_nanos_opt().unwrap()
                ));
                tokio::fs::create_dir_all(&root).await.unwrap();
                let before = stage_commit::metrics();
                let started = Instant::now();
                let payload = vec![7u8; size];
                let mut tasks = Vec::new();
                for index in 0..files {
                    let path = root.join(format!("{index}.data"));
                    let payload = payload.clone();
                    tasks.push(tokio::spawn(async move {
                        let mut stage = Stage::create(path, format!("bench/{index}"), 1)
                            .await
                            .unwrap();
                        let ack_started = Instant::now();
                        stage.write_durable(0, &payload, 1).await.unwrap();
                        ack_started.elapsed().as_micros()
                    }));
                }
                let mut ack_micros = Vec::with_capacity(files);
                for task in tasks {
                    ack_micros.push(task.await.unwrap());
                }
                ack_micros.sort_unstable();
                let percentile = |values: &[u128], pct: usize| -> u128 {
                    if values.is_empty() {
                        return 0;
                    }
                    let index = ((values.len() - 1) * pct) / 100;
                    values[index]
                };
                let elapsed = started.elapsed();
                let wal_bytes = tokio::fs::metadata(root.join(".stage.wal"))
                    .await
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                let recovery_started = Instant::now();
                replay_write_intents(&root).await.unwrap();
                let recovery_ms = recovery_started.elapsed().as_millis();
                let after = stage_commit::metrics();
                let sync_bytes = after.sync_bytes.saturating_sub(before.sync_bytes);
                let payload_bytes = (size as u64).saturating_mul(files as u64);
                eprintln!(
                    "wal_ack size={} files={} elapsed_ms={} ack_p50_us={} ack_p95_us={} sync_batches={} sync_files={} sync_parents={} sync_bytes={} sync_gib={:.6} payload_gib={:.6} wal_bytes={} wal_gib={:.6} workers_started={} recovery_ms={} cpu=external_time_l",
                    size,
                    files,
                    elapsed.as_millis(),
                    percentile(&ack_micros, 50),
                    percentile(&ack_micros, 95),
                    after.sync_batches.saturating_sub(before.sync_batches),
                    after.sync_files.saturating_sub(before.sync_files),
                    after.sync_parents.saturating_sub(before.sync_parents),
                    sync_bytes,
                    sync_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                    payload_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                    wal_bytes,
                    wal_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                    after.workers_started.saturating_sub(before.workers_started),
                    recovery_ms,
                );
                tokio::fs::remove_dir_all(root).await.unwrap();
            }
        }
    }

    #[tokio::test]
    #[ignore = "snapshot copy measurement; run intentionally on the target filesystem"]
    async fn snapshot_copy_measurement() {
        let root = std::env::temp_dir().join(format!(
            "r2-snapshot-copy-bench-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let path = root.join("record.data");
        let mut stage = Stage::create(path, "snapshot/bench".into(), 1)
            .await
            .unwrap();
        let payload = vec![3u8; 16 * 1024 * 1024];
        for (index, chunk) in payload.chunks(1024 * 1024).enumerate() {
            stage
                .write_durable((index * 1024 * 1024) as u64, chunk, 1)
                .await
                .unwrap();
        }
        let started = Instant::now();
        let snapshot = stage.upload_snapshot().await.unwrap();
        let elapsed = started.elapsed();
        let source_len = tokio::fs::metadata(stage.path()).await.unwrap().len();
        let snapshot_len = tokio::fs::metadata(&snapshot.path).await.unwrap().len();
        eprintln!(
            "snapshot_copy source_bytes={} snapshot_bytes={} elapsed_ms={} copy_impl=std_fs_copy_spawn_blocking",
            source_len,
            snapshot_len,
            elapsed.as_millis()
        );
        drop(stage);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    fn idle_stage() -> (bool, bool, FlushState) {
        (true, false, FlushState::Idle)
    }

    // ---- flush debounce ----

    #[test]
    fn a_clean_stage_is_never_uploaded() {
        let now = Instant::now();
        assert!(!should_flush(
            false,
            false,
            FlushState::Idle,
            Duration::from_secs(60),
            now
        ));
        // Not even when something asked for a flush explicitly.
        assert!(!should_flush(
            false,
            true,
            FlushState::Idle,
            Duration::from_secs(60),
            now
        ));
    }

    #[test]
    fn a_file_still_being_written_waits_out_the_quiet_window() {
        let now = Instant::now();
        let (dirty, requested, state) = idle_stage();
        assert!(!should_flush(
            dirty,
            requested,
            state,
            Duration::from_millis(500),
            now
        ));
        assert!(!should_flush(
            dirty,
            requested,
            state,
            FLUSH_DEBOUNCE - Duration::from_millis(1),
            now
        ));
        assert!(should_flush(dirty, requested, state, FLUSH_DEBOUNCE, now));
    }

    #[test]
    fn an_end_of_copy_timestamp_skips_the_quiet_window() {
        let now = Instant::now();
        assert!(should_flush(
            true,
            true,
            FlushState::Idle,
            Duration::ZERO,
            now
        ));
    }

    #[test]
    fn an_upload_in_flight_is_never_started_twice() {
        let now = Instant::now();
        assert!(!should_flush(
            true,
            false,
            FlushState::Uploading,
            Duration::from_secs(60),
            now
        ));
        // Even the end-of-copy trigger must not start a second upload.
        assert!(!should_flush(
            true,
            true,
            FlushState::Uploading,
            Duration::from_secs(60),
            now
        ));
    }

    #[test]
    fn a_failed_upload_is_left_alone_until_its_cooldown_expires() {
        let now = Instant::now();
        let cooling = FlushState::Failed {
            retry_after: now + FLUSH_RETRY_COOLDOWN,
        };
        assert!(!should_flush(
            true,
            false,
            cooling,
            Duration::from_secs(60),
            now
        ));
        assert!(!should_flush(
            true,
            true,
            cooling,
            Duration::from_secs(60),
            now
        ));

        let expired = FlushState::Failed {
            retry_after: now - Duration::from_secs(1),
        };
        assert!(should_flush(
            true,
            false,
            expired,
            Duration::from_secs(60),
            now
        ));
    }

    #[test]
    fn a_retry_that_is_due_still_respects_the_quiet_window() {
        let now = Instant::now();
        let expired = FlushState::Failed {
            retry_after: now - Duration::from_secs(1),
        };
        assert!(!should_flush(
            true,
            false,
            expired,
            Duration::from_millis(100),
            now
        ));
    }

    // ---- eviction eligibility ----

    #[test]
    fn only_a_clean_idle_stage_may_be_dropped() {
        assert!(can_evict(false, FlushState::Idle));

        // Dropping any of these would delete the only copy of a write.
        assert!(!can_evict(true, FlushState::Idle));
        assert!(!can_evict(false, FlushState::Uploading));
        assert!(!can_evict(true, FlushState::Uploading));
        assert!(!can_evict(
            false,
            FlushState::Failed {
                retry_after: Instant::now()
            }
        ));
    }

    // ---- generation conflicts ----

    #[test]
    fn an_upload_only_settles_the_generation_it_captured() {
        assert!(upload_settles_stage(4, 4));
        // A write landed mid-upload: what is in the bucket is already stale.
        assert!(!upload_settles_stage(4, 5));
    }

    // ---- read-modify-write decision ----

    #[test]
    fn an_empty_object_is_staged_without_a_download() {
        assert_eq!(stage_init(0), StageInit::Empty);
    }

    #[test]
    fn an_object_with_content_is_downloaded_before_it_is_modified() {
        assert_eq!(stage_init(1), StageInit::Download);
        assert_eq!(stage_init(RMW_DOWNLOAD_CAP), StageInit::Download);
    }

    #[test]
    fn an_object_past_the_cap_is_refused_rather_than_downloaded() {
        assert_eq!(stage_init(RMW_DOWNLOAD_CAP + 1), StageInit::TooLarge);
        assert_eq!(stage_init(u64::MAX), StageInit::TooLarge);
    }

    // ---- multipart split ----

    #[test]
    fn parts_cover_the_whole_file_exactly_once() {
        for size in [
            MULTIPART_THRESHOLD + 1,
            PART_SIZE * 3,
            PART_SIZE * 3 + 1,
            PART_SIZE * 7 - 1,
        ] {
            let parts = part_count(size);
            let mut covered = 0u64;
            let mut previous_end = 0u64;
            for index in 0..parts {
                let (start, len) = part_range(size, index);
                assert_eq!(
                    start,
                    previous_end,
                    "part {} must start where {} ended",
                    index,
                    index.saturating_sub(1)
                );
                assert!(len > 0, "part {} of {} bytes is empty", index, size);
                assert!(len <= PART_SIZE);
                covered += len;
                previous_end = start + len;
            }
            assert_eq!(covered, size, "parts must cover {} bytes exactly", size);
        }
    }

    #[test]
    fn a_file_shorter_than_one_part_is_still_one_part() {
        assert_eq!(part_count(1), 1);
        assert_eq!(part_range(1, 0), (0, 1));
        // An empty file never reaches the multipart path, but must not produce
        // a zero-part upload if it somehow did.
        assert_eq!(part_count(0), 1);
    }

    #[test]
    fn the_upload_thresholds_match_the_apps_own_upload_path() {
        assert_eq!(MULTIPART_THRESHOLD, 100 * 1024 * 1024);
        assert_eq!(PART_SIZE, 20 * 1024 * 1024);
    }

    // ---- staging file IO ----

    async fn temp_stage(name: &str) -> Stage {
        let path = std::env::temp_dir().join(format!(
            "r2-mount-stage-{}-{}-{}",
            std::process::id(),
            name,
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        Stage::create(path, "obj".to_string(), 0)
            .await
            .expect("create stage")
    }

    #[tokio::test]
    async fn writes_land_at_their_offset_and_extend_the_file() {
        let mut stage = temp_stage("write").await;
        stage.write_at(0, b"hello").await.expect("write");
        assert_eq!(stage.size, 5);

        stage.write_at(5, b" world").await.expect("write");
        assert_eq!(stage.size, 11);
        assert_eq!(stage.read_at(0, 11).await.expect("read"), b"hello world");

        let path = stage.path().to_path_buf();
        drop(stage);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn a_gap_between_writes_reads_back_as_zeros() {
        // Sparse writes are normal — the OS may deliver a copy out of order.
        let mut stage = temp_stage("sparse").await;
        stage.write_at(4, b"tail").await.expect("write");
        assert_eq!(stage.size, 8);
        assert_eq!(
            stage.read_at(0, 8).await.expect("read"),
            b"\0\0\0\0tail".to_vec()
        );

        let path = stage.path().to_path_buf();
        drop(stage);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn reads_stop_at_the_end_of_the_staged_content() {
        let mut stage = temp_stage("eof").await;
        stage.write_at(0, b"abc").await.expect("write");

        assert_eq!(stage.read_at(0, 100).await.expect("read"), b"abc");
        assert!(stage.read_at(3, 10).await.expect("read").is_empty());
        assert!(stage.read_at(99, 10).await.expect("read").is_empty());
        assert!(stage.read_at(0, 0).await.expect("read").is_empty());

        let path = stage.path().to_path_buf();
        drop(stage);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn truncation_shrinks_and_grows_the_staged_size() {
        let mut stage = temp_stage("truncate").await;
        stage.write_at(0, b"abcdef").await.expect("write");

        stage.truncate(3).await.expect("truncate");
        assert_eq!(stage.size, 3);
        assert_eq!(stage.read_at(0, 10).await.expect("read"), b"abc");

        stage.truncate(0).await.expect("truncate");
        assert_eq!(stage.size, 0);
        assert!(stage.read_at(0, 10).await.expect("read").is_empty());

        let path = stage.path().to_path_buf();
        drop(stage);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn every_write_advances_the_generation() {
        let mut stage = temp_stage("generation").await;
        stage.flush_requested = true;

        stage.mark_dirty(100);
        assert_eq!(stage.dirty_gen, 1);
        assert!(stage.dirty);
        assert_eq!(stage.mtime_secs, 100);
        // A write after the end-of-copy timestamp means the copy is not over.
        assert!(!stage.flush_requested);

        stage.mark_dirty(101);
        assert_eq!(stage.dirty_gen, 2);

        let path = stage.path().to_path_buf();
        drop(stage);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn a_freshly_written_stage_becomes_due_once_it_goes_quiet() {
        let mut stage = temp_stage("due").await;
        stage.write_at(0, b"x").await.expect("write");
        stage.mark_dirty(1);

        assert!(!stage.is_due(Instant::now()));
        assert!(stage.is_due(stage.last_write + FLUSH_DEBOUNCE));

        let path = stage.path().to_path_buf();
        drop(stage);
        let _ = std::fs::remove_file(path);
    }
}
