use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::oneshot;

const COMMIT_DELAY: Duration = Duration::from_millis(5);
static COORDINATOR: OnceLock<Arc<CommitCoordinator>> = OnceLock::new();
static SYNC_BATCHES: AtomicU64 = AtomicU64::new(0);
static SYNC_FILES: AtomicU64 = AtomicU64::new(0);
static SYNC_PARENTS: AtomicU64 = AtomicU64::new(0);
static SYNC_BYTES: AtomicU64 = AtomicU64::new(0);
static SYNC_FAILURES: AtomicU64 = AtomicU64::new(0);
static WORKERS_STARTED: AtomicU64 = AtomicU64::new(0);
/// Files whose fsync failed, with that failure. Linux may mark the pages a
/// failed write-back dropped as clean, so a later fsync of the same file can
/// succeed without them: nothing is acknowledged through a poisoned file
/// until its owner has rewritten it and cleared the entry.
static POISONED: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct CommitMetrics {
    pub sync_batches: u64,
    pub sync_files: u64,
    pub sync_parents: u64,
    pub sync_bytes: u64,
    pub workers_started: u64,
    pub sync_failures: u64,
    /// Files currently refusing acknowledgements after a failed fsync.
    pub poisoned_files: u64,
}

pub fn metrics() -> CommitMetrics {
    CommitMetrics {
        sync_batches: SYNC_BATCHES.load(Ordering::Relaxed),
        sync_files: SYNC_FILES.load(Ordering::Relaxed),
        sync_parents: SYNC_PARENTS.load(Ordering::Relaxed),
        sync_bytes: SYNC_BYTES.load(Ordering::Relaxed),
        workers_started: WORKERS_STARTED.load(Ordering::Relaxed),
        sync_failures: SYNC_FAILURES.load(Ordering::Relaxed),
        poisoned_files: poisoned_files().len() as u64,
    }
}

fn poisoned_files() -> std::sync::MutexGuard<'static, HashMap<PathBuf, String>> {
    POISONED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// The fsync failure that poisoned `path`, if one did.
pub fn poisoned(path: &Path) -> Option<String> {
    poisoned_files().get(path).cloned()
}

/// Records that an fsync of `path` (or of the directory entry that names it)
/// failed; see `POISONED`.
pub(super) fn poison(path: &Path, error: &std::io::Error) {
    SYNC_FAILURES.fetch_add(1, Ordering::Relaxed);
    poisoned_files()
        .entry(path.to_path_buf())
        .or_insert_with(|| error.to_string());
}

/// Only for the owner, once the file has been rewritten and made durable.
pub(super) fn clear_poison(path: &Path) {
    poisoned_files().remove(path);
}

fn poisoned_error(failure: &str) -> std::io::Error {
    std::io::Error::other(format!(
        "an earlier fsync of this file failed ({failure}); nothing is acknowledged until it has been rewritten"
    ))
}

pub async fn commit(paths: Vec<PathBuf>) -> std::io::Result<()> {
    coordinator().commit(paths).await
}

/// How every existing file that is about to be fsynced gets opened: the WAL,
/// data files, upload snapshots, recovery exports.
///
/// `File::sync_all` is `FlushFileBuffers` on Windows, which fails with access
/// denied on a handle without write access, so these files are opened
/// read-write on every platform. Unix ignores the access mode for fsync, so
/// behaviour there is unchanged. It never creates or truncates; files created
/// for writing already hold a writable handle. Directories are not opened
/// here: their fsync (`sync_parent`) is Unix-only and a no-op on Windows,
/// where NTFS journals its own metadata and std cannot open a directory.
pub fn sync_open_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    options
}

pub fn record_file_sync_bytes(bytes: u64) {
    SYNC_FILES.fetch_add(1, Ordering::Relaxed);
    SYNC_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

/// Directories are synced only on Unix (`sync_parent_blocking`, `stage::sync_parent`).
#[cfg(unix)]
pub fn record_parent_sync() {
    SYNC_PARENTS.fetch_add(1, Ordering::Relaxed);
}

/// Paths whose next fsyncs fail, for fault-injection tests.
#[cfg(test)]
static FAILING_SYNCS: OnceLock<Mutex<HashMap<PathBuf, u32>>> = OnceLock::new();

#[cfg(test)]
pub fn fail_next_syncs(path: &Path, count: u32) {
    *FAILING_SYNCS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_default() += count;
}

/// Consumes one failure injected for `path` by a test; always Ok otherwise.
pub(super) fn injected_sync_failure(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    {
        let mut failing = FAILING_SYNCS.get_or_init(Default::default).lock().unwrap();
        if let Some(remaining) = failing.get_mut(path).filter(|remaining| **remaining > 0) {
            *remaining -= 1;
            return Err(std::io::Error::other("injected fsync failure"));
        }
    }
    #[cfg(not(test))]
    let _ = path;
    Ok(())
}

fn coordinator() -> Arc<CommitCoordinator> {
    COORDINATOR.get_or_init(CommitCoordinator::start).clone()
}

struct CommitCoordinator {
    sender: std::sync::mpsc::Sender<CommitRequest>,
    #[cfg(test)]
    workers_started: AtomicU64,
}

struct CommitRequest {
    paths: Vec<PathBuf>,
    result: oneshot::Sender<std::io::Result<()>>,
}

impl CommitCoordinator {
    fn start() -> Arc<Self> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let coordinator = Arc::new(Self {
            sender,
            #[cfg(test)]
            workers_started: AtomicU64::new(1),
        });
        WORKERS_STARTED.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || worker_loop(receiver));
        coordinator
    }

    async fn commit(&self, paths: Vec<PathBuf>) -> std::io::Result<()> {
        let (result, wait) = oneshot::channel();
        self.sender
            .send(CommitRequest { paths, result })
            .map_err(|_| std::io::Error::other("commit coordinator stopped"))?;
        wait.await
            .unwrap_or_else(|_| Err(std::io::Error::other("commit coordinator stopped")))
    }
}

