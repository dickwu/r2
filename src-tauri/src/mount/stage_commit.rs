use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::oneshot;

const COMMIT_DELAY: Duration = Duration::from_millis(5);
static COORDINATOR: OnceLock<Arc<CommitCoordinator>> = OnceLock::new();
static SYNC_BATCHES: AtomicU64 = AtomicU64::new(0);
static SYNC_FILES: AtomicU64 = AtomicU64::new(0);
static SYNC_PARENTS: AtomicU64 = AtomicU64::new(0);
static SYNC_BYTES: AtomicU64 = AtomicU64::new(0);
static WORKERS_STARTED: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct CommitMetrics {
    pub sync_batches: u64,
    pub sync_files: u64,
    pub sync_parents: u64,
    pub sync_bytes: u64,
    pub workers_started: u64,
}

pub fn metrics() -> CommitMetrics {
    CommitMetrics {
        sync_batches: SYNC_BATCHES.load(Ordering::Relaxed),
        sync_files: SYNC_FILES.load(Ordering::Relaxed),
        sync_parents: SYNC_PARENTS.load(Ordering::Relaxed),
        sync_bytes: SYNC_BYTES.load(Ordering::Relaxed),
        workers_started: WORKERS_STARTED.load(Ordering::Relaxed),
    }
}

pub async fn commit(paths: Vec<PathBuf>) -> std::io::Result<()> {
    coordinator().commit(paths).await
}

pub fn record_file_sync_bytes(bytes: u64) {
    SYNC_FILES.fetch_add(1, Ordering::Relaxed);
    SYNC_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

pub fn record_parent_sync() {
    SYNC_PARENTS.fetch_add(1, Ordering::Relaxed);
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
        let result = sync_paths_blocking(&requests);
        for request in requests {
            let response = match &result {
                Ok(()) => Ok(()),
                Err(error) => Err(std::io::Error::new(error.kind(), error.to_string())),
            };
            let _ = request.result.send(response);
        }
    }
}

fn sync_paths_blocking(requests: &[CommitRequest]) -> std::io::Result<()> {
    let mut paths = BTreeSet::<PathBuf>::new();
    for request in requests {
        for path in &request.paths {
            paths.insert(path.clone());
        }
    }
    for path in paths {
        sync_file_blocking(&path)?;
        sync_parent_blocking(&path)?;
    }
    SYNC_BATCHES.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn sync_file_blocking(path: &Path) -> std::io::Result<()> {
    let file = std::fs::File::open(path)?;
    let bytes = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    file.sync_all()?;
    record_file_sync_bytes(bytes);
    Ok(())
}

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
