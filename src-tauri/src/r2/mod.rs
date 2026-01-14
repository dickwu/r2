//! R2 module - Cloudflare R2 storage operations
//!
//! This module is organized into submodules:
//! - `types`: Core types and client creation
//! - `list`: List operations (buckets, objects)
//! - `objects`: Object operations (delete, copy, rename)
//! - `upload`: Upload operations (simple, multipart)
//! - `presigned`: Presigned URL generation
//! - `commands`: Tauri commands

pub mod commands;
mod list;
mod objects;
mod presigned;
mod types;
mod upload;

// Re-export types
pub use types::{ListObjectsResult, R2Bucket, R2Config, R2Object};

// Re-export list operations
pub use list::{list_all_objects_recursive, list_buckets, list_folder_objects, list_objects};

// Re-export object operations
pub use objects::{delete_object, delete_objects, rename_object};

// Re-export presigned URL
pub use presigned::generate_presigned_url;

// Re-export upload operations
pub use upload::upload_content;
