//! Tauri commands module
//!
//! This module contains all Tauri commands split into logical submodules:
//! - `r2_commands`: R2 API operations (list, delete, move, rename)
//! - `file_cache`: File caching operations (store, search, directory tree)

mod aws_commands;
mod cache_events;
pub(crate) mod delete_cache;
mod file_cache;
mod minio_commands;
mod r2_commands;
mod rustfs_commands;
pub(crate) mod upload_cache;

// Re-export all commands
pub use aws_commands::*;
pub use file_cache::*;
pub use minio_commands::*;
pub use r2_commands::*;
pub use rustfs_commands::*;
