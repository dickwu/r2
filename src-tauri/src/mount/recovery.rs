//! Durable mount identity and user-visible recovery inventory. No credentials
//! are persisted; resuming requires a current account with the same namespace.

use super::manager::{manager, MountProvider};
use super::stage::{self, StageRecovery};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct MountManifest {
    version: u32,
    provider: MountProvider,
    account_id: String,
    bucket: String,
    namespace_id: String,
}

#[derive(Serialize)]
pub struct MountRecovery {
    pub recovery_id: String,
    pub provider: Option<MountProvider>,
    pub account_id: Option<String>,
    pub bucket: Option<String>,
    pub namespace_id: Option<String>,
    pub path: String,
    pub files: Vec<StageRecovery>,
    pub error: Option<String>,
    pub active: bool,
}

pub fn resolve_recovery_dir(root: &Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("Invalid recovery identifier".to_string());
    }
    let path = root.join(id);
    let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Recovery folder must be a real directory".to_string());
    }
    Ok(path)
}

pub fn save_mount_manifest(
    root: &Path,
    provider: MountProvider,
    account_id: &str,
    bucket: &str,
    namespace_id: &str,
) -> Result<(), String> {
    use std::io::Write;
    let manifest = MountManifest {
        version: 1,
        provider,
        account_id: account_id.to_string(),
        bucket: bucket.to_string(),
        namespace_id: namespace_id.to_string(),
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    let temporary = root.join("mount.json.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(temporary, root.join("mount.json")).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    std::fs::File::open(root)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn load_manifest(root: &Path) -> Result<MountManifest, String> {
    let file = root.join("mount.json");
    let metadata = std::fs::symlink_metadata(&file)
        .map_err(|e| format!("Mount identity is unavailable: {e}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 {
        return Err("Invalid mount identity manifest".to_string());
    }
    let manifest: MountManifest =
        serde_json::from_slice(&std::fs::read(&file).map_err(|e| e.to_string())?)
            .map_err(|e| format!("Invalid mount identity: {e}"))?;
    if manifest.version != 1 {
        return Err("Unsupported mount recovery version".to_string());
    }
    Ok(manifest)
}

pub fn validate_identity(
    root: &Path,
    provider: MountProvider,
    account_id: &str,
    bucket: &str,
    namespace_id: &str,
) -> Result<(), String> {
    let manifest = load_manifest(root)?;
    if manifest.provider != provider
        || manifest.account_id != account_id
        || manifest.bucket != bucket
        || manifest.namespace_id != namespace_id
    {
        return Err("The saved uploads belong to a different storage namespace; export them or select the original account".to_string());
    }
    Ok(())
}

pub async fn list_recoveries(root: &Path, id_prefix: &str) -> Result<Vec<MountRecovery>, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut recoveries = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let path = entry.path();
        let manifest = load_manifest(&path);
        let recovery = stage::recovery_entries(&path).await;
        let mut error = manifest.as_ref().err().cloned();
        let mut files = match recovery {
            Ok(files) => files,
            Err(stage_error) => {
                error = Some(stage_error);
                Vec::new()
            }
        };
        match super::nfs_fs::S3NfsFs::pending_operations(&path).await {
            Ok(operations) => files.extend(operations),
            Err(reason) => error = Some(reason),
        }
        // Legacy/invalid directories are visible even without usable manifests.
        if files.is_empty() && error.is_none() {
            continue;
        }
        let manifest = manifest.ok();
        recoveries.push(MountRecovery {
            recovery_id: format!("{id_prefix}{}", entry.file_name().to_string_lossy()),
            provider: manifest.as_ref().map(|m| m.provider),
            account_id: manifest.as_ref().map(|m| m.account_id.clone()),
            bucket: manifest.as_ref().map(|m| m.bucket.clone()),
            namespace_id: manifest.map(|m| m.namespace_id),
            path: path.to_string_lossy().to_string(),
            files,
            error,
            active: manager().staging_is_active(&path),
        });
    }
    recoveries.sort_by(|a, b| a.recovery_id.cmp(&b.recovery_id));
    Ok(recoveries)
}

/// Copies into a new folder and refuses links or overwrites. The saved bytes
/// remain untouched if an export fails midway.
pub fn export_recovery(source: &Path, destination: &Path) -> Result<PathBuf, String> {
    let source = source.canonicalize().map_err(|e| e.to_string())?;
    let destination = destination.canonicalize().map_err(|e| e.to_string())?;
    if destination.starts_with(&source) {
        return Err("Export must be outside the recovery folder".to_string());
    }
    let name = source
        .file_name()
        .ok_or("Invalid recovery folder")?
        .to_string_lossy();
    let target = destination.join(format!("r2-recovery-{name}"));
    std::fs::create_dir(&target)
        .map_err(|e| format!("Choose a folder without an existing export: {e}"))?;
    copy_directory(&source, &target)?;
    Ok(target)
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            std::fs::create_dir(&target).map_err(|e| e.to_string())?;
            copy_directory(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).map_err(|e| e.to_string())?;
            super::stage_commit::sync_open_options()
                .open(&target)
                .and_then(|file| file.sync_all())
                .map_err(|e| e.to_string())?;
        } else {
            return Err("Recovery contains a link or unsupported file; export stopped".to_string());
        }
    }
    #[cfg(unix)]
    std::fs::File::open(destination)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_ids_cannot_escape_the_managed_root() {
        for id in ["", "..", "../outside", "/tmp/other", "a/b", "a\\b"] {
            assert!(resolve_recovery_dir(Path::new("/tmp"), id).is_err());
        }
    }
    #[test]
    fn recovery_requires_exact_saved_namespace() {
        let root = std::env::temp_dir().join(format!("r2-recovery-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        save_mount_manifest(
            &root,
            MountProvider::Minio,
            "account",
            "bucket",
            "endpoint-a",
        )
        .unwrap();
        assert!(validate_identity(
            &root,
            MountProvider::Minio,
            "account",
            "bucket",
            "endpoint-a"
        )
        .is_ok());
        assert!(validate_identity(
            &root,
            MountProvider::Minio,
            "account",
            "bucket",
            "endpoint-b"
        )
        .is_err());
        assert!(validate_identity(
            &root,
            MountProvider::Minio,
            "account",
            "other",
            "endpoint-a"
        )
        .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
