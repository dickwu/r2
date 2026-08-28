//! Real-time progress events for a mount's background transfers.
//!
//! The slow work a mount does is invisible from the file manager: a file
//! copied in sits in the staging area, waits out the write debounce, and only
//! then uploads; opening a large object for editing first downloads all of it.
//! The OS shows nothing during any of that. Every such transfer is therefore
//! reported over the `mount-transfer` event so the app's transfer dock can
//! show a live progress bar for it.
//!
//! One transfer id names one file's transfer in one direction —
//! `"<mount>:<fileid>:up"` — so repeated uploads of the same file update one
//! row instead of piling up new ones. Progress emissions are throttled;
//! `waiting`, `done`, `error` and `removed` transitions always go out.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::Emitter;

use crate::transfer_progress::{SpeedWindow, ThrottleGate};

pub const EVENT: &str = "mount-transfer";

/// Ceiling on progress emissions per transfer, so a fast upload cannot flood
/// the IPC bridge. Terminal transitions bypass it.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Which way the bytes are going, as the frontend sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferKind {
    /// Staged local content moving to the bucket.
    Upload,
    /// Object content being staged locally before an in-place edit.
    Download,
}

/// Where the transfer is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferState {
    /// Dirty and waiting out the write debounce; no bytes moving yet.
    Waiting,
    /// Bytes are moving.
    Active,
    Done,
    Error,
    /// The transfer stopped mattering — the file was deleted or its stage
    /// discarded — and the row should disappear rather than claim completion.
    Removed,
}

/// Payload of one `mount-transfer` event.
#[derive(Debug, Clone, Serialize)]
pub struct MountTransferEvent {
    pub mount_id: String,
    pub bucket: String,
    pub transfer_id: String,
    pub key: String,
    pub kind: TransferKind,
    pub state: TransferState,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Smoothed bytes/sec while active, 0 otherwise.
    pub speed: f64,
    pub error: Option<String>,
}

/// Per-mount emitter handle, cheap to clone into upload tasks.
#[derive(Clone)]
pub struct MountProgress {
    app: tauri::AppHandle,
    mount_id: String,
    bucket: String,
}

/// The suffix that makes a file's upload and download ids distinct.
fn direction_tag(kind: TransferKind) -> &'static str {
    match kind {
        TransferKind::Upload => "up",
        TransferKind::Download => "down",
    }
}

/// Stable id for one file's transfers in one direction.
pub fn transfer_id(mount_id: &str, fileid: u64, kind: TransferKind) -> String {
    format!("{}:{}:{}", mount_id, fileid, direction_tag(kind))
}

impl MountProgress {
    pub fn new(app: tauri::AppHandle, mount_id: String, bucket: String) -> Self {
        Self {
            app,
            mount_id,
            bucket,
        }
    }

    fn emit(&self, fileid: u64, key: &str, kind: TransferKind, frame: Frame) {
        let _ = self.app.emit(
            EVENT,
            MountTransferEvent {
                mount_id: self.mount_id.clone(),
                bucket: self.bucket.clone(),
                transfer_id: transfer_id(&self.mount_id, fileid, kind),
                key: key.to_string(),
                kind,
                state: frame.state,
                bytes_done: frame.bytes_done,
                bytes_total: frame.bytes_total,
                speed: frame.speed,
                error: frame.error,
            },
        );
    }

    /// A file's staged content became dirty: an upload will follow once the
    /// writes go quiet. `size` is the staged size so far.
    pub fn waiting(&self, fileid: u64, key: &str, size: u64) {
        self.emit(
            fileid,
            key,
            TransferKind::Upload,
            Frame {
                state: TransferState::Waiting,
                bytes_done: 0,
                bytes_total: size,
                speed: 0.0,
                error: None,
            },
        );
    }

    /// The file stopped having a transfer to report — deleted mid-copy, or its
    /// stage was discarded — so any row it had should go away.
    pub fn removed(&self, fileid: u64, key: &str) {
        self.emit(
            fileid,
            key,
            TransferKind::Upload,
            Frame {
                state: TransferState::Removed,
                bytes_done: 0,
                bytes_total: 0,
                speed: 0.0,
                error: None,
            },
        );
    }

    /// Starts tracking one transfer and announces it as active.
    pub fn track(
        &self,
        fileid: u64,
        key: &str,
        kind: TransferKind,
        bytes_total: u64,
    ) -> TransferTracker {
        let tracker = TransferTracker {
            progress: self.clone(),
            fileid,
            key: key.to_string(),
            kind,
            bytes_total,
            bytes_done: AtomicU64::new(0),
            window: SpeedWindow::new(),
            gate: ThrottleGate::new(PROGRESS_INTERVAL),
        };
        tracker.progress.emit(
            fileid,
            key,
            kind,
            Frame {
                state: TransferState::Active,
                bytes_done: 0,
                bytes_total,
                speed: 0.0,
                error: None,
            },
        );
        tracker
    }
}

