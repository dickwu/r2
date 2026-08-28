//! Mount lifecycle: one localhost NFS server plus one OS mount per bucket.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aws_sdk_s3::Client;
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tokio::task::JoinHandle;

use super::nfs_fs::S3NfsFs;
use super::platform::{self, MountPlatform};
use super::stage;

/// Total time the exit path may spend unmounting, across every mount and every
/// fallback command. App exit runs on the main thread, so the whole teardown —
/// not just one attempt — has to be bounded.
const EXIT_UNMOUNT_BUDGET: Duration = Duration::from_secs(8);
/// Total time the exit path may spend uploading staged writes, across every
/// mount. Whatever does not fit is left on disk rather than delaying the quit.
const EXIT_FLUSH_BUDGET: Duration = Duration::from_secs(10);
/// Upload rounds attempted before the client is disconnected, and after.
const DRAIN_ROUNDS: u32 = 3;
/// The exit path retries nothing: every round and every attempt comes out of
/// the budget shared with the other mounts.
const EXIT_DRAIN_ROUNDS: u32 = 1;
const EXIT_UPLOAD_ATTEMPTS: u32 = 1;
/// Ceiling on any single blocking unmount attempt within that budget.
const EXIT_UNMOUNT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
/// Shorter ceiling for the unmount used to clear a leftover mount.
#[cfg(not(windows))]
const STALE_UMOUNT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long to wait for a killed child to be reaped before detaching from it.
const REAP_TIMEOUT: Duration = Duration::from_millis(500);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Ceilings for the interactive (user-initiated) commands.
const MOUNT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const UNMOUNT_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Process-wide mount registry, following the `OnceLock` globals this crate
/// uses for shared state (see `db::DB_CONNECTION`).
static MOUNT_MANAGER: OnceLock<MountManager> = OnceLock::new();

pub fn manager() -> &'static MountManager {
    MOUNT_MANAGER.get_or_init(MountManager::new)
}

/// Storage backend a mount is served from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountProvider {
    R2,
    Aws,
    Minio,
    Rustfs,
}

/// A live mount, as reported to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct MountInfo {
    pub mount_id: String,
    pub provider: MountProvider,
    pub account_id: String,
    pub bucket: String,
    pub local_path: String,
    pub port: u16,
    pub read_only: bool,
    pub mounted_at: i64,
}

/// Payload of the `mount-changed` event: always the complete mount list.
#[derive(Debug, Clone, Serialize)]
pub struct MountChangedPayload {
    pub mounts: Vec<MountInfo>,
}

/// Everything the manager needs to bring a mount up. The S3 client is built by
/// the command layer so provider credential handling stays in one place.
pub struct MountRequest {
    pub provider: MountProvider,
    pub account_id: String,
    pub bucket: String,
    /// Absolute directory path (unix) or drive specifier such as `Z:` (Windows).
    pub local_path: String,
    pub client: Client,
    pub read_only: bool,
    /// Directory the per-mount staging folder is created under. Deliberately
    /// the app's cache directory rather than the system temp directory:
    /// unflushed writes have to survive an OS temp sweep so they can still be
    /// recovered by hand.
    pub staging_root: PathBuf,
    /// Used by the flusher to report an upload it could not complete.
    pub app: tauri::AppHandle,
}

struct ActiveMount {
    info: MountInfo,
    server: JoinHandle<()>,
    /// Absent on a read-only mount, which has nothing to upload.
    flusher: Option<JoinHandle<()>>,
    /// Second handle to the filesystem, kept so staged writes can be drained
    /// once the client has been disconnected.
    fs: S3NfsFs,
}

/// Registry of active mounts.
///
/// The map is a plain `std::sync::Mutex` — only ever held for reads and writes,
/// never across an await — so the app-exit hook can drain it synchronously
/// without needing an async runtime.
pub struct MountManager {
    mounts: Mutex<HashMap<String, ActiveMount>>,
    /// Serializes mount setup so two concurrent requests cannot claim the same
    /// path between validation and registration.
    setup: tokio::sync::Mutex<()>,
    counter: AtomicU64,
}

impl Default for MountManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MountManager {
    pub fn new() -> Self {
        Self {
            mounts: Mutex::new(HashMap::new()),
            setup: tokio::sync::Mutex::new(()),
            counter: AtomicU64::new(1),
        }
    }

    /// Active mounts, oldest first, so the UI ordering is stable.
    pub fn list(&self) -> Vec<MountInfo> {
        let Ok(mounts) = self.mounts.lock() else {
            return Vec::new();
        };
        let mut infos: Vec<MountInfo> = mounts.values().map(|m| m.info.clone()).collect();
        infos.sort_by(|a, b| {
            a.mounted_at
                .cmp(&b.mounted_at)
                .then_with(|| a.mount_id.cmp(&b.mount_id))
        });
        infos
    }

