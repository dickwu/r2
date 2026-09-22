//! Process-wide accounting of bytes owned by storage operations.
//!
//! These are owned logical sizes and explicit in-flight reservations, not RSS
//! or allocated filesystem blocks. SDK
//! internals and files not opened by this process are deliberately not claimed.
//! Admission limits remain enforced by their owning cache/stage/relay budgets.
use serde::Serialize;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub enum ResourceKind {
    Stage,
    Snapshot,
    Wal,
    RelaySpool,
    RelayBuffer,
    ReadCache,
    ResponseBody,
}

const KINDS: usize = 7;
#[derive(Debug, Default)]
struct Usage {
    live: AtomicU64,
    peak: AtomicU64,
}
static USAGE: [Usage; KINDS] = [const {
    Usage {
        live: AtomicU64::new(0),
        peak: AtomicU64::new(0),
    }
}; KINDS];

/// Follows the allocation/file owner, including early return and cancellation.
/// It is intentionally not Clone, preventing duplicate accounting ownership.
#[derive(Debug)]
pub struct ByteLease {
    usage: &'static Usage,
    bytes: u64,
}

impl ByteLease {
    pub fn new(kind: ResourceKind, bytes: u64) -> Self {
        Self::with_usage(&USAGE[kind as usize], bytes)
    }

    fn with_usage(usage: &'static Usage, bytes: u64) -> Self {
        let mut lease = Self { usage, bytes: 0 };
        lease.resize(bytes);
        lease
    }