/// The per-event fields of one emission; the identity fields come from the
/// [`MountProgress`] handle.
struct Frame {
    state: TransferState,
    bytes_done: u64,
    bytes_total: u64,
    speed: f64,
    error: Option<String>,
}

/// Progress state for one transfer in flight. Concurrent part uploads all add
/// into the same counter, so it is shareable by reference across tasks.
pub struct TransferTracker {
    progress: MountProgress,
    fileid: u64,
    key: String,
    kind: TransferKind,
    bytes_total: u64,
    bytes_done: AtomicU64,
    window: SpeedWindow,
    gate: ThrottleGate,
}

impl TransferTracker {
    /// Adds transferred bytes and emits a throttled progress update.
    pub fn add(&self, bytes: u64) {
        let done = self
            .bytes_done
            .fetch_add(bytes, Ordering::Relaxed)
            .saturating_add(bytes);
        self.report(done);
    }

    /// Sets the cumulative transferred byte count, for loops that already
    /// track their own offset.
    pub fn set(&self, bytes: u64) {
        self.bytes_done.store(bytes, Ordering::Relaxed);
        self.report(bytes);
    }

    fn report(&self, done: u64) {
        let speed = self.window.sample(done);
        if !self.gate.try_pass() {
            return;
        }
        self.progress.emit(
            self.fileid,
            &self.key,
            self.kind,
            Frame {
                state: TransferState::Active,
                bytes_done: done.min(self.bytes_total),
                bytes_total: self.bytes_total,
                speed,
                error: None,
            },
        );
    }

    /// A retry is starting over from zero — without the reset the bar would
    /// stick at wherever the failed attempt died.
    pub fn restart(&self) {
        self.bytes_done.store(0, Ordering::Relaxed);
        self.progress.emit(
            self.fileid,
            &self.key,
            self.kind,
            Frame {
                state: TransferState::Active,
                bytes_done: 0,
                bytes_total: self.bytes_total,
                speed: 0.0,
                error: None,
            },
        );
    }

    pub fn done(&self) {
        self.progress.emit(
            self.fileid,
            &self.key,
            self.kind,
            Frame {
                state: TransferState::Done,
                bytes_done: self.bytes_total,
                bytes_total: self.bytes_total,
                speed: 0.0,
                error: None,
            },
        );
    }

    pub fn failed(&self, error: &str) {
        self.progress.emit(
            self.fileid,
            &self.key,
            self.kind,
            Frame {
                state: TransferState::Error,
                bytes_done: self
                    .bytes_done
                    .load(Ordering::Relaxed)
                    .min(self.bytes_total),
                bytes_total: self.bytes_total,
                speed: 0.0,
                error: Some(error.to_string()),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_ids_separate_files_and_directions() {
        let up = transfer_id("mnt-1", 42, TransferKind::Upload);
        let down = transfer_id("mnt-1", 42, TransferKind::Download);
        let other = transfer_id("mnt-1", 43, TransferKind::Upload);
        assert_eq!(up, "mnt-1:42:up");
        assert_ne!(up, down);
        assert_ne!(up, other);
        // The same file uploading twice maps onto one row.
        assert_eq!(up, transfer_id("mnt-1", 42, TransferKind::Upload));
    }

    #[test]
    fn the_wire_format_is_lowercase_snake_case() {
        // The frontend matches these strings; the serde rename is the contract.
        assert_eq!(
            serde_json::to_string(&TransferKind::Upload).expect("json"),
            "\"upload\""
        );
        assert_eq!(
            serde_json::to_string(&TransferState::Waiting).expect("json"),
            "\"waiting\""
        );
        assert_eq!(
            serde_json::to_string(&TransferState::Removed).expect("json"),
            "\"removed\""
        );

        let event = MountTransferEvent {
            mount_id: "mnt-1".into(),
            bucket: "photos".into(),
            transfer_id: "mnt-1:42:up".into(),
            key: "a/b.txt".into(),
            kind: TransferKind::Upload,
            state: TransferState::Active,
            bytes_done: 1,
            bytes_total: 2,
            speed: 0.0,
            error: None,
        };
        let json = serde_json::to_string(&event).expect("json");
        assert!(json.contains("\"bytes_done\":1"), "{}", json);
        assert!(json.contains("\"transfer_id\""), "{}", json);
    }
}