    /// Broadcasts the full mount list. Fire-and-forget, like the transfer events.
    pub fn emit_changed(&self, app: &tauri::AppHandle) {
        let _ = app.emit(
            "mount-changed",
            MountChangedPayload {
                mounts: self.list(),
            },
        );
    }

    fn next_mount_id(&self) -> String {
        format!(
            "mnt-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            self.counter.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Starts an NFS server for the bucket and mounts it at `local_path`.
    pub async fn mount(&self, request: MountRequest) -> Result<MountInfo, String> {
        let platform = MountPlatform::CURRENT;
        let _setup = self.setup.lock().await;

        let target = normalize_target(&request.local_path)?;
        self.ensure_path_available(&target)?;
        let created_dir = prepare_target(platform, &target)?;

        // Fail early with a credentials/bucket error rather than a confusing
        // mount timeout later on.
        request
            .client
            .list_objects_v2()
            .bucket(&request.bucket)
            .max_keys(1)
            .send()
            .await
            .map_err(|e| format!("Failed to open bucket \"{}\": {}", request.bucket, e))?;

        let active: Vec<String> = match self.mounts.lock() {
            Ok(mounts) => mounts.keys().cloned().collect(),
            Err(_) => Vec::new(),
        };
        sweep_staging_root(&request.staging_root, &active);

        // The id is minted up front because the staging folder is named after
        // it and the filesystem needs it before the server can bind.
        let mount_id = self.next_mount_id();
        let staging_dir = request.staging_root.join(&mount_id);
        if !request.read_only {
            // Surfaced now rather than at the first write, when the failure
            // would reach the user as an unexplained I/O error.
            tokio::fs::create_dir_all(&staging_dir).await.map_err(|e| {
                format!(
                    "Failed to create the staging folder \"{}\": {}",
                    staging_dir.display(),
                    e
                )
            })?;
        }

        let fs = S3NfsFs::new(
            request.client,
            request.bucket.clone(),
            request.read_only,
            staging_dir,
        );
        fs.set_progress(request.app.clone(), mount_id.clone());

        // The Windows NFS client cannot be pointed at a port: it asks the
        // portmapper on port 111 of the host it mounts. So on Windows the
        // server takes port 111 on a private loopback IP of its own
        // (127.88.x.y, nfsserve's "auto" mode) — which also lets every mount
        // keep its own server. Unix clients accept a port option, so there an
        // ephemeral port on 127.0.0.1 does.
        let bind_addr = match platform {
            MountPlatform::Windows => "auto:111",
            _ => "127.0.0.1:0",
        };
        let listener = NFSTcpListener::bind(bind_addr, fs.clone())
            .await
            .map_err(|e| match platform {
                MountPlatform::Windows => format!(
                    "Failed to start the local NFS server on port 111: {}. Another NFS or portmapper service may be using it.",
                    e
                ),
                _ => format!("Failed to start local NFS server: {}", e),
            })?;
        let port = listener.get_listen_port();
        let server_ip = listener.get_listen_ip().to_string();

        let server = tokio::spawn(async move {
            if let Err(e) = listener.handle_forever().await {
                log::error!("mount: NFS server stopped: {}", e);
            }
        });

        if let Err(e) =
            run_mount_command(platform, &server_ip, port, &target, request.read_only).await
        {
            server.abort();
            let _ = tokio::fs::remove_dir(fs.staging_root()).await;
            if created_dir {
                let _ = std::fs::remove_dir(&target);
            }
            return Err(e);
        }

        let flusher = if request.read_only {
            None
        } else {
            Some(fs.spawn_flusher(request.app.clone(), mount_id.clone()))
        };

        let info = MountInfo {
            mount_id,
            provider: request.provider,
            account_id: request.account_id,
            bucket: request.bucket,
            local_path: target,
            port,
            read_only: request.read_only,
            mounted_at: chrono::Utc::now().timestamp(),
        };

        // The registry guard must not be held across the cleanup await below.
        let unregistered = match self.mounts.lock() {
            Ok(mut mounts) => {
                mounts.insert(
                    info.mount_id.clone(),
                    ActiveMount {
                        info: info.clone(),
                        server,
                        flusher,
                        fs,
                    },
                );
                None
            }
            Err(_) => Some((server, flusher)),
        };

        if let Some((server, flusher)) = unregistered {
            // Nothing can reach this mount if it cannot be registered, so undo
            // it rather than leave a mountpoint nobody can release.
            let _ = run_umount_command(platform, &info.local_path).await;
            server.abort();
            if let Some(flusher) = flusher {
                flusher.abort();
            }
            return Err("Mount registry is unavailable".to_string());
        }

        Ok(info)
    }

    /// Unmounts a single mount, uploading anything staged along the way.
    ///
    /// The order matters for data safety. The pre-drain narrows the window;
    /// unmounting then forces the OS client to write back its own page cache,
    /// so the last writes of a copy only arrive after the `umount` returns,
    /// which is what the post-drain is for. If the OS refuses to unmount
    /// (usually a file still open) the mount stays registered, with its flusher
    /// still running, so the user can retry.
    pub async fn unmount(&self, mount_id: &str) -> Result<(), String> {
        let (local_path, fs) = {
            let mounts = self
                .mounts
                .lock()
                .map_err(|_| "Mount registry is unavailable".to_string())?;
            mounts
                .get(mount_id)
                .map(|mount| (mount.info.local_path.clone(), mount.fs.clone()))
                .ok_or_else(|| format!("No active mount with id \"{}\"", mount_id))?
        };

        fs.drain(DRAIN_ROUNDS, stage::UPLOAD_ATTEMPTS).await;

        run_umount_command(MountPlatform::CURRENT, &local_path).await?;

        let removed = match self.mounts.lock() {
            Ok(mut mounts) => mounts.remove(mount_id),
            Err(_) => None,
        };
        if let Some(mount) = &removed {
            // Stop the background flusher before the final drain so the two do
            // not upload the same file at once.
            if let Some(flusher) = &mount.flusher {
                flusher.abort();
            }
        }
        // Aborting the scanner does not stop the uploads it already started —
        // those are detached tasks. Waiting them out is what keeps the drain
        // below from racing one for the same key, and keeps the staging folder
        // from being deleted while a multipart upload is still reading it.
        fs.wait_for_flushes().await;
        fs.reset_flush_state().await;

        let unflushed = fs.drain(DRAIN_ROUNDS, stage::UPLOAD_ATTEMPTS).await;

        if let Some(mount) = removed {
            mount.server.abort();
        }

        if unflushed > 0 {
            // The OS mount is gone either way, so the registry entry cannot
            // stay — but the staged copies do, because they are the only place
            // that content still exists.
            return Err(format!(
                "Unmounted \"{}\", but {} file{} could not be uploaded. The unsent copies are kept in {}",
                local_path,
                unflushed,
                if unflushed == 1 { "" } else { "s" },
                fs.staging_root().display()
            ));
        }

        let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
        Ok(())
    }

    /// Best-effort teardown of every mount, used when the app exits.
    ///
    /// Synchronous on purpose: it runs from Tauri's `RunEvent::Exit` handler,
    /// where there is no async context to await in. The whole teardown shares
    /// one deadline, so neither a wedged mount nor a long list of them can stall
    /// the quit; whatever is left when the budget runs out is abandoned.
    pub fn unmount_all(&self, app: &tauri::AppHandle) {
        let drained: Vec<ActiveMount> = match self.mounts.lock() {
            Ok(mut mounts) => mounts.drain().map(|(_, mount)| mount).collect(),
            Err(_) => return,
        };

        if drained.is_empty() {
            return;
        }

        let platform = MountPlatform::CURRENT;
        let deadline = Instant::now() + EXIT_UNMOUNT_BUDGET;
        let mut writable = Vec::new();

        for mount in drained {
            let mut unmounted = false;
            for argv in platform::umount_argvs(platform, &mount.info.local_path) {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                if run_bounded(&argv, remaining.min(EXIT_UNMOUNT_ATTEMPT_TIMEOUT)) {
                    unmounted = true;
                    break;
                }
            }
            if !unmounted {
                log::warn!(
                    "mount: failed to unmount \"{}\" on exit",
                    mount.info.local_path
                );
            }
            // The server task is aborted either way — the process is going away.
            mount.server.abort();
            if let Some(flusher) = mount.flusher {
                flusher.abort();
                writable.push(mount.fs);
            }
        }

        drain_on_exit(writable);
        self.emit_changed(app);
    }

    /// Rejects a target that is the same as, inside, or a parent of an existing
    /// mount — nesting mounts produces a filesystem the user cannot untangle.
    fn ensure_path_available(&self, target: &str) -> Result<(), String> {
        let mounts = self
            .mounts
            .lock()
            .map_err(|_| "Mount registry is unavailable".to_string())?;

        for mount in mounts.values() {
            if paths_conflict(target, &mount.info.local_path) {
                return Err(format!(
                    "\"{}\" conflicts with the existing mount at \"{}\"",
                    target, mount.info.local_path
                ));
            }
        }

        Ok(())
    }
}

/// Clears out staging folders left behind by an earlier session.
///
/// An empty one is just litter from a clean unmount that raced the process
/// exiting. A folder with files in it holds writes that never reached the
/// bucket, so it is kept and its path logged — deleting it would be deleting
/// the only copy. Folders belonging to mounts that are live right now are left
/// alone, since one of them is in use by definition.
fn sweep_staging_root(root: &Path, active: &[String]) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || active.iter().any(|id| entry.file_name() == id.as_str()) {
            continue;
        }
        match std::fs::read_dir(&path).map(|mut dir| dir.next().is_none()) {
            Ok(true) => {
                let _ = std::fs::remove_dir(&path);
            }
            Ok(false) => log::warn!(
                "mount: \"{}\" still holds writes from an earlier session that never reached the bucket",
                path.display()
            ),
            Err(_) => {}
        }
    }
}

