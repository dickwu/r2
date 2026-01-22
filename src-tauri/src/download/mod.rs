//! File download module with streaming, progress tracking, and database persistence
//!
//! Provides download functionality for R2 objects with:
//! - Streaming downloads to avoid memory issues with large files
//! - Progress tracking via Tauri events
//! - Database persistence for resume after app restart
//! - Pause/Resume/Cancel support
//! - Backend-managed download queue with concurrency control

pub mod commands;
mod types;
mod worker;
