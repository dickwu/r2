//! NFSv3 view of an S3-compatible bucket, read-only or writable.
//!
//! Object storage has no directories, so the tree is synthesized from
//! `ListObjectsV2` with `delimiter="/"`: common prefixes become directories and
//! the remaining keys become files. Every path the client has seen is assigned a
//! stable `fileid3` for the lifetime of the mount, because NFS file handles are
//! opaque ids the client may hold on to indefinitely.
//!
//! Writes never go straight to the bucket. S3 has no partial update and NFSv3
//! has no close hook, so a modified file is written to a local staging file
//! (see [`super::stage`]) and uploaded once the client stops writing to it.
//! Namespace changes — create, mkdir, remove, rename — do go straight through,
//! which is what keeps `lookup` and `readdir` free of overlay logic: S3 stays
//! the single source of truth for which names exist, and a stage only overrides
//! the content and size of a file that already exists there.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::primitives::{ByteStream, Length};
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart, MetadataDirective};
use aws_sdk_s3::Client;

use crate::providers::operation::{
    execute as execute_storage_operation, AttemptError, OperationContext, OperationError,
    OperationKind, OperationMetrics,
};
use crate::providers::resources::{ByteLease, ResourceKind};
use crate::providers::s3_client::{describe_s3_error, StorageErrorClass};
use futures_util::{stream, StreamExt};
use nfsserve::nfs::{
    fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfstime3, sattr3, set_atime, set_mtime,
    set_size3, specdata3,
};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, RwLock as AsyncRwLock, Semaphore};
use tokio::task::JoinHandle;

use super::progress::{MountProgress, TransferKind, TransferTracker};
use super::read_cache::{self, ReadCache};
use super::stage::{self, FlushState, Stage, StageInit, UploadSnapshot};
use fence::{FenceGuard, FencePath, NamespaceFences};

/// Reserved root id. `0` is reserved by the protocol and must never be used.
const ROOT_ID: fileid3 = 1;
/// How long a directory listing is served before it is re-fetched from S3.
const DIR_CACHE_TTL: Duration = Duration::from_secs(30);
const NEGATIVE_LOOKUP_TTL: Duration = Duration::from_secs(2);
const LIST_PAGE_SIZE: i32 = 1000;
/// Staged-size growth that earns a queued transfer another progress event.
/// A 1 GiB copy reports ~128 times over its whole staging phase — enough for
/// a live total, nowhere near one event per 128 KiB write.
const WAITING_REPORT_STEP: u64 = 8 * 1024 * 1024;
/// Synthetic size reported for directories, matching a typical unix filesystem.
const DIR_SIZE: u64 = 4096;
const DIR_MODE: u32 = 0o755;
const FILE_MODE: u32 = 0o644;

/// Objects a directory rename may move. An NFS RENAME has to finish inside the
/// client's retransmission window, and a bigger move belongs in the app's Batch
/// Move, which has progress, pause and resume.
const MAX_RENAME_KEYS: usize = 1000;
/// Server-side copies issued at once while renaming a directory.
const RENAME_COPY_CONCURRENCY: usize = 8;
/// Times an operation re-derives the key it has to fence after finding that
/// a rename moved it during the fence wait. Each retry follows a rename that
/// completed in between, so running out means the name is being moved
/// continuously and the client is told to try again later.
const FENCED_KEY_ATTEMPTS: usize = 16;

// ============ Key helpers (pure) ============

/// Normalizes a directory key to the form used as a `ListObjectsV2` prefix:
/// the bucket root is `""` and every other directory ends with `/`.
fn normalize_dir_key(key: &str) -> String {
    let key = key.strip_prefix('/').unwrap_or(key);
    if key.is_empty() {
        String::new()
    } else if key.ends_with('/') {
        key.to_string()
    } else {
        format!("{}/", key)
    }
}

/// Last path component of a key. Directory keys carry a trailing slash that is
/// not part of the name.
fn entry_name(key: &str) -> &str {
    let trimmed = key.strip_suffix('/').unwrap_or(key);
    match trimmed.rfind('/') {
        Some(idx) => &trimmed[idx + 1..],
        None => trimmed,
    }
}

/// Key of `name` inside the directory `dir_key` (which is `""` or ends with `/`).
fn child_key(dir_key: &str, name: &str, is_dir: bool) -> String {
    let mut key = String::with_capacity(dir_key.len() + name.len() + 1);
    key.push_str(dir_key);
    key.push_str(name);
    if is_dir {
        key.push('/');
    }
    key
}

/// New key for `key` when the directory `from_prefix` is renamed to `to_prefix`,
/// or `None` when the key is not under that directory.
fn rewrite_key(key: &str, from_prefix: &str, to_prefix: &str) -> Option<String> {
    key.strip_prefix(from_prefix)
        .map(|rest| format!("{}{}", to_prefix, rest))
}

/// Value for the `x-amz-copy-source` header.
///
/// The AWS SDK puts this string straight into the header and S3 URL-decodes it,
/// so the encoding is ours to do: an unescaped `%` or `+` changes which object
/// is copied, and a key with a non-ASCII character is not a legal header value
/// at all. Slashes stay literal — they separate the bucket from the key and the
/// key's own path components.
///
/// This is stricter than `r2::objects::copy_object`, which sends the raw key. A
/// mount copies whatever names the operating system hands it, so unlike the
/// file list it cannot assume they are URL-safe.
fn encode_copy_source(bucket: &str, key: &str) -> String {
    let mut source = String::with_capacity(bucket.len() + key.len() + 1);
    source.push_str(bucket);
    for segment in key.split('/') {
        source.push('/');
        source.push_str(&urlencoding::encode(segment));
    }
    source
}

/// What `create` should do about a name that may already be taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreateAction {
    /// Nothing there — write the zero-byte object.
    Create,
    /// Something is there and the caller did not ask to replace it. CREATE is
    /// also how a client opens a file it expects to already exist, so writing
    /// an empty object here would blank content nobody asked to lose.
    OpenExisting,
    /// Something is there and the caller asked for size 0 — a real truncate.
    Truncate,
}

fn create_action(exists: bool, truncate_requested: bool) -> CreateAction {
    match (exists, truncate_requested) {
        (false, _) => CreateAction::Create,
        (true, false) => CreateAction::OpenExisting,
        (true, true) => CreateAction::Truncate,
    }
}

/// Whether a rename may go ahead over whatever is already at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenameTarget {
    /// Free, or occupied by something this rename is allowed to replace.
    Replace,
    /// A file cannot replace a directory or the other way round. Object storage
    /// would happily hold both `b` and `b/`, but a listing renders the
    /// directory and hides the file, so the moved data would vanish.
    KindMismatch,
    /// A directory can only replace an empty one; anything else would silently
    /// merge two trees.
    NotEmpty,
}

fn classify_rename_target(
    source_is_dir: bool,
    destination: Option<EntryKind>,
    destination_dir_is_empty: bool,
) -> RenameTarget {
    let Some(destination) = destination else {
        return RenameTarget::Replace;
    };
    let destination_is_dir = destination == EntryKind::Dir;
    if destination_is_dir != source_is_dir {
        return RenameTarget::KindMismatch;
    }
    if destination_is_dir && !destination_dir_is_empty {
        return RenameTarget::NotEmpty;
    }
    RenameTarget::Replace
}

/// Whether a `ListObjectsV2` page taken under `dir_key` shows anything besides
/// the directory's own folder marker.
fn dir_listing_is_empty(dir_key: &str, keys: &[&str], common_prefixes: usize) -> bool {
    common_prefixes == 0 && keys.iter().all(|key| *key == dir_key)
}

/// Where a `readdir` continuation resumes in a name-sorted child list.
///
/// The cursor is resolved by name rather than by position so a directory that
/// was re-listed between two `readdir` calls still resumes at the right place,
/// even if the entry the client last saw has since been deleted.
fn resume_index(children: &[DirChild], after_name: Option<&str>) -> usize {
    match after_name {
        None => 0,
        Some(name) => children.partition_point(|child| child.name.as_str() <= name),
    }
}

/// End index of the `readdir` page starting at `start`, and whether it reaches
/// the end of the directory.
///
/// `max_entries` is clamped to at least one entry: a page of nothing that is
/// not flagged as the end would make the client re-issue the same cookie
/// forever.
fn page_end(total: usize, start: usize, max_entries: usize) -> (usize, bool) {
    let end = start.saturating_add(max_entries.max(1)).min(total);
    (end, end >= total)
}

fn now_secs() -> u32 {
    u32::try_from(chrono::Utc::now().timestamp().max(0)).unwrap_or(u32::MAX)
}

/// NFS status for an S3 error code, so the client reports "permission denied"
/// and "no such file" as themselves rather than as a blanket I/O error.
fn status_for_s3_code(code: Option<&str>) -> nfsstat3 {
    match code {
        Some(
            "AccessDenied" | "AllAccessDisabled" | "InvalidAccessKeyId" | "SignatureDoesNotMatch",
        ) => nfsstat3::NFS3ERR_ACCES,
        Some("NoSuchKey" | "NoSuchBucket" | "NotFound") => nfsstat3::NFS3ERR_NOENT,
        Some("PreconditionFailed" | "ConditionalRequestConflict") => nfsstat3::NFS3ERR_IO,
        _ => nfsstat3::NFS3ERR_IO,
    }
}

fn map_s3_error<E, R>(error: &SdkError<E, R>) -> nfsstat3
where
    E: ProvideErrorMetadata,
{
    let code = match error {
        SdkError::ServiceError(service) => service.err().code(),
        _ => None,
    };
    status_for_s3_code(code)
}

// ============ Inode table ============

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Dir,
    File,
}

#[derive(Debug, Clone)]
struct Inode {
    key: String,
    parent: fileid3,
    kind: EntryKind,
    size: u64,
    mtime_secs: u32,
}

/// Bidirectional key ↔ id map. Ids are handed out monotonically and never
/// reused or evicted, so a file handle stays valid for the whole session.
struct InodeTable {
    next_id: fileid3,
    by_id: HashMap<fileid3, Inode>,
    by_key: HashMap<String, fileid3>,
}

impl InodeTable {
    fn new() -> Self {
        let root = Inode {
            key: String::new(),
            parent: ROOT_ID,
            kind: EntryKind::Dir,
            size: DIR_SIZE,
            mtime_secs: 0,
        };
        let mut by_id = HashMap::new();
        by_id.insert(ROOT_ID, root);
        let mut by_key = HashMap::new();
        by_key.insert(String::new(), ROOT_ID);

        Self {
            next_id: ROOT_ID + 1,
            by_id,
            by_key,
        }
    }

    /// Returns the stable id of `key`, allocating one the first time it is seen
    /// and refreshing the cached attributes on every later sighting.
    fn intern(
        &mut self,
        key: &str,
        parent: fileid3,
        kind: EntryKind,
        size: u64,
        mtime_secs: u32,
    ) -> fileid3 {
        if let Some(&id) = self.by_key.get(key) {
            if let Some(entry) = self.by_id.get_mut(&id) {
                entry.parent = parent;
                entry.kind = kind;
                entry.size = size;
                entry.mtime_secs = mtime_secs;
            }
            return id;
        }

        let id = self.next_id;
        self.next_id += 1;
        self.by_id.insert(
            id,
            Inode {
                key: key.to_string(),
                parent,
                kind,
                size,
                mtime_secs,
            },
        );
        self.by_key.insert(key.to_string(), id);
        id
    }

    fn get(&self, id: fileid3) -> Option<&Inode> {
        self.by_id.get(&id)
    }

    fn remove(&mut self, id: fileid3) {
        if let Some(inode) = self.by_id.remove(&id) {
            if self.by_key.get(&inode.key) == Some(&id) {
                self.by_key.remove(&inode.key);
            }
        }
    }

    fn set_attrs(&mut self, id: fileid3, size: u64, mtime_secs: u32) {
        if let Some(inode) = self.by_id.get_mut(&id) {
            inode.size = size;
            inode.mtime_secs = mtime_secs;
        }
    }

    /// Moves one inode to a new key without renumbering it.
    ///
    /// NFS RENAME does not invalidate the client's file handle — the file keeps
    /// its identity across the move — so the id has to survive and only the key
    /// it points at may change.
    fn rekey(&mut self, id: fileid3, new_key: &str, new_parent: fileid3) {
        let old_key = match self.by_id.get_mut(&id) {
            Some(inode) => {
                inode.parent = new_parent;
                std::mem::replace(&mut inode.key, new_key.to_string())
            }
            None => return,
        };
        if self.by_key.get(&old_key) == Some(&id) {
            self.by_key.remove(&old_key);
        }
        self.by_key.insert(new_key.to_string(), id);
    }

    /// Moves every inode under `from_prefix` onto `to_prefix`, returning how
    /// many were rewritten. Used by a directory rename, where the client keeps
    /// its handles for everything inside the moved tree.
    fn rekey_prefix(&mut self, from_prefix: &str, to_prefix: &str) -> usize {
        let moved: Vec<(fileid3, String)> = self
            .by_id
            .iter()
            .filter_map(|(&id, inode)| {
                rewrite_key(&inode.key, from_prefix, to_prefix).map(|key| (id, key))
            })
            .collect();

        for (id, new_key) in &moved {
            let old_key = self
                .by_id
                .get_mut(id)
                .map(|inode| std::mem::replace(&mut inode.key, new_key.clone()));
            if let Some(old_key) = old_key {
                if self.by_key.get(&old_key) == Some(id) {
                    self.by_key.remove(&old_key);
                }
            }
            self.by_key.insert(new_key.clone(), *id);
        }

        moved.len()
    }
}

// ============ Directory cache ============

#[derive(Debug, Clone)]
struct DirChild {
    fileid: fileid3,
    name: String,
}

use directory_listing::{DirListing, DirectoryCookie};

#[derive(Debug, Clone)]
struct NegativeLookup {
    generation: u64,
    stored_at: Instant,
}

#[derive(Debug, Clone)]
struct PositiveLookup {
    generation: u64,
    fileid: fileid3,
    stored_at: Instant,
}

// ============ Flush events ============

/// Payload of `mount-flush-error`: one staged file could not be uploaded.
#[derive(Debug, Clone, Serialize)]
struct FlushErrorPayload {
    mount_id: String,
    bucket: String,
    key: String,
    error: String,
}

/// Everything an upload needs, captured under the stage lock so the upload
/// itself runs without holding it.
struct FlushJob {
    id: fileid3,
    key: String,
    snapshot: UploadSnapshot,
}

#[derive(Default)]
struct KeyLifecycle {
    // Lock order: namespace -> lifecycle -> publication -> stage -> registry.
    // Writes share the lifecycle permit with uploads, but use the stage mutex
    // only while changing data. Destructive operations wait for publication.
    access: AsyncRwLock<()>,
    publication: AsyncMutex<()>,
}