/// Uploads whatever the writable mounts still hold, within one shared budget.
///
/// Runs from Tauri's exit handler, which is not inside the async runtime, so
/// the async drain is entered through `block_on`. Quitting mid-copy is the one
/// case where content can be left behind; the staging folder is kept and its
/// path logged so the file can still be recovered by hand.
fn drain_on_exit(mounts: Vec<S3NfsFs>) {
    if mounts.is_empty() {
        return;
    }

    let deadline = Instant::now() + EXIT_FLUSH_BUDGET;
    for fs in mounts {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let staging_root = fs.staging_root().to_path_buf();

        let unflushed = if remaining.is_zero() {
            1
        } else {
            tauri::async_runtime::block_on(async {
                // The uploads the flusher already started are detached tasks
                // that the abort did not touch. They have to finish before the
                // drain, or two PUTs race for one key and the older bytes can
                // be the ones that land last.
                if tokio::time::timeout(remaining, fs.wait_for_flushes())
                    .await
                    .is_err()
                {
                    return 1;
                }

                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return 1;
                }

                fs.reset_flush_state().await;
                // One round, one attempt: the budget is shared across every
                // mount, so a file that is failing must not spend it retrying.
                // A timeout counts as a file left behind — all that matters
                // past here is whether the staging folder has to be kept.
                tokio::time::timeout(remaining, fs.drain(EXIT_DRAIN_ROUNDS, EXIT_UPLOAD_ATTEMPTS))
                    .await
                    .unwrap_or(1)
            })
        };

        if unflushed > 0 {
            log::warn!(
                "mount: quit before everything was uploaded; the unsent copies are kept in {}",
                staging_root.display()
            );
        } else {
            let _ = std::fs::remove_dir_all(&staging_root);
        }
    }
}

