//! Mount an S3-compatible bucket as a folder on the local machine.
//!
//! The app starts a small NFSv3 server on `127.0.0.1` backed by the bucket and
//! then asks the operating system's built-in NFS client to mount it. That keeps
//! the feature driver-free on macOS, Linux and Windows — no macFUSE kernel
//! extension and no WinFsp installer.
//!
//! A mount is writable unless it was asked for read-only. Writes land in a
//! local staging file first and are uploaded once the client stops writing, so
//! the bucket never sees a half-copied object.
//!
//! - `nfs_fs`: the bucket filesystem exposed over NFS
//! - `read_cache`: chunked read-ahead cache behind the read path
//! - `stage`: the write-back cache behind a writable mount
//! - `progress`: real-time transfer events for the app's transfer dock
//! - `manager`: mount lifecycle, drain-on-unmount, and the OS mount calls
//! - `platform`: per-OS command construction

mod manager;
mod nfs_fs;
mod platform;
mod progress;
mod read_cache;
mod stage;

pub use manager::{manager, MountInfo, MountProvider, MountRequest};
