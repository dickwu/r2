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
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::primitives::{ByteStream, Length};
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client;

use crate::providers::s3_client::describe_s3_error;
use futures_util::{stream, StreamExt};
use nfsserve::nfs::{
    fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfstime3, sattr3, set_atime, set_mtime,
    set_size3, specdata3,
};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use serde::Serialize;
use tauri::Emitter;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, Semaphore};
use tokio::task::JoinHandle;

use super::progress::{MountProgress, TransferKind, TransferTracker};
use super::read_cache::{self, ReadCache};
use super::stage::{self, FlushState, Stage, StageInit};

/// Reserved root id. `0` is reserved by the protocol and must never be used.
const ROOT_ID: fileid3 = 1;
/// How long a directory listing is served before it is re-fetched from S3.
const DIR_CACHE_TTL: Duration = Duration::from_secs(30);
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

struct DirListing {
    children: Arc<Vec<DirChild>>,
    fetched_at: Instant,
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
    path: PathBuf,
    size: u64,
    generation: u64,
}

// ============ Filesystem ============

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
}

impl S3NfsFs {
    pub fn new(client: Client, bucket: String, read_only: bool, staging_root: PathBuf) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bucket.hash(&mut hasher);
        let fsid = hasher.finish();

        Self {
            inner: Arc::new(FsInner {
                client,
                bucket,
                read_only,
                staging_root,
                inodes: RwLock::new(InodeTable::new()),
                dirs: RwLock::new(HashMap::new()),
                stages: AsyncMutex::new(HashMap::new()),
                flush_slots: Arc::new(Semaphore::new(stage::MAX_CONCURRENT_FLUSHES)),
                read_cache: ReadCache::new(),
                prefetch_slots: Arc::new(Semaphore::new(read_cache::PREFETCH_CONCURRENCY)),
                progress: OnceLock::new(),
                uid: current_uid(),
                gid: current_gid(),
                fsid,
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

    fn progress(&self) -> Option<&MountProgress> {
        self.inner.progress.get()
    }

    pub fn staging_root(&self) -> &Path {
        &self.inner.staging_root
    }

    /// Rejects a mutation on a read-only mount.
    ///
    /// `capabilities()` already tells the server we are read-only, but each
    /// handler checks that independently, so every mutator repeats the check:
    /// a mistake there must not turn into a write to the user's bucket.
    fn ensure_writable(&self) -> Result<(), nfsstat3> {
        if self.inner.read_only {
            Err(nfsstat3::NFS3ERR_ROFS)
        } else {
            Ok(())
        }
    }

    fn inode(&self, id: fileid3) -> Result<Inode, nfsstat3> {
        self.inner
            .inodes
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?
            .get(id)
            .cloned()
            .ok_or(nfsstat3::NFS3ERR_NOENT)
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
        if let Ok(mut dirs) = self.inner.dirs.write() {
            dirs.remove(&dirid);
        }
    }

    /// Drops every cached listing. A directory rename moves an unbounded set of
    /// paths, and re-listing costs one request per directory the user actually
    /// looks at.
    fn invalidate_all_dirs(&self) {
        if let Ok(mut dirs) = self.inner.dirs.write() {
            dirs.clear();
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

    /// Cached children of `dirid`, re-listing from S3 once the entry is older
    /// than [`DIR_CACHE_TTL`].
    async fn children_of(
        &self,
        dirid: fileid3,
        dir_key: &str,
    ) -> Result<Arc<Vec<DirChild>>, nfsstat3> {
        if let Ok(dirs) = self.inner.dirs.read() {
            if let Some(listing) = dirs.get(&dirid) {
                if listing.fetched_at.elapsed() < DIR_CACHE_TTL {
                    return Ok(listing.children.clone());
                }
            }
        }

        self.list_dir(dirid, dir_key).await
    }

    /// Lists one directory level and registers an inode for every child.
    async fn list_dir(
        &self,
        dirid: fileid3,
        dir_key: &str,
    ) -> Result<Arc<Vec<DirChild>>, nfsstat3> {
        // BTreeMap gives the deterministic, name-sorted ordering readdir needs.
        let mut entries: BTreeMap<String, (EntryKind, u64, u32)> = BTreeMap::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut request = self
                .inner
                .client
                .list_objects_v2()
                .bucket(&self.inner.bucket)
                .delimiter("/")
                .max_keys(LIST_PAGE_SIZE);

            if !dir_key.is_empty() {
                request = request.prefix(dir_key);
            }
            if let Some(token) = &continuation_token {
                request = request.continuation_token(token);
            }

            let response = request.send().await.map_err(|e| {
                log::error!("mount: failed to list \"{}\": {}", dir_key, e);
                map_s3_error(&e)
            })?;

            for prefix in response.common_prefixes() {
                let Some(prefix) = prefix.prefix() else {
                    continue;
                };
                let name = entry_name(prefix);
                if name.is_empty() {
                    continue;
                }
                // A directory always wins over an object with the same name so
                // the subtree underneath it stays reachable.
                entries.insert(name.to_string(), (EntryKind::Dir, DIR_SIZE, 0));
            }

            for object in response.contents() {
                let Some(key) = object.key() else {
                    continue;
                };
                // The prefix itself and any other explicit folder marker are
                // already represented as directories.
                if key == dir_key || key.ends_with('/') {
                    continue;
                }
                let name = entry_name(key);
                if name.is_empty() {
                    continue;
                }
                let size = object.size().unwrap_or(0).max(0) as u64;
                let mtime = object
                    .last_modified()
                    .map(|time| time.secs())
                    .unwrap_or_default();
                let mtime = u32::try_from(mtime.max(0)).unwrap_or(u32::MAX);
                entries
                    .entry(name.to_string())
                    .or_insert((EntryKind::File, size, mtime));
            }

            if !response.is_truncated().unwrap_or(false) {
                break;
            }
            continuation_token = response.next_continuation_token().map(str::to_string);
            if continuation_token.is_none() {
                break;
            }
        }

        let children = {
            let mut inodes = self
                .inner
                .inodes
                .write()
                .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
            entries
                .into_iter()
                .map(|(name, (kind, size, mtime))| {
                    let key = child_key(dir_key, &name, kind == EntryKind::Dir);
                    let fileid = inodes.intern(&key, dirid, kind, size, mtime);
                    DirChild { fileid, name }
                })
                .collect::<Vec<_>>()
        };

        let children = Arc::new(children);
        if let Ok(mut dirs) = self.inner.dirs.write() {
            dirs.insert(
                dirid,
                DirListing {
                    children: children.clone(),
                    fetched_at: Instant::now(),
                },
            );
        }

        Ok(children)
    }

    /// Resolves a name the cached listing does not contain by asking S3
    /// directly, which covers objects written after the listing was taken.
    async fn lookup_uncached(
        &self,
        dirid: fileid3,
        dir_key: &str,
        name: &str,
    ) -> Result<fileid3, nfsstat3> {
        let key = child_key(dir_key, name, false);
        let head = self
            .inner
            .client
            .head_object()
            .bucket(&self.inner.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|_| nfsstat3::NFS3ERR_NOENT)?;

        let size = head.content_length().unwrap_or(0).max(0) as u64;
        let mtime = head
            .last_modified()
            .map(|time| time.secs())
            .unwrap_or_default();
        let mtime = u32::try_from(mtime.max(0)).unwrap_or(u32::MAX);

        let mut inodes = self
            .inner
            .inodes
            .write()
            .map_err(|_| nfsstat3::NFS3ERR_SERVERFAULT)?;
        Ok(inodes.intern(&key, dirid, EntryKind::File, size, mtime))
    }

    /// The child of `dirid` named `name`, resolved through the cached listing so
    /// its kind is known, or `None` when the directory has no such entry.
    async fn lookup_child(
        &self,
        dirid: fileid3,
        dir_key: &str,
        name: &str,
    ) -> Result<Option<(fileid3, Inode)>, nfsstat3> {
        let children = self.children_of(dirid, dir_key).await?;
        let Some(child) = children.iter().find(|child| child.name == name) else {
            return Ok(None);
        };
        Ok(Some((child.fileid, self.inode(child.fileid)?)))
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

    /// Size and modification time of an object, or `None` when it is not there.
    async fn head_object(&self, key: &str) -> Option<(u64, u32)> {
        let head = self
            .inner
            .client
            .head_object()
            .bucket(&self.inner.bucket)
            .key(key)
            .send()
            .await
            .ok()?;

        let size = head.content_length().unwrap_or(0).max(0) as u64;
        let mtime = head
            .last_modified()
            .map(|time| time.secs())
            .unwrap_or_default();
        Some((size, u32::try_from(mtime.max(0)).unwrap_or(u32::MAX)))
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
        object_size: u64,
    ) -> Result<Arc<Vec<u8>>, nfsstat3> {
        let slot = self.inner.read_cache.slot(id, index);
        let result = slot
            .cell
            .get_or_try_init(|| async {
                let start = read_cache::chunk_start(index);
                let len = read_cache::chunk_len(index, object_size);
                let range = format!("bytes={}-{}", start, start + len.max(1) - 1);

                let response = self
                    .inner
                    .client
                    .get_object()
                    .bucket(&self.inner.bucket)
                    .key(key)
                    .range(range)
                    .send()
                    .await
                    .map_err(|e| {
                        log::error!("mount: failed to read \"{}\": {}", key, e);
                        map_s3_error(&e)
                    })?;

                let bytes = response
                    .body
                    .collect()
                    .await
                    .map_err(|e| {
                        log::error!("mount: failed to buffer \"{}\": {}", key, e);
                        nfsstat3::NFS3ERR_IO
                    })?
                    .into_bytes()
                    .to_vec();

                let bytes = Arc::new(bytes);
                self.inner
                    .read_cache
                    .note_filled(id, index, &slot, bytes.len() as u64);
                Ok(bytes)
            })
            .await;

        match result {
            Ok(bytes) => Ok(bytes.clone()),
            Err(status) => {
                self.inner.read_cache.remove_slot(id, index);
                Err(status)
            }
        }
    }

    /// Warms the chunks after `served` in the background, so a sequential
    /// reader finds the next chunk already arriving. Cheap for everyone else:
    /// a chunk that exists is skipped, and when the prefetch slots are busy
    /// nothing is queued.
    fn prefetch_after(&self, id: fileid3, key: &str, served: u64, object_size: u64) {
        for step in 1..=read_cache::PREFETCH_CHUNKS {
            let index = served + step;
            if read_cache::chunk_len(index, object_size) == 0 {
                return;
            }
            if self.inner.read_cache.is_present(id, index) {
                continue;
            }
            let Ok(permit) = self.inner.prefetch_slots.clone().try_acquire_owned() else {
                return;
            };
            let fs = self.clone();
            let key = key.to_string();
            tokio::spawn(async move {
                let _permit = permit;
                let _ = fs.chunk_bytes(id, &key, index, object_size).await;
            });
        }
    }

    // ---- S3 mutations ----

    /// Writes a zero-byte object, used for both `create` and the folder markers
    /// `mkdir` leaves behind.
    async fn put_empty_object(&self, key: &str) -> Result<(), nfsstat3> {
        self.inner
            .client
            .put_object()
            .bucket(&self.inner.bucket)
            .key(key)
            .body(ByteStream::from_static(b""))
            .send()
            .await
            .map_err(|e| {
                log::error!("mount: failed to create \"{}\": {}", key, e);
                map_s3_error(&e)
            })?;
        Ok(())
    }

    async fn delete_object(&self, key: &str) -> Result<(), nfsstat3> {
        self.inner
            .client
            .delete_object()
            .bucket(&self.inner.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                log::error!("mount: failed to delete \"{}\": {}", key, e);
                map_s3_error(&e)
            })?;
        Ok(())
    }

    /// Server-side copy. No bytes travel through this machine, which is what
    /// makes a rename inside a mount as cheap as the app's own move.
    async fn copy_object(&self, from_key: &str, to_key: &str) -> Result<(), nfsstat3> {
        self.inner
            .client
            .copy_object()
            .bucket(&self.inner.bucket)
            .copy_source(encode_copy_source(&self.inner.bucket, from_key))
            .key(to_key)
            .send()
            .await
            .map_err(|e| {
                // CopyObject caps out at 5 GB; past that the app's Move is the
                // only path, since it can fall back to streaming.
                log::error!(
                    "mount: failed to copy \"{}\" to \"{}\": {}. Objects over 5 GB have to be moved with the app's Move.",
                    from_key,
                    to_key,
                    e
                );
                map_s3_error(&e)
            })?;
        Ok(())
    }

    /// Whether the directory holds nothing but its own folder marker.
    async fn dir_is_empty(&self, dir_key: &str) -> Result<bool, nfsstat3> {
        let response = self
            .inner
            .client
            .list_objects_v2()
            .bucket(&self.inner.bucket)
            .prefix(dir_key)
            .delimiter("/")
            .max_keys(2)
            .send()
            .await
            .map_err(|e| {
                log::error!("mount: failed to list \"{}\": {}", dir_key, e);
                map_s3_error(&e)
            })?;

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

            let response = request.send().await.map_err(|e| {
                log::error!("mount: failed to list \"{}\": {}", prefix, e);
                map_s3_error(&e)
            })?;

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
            continuation_token = response.next_continuation_token().map(str::to_string);
            if continuation_token.is_none() {
                break;
            }
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
            if let Some(existing) = stages.get(&id).cloned() {
                drop(stages);
                let guard = existing.lock_owned().await;
                if guard.evicted {
                    // Dropped while we waited for it; the map has the truth.
                    continue;
                }
                return Ok(guard);
            }

            let path = self.inner.staging_root.join(id.to_string());
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
        let _ = tokio::fs::remove_file(guard.path()).await;
        stages.remove(&id);
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
        if let Some(mut guard) = self.stage_guard(id).await {
            guard.truncate(0).await.map_err(|e| {
                log::error!(
                    "mount: failed to truncate the stage for \"{}\": {}",
                    inode.key,
                    e
                );
                nfsstat3::NFS3ERR_IO
            })?;
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
        match stage::stage_init(inode.size) {
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
                let tracker = self
                    .progress()
                    .map(|p| p.track(id, &inode.key, TransferKind::Download, inode.size));
                let outcome = self
                    .download_into_stage(stage, inode, tracker.as_ref())
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
        tracker: Option<&TransferTracker>,
    ) -> Result<(), nfsstat3> {
        let response = self
            .inner
            .client
            .get_object()
            .bucket(&self.inner.bucket)
            .key(&inode.key)
            .send()
            .await
            .map_err(|e| {
                log::error!("mount: failed to stage \"{}\": {}", inode.key, e);
                map_s3_error(&e)
            })?;

        let mut body = response.body;
        let mut offset = 0u64;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| {
                log::error!("mount: failed to stage \"{}\": {}", inode.key, e);
                nfsstat3::NFS3ERR_IO
            })?;
            stage.write_at(offset, &chunk).await.map_err(|e| {
                log::error!("mount: failed to stage \"{}\": {}", inode.key, e);
                nfsstat3::NFS3ERR_IO
            })?;
            offset = offset.saturating_add(chunk.len() as u64);
            if let Some(tracker) = tracker {
                tracker.set(offset);
            }
        }
        Ok(())
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
        let _ = tokio::fs::remove_file(guard.path()).await;
        stages.remove(&id);
    }

    // ---- Uploads ----

    /// Uploads one staged file, reporting it as a live transfer from first
    /// attempt to final outcome.
    async fn upload_with_retries(
        &self,
        id: fileid3,
        key: &str,
        path: &Path,
        size: u64,
    ) -> Result<(), String> {
        self.upload_with_attempts(id, key, path, size, stage::UPLOAD_ATTEMPTS)
            .await
    }

    async fn upload_with_attempts(
        &self,
        id: fileid3,
        key: &str,
        path: &Path,
        size: u64,
        attempts: u32,
    ) -> Result<(), String> {
        let tracker = self
            .progress()
            .map(|p| p.track(id, key, TransferKind::Upload, size));

        let attempts = attempts.max(1);
        let mut last_error = String::new();
        for attempt in 1..=attempts {
            if attempt > 1 {
                if let Some(tracker) = &tracker {
                    tracker.restart();
                }
            }
            match self
                .upload_stage_file(key, path, size, tracker.as_ref())
                .await
            {
                Ok(()) => {
                    // The bucket now holds different bytes than any cached
                    // chunk of this file.
                    self.inner.read_cache.forget_file(id);
                    if let Some(tracker) = &tracker {
                        tracker.done();
                    }
                    return Ok(());
                }
                Err(error) => {
                    last_error = error;
                    if attempt < attempts {
                        tokio::time::sleep(stage::UPLOAD_RETRY_BACKOFF * attempt).await;
                    }
                }
            }
        }
        if let Some(tracker) = &tracker {
            tracker.failed(&last_error);
        }
        Err(last_error)
    }

    /// Uploads the staging file, streaming from disk so a large file never
    /// passes through memory.
    ///
    /// A file under the multipart threshold goes up as a single `PutObject`,
    /// which offers no mid-flight byte count — its transfer row jumps from
    /// started to done. Multipart uploads report per finished part.
    async fn upload_stage_file(
        &self,
        key: &str,
        path: &Path,
        size: u64,
        tracker: Option<&TransferTracker>,
    ) -> Result<(), String> {
        if size <= stage::MULTIPART_THRESHOLD {
            let body = ByteStream::from_path(path)
                .await
                .map_err(|e| format!("Failed to read the staged file: {}", e))?;
            self.inner
                .client
                .put_object()
                .bucket(&self.inner.bucket)
                .key(key)
                .body(body)
                .send()
                .await
                .map_err(|e| describe_s3_error(&e))?;
            return Ok(());
        }

        self.upload_stage_multipart(key, path, size, tracker).await
    }

    async fn upload_stage_multipart(
        &self,
        key: &str,
        path: &Path,
        size: u64,
        tracker: Option<&TransferTracker>,
    ) -> Result<(), String> {
        let created = self
            .inner
            .client
            .create_multipart_upload()
            .bucket(&self.inner.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| describe_s3_error(&e))?;
        let upload_id = created
            .upload_id()
            .ok_or_else(|| "The storage provider did not return an upload id".to_string())?
            .to_string();

        // `buffered` preserves order, so the parts come back numbered the way
        // S3 needs them without a sort.
        let results: Vec<Result<CompletedPart, String>> =
            stream::iter((0..stage::part_count(size)).map(|index| {
                let upload_id = upload_id.as_str();
                async move {
                    let (offset, length) = stage::part_range(size, index);
                    let part_number = i32::try_from(index + 1)
                        .map_err(|_| "Staged file needs too many parts".to_string())?;

                    let body = ByteStream::read_from()
                        .path(path)
                        .offset(offset)
                        .length(Length::Exact(length))
                        .build()
                        .await
                        .map_err(|e| format!("Failed to read the staged file: {}", e))?;

                    let response = self
                        .inner
                        .client
                        .upload_part()
                        .bucket(&self.inner.bucket)
                        .key(key)
                        .upload_id(upload_id)
                        .part_number(part_number)
                        .body(body)
                        .send()
                        .await
                        .map_err(|e| describe_s3_error(&e))?;

                    if let Some(tracker) = tracker {
                        tracker.add(length);
                    }

                    Ok(CompletedPart::builder()
                        .part_number(part_number)
                        .set_e_tag(response.e_tag().map(str::to_string))
                        .build())
                }
            }))
            .buffered(stage::PART_CONCURRENCY)
            .collect()
            .await;

        let mut parts = Vec::with_capacity(results.len());
        for result in results {
            match result {
                Ok(part) => parts.push(part),
                Err(error) => {
                    // Unfinished multipart uploads are billed, so the abort is
                    // worth attempting even though it may fail too.
                    let _ = self
                        .inner
                        .client
                        .abort_multipart_upload()
                        .bucket(&self.inner.bucket)
                        .key(key)
                        .upload_id(&upload_id)
                        .send()
                        .await;
                    return Err(error);
                }
            }
        }

        self.inner
            .client
            .complete_multipart_upload()
            .bucket(&self.inner.bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            )
            .send()
            .await
            .map_err(|e| describe_s3_error(&e))?;

        Ok(())
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
        let candidates: Vec<(fileid3, Arc<AsyncMutex<Stage>>)> = {
            let stages = self.inner.stages.lock().await;
            stages
                .iter()
                .map(|(&id, handle)| (id, handle.clone()))
                .collect()
        };

        let now = Instant::now();
        for (id, handle) in candidates {
            let Ok(permit) = self.inner.flush_slots.clone().try_acquire_owned() else {
                // At the concurrency limit; the next tick picks the rest up.
                return;
            };
            // A locked stage is mid-download or mid-write, so it is not due yet
            // and is not worth stalling the scan for.
            let Ok(mut guard) = handle.clone().try_lock_owned() else {
                continue;
            };
            if guard.evicted || !guard.is_due(now) {
                continue;
            }

            guard.state = FlushState::Uploading;
            let job = FlushJob {
                id,
                key: guard.key.clone(),
                path: guard.path().to_path_buf(),
                size: guard.size,
                generation: guard.dirty_gen,
            };
            drop(guard);

            let fs = self.clone();
            let app = app.clone();
            let mount_id = mount_id.to_string();
            tokio::spawn(async move {
                let _permit = permit;
                fs.run_flush(job, handle, &app, &mount_id).await;
            });
        }
    }

    /// Uploads one stage and settles its state.
    ///
    /// The stage lock is deliberately not held across the upload: a client that
    /// keeps writing must not block on it. A write that lands meanwhile bumps
    /// the generation, which is how the upload knows what it published is
    /// already stale.
    async fn run_flush(
        &self,
        job: FlushJob,
        handle: Arc<AsyncMutex<Stage>>,
        app: &tauri::AppHandle,
        mount_id: &str,
    ) {
        let outcome = self
            .upload_with_retries(job.id, &job.key, &job.path, job.size)
            .await;

        let mut guard = handle.lock_owned().await;
        if guard.evicted {
            // The file was deleted or moved while the upload was in flight;
            // there is no longer a stage for this to settle.
            return;
        }
        match outcome {
            Ok(()) => {
                guard.state = FlushState::Idle;
                if !stage::upload_settles_stage(job.generation, guard.dirty_gen) {
                    // More was written mid-upload; the next tick re-uploads it.
                    // The tracker already announced "done", so put the row
                    // back to queued or it would read finished while dirty.
                    if let Some(progress) = self.progress() {
                        progress.waiting(job.id, &guard.key, guard.size);
                    }
                    return;
                }
                guard.dirty = false;
                guard.flush_requested = false;
                self.update_inode_attrs(job.id, job.size, guard.mtime_secs);
                drop(guard);
                self.evict_stage(job.id).await;
            }
            Err(error) => {
                log::error!("mount: failed to upload \"{}\": {}", job.key, error);
                guard.state = FlushState::Failed {
                    retry_after: Instant::now() + stage::FLUSH_RETRY_COOLDOWN,
                };
                drop(guard);

                let _ = app.emit(
                    "mount-flush-error",
                    FlushErrorPayload {
                        mount_id: mount_id.to_string(),
                        bucket: self.inner.bucket.clone(),
                        key: job.key,
                        error,
                    },
                );
            }
        }
    }

    /// Uploads a file's staged content right now, holding its lock for the whole
    /// upload so nothing can be written between the upload and whatever the
    /// caller does next with the object.
    async fn flush_stage_blocking(&self, id: fileid3) -> Result<(), nfsstat3> {
        let Some(mut guard) = self.stage_guard(id).await else {
            return Ok(());
        };
        if !guard.dirty {
            return Ok(());
        }

        let key = guard.key.clone();
        let path = guard.path().to_path_buf();
        let size = guard.size;
        self.upload_with_retries(id, &key, &path, size)
            .await
            .map_err(|error| {
                log::error!("mount: failed to upload \"{}\": {}", key, error);
                nfsstat3::NFS3ERR_IO
            })?;

        guard.dirty = false;
        guard.flush_requested = false;
        guard.state = FlushState::Idle;
        self.update_inode_attrs(id, size, guard.mtime_secs);
        Ok(())
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
    async fn rekey_stage(&self, id: fileid3, new_key: &str) {
        let Some(mut guard) = self.stage_guard(id).await else {
            return;
        };
        guard.key = new_key.to_string();
    }

    /// Re-keys every stage under a directory that has just been renamed.
    async fn rekey_stages_under(&self, from_prefix: &str, to_prefix: &str) {
        for id in self.stages_under(from_prefix).await {
            let Some(mut guard) = self.stage_guard(id).await else {
                continue;
            };
            if let Some(new_key) = rewrite_key(&guard.key, from_prefix, to_prefix) {
                guard.key = new_key;
            }
        }
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
    /// Unlike the scanner this holds each stage's lock across its upload, so no
    /// write can slip in: it runs when the mount is going away and nothing more
    /// should be accepted.
    async fn flush_everything(&self, attempts: u32) -> usize {
        let staged: Vec<(fileid3, Arc<AsyncMutex<Stage>>)> = {
            let stages = self.inner.stages.lock().await;
            stages
                .iter()
                .map(|(&id, handle)| (id, handle.clone()))
                .collect()
        };

        let pending: Vec<usize> = stream::iter(staged.into_iter().map(|(id, handle)| async move {
            let mut guard = handle.lock_owned().await;
            if guard.evicted || !guard.dirty {
                return 0;
            }
            if guard.state == FlushState::Uploading {
                // The background flusher already has this one; counting it as
                // pending makes the next round wait for it rather than upload
                // the same bytes twice.
                return 1;
            }

            let key = guard.key.clone();
            let path = guard.path().to_path_buf();
            let size = guard.size;
            match self.upload_with_attempts(id, &key, &path, size, attempts).await {
                Ok(()) => {
                    guard.dirty = false;
                    guard.state = FlushState::Idle;
                    self.update_inode_attrs(id, size, guard.mtime_secs);
                    0
                }
                Err(error) => {
                    log::error!(
                        "mount: failed to upload \"{}\" while unmounting: {}. The staged copy is kept at {}",
                        key,
                        error,
                        path.display()
                    );
                    guard.state = FlushState::Failed {
                        retry_after: Instant::now(),
                    };
                    1
                }
            }
        }))
        .buffer_unordered(stage::MAX_CONCURRENT_FLUSHES)
        .collect()
        .await;

        pending.into_iter().sum()
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
        let children = self.children_of(dirid, &dir_key).await?;
        if let Some(child) = children.iter().find(|child| child.name == name) {
            return Ok(child.fileid);
        }

        self.lookup_uncached(dirid, &dir_key, name).await
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
        let inode = self.inode(id)?;

        // Directories have no content to resize, and mode or owner changes have
        // nowhere to go in object storage, so they are accepted and reported
        // back as the canned attributes.
        if inode.kind != EntryKind::File {
            return Ok(self.attr_of(id, &inode));
        }

        let times_touched = !matches!(setattr.mtime, set_mtime::DONT_CHANGE)
            || !matches!(setattr.atime, set_atime::DONT_CHANGE);

        if let set_size3::size(size) = setattr.size {
            let mut guard = if size == 0 {
                self.reset_stage(id, &inode).await?
            } else {
                self.ensure_stage(id, &inode).await?
            };
            guard.truncate(size).await.map_err(|e| {
                log::error!("mount: failed to resize \"{}\": {}", inode.key, e);
                nfsstat3::NFS3ERR_IO
            })?;
            let newly_dirty = !guard.dirty;
            guard.mark_dirty(now_secs());
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
        let inode = self.inode(id)?;
        if inode.kind != EntryKind::File {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }

        // Staged content is newer than anything in the bucket.
        if let Some(mut guard) = self.stage_guard(id).await {
            let size = guard.size;
            let data = guard.read_at(offset, count as usize).await.map_err(|e| {
                log::error!(
                    "mount: failed to read the stage for \"{}\": {}",
                    inode.key,
                    e
                );
                nfsstat3::NFS3ERR_IO
            })?;
            let eof = offset.saturating_add(data.len() as u64) >= size;
            return Ok((data, eof));
        }

        if offset >= inode.size || count == 0 {
            return Ok((Vec::new(), offset >= inode.size));
        }

        // Served from the chunk cache: one object fetch covers dozens of
        // client transfers, and concurrent transfers share a fetch instead of
        // each paying an S3 round trip.
        let end = offset.saturating_add(u64::from(count)).min(inode.size);
        let (first, last) = read_cache::chunks_covering(offset, end);

        let mut data = Vec::with_capacity((end - offset) as usize);
        for index in first..=last {
            let mut chunk = self.chunk_bytes(id, &inode.key, index, inode.size).await?;
            // A cached chunk shorter than the current size implies is stale —
            // the object was replaced with a bigger one after the chunk was
            // fetched. Refetch once; the second answer is the object as it is
            // now, whatever length that turns out to be.
            if (chunk.len() as u64) < read_cache::chunk_len(index, inode.size) {
                self.inner.read_cache.remove_slot(id, index);
                chunk = self.chunk_bytes(id, &inode.key, index, inode.size).await?;
            }
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
            self.prefetch_after(id, &inode.key, last, inode.size);
        }

        // Producing fewer bytes than the request spans means the object ends
        // before `inode.size` says it does. That is the true end of file:
        // answering "no data, not EOF" would make the client re-issue the same
        // read forever.
        let produced_end = offset.saturating_add(data.len() as u64);
        let eof = produced_end >= inode.size || produced_end < end;
        Ok((data, eof))
    }

    async fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> Result<fattr3, nfsstat3> {
        self.ensure_writable()?;
        let inode = self.inode(id)?;
        if inode.kind != EntryKind::File {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }

        let mut guard = self.ensure_stage(id, &inode).await?;
        guard.write_at(offset, data).await.map_err(|e| {
            log::error!("mount: failed to stage a write to \"{}\": {}", inode.key, e);
            nfsstat3::NFS3ERR_IO
        })?;
        let newly_dirty = !guard.dirty;
        guard.mark_dirty(now_secs());

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
        let dir = self.dir_inode(dirid)?;
        let name = self.child_name(filename)?;
        let dir_key = normalize_dir_key(&dir.key);
        let key = child_key(&dir_key, &name, false);

        // CREATE is not only used for new files: a client that misses in its
        // own cache sends it for a name that is already there, and `touch` on
        // an existing file is exactly that. Asking S3 first is what keeps the
        // zero-byte object below from blanking real content.
        let existing = self.head_object(&key).await;
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
        self.put_empty_object(&key).await?;

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
        let dir = self.dir_inode(dirid)?;
        let name = self.child_name(filename)?;
        let dir_key = normalize_dir_key(&dir.key);
        let key = child_key(&dir_key, &name, false);

        // S3 has no conditional create, so exclusivity is a check followed by a
        // write. Two clients racing for the same new name is not a case worth
        // more machinery than that.
        if self.head_object(&key).await.is_some() {
            return Err(nfsstat3::NFS3ERR_EXIST);
        }

        self.put_empty_object(&key).await?;
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
        let dir = self.dir_inode(dirid)?;
        let name = self.child_name(dirname)?;
        let dir_key = normalize_dir_key(&dir.key);

        let children = self.children_of(dirid, &dir_key).await?;
        if children.iter().any(|child| child.name == name) {
            return Err(nfsstat3::NFS3ERR_EXIST);
        }

        // A zero-byte object whose key ends in `/` is the folder marker every
        // S3 tool understands, and is what makes an empty directory visible at
        // all — without it there is no prefix to list.
        let key = child_key(&dir_key, &name, true);
        self.put_empty_object(&key).await?;

        let id = self.intern_child(&key, dirid, EntryKind::Dir, DIR_SIZE, now_secs())?;
        self.invalidate_dir(dirid);

        let inode = self.inode(id)?;
        Ok((id, self.attr_of(id, &inode)))
    }

    async fn remove(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3> {
        self.ensure_writable()?;
        let dir = self.dir_inode(dirid)?;
        let name = self.child_name(filename)?;
        let dir_key = normalize_dir_key(&dir.key);

        // RMDIR and REMOVE both land here, so the entry's kind decides what
        // "delete" means.
        let (id, target) = self.resolve_child(dirid, &dir_key, &name).await?;

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
        let from_dir = self.dir_inode(from_dirid)?;
        let to_dir = self.dir_inode(to_dirid)?;
        let from_name = self.child_name(from_filename)?;
        let to_name = self.child_name(to_filename)?;

        let from_dir_key = normalize_dir_key(&from_dir.key);
        let to_dir_key = normalize_dir_key(&to_dir.key);

        let (id, source) = self
            .resolve_child(from_dirid, &from_dir_key, &from_name)
            .await?;
        let is_dir = source.kind == EntryKind::Dir;
        let target_key = child_key(&to_dir_key, &to_name, is_dir);
        if target_key == source.key {
            return Ok(());
        }

        // What is already at the destination decides whether this rename is
        // allowed at all. Checked after the same-name shortcut above, since
        // there the destination *is* the source.
        let destination = self.lookup_child(to_dirid, &to_dir_key, &to_name).await?;
        let destination_dir_is_empty = match &destination {
            Some((_, inode)) if inode.kind == EntryKind::Dir => {
                self.dir_is_empty(&normalize_dir_key(&inode.key)).await?
            }
            _ => true,
        };
        match classify_rename_target(
            is_dir,
            destination.as_ref().map(|(_, inode)| inode.kind),
            destination_dir_is_empty,
        ) {
            RenameTarget::Replace => {}
            RenameTarget::KindMismatch => return Err(nfsstat3::NFS3ERR_EXIST),
            RenameTarget::NotEmpty => return Err(nfsstat3::NFS3ERR_NOTEMPTY),
        }

        if is_dir {
            self.rename_dir(id, &source.key, &target_key, to_dirid)
                .await?;
        } else {
            self.rename_file(id, &source.key, &target_key, to_dirid)
                .await?;
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
        let dir = self.dir_inode(dirid)?;
        let dir_key = normalize_dir_key(&dir.key);
        let children = self.children_of(dirid, &dir_key).await?;

        let after_name = if start_after == 0 {
            None
        } else {
            // An unknown cookie is the one case we cannot interpret at all.
            let inode = self
                .inode(start_after)
                .map_err(|_| nfsstat3::NFS3ERR_BAD_COOKIE)?;
            Some(entry_name(&inode.key).to_string())
        };

        let start = resume_index(&children, after_name.as_deref());
        let (end, is_last_page) = page_end(children.len(), start, max_entries);

        let mut entries = Vec::with_capacity(end.saturating_sub(start));
        for child in &children[start..end] {
            let inode = self.inode(child.fileid)?;
            let attr = match self.try_staged_attr(child.fileid, &inode).await {
                Some(staged) => staged,
                None => self.attr_of(child.fileid, &inode),
            };
            entries.push(DirEntry {
                fileid: child.fileid,
                name: child.name.as_bytes().into(),
                attr,
            });
        }

        Ok(ReadDirResult {
            entries,
            end: is_last_page,
        })
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
        // Unuploaded content has to reach the bucket first, or the server-side
        // copy would publish the pre-edit object at the new name.
        self.flush_stage_blocking(id).await?;

        self.copy_object(from_key, to_key).await?;
        self.delete_object(from_key).await?;
        // The stage is kept rather than dropped: a write that landed between
        // the flush and here would otherwise be thrown away.
        self.rekey_stage(id, to_key).await;

        if let Ok(mut inodes) = self.inner.inodes.write() {
            inodes.rekey(id, to_key, to_dirid);
        }
        Ok(())
    }

    /// Moves a whole prefix, one server-side copy per object.
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
            // Moving a directory inside itself has no meaning and would rewrite
            // keys onto themselves forever.
            return Err(nfsstat3::NFS3ERR_INVAL);
        }

        self.flush_stages_under(&from_prefix).await?;
        let keys = self.list_prefix_keys(&from_prefix).await?;
        let moves: Vec<(String, String)> = keys
            .into_iter()
            .filter_map(|key| {
                rewrite_key(&key, &from_prefix, &to_prefix).map(|new_key| (key, new_key))
            })
            .collect();

        let copies: Vec<_> = moves
            .iter()
            .map(|(from, to)| self.copy_object(from, to))
            .collect();
        let copies: Vec<Result<(), nfsstat3>> = stream::iter(copies)
            .buffer_unordered(RENAME_COPY_CONCURRENCY)
            .collect()
            .await;

        // The listings are wrong either way once the first copy has landed.
        self.invalidate_all_dirs();

        let mut failure: Option<nfsstat3> = None;
        let mut copied: Vec<&str> = Vec::new();
        for ((_, to), result) in moves.iter().zip(copies) {
            match result {
                Ok(()) => copied.push(to.as_str()),
                Err(status) => {
                    failure.get_or_insert(status);
                }
            }
        }

        if let Some(status) = failure {
            // Undo the copies that did land. A half-copied rename that is left
            // alone shows the folder under both names with different contents,
            // and the source is still the real one.
            let rollback: Vec<_> = copied.iter().map(|key| self.delete_object(key)).collect();
            let rollback: Vec<Result<(), nfsstat3>> = stream::iter(rollback)
                .buffer_unordered(RENAME_COPY_CONCURRENCY)
                .collect()
                .await;
            for (key, result) in copied.iter().zip(rollback) {
                if result.is_err() {
                    log::error!(
                        "mount: a failed rename left a copy at \"{}\" that could not be removed; it has to be deleted by hand",
                        key
                    );
                }
            }
            return Err(status);
        }

        let deletes: Vec<_> = moves
            .iter()
            .map(|(from, _)| self.delete_object(from))
            .collect();
        let deletes: Vec<Result<(), nfsstat3>> = stream::iter(deletes)
            .buffer_unordered(RENAME_COPY_CONCURRENCY)
            .collect()
            .await;

        let mut failure: Option<nfsstat3> = None;
        for ((from, _), result) in moves.iter().zip(deletes) {
            if let Err(status) = result {
                // The copy succeeded, so the data is safe at the new name; what
                // is left is the old name still pointing at it.
                log::error!(
                    "mount: renamed \"{}\" but could not delete the original; it is still in the bucket under its old name",
                    from
                );
                failure.get_or_insert(status);
            }
        }
        if let Some(status) = failure {
            return Err(status);
        }

        self.rekey_stages_under(&from_prefix, &to_prefix).await;
        if let Ok(mut inodes) = self.inner.inodes.write() {
            inodes.rekey_prefix(&from_prefix, &to_prefix);
            inodes.rekey(id, &to_prefix, to_dirid);
        }
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