/// True when either path contains the other, or they are the same path.
fn paths_conflict(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches(['/', '\\']);
    let b = b.trim_end_matches(['/', '\\']);
    a.eq_ignore_ascii_case(b) || is_inside(a, b) || is_inside(b, a)
}

/// True when `inner` sits below `outer` in the directory tree.
fn is_inside(inner: &str, outer: &str) -> bool {
    if outer.is_empty() || inner.len() <= outer.len() {
        return false;
    }
    if !inner.as_bytes()[..outer.len()].eq_ignore_ascii_case(outer.as_bytes()) {
        return false;
    }
    matches!(inner.as_bytes().get(outer.len()), Some(b'/') | Some(b'\\'))
}

/// Runs `argv` to completion, killing it if it outlives `timeout`. Returns true
/// only on a clean exit.
///
/// A process wedged in uninterruptible I/O — exactly what a dead NFS mount
/// causes — may not die even after `kill`, so the reap is bounded too and the
/// child is detached rather than blocking the caller forever.
fn run_bounded(argv: &[String], timeout: Duration) -> bool {
    let Ok(mut child) = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            reap_briefly(&mut child);
            return false;
        }
        std::thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

/// Polls a killed child for a short while so the common case is reaped, then
/// gives up. Leaving a zombie behind is fine: the process is exiting and init
/// will collect it.
fn reap_briefly(child: &mut std::process::Child) {
    let deadline = Instant::now() + REAP_TIMEOUT;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => std::thread::sleep(PROCESS_POLL_INTERVAL),
        }
    }
    log::warn!(
        "mount: pid {} did not exit after kill; detaching",
        child.id()
    );
}

/// Releases a mountpoint left behind by a previous crashed session.
///
/// Only reached once [`decide_target_action`] has established that the path is
/// unreadable in a way a dead mount explains — never for a directory that merely
/// has files in it. Only the primary unmount is tried.
#[cfg(not(windows))]
fn clear_stale_mount(platform: MountPlatform, target: &str) {
    if let Some(argv) = platform::umount_argvs(platform, target).first() {
        if run_bounded(argv, STALE_UMOUNT_TIMEOUT) {
            log::info!("mount: released a leftover mount at \"{}\"", target);
        }
    }
}

/// Validates the user-supplied mount location for the host platform.
#[cfg(windows)]
fn normalize_target(local_path: &str) -> Result<String, String> {
    platform::normalize_drive_spec(local_path).ok_or_else(|| {
        format!(
            "Windows NFS mounts require a drive letter, not a folder. \"{}\" is not a drive letter — use something like Z: instead.",
            local_path
        )
    })
}