fn worker_loop(receiver: std::sync::mpsc::Receiver<CommitRequest>) {
    while let Ok(first) = receiver.recv() {
        std::thread::sleep(COMMIT_DELAY);
        let mut requests = vec![first];
        while let Ok(request) = receiver.try_recv() {
            requests.push(request);
        }
        let results = sync_paths_blocking(&requests);
        for request in requests {
            // A request succeeds only if every file it asked for is durable.
            let response = request
                .paths
                .iter()
                .find_map(|path| results.get(path).and_then(|result| result.as_ref().err()))
                .map_or(Ok(()), |error| {
                    Err(std::io::Error::new(error.kind(), error.to_string()))
                });
            let _ = request.result.send(response);
        }
    }
}

fn sync_paths_blocking(requests: &[CommitRequest]) -> HashMap<PathBuf, std::io::Result<()>> {
    let mut paths = BTreeSet::<PathBuf>::new();
    for request in requests {
        for path in &request.paths {
            paths.insert(path.clone());
        }
    }
    let results = paths
        .into_iter()
        .map(|path| {
            let result = sync_or_poison(&path);
            (path, result)
        })
        .collect();
    SYNC_BATCHES.fetch_add(1, Ordering::Relaxed);
    results
}

/// Every acknowledgement fsync runs here, on the one worker thread, in batch
/// order. The poison check therefore sits between a failed fsync and every
/// batch after it: none of them can acknowledge the file until it is cleared.
fn sync_or_poison(path: &Path) -> std::io::Result<()> {
    if let Some(failure) = poisoned(path) {
        return Err(poisoned_error(&failure));
    }
    let result = sync_file_blocking(path).and_then(|()| sync_parent_blocking(path));
    if let Err(error) = &result {
        poison(path, error);
    }
    result
}

fn sync_file_blocking(path: &Path) -> std::io::Result<()> {
    injected_sync_failure(path)?;
    let file = sync_open_options().open(path)?;
    let bytes = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    file.sync_all()?;
    record_file_sync_bytes(bytes);
    Ok(())
}

/// Directory fsync; a no-op on Windows (see `sync_open_options`).
fn sync_parent_blocking(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
        record_parent_sync();
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_commits_start_one_worker_for_one_wave() {
        let root = std::env::temp_dir().join(format!(
            "r2-commit-worker-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let path = root.join("wal");
        tokio::fs::write(&path, b"record").await.unwrap();
        let coordinator = CommitCoordinator::start();
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let path = path.clone();
            let coordinator = coordinator.clone();
            tasks.push(tokio::spawn(async move {
                coordinator.commit(vec![path]).await.unwrap()
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(coordinator.workers_started.load(Ordering::Relaxed), 1);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn files_opened_for_sync_carry_write_access_and_are_never_created() {
        let root = std::env::temp_dir().join(format!(
            "r2-sync-handle-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("wal");
        std::fs::write(&path, b"record").unwrap();
        // FlushFileBuffers — sync_all on Windows — refuses a handle without
        // write access. Changing the length needs that access on every OS,
        // so a handle that can do it is one Windows will flush.
        let file = sync_open_options().open(&path).unwrap();
        file.set_len(6).unwrap();
        file.sync_all().unwrap();
        assert!(
            std::fs::File::open(&path).unwrap().set_len(6).is_err(),
            "a plain read-only open is the handle Windows cannot flush"
        );
        assert!(sync_open_options().open(root.join("missing")).is_err());
        assert!(!root.join("missing").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_fsync_fails_every_later_batch_for_that_file_only() {
        let root = std::env::temp_dir().join(format!(
            "r2-commit-poison-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let wal = root.join("wal");
        let other = root.join("other");
        tokio::fs::write(&wal, b"record").await.unwrap();
        tokio::fs::write(&other, b"record").await.unwrap();
        let coordinator = CommitCoordinator::start();
        fail_next_syncs(&wal, 1);
        assert!(coordinator.commit(vec![wal.clone()]).await.is_err());
        // The injected failure is spent, so a plain fsync would now succeed —
        // exactly how Linux can report success after dropping dirty pages.
        for _ in 0..3 {
            assert!(
                coordinator.commit(vec![wal.clone()]).await.is_err(),
                "a later batch acknowledged past a failed fsync"
            );
        }
        let (unrelated, poisoned) = tokio::join!(
            coordinator.commit(vec![other.clone()]),
            coordinator.commit(vec![wal.clone()])
        );
        unrelated.unwrap();
        assert!(poisoned.is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn runtime_drop_does_not_poison_global_coordinator() {
        let root = std::env::temp_dir().join(format!(
            "r2-commit-runtime-drop-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("wal");
        std::fs::write(&path, b"record").unwrap();
        {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let path = path.clone();
            let handle = runtime.spawn(async move {
                let _ = commit(vec![path]).await;
            });
            drop(handle);
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async { commit(vec![path]).await.unwrap() });
        std::fs::remove_dir_all(root).unwrap();
    }
}