#[derive(Clone)]
struct ReadIdentity {
    etag: String,
    version_id: Option<String>,
    size: u64,
    observed_at: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RenameObject {
    from: String,
    to: String,
    source_etag: String,
    size: u64,
    destination_etag: Option<String>,
    phase: String,
    #[serde(default)]
    replaced_etag: Option<String>,
    #[serde(default)]
    source_version: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RenameJournal {
    from: String,
    to: String,
    token: String,
    objects: Vec<RenameObject>,
}

#[derive(Debug, Clone)]
struct UploadFailure {
    message: String,
    retryable: bool,
    uncertain: bool,
}
impl std::fmt::Display for UploadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl UploadFailure {
    fn local(message: impl ToString) -> Self {
        Self {
            message: message.to_string(),
            retryable: false,
            uncertain: false,
        }
    }
}
fn upload_error<E>(error: &SdkError<E>) -> UploadFailure
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    use crate::providers::s3_client::{s3_error_class, StorageErrorClass};
    let class = s3_error_class(error, true);
    UploadFailure {
        message: format!("{}: {}", class.label(), describe_s3_error(error)),
        retryable: class == StorageErrorClass::Transient,
        uncertain: class == StorageErrorClass::OutcomeUnknown,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FsHealthSnapshot {
    pub pending_uploads: usize,
    pub dirty_bytes: u64,
    pub oldest_dirty_ms: u64,
    pub last_error: Option<String>,
    pub last_successful_io: Option<i64>,
    pub resources: crate::providers::resources::ResourceSnapshot,
    pub operations: OperationMetrics,
    pub commit: super::stage_commit::CommitMetrics,
    pub fences: fence::FenceMetrics,
}

#[derive(Default, Clone)]
struct IoHealth {
    last_successful_io: Option<i64>,
    last_error: Option<String>,
}

type AsyncGate = AsyncMutex<()>;
type LookupKey = (fileid3, String);
type DirectoryFlights = HashMap<fileid3, Weak<AsyncGate>>;
type LookupFlights = HashMap<LookupKey, Weak<AsyncGate>>;

#[derive(Default, Clone)]
struct StageHealthStats {
    dirty: bool,
    size: u64,
    first_dirty_at: Option<i64>,
    last_error: Option<String>,
}

// ============ Filesystem ============
#[path = "directory_listing.rs"]
mod directory_listing;
#[path = "fence.rs"]
mod fence;
#[path = "namespace_recovery.rs"]
mod namespace_recovery;
#[cfg(test)]
#[path = "native_smoke_tests.rs"]
mod native_smoke_tests;
#[cfg(test)]
#[path = "nfs_protocol_tests.rs"]
mod protocol_tests;
#[path = "rename_copy.rs"]
mod rename_copy;

/// Cheap-clone handle to one mount's filesystem.
///
/// `NFSTcpListener::bind` takes the filesystem by value, so the manager keeps a
/// second handle to drain staged writes through at unmount; both must see the
/// same inode table and the same stages.
#[derive(Clone)]
pub struct S3NfsFs {
    inner: Arc<FsInner>,
}

pub struct FsInner {
    client: Client,
    bucket: String,
    endpoint: String,
    read_only: bool,
    /// Directory holding this mount's staging files.
    staging_root: PathBuf,
    inodes: RwLock<InodeTable>,
    dirs: RwLock<HashMap<fileid3, DirListing>>,
    /// Files with content that is not in the bucket yet.
    ///
    /// Two locks are involved and the order between them is fixed: the map may
    /// be locked while a stage is locked, but a stage is never *waited* on while
    /// the map is held. Everything that needs both takes the map first and only
    /// ever `try_lock`s the stage.
    stages: AsyncMutex<HashMap<fileid3, Arc<AsyncMutex<Stage>>>>,
    flush_slots: Arc<Semaphore>,
    namespace: AsyncRwLock<()>,
    key_lifecycles: std::sync::Mutex<HashMap<String, Weak<KeyLifecycle>>>,
    accepting_writes: AtomicBool,
    /// Cancel flag of every storage request this mount makes. Set only by a
    /// forced abort: the unmount drain still has to publish staged files after
    /// `accepting_writes` is cleared.
    shutdown: AtomicBool,
    directory_generation: AtomicU64,
    directory_cookies: RwLock<HashMap<(fileid3, fileid3), DirectoryCookie>>,
    directory_flights: AsyncMutex<DirectoryFlights>,
    lookup_flights: AsyncMutex<LookupFlights>,
    negative_lookups: RwLock<HashMap<(fileid3, String), NegativeLookup>>,
    positive_lookups: RwLock<HashMap<(fileid3, String), PositiveLookup>>,
    namespace_fences: Arc<NamespaceFences>,
    read_identities: AsyncMutex<HashMap<fileid3, ReadIdentity>>,
    /// Chunked cache behind the read path; see [`super::read_cache`].
    read_cache: ReadCache,
    /// Bounds background chunk prefetches so they never crowd out demand reads.
    prefetch_slots: Arc<Semaphore>,
    /// Reporter for the `mount-transfer` events, attached by the manager once
    /// the mount id exists. Absent in unit tests, which have no Tauri app.
    progress: OnceLock<MountProgress>,
    uid: u32,
    gid: u32,
    fsid: u64,
    transfer_config: OnceLock<crate::move_transfer::config::MoveConfig>,
    quota: AsyncMutex<super::quota::StageQuota>,
    pending_renames: RwLock<HashMap<PathBuf, (String, String)>>,
    io_health: std::sync::Mutex<IoHealth>,
    stage_health: std::sync::Mutex<HashMap<fileid3, StageHealthStats>>,
    quarantined: RwLock<Vec<stage::StageRecovery>>,
    recovery_errors: std::sync::Mutex<Vec<String>>,
}

impl S3NfsFs {
    #[cfg(test)]
    pub fn new(client: Client, bucket: String, read_only: bool, staging_root: PathBuf) -> Self {
        let endpoint = format!("mount-test:{bucket}");
        Self::new_with_endpoint(client, bucket, endpoint, read_only, staging_root)
    }

    pub fn new_with_endpoint(
        client: Client,
        bucket: String,
        endpoint: String,
        read_only: bool,
        staging_root: PathBuf,
    ) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bucket.hash(&mut hasher);
        let fsid = hasher.finish();

        Self {
            inner: Arc::new(FsInner {
                client,
                bucket,
                endpoint,
                read_only,
                staging_root,
                inodes: RwLock::new(InodeTable::new()),
                dirs: RwLock::new(HashMap::new()),
                stages: AsyncMutex::new(HashMap::new()),
                flush_slots: Arc::new(Semaphore::new(stage::MAX_CONCURRENT_FLUSHES)),
                namespace: AsyncRwLock::new(()),
                key_lifecycles: std::sync::Mutex::new(HashMap::new()),
                accepting_writes: AtomicBool::new(true),
                shutdown: AtomicBool::new(false),
                directory_generation: AtomicU64::new(1),
                directory_cookies: RwLock::new(HashMap::new()),
                directory_flights: AsyncMutex::new(HashMap::new()),
                lookup_flights: AsyncMutex::new(HashMap::new()),
                negative_lookups: RwLock::new(HashMap::new()),
                positive_lookups: RwLock::new(HashMap::new()),
                namespace_fences: Arc::new(NamespaceFences::default()),
                read_identities: AsyncMutex::new(HashMap::new()),
                read_cache: ReadCache::new(),
                prefetch_slots: Arc::new(Semaphore::new(read_cache::PREFETCH_CONCURRENCY)),
                progress: OnceLock::new(),
                uid: current_uid(),
                gid: current_gid(),
                fsid,
                transfer_config: OnceLock::new(),
                quota: AsyncMutex::new(super::quota::StageQuota::default()),
                pending_renames: RwLock::new(HashMap::new()),
                io_health: std::sync::Mutex::new(IoHealth::default()),
                stage_health: std::sync::Mutex::new(HashMap::new()),
                quarantined: RwLock::new(Vec::new()),
                recovery_errors: std::sync::Mutex::new(Vec::new()),
            }),
        }
    }

    /// Attaches the transfer-progress reporter. Called once by the manager,
    /// after the mount id is minted and before the server starts serving.
    pub fn set_progress(&self, app: tauri::AppHandle, mount_id: String) {
        let _ =
            self.inner
                .progress
                .set(MountProgress::new(app, mount_id, self.inner.bucket.clone()));
    }

    pub fn configure_transfer(&self, config: crate::move_transfer::config::MoveConfig) {
        let _ = self.inner.transfer_config.set(config);
    }

    pub async fn configure_quota(&self, limit: u64) {
        self.inner.quota.lock().await.limit = limit;
    }

    async fn reserve_stage(&self, id: fileid3, size: u64) -> Result<(), nfsstat3> {
        let bytes = size.checked_mul(2).ok_or(nfsstat3::NFS3ERR_NOSPC)?;
        if super::quota::available_space(&self.inner.staging_root)
            .is_ok_and(|free| free < 8 * 1024 * 1024)
        {
            return Err(nfsstat3::NFS3ERR_NOSPC);
        }
        if !self.inner.quota.lock().await.reserve(id, bytes) {
            return Err(nfsstat3::NFS3ERR_NOSPC);
        }
        Ok(())
    }

    async fn condition_supported(
        &self,
        condition: crate::providers::conditional::Condition,
    ) -> Result<bool, nfsstat3> {
        self.inner
            .transfer_config
            .get()
            .ok_or(nfsstat3::NFS3ERR_NOTSUPP)?
            .supports_condition(condition)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)
    }

    fn progress(&self) -> Option<&MountProgress> {
        self.inner.progress.get()
    }

    pub fn staging_root(&self) -> &Path {
        &self.inner.staging_root
    }

    fn lifecycle(&self, key: &str) -> Arc<KeyLifecycle> {
        let mut keys = self
            .inner
            .key_lifecycles
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        keys.retain(|_, value| value.strong_count() != 0);
        if let Some(lock) = keys.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(KeyLifecycle::default());
        keys.insert(key.to_string(), Arc::downgrade(&lock));
        lock
    }

    async fn fence_exact_key(&self, key: &str) -> FenceGuard {
        self.inner
            .namespace_fences
            .acquire(vec![FencePath::exact(key)])
            .await
    }

    async fn fence_exact_key_shared(&self, key: &str) -> FenceGuard {
        self.inner
            .namespace_fences
            .acquire_shared(vec![FencePath::exact(key)])
            .await
    }

    async fn fence_paths(&self, paths: Vec<FencePath>) -> FenceGuard {
        self.inner.namespace_fences.acquire(paths).await
    }

    async fn fence_paths_shared(&self, paths: Vec<FencePath>) -> FenceGuard {
        self.inner.namespace_fences.acquire_shared(paths).await
    }

    fn fence_path_for_inode(inode: &Inode) -> FencePath {
        match inode.kind {
            EntryKind::Dir => FencePath::prefix(normalize_dir_key(&inode.key)),
            EntryKind::File => FencePath::exact(inode.key.clone()),
        }
    }

    // Keys change only inside a rename, which holds exclusive fences on the
    // old key (a directory's whole prefix). A key derived before waiting on a
    // fence can be stale once the wait ends; one re-derived while the fence is
    // held stays valid until it is released, because any rename that could
    // move it — of the file or of an ancestor — overlaps the held path.

    /// Exclusive fence on the entry `name` names in `dirid` once the fence is
    /// held. The name is resolved before the wait to know what to fence and
    /// again after it, because a rename holding the fence meanwhile may have
    /// moved that id to another key; acting on the id alone would then delete
    /// the file under its new name.
    async fn fence_existing_child(
        &self,
        dirid: fileid3,
        name: &str,
    ) -> Result<(FenceGuard, fileid3, Inode), nfsstat3> {
        for _ in 0..FENCED_KEY_ATTEMPTS {
            let dir_key = normalize_dir_key(&self.dir_inode(dirid)?.key);
            let (id, target) = self.resolve_child(dirid, &dir_key, name).await?;
            let fence = self
                .fence_paths(vec![Self::fence_path_for_inode(&target)])
                .await;
            if normalize_dir_key(&self.dir_inode(dirid)?.key) != dir_key {
                continue;
            }
            let (current_id, current) = self.resolve_child_fenced(dirid, &dir_key, name).await?;
            if current_id == id && current.key == target.key {
                return Ok((fence, id, current));
            }
        }
        Err(nfsstat3::NFS3ERR_JUKEBOX)
    }

    /// Exclusive fence on the key `name` gets inside `dirid`, built from the
    /// directory's key as it stands once the fence is held — a directory
    /// renamed during the wait must not have its old path published again.
    /// Returns the directory key the child key was built from, and that key.
    async fn fence_new_child(
        &self,
        dirid: fileid3,
        name: &str,
        is_dir: bool,
    ) -> Result<(FenceGuard, String, String), nfsstat3> {
        for _ in 0..FENCED_KEY_ATTEMPTS {
            let dir_key = normalize_dir_key(&self.dir_inode(dirid)?.key);
            let key = child_key(&dir_key, name, is_dir);
            let fence = self.fence_exact_key(&key).await;
            if normalize_dir_key(&self.dir_inode(dirid)?.key) == dir_key {
                return Ok((fence, dir_key, key));
            }
        }
        Err(nfsstat3::NFS3ERR_JUKEBOX)
    }

    /// Shared fence on the key `id` has once the fence is held, with the inode
    /// as read under it.
    async fn fence_inode_shared(&self, id: fileid3) -> Result<(FenceGuard, Inode), nfsstat3> {
        for _ in 0..FENCED_KEY_ATTEMPTS {
            let key = self.inode(id)?.key;
            let fence = self.fence_exact_key_shared(&key).await;
            let inode = self.inode(id)?;
            if inode.key == key {
                return Ok((fence, inode));
            }
        }
        Err(nfsstat3::NFS3ERR_JUKEBOX)
    }

    fn storage_endpoint(&self) -> &str {
        &self.inner.endpoint
    }

    fn storage_scope(&self, scope: &str) -> String {
        if scope.is_empty() {
            self.inner.bucket.clone()
        } else {
            format!("{}:{scope}", self.inner.bucket)
        }
    }

    fn map_operation_error(&self, label: &str, error: OperationError) -> nfsstat3 {
        self.io_failed(format!("{label}: {error}"));
        match error.class() {
            StorageErrorClass::NeedsAuth => nfsstat3::NFS3ERR_ACCES,
            StorageErrorClass::NotFound => nfsstat3::NFS3ERR_NOENT,
            _ => nfsstat3::NFS3ERR_IO,
        }
    }

    fn read_operation_context<'a>(
        &'a self,
        kind: OperationKind,
        scope: &'a str,
        identity: &'a str,
        budget: Duration,
    ) -> OperationContext<'a> {
        OperationContext::new(
            kind,
            self.storage_endpoint(),
            scope,
            identity,
            tokio::time::Instant::now() + budget,
            &self.inner.shutdown,
        )
    }

    /// Refuses every new mutation. Storage requests keep running: in-flight
    /// operations settle and the unmount drain publishes what is staged,
    /// multipart uploads included.
    pub fn stop_accepting_writes(&self) {
        self.inner.accepting_writes.store(false, Ordering::SeqCst);
    }

    /// Forced teardown: cancels this mount's in-flight and queued storage
    /// requests at their next check. Nothing is lost — unpublished content
    /// stays staged for the next session — but no drain can publish after it.
    pub fn abort_storage_operations(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
    }

    pub async fn wait_for_mutations(&self) {
        drop(self.inner.namespace.write().await);
        drop(
            self.inner
                .namespace_fences
                .acquire(vec![FencePath::prefix("")])
                .await,
        );
    }

    pub async fn pending_upload_count(&self) -> usize {
        let handles: Vec<_> = self.inner.stages.lock().await.values().cloned().collect();
        let mut count = 0;
        for handle in handles {
            let stage = handle.lock().await;
            if stage.dirty && !stage.evicted {
                count += 1;
            }
        }
        count
            + self
                .inner
                .quarantined
                .read()
                .map(|records| records.len())
                .unwrap_or(1)
            + namespace_recovery::pending_operations(&self.inner.staging_root)
                .await
                .map(|ops| ops.len())
                .unwrap_or(1)
    }

    fn record_stage_health(&self, id: fileid3, stage: &Stage) {
        let mut health = self
            .inner
            .stage_health
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if stage.evicted {
            health.remove(&id);
            return;
        }
        health.insert(
            id,
            StageHealthStats {
                dirty: stage.dirty,
                size: stage.size,
                first_dirty_at: stage.first_dirty_at,
                last_error: stage.last_error.clone(),
            },
        );
    }

    fn record_pending_stage_health(
        &self,
        id: fileid3,
        size: u64,
        first_dirty_at: i64,
        last_error: Option<String>,
    ) {
        self.inner
            .stage_health
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                id,
                StageHealthStats {
                    dirty: true,
                    size,
                    first_dirty_at: Some(first_dirty_at),
                    last_error,
                },
            );
    }

    fn remove_stage_health(&self, id: fileid3) {
        self.inner
            .stage_health
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&id);
    }

    pub async fn health_snapshot(&self) -> FsHealthSnapshot {
        let io = self
            .inner
            .io_health
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut health = FsHealthSnapshot {
            pending_uploads: 0,
            dirty_bytes: 0,
            oldest_dirty_ms: 0,
            last_error: io.last_error,
            last_successful_io: io.last_successful_io,
            resources: crate::providers::resources::snapshot(),
            operations: crate::providers::operation::metrics(),
            commit: super::stage_commit::metrics(),
            fences: self.inner.namespace_fences.metrics(),
        };
        if let Ok(records) = self.inner.quarantined.read() {
            health.pending_uploads += records.len();
            if let Some(record) = records.first() {
                health.last_error = record.error.clone();
            }
        }
        let stage_health = self
            .inner
            .stage_health
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        for stage in stage_health.values() {
            if stage.dirty {
                health.pending_uploads += 1;
                health.dirty_bytes = health.dirty_bytes.saturating_add(stage.size);
                if let Some(first_dirty_at) = stage.first_dirty_at {
                    let age = now_ms.saturating_sub(first_dirty_at).max(0) as u64;
                    health.oldest_dirty_ms = health.oldest_dirty_ms.max(age);
                }
                if let Some(error) = &stage.last_error {
                    health.last_error = Some(error.clone());
                }
            }
        }
        match namespace_recovery::pending_operations(&self.inner.staging_root).await {
            Ok(operations) => {
                health.pending_uploads += operations.len();
                if !operations.is_empty() && self.inner.namespace.try_write().is_ok() {
                    health.last_error =
                        Some("Interrupted file changes are retained for recovery".into());
                }
            }
            Err(error) => {
                health.pending_uploads += 1;
                health.last_error = Some(error);
            }
        }
        if let Some(error) = self
            .inner
            .recovery_errors
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .first()
        {
            health.last_error = Some(error.clone());
        }
        health
    }

    fn io_succeeded(&self) {
        let mut health = self
            .inner
            .io_health
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        health.last_successful_io = Some(chrono::Utc::now().timestamp());
        health.last_error = None;
    }
    fn io_failed(&self, message: String) {
        self.inner
            .io_health
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .last_error = Some(message);
    }

    /// Restore only after the manager has verified mount.json account/bucket.
    pub async fn restore_stages(&self) -> Result<usize, String> {
        let replay_errors: HashMap<_, _> = stage::replay_write_intents(&self.inner.staging_root)
            .await?
            .into_iter()
            .collect();
        let records = stage::recovery_entries(&self.inner.staging_root).await?;
        let mut counts = HashMap::<String, usize>::new();
        for record in &records {
            if record.dirty && record.state != "unreadable" {
                *counts.entry(record.key.clone()).or_default() += 1;
            }
        }
        let mut count = 0;
        for mut record in records {
            if record.state == "replay_pending" {
                record.error = Some(
                    replay_errors
                        .get(&record.path.with_extension("write.json"))
                        .cloned()
                        .unwrap_or_else(|| {
                            "Interrupted write could not be replayed; retained for recovery".into()
                        }),
                );
                record.state = "unreadable".into();
            }
            if counts.get(&record.key).is_some_and(|count| *count > 1) {
                record.state = "unreadable".into();
                record.error = Some(
                    "Multiple staged records claim this object; export them for review".into(),
                );
            }
            if record.state == "unreadable" {
                self.inner
                    .quarantined
                    .write()
                    .map_err(|_| "Recovery registry unavailable")?
                    .push(record);
                continue;
            }
            let stage = match Stage::restore(record.clone()).await {
                Ok(stage) => stage,
                Err(error) => {
                    record.state = "unreadable".into();
                    record.error = Some(error.to_string());
                    self.inner
                        .quarantined
                        .write()
                        .map_err(|_| "Recovery registry unavailable")?
                        .push(record);
                    continue;
                }
            };
            let mut parent = ROOT_ID;
            let mut prefix = String::new();
            let components: Vec<_> = stage.key.split('/').collect();
            for component in components.iter().take(components.len().saturating_sub(1)) {
                prefix.push_str(component);
                prefix.push('/');
                parent = self
                    .intern_child(&prefix, parent, EntryKind::Dir, DIR_SIZE, 0)
                    .map_err(|e| format!("{:?}", e))?;
            }
            let id = self
                .intern_child(
                    &stage.key,
                    parent,
                    EntryKind::File,
                    stage.size,
                    stage.mtime_secs,
                )
                .map_err(|e| format!("{:?}", e))?;
            let reservation = stage.reservation_bytes().await;
            self.inner.quota.lock().await.restore(id, reservation);
            self.record_stage_health(id, &stage);
            self.inner
                .stages
                .lock()
                .await
                .insert(id, Arc::new(AsyncMutex::new(stage)));
            count += 1;
        }
        self.resume_namespace_operations().await?;
        Ok(count)
    }

    pub(super) async fn pending_operations(
        root: &Path,
    ) -> Result<Vec<stage::StageRecovery>, String> {
        namespace_recovery::pending_operations(root).await
    }

    fn pending_rename_overlaps(a: &str, b: &str) -> bool {
        a == b || (a.ends_with('/') && b.starts_with(a)) || (b.ends_with('/') && a.starts_with(b))
    }

    fn ensure_rename_not_blocked(
        &self,
        from: &str,
        to: &str,
        own_journal: &Path,
    ) -> Result<(), nfsstat3> {
        let renames = self
            .inner
            .pending_renames
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        for (path, (pending_from, pending_to)) in renames.iter() {
            if path == own_journal {
                continue;
            }
            if [pending_from.as_str(), pending_to.as_str()]
                .iter()
                .any(|pending| {
                    Self::pending_rename_overlaps(from, pending)
                        || Self::pending_rename_overlaps(to, pending)
                })
            {
                return Err(nfsstat3::NFS3ERR_IO);
            }
        }
        Ok(())
    }

    fn ensure_rename_available(&self, key: &str) -> Result<(), nfsstat3> {
        if self
            .inner
            .quarantined
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
            .iter()
            .any(|record| {
                !record.key.is_empty()
                    && (record.key == key || (key.ends_with('/') && record.key.starts_with(key)))
            })
        {
            return Err(nfsstat3::NFS3ERR_IO);
        }
        let renames = self
            .inner
            .pending_renames
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        if renames.values().any(|(from, to)| {
            [from, to]
                .iter()
                .any(|prefix| Self::pending_rename_overlaps(key, prefix.as_str()))
        }) {
            return Err(nfsstat3::NFS3ERR_IO);
        }
        Ok(())
    }

    /// Rejects a mutation on a read-only mount.
    ///
    /// `capabilities()` already tells the server we are read-only, but each
    /// handler checks that independently, so every mutator repeats the check:
    /// a mistake there must not turn into a write to the user's bucket.
    fn ensure_writable(&self) -> Result<(), nfsstat3> {
        if self.inner.read_only {
            Err(nfsstat3::NFS3ERR_ROFS)
        } else if !self.inner.accepting_writes.load(Ordering::SeqCst) {
            Err(nfsstat3::NFS3ERR_IO)
        } else {
            Ok(())
        }
    }

    fn inode(&self, id: fileid3) -> Result<Inode, nfsstat3> {
        let inodes = self
            .inner
            .inodes
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
        inodes
            .get(id)
            .cloned()
            .ok_or(if id > 0 && id < inodes.next_id {
                nfsstat3::NFS3ERR_STALE
            } else {
                nfsstat3::NFS3ERR_NOENT
            })
    }

    fn dir_inode(&self, id: fileid3) -> Result<Inode, nfsstat3> {
        let inode = self.inode(id)?;
        if inode.kind != EntryKind::Dir {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        Ok(inode)
    }

    /// Validates a name the client wants to create or resolve.
    ///
    /// A name carrying a slash would compose a key outside the directory it was
    /// sent for, so it is refused rather than normalized.
    fn child_name(&self, filename: &filename3) -> Result<String, nfsstat3> {
        let name = std::str::from_utf8(filename).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        Ok(name.to_string())
    }

    fn intern_child(
        &self,
        key: &str,
        parent: fileid3,
        kind: EntryKind,
        size: u64,
        mtime_secs: u32,
    ) -> Result<fileid3, nfsstat3> {
        let mut inodes = self
            .inner
            .inodes
            .write()
            .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
        Ok(inodes.intern(key, parent, kind, size, mtime_secs))
    }

    fn update_inode_attrs(&self, id: fileid3, size: u64, mtime_secs: u32) {
        if let Ok(mut inodes) = self.inner.inodes.write() {
            inodes.set_attrs(id, size, mtime_secs);
        }
    }

    fn invalidate_dir(&self, dirid: fileid3) {
        self.inner
            .directory_generation
            .fetch_add(1, Ordering::SeqCst);
        if let Ok(mut dirs) = self.inner.dirs.write() {
            dirs.remove(&dirid);
            if let Ok(mut cookies) = self.inner.directory_cookies.write() {
                cookies.retain(|(directory, _), _| *directory != dirid);
            }
        }
        if let Ok(mut cache) = self.inner.negative_lookups.write() {
            cache.retain(|(directory, _), _| *directory != dirid);
        }
        if let Ok(mut cache) = self.inner.positive_lookups.write() {
            cache.retain(|(directory, _), _| *directory != dirid);
        }
    }

    /// Invalidate names changed by a rename without expiring independent
    /// directory scans. Called both after remote copies (including failure)
    /// and after rekeying, to catch descendant listings opened during deletes.
    fn invalidate_rename_dirs(&self, from: &str, to: &str) -> Result<(), nfsstat3> {
        fn parent_key(key: &str) -> &str {
            let key = key.strip_suffix('/').unwrap_or(key);
            key.rfind('/').map(|index| &key[..=index]).unwrap_or("")
        }
        let from_parent = parent_key(from);
        let to_parent = parent_key(to);
        let is_directory = from.ends_with('/');
        let affected: Vec<_> = {
            let inodes = self
                .inner
                .inodes
                .read()
                .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
            inodes
                .by_id
                .iter()
                .filter_map(|(&id, inode)| {
                    (inode.kind == EntryKind::Dir
                        && (inode.key == from_parent
                            || inode.key == to_parent
                            || (is_directory
                                && (inode.key.starts_with(from) || inode.key.starts_with(to)))))
                    .then_some(id)
                })
                .collect()
        };
        for id in affected {
            self.invalidate_dir(id);
        }
        Ok(())
    }

    /// Drop all directory generations when recovery affects arbitrary keys.
    fn invalidate_all_dirs(&self) {
        if let Ok(mut dirs) = self.inner.dirs.write() {
            dirs.clear();
            if let Ok(mut cookies) = self.inner.directory_cookies.write() {
                cookies.clear();
            }
        }
    }

    fn attr_of(&self, id: fileid3, inode: &Inode) -> fattr3 {
        let (ftype, mode, nlink, size) = match inode.kind {
            EntryKind::Dir => (ftype3::NF3DIR, DIR_MODE, 2, DIR_SIZE),
            EntryKind::File => (ftype3::NF3REG, FILE_MODE, 1, inode.size),
        };
        let time = nfstime3 {
            seconds: inode.mtime_secs,
            nseconds: 0,
        };

        fattr3 {
            ftype,
            mode,
            nlink,
            uid: self.inner.uid,
            gid: self.inner.gid,
            size,
            used: size,
            rdev: specdata3 {
                specdata1: 0,
                specdata2: 0,
            },
            fsid: self.inner.fsid,
            fileid: id,
            atime: time,
            mtime: time,
            ctime: time,
        }
    }

    /// Staged attributes for `id`, when a stage exists and is not busy.
    ///
    /// `readdir` answers feed the client's attribute cache for the whole
    /// `actimeo` window, so a file listed at its pre-copy size reads back empty
    /// for two minutes even after the upload lands. The lock is only ever
    /// attempted, never waited on: a stage that is held is mid-download, and
    /// the object's own attributes are the right answer then anyway.
    async fn try_staged_attr(&self, id: fileid3, inode: &Inode) -> Option<fattr3> {
        let handle = self.inner.stages.lock().await.get(&id).cloned()?;
        let guard = handle.try_lock().ok()?;
        if guard.evicted {
            return None;
        }
        Some(self.staged_attr(id, inode, guard.size, guard.mtime_secs))
    }

    /// Attributes of a file whose staged content has not been uploaded yet.
    fn staged_attr(&self, id: fileid3, inode: &Inode, size: u64, mtime_secs: u32) -> fattr3 {
        let time = nfstime3 {
            seconds: mtime_secs,
            nseconds: 0,
        };
        fattr3 {
            size,
            used: size,
            atime: time,
            mtime: time,
            ctime: time,
            ..self.attr_of(id, inode)
        }
    }

    fn negative_lookup_hit(&self, dirid: fileid3, name: &str, generation: u64) -> bool {
        self.inner
            .negative_lookups
            .read()
            .map(|cache| {
                cache.get(&(dirid, name.to_string())).is_some_and(|entry| {
                    entry.generation == generation
                        && entry.stored_at.elapsed() < NEGATIVE_LOOKUP_TTL
                })
            })
            .unwrap_or(false)
    }

    fn remember_negative_lookup(&self, dirid: fileid3, name: &str, generation: u64) {
        if let Ok(mut cache) = self.inner.negative_lookups.write() {
            cache.retain(|_, entry| entry.stored_at.elapsed() < NEGATIVE_LOOKUP_TTL);
            cache.insert(
                (dirid, name.to_string()),
                NegativeLookup {
                    generation,
                    stored_at: Instant::now(),
                },
            );
        }
    }

    fn positive_lookup_hit(&self, dirid: fileid3, name: &str, generation: u64) -> Option<fileid3> {
        self.inner.positive_lookups.read().ok().and_then(|cache| {
            cache.get(&(dirid, name.to_string())).and_then(|entry| {
                (entry.generation == generation && entry.stored_at.elapsed() < DIR_CACHE_TTL)
                    .then_some(entry.fileid)
            })
        })
    }

    fn remember_positive_lookup(
        &self,
        dirid: fileid3,
        name: &str,
        generation: u64,
        fileid: fileid3,
    ) {
        if let Ok(mut cache) = self.inner.positive_lookups.write() {
            cache.retain(|_, entry| entry.stored_at.elapsed() < DIR_CACHE_TTL);
            cache.insert(
                (dirid, name.to_string()),
                PositiveLookup {
                    generation,
                    fileid,
                    stored_at: Instant::now(),
                },
            );
        }
    }

    fn inode_matches_child(
        &self,
        dirid: fileid3,
        dir_key: &str,
        name: &str,
        inode: &Inode,
    ) -> bool {
        inode.parent == dirid
            && inode.key == child_key(dir_key, name, matches!(inode.kind, EntryKind::Dir))
    }

    async fn lookup_flight(&self, dirid: fileid3, name: &str) -> Arc<AsyncMutex<()>> {
        let mut flights = self.inner.lookup_flights.lock().await;
        flights.retain(|_, value| value.strong_count() != 0);
        let key = (dirid, name.to_string());
        let flight = flights
            .get(&key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| Arc::new(AsyncMutex::new(())));
        flights.insert(key, Arc::downgrade(&flight));
        flight
    }

    async fn directory_child_exists(&self, dir_key: &str, name: &str) -> Result<bool, nfsstat3> {
        let prefix = child_key(dir_key, name, true);
        let request = self
            .inner
            .client
            .list_objects_v2()
            .bucket(&self.inner.bucket)
            .prefix(&prefix)
            .delimiter("/")
            .max_keys(1);
        let scope = self.storage_scope(&prefix);
        let context =
            self.read_operation_context(OperationKind::List, &scope, "", Duration::from_secs(30));
        let response = execute_storage_operation(&context, || {
            let request = request.clone();
            async move {
                request
                    .send()
                    .await
                    .map_err(|error| AttemptError::from_sdk(&error))
            }
        })
        .await
        .map_err(|error| self.map_operation_error("List", error))?;
        self.io_succeeded();
        Ok(response.common_prefixes().iter().any(|common| {
            common
                .prefix()
                .is_some_and(|value| value.starts_with(&prefix))
        }) || response
            .contents()
            .iter()
            .any(|object| object.key().is_some_and(|key| key.starts_with(&prefix))))
    }

    async fn lookup_exact_child(
        &self,
        dirid: fileid3,
        dir_key: &str,
        name: &str,
        generation: u64,
        fence: bool,
        flight: bool,
    ) -> Result<Option<(fileid3, Inode)>, nfsstat3> {
        let _flight = if flight {
            let flight = self.lookup_flight(dirid, name).await;
            Some(flight.lock_owned().await)
        } else {
            None
        };
        let _fence = if fence {
            Some(
                self.fence_paths_shared(vec![
                    FencePath::exact(child_key(dir_key, name, false)),
                    FencePath::prefix(child_key(dir_key, name, true)),
                ])
                .await,
            )
        } else {
            None
        };

        if let Some((child, complete, current_generation)) =
            self.cached_directory_child(dirid, dir_key, name)?
        {
            if let Some(child) = child {
                let inode = self.inode(child.fileid)?;
                if self.inode_matches_child(dirid, dir_key, name, &inode)
                    && (inode.kind == EntryKind::Dir
                        || complete
                        || self
                            .positive_lookup_hit(dirid, name, current_generation)
                            .is_some_and(|id| id == child.fileid))
                {
                    return Ok(Some((child.fileid, inode)));
                }
            }
            if complete {
                return Ok(None);
            }
            if current_generation == generation && self.negative_lookup_hit(dirid, name, generation)
            {
                return Ok(None);
            }
        }

        if let Some(fileid) = self.positive_lookup_hit(dirid, name, generation) {
            let inode = self.inode(fileid)?;
            if self.inode_matches_child(dirid, dir_key, name, &inode) {
                return Ok(Some((fileid, inode)));
            }
        }
        if self.negative_lookup_hit(dirid, name, generation) {
            return Ok(None);
        }

        if self.directory_child_exists(dir_key, name).await? {
            let key = child_key(dir_key, name, true);
            let id = self.intern_child(&key, dirid, EntryKind::Dir, DIR_SIZE, 0)?;
            self.remember_positive_lookup(dirid, name, generation, id);
            return Ok(Some((id, self.inode(id)?)));
        }

        let key = child_key(dir_key, name, false);
        match self.head_object(&key).await? {
            Some((size, mtime)) => {
                let id = self.intern_child(&key, dirid, EntryKind::File, size, mtime)?;
                self.remember_positive_lookup(dirid, name, generation, id);
                Ok(Some((id, self.inode(id)?)))
            }
            None => {
                self.remember_negative_lookup(dirid, name, generation);
                Ok(None)
            }
        }
    }

    /// The child of `dirid` named `name`, resolved through the cached listing so
    /// its kind is known, or `None` when the directory has no such entry.
    async fn lookup_child(
        &self,
        dirid: fileid3,
        dir_key: &str,
        name: &str,
    ) -> Result<Option<(fileid3, Inode)>, nfsstat3> {
        if let Some((child, complete, generation)) =
            self.cached_directory_child(dirid, dir_key, name)?
        {
            if let Some(child) = child {
                let inode = self.inode(child.fileid)?;
                if self.inode_matches_child(dirid, dir_key, name, &inode)
                    && (inode.kind == EntryKind::Dir
                        || complete
                        || self
                            .positive_lookup_hit(dirid, name, generation)
                            .is_some_and(|id| id == child.fileid))
                {
                    return Ok(Some((child.fileid, inode)));
                }
                return self
                    .lookup_exact_child(dirid, dir_key, name, generation, true, true)
                    .await;
            }
            if complete {
                return Ok(None);
            }
            let children = self.children_of(dirid, dir_key).await?;
            return children
                .binary_search_by(|child| child.name.as_str().cmp(name))
                .ok()
                .and_then(|index| children.get(index).cloned())
                .map(|child| {
                    let inode = self.inode(child.fileid)?;
                    Ok((child.fileid, inode))
                })
                .transpose();
        }

        let generation = self.inner.directory_generation.load(Ordering::SeqCst);
        self.lookup_exact_child(dirid, dir_key, name, generation, true, true)
            .await
    }

    async fn lookup_child_fenced(
        &self,
        dirid: fileid3,
        dir_key: &str,
        name: &str,
    ) -> Result<Option<(fileid3, Inode)>, nfsstat3> {
        if let Some((child, complete, generation)) =
            self.cached_directory_child(dirid, dir_key, name)?
        {
            if let Some(child) = child {
                let inode = self.inode(child.fileid)?;
                if self.inode_matches_child(dirid, dir_key, name, &inode)
                    && (inode.kind == EntryKind::Dir
                        || complete
                        || self
                            .positive_lookup_hit(dirid, name, generation)
                            .is_some_and(|id| id == child.fileid))
                {
                    return Ok(Some((child.fileid, inode)));
                }
                return self
                    .lookup_exact_child(dirid, dir_key, name, generation, false, false)
                    .await;
            }
            if complete {
                return Ok(None);
            }
            let children = self.children_of(dirid, dir_key).await?;
            return children
                .binary_search_by(|child| child.name.as_str().cmp(name))
                .ok()
                .and_then(|index| children.get(index).cloned())
                .map(|child| {
                    let inode = self.inode(child.fileid)?;
                    Ok((child.fileid, inode))
                })
                .transpose();
        }

        let generation = self.inner.directory_generation.load(Ordering::SeqCst);
        self.lookup_exact_child(dirid, dir_key, name, generation, false, false)
            .await
    }

    async fn resolve_child(
        &self,
        dirid: fileid3,
        dir_key: &str,
        name: &str,
    ) -> Result<(fileid3, Inode), nfsstat3> {
        self.lookup_child(dirid, dir_key, name)
            .await?
            .ok_or(nfsstat3::NFS3ERR_NOENT)
    }

    async fn resolve_child_fenced(
        &self,
        dirid: fileid3,
        dir_key: &str,
        name: &str,
    ) -> Result<(fileid3, Inode), nfsstat3> {
        self.lookup_child_fenced(dirid, dir_key, name)
            .await?
            .ok_or(nfsstat3::NFS3ERR_NOENT)
    }

    /// Size and modification time of an object, or `None` when it is not there.
    async fn head_object(&self, key: &str) -> Result<Option<(u64, u32)>, nfsstat3> {
        let request = self
            .inner
            .client
            .head_object()
            .bucket(&self.inner.bucket)
            .key(key);
        let scope = self.storage_scope(key);
        let context =
            self.read_operation_context(OperationKind::Head, &scope, "", Duration::from_secs(30));
        let head = match execute_storage_operation(&context, || {
            let request = request.clone();
            async move {
                request
                    .send()
                    .await
                    .map_err(|error| AttemptError::from_sdk(&error))
            }
        })
        .await
        {
            Ok(head) => head,
            Err(error) if matches!(error.class(), StorageErrorClass::NotFound) => return Ok(None),
            Err(error) => return Err(self.map_operation_error("HEAD", error)),
        };

        let size = head.content_length().unwrap_or(0).max(0) as u64;
        let mtime = head
            .last_modified()
            .map(|time| time.secs())
            .unwrap_or_default();
        Ok(Some((
            size,
            u32::try_from(mtime.max(0)).unwrap_or(u32::MAX),
        )))
    }

    async fn read_identity(&self, id: fileid3, key: &str) -> Result<ReadIdentity, nfsstat3> {
        if let Some(identity) = self.inner.read_identities.lock().await.get(&id).cloned() {
            if identity.observed_at.elapsed() < DIR_CACHE_TTL {
                return Ok(identity);
            }
        }
        let head = self
            .object_head(key)
            .await?
            .ok_or(nfsstat3::NFS3ERR_NOENT)?;
        let identity = ReadIdentity {
            etag: head
                .e_tag()
                .filter(|e| !e.is_empty())
                .ok_or(nfsstat3::NFS3ERR_IO)?
                .to_string(),
            version_id: head
                .version_id()
                .filter(|value| *value != "null")
                .map(str::to_string),
            size: head
                .content_length()
                .filter(|n| *n >= 0)
                .ok_or(nfsstat3::NFS3ERR_IO)? as u64,
            observed_at: Instant::now(),
        };
        let mut identities = self.inner.read_identities.lock().await;
        if identities.get(&id).is_some_and(|old| {
            old.etag != identity.etag
                || old.version_id != identity.version_id
                || old.size != identity.size
        }) {
            // Changing an old handle to a new object version can mix the OS
            // client's cached pages. Retire it and require a fresh LOOKUP.
            let stages = self.inner.stages.lock().await;
            if stages.contains_key(&id) {
                return Err(nfsstat3::NFS3ERR_IO);
            }
            let mut inodes = self
                .inner
                .inodes
                .write()
                .map_err(|_| nfsstat3::NFS3ERR_IO)?;
            let old = inodes
                .get(id)
                .filter(|inode| inode.key == key)
                .cloned()
                .ok_or(nfsstat3::NFS3ERR_STALE)?;
            inodes.remove(id);
            let new_id = inodes.intern(
                key,
                old.parent,
                old.kind,
                identity.size,
                head.last_modified()
                    .map(|value| value.secs().max(0) as u32)
                    .unwrap_or(old.mtime_secs),
            );
            identities.remove(&id);
            identities.insert(new_id, identity);
            self.inner.read_cache.forget_file(id);
            self.invalidate_dir(old.parent);
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        self.update_inode_attrs(
            id,
            identity.size,
            head.last_modified()
                .map(|value| value.secs().clamp(0, u32::MAX as i64) as u32)
                .unwrap_or(0),
        );
        identities.insert(id, identity.clone());
        Ok(identity)
    }

    // ---- Reading ----

    /// Chunk `index` of the object behind `id`, from the cache or from S3.
    ///
    /// The first reader to want a chunk fetches it; every concurrent reader
    /// awaits that same fetch through the chunk's cell. A failed fetch drops
    /// the slot so the next read retries rather than inheriting the error.
    async fn chunk_bytes(
        &self,
        id: fileid3,
        key: &str,
        index: u64,
        identity: &ReadIdentity,
    ) -> Result<Arc<read_cache::CachedBytes>, nfsstat3> {
        let object_size = identity.size;
        let cache_version = format!(
            "{}:{}",
            identity.version_id.as_deref().unwrap_or_default(),
            identity.etag
        );
        let slot = self
            .inner
            .read_cache
            .slot_version(id, &cache_version, index);
        let result = slot
            .cell
            .get_or_try_init(|| async {
                let start = read_cache::chunk_start(index);
                let len = read_cache::chunk_len(index, object_size);
                let range = format!("bytes={}-{}", start, start + len.max(1) - 1);

                let expected_range = format!("bytes {}-{}/{}", start, start + len - 1, object_size);
                let scope = self.storage_scope(key);
                let frozen_identity = format!("{cache_version}:{range}");
                let context = self.read_operation_context(
                    OperationKind::Get,
                    &scope,
                    &frozen_identity,
                    Duration::from_secs(90),
                );
                let (bytes, body_lease) = execute_storage_operation(&context, || {
                    let request = self
                        .inner
                        .client
                        .get_object()
                        .bucket(&self.inner.bucket)
                        .key(key)
                        .range(&range)
                        .if_match(&identity.etag)
                        .set_version_id(identity.version_id.clone());
                    let expected_range = expected_range.clone();
                    async move {
                        let response = request
                            .send()
                            .await
                            .map_err(|error| AttemptError::from_sdk(&error))?;

                        if response.content_range() != Some(expected_range.as_str())
                            || response.content_length() != Some(len as i64)
                            || response.e_tag() != Some(identity.etag.as_str())
                        {
                            return Err(AttemptError::permanent(
                                "GET returned headers outside the frozen read identity",
                            ));
                        }

                        let body_lease = ByteLease::new(ResourceKind::ResponseBody, len);
                        let body = tokio::time::timeout(Duration::from_secs(30), async move {
                            let capacity = usize::try_from(len)
                                .map_err(|_| AttemptError::permanent("GET body is too large"))?;
                            let mut stream = response.body;
                            let mut body = Vec::with_capacity(capacity);
                            while let Some(chunk) = stream.next().await {
                                let chunk = chunk.map_err(|error| {
                                    log::error!("mount: failed to buffer \"{}\": {}", key, error);
                                    AttemptError::transient("GET body read failed")
                                })?;
                                if body.len().saturating_add(chunk.len()) > capacity {
                                    return Err(AttemptError::permanent(
                                        "GET body exceeded Content-Length",
                                    ));
                                }
                                body.extend_from_slice(&chunk);
                            }
                            Ok::<_, AttemptError>(body)
                        })
                        .await
                        .map_err(|_| AttemptError::transient("GET body read timed out"))??;
                        if body.len() as u64 != len {
                            return Err(AttemptError::transient(
                                "GET body length did not match Content-Length",
                            ));
                        }
                        Ok((body, body_lease))
                    }
                })
                .await
                .map_err(|error| self.map_operation_error("Read", error))?;
                self.io_succeeded();
                let bytes = Arc::new(read_cache::CachedBytes::new(bytes));
                drop(body_lease);
                self.inner.read_cache.note_filled_version(
                    id,
                    &cache_version,
                    index,
                    &slot,
                    bytes.len() as u64,
                );
                Ok(bytes)
            })
            .await;

        match result {
            Ok(bytes) => Ok(bytes.clone()),
            Err(status) => {
                self.inner
                    .read_cache
                    .remove_slot_version(id, &cache_version, index);
                self.inner.read_cache.forget_file(id);
                if let Some(identity) = self.inner.read_identities.lock().await.get_mut(&id) {
                    identity.observed_at = Instant::now() - DIR_CACHE_TTL;
                }
                Err(status)
            }
        }
    }

    /// Warms the chunks after `served` in the background, so a sequential
    /// reader finds the next chunk already arriving. Cheap for everyone else:
    /// a chunk that exists is skipped, and when the prefetch slots are busy
    /// nothing is queued.
    fn prefetch_after(&self, id: fileid3, key: &str, served: u64, identity: &ReadIdentity) {
        let object_size = identity.size;
        let cache_version = format!(
            "{}:{}",
            identity.version_id.as_deref().unwrap_or_default(),
            identity.etag
        );
        for step in 1..=read_cache::PREFETCH_CHUNKS {
            let index = served + step;
            if read_cache::chunk_len(index, object_size) == 0 {
                return;
            }
            if self
                .inner
                .read_cache
                .is_present_version(id, &cache_version, index)
            {
                continue;
            }
            let Ok(permit) = self.inner.prefetch_slots.clone().try_acquire_owned() else {
                return;
            };
            let fs = self.clone();
            let key = key.to_string();
            let identity = identity.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let _fence = fs.fence_exact_key_shared(&key).await;
                // A rename that finished while this waited moved the file;
                // its next read warms the new key instead.
                if fs.inode(id).is_ok_and(|inode| inode.key == key) {
                    let _ = fs.chunk_bytes(id, &key, index, &identity).await;
                }
            });
        }
    }

    async fn ensure_key_settled(&self, key: &str) -> Result<(), nfsstat3> {
        let id = self
            .inner
            .inodes
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
            .by_key
            .get(key)
            .copied();
        if let Some(id) = id {
            let snapshot = self
                .stage_guard(id)
                .await
                .and_then(|guard| guard.snapshot.clone());
            if let Some(snapshot) = snapshot {
                let journal = snapshot.journal().await.map_err(|_| nfsstat3::NFS3ERR_IO)?;
                if journal.completing && !self.snapshot_published(key, &snapshot).await {
                    return Err(nfsstat3::NFS3ERR_IO);
                }
            }
        }
        // An uncertain namespace mutation can outlive its local Future. Do
        // not publish another generation or delete this key while it remains.
        if tokio::fs::try_exists(self.namespace_journal_path(key))
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
        {
            return Err(nfsstat3::NFS3ERR_IO);
        }
        Ok(())
    }

    fn namespace_journal_path(&self, key: &str) -> PathBuf {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hash);
        self.inner
            .staging_root
            .join(format!("namespace-{:016x}.json", hash.finish()))
    }

    /// Server-side copy. No bytes travel through this machine, which is what
    /// makes a rename inside a mount as cheap as the app's own move.
    #[allow(deprecated)] // SDK copy builder still accepts parsed Expires only.
    async fn copy_object(&self, object: &RenameObject, token: &str) -> Result<String, nfsstat3> {
        if object.size > 5 * 1024u64.pow(3) {
            return self.copy_large_object(object, token).await;
        }
        let head = self
            .object_head(&object.from)
            .await?
            .ok_or(nfsstat3::NFS3ERR_NOENT)?;
        if head.e_tag() != Some(object.source_etag.as_str())
            || head.content_length() != Some(object.size as i64)
        {
            return Err(nfsstat3::NFS3ERR_IO);
        }
        let mut metadata = head.metadata().cloned().unwrap_or_default();
        metadata.insert("r2-rename-operation".into(), token.to_string());
        let is_r2 = matches!(
            self.inner.transfer_config.get(),
            Some(crate::move_transfer::config::MoveConfig::R2(_))
        );
        if !is_r2
            && !self
                .condition_supported(if object.replaced_etag.is_some() {
                    crate::providers::conditional::Condition::CopyMatch
                } else {
                    crate::providers::conditional::Condition::CopyCreate
                })
                .await?
        {
            return Err(nfsstat3::NFS3ERR_NOTSUPP);
        }
        let request = self
            .inner
            .client
            .copy_object()
            .bucket(&self.inner.bucket)
            .copy_source(encode_copy_source(&self.inner.bucket, &object.from))
            .key(&object.to)
            .copy_source_if_match(&object.source_etag)
            .metadata_directive(MetadataDirective::Replace)
            .set_metadata(Some(metadata))
            .set_content_type(head.content_type().map(str::to_string))
            .set_cache_control(head.cache_control().map(str::to_string))
            .set_content_disposition(head.content_disposition().map(str::to_string))
            .set_content_encoding(head.content_encoding().map(str::to_string))
            .set_content_language(head.content_language().map(str::to_string))
            .set_expires(head.expires().cloned());
        let result = if is_r2 {
            let previous = object.replaced_etag.clone();
            request
                .customize()
                .mutate_request(move |request| {
                    if let Some(etag) = &previous {
                        request
                            .headers_mut()
                            .insert("cf-copy-destination-if-match", etag.clone());
                    } else {
                        request
                            .headers_mut()
                            .insert("cf-copy-destination-if-none-match", "*");
                    }
                })
                .send()
                .await
        } else if let Some(etag) = &object.replaced_etag {
            request.if_match(etag).send().await
        } else {
            request.if_none_match("*").send().await
        }
        .map_err(|e| map_s3_error(&e))?;
        result
            .copy_object_result()
            .and_then(|r| r.e_tag())
            .map(str::to_string)
            .ok_or(nfsstat3::NFS3ERR_IO)
    }

    fn rename_journal_path(&self, from: &str, to: &str) -> PathBuf {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        from.hash(&mut hash);
        to.hash(&mut hash);
        self.inner
            .staging_root
            .join(format!("rename-{:016x}.json", hash.finish()))
    }

    async fn preflight_rename(&self, objects: &[RenameObject]) -> Result<(), nfsstat3> {
        use crate::providers::conditional::Condition;
        let is_r2 = matches!(
            self.inner.transfer_config.get(),
            Some(crate::move_transfer::config::MoveConfig::R2(_))
        );
        for object in objects {
            if object.source_version.is_none()
                && !self.condition_supported(Condition::DeleteMatch).await?
            {
                return Err(nfsstat3::NFS3ERR_NOTSUPP);
            }
            let multipart = object.size > 5 * 1024u64.pow(3);
            if !multipart && is_r2 {
                continue;
            }
            let destination = match (multipart, object.replaced_etag.is_some()) {
                (true, true) => Condition::CompleteMatch,
                (true, false) => Condition::CompleteCreate,
                (false, true) => Condition::CopyMatch,
                (false, false) => Condition::CopyCreate,
            };
            if !self.condition_supported(destination).await?
                || (!multipart && !self.condition_supported(Condition::CopySource).await?)
            {
                return Err(nfsstat3::NFS3ERR_NOTSUPP);
            }
        }
        Ok(())
    }

    /// Persist each actual key pair and phase, not its unordered completion
    /// position. A partial rename resumes this journal without recopying a
    /// verified destination. Copies are retained on error: unconditional
    /// rollback could erase another writer's replacement.
    async fn rename_objects(
        &self,
        from: &str,
        to: &str,
        pairs: Vec<(String, String)>,
    ) -> Result<(), nfsstat3> {
        let path = self.rename_journal_path(from, to);
        let mut journal = match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let value: RenameJournal =
                    serde_json::from_slice(&bytes).map_err(|_| nfsstat3::NFS3ERR_IO)?;
                if value.from != from || value.to != to {
                    return Err(nfsstat3::NFS3ERR_IO);
                }
                value
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::create_dir_all(&self.inner.staging_root)
                    .await
                    .map_err(|_| nfsstat3::NFS3ERR_IO)?;
                let mut objects = Vec::with_capacity(pairs.len());
                for (from, to) in pairs {
                    let head = self
                        .object_head(&from)
                        .await?
                        .ok_or(nfsstat3::NFS3ERR_NOENT)?;
                    let replaced_etag = self
                        .object_head(&to)
                        .await?
                        .map(|head| head.e_tag().ok_or(nfsstat3::NFS3ERR_IO).map(str::to_string))
                        .transpose()?;
                    objects.push(RenameObject {
                        from,
                        to,
                        source_etag: head.e_tag().ok_or(nfsstat3::NFS3ERR_IO)?.to_string(),
                        size: head
                            .content_length()
                            .filter(|n| *n >= 0)
                            .ok_or(nfsstat3::NFS3ERR_IO)? as u64,
                        destination_etag: None,
                        phase: "pending".into(),
                        replaced_etag,
                        source_version: head
                            .version_id()
                            .filter(|v| *v != "null" && !v.is_empty())
                            .map(str::to_string),
                    });
                }
                // Unsupported conditions are known before any object copy is
                // dispatched. Reject them before fencing either staged key.
                self.preflight_rename(&objects).await?;
                let value = RenameJournal {
                    from: from.to_string(),
                    to: to.to_string(),
                    token: format!(
                        "{}-{}",
                        std::process::id(),
                        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
                    ),
                    objects,
                };
                stage::write_json_atomic(&path, &value)
                    .await
                    .map_err(|_| nfsstat3::NFS3ERR_IO)?;
                value
            }
            Err(_) => return Err(nfsstat3::NFS3ERR_IO),
        };
        self.inner
            .pending_renames
            .write()
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
            .insert(path.clone(), (from.to_string(), to.to_string()));
        // Reconcile copies that could have committed before their response or
        // local journal update was lost, using a persisted per-operation token.
        for object in &mut journal.objects {
            if object.phase != "copying" {
                continue;
            }
            match self.object_head(&object.to).await {
                Ok(Some(head))
                    if head.metadata().and_then(|m| m.get("r2-rename-operation"))
                        == Some(&journal.token)
                        && head.content_length() == Some(object.size as i64) =>
                {
                    object.destination_etag = head.e_tag().map(str::to_string);
                    if object.destination_etag.is_none() {
                        return Err(nfsstat3::NFS3ERR_IO);
                    }
                    let source = crate::db::move_sessions::SourceIdentity {
                        size: object.size,
                        etag: object.source_etag.clone(),
                        version_id: object.source_version.clone(),
                    };
                    let destination = crate::db::move_sessions::SourceIdentity {
                        size: object.size,
                        etag: object
                            .destination_etag
                            .clone()
                            .ok_or(nfsstat3::NFS3ERR_IO)?,
                        version_id: head
                            .version_id()
                            .filter(|v| *v != "null")
                            .map(str::to_string),
                    };
                    use crate::move_transfer::planner::{verify_object_content, ObjectRead};
                    let paused = AtomicBool::new(false);
                    verify_object_content(
                        ObjectRead {
                            client: &self.inner.client,
                            bucket: &self.inner.bucket,
                            key: &object.from,
                            identity: &source,
                            endpoint: self.storage_endpoint().to_string(),
                            scope: self.storage_scope(&object.from),
                        },
                        ObjectRead {
                            client: &self.inner.client,
                            bucket: &self.inner.bucket,
                            key: &object.to,
                            identity: &destination,
                            endpoint: self.storage_endpoint().to_string(),
                            scope: self.storage_scope(&object.to),
                        },
                        &self.inner.shutdown,
                        &paused,
                    )
                    .await
                    .map_err(|_| nfsstat3::NFS3ERR_IO)?;
                    object.phase = "copied".into();
                }
                Ok(Some(head))
                    if object.replaced_etag.is_some()
                        && head.e_tag() == object.replaced_etag.as_deref() =>
                {
                    object.phase = "pending".into();
                }
                Ok(None) => {
                    object.phase = "pending".into();
                }
                Err(error) => return Err(error),
                _ => return Err(nfsstat3::NFS3ERR_IO),
            }
        }
        stage::write_json_atomic(&path, &journal)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let pending: Vec<_> = journal
            .objects
            .iter()
            .enumerate()
            .filter(|(_, object)| object.phase == "pending")
            .map(|(index, object)| (index, object.clone()))
            .collect();
        let journal = AsyncMutex::new(journal);
        let results = stream::iter(pending.into_iter().map(|(index, object)| {
            let journal = &journal;
            let path = &path;
            async move {
                let token = {
                    let mut state = journal.lock().await;
                    state.objects[index].phase = "copying".into();
                    stage::write_json_atomic(path, &*state)
                        .await
                        .map_err(|_| nfsstat3::NFS3ERR_IO)?;
                    state.token.clone()
                };
                let result = self.copy_object(&object, &token).await;
                if let Ok(etag) = &result {
                    let mut state = journal.lock().await;
                    state.objects[index].destination_etag = Some(etag.clone());
                    state.objects[index].phase = "copied".into();
                    stage::write_json_atomic(path, &*state)
                        .await
                        .map_err(|_| nfsstat3::NFS3ERR_IO)?;
                }
                // Identity is returned with the result even when completion
                // order is reversed. No zip with the input list is possible.
                Ok::<_, nfsstat3>((object.from, object.to, result))
            }
        }))
        .buffer_unordered(RENAME_COPY_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
        self.invalidate_rename_dirs(from, to)?;
        for result in results {
            let (_, _, copy) = result?;
            copy?;
        }
        let mut journal = journal.into_inner();
        for index in 0..journal.objects.len() {
            let object = journal.objects[index].clone();
            if object.phase == "deleted" {
                continue;
            }
            let dest = self
                .object_head(&object.to)
                .await?
                .ok_or(nfsstat3::NFS3ERR_NOENT)?;
            if dest.e_tag() != object.destination_etag.as_deref()
                || dest.metadata().and_then(|m| m.get("r2-rename-operation"))
                    != Some(&journal.token)
            {
                return Err(nfsstat3::NFS3ERR_IO);
            }
            journal.objects[index].phase = "deleting".into();
            stage::write_json_atomic(&path, &journal)
                .await
                .map_err(|_| nfsstat3::NFS3ERR_IO)?;
            // If DELETE committed before a lost reply, absence is convergence.
            // If another writer replaced the source, If-Match prevents loss.
            let head = self.head_object(&object.from).await?;
            if head.is_some() {
                if object.source_version.is_none()
                    && !self
                        .condition_supported(crate::providers::conditional::Condition::DeleteMatch)
                        .await?
                {
                    return Err(nfsstat3::NFS3ERR_NOTSUPP);
                }
                self.inner
                    .client
                    .delete_object()
                    .bucket(&self.inner.bucket)
                    .key(&object.from)
                    .if_match(&object.source_etag)
                    .set_version_id(object.source_version.clone())
                    .send()
                    .await
                    .map_err(|e| map_s3_error(&e))?;
            }
            journal.objects[index].phase = "deleted".into();
            stage::write_json_atomic(&path, &journal)
                .await
                .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        }
        // Keep the finished journal until local inode/stage rekey is durable;
        // the caller removes it only after that final step.
        Ok(())
    }

    /// Whether the directory holds nothing but its own folder marker.
    async fn dir_is_empty(&self, dir_key: &str) -> Result<bool, nfsstat3> {
        let request = self
            .inner
            .client
            .list_objects_v2()
            .bucket(&self.inner.bucket)
            .prefix(dir_key)
            .delimiter("/")
            .max_keys(2);
        let scope = self.storage_scope(dir_key);
        let context =
            self.read_operation_context(OperationKind::List, &scope, "", Duration::from_secs(30));
        let response = execute_storage_operation(&context, || {
            let request = request.clone();
            async move {
                request
                    .send()
                    .await
                    .map_err(|error| AttemptError::from_sdk(&error))
            }
        })
        .await
        .map_err(|error| self.map_operation_error("List", error))?;

        let keys: Vec<&str> = response.contents().iter().filter_map(|o| o.key()).collect();
        Ok(dir_listing_is_empty(
            dir_key,
            &keys,
            response.common_prefixes().len(),
        ))
    }

    /// Every key under `prefix`, refusing a set too large to move inside one
    /// NFS call.
    async fn list_prefix_keys(&self, prefix: &str) -> Result<Vec<String>, nfsstat3> {
        let mut keys: Vec<String> = Vec::new();
        let mut continuation_token: Option<String> = None;
        let mut seen_tokens = std::collections::HashSet::new();

        loop {
            let mut request = self
                .inner
                .client
                .list_objects_v2()
                .bucket(&self.inner.bucket)
                .prefix(prefix)
                .max_keys(LIST_PAGE_SIZE);
            if let Some(token) = &continuation_token {
                request = request.continuation_token(token);
            }

            let scope = self.storage_scope(prefix);
            let context = self.read_operation_context(
                OperationKind::List,
                &scope,
                "",
                Duration::from_secs(30),
            );
            let response = execute_storage_operation(&context, || {
                let request = request.clone();
                async move {
                    request
                        .send()
                        .await
                        .map_err(|error| AttemptError::from_sdk(&error))
                }
            })
            .await
            .map_err(|error| self.map_operation_error("List", error))?;

            for object in response.contents() {
                if let Some(key) = object.key() {
                    keys.push(key.to_string());
                }
            }
            if keys.len() > MAX_RENAME_KEYS {
                log::error!(
                    "mount: refusing to rename \"{}\": more than {} objects. Use the app's Move for a folder this large.",
                    prefix,
                    MAX_RENAME_KEYS
                );
                return Err(nfsstat3::NFS3ERR_NOTSUPP);
            }

            if !response.is_truncated().unwrap_or(false) {
                break;
            }
            let next = response
                .next_continuation_token()
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if next
                .as_ref()
                .is_none_or(|token| token.is_empty() || !seen_tokens.insert(token.clone()))
            {
                return Err(nfsstat3::NFS3ERR_IO);
            }
            continuation_token = next;
        }

        Ok(keys)
    }

    // ---- Staging ----

    /// The stage for `id`, locked, if the file has content that is not yet in
    /// the bucket.
    ///
    /// Looking up and locking are one operation because a stage can be dropped
    /// in between, leaving the caller holding a handle whose backing file has
    /// already been unlinked. The tombstone says so, and since it is set under
    /// the same map lock that removes the entry, starting over cannot find the
    /// same handle twice.
    async fn stage_guard(&self, id: fileid3) -> Option<OwnedMutexGuard<Stage>> {
        loop {
            let handle = self.inner.stages.lock().await.get(&id).cloned()?;
            let guard = handle.lock_owned().await;
            if !guard.evicted {
                return Some(guard);
            }
        }
    }

    /// Stage for `id`, creating it on first write.
    ///
    /// A new stage is locked before it is published, so no other task can
    /// observe it half-primed, and the map lock is released before the
    /// read-modify-write download starts.
    async fn stage_for(
        &self,
        id: fileid3,
        inode: &Inode,
        prime: bool,
    ) -> Result<OwnedMutexGuard<Stage>, nfsstat3> {
        let mut guard = loop {
            let mut stages = self.inner.stages.lock().await;
            self.inode(id)?;
            if let Some(existing) = stages.get(&id).cloned() {
                drop(stages);
                let guard = existing.lock_owned().await;
                if guard.evicted {
                    // Dropped while we waited for it; the map has the truth.
                    continue;
                }
                return Ok(guard);
            }

            let path = self.inner.staging_root.join(format!(
                "{}-{}.data",
                id,
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            self.reserve_stage(id, if prime { inode.size } else { 0 })
                .await?;
            let created = Stage::create(path, inode.key.clone(), inode.mtime_secs)
                .await
                .map_err(|e| {
                    log::error!(
                        "mount: failed to open a staging file for \"{}\": {}",
                        inode.key,
                        e
                    );
                    nfsstat3::NFS3ERR_IO
                })?;

            let handle = Arc::new(AsyncMutex::new(created));
            let guard = handle.clone().lock_owned().await;
            stages.insert(id, handle);
            // The stage is the content now; chunks of the pre-edit object must
            // not survive it, or a read after the upload could see old bytes.
            self.inner.read_cache.forget_file(id);
            break guard;
        };

        if prime {
            if let Err(status) = self.prime_stage(id, &mut guard, inode).await {
                // A half-primed stage would shadow the real object with a
                // truncated copy of it, so it is unpublished before the lock is
                // released and nobody can ever read it.
                self.unpublish(id, &mut guard).await;
                return Err(status);
            }
        }

        Ok(guard)
    }

    /// Tombstones a locked stage, deletes its backing file and drops it from
    /// the registry.
    ///
    /// The unlink happens before the map entry goes so a writer waiting on the
    /// map cannot create a new staging file at the same path and have it
    /// deleted out from under it — the path is derived from the file id, so the
    /// old and the new stage would name the same file.
    async fn unpublish(&self, id: fileid3, guard: &mut Stage) {
        let mut stages = self.inner.stages.lock().await;
        guard.evicted = true;
        guard.remove_files().await;
        stages.remove(&id);
        self.remove_stage_health(id);
        self.inner.quota.lock().await.release(id);
        drop(stages);
        // Any transfer row this stage had is moot now — the content it was
        // going to upload no longer exists.
        if let Some(progress) = self.progress() {
            progress.removed(id, &guard.key);
        }
    }

    async fn ensure_stage(
        &self,
        id: fileid3,
        inode: &Inode,
    ) -> Result<OwnedMutexGuard<Stage>, nfsstat3> {
        self.stage_for(id, inode, true).await
    }

    /// Stage for `id` holding no content at all — the truncate-then-rewrite path
    /// an editor or `cp` takes, where downloading the old bytes is pure waste.
    async fn reset_stage(
        &self,
        id: fileid3,
        inode: &Inode,
    ) -> Result<OwnedMutexGuard<Stage>, nfsstat3> {
        if let Some(guard) = self.stage_guard(id).await {
            return Ok(guard);
        }
        self.stage_for(id, inode, false).await
    }

    /// Fills a new stage with the object's current content.
    ///
    /// S3 cannot update part of an object, so a write into the middle of an
    /// existing file has to become a full rewrite, and the old bytes have to be
    /// here for that rewrite to preserve them. The download is reported as a
    /// live transfer: from the file manager it is an inexplicable stall.
    async fn prime_stage(
        &self,
        id: fileid3,
        stage: &mut Stage,
        inode: &Inode,
    ) -> Result<(), nfsstat3> {
        let head = self
            .object_head(&inode.key)
            .await?
            .ok_or(nfsstat3::NFS3ERR_NOENT)?;
        let size = head
            .content_length()
            .filter(|size| *size >= 0)
            .ok_or(nfsstat3::NFS3ERR_IO)? as u64;
        stage.publication_guard = Some(stage::PublicationGuard::Match {
            etag: head
                .e_tag()
                .filter(|etag| !etag.is_empty())
                .ok_or(nfsstat3::NFS3ERR_IO)?
                .to_string(),
        });
        match stage::stage_init(size) {
            StageInit::Empty => Ok(()),
            StageInit::TooLarge => {
                log::error!(
                    "mount: refusing to edit \"{}\": {} bytes is past the {} byte staging limit",
                    inode.key,
                    inode.size,
                    stage::RMW_DOWNLOAD_CAP
                );
                Err(nfsstat3::NFS3ERR_IO)
            }
            StageInit::Download => {
                self.reserve_stage(id, size).await?;
                let tracker = self
                    .progress()
                    .map(|p| p.track(id, &inode.key, TransferKind::Download, size));
                let outcome = self
                    .download_into_stage(stage, inode, &head, tracker.as_ref())
                    .await;
                if let Some(tracker) = &tracker {
                    match &outcome {
                        Ok(()) => tracker.done(),
                        Err(_) => tracker.failed("Could not download the object for editing"),
                    }
                }
                outcome
            }
        }
    }

    async fn download_into_stage(
        &self,
        stage: &mut Stage,
        inode: &Inode,
        head: &aws_sdk_s3::operation::head_object::HeadObjectOutput,
        tracker: Option<&TransferTracker>,
    ) -> Result<(), nfsstat3> {
        let expected = head
            .content_length()
            .filter(|size| *size >= 0)
            .ok_or(nfsstat3::NFS3ERR_IO)? as u64;
        if stage.size != 0 {
            return Err(nfsstat3::NFS3ERR_IO);
        }
        let etag = head.e_tag().ok_or(nfsstat3::NFS3ERR_IO)?.to_string();
        let version = head
            .version_id()
            .filter(|v| *v != "null")
            .map(str::to_string);
        let scope = self.storage_scope(&inode.key);
        let identity = format!(
            "{}:{}:{}:{}",
            inode.key,
            etag,
            version.as_deref().unwrap_or_default(),
            expected
        );
        let context = self.read_operation_context(
            OperationKind::Get,
            &scope,
            &identity,
            crate::move_transfer::stream::protocol::attempt_timeout(expected),
        );
        let temp = execute_storage_operation(&context, || {
            let key = inode.key.clone();
            let etag = etag.clone();
            let version = version.clone();
            async move {
                self.download_object_to_private_file(&key, &etag, version, expected)
                    .await
            }
        })
        .await
        .map_err(|error| self.map_operation_error("GET", error))?;
        let apply = async {
            use tokio::io::AsyncReadExt;
            let mut file = tokio::fs::File::open(&temp)
                .await
                .map_err(|_| nfsstat3::NFS3ERR_IO)?;
            let mut buffer = vec![0u8; 1024 * 1024];
            let mut offset = 0u64;
            loop {
                let read = file
                    .read(&mut buffer)
                    .await
                    .map_err(|_| nfsstat3::NFS3ERR_IO)?;
                if read == 0 {
                    break;
                }
                if offset.saturating_add(read as u64) > expected {
                    return Err(nfsstat3::NFS3ERR_IO);
                }
                stage.write_at(offset, &buffer[..read]).await.map_err(|e| {
                    log::error!("mount: failed to stage \"{}\": {}", inode.key, e);
                    nfsstat3::NFS3ERR_IO
                })?;
                offset = offset.saturating_add(read as u64);
                if let Some(tracker) = tracker {
                    tracker.set(offset);
                }
            }
            if offset == expected {
                Ok(())
            } else {
                Err(nfsstat3::NFS3ERR_IO)
            }
        }
        .await;
        let _ = tokio::fs::remove_file(&temp).await;
        apply
    }

    async fn download_object_to_private_file(
        &self,
        key: &str,
        etag: &str,
        version: Option<String>,
        expected: u64,
    ) -> Result<PathBuf, AttemptError> {
        use std::sync::atomic::AtomicU64;
        use tokio::io::AsyncWriteExt;
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let temp = self.inner.staging_root.join(format!(
            ".stage-get-{}-{}-{}.tmp",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let result = async {
            let response = self
                .inner
                .client
                .get_object()
                .bucket(&self.inner.bucket)
                .key(key)
                .if_match(etag)
                .set_version_id(version)
                .send()
                .await
                .map_err(|error| AttemptError::from_sdk(&error))?;
            if response.content_length() != Some(expected as i64) || response.e_tag() != Some(etag)
            {
                return Err(AttemptError::permanent(
                    "GET response identity did not match the staged source",
                ));
            }
            if let Some(parent) = temp.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| AttemptError::permanent(error.to_string()))?;
            }
            let mut file = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)
                .await
                .map_err(|error| AttemptError::permanent(error.to_string()))?;
            let mut lease = ByteLease::new(ResourceKind::ResponseBody, expected);
            let mut body = response.body;
            let mut offset = 0u64;
            while let Some(chunk) = tokio::time::timeout(Duration::from_secs(30), body.next())
                .await
                .map_err(|_| AttemptError::transient("GET body read timed out"))?
            {
                let chunk = chunk.map_err(|_| AttemptError::transient("GET body read failed"))?;
                if offset.saturating_add(chunk.len() as u64) > expected {
                    return Err(AttemptError::permanent(
                        "GET response body exceeded promised content length",
                    ));
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|error| AttemptError::permanent(error.to_string()))?;
                offset = offset.saturating_add(chunk.len() as u64);
            }
            if offset != expected {
                return Err(AttemptError::transient(
                    "GET body ended before promised length",
                ));
            }
            file.flush()
                .await
                .map_err(|error| AttemptError::permanent(error.to_string()))?;
            drop(file);
            lease.resize(0);
            Ok(temp.clone())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        result
    }

    /// Unpublishes a stage and deletes its backing file, used when the object is
    /// deleted, has moved, or was never primed successfully.
    async fn discard_stage(&self, id: fileid3) {
        // Locked outside the map so a stage that is mid-download does not stall
        // every other file, then the map is taken to unpublish it. Taking the
        // map while holding a stage is safe in this one direction: nothing ever
        // *waits* on a stage lock while holding the map.
        let Some(mut guard) = self.stage_guard(id).await else {
            return;
        };
        self.unpublish(id, &mut guard).await;
    }

    /// Drops a stage whose content is safely in the bucket, so later reads go
    /// back to S3. All four providers are read-after-write consistent, so the
    /// object is readable the moment the upload returns.
    async fn evict_stage(&self, id: fileid3) {
        let mut stages = self.inner.stages.lock().await;
        let Some(handle) = stages.get(&id).cloned() else {
            return;
        };
        // A stage that is busy again is not ours to evict, and waiting for it
        // here would hold the map lock for the length of a write.
        let Ok(mut guard) = handle.try_lock_owned() else {
            return;
        };
        if guard.evicted || !stage::can_evict(guard.dirty, guard.state) {
            return;
        }
        guard.evicted = true;
        // Unlinked before the map entry goes, for the reason in `unpublish`.
        guard.remove_files().await;
        stages.remove(&id);
        self.remove_stage_health(id);
        self.inner.quota.lock().await.release(id);
    }

    // ---- Uploads ----

    /// Uploads one staged file, reporting it as a live transfer from first
    /// attempt to final outcome.
    async fn upload_with_attempts(
        &self,
        id: fileid3,
        key: &str,
        snapshot: &UploadSnapshot,
        attempts: u32,
    ) -> Result<String, UploadFailure> {
        let tracker = self
            .progress()
            .map(|progress| progress.track(id, key, TransferKind::Upload, snapshot.size));
        let result = self
            .upload_stage_file(key, snapshot, tracker.as_ref(), attempts)
            .await;
        let mut journal = snapshot.journal().await.map_err(UploadFailure::local)?;
        let mut etag = if result.is_ok() {
            journal.published_etag.clone()
        } else {
            None
        };
        if etag.is_none()
            && (result.is_ok()
                || result
                    .as_ref()
                    .is_err_and(|error| error.uncertain || error.message.starts_with("conflict:")))
        {
            etag = self.snapshot_verified_etag(key, snapshot).await;
        }
        if let Some(etag) = etag {
            journal.published_etag = Some(etag.clone());
            snapshot
                .save_journal(&journal)
                .await
                .map_err(UploadFailure::local)?;
            self.inner.read_cache.forget_file(id);
            self.inner.read_identities.lock().await.remove(&id);
            if let Some(tracker) = &tracker {
                tracker.done();
            }
            return Ok(etag);
        }
        let failure = result.err().unwrap_or_else(|| UploadFailure {
            message: "outcome_unknown: Publication returned no verifiable identity".into(),
            retryable: false,
            uncertain: true,
        });
        if let Some(tracker) = &tracker {
            tracker.failed(&failure.message);
        }
        Err(failure)
    }

    async fn snapshot_publication_guard(
        &self,
        key: &str,
        snapshot: &UploadSnapshot,
        journal: &mut stage::MultipartJournal,
        multipart: bool,
    ) -> Result<stage::PublicationGuard, UploadFailure> {
        use crate::providers::conditional::Condition;
        use stage::PublicationGuard;
        if journal.precondition.is_none() {
            if journal.completing {
                return Err(UploadFailure::local(
                    "Legacy publication has no durable condition; retain it for manual recovery",
                ));
            }
            journal.precondition = snapshot.publication_guard.clone();
            if journal.precondition.is_none() {
                match self.object_head(key).await.map_err(|e| UploadFailure {
                    message: format!("Unable to establish publication identity: {e:?}"),
                    retryable: matches!(e, nfsstat3::NFS3ERR_IO | nfsstat3::NFS3ERR_JUKEBOX),
                    uncertain: false,
                })? {
                    None => journal.precondition = Some(PublicationGuard::Absent),
                    Some(_) => {
                        return Err(UploadFailure::local(
                            "Stage has no original object identity; export it before replacing an existing object",
                        ));
                    }
                }
            }
        }
        let guard = journal
            .precondition
            .clone()
            .ok_or_else(|| UploadFailure::local("Missing publication condition"))?;
        if !guard.is_valid() {
            return Err(UploadFailure::local(
                "Invalid publication condition; local data retained",
            ));
        }
        let condition = match (&guard, multipart) {
            (PublicationGuard::Absent, false) => Condition::PutCreate,
            (PublicationGuard::Match { .. }, false) => Condition::PutMatch,
            (PublicationGuard::Absent, true) => Condition::CompleteCreate,
            (PublicationGuard::Match { .. }, true) => Condition::CompleteMatch,
        };
        let config = self
            .inner
            .transfer_config
            .get()
            .ok_or_else(|| UploadFailure::local("Missing storage configuration"))?;
        if !config
            .supports_condition(condition)
            .await
            .map_err(|message| UploadFailure {
                retryable: message.starts_with("transient:"),
                message,
                uncertain: false,
            })?
        {
            return Err(UploadFailure::local(
                "This endpoint cannot safely publish the staged object with its required condition",
            ));
        }
        if journal.completing {
            let head = self.object_head(key).await.map_err(|e| UploadFailure {
                message: format!("outcome_unknown: Target reconciliation failed: {e:?}"),
                retryable: false,
                uncertain: true,
            })?;
            let current = head
                .as_ref()
                .map(|head| {
                    head.e_tag()
                        .filter(|etag| !etag.is_empty())
                        .ok_or_else(|| UploadFailure::local("Target identity is missing"))
                })
                .transpose()?;
            if !guard.matches(current) {
                return Err(UploadFailure::local(
                    "conflict: Target changed after the staged publication; local data retained",
                ));
            }
        }
        snapshot
            .save_journal(journal)
            .await
            .map_err(UploadFailure::local)?;
        Ok(guard)
    }

    async fn snapshot_published(&self, key: &str, snapshot: &UploadSnapshot) -> bool {
        self.snapshot_verified_etag(key, snapshot).await.is_some()
    }

    async fn snapshot_verified_etag(&self, key: &str, snapshot: &UploadSnapshot) -> Option<String> {
        use tokio::io::AsyncReadExt;
        let head = self.object_head(key).await.ok().flatten()?;
        if head.content_length() != i64::try_from(snapshot.size).ok()
            || head.metadata().and_then(|m| m.get("r2-stage-snapshot")) != Some(&snapshot.token())
        {
            return None;
        }
        let etag = head.e_tag()?.to_string();
        let version = head
            .version_id()
            .filter(|v| *v != "null")
            .map(str::to_string);
        let scope = self.storage_scope(key);
        let identity = format!(
            "{}:{}:{}:{}:{}",
            key,
            etag,
            version.as_deref().unwrap_or_default(),
            snapshot.size,
            snapshot.token()
        );
        let context = self.read_operation_context(
            OperationKind::Get,
            &scope,
            &identity,
            crate::move_transfer::stream::protocol::attempt_timeout(snapshot.size),
        );
        execute_storage_operation(&context, || {
            let etag = etag.clone();
            let version = version.clone();
            async move {
                let response = self
                    .inner
                    .client
                    .get_object()
                    .bucket(&self.inner.bucket)
                    .key(key)
                    .if_match(&etag)
                    .set_version_id(version)
                    .send()
                    .await
                    .map_err(|error| AttemptError::from_sdk(&error))?;
                if response.e_tag() != Some(etag.as_str())
                    || response.content_length() != Some(snapshot.size as i64)
                {
                    return Err(AttemptError::permanent(
                        "Snapshot verification GET identity changed",
                    ));
                }
                let Ok(mut local) = tokio::fs::File::open(&snapshot.path).await else {
                    return Err(AttemptError::permanent("Snapshot file is unreadable"));
                };
                if local.metadata().await.map(|m| m.len()).ok() != Some(snapshot.size) {
                    return Err(AttemptError::permanent("Snapshot file size changed"));
                }
                let mut remote = response.body.into_async_read();
                let mut expected = vec![0; 1024 * 1024];
                let mut received = vec![0; 1024 * 1024];
                let mut remaining = snapshot.size;
                while remaining > 0 {
                    let count = remaining.min(expected.len() as u64) as usize;
                    let (a, b) = tokio::join!(
                        tokio::time::timeout(
                            Duration::from_secs(30),
                            local.read_exact(&mut expected[..count])
                        ),
                        tokio::time::timeout(
                            Duration::from_secs(30),
                            remote.read_exact(&mut received[..count])
                        )
                    );
                    if !matches!(a, Ok(Ok(_))) {
                        return Err(AttemptError::permanent("Snapshot local read failed"));
                    }
                    if !matches!(b, Ok(Ok(_))) {
                        return Err(AttemptError::transient(
                            "Snapshot verification GET body failed",
                        ));
                    }
                    if expected[..count] != received[..count] {
                        return Err(AttemptError::permanent("Snapshot content mismatch"));
                    }
                    remaining -= count as u64;
                }
                if matches!(
                    tokio::time::timeout(Duration::from_secs(30), remote.read(&mut received[..1]))
                        .await,
                    Ok(Ok(0))
                ) {
                    Ok(etag)
                } else {
                    Err(AttemptError::permanent(
                        "Snapshot verification GET returned extra bytes",
                    ))
                }
            }
        })
        .await
        .ok()
    }

    async fn upload_stage_file(
        &self,
        key: &str,
        snapshot: &UploadSnapshot,
        tracker: Option<&TransferTracker>,
        attempts: u32,
    ) -> Result<(), UploadFailure> {
        if snapshot.size > 0 {
            if let Some(config) = self.inner.transfer_config.get() {
                crate::move_transfer::stream::MultipartPlan::new(config, snapshot.size, None)
                    .map_err(UploadFailure::local)?;
            } else if snapshot.size > 5 * 1024u64.pow(4) - 5 * 1024u64.pow(3) {
                return Err(UploadFailure::local(
                    "Staged object exceeds the supported object size limit",
                ));
            }
        }
        if snapshot.size <= stage::MULTIPART_THRESHOLD {
            let mut journal = snapshot.journal().await.map_err(UploadFailure::local)?;
            if journal.completing {
                if let Some(etag) = self.snapshot_verified_etag(key, snapshot).await {
                    journal.published_etag = Some(etag);
                    snapshot
                        .save_journal(&journal)
                        .await
                        .map_err(UploadFailure::local)?;
                    return Ok(());
                }
            }
            let guard = self
                .snapshot_publication_guard(key, snapshot, &mut journal, false)
                .await?;
            let body = ByteStream::from_path(&snapshot.path)
                .await
                .map_err(UploadFailure::local)?;
            journal.completing = true;
            snapshot
                .save_journal(&journal)
                .await
                .map_err(UploadFailure::local)?;
            let request = self
                .inner
                .client
                .put_object()
                .bucket(&self.inner.bucket)
                .key(key)
                .metadata("r2-stage-snapshot", snapshot.token())
                .body(body);
            let request = match guard {
                stage::PublicationGuard::Absent => request.if_none_match("*"),
                stage::PublicationGuard::Match { etag } => request.if_match(etag),
            };
            match request
                .customize()
                .config_override(crate::move_transfer::stream::data_timeouts(snapshot.size))
                .send()
                .await
            {
                Ok(output) => {
                    journal.published_etag = output.e_tag().map(str::to_string);
                    snapshot
                        .save_journal(&journal)
                        .await
                        .map_err(UploadFailure::local)?;
                    return Ok(());
                }
                Err(error) => {
                    let failure = upload_error(&error);
                    if !failure.uncertain {
                        journal.completing = false;
                        snapshot
                            .save_journal(&journal)
                            .await
                            .map_err(UploadFailure::local)?;
                    }
                    return Err(failure);
                }
            }
        }
        self.upload_stage_multipart(key, snapshot, tracker, attempts)
            .await
    }

    async fn upload_stage_multipart(
        &self,
        key: &str,
        snapshot: &UploadSnapshot,
        tracker: Option<&TransferTracker>,
        attempts: u32,
    ) -> Result<(), UploadFailure> {
        let mut journal = snapshot.journal().await.map_err(UploadFailure::local)?;
        if journal.part_size != stage::planned_part_size(snapshot.size) {
            return Err(UploadFailure::local(
                "Multipart geometry does not match the persisted snapshot",
            ));
        }
        if journal.completing {
            if let Some(etag) = self.snapshot_verified_etag(key, snapshot).await {
                journal.published_etag = Some(etag);
                snapshot
                    .save_journal(&journal)
                    .await
                    .map_err(UploadFailure::local)?;
                return Ok(());
            }
        }
        let guard = self
            .snapshot_publication_guard(key, snapshot, &mut journal, true)
            .await?;
        if journal.upload_id.is_some() {
            // Resume reconciles every remote part, including those accepted
            // before a crash prevented the local ETag journal update.
            let mut marker: Option<String> = None;
            let mut parts = BTreeMap::new();
            loop {
                let upload_id = journal.upload_id.clone().unwrap_or_default();
                let request = self
                    .inner
                    .client
                    .list_parts()
                    .bucket(&self.inner.bucket)
                    .key(key)
                    .upload_id(upload_id)
                    .set_part_number_marker(marker.clone())
                    .max_parts(1000);
                let scope = self.storage_scope(key);
                let context = self
                    .read_operation_context(
                        OperationKind::ListParts,
                        &scope,
                        "",
                        Duration::from_secs(30),
                    )
                    .with_max_attempts(attempts);
                let response = execute_storage_operation(&context, || {
                    let request = request.clone();
                    async move {
                        request
                            .send()
                            .await
                            .map_err(|error| AttemptError::from_sdk(&error))
                    }
                })
                .await;
                let response = match response {
                    Ok(response) => response,
                    Err(error) if matches!(error.class(), StorageErrorClass::NotFound) => {
                        journal.upload_id = None;
                        journal.parts.clear();
                        journal.completing = false;
                        break;
                    }
                    Err(error) => {
                        let mut failure = UploadFailure::local(error.to_string());
                        failure.retryable = matches!(error.class(), StorageErrorClass::Transient);
                        failure.uncertain = false;
                        return Err(failure);
                    }
                };
                for part in response.parts() {
                    if let (Some(number), Some(etag), Some(size)) =
                        (part.part_number(), part.e_tag(), part.size())
                    {
                        if number > 0
                            && (number as u64) <= stage::part_count(snapshot.size)
                            && size >= 0
                            && size as u64 == stage::part_range(snapshot.size, number as u64 - 1).1
                        {
                            parts.insert(number, etag.to_string());
                        }
                    }
                }
                if !response.is_truncated().unwrap_or(false) {
                    break;
                }
                let next = response
                    .next_part_number_marker()
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let next_number = next.as_deref().and_then(|value| value.parse::<u64>().ok());
                let previous = marker
                    .as_deref()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0);
                if next_number.is_none_or(|number| number <= previous || number > 10000) {
                    return Err(UploadFailure::local(
                        "ListParts returned an invalid continuation marker",
                    ));
                }
                marker = next;
            }
            if journal.upload_id.is_some() {
                journal.parts = parts;
            }
            snapshot
                .save_journal(&journal)
                .await
                .map_err(UploadFailure::local)?;
        }
        if journal.upload_id.is_none() {
            let created = self
                .inner
                .client
                .create_multipart_upload()
                .bucket(&self.inner.bucket)
                .key(key)
                .metadata("r2-stage-snapshot", snapshot.token())
                .send()
                .await
                .map_err(|e| upload_error(&e))?;
            journal.upload_id = Some(
                created
                    .upload_id()
                    .ok_or_else(|| UploadFailure::local("Storage provider returned no upload id"))?
                    .to_string(),
            );
            snapshot
                .save_journal(&journal)
                .await
                .map_err(UploadFailure::local)?;
        }
        let upload_id = journal.upload_id.clone().unwrap_or_default();
        let journal = AsyncMutex::new(journal);
        let failed = AtomicBool::new(false);
        let results = stream::iter((0..stage::part_count(snapshot.size)).map(|index| {
            let journal = &journal;
            let failed = &failed;
            let upload_id = &upload_id;
            async move {
                if failed.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let part_number = (index + 1) as i32;
                let (offset, length) = stage::part_range(snapshot.size, index);
                if journal.lock().await.parts.contains_key(&part_number) {
                    return Ok(());
                }
                let outcome: Result<(), UploadFailure> = async {
                    let scope = self.storage_scope(key);
                    let identity = format!(
                        "{}:{}:{}:{}:{}:{}",
                        key, upload_id, part_number, offset, length, snapshot.generation
                    );
                    let context = self
                        .read_operation_context(
                            OperationKind::UploadPart,
                            &scope,
                            &identity,
                            crate::move_transfer::stream::protocol::attempt_timeout(length),
                        )
                        .with_max_attempts(attempts);
                    let response = execute_storage_operation(&context, || async {
                        let body = ByteStream::read_from()
                            .path(&snapshot.path)
                            .offset(offset)
                            .length(Length::Exact(length))
                            .build()
                            .await
                            .map_err(|error| AttemptError::permanent(error.to_string()))?;
                        self.inner
                            .client
                            .upload_part()
                            .bucket(&self.inner.bucket)
                            .key(key)
                            .upload_id(upload_id)
                            .part_number(part_number)
                            .body(body)
                            .customize()
                            .config_override(crate::move_transfer::stream::data_timeouts(length))
                            .send()
                            .await
                            .map_err(|error| AttemptError::from_sdk(&error))
                    })
                    .await
                    .map_err(|error| UploadFailure {
                        message: format!("UploadPart: {error}"),
                        retryable: matches!(error.class(), StorageErrorClass::Transient),
                        uncertain: false,
                    })?;
                    let etag = response
                        .e_tag()
                        .ok_or_else(|| UploadFailure::local("UploadPart returned no ETag"))?
                        .to_string();
                    let mut journal = journal.lock().await;
                    journal.parts.insert(part_number, etag);
                    snapshot
                        .save_journal(&journal)
                        .await
                        .map_err(UploadFailure::local)?;
                    if let Some(tracker) = tracker {
                        tracker.add(length);
                    }
                    Ok(())
                }
                .await;
                if outcome.is_err() {
                    failed.store(true, Ordering::SeqCst);
                }
                outcome
            }
        }))
        .buffer_unordered(stage::PART_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
        // Already issued parts settle and are recorded; no new part requests
        // start after the first failure. Keep the remote upload for resumption.
        for result in results {
            result?;
        }
        let mut journal = journal.into_inner();
        if journal.parts.len() as u64 != stage::part_count(snapshot.size) {
            return Err(UploadFailure::local("Multipart upload is incomplete"));
        }
        let parts: Vec<_> = journal
            .parts
            .iter()
            .map(|(&number, etag)| {
                CompletedPart::builder()
                    .part_number(number)
                    .e_tag(etag)
                    .build()
            })
            .collect();
        journal.completing = true;
        snapshot
            .save_journal(&journal)
            .await
            .map_err(UploadFailure::local)?;
        let request = self
            .inner
            .client
            .complete_multipart_upload()
            .bucket(&self.inner.bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            );
        let request = match guard {
            stage::PublicationGuard::Absent => request.if_none_match("*"),
            stage::PublicationGuard::Match { etag } => request.if_match(etag),
        };
        let result = request.send().await;
        match result {
            Ok(output) => {
                journal.published_etag = output.e_tag().map(str::to_string);
                snapshot
                    .save_journal(&journal)
                    .await
                    .map_err(UploadFailure::local)?;
                Ok(())
            }
            Err(error) => {
                if let Some(etag) = self.snapshot_verified_etag(key, snapshot).await {
                    journal.published_etag = Some(etag);
                    snapshot
                        .save_journal(&journal)
                        .await
                        .map_err(UploadFailure::local)?;
                    return Ok(());
                }
                let failure = upload_error(&error);
                if !failure.uncertain {
                    journal.completing = false;
                    snapshot
                        .save_journal(&journal)
                        .await
                        .map_err(UploadFailure::local)?;
                }
                Err(failure)
            }
        }
    }

    // ---- Flushing ----

    /// Starts the background task that uploads staged writes for this mount.
    pub fn spawn_flusher(&self, app: tauri::AppHandle, mount_id: String) -> JoinHandle<()> {
        let fs = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(stage::FLUSH_SCAN_INTERVAL).await;
                fs.flush_due(&app, &mount_id).await;
            }
        })
    }

    /// One scanner tick: starts uploads for every file the client has stopped
    /// writing to, up to the mount's concurrency limit.
    async fn flush_due(&self, app: &tauri::AppHandle, mount_id: &str) {
        let candidates: Vec<_> = self
            .inner
            .stages
            .lock()
            .await
            .iter()
            .map(|(&id, handle)| (id, handle.clone()))
            .collect();
        for (id, handle) in candidates {
            let Ok(permit) = self.inner.flush_slots.clone().try_acquire_owned() else {
                return;
            };
            let Ok(guard) = handle.try_lock() else {
                continue;
            };
            if guard.evicted || !guard.is_due(Instant::now()) {
                continue;
            }
            let key = guard.key.clone();
            drop(guard);
            let fs = self.clone();
            let app = app.clone();
            let mount_id = mount_id.to_string();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(error) = fs.flush_fenced(id, stage::UPLOAD_ATTEMPTS, false).await {
                    let _ = app.emit(
                        "mount-flush-error",
                        FlushErrorPayload {
                            mount_id,
                            bucket: fs.inner.bucket.clone(),
                            key,
                            error: error.message,
                        },
                    );
                }
            });
        }
    }

    /// Publishes the stage for `id` under the fence and lifecycle locks of the
    /// key the stage has once they are held. A rename rekeys a stage only
    /// inside its exclusive fence, so a key read before the wait can be stale
    /// when it ends, and publishing under a stale key's locks would let a
    /// DELETE or rename of the real key run alongside this upload.
    async fn flush_fenced(
        &self,
        id: fileid3,
        attempts: u32,
        explicit: bool,
    ) -> Result<(), UploadFailure> {
        for _ in 0..FENCED_KEY_ATTEMPTS {
            let Some(key) = self.stage_guard(id).await.map(|guard| guard.key.clone()) else {
                return Ok(());
            };
            let _namespace = self.inner.namespace.read().await;
            let _fence = self.fence_exact_key_shared(&key).await;
            let lifecycle = self.lifecycle(&key);
            let _access = lifecycle.access.read().await;
            let _publication = lifecycle.publication.lock().await;
            // DELETE may have unpublished this stage, and a rename rekeyed it,
            // while this task waited; resolve again under the locks.
            match self.stage_guard(id).await.map(|guard| guard.key.clone()) {
                None => return Ok(()),
                Some(current) if current == key => {
                    return self.flush_one_locked(id, attempts, explicit).await;
                }
                Some(_) => {}
            }
        }
        Err(UploadFailure {
            message: "Staged file kept moving; publication deferred".into(),
            retryable: true,
            uncertain: false,
        })
    }

    /// Caller holds the shared fence, lifecycle and publication locks of the
    /// stage's current key (see [`Self::flush_fenced`]), or the exclusive
    /// fence of a rename covering it. Stage data is locked only while making
    /// its immutable snapshot and settling durable state.
    async fn flush_one_locked(
        &self,
        id: fileid3,
        attempts: u32,
        explicit: bool,
    ) -> Result<(), UploadFailure> {
        let key = match self.stage_guard(id).await {
            Some(guard) => guard.key.clone(),
            None => return Ok(()),
        };
        self.ensure_rename_available(&key)
            .map_err(|_| UploadFailure::local("Rename outcome is pending; publication paused"))?;
        if tokio::fs::try_exists(self.namespace_journal_path(&key))
            .await
            .map_err(UploadFailure::local)?
        {
            return Err(UploadFailure::local(
                "Namespace mutation outcome is unknown; publication paused",
            ));
        }
        let Some(mut guard) = self.stage_guard(id).await else {
            return Ok(());
        };
        if !guard.dirty {
            return Ok(());
        }
        if matches!(guard.state,FlushState::Failed {retry_after} if retry_after>Instant::now()) {
            return Err(UploadFailure::local(
                "Retry is scheduled after the current cooldown",
            ));
        }
        let reconcile_only = if guard.state == FlushState::Paused && explicit {
            match guard.snapshot.as_ref() {
                Some(snapshot) => {
                    snapshot
                        .journal()
                        .await
                        .map_err(UploadFailure::local)?
                        .completing
                }
                None => false,
            }
        } else {
            false
        };
        if guard.state == FlushState::Paused
            && (!reconcile_only
                || guard.last_error.as_deref()
                    == Some("Destination replacement pending; retained for recovery"))
        {
            return Err(UploadFailure::local(
                guard
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "Upload requires recovery".into()),
            ));
        }
        let snapshot = guard
            .upload_snapshot()
            .await
            .map_err(UploadFailure::local)?;
        let job = FlushJob {
            id,
            key: guard.key.clone(),
            snapshot,
        };
        guard.state = FlushState::Uploading;
        guard.persist().await.map_err(UploadFailure::local)?;
        self.record_stage_health(id, &guard);
        drop(guard);
        let result = self
            .upload_with_attempts(id, &job.key, &job.snapshot, attempts)
            .await;
        let Some(mut guard) = self.stage_guard(id).await else {
            return result.map(|_| ());
        };
        guard
            .replay_pending_write()
            .await
            .map_err(UploadFailure::local)?;
        match &result {
            Ok(etag) => {
                guard.publication_guard =
                    Some(stage::PublicationGuard::Match { etag: etag.clone() });
                guard.state = FlushState::Idle;
                guard.last_error = None;
                guard.snapshot = None;
                guard.release_snapshot_accounting();
                if stage::upload_settles_stage(job.snapshot.generation, guard.dirty_gen) {
                    guard.dirty = false;
                    guard.first_dirty_at = None;
                    guard.flush_requested = false;
                    self.update_inode_attrs(job.id, job.snapshot.size, guard.mtime_secs);
                } else if let Some(progress) = self.progress() {
                    progress.waiting(id, &guard.key, guard.size);
                }
                guard.persist().await.map_err(UploadFailure::local)?;
                self.record_stage_health(id, &guard);
                job.snapshot.remove().await;
            }
            Err(error) => {
                guard.last_error = Some(error.message.clone());
                let guarded = job
                    .snapshot
                    .journal()
                    .await
                    .map_err(UploadFailure::local)?
                    .precondition
                    .is_some();
                guard.state = if error.retryable || (error.uncertain && guarded) {
                    FlushState::Failed {
                        retry_after: Instant::now() + stage::FLUSH_RETRY_COOLDOWN,
                    }
                } else {
                    FlushState::Paused
                };
                guard.persist().await.map_err(UploadFailure::local)?;
                self.record_stage_health(id, &guard);
            }
        }
        drop(guard);
        if result.is_ok() {
            self.evict_stage(id).await;
        }
        result.map(|_| ())
    }

    async fn flush_stage_blocking(&self, id: fileid3) -> Result<(), nfsstat3> {
        // Rename owns the exclusive namespace fence, so every existing
        // publisher has already finished and no new writer can start.
        self.flush_one_locked(id, stage::UPLOAD_ATTEMPTS, true)
            .await
            .map_err(|error| {
                log::error!("mount: failed to flush inode {}: {}", id, error);
                nfsstat3::NFS3ERR_IO
            })
    }

    /// Every staged file whose key sits under `prefix`.
    async fn stages_under(&self, prefix: &str) -> Vec<fileid3> {
        let staged: Vec<(fileid3, Arc<AsyncMutex<Stage>>)> = {
            let stages = self.inner.stages.lock().await;
            stages
                .iter()
                .map(|(&id, handle)| (id, handle.clone()))
                .collect()
        };

        let mut under = Vec::new();
        for (id, handle) in staged {
            // Locked outside the map so a file that is mid-write is waited for
            // rather than missed — this decides what a rename copies.
            let guard = handle.lock_owned().await;
            if !guard.evicted && guard.key.starts_with(prefix) {
                under.push(id);
            }
        }
        under
    }

    /// Flushes every stage under `prefix`, used before a directory is renamed:
    /// the copy is server-side and cannot see content that is still only on
    /// this machine.
    async fn flush_stages_under(&self, prefix: &str) -> Result<(), nfsstat3> {
        for id in self.stages_under(prefix).await {
            self.flush_stage_blocking(id).await?;
        }
        Ok(())
    }

    /// Points a stage at the key its content belongs to after a rename.
    ///
    /// Without this the next flush would upload to the name the file was moved
    /// away from.
    async fn rekey_stage(&self, id: fileid3, new_key: &str) -> Result<(), nfsstat3> {
        let Some(mut guard) = self.stage_guard(id).await else {
            return Ok(());
        };
        guard.key = new_key.to_string();
        guard.persist().await.map_err(|_| nfsstat3::NFS3ERR_IO)
    }

    /// Re-keys every stage under a directory that has just been renamed.
    async fn rekey_stages_under(&self, from_prefix: &str, to_prefix: &str) -> Result<(), nfsstat3> {
        for id in self.stages_under(from_prefix).await {
            let Some(mut guard) = self.stage_guard(id).await else {
                continue;
            };
            if let Some(new_key) = rewrite_key(&guard.key, from_prefix, to_prefix) {
                guard.key = new_key;
                guard.persist().await.map_err(|_| nfsstat3::NFS3ERR_IO)?;
            }
        }
        Ok(())
    }

    /// Clears the in-flight marker on every stage.
    ///
    /// Called once the flusher task has been aborted: nothing is uploading any
    /// more, whatever the state says, and a stage left marked `Uploading` would
    /// be skipped by the drain that follows.
    pub async fn reset_flush_state(&self) {
        let staged: Vec<Arc<AsyncMutex<Stage>>> = {
            let stages = self.inner.stages.lock().await;
            stages.values().cloned().collect()
        };
        for handle in staged {
            let mut guard = handle.lock_owned().await;
            if !guard.evicted && guard.state == FlushState::Uploading {
                guard.state = FlushState::Idle;
            }
        }
    }

    /// Waits for every upload that is already in flight to finish.
    ///
    /// The flusher's task handle covers only the scanner — each upload runs as
    /// a detached task holding one of the mount's flush permits — so taking
    /// every permit is the only way to know that nothing is still writing to
    /// the bucket or still reading a staging file that is about to be deleted.
    pub async fn wait_for_flushes(&self) {
        if let Ok(permits) = self
            .inner
            .flush_slots
            .acquire_many(stage::MAX_CONCURRENT_FLUSHES as u32)
            .await
        {
            drop(permits);
        }
    }

    /// One pass over every dirty stage, uploading in place. Returns how many are
    /// still unflushed afterwards.
    ///
    /// Publishers own their lifecycle fences and resource permits through disk
    /// snapshotting, network publication and durable settlement.
    async fn flush_everything(&self, attempts: u32) -> usize {
        let staged: Vec<_> = self.inner.stages.lock().await.keys().copied().collect();
        let pending = stream::iter(staged.into_iter().map(|id| async move {
            let Ok(permit) = self.inner.flush_slots.clone().acquire_owned().await else {
                return 1;
            };
            let fs = self.clone();
            // A drain deadline cancels waiting, not an already-started disk
            // copy, journal write or PUT. The owned task keeps every lifecycle
            // fence and permit until those operations actually settle.
            tokio::spawn(async move {
                let _permit = permit;
                usize::from(fs.flush_fenced(id, attempts, true).await.is_err())
            })
            .await
            .unwrap_or(1)
        }))
        .buffer_unordered(stage::MAX_CONCURRENT_FLUSHES)
        .collect::<Vec<_>>()
        .await;
        let failures = pending.into_iter().sum();
        self.pending_upload_count().await.max(failures)
    }

    /// Uploads everything still dirty, retrying up to `rounds` times with
    /// `attempts` tries per file. Returns how many files are still not in the
    /// bucket.
    ///
    /// The exit path passes one of each: it shares a short budget across every
    /// mount, and a file that is failing must not spend it on backoff sleeps
    /// that would starve the files after it.
    pub async fn drain(&self, rounds: u32, attempts: u32) -> usize {
        let mut pending = 0;
        for _ in 0..rounds.max(1) {
            pending = self.flush_everything(attempts).await;
            if pending == 0 {
                return 0;
            }
        }
        pending
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid is always safe to call and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(unix)]
fn current_gid() -> u32 {
    // SAFETY: getgid is always safe to call and cannot fail.
    unsafe { libc::getgid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[cfg(not(unix))]
fn current_gid() -> u32 {
    0
}

#[async_trait]
impl NFSFileSystem for S3NfsFs {
    fn capabilities(&self) -> VFSCapabilities {
        if self.inner.read_only {
            VFSCapabilities::ReadOnly
        } else {
            VFSCapabilities::ReadWrite
        }
    }

    fn root_dir(&self) -> fileid3 {
        ROOT_ID
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let dir = self.dir_inode(dirid)?;
        let name = std::str::from_utf8(filename).map_err(|_| nfsstat3::NFS3ERR_NOENT)?;

        if name.is_empty() || name == "." {
            return Ok(dirid);
        }
        if name == ".." {
            return Ok(dir.parent);
        }

        let dir_key = normalize_dir_key(&dir.key);
        self.lookup_child(dirid, &dir_key, name)
            .await?
            .map(|(id, _)| id)
            .ok_or(nfsstat3::NFS3ERR_NOENT)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let inode = self.inode(id)?;
        if let Some(guard) = self.stage_guard(id).await {
            return Ok(self.staged_attr(id, &inode, guard.size, guard.mtime_secs));
        }
        Ok(self.attr_of(id, &inode))
    }

    async fn setattr(&self, id: fileid3, setattr: sattr3) -> Result<fattr3, nfsstat3> {
        self.ensure_writable()?;
        let _namespace = self.inner.namespace.read().await;
        let (_fence, inode) = self.fence_inode_shared(id).await?;
        let lifecycle = self.lifecycle(&inode.key);
        let _access = lifecycle.access.read().await;
        self.ensure_writable()?;
        let inode = self.inode(id)?;
        self.ensure_rename_available(&inode.key)?;
        if tokio::fs::try_exists(self.namespace_journal_path(&inode.key))
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
        {
            return Err(nfsstat3::NFS3ERR_IO);
        }

        // Directories have no content to resize, and mode or owner changes have
        // nowhere to go in object storage, so they are accepted and reported
        // back as the canned attributes.
        if inode.kind != EntryKind::File {
            return Ok(self.attr_of(id, &inode));
        }

        let times_touched = !matches!(setattr.mtime, set_mtime::DONT_CHANGE)
            || !matches!(setattr.atime, set_atime::DONT_CHANGE);

        if let set_size3::size(size) = setattr.size {
            let initial_guard = if size == 0 && self.stage_guard(id).await.is_none() {
                let head = self
                    .object_head(&inode.key)
                    .await?
                    .ok_or(nfsstat3::NFS3ERR_STALE)?;
                Some(stage::PublicationGuard::Match {
                    etag: head
                        .e_tag()
                        .filter(|etag| !etag.is_empty())
                        .ok_or(nfsstat3::NFS3ERR_IO)?
                        .to_string(),
                })
            } else {
                None
            };
            let mut guard = if size == 0 {
                self.reset_stage(id, &inode).await?
            } else {
                self.ensure_stage(id, &inode).await?
            };
            if guard.publication_guard.is_none() {
                guard.publication_guard = initial_guard;
            }
            self.reserve_stage(
                id,
                size.max(guard.snapshot.as_ref().map(|s| s.size).unwrap_or(0))
                    .saturating_add(4096),
            )
            .await?;
            let newly_dirty = !guard.dirty;
            let pending_dirty_at = guard
                .first_dirty_at
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
            self.record_pending_stage_health(id, size, pending_dirty_at, guard.last_error.clone());
            let truncate_result = guard.truncate_durable(size, now_secs()).await;
            if let Err(e) = truncate_result {
                self.record_stage_health(id, &guard);
                return Err({
                    log::error!("mount: failed to resize \"{}\": {}", inode.key, e);
                    nfsstat3::NFS3ERR_IO
                });
            }
            let reservation = guard.reservation_bytes().await;
            self.inner.quota.lock().await.restore(id, reservation);
            self.record_stage_health(id, &guard);
            guard.flush_requested = times_touched;
            if newly_dirty {
                guard.reported_size = guard.size;
                if let Some(progress) = self.progress() {
                    progress.waiting(id, &guard.key, guard.size);
                }
            }
            return Ok(self.staged_attr(id, &inode, guard.size, guard.mtime_secs));
        }

        if let Some(mut guard) = self.stage_guard(id).await {
            // The `utimes` a client sends when it finishes copying a file is the
            // only end-of-write signal NFSv3 offers, so it short-circuits the
            // debounce instead of waiting the file out.
            if guard.dirty && times_touched {
                guard.flush_requested = true;
                // The copy is over, so the staged size is final — bring the
                // queued transfer row up to date before the upload starts.
                guard.reported_size = guard.size;
                if let Some(progress) = self.progress() {
                    progress.waiting(id, &guard.key, guard.size);
                }
            }
            return Ok(self.staged_attr(id, &inode, guard.size, guard.mtime_secs));
        }

        // mode/uid/gid are accepted silently: refusing them breaks `cp -p` and
        // Finder's copy for no gain.
        Ok(self.attr_of(id, &inode))
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let _namespace = self.inner.namespace.read().await;
        let (_fence, inode) = self.fence_inode_shared(id).await?;
        self.ensure_rename_available(&inode.key)?;
        if inode.kind != EntryKind::File {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }

        // Staged content is newer than anything in the bucket.
        if let Some(mut guard) = self.stage_guard(id).await {
            let data = guard.read_at(offset, count as usize).await.map_err(|e| {
                log::error!(
                    "mount: failed to read the stage for \"{}\": {}",
                    inode.key,
                    e
                );
                nfsstat3::NFS3ERR_IO
            })?;
            let size = guard.size;
            let eof = offset.saturating_add(data.len() as u64) >= size;
            return Ok((data, eof));
        }

        if count == 0 {
            return Ok((Vec::new(), offset >= inode.size));
        }
        let identity = self.read_identity(id, &inode.key).await?;
        if offset >= identity.size {
            return Ok((Vec::new(), offset >= identity.size));
        }

        // Served from the chunk cache: one object fetch covers dozens of
        // client transfers, and concurrent transfers share a fetch instead of
        // each paying an S3 round trip.
        let end = offset.saturating_add(u64::from(count)).min(identity.size);
        let (first, last) = read_cache::chunks_covering(offset, end);

        let mut data = Vec::with_capacity((end - offset) as usize);
        for index in first..=last {
            let chunk = self.chunk_bytes(id, &inode.key, index, &identity).await?;
            let chunk_start = read_cache::chunk_start(index);
            let from = offset.max(chunk_start) - chunk_start;
            let to = (end - chunk_start).min(chunk.len() as u64);
            if to > from {
                data.extend_from_slice(&chunk[from as usize..to as usize]);
            }
        }

        // Read-ahead only once this reader has proven sequential: a cold peek
        // — a preview thumbnail, a magic-byte sniff — must not cost
        // speculative megabytes on a metered backend.
        let bytes_into_last = end - read_cache::chunk_start(last);
        if self.inner.read_cache.note_read(id, last, bytes_into_last) {
            self.prefetch_after(id, &inode.key, last, &identity);
        }

        // Producing fewer bytes than the request spans means the object ends
        // before `identity.size` says it does. That is the true end of file:
        // answering "no data, not EOF" would make the client re-issue the same
        // read forever.
        let produced_end = offset.saturating_add(data.len() as u64);
        let eof = produced_end >= identity.size || produced_end < end;
        Ok((data, eof))
    }

    async fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> Result<fattr3, nfsstat3> {
        self.ensure_writable()?;
        let _namespace = self.inner.namespace.read().await;
        let (_fence, inode) = self.fence_inode_shared(id).await?;
        let lifecycle = self.lifecycle(&inode.key);
        let _access = lifecycle.access.read().await;
        self.ensure_writable()?;
        let inode = self.inode(id)?;
        self.ensure_rename_available(&inode.key)?;
        if tokio::fs::try_exists(self.namespace_journal_path(&inode.key))
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
        {
            return Err(nfsstat3::NFS3ERR_IO);
        }
        if inode.kind != EntryKind::File {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }

        let mut guard = self.ensure_stage(id, &inode).await?;
        let size = guard
            .size
            .max(
                offset
                    .checked_add(data.len() as u64)
                    .ok_or(nfsstat3::NFS3ERR_FBIG)?,
            )
            .max(guard.snapshot.as_ref().map(|s| s.size).unwrap_or(0));
        self.reserve_stage(
            id,
            size.saturating_add(data.len() as u64).saturating_add(4096),
        )
        .await?;
        let newly_dirty = !guard.dirty;
        let pending_dirty_at = guard
            .first_dirty_at
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        self.record_pending_stage_health(id, size, pending_dirty_at, guard.last_error.clone());
        let write_result = guard.write_durable(offset, data, now_secs()).await;
        if let Err(e) = write_result {
            self.record_stage_health(id, &guard);
            return Err({
                log::error!("mount: failed to stage a write to \"{}\": {}", inode.key, e);
                nfsstat3::NFS3ERR_IO
            });
        }
        let reservation = guard.reservation_bytes().await;
        self.inner.quota.lock().await.restore(id, reservation);
        self.record_stage_health(id, &guard);

        // The copy is surfaced as a queued upload from its first write, and
        // its reported total follows the staged size in steps — the first
        // event alone would freeze the row at one transfer's worth (128 KiB)
        // for the whole copy, and the dock's aggregate math with it.
        if newly_dirty || guard.size.saturating_sub(guard.reported_size) >= WAITING_REPORT_STEP {
            guard.reported_size = guard.size;
            if let Some(progress) = self.progress() {
                progress.waiting(id, &guard.key, guard.size);
            }
        }

        Ok(self.staged_attr(id, &inode, guard.size, guard.mtime_secs))
    }

    async fn create(
        &self,
        dirid: fileid3,
        filename: &filename3,
        attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        self.ensure_writable()?;
        let _namespace = self.inner.namespace.read().await;
        self.dir_inode(dirid)?;
        let name = self.child_name(filename)?;
        let (_fence, _, key) = self.fence_new_child(dirid, &name, false).await?;
        let lifecycle = self.lifecycle(&key);
        let _access = lifecycle.access.write().await;
        self.ensure_writable()?;
        self.ensure_key_settled(&key).await?;
        self.ensure_rename_available(&key)?;

        // CREATE is not only used for new files: a client that misses in its
        // own cache sends it for a name that is already there, and `touch` on
        // an existing file is exactly that. Asking S3 first is what keeps the
        // zero-byte object below from blanking real content.
        let existing = self.head_object(&key).await?;
        let truncating = matches!(attr.size, set_size3::size(0));

        if let (CreateAction::OpenExisting, Some((size, mtime))) =
            (create_action(existing.is_some(), truncating), existing)
        {
            let id = self.intern_child(&key, dirid, EntryKind::File, size, mtime)?;
            let inode = self.inode(id)?;
            return Ok((id, self.attr_of(id, &inode)));
        }

        // The zero-byte object goes in immediately rather than when the first
        // write is flushed. That is what keeps lookup and readdir free of
        // overlay logic: the name exists in S3 from the moment it exists here.
        self.put_empty_object(&key, truncating).await?;

        let id = self.intern_child(&key, dirid, EntryKind::File, 0, now_secs())?;
        // Whatever was staged or cached belonged to the content just replaced.
        self.discard_stage(id).await;
        self.inner.read_cache.forget_file(id);
        self.invalidate_dir(dirid);

        let inode = self.inode(id)?;
        Ok((id, self.attr_of(id, &inode)))
    }

    async fn create_exclusive(
        &self,
        dirid: fileid3,
        filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        self.ensure_writable()?;
        let _namespace = self.inner.namespace.read().await;
        self.dir_inode(dirid)?;
        let name = self.child_name(filename)?;
        let (_fence, _, key) = self.fence_new_child(dirid, &name, false).await?;
        let lifecycle = self.lifecycle(&key);
        let _access = lifecycle.access.write().await;
        self.ensure_writable()?;
        self.ensure_key_settled(&key).await?;
        self.ensure_rename_available(&key)?;

        // The provider must honor conditional creation. Unsupported providers
        // return an error; silently weakening exclusivity risks overwrites.
        if self.head_object(&key).await?.is_some() {
            return Err(nfsstat3::NFS3ERR_EXIST);
        }

        self.put_empty_object(&key, false).await?;
        let id = self.intern_child(&key, dirid, EntryKind::File, 0, now_secs())?;
        self.invalidate_dir(dirid);
        Ok(id)
    }

    async fn mkdir(
        &self,
        dirid: fileid3,
        dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        self.ensure_writable()?;
        let _namespace = self.inner.namespace.read().await;
        self.dir_inode(dirid)?;
        let name = self.child_name(dirname)?;

        // A zero-byte object whose key ends in `/` is the folder marker every
        // S3 tool understands, and is what makes an empty directory visible at
        // all — without it there is no prefix to list.
        let (_fence, dir_key, key) = self.fence_new_child(dirid, &name, true).await?;
        let children = self.children_of(dirid, &dir_key).await?;
        if children.iter().any(|child| child.name == name) {
            return Err(nfsstat3::NFS3ERR_EXIST);
        }
        let lifecycle = self.lifecycle(&key);
        let _access = lifecycle.access.write().await;
        self.ensure_writable()?;
        self.ensure_key_settled(&key).await?;
        self.ensure_rename_available(&key)?;
        self.put_empty_object(&key, false).await?;

        let id = self.intern_child(&key, dirid, EntryKind::Dir, DIR_SIZE, now_secs())?;
        self.invalidate_dir(dirid);

        let inode = self.inode(id)?;
        Ok((id, self.attr_of(id, &inode)))
    }

    async fn remove(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3> {
        self.ensure_writable()?;
        let _namespace = self.inner.namespace.read().await;
        self.dir_inode(dirid)?;
        let name = self.child_name(filename)?;

        // RMDIR and REMOVE both land here, so the entry's kind decides what
        // "delete" means.
        let (_fence, id, target) = self.fence_existing_child(dirid, &name).await?;
        let lifecycle = self.lifecycle(&target.key);
        let _access = lifecycle.access.write().await;
        self.ensure_writable()?;
        self.ensure_key_settled(&target.key).await?;
        self.ensure_rename_available(&target.key)?;

        match target.kind {
            EntryKind::File => {
                self.delete_object(&target.key).await?;
                // Deleting the file is an explicit instruction to throw the
                // unuploaded content away, cached reads included.
                self.discard_stage(id).await;
                self.inner.read_cache.forget_file(id);
            }
            EntryKind::Dir => {
                let target_key = normalize_dir_key(&target.key);
                if !self.dir_is_empty(&target_key).await? {
                    return Err(nfsstat3::NFS3ERR_NOTEMPTY);
                }
                // A prefix that only ever held objects has no marker to delete;
                // S3 reports that as success either way.
                self.delete_object(&target_key).await?;
                self.invalidate_dir(id);
            }
        }

        self.inner.read_identities.lock().await.remove(&id);
        if let Ok(mut inodes) = self.inner.inodes.write() {
            inodes.remove(id);
        }
        self.invalidate_dir(dirid);
        Ok(())
    }

    async fn rename(
        &self,
        from_dirid: fileid3,
        from_filename: &filename3,
        to_dirid: fileid3,
        to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        self.ensure_writable()?;
        self.dir_inode(from_dirid)?;
        self.dir_inode(to_dirid)?;
        let from_name = self.child_name(from_filename)?;
        let to_name = self.child_name(to_filename)?;

        let Some((_fence, id, source, to_dir_key)) = self
            .fence_rename(from_dirid, &from_name, to_dirid, &to_name)
            .await?
        else {
            return Ok(());
        };
        // Storage requests outlive `stop_accepting_writes` so the unmount
        // drain can publish; a rename that waited out its fence past that
        // point must not start copying.
        self.ensure_writable()?;
        let is_dir = source.kind == EntryKind::Dir;
        let target_key = child_key(&to_dir_key, &to_name, is_dir);
        let journal_path = self.rename_journal_path(&source.key, &target_key);
        self.ensure_rename_not_blocked(&source.key, &target_key, &journal_path)?;
        self.ensure_key_settled(&source.key).await?;
        self.ensure_key_settled(&target_key).await?;

        // What is already at the destination decides whether this rename is
        // allowed at all. Checked after the same-name shortcut above, since
        // there the destination *is* the source.
        let destination = self
            .lookup_child_fenced(to_dirid, &to_dir_key, &to_name)
            .await?;
        let destination_dir_is_empty = match &destination {
            Some((_, inode)) if inode.kind == EntryKind::Dir => {
                self.dir_is_empty(&normalize_dir_key(&inode.key)).await?
            }
            _ => true,
        };
        let resuming = tokio::fs::try_exists(self.rename_journal_path(&source.key, &target_key))
            .await
            .unwrap_or(false);
        match classify_rename_target(
            is_dir,
            destination.as_ref().map(|(_, inode)| inode.kind),
            destination_dir_is_empty || resuming,
        ) {
            RenameTarget::Replace => {}
            RenameTarget::KindMismatch => return Err(nfsstat3::NFS3ERR_EXIST),
            RenameTarget::NotEmpty => return Err(nfsstat3::NFS3ERR_NOTEMPTY),
        }

        let mut destination_before_pause = None;
        if let Some((dest_id, dest_inode)) = &destination {
            if dest_inode.kind == EntryKind::File {
                if let Some(mut stage) = self.stage_guard(*dest_id).await {
                    // Retain the displaced bytes for export until the rename
                    // finishes, but never let them overwrite a copied target
                    // after a partially successful rename returns an error.
                    destination_before_pause =
                        Some((*dest_id, stage.state, stage.last_error.clone()));
                    stage.state = FlushState::Paused;
                    stage.last_error =
                        Some("Destination replacement pending; retained for recovery".into());
                    stage.persist().await.map_err(|_| nfsstat3::NFS3ERR_IO)?;
                }
            }
        }
        let result = if is_dir {
            self.rename_dir(id, &source.key, &target_key, to_dirid)
                .await
        } else {
            self.rename_file(id, &source.key, &target_key, to_dirid)
                .await
        };
        if let Err(status) = result {
            // No durable rename journal means no destination publication was
            // dispatched. Its previously acknowledged stage remains usable.
            if matches!(
                tokio::fs::try_exists(self.rename_journal_path(&source.key, &target_key)).await,
                Ok(false)
            ) {
                if let Some((id, state, error)) = destination_before_pause {
                    if let Some(mut stage) = self.stage_guard(id).await {
                        stage.state = state;
                        stage.last_error = error;
                        stage.persist().await.map_err(|_| nfsstat3::NFS3ERR_IO)?;
                    }
                }
            }
            return Err(status);
        }

        // A replaced file's cached chunks and staged content both belong to a
        // file that no longer exists. Dropping the stage matters most: left
        // alone, its debounced flusher would later upload the replaced file's
        // old bytes over the freshly renamed object. The moved file keeps its
        // own cache: its id and bytes did not change.
        if let Some((dest_id, dest_inode)) = &destination {
            if dest_inode.kind == EntryKind::File {
                self.discard_stage(*dest_id).await;
                self.inner.read_cache.forget_file(*dest_id);
                self.inner.read_identities.lock().await.remove(dest_id);
                if let Ok(mut inodes) = self.inner.inodes.write() {
                    inodes.remove(*dest_id);
                }
            }
        }

        self.invalidate_dir(from_dirid);
        self.invalidate_dir(to_dirid);
        Ok(())
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        self.readdir_page(dirid, start_after, max_entries).await
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_NOTSUPP)
    }

    async fn readlink(&self, _id: fileid3) -> Result<nfspath3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_NOTSUPP)
    }
}