#[cfg(not(windows))]
fn normalize_target(local_path: &str) -> Result<String, String> {
    let trimmed = local_path.trim();
    if trimmed.is_empty() {
        return Err("Mount path is empty".to_string());
    }
    if !Path::new(trimmed).is_absolute() {
        return Err(format!("Mount path \"{}\" must be absolute", trimmed));
    }

    let normalized = trimmed.trim_end_matches('/');
    if normalized.is_empty() {
        return Err("Refusing to mount over the filesystem root".to_string());
    }
    Ok(normalized.to_string())
}

/// Prepares the mount location, returning whether a directory was created (so a
/// failed mount can clean up after itself).
#[cfg(windows)]
fn prepare_target(_platform: MountPlatform, target: &str) -> Result<bool, String> {
    // An in-use drive letter may well be a real volume of the user's, so it is
    // refused rather than unmounted.
    if Path::new(&format!("{}\\", target)).exists() {
        return Err(format!("Drive {} is already in use", target));
    }
    Ok(false)
}

#[cfg(not(windows))]
fn prepare_target(platform: MountPlatform, target: &str) -> Result<bool, String> {
    let path = Path::new(target);
    let created = !path.exists();

    if !created {
        let probe = probe_dir(path);
        match decide_target_action(&probe) {
            TargetAction::Use => {}
            TargetAction::Reject => return Err(unusable_target_message(target, &probe)),
            TargetAction::ClearStaleMount => {
                clear_stale_mount(platform, target);
                let probe = probe_dir(path);
                if decide_target_action(&probe) != TargetAction::Use {
                    return Err(unusable_target_message(target, &probe));
                }
            }
        }
    }

    std::fs::create_dir_all(path)
        .map_err(|e| format!("Failed to create mount folder \"{}\": {}", target, e))?;

    if !path.is_dir() {
        return Err(format!("\"{}\" is not a folder", target));
    }

    Ok(created)
}

/// What to do with a mount target that already exists.
#[cfg(not(windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetAction {
    /// Empty and readable — mount straight onto it.
    Use,
    /// Unreadable in a way only a dead mount explains; try to release it.
    ClearStaleMount,
    /// Occupied by real content, or unreadable for some other reason.
    Reject,
}

/// `Ok(true)` when the directory is empty, `Ok(false)` when it has entries.
#[cfg(not(windows))]
fn probe_dir(path: &Path) -> std::io::Result<bool> {
    match std::fs::read_dir(path)?.next() {
        None => Ok(true),
        Some(Ok(_)) => Ok(false),
        Some(Err(e)) => Err(e),
    }
}

/// Decides what a directory probe justifies doing to the user's path.
///
/// Unmounting is destructive to whatever is mounted there, so it is only ever
/// justified by an error that a dead mount explains. A directory that simply has
/// files in it belongs to the user — it might be an unrelated volume they typed
/// by mistake — and must be refused, never unmounted.
#[cfg(not(windows))]
fn decide_target_action(probe: &std::io::Result<bool>) -> TargetAction {
    match probe {
        Ok(true) => TargetAction::Use,
        Ok(false) => TargetAction::Reject,
        Err(e) if looks_like_dead_mount(e) => TargetAction::ClearStaleMount,
        Err(_) => TargetAction::Reject,
    }
}

/// True for the errors a server-less NFS mountpoint produces.
#[cfg(not(windows))]
fn looks_like_dead_mount(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::NotConnected {
        return true;
    }
    matches!(
        error.raw_os_error(),
        Some(libc::ENOTCONN) | Some(libc::ESTALE) | Some(libc::EIO)
    )
}

#[cfg(not(windows))]
fn unusable_target_message(target: &str, probe: &std::io::Result<bool>) -> String {
    match probe {
        Ok(_) => format!(
            "Mount folder \"{}\" is not empty. Choose an empty folder.",
            target
        ),
        Err(e) => format!("Failed to read mount folder \"{}\": {}", target, e),
    }
}

/// Why an OS command did not produce an exit status.
enum CommandFailure {
    NotFound,
    TimedOut,
    Io(std::io::Error),
}

/// Runs `argv`, killing the child if it outlives `limit`.
///
/// `kill_on_drop` is what makes the timeout real: when the timeout future is
/// dropped the child goes with it, instead of being left behind still blocking
/// on an unreachable server.
async fn run_with_timeout(
    argv: &[String],
    limit: Duration,
) -> Result<std::process::Output, CommandFailure> {
    let mut command = tokio::process::Command::new(&argv[0]);
    command.args(&argv[1..]).kill_on_drop(true);

    match tokio::time::timeout(limit, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(CommandFailure::NotFound),
        Ok(Err(e)) => Err(CommandFailure::Io(e)),
        Err(_elapsed) => Err(CommandFailure::TimedOut),
    }
}

