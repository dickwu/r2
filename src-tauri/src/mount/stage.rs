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
    /// Key this stage belongs to, so the flusher does not have to re-resolve
    /// the inode — and stays correct if the inode is re-keyed by a rename.
    pub key: String,
    pub size: u64,
    pub mtime_secs: u32,
    pub dirty: bool,
    /// Bumped by every write. Captured before an upload so a write that lands
    /// mid-upload is noticed instead of being lost.
    pub dirty_gen: u64,
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
}

#[derive(Serialize, Deserialize)]
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
    let mut file = OpenOptions::new()
        .write(true)
        .read(true)
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
    drop(file);
    tokio::fs::rename(&temporary, path).await?;
    sync_parent(path).await
}

pub async fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent).await?.sync_all().await?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub async fn replay_write_intents(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let mut errors = Vec::new();
    let mut dir = tokio::fs::read_dir(root).await.map_err(|e| e.to_string())?;
    while let Some(entry) = dir.next_entry().await.map_err(|e| e.to_string())? {
        if entry.file_name().to_string_lossy().ends_with(".write.json") {
            if let Err(error) = replay_write(root, &entry.path()).await {
                errors.push((entry.path(), error.to_string()));
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
            key,
            size: 0,
            mtime_secs,
            dirty: false,
            dirty_gen: 0,
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
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&record.path)
            .await?;
        if file.metadata().await?.len() != record.size {
            return Err(std::io::Error::other(
                "Stage size does not match its durable journal",
            ));
        }
        Ok(Self {
            file,
            path: record.path,
            key: record.key,
            size: record.size,
            mtime_secs: record.mtime_secs,
            dirty: record.dirty,
            dirty_gen: record.generation,
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

    pub async fn persist(&mut self) -> std::io::Result<()> {
        self.file.flush().await?;
        self.file.sync_all().await?;
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
            },
        )
        .await
    }

    pub async fn replay_pending_write(&mut self) -> std::io::Result<()> {
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
            self.dirty_gen = record.generation;
            self.dirty = record.dirty;
            self.mtime_secs = record.mtime_secs;
            self.publication_guard = record.publication_guard;
            self.last_write = Instant::now();
        }
        Ok(())
    }

    async fn durable_change(
        &mut self,
        change: DurableChange,
        size: u64,
        mtime: u32,
    ) -> std::io::Result<()> {
        self.replay_pending_write().await?;
        let intent = WriteIntent {
            state: StageRecovery {
                key: self.key.clone(),
                size,
                mtime_secs: mtime,
                generation: self.dirty_gen.saturating_add(1),
                dirty: true,
                state: "waiting".into(),
                error: None,
                path: self.path.clone(),
                snapshot: self.snapshot.clone(),
                publication_guard: self.publication_guard.clone(),
            },
            change,
        };
        let path = self.path.with_extension("write.json");
        write_json_atomic(&path, &intent).await?;
        // Once the intent is durable, an older in-flight upload must no longer
        // be allowed to mark this stage clean, even if disk I/O now fails.
        self.dirty = true;
        self.dirty_gen = intent.state.generation;
        self.last_write = Instant::now();
        self.size = size;
        self.mtime_secs = mtime;
        self.replay_pending_write().await
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
        self.file.flush().await?;
        self.file.sync_all().await?;
        let path = self
            .path
            .with_extension(format!("g{}.snapshot", self.dirty_gen));
        tokio::fs::copy(&self.path, &path).await?;
        File::open(&path).await?.sync_all().await?;
        let snapshot = UploadSnapshot {
            path,
            generation: self.dirty_gen,
            size: self.size,
            publication_guard: self.publication_guard.clone(),
        };
        self.snapshot = Some(snapshot.clone());
        self.persist().await?;
        Ok(snapshot)
    }

    pub async fn remove_files(&self) {
        let _ = tokio::fs::remove_file(self.path.with_extension("write.json")).await;
        let _ = tokio::fs::remove_file(self.manifest_path()).await;
        let _ = tokio::fs::remove_file(&self.path).await;
        if let Some(snapshot) = &self.snapshot {
            snapshot.remove().await;
        }
        let _ = sync_parent(&self.path).await;
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
        self.file.seek(SeekFrom::Start(offset)).await?;
        self.file.write_all(data).await?;
        self.file.flush().await?;
        self.size = self.size.max(offset.saturating_add(data.len() as u64));
        Ok(())
    }

    /// Reads up to `count` bytes at `offset`, stopping at end of file.
    pub async fn read_at(&mut self, offset: u64, count: usize) -> std::io::Result<Vec<u8>> {
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
        let records = recovery_entries(&root).await.unwrap();
        let mut restored = Stage::restore(records.into_iter().next().unwrap())
            .await
            .unwrap();
        assert_eq!(restored.read_at(0, 100).await.unwrap(), b"co");
        drop(restored);
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