impl S3NfsFs {
    /// Exclusive fences on a rename's source and target, with both keys
    /// derived from the directories' keys as they stand once the fences are
    /// held: a rename of either directory, or of the source, that finished
    /// during the wait would otherwise send this one to paths that no longer
    /// name anything — or resurrect an old one. Returns the fence, the source
    /// and the target directory's key; `None` when the source is already at
    /// the target.
    async fn fence_rename(
        &self,
        from_dirid: fileid3,
        from_name: &str,
        to_dirid: fileid3,
        to_name: &str,
    ) -> Result<Option<(FenceGuard, fileid3, Inode, String)>, nfsstat3> {
        for _ in 0..FENCED_KEY_ATTEMPTS {
            let from_dir_key = normalize_dir_key(&self.dir_inode(from_dirid)?.key);
            let to_dir_key = normalize_dir_key(&self.dir_inode(to_dirid)?.key);
            let recovered = self
                .recovered_rename_source(from_dirid, &from_dir_key, from_name, &to_dir_key, to_name)
                .await?;
            let (_, source) = match recovered.clone() {
                Some(source) => source,
                None => {
                    self.resolve_child(from_dirid, &from_dir_key, from_name)
                        .await?
                }
            };
            let is_dir = source.kind == EntryKind::Dir;
            let target_key = child_key(&to_dir_key, to_name, is_dir);
            if target_key == source.key {
                return Ok(None);
            }
            let fence = self
                .fence_paths(vec![
                    Self::fence_path_for_inode(&source),
                    if is_dir {
                        FencePath::prefix(normalize_dir_key(&target_key))
                    } else {
                        FencePath::exact(target_key)
                    },
                ])
                .await;
            if normalize_dir_key(&self.dir_inode(from_dirid)?.key) != from_dir_key
                || normalize_dir_key(&self.dir_inode(to_dirid)?.key) != to_dir_key
            {
                continue;
            }
            let (id, current) = match recovered {
                Some(source) => source,
                None => {
                    self.resolve_child_fenced(from_dirid, &from_dir_key, from_name)
                        .await?
                }
            };
            if current.key == source.key {
                return Ok(Some((fence, id, current, to_dir_key)));
            }
        }
        Err(nfsstat3::NFS3ERR_JUKEBOX)
    }