async fn run_mount_command(
    platform: MountPlatform,
    server_ip: &str,
    port: u16,
    target: &str,
    read_only: bool,
) -> Result<(), String> {
    let argv = platform::mount_argv(platform, server_ip, port, target, read_only);

    match run_with_timeout(&argv, MOUNT_COMMAND_TIMEOUT).await {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("{} exited with {}", argv[0], output.status)
            } else {
                stderr
            };
            Err(mount_failure_message(
                platform, server_ip, port, target, read_only, &detail,
            ))
        }
        // A timeout still carries the manual command, since running it by hand
        // is exactly how the user sees what the mount is waiting on.
        Err(CommandFailure::TimedOut) => Err(mount_failure_message(
            platform,
            server_ip,
            port,
            target,
            read_only,
            &format!(
                "{} timed out after {} seconds",
                argv[0],
                MOUNT_COMMAND_TIMEOUT.as_secs()
            ),
        )),
        Err(CommandFailure::NotFound) => Err(missing_client_message(platform)),
        Err(CommandFailure::Io(e)) => Err(format!("Failed to run {}: {}", argv[0], e)),
    }
}

/// Error text for a failed mount.
///
/// The manual command occupies the final line on its own, unindented, so the UI
/// can lift it into a copyable block. On Linux that line begins with
/// `sudo mount`, which is exactly what the frontend keys its extraction on.
fn mount_failure_message(
    platform: MountPlatform,
    server_ip: &str,
    port: u16,
    target: &str,
    read_only: bool,
    detail: &str,
) -> String {
    format!(
        "Failed to mount at \"{}\": {}\n\nRun this command manually to mount it yourself:\n{}",
        target,
        detail,
        platform::manual_mount_command(platform, server_ip, port, target, read_only)
    )
}

fn missing_client_message(platform: MountPlatform) -> String {
    match platform {
        MountPlatform::Windows => {
            "The Windows NFS client is not available. Enable \"Services for NFS > Client for NFS\" in Windows Features (Windows Pro or Enterprise)."
                .to_string()
        }
        MountPlatform::Linux => {
            "mount.nfs was not found. Install the NFS client package (nfs-utils or nfs-common) and try again."
                .to_string()
        }
        MountPlatform::MacOs => "/sbin/mount_nfs was not found on this system.".to_string(),
    }
}

