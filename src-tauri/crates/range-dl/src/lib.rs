//! range-dl: Multi-threaded HTTP download engine with Range-based parallel chunk acceleration.
//!
//! Splits large files into parallel HTTP Range requests for faster downloads.
//! Provider-agnostic: takes a URL provider callback, doesn't know about S3/R2/MinIO.
//!
//! # Features
//! - Parallel chunk downloads with per-chunk file handles (no cursor contention)
//! - Auto-tuning: starts with 2 chunks, doubles based on measured throughput
//! - Pause/Resume with per-chunk byte-level persistence
//! - Cancel with .part file cleanup
//! - Per-chunk retry with exponential backoff and error classification
//! - Backend-side progress aggregation (one event per file per 200ms)
//! - .download_meta JSON sidecar for recovery

mod chunk;
pub mod engine;
pub mod meta;
pub mod types;

pub use engine::RangeDownloader;
pub use types::{
    classify_error, ChunkEvent, ChunkProgress, ChunkState, ChunkStatus, DownloadControl,
    DownloadTarget, ErrorKind, RangeDownloadConfig, UrlProvider,
};