    /// The source of an interrupted rename whose journal is still on disk,
    /// re-interned so that repeating the rename finishes it.
    async fn recovered_rename_source(
        &self,
        from_dirid: fileid3,
        from_dir_key: &str,
        from_name: &str,
        to_dir_key: &str,
        to_name: &str,
    ) -> Result<Option<(fileid3, Inode)>, nfsstat3> {
        for kind in [EntryKind::File, EntryKind::Dir] {
            let from_key = child_key(from_dir_key, from_name, kind == EntryKind::Dir);
            let to_key = child_key(to_dir_key, to_name, kind == EntryKind::Dir);
            if let Ok(bytes) = tokio::fs::read(self.rename_journal_path(&from_key, &to_key)).await {
                let journal: RenameJournal =
                    serde_json::from_slice(&bytes).map_err(|_| nfsstat3::NFS3ERR_IO)?;
                if journal.from != from_key || journal.to != to_key {
                    return Err(nfsstat3::NFS3ERR_IO);
                }
                let size = if kind == EntryKind::Dir {
                    DIR_SIZE
                } else {
                    journal.objects.first().map(|o| o.size).unwrap_or(0)
                };
                let id = self.intern_child(&from_key, from_dirid, kind, size, 0)?;
                return Ok(Some((id, self.inode(id)?)));
            }
        }
        Ok(None)
    }

