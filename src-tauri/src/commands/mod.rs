//! Tauri commands module
//!
//! This module contains all Tauri commands split into logical submodules:
//! - `r2_commands`: R2 API operations (list, delete, move, rename)
//! - `file_cache`: File caching operations (store, search, directory tree)

mod file_cache;
mod r2_commands;
mod aws_commands;
mod minio_commands;
mod rustfs_commands;

// Re-export all commands
pub use file_cache::*;
pub use r2_commands::*;
pub use aws_commands::*;
pub use minio_commands::*;
pub use rustfs_commands::*;