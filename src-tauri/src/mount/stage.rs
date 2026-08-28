//! Write-back staging for writable mounts.
//!
//! NFSv3 has no close, commit or flush hook: the client sends a stream of
//! ≤1 MiB WRITEs and expects each one acknowledged immediately. Uploading per
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
/// Backoff between those attempts.
pub const UPLOAD_RETRY_BACKOFF: Duration = Duration::from_secs(1);

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
        FlushState::Uploading => false,
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
pub fn part_count(size: u64) -> u64 {
    size.div_ceil(PART_SIZE).max(1)
}

/// Byte range of part `index` (zero-based) of a `size`-byte upload.
pub fn part_range(size: u64, index: u64) -> (u64, u64) {
    let start = index * PART_SIZE;
    let end = start.saturating_add(PART_SIZE).min(size);
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
}

impl Stage {
    /// Creates an empty staging file, replacing any leftover from a previous
    /// session.
    pub async fn create(path: PathBuf, key: String, mtime_secs: u32) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
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
        })
    }

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

    pub async fn truncate(&mut self, size: u64) -> std::io::Result<()> {
        self.file.set_len(size).await?;
        self.file.flush().await?;
        self.size = size;
        Ok(())
    }

    /// Records that the content changed and restarts the debounce window.
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