    /// Moves one object. The same copy-then-delete the app's own rename
    /// performs, issued on this mount's client so it completes inside the
    /// client's retransmission window with no session bookkeeping.
    async fn rename_file(
        &self,
        id: fileid3,
        from_key: &str,
        to_key: &str,
        to_dirid: fileid3,
    ) -> Result<(), nfsstat3> {
        self.flush_stage_blocking(id).await?;
        self.rename_objects(
            from_key,
            to_key,
            vec![(from_key.to_string(), to_key.to_string())],
        )
        .await?;
        self.rekey_stage(id, to_key).await?;
        if let Ok(mut inodes) = self.inner.inodes.write() {
            inodes.rekey(id, to_key, to_dirid);
        }
        self.inner.read_identities.lock().await.remove(&id);
        self.inner.read_cache.forget_file(id);
        let journal_path = self.rename_journal_path(from_key, to_key);
        tokio::fs::remove_file(&journal_path)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        stage::sync_parent(&journal_path)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        self.inner
            .pending_renames
            .write()
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
            .remove(&journal_path);
        Ok(())
    }

    async fn rename_dir(
        &self,
        id: fileid3,
        from_key: &str,
        to_key: &str,
        to_dirid: fileid3,
    ) -> Result<(), nfsstat3> {
        let from_prefix = normalize_dir_key(from_key);
        let to_prefix = normalize_dir_key(to_key);
        if to_prefix.starts_with(&from_prefix) {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        self.flush_stages_under(&from_prefix).await?;
        let keys = self.list_prefix_keys(&from_prefix).await?;
        let moves = keys
            .into_iter()
            .filter_map(|key| {
                rewrite_key(&key, &from_prefix, &to_prefix).map(|new_key| (key, new_key))
            })
            .collect();
        self.rename_objects(&from_prefix, &to_prefix, moves).await?;
        self.rekey_stages_under(&from_prefix, &to_prefix).await?;
        if let Ok(mut inodes) = self.inner.inodes.write() {
            inodes.rekey_prefix(&from_prefix, &to_prefix);
            inodes.rekey(id, &to_prefix, to_dirid);
        }
        self.invalidate_rename_dirs(&from_prefix, &to_prefix)?;
        let moved: Vec<_> = self
            .inner
            .inodes
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
            .by_id
            .iter()
            .filter(|(_, inode)| inode.key.starts_with(&to_prefix))
            .map(|(&id, _)| id)
            .collect();
        let mut identities = self.inner.read_identities.lock().await;
        for id in moved {
            identities.remove(&id);
            self.inner.read_cache.forget_file(id);
        }
        drop(identities);
        let journal_path = self.rename_journal_path(&from_prefix, &to_prefix);
        tokio::fs::remove_file(&journal_path)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        stage::sync_parent(&journal_path)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        self.inner
            .pending_renames
            .write()
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
            .remove(&journal_path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(fileid: fileid3, name: &str) -> DirChild {
        DirChild {
            fileid,
            name: name.to_string(),
        }
    }

    // ---- key normalization ----

    #[test]
    fn root_normalizes_to_the_empty_prefix() {
        assert_eq!(normalize_dir_key(""), "");
        assert_eq!(normalize_dir_key("/"), "");
    }

    #[test]
    fn directory_keys_always_end_with_a_single_slash() {
        assert_eq!(normalize_dir_key("photos"), "photos/");
        assert_eq!(normalize_dir_key("photos/"), "photos/");
        assert_eq!(normalize_dir_key("/photos"), "photos/");
        assert_eq!(normalize_dir_key("a/b/c"), "a/b/c/");
        assert_eq!(normalize_dir_key("a/b/c/"), "a/b/c/");
    }

    #[test]
    fn entry_names_drop_the_parent_path_and_trailing_slash() {
        assert_eq!(entry_name("photos/"), "photos");
        assert_eq!(entry_name("a/b/c/"), "c");
        assert_eq!(entry_name("a/b/note.txt"), "note.txt");
        assert_eq!(entry_name("note.txt"), "note.txt");
        assert_eq!(entry_name(""), "");
        // A key ending in "//" names an entry with an empty name, which cannot
        // be represented over NFS and is filtered out of listings.
        assert_eq!(entry_name("a//"), "");
    }

    #[test]
    fn child_keys_compose_from_the_parent_prefix() {
        assert_eq!(child_key("", "photos", true), "photos/");
        assert_eq!(child_key("", "note.txt", false), "note.txt");
        assert_eq!(child_key("a/b/", "c", true), "a/b/c/");
        assert_eq!(child_key("a/b/", "note.txt", false), "a/b/note.txt");
    }

    #[test]
    fn normalized_dir_key_round_trips_through_child_key() {
        let dir = normalize_dir_key("photos");
        let nested = child_key(&dir, "2024", true);
        assert_eq!(nested, "photos/2024/");
        assert_eq!(normalize_dir_key(&nested), nested);
        assert_eq!(entry_name(&nested), "2024");
    }

    // ---- copy source encoding ----

    #[test]
    fn a_copy_source_keeps_its_path_separators_and_escapes_the_rest() {
        assert_eq!(
            encode_copy_source("photos", "a/b/note.txt"),
            "photos/a/b/note.txt"
        );
        // A trailing slash is a folder marker and must survive as a separator.
        assert_eq!(encode_copy_source("photos", "a/b/"), "photos/a/b/");
    }

    #[test]
    fn a_copy_source_escapes_what_s3_would_otherwise_decode() {
        // Every one of these means something else after S3 URL-decodes it.
        assert_eq!(encode_copy_source("b", "my file.txt"), "b/my%20file.txt");
        assert_eq!(encode_copy_source("b", "a+b.txt"), "b/a%2Bb.txt");
        assert_eq!(encode_copy_source("b", "100%.txt"), "b/100%25.txt");
        assert_eq!(encode_copy_source("b", "a?b=c"), "b/a%3Fb%3Dc");
        // A non-ASCII key is not a legal header value unescaped at all.
        assert_eq!(
            encode_copy_source("b", "照片.jpg"),
            "b/%E7%85%A7%E7%89%87.jpg"
        );
    }

    // ---- rename key rewriting ----

    #[test]
    fn renaming_a_directory_rewrites_only_its_own_subtree() {
        assert_eq!(
            rewrite_key("a/b/note.txt", "a/", "c/").as_deref(),
            Some("c/b/note.txt")
        );
        assert_eq!(rewrite_key("a/", "a/", "c/").as_deref(), Some("c/"));
        assert_eq!(rewrite_key("other/note.txt", "a/", "c/"), None);
        // A shared string prefix is not a shared path prefix.
        assert_eq!(rewrite_key("ab/note.txt", "a/", "c/"), None);
    }

    #[test]
    fn a_file_rename_keeps_its_id_and_leaves_no_stale_key_behind() {
        let mut table = InodeTable::new();
        let dir = table.intern("photos/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0);
        let id = table.intern("note.txt", ROOT_ID, EntryKind::File, 10, 5);

        table.rekey(id, "photos/renamed.txt", dir);

        assert_eq!(table.by_key.get("photos/renamed.txt"), Some(&id));
        assert_eq!(table.by_key.get("note.txt"), None);
        let inode = table.get(id).expect("inode");
        assert_eq!(inode.key, "photos/renamed.txt");
        assert_eq!(inode.parent, dir);
        // The client keeps its file handle across a rename, so the id must not
        // change.
        assert_eq!(
            table.intern("photos/renamed.txt", dir, EntryKind::File, 10, 5),
            id
        );
    }

    #[test]
    fn a_directory_rename_rewrites_every_key_underneath_it() {
        let mut table = InodeTable::new();
        let dir = table.intern("a/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0);
        let sub = table.intern("a/b/", dir, EntryKind::Dir, DIR_SIZE, 0);
        let file = table.intern("a/b/note.txt", sub, EntryKind::File, 3, 0);
        let outside = table.intern("z/note.txt", ROOT_ID, EntryKind::File, 3, 0);

        let moved = table.rekey_prefix("a/", "c/");
        assert_eq!(moved, 3, "the directory and everything under it");

        assert_eq!(table.get(dir).expect("dir").key, "c/");
        assert_eq!(table.get(sub).expect("sub").key, "c/b/");
        assert_eq!(table.get(file).expect("file").key, "c/b/note.txt");
        assert_eq!(table.get(outside).expect("outside").key, "z/note.txt");

        // The reverse map has to agree, or a later listing would mint a second
        // id for a key that already has one.
        assert_eq!(table.by_key.get("c/b/note.txt"), Some(&file));
        assert_eq!(table.by_key.get("a/b/note.txt"), None);
        assert_eq!(table.by_key.get("a/"), None);
        assert_eq!(table.by_key.len(), table.by_id.len());
    }

    #[test]
    fn a_rename_into_a_new_parent_updates_the_reverse_map_consistently() {
        let mut table = InodeTable::new();
        let from = table.intern("from/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0);
        let to = table.intern("to/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0);
        let file = table.intern("from/note.txt", from, EntryKind::File, 1, 0);

        table.rekey(file, "to/note.txt", to);

        assert_eq!(table.by_key.len(), table.by_id.len());
        for (key, id) in &table.by_key {
            assert_eq!(
                &table.get(*id).expect("inode").key,
                key,
                "by_key and by_id disagree about {}",
                key
            );
        }
    }

    // ---- create over an existing name ----

    #[test]
    fn creating_a_new_name_writes_the_placeholder_object() {
        assert_eq!(create_action(false, false), CreateAction::Create);
        assert_eq!(create_action(false, true), CreateAction::Create);
    }

    #[test]
    fn creating_a_name_that_exists_must_not_blank_it() {
        // A client that misses in its own cache sends CREATE for a file that is
        // already there — `touch` on an existing file is exactly this — and the
        // object must come through it untouched.
        assert_eq!(create_action(true, false), CreateAction::OpenExisting);
    }

    #[test]
    fn an_explicit_zero_size_is_still_a_real_truncate() {
        assert_eq!(create_action(true, true), CreateAction::Truncate);
    }

    // ---- rename over an existing name ----

    #[test]
    fn a_rename_onto_a_free_name_goes_ahead() {
        assert_eq!(
            classify_rename_target(false, None, true),
            RenameTarget::Replace
        );
        assert_eq!(
            classify_rename_target(true, None, true),
            RenameTarget::Replace
        );
    }

    #[test]
    fn a_file_may_replace_a_file_and_an_empty_directory_a_directory() {
        assert_eq!(
            classify_rename_target(false, Some(EntryKind::File), true),
            RenameTarget::Replace
        );
        assert_eq!(
            classify_rename_target(true, Some(EntryKind::Dir), true),
            RenameTarget::Replace
        );
    }

    #[test]
    fn a_file_and_a_directory_are_never_interchangeable() {
        // The bucket would hold both "b" and "b/", and a listing renders the
        // directory and hides the object — the moved file would be gone.
        assert_eq!(
            classify_rename_target(false, Some(EntryKind::Dir), true),
            RenameTarget::KindMismatch
        );
        assert_eq!(
            classify_rename_target(true, Some(EntryKind::File), true),
            RenameTarget::KindMismatch
        );
        // Emptiness is irrelevant once the kinds disagree.
        assert_eq!(
            classify_rename_target(false, Some(EntryKind::Dir), false),
            RenameTarget::KindMismatch
        );
    }

    #[test]
    fn a_directory_will_not_silently_merge_into_a_populated_one() {
        assert_eq!(
            classify_rename_target(true, Some(EntryKind::Dir), false),
            RenameTarget::NotEmpty
        );
    }

    // ---- directory emptiness ----

    #[test]
    fn a_directory_holding_only_its_marker_is_empty() {
        assert!(dir_listing_is_empty("a/b/", &["a/b/"], 0));
        assert!(dir_listing_is_empty("a/b/", &[], 0));
    }

    #[test]
    fn a_directory_with_a_file_or_a_subdirectory_is_not_empty() {
        assert!(!dir_listing_is_empty("a/b/", &["a/b/", "a/b/note.txt"], 0));
        assert!(!dir_listing_is_empty("a/b/", &["a/b/note.txt"], 0));
        // A subdirectory shows up as a common prefix, never as a key.
        assert!(!dir_listing_is_empty("a/b/", &["a/b/"], 1));
    }

    // ---- inode table ----

    #[test]
    fn root_is_id_one_and_its_own_parent() {
        let table = InodeTable::new();
        let root = table.get(ROOT_ID).expect("root inode");
        assert_eq!(root.key, "");
        assert_eq!(root.parent, ROOT_ID);
        assert_eq!(root.kind, EntryKind::Dir);
    }

    #[test]
    fn ids_are_stable_across_relisting_and_attrs_refresh() {
        let mut table = InodeTable::new();
        let first = table.intern("a/note.txt", ROOT_ID, EntryKind::File, 10, 100);
        let second = table.intern("a/note.txt", ROOT_ID, EntryKind::File, 42, 200);

        assert_eq!(first, second, "re-listing must not renumber a known key");
        let inode = table.get(first).expect("inode");
        assert_eq!(inode.size, 42);
        assert_eq!(inode.mtime_secs, 200);
    }

    #[test]
    fn a_flushed_upload_refreshes_the_cached_size() {
        let mut table = InodeTable::new();
        let id = table.intern("note.txt", ROOT_ID, EntryKind::File, 0, 0);
        table.set_attrs(id, 4096, 900);

        let inode = table.get(id).expect("inode");
        assert_eq!(inode.size, 4096);
        assert_eq!(inode.mtime_secs, 900);
    }

    #[test]
    fn distinct_keys_get_distinct_monotonic_ids() {
        let mut table = InodeTable::new();
        let a = table.intern("a/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0);
        let b = table.intern("b/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0);
        let c = table.intern("a/x.txt", a, EntryKind::File, 1, 0);

        assert_ne!(a, b);
        assert_ne!(b, c);
        assert!(a > ROOT_ID && b > a && c > b);
        assert_eq!(table.get(c).expect("inode").parent, a);
    }

    #[test]
    fn a_file_and_a_directory_of_the_same_name_are_separate_inodes() {
        let mut table = InodeTable::new();
        let file = table.intern("x", ROOT_ID, EntryKind::File, 3, 0);
        let dir = table.intern("x/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0);
        assert_ne!(file, dir);
    }

    #[test]
    fn parenting_is_updated_when_a_key_is_re_interned() {
        let mut table = InodeTable::new();
        let dir = table.intern("a/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0);
        let id = table.intern("a/x.txt", ROOT_ID, EntryKind::File, 1, 0);
        table.intern("a/x.txt", dir, EntryKind::File, 1, 0);
        assert_eq!(table.get(id).expect("inode").parent, dir);
    }

    // ---- readdir pagination ----

    #[test]
    fn a_zero_cookie_starts_at_the_beginning() {
        let children = vec![child(2, "a"), child(3, "b"), child(4, "c")];
        assert_eq!(resume_index(&children, None), 0);
    }

    #[test]
    fn the_cursor_resumes_after_the_named_entry() {
        let children = vec![child(2, "a"), child(3, "b"), child(4, "c")];
        assert_eq!(resume_index(&children, Some("a")), 1);
        assert_eq!(resume_index(&children, Some("b")), 2);
        assert_eq!(resume_index(&children, Some("c")), 3);
    }

    #[test]
    fn paging_through_a_directory_visits_every_entry_once() {
        let children: Vec<DirChild> = (0..7)
            .map(|i| child(i as fileid3 + 2, &format!("f{}", i)))
            .collect();

        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let start = resume_index(&children, cursor.as_deref());
            let end = (start + 3).min(children.len());
            if start >= end {
                break;
            }
            for entry in &children[start..end] {
                seen.push(entry.name.clone());
            }
            cursor = Some(children[end - 1].name.clone());
            if end >= children.len() {
                break;
            }
        }

        let expected: Vec<String> = children.iter().map(|c| c.name.clone()).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn the_cursor_survives_deletion_of_the_entry_it_points_at() {
        // The client paged up to "b", then "b" disappeared before the next call.
        let after_delete = vec![child(2, "a"), child(4, "c"), child(5, "d")];
        assert_eq!(resume_index(&after_delete, Some("b")), 1);
        assert_eq!(after_delete[1].name, "c");
    }

    #[test]
    fn the_cursor_accounts_for_entries_inserted_before_it() {
        // "aa" was uploaded while the client was paging past "b".
        let after_insert = vec![child(2, "a"), child(6, "aa"), child(3, "b"), child(4, "c")];
        assert_eq!(resume_index(&after_insert, Some("b")), 3);
        assert_eq!(after_insert[3].name, "c");
    }

    #[test]
    fn a_cursor_past_the_last_entry_yields_an_empty_page() {
        let children = vec![child(2, "a"), child(3, "b")];
        let start = resume_index(&children, Some("z"));
        assert_eq!(start, children.len());
        assert!(children[start..].is_empty());

        // An empty page must still be flagged as the end of the directory.
        let (end, is_last) = page_end(children.len(), start, 8);
        assert_eq!(end, start);
        assert!(is_last);
    }

    #[test]
    fn a_page_always_advances_even_when_no_entries_are_requested() {
        let (end, is_last) = page_end(5, 0, 0);
        assert_eq!(end, 1, "a zero-sized page would stall the client");
        assert!(!is_last);
    }

    #[test]
    fn only_the_page_reaching_the_last_entry_is_flagged_as_the_end() {
        assert_eq!(page_end(5, 0, 3), (3, false));
        assert_eq!(page_end(5, 3, 3), (5, true));
        assert_eq!(page_end(5, 0, 99), (5, true));
        assert_eq!(page_end(0, 0, 3), (0, true));
    }

    // ---- read-only gating ----

    fn read_only_fs() -> S3NfsFs {
        S3NfsFs::new(
            test_client(),
            "photos".to_string(),
            true,
            std::env::temp_dir().join("r2-mount-test-readonly"),
        )
    }

    fn writable_fs() -> S3NfsFs {
        S3NfsFs::new(
            test_client(),
            "photos".to_string(),
            false,
            std::env::temp_dir().join("r2-mount-test-writable"),
        )
    }

    /// A client pointed at a port nothing listens on. Every test here is either
    /// rejected before a request is built or is expected to fail as I/O.
    fn test_client() -> Client {
        crate::providers::s3_client::create_s3_client(
            &crate::providers::s3_client::S3ClientConfig {
                access_key_id: "test",
                secret_access_key: "test",
                region: "auto",
                endpoint_url: Some("http://127.0.0.1:1"),
                force_path_style: true,
            },
        )
        .expect("build a test client")
    }

    #[test]
    fn a_read_only_mount_advertises_itself_as_read_only() {
        assert!(matches!(
            read_only_fs().capabilities(),
            VFSCapabilities::ReadOnly
        ));
        assert!(matches!(
            writable_fs().capabilities(),
            VFSCapabilities::ReadWrite
        ));
    }

    #[tokio::test]
    async fn every_mutator_refuses_on_a_read_only_mount() {
        let fs = read_only_fs();
        let name: filename3 = b"note.txt".as_slice().into();
        let attr = sattr3::default();

        // The server checks capabilities too, but each handler does so
        // independently: this is the check that must not be skippable.
        assert!(matches!(
            fs.write(ROOT_ID, 0, b"x").await,
            Err(nfsstat3::NFS3ERR_ROFS)
        ));
        assert!(matches!(
            fs.setattr(ROOT_ID, attr).await,
            Err(nfsstat3::NFS3ERR_ROFS)
        ));
        assert!(matches!(
            fs.create(ROOT_ID, &name, attr).await,
            Err(nfsstat3::NFS3ERR_ROFS)
        ));
        assert!(matches!(
            fs.create_exclusive(ROOT_ID, &name).await,
            Err(nfsstat3::NFS3ERR_ROFS)
        ));
        assert!(matches!(
            fs.mkdir(ROOT_ID, &name).await,
            Err(nfsstat3::NFS3ERR_ROFS)
        ));
        assert!(matches!(
            fs.remove(ROOT_ID, &name).await,
            Err(nfsstat3::NFS3ERR_ROFS)
        ));
        assert!(matches!(
            fs.rename(ROOT_ID, &name, ROOT_ID, &name).await,
            Err(nfsstat3::NFS3ERR_ROFS)
        ));
    }

    #[tokio::test]
    async fn a_writable_mount_gets_past_the_read_only_gate() {
        let fs = writable_fs();
        let name: filename3 = b"note.txt".as_slice().into();

        // Nothing is listening on the endpoint, so these still fail — but as
        // I/O, not as ROFS, which is what proves the gate is open.
        assert!(!matches!(
            fs.create(ROOT_ID, &name, sattr3::default()).await,
            Err(nfsstat3::NFS3ERR_ROFS)
        ));
        assert!(!matches!(
            fs.mkdir(ROOT_ID, &name).await,
            Err(nfsstat3::NFS3ERR_ROFS)
        ));
    }

    #[tokio::test]
    async fn a_name_that_would_escape_its_directory_is_refused() {
        let fs = writable_fs();
        for raw in [
            b"a/b".as_slice(),
            b"..".as_slice(),
            b".".as_slice(),
            b"".as_slice(),
        ] {
            let name: filename3 = raw.into();
            assert!(
                matches!(
                    fs.create(ROOT_ID, &name, sattr3::default()).await,
                    Err(nfsstat3::NFS3ERR_INVAL)
                ),
                "{:?} must not compose a key",
                raw
            );
        }
    }

    #[tokio::test]
    async fn writing_to_a_directory_is_not_a_write_at_all() {
        let fs = writable_fs();
        assert!(matches!(
            fs.write(ROOT_ID, 0, b"x").await,
            Err(nfsstat3::NFS3ERR_INVAL)
        ));
    }

    // ---- stage lifecycle ----

    /// A writable filesystem with a staging directory of its own. Staging a
    /// file that has no content to preserve never touches the network, so the
    /// unreachable client below is never used.
    fn staging_fs(name: &str) -> S3NfsFs {
        let root = std::env::temp_dir().join(format!(
            "r2-mount-fs-{}-{}-{}",
            std::process::id(),
            name,
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        S3NfsFs::new(test_client(), "photos".to_string(), false, root)
    }

    /// Interns a file and returns its id and inode.
    fn staged_file(fs: &S3NfsFs, key: &str) -> (fileid3, Inode) {
        let id = fs
            .intern_child(key, ROOT_ID, EntryKind::File, 0, 0)
            .expect("intern");
        let inode = fs.inode(id).expect("inode");
        (id, inode)
    }

    #[tokio::test]
    async fn an_evicted_stage_leaves_neither_a_registry_entry_nor_a_file() {
        let fs = staging_fs("evict");
        let (id, inode) = staged_file(&fs, "note.txt");

        let path = {
            let mut guard = fs.reset_stage(id, &inode).await.expect("stage");
            guard.write_at(0, b"hello").await.expect("write");
            guard.path().to_path_buf()
        };
        assert!(path.exists());

        fs.evict_stage(id).await;

        assert!(!path.exists(), "the staging file must be unlinked");
        assert!(fs.stage_guard(id).await.is_none());
        assert!(fs.inner.stages.lock().await.is_empty());

        let _ = std::fs::remove_dir_all(fs.staging_root());
    }

    #[tokio::test]
    async fn a_stage_with_unsent_content_is_never_evicted() {
        let fs = staging_fs("evict-dirty");
        let (id, inode) = staged_file(&fs, "note.txt");

        {
            let mut guard = fs.reset_stage(id, &inode).await.expect("stage");
            guard.write_at(0, b"unsent").await.expect("write");
            guard.mark_dirty(1);
        }

        fs.evict_stage(id).await;

        let guard = fs.stage_guard(id).await.expect("the stage must survive");
        assert!(guard.dirty);
        assert!(guard.path().exists(), "the only copy must still be on disk");
        drop(guard);

        let _ = std::fs::remove_dir_all(fs.staging_root());
    }

    #[tokio::test]
    async fn a_handle_held_across_an_eviction_is_marked_stale() {
        // The race the tombstone exists for: a task resolves a stage, and the
        // stage is dropped before that task takes the lock. Without the
        // tombstone it would go on writing into a file that is already gone.
        let fs = staging_fs("tombstone");
        let (id, inode) = staged_file(&fs, "note.txt");
        drop(fs.reset_stage(id, &inode).await.expect("stage"));

        let stale = fs
            .inner
            .stages
            .lock()
            .await
            .get(&id)
            .cloned()
            .expect("handle");

        fs.evict_stage(id).await;

        assert!(stale.lock().await.evicted);
        // And the lookup that follows finds nothing rather than the dead
        // handle, which is what keeps the restart from spinning.
        assert!(fs.stage_guard(id).await.is_none());

        let _ = std::fs::remove_dir_all(fs.staging_root());
    }

    #[tokio::test]
    async fn a_write_after_an_eviction_gets_a_fresh_staging_file() {
        let fs = staging_fs("restart");
        let (id, inode) = staged_file(&fs, "note.txt");
        drop(fs.reset_stage(id, &inode).await.expect("stage"));
        fs.evict_stage(id).await;

        let mut guard = fs.reset_stage(id, &inode).await.expect("fresh stage");
        assert!(!guard.evicted);
        guard.write_at(0, b"again").await.expect("write");
        assert!(guard.path().exists());
        assert_eq!(guard.read_at(0, 5).await.expect("read"), b"again");
        drop(guard);

        let _ = std::fs::remove_dir_all(fs.staging_root());
    }

    #[tokio::test]
    async fn discarding_a_stage_tombstones_it_even_when_it_is_dirty() {
        // Deleting the file the stage belongs to is an explicit instruction to
        // throw the unsent content away, unlike eviction.
        let fs = staging_fs("discard");
        let (id, inode) = staged_file(&fs, "note.txt");

        let (path, handle) = {
            let mut guard = fs.reset_stage(id, &inode).await.expect("stage");
            guard.write_at(0, b"doomed").await.expect("write");
            guard.mark_dirty(1);
            let path = guard.path().to_path_buf();
            drop(guard);
            let handle = fs
                .inner
                .stages
                .lock()
                .await
                .get(&id)
                .cloned()
                .expect("handle");
            (path, handle)
        };

        fs.discard_stage(id).await;

        assert!(handle.lock().await.evicted);
        assert!(!path.exists());
        assert!(fs.stage_guard(id).await.is_none());

        let _ = std::fs::remove_dir_all(fs.staging_root());
    }

    #[tokio::test]
    async fn a_directory_listing_reports_staged_sizes() {
        // Without this a file mid-copy lists as zero bytes, and the client
        // caches that answer for the whole actimeo window.
        let fs = staging_fs("readdir-attrs");
        let (id, inode) = staged_file(&fs, "note.txt");

        assert!(fs.try_staged_attr(id, &inode).await.is_none());

        {
            let mut guard = fs.reset_stage(id, &inode).await.expect("stage");
            guard.write_at(0, b"0123456789").await.expect("write");
            guard.mark_dirty(4242);
        }

        let attr = fs.try_staged_attr(id, &inode).await.expect("staged attrs");
        assert_eq!(attr.size, 10);
        assert_eq!(attr.mtime.seconds, 4242);
        // The inode still carries what S3 last reported, which is the point.
        assert_eq!(fs.attr_of(id, &inode).size, 0);

        let _ = std::fs::remove_dir_all(fs.staging_root());
    }

    #[tokio::test]
    async fn a_busy_stage_falls_back_to_the_object_attributes() {
        let fs = staging_fs("readdir-busy");
        let (id, inode) = staged_file(&fs, "note.txt");
        let held = fs.reset_stage(id, &inode).await.expect("stage");

        // Held elsewhere: a listing must not wait for a multi-gigabyte download
        // to finish before it can answer.
        assert!(fs.try_staged_attr(id, &inode).await.is_none());
        drop(held);
        assert!(fs.try_staged_attr(id, &inode).await.is_some());

        let _ = std::fs::remove_dir_all(fs.staging_root());
    }

    // ---- error mapping ----

    #[test]
    fn s3_failures_map_onto_the_matching_nfs_status() {
        assert!(matches!(
            status_for_s3_code(Some("AccessDenied")),
            nfsstat3::NFS3ERR_ACCES
        ));
        assert!(matches!(
            status_for_s3_code(Some("SignatureDoesNotMatch")),
            nfsstat3::NFS3ERR_ACCES
        ));
        assert!(matches!(
            status_for_s3_code(Some("NoSuchKey")),
            nfsstat3::NFS3ERR_NOENT
        ));
        assert!(matches!(
            status_for_s3_code(Some("NotFound")),
            nfsstat3::NFS3ERR_NOENT
        ));
        assert!(matches!(
            status_for_s3_code(Some("SlowDown")),
            nfsstat3::NFS3ERR_IO
        ));
        // A failure that never reached the service carries no code at all.
        assert!(matches!(status_for_s3_code(None), nfsstat3::NFS3ERR_IO));
    }
}