async fn run_umount_command(platform: MountPlatform, target: &str) -> Result<(), String> {
    let mut last_error = format!("Failed to unmount \"{}\"", target);

    for argv in platform::umount_argvs(platform, target) {
        match run_with_timeout(&argv, UNMOUNT_COMMAND_TIMEOUT).await {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if !stderr.is_empty() {
                    last_error = format!("Failed to unmount \"{}\": {}", target, stderr);
                }
            }
            Err(CommandFailure::TimedOut) => {
                last_error = format!(
                    "Unmounting \"{}\" timed out after {} seconds. Close any open files on it and try again.",
                    target,
                    UNMOUNT_COMMAND_TIMEOUT.as_secs()
                );
            }
            Err(CommandFailure::NotFound) => {
                last_error = format!("{} was not found on this system", argv[0]);
            }
            Err(CommandFailure::Io(e)) => {
                last_error = format!("Failed to run {}: {}", argv[0], e);
            }
        }
    }

    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_conflicts_with_itself() {
        assert!(paths_conflict("/Users/me/mnt", "/Users/me/mnt"));
        assert!(paths_conflict("/Users/me/mnt/", "/Users/me/mnt"));
    }

    #[test]
    fn nesting_inside_an_existing_mount_conflicts() {
        assert!(paths_conflict("/Users/me/mnt/sub", "/Users/me/mnt"));
        assert!(paths_conflict("/Users/me/mnt", "/Users/me/mnt/sub"));
    }

    #[test]
    fn sibling_paths_do_not_conflict() {
        assert!(!paths_conflict("/Users/me/mnt-a", "/Users/me/mnt-b"));
        // A shared string prefix is not a shared path prefix.
        assert!(!paths_conflict("/Users/me/mnt2", "/Users/me/mnt"));
    }

    #[test]
    fn windows_drive_letters_conflict_only_with_themselves() {
        assert!(paths_conflict("Z:", "z:"));
        assert!(!paths_conflict("Z:", "Y:"));
    }

    #[test]
    fn is_inside_requires_a_separator_at_the_boundary() {
        assert!(is_inside("/a/b", "/a"));
        assert!(!is_inside("/ab", "/a"));
        assert!(!is_inside("/a", "/a"));
        assert!(!is_inside("/a", "/a/b"));
    }

    // ---- target classification (C2: never unmount the user's data) ----

    #[cfg(not(windows))]
    #[test]
    fn a_non_empty_directory_is_rejected_and_never_unmounted() {
        // The whole point: `clear_stale_mount` runs only in the ClearStaleMount
        // arm, so proving a populated directory maps to Reject proves no umount
        // is attempted against, say, /Volumes/Backup.
        assert_eq!(decide_target_action(&Ok(false)), TargetAction::Reject);
    }

    #[cfg(not(windows))]
    #[test]
    fn an_empty_directory_is_used_as_is() {
        assert_eq!(decide_target_action(&Ok(true)), TargetAction::Use);
    }

    #[cfg(not(windows))]
    #[test]
    fn only_dead_mount_errors_justify_an_unmount() {
        for errno in [libc::ENOTCONN, libc::ESTALE, libc::EIO] {
            let probe = Err(std::io::Error::from_raw_os_error(errno));
            assert_eq!(
                decide_target_action(&probe),
                TargetAction::ClearStaleMount,
                "errno {} should look like a dead mount",
                errno
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn unrelated_io_errors_are_rejected_rather_than_unmounted() {
        for errno in [libc::EACCES, libc::ENOENT, libc::ENOTDIR, libc::EPERM] {
            let probe = Err(std::io::Error::from_raw_os_error(errno));
            assert_eq!(
                decide_target_action(&probe),
                TargetAction::Reject,
                "errno {} must not trigger an unmount",
                errno
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn the_rejection_message_distinguishes_full_from_unreadable() {
        let full = unusable_target_message("/Users/me/mnt", &Ok(false));
        assert!(full.contains("is not empty"), "{}", full);

        let unreadable = unusable_target_message(
            "/Users/me/mnt",
            &Err(std::io::Error::from_raw_os_error(libc::ENOTCONN)),
        );
        assert!(unreadable.contains("Failed to read"), "{}", unreadable);
    }

    #[cfg(not(windows))]
    #[test]
    fn an_empty_real_directory_probes_as_empty() {
        let dir = std::env::temp_dir().join(format!("r2-mount-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create probe dir");
        assert_eq!(probe_dir(&dir).ok(), Some(true));

        std::fs::write(dir.join("file.txt"), b"x").expect("write probe file");
        assert_eq!(probe_dir(&dir).ok(), Some(false));
        assert_eq!(decide_target_action(&probe_dir(&dir)), TargetAction::Reject);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn mount_targets_must_be_absolute_and_not_the_root() {
        assert!(normalize_target("relative/path").is_err());
        assert!(normalize_target("  ").is_err());
        assert!(normalize_target("/").is_err());
        assert_eq!(
            normalize_target("/Users/me/mnt/").as_deref(),
            Ok("/Users/me/mnt")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_folder_paths_with_a_drive_letter_hint() {
        let error = normalize_target("C:\\Users\\me\\CloudMounts\\photos").unwrap_err();
        assert!(error.contains("drive letter"), "{}", error);
        assert_eq!(normalize_target("r:\\").as_deref(), Ok("R:"));
        assert_eq!(normalize_target("R:").as_deref(), Ok("R:"));
    }

    #[test]
    fn a_fresh_manager_reports_no_mounts() {
        let manager = MountManager::new();
        assert!(manager.list().is_empty());
        assert!(manager.ensure_path_available("/tmp/anything").is_ok());
    }

    #[test]
    fn mount_ids_are_unique() {
        let manager = MountManager::new();
        let first = manager.next_mount_id();
        let second = manager.next_mount_id();
        assert_ne!(first, second);
        assert!(first.starts_with("mnt-"));
    }

    #[test]
    fn a_linux_mount_failure_puts_the_sudo_command_on_its_own_line() {
        let message = mount_failure_message(
            MountPlatform::Linux,
            "127.0.0.1",
            51234,
            "/home/me/CloudMounts/photos",
            false,
            "mount.nfs: failed, reason given by server: No such file or directory",
        );

        // The frontend lifts this line verbatim into a copyable block.
        let command = message
            .lines()
            .find(|line| line.starts_with("sudo mount"))
            .expect("a line starting with \"sudo mount\"");
        assert!(command.contains("127.0.0.1:/"));
        assert!(command.ends_with("/home/me/CloudMounts/photos"));
        assert_eq!(message.lines().last(), Some(command));
    }

    #[test]
    fn a_macos_mount_failure_carries_the_command_without_sudo() {
        let message = mount_failure_message(
            MountPlatform::MacOs,
            "127.0.0.1",
            51234,
            "/Users/me/CloudMounts/photos",
            false,
            "mount_nfs: can't mount",
        );

        assert!(!message.contains("sudo"));
        assert_eq!(
            message
                .lines()
                .last()
                .map(|line| line.starts_with("/sbin/mount_nfs")),
            Some(true)
        );
    }

    #[test]
    fn the_global_manager_is_a_single_instance() {
        assert!(std::ptr::eq(manager(), manager()));
    }

    #[cfg(unix)]
    #[test]
    fn a_bounded_command_gives_up_on_a_process_that_never_exits() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 30".to_string(),
        ];
        let started = Instant::now();
        assert!(!run_bounded(&argv, Duration::from_millis(200)));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "run_bounded must not wait for the child"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_killed_child_does_not_hold_up_the_caller() {
        // SIGTERM is trapped and ignored, so the kill does not reap it promptly;
        // run_bounded must detach instead of blocking on wait().
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "trap '' TERM; sleep 30".to_string(),
        ];
        let started = Instant::now();
        assert!(!run_bounded(&argv, Duration::from_millis(100)));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "a child that ignores the kill must not block the exit path, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn the_exit_budget_bounds_the_whole_teardown_not_one_attempt() {
        // Three fallbacks per mount at 5s each would exceed the budget on its
        // own, which is why unmount_all shares one deadline across them all.
        assert!(EXIT_UNMOUNT_ATTEMPT_TIMEOUT <= EXIT_UNMOUNT_BUDGET);
        assert!(EXIT_UNMOUNT_BUDGET <= Duration::from_secs(10));
        assert!(REAP_TIMEOUT < EXIT_UNMOUNT_ATTEMPT_TIMEOUT);
    }

    #[test]
    fn interactive_commands_are_bounded_more_generously_than_the_exit_path() {
        assert_eq!(MOUNT_COMMAND_TIMEOUT, Duration::from_secs(30));
        assert_eq!(UNMOUNT_COMMAND_TIMEOUT, Duration::from_secs(15));
        assert!(UNMOUNT_COMMAND_TIMEOUT > EXIT_UNMOUNT_ATTEMPT_TIMEOUT);
    }

    #[test]
    fn the_exit_flush_budget_is_bounded_and_separate_from_the_unmount_budget() {
        // Unmounting has to happen before the flush — that is what makes the OS
        // hand over the last writes — so the two budgets are consecutive and
        // both have to be short enough that quitting still feels instant.
        assert_eq!(EXIT_FLUSH_BUDGET, Duration::from_secs(10));
        assert!(EXIT_UNMOUNT_BUDGET + EXIT_FLUSH_BUDGET <= Duration::from_secs(20));
    }

    #[test]
    fn the_exit_path_never_retries_a_failing_upload() {
        // Every retry comes out of a budget shared with the other mounts, so
        // one bad file must not be able to spend it.
        assert_eq!(EXIT_DRAIN_ROUNDS, 1);
        assert_eq!(EXIT_UPLOAD_ATTEMPTS, 1);
        // An interactive unmount has no shared budget and does keep trying.
        assert_eq!(DRAIN_ROUNDS, 3);
    }

    #[test]
    fn a_sweep_removes_empty_leftovers_and_keeps_the_rest() {
        let root = std::env::temp_dir().join(format!("r2-mount-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let empty = root.join("mnt-old-empty");
        let unsent = root.join("mnt-old-unsent");
        let live = root.join("mnt-live");
        for dir in [&empty, &unsent, &live] {
            std::fs::create_dir_all(dir).expect("create staging dir");
        }
        std::fs::write(unsent.join("42"), b"never uploaded").expect("write");
        std::fs::write(live.join("7"), b"in use").expect("write");

        sweep_staging_root(&root, &["mnt-live".to_string()]);

        assert!(!empty.exists(), "an empty leftover is just litter");
        assert!(
            unsent.exists(),
            "a folder with unsent writes is the only copy of them"
        );
        assert!(live.exists(), "a live mount's folder is in use");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_sweep_of_a_missing_root_is_harmless() {
        sweep_staging_root(Path::new("/definitely/not/a/real/staging/root"), &[]);
    }

    #[test]
    fn draining_nothing_costs_nothing() {
        // The exit path runs on the main thread with no async context, so the
        // read-only case must not enter the runtime at all.
        let started = Instant::now();
        drain_on_exit(Vec::new());
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[cfg(unix)]
    #[test]
    fn a_bounded_command_reports_the_exit_status() {
        let ok = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 0".to_string(),
        ];
        let bad = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 1".to_string(),
        ];
        assert!(run_bounded(&ok, Duration::from_secs(5)));
        assert!(!run_bounded(&bad, Duration::from_secs(5)));
    }

    #[test]
    fn a_missing_binary_is_not_a_success() {
        let argv = vec!["definitely-not-a-real-binary-xyz".to_string()];
        assert!(!run_bounded(&argv, Duration::from_secs(1)));
    }
}