    pub fn resize(&mut self, bytes: u64) {
        if bytes > self.bytes {
            let delta = bytes - self.bytes;
            let total = self.usage.live.fetch_add(delta, Ordering::Relaxed) + delta;
            self.usage.peak.fetch_max(total, Ordering::Relaxed);
        } else {
            self.usage
                .live
                .fetch_sub(self.bytes - bytes, Ordering::Relaxed);
        }
        self.bytes = bytes;
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for ByteLease {
    fn drop(&mut self) {
        self.resize(0);
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ResourceSnapshot {
    pub stage_bytes: u64,
    pub snapshot_bytes: u64,
    pub wal_bytes: u64,
    pub relay_spool_reserved_bytes: u64,
    pub relay_buffer_reserved_bytes: u64,
    pub read_cache_bytes: u64,
    pub response_body_reserved_bytes: u64,
    pub disk_growth_reserved_bytes: u64,
    /// Same ordering as the named fields. Each peak belongs to its own class;
    /// summing peaks would not be a simultaneous process high-water mark.
    pub peak_bytes_by_kind: [u64; KINDS],
}

pub fn snapshot() -> ResourceSnapshot {
    let read = |kind: ResourceKind| USAGE[kind as usize].live.load(Ordering::Relaxed);
    ResourceSnapshot {
        stage_bytes: read(ResourceKind::Stage),
        snapshot_bytes: read(ResourceKind::Snapshot),
        wal_bytes: read(ResourceKind::Wal),
        relay_spool_reserved_bytes: read(ResourceKind::RelaySpool),
        relay_buffer_reserved_bytes: read(ResourceKind::RelayBuffer),
        read_cache_bytes: read(ResourceKind::ReadCache),
        response_body_reserved_bytes: read(ResourceKind::ResponseBody),
        disk_growth_reserved_bytes: DISK_GROWTH.load(Ordering::Relaxed),
        peak_bytes_by_kind: std::array::from_fn(|index| USAGE[index].peak.load(Ordering::Relaxed)),
    }
}

static DISK_GROWTH: AtomicU64 = AtomicU64::new(0);
static DISK_POOLS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

/// Reserves future physical growth until the write finishes (or fails). Free
/// space is sampled under the same lock as admission, preventing independently
/// admitted stages/snapshots/spools from promising the same free bytes.
/// Retained files already consume filesystem free space and are not reserved
/// again. This is separate from retained-data quotas and does not delete data.
pub struct DiskLease {
    volume: String,
    bytes: u64,
}

impl DiskLease {
    pub fn reserve(
        path: &Path,
        additional: u64,
        available: impl FnOnce() -> io::Result<u64>,
    ) -> io::Result<Self> {
        Self::reserve_volume(volume_key(path)?, additional, available)
    }

    fn reserve_volume(
        volume: String,
        additional: u64,
        available: impl FnOnce() -> io::Result<u64>,
    ) -> io::Result<Self> {
        let mut pools = DISK_POOLS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let pending = pools.get(&volume).copied().unwrap_or(0);
        let proposed = pending
            .checked_add(additional)
            .ok_or_else(|| io::Error::other("Disk reservation overflow"))?;
        if proposed > available()? {
            return Err(io::Error::other(
                "Storage writes already reserved the available disk space",
            ));
        }
        pools.insert(volume.clone(), proposed);
        DISK_GROWTH.fetch_add(additional, Ordering::Relaxed);
        Ok(Self {
            volume,
            bytes: additional,
        })
    }
}

impl Drop for DiskLease {
    fn drop(&mut self) {
        if let Some(pools) = DISK_POOLS.get() {
            let mut pools = pools.lock().unwrap_or_else(|e| e.into_inner());
            let remaining = pools
                .get(&self.volume)
                .copied()
                .unwrap_or(0)
                .saturating_sub(self.bytes);
            if remaining == 0 {
                pools.remove(&self.volume);
            } else {
                pools.insert(self.volume.clone(), remaining);
            }
        }
        DISK_GROWTH.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

fn existing_probe_path(path: &Path) -> &Path {
    if path.exists() {
        path
    } else {
        path.parent().unwrap_or(path)
    }
}

#[cfg(unix)]
fn volume_key(path: &Path) -> io::Result<String> {
    use std::os::unix::fs::MetadataExt;
    Ok(format!(
        "device:{}",
        std::fs::metadata(existing_probe_path(path))?.dev()
    ))
}

#[cfg(not(unix))]
fn volume_key(path: &Path) -> io::Result<String> {
    // Canonicalization also resolves reparse points/mounted-volume aliases
    // before deriving the volume prefix. No path or identity is logged.
    let canonical = std::fs::canonicalize(existing_probe_path(path))?;
    Ok(canonical
        .components()
        .next()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_resize_and_drop_release_only_owned_bytes() {
        static TEST_USAGE: Usage = Usage {
            live: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        };
        {
            let mut lease = ByteLease::with_usage(&TEST_USAGE, 10);
            let other = ByteLease::with_usage(&TEST_USAGE, 3);
            lease.resize(5);
            assert_eq!(lease.bytes(), 5);
            assert_eq!(TEST_USAGE.live.load(Ordering::Relaxed), 8);
            drop(other);
            assert_eq!(TEST_USAGE.live.load(Ordering::Relaxed), 5);
        }
        assert_eq!(TEST_USAGE.live.load(Ordering::Relaxed), 0);
        assert_eq!(TEST_USAGE.peak.load(Ordering::Relaxed), 13);
    }

    #[test]
    fn disk_growth_is_shared_across_independent_writers_and_released_on_failure() {
        let volume = "disk-lease-test".to_owned();
        let first = DiskLease::reserve_volume(volume.clone(), 60, || Ok(100)).unwrap();
        assert!(DiskLease::reserve_volume(volume.clone(), 50, || Ok(100)).is_err());
        let second = DiskLease::reserve_volume(volume.clone(), 40, || Ok(100)).unwrap();
        drop(first);
        // The first writer used 60 physical bytes; its now-released future
        // growth does not make the already allocated space available again.
        assert!(DiskLease::reserve_volume(volume.clone(), 1, || Ok(40)).is_err());
        drop(second);
        assert!(DiskLease::reserve_volume(volume, 40, || Ok(40)).is_ok());
    }

    #[test]
    fn disk_probe_failure_does_not_install_a_reservation() {
        let volume = "disk-probe-error-test".to_owned();
        assert!(
            DiskLease::reserve_volume(volume.clone(), 100, || Err(io::Error::other("offline")))
                .is_err()
        );
        assert!(DiskLease::reserve_volume(volume, 100, || Ok(100)).is_ok());
    }
}
