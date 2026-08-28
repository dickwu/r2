//! Tauri commands for mounting a bucket as a local folder.

use crate::mount::{self, MountInfo, MountProvider, MountRequest};
use crate::providers::{aws, minio};
use crate::r2;
use aws_sdk_s3::Client;
use serde::Deserialize;
use tauri::Manager;

/// Folder under the user's home directory where suggested mount points live.
#[cfg(not(windows))]
const MOUNT_ROOT: &str = "CloudMounts";

/// Folder under the app's cache directory holding one staging folder per mount.
const STAGING_ROOT: &str = "mount-stage";

/// Everything needed to mount one bucket. Commands are stateless: the frontend
/// pushes the full credentials on every call, like the other provider commands.
///
/// The provider-specific fields are optional so a single command can serve all
/// four providers; each one validates what it needs.
#[derive(Deserialize)]
pub struct MountBucketInput {
    pub provider: MountProvider,
    pub account_id: String,
    pub bucket: String,
    /// Where to mount. Blank falls back to [`default_mount_path`].
    pub local_path: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub force_path_style: Option<bool>,
    /// Absent means writable, which is the default a mount is offered as.
    #[serde(default)]
    pub read_only: Option<bool>,
}

/// Hand-written so credentials cannot reach a log line or a panic message.
impl std::fmt::Debug for MountBucketInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MountBucketInput")
            .field("provider", &self.provider)
            .field("account_id", &self.account_id)
            .field("bucket", &self.bucket)
            .field("local_path", &self.local_path)
            .field("access_key_id", &"***")
            .field("secret_access_key", &"***")
            .field("region", &self.region)
            .field("endpoint_url", &self.endpoint_url)
            .field("force_path_style", &self.force_path_style)
            .field("read_only", &self.read_only)
            .finish()
    }
}

/// Splits `https://minio.example.com:9000` into its scheme and host parts.
/// A bare host defaults to https, matching how the account editors treat it.
fn split_endpoint_url(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let (scheme, host) = match url.split_once("://") {
        Some((scheme, host)) => (scheme.trim(), host),
        None => ("https", url),
    };
    let host = host.trim().trim_end_matches('/');
    if scheme.is_empty() || host.is_empty() {
        return None;
    }
    Some((scheme.to_string(), host.to_string()))
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

impl MountBucketInput {
    fn endpoint_parts(&self) -> Option<(String, String)> {
        self.endpoint_url
            .as_deref()
            .and_then(non_empty)
            .and_then(split_endpoint_url)
    }

    /// Builds the S3 client through the same per-provider factory the sync and
    /// listing commands use, so mounts cannot drift from the rest of the app.
    async fn create_client(&self) -> Result<Client, String> {
        match self.provider {
            MountProvider::R2 => r2::create_r2_client(&r2::R2Config {
                account_id: self.account_id.clone(),
                bucket: self.bucket.clone(),
                access_key_id: self.access_key_id.clone(),
                secret_access_key: self.secret_access_key.clone(),
            })
            .await
            .map_err(|e| format!("Failed to create client: {}", e)),

            MountProvider::Aws => {
                let (endpoint_scheme, endpoint_host) = match self.endpoint_parts() {
                    Some((scheme, host)) => (Some(scheme), Some(host)),
                    None => (None, None),
                };

                aws::create_aws_client(&aws::AwsConfig {
                    bucket: self.bucket.clone(),
                    access_key_id: self.access_key_id.clone(),
                    secret_access_key: self.secret_access_key.clone(),
                    region: self
                        .region
                        .as_deref()
                        .and_then(non_empty)
                        .ok_or_else(|| "AWS mounts require a region".to_string())?
                        .to_string(),
                    endpoint_scheme,
                    endpoint_host,
                    force_path_style: self.force_path_style.unwrap_or(false),
                })
                .await
                .map_err(|e| format!("Failed to create client: {}", e))
            }

            // RustFS speaks the MinIO dialect and always uses path-style URLs.
            MountProvider::Minio | MountProvider::Rustfs => {
                let (endpoint_scheme, endpoint_host) = self
                    .endpoint_parts()
                    .ok_or_else(|| "This provider requires an endpoint URL".to_string())?;
                let force_path_style = match self.provider {
                    MountProvider::Rustfs => true,
                    _ => self.force_path_style.unwrap_or(true),
                };

                minio::create_minio_client(&minio::MinioConfig {
                    bucket: self.bucket.clone(),
                    access_key_id: self.access_key_id.clone(),
                    secret_access_key: self.secret_access_key.clone(),
                    endpoint_scheme,
                    endpoint_host,
                    force_path_style,
                })
                .await
                .map_err(|e| format!("Failed to create client: {}", e))
            }
        }
    }
}

/// Directory the per-mount staging folders live under.
///
/// The app cache directory rather than the system temp directory on purpose:
/// a file that was written into a mount but not uploaded before the app quit
/// only exists here, and an OS temp sweep would take it.
fn staging_root(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let cache = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("Failed to resolve the cache directory: {}", e))?;
    Ok(cache.join(STAGING_ROOT))
}

/// Expands a leading `~` against the user's home directory.
fn expand_home(app: &tauri::AppHandle, path: &str) -> Result<String, String> {
    let path = path.trim();
    let Some(rest) = path.strip_prefix('~') else {
        return Ok(path.to_string());
    };

    let home = app
        .path()
        .home_dir()
        .map_err(|e| format!("Failed to resolve home directory: {}", e))?;
    let rest = rest.trim_start_matches(['/', '\\']);
    if rest.is_empty() {
        return Ok(home.to_string_lossy().to_string());
    }
    Ok(home.join(rest).to_string_lossy().to_string())
}

/// Strips anything that cannot appear in a path component.
#[cfg(any(not(windows), test))]
fn sanitize_bucket(bucket: &str) -> String {
    let cleaned: String = bucket
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            other => other,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "bucket".to_string()
    } else {
        cleaned
    }
}

/// Highest free drive letter down to D:, falling back to Z: when every letter
/// is taken. Compiled on all platforms so the scan stays unit-tested.
#[cfg(any(windows, test))]
fn first_free_drive(is_taken: impl Fn(char) -> bool) -> String {
    ('D'..='Z')
        .rev()
        .find(|letter| !is_taken(*letter))
        .map(|letter| format!("{}:", letter))
        .unwrap_or_else(|| "Z:".to_string())
}

/// First drive letter with no existing root. The Windows NFS client mounts to a
/// drive, not to a folder.
#[cfg(windows)]
fn suggest_drive_letter() -> String {
    first_free_drive(|letter| std::path::Path::new(&format!("{}:\\", letter)).exists())
}

#[tauri::command]
pub async fn mount_bucket(
    input: MountBucketInput,
    app: tauri::AppHandle,
) -> Result<MountInfo, String> {
    let local_path = match non_empty(&input.local_path) {
        Some(path) => expand_home(&app, path)?,
        None => default_mount_path_for(&app, &input.bucket)?,
    };

    let client = input.create_client().await?;
    let staging_root = staging_root(&app)?;

    let info = mount::manager()
        .mount(MountRequest {
            provider: input.provider,
            account_id: input.account_id,
            bucket: input.bucket,
            local_path,
            client,
            read_only: input.read_only.unwrap_or(false),
            staging_root,
            app: app.clone(),
        })
        .await?;

    mount::manager().emit_changed(&app);

    Ok(info)
}

#[tauri::command]
pub async fn unmount_bucket(mount_id: String, app: tauri::AppHandle) -> Result<(), String> {
    // Unmounting can fail after the mount has already been unregistered — the
    // OS mount is gone but staged writes were not all uploaded — so the list is
    // broadcast either way, or the sidebar would keep showing a mount that is
    // no longer there.
    let result = mount::manager().unmount(&mount_id).await;
    mount::manager().emit_changed(&app);
    result
}

#[tauri::command]
pub async fn list_mounts() -> Result<Vec<MountInfo>, String> {
    Ok(mount::manager().list())
}

#[tauri::command]
pub async fn default_mount_path(bucket: String, app: tauri::AppHandle) -> Result<String, String> {
    default_mount_path_for(&app, &bucket)
}

/// Suggested mount location: `~/CloudMounts/<bucket>` on macOS and Linux, and a
/// free drive letter on Windows. The folder is not created here — that happens
/// during the mount itself.
fn default_mount_path_for(app: &tauri::AppHandle, bucket: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        let _ = (app, bucket);
        Ok(suggest_drive_letter())
    }

    #[cfg(not(windows))]
    {
        let home = app
            .path()
            .home_dir()
            .map_err(|e| format!("Failed to resolve home directory: {}", e))?;
        Ok(home
            .join(MOUNT_ROOT)
            .join(sanitize_bucket(bucket))
            .to_string_lossy()
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> MountBucketInput {
        serde_json::from_str(json).expect("mount input")
    }

    const R2_INPUT: &str = r#"{
        "provider": "r2",
        "account_id": "acct",
        "bucket": "photos",
        "local_path": "/Users/me/CloudMounts/photos",
        "access_key_id": "key",
        "secret_access_key": "secret"
    }"#;

    #[test]
    fn bucket_names_are_reduced_to_a_single_path_component() {
        assert_eq!(sanitize_bucket("photos"), "photos");
        assert_eq!(sanitize_bucket("my.photos-2024"), "my.photos-2024");
        assert_eq!(sanitize_bucket("a/b"), "a-b");
        assert_eq!(sanitize_bucket("../etc"), "-etc");
        assert_eq!(sanitize_bucket("  "), "bucket");
        assert_eq!(sanitize_bucket(""), "bucket");
    }

    #[test]
    fn provider_discriminators_are_lowercase_strings() {
        let parse = |raw: &str| serde_json::from_str::<MountProvider>(raw).expect("provider");
        assert_eq!(parse("\"r2\""), MountProvider::R2);
        assert_eq!(parse("\"aws\""), MountProvider::Aws);
        assert_eq!(parse("\"minio\""), MountProvider::Minio);
        assert_eq!(parse("\"rustfs\""), MountProvider::Rustfs);
        assert!(serde_json::from_str::<MountProvider>("\"gcs\"").is_err());
    }

    #[test]
    fn provider_specific_fields_are_optional_in_the_ipc_payload() {
        let input = parse(R2_INPUT);

        assert_eq!(input.provider, MountProvider::R2);
        assert_eq!(input.local_path, "/Users/me/CloudMounts/photos");
        assert!(input.region.is_none());
        assert!(input.endpoint_url.is_none());
        assert!(input.force_path_style.is_none());
        assert!(input.endpoint_parts().is_none());
    }

    #[test]
    fn a_mount_is_writable_unless_it_asks_not_to_be() {
        // A payload from before the toggle existed has to keep working, and the
        // mode it lands in is the new default rather than the old behaviour.
        assert!(!parse(R2_INPUT).read_only.unwrap_or(false));

        let explicit = |value: &str| {
            let json = format!(
                r#"{{
                    "provider": "r2",
                    "account_id": "acct",
                    "bucket": "photos",
                    "local_path": "/mnt/photos",
                    "access_key_id": "key",
                    "secret_access_key": "secret",
                    "read_only": {}
                }}"#,
                value
            );
            parse(&json).read_only
        };

        assert_eq!(explicit("true"), Some(true));
        assert_eq!(explicit("false"), Some(false));
        assert_eq!(explicit("null"), None);
    }

    #[test]
    fn an_endpoint_url_splits_into_scheme_and_host() {
        assert_eq!(
            split_endpoint_url("https://minio.example.com:9000"),
            Some(("https".to_string(), "minio.example.com:9000".to_string()))
        );
        assert_eq!(
            split_endpoint_url("http://localhost:9000/"),
            Some(("http".to_string(), "localhost:9000".to_string()))
        );
        // A bare host is assumed to be https, as elsewhere in the app.
        assert_eq!(
            split_endpoint_url("minio.example.com"),
            Some(("https".to_string(), "minio.example.com".to_string()))
        );
        assert_eq!(split_endpoint_url("https://"), None);
        assert_eq!(split_endpoint_url("   "), None);
    }

    #[test]
    fn the_endpoint_url_drives_the_minio_client_config() {
        let input = parse(
            r#"{
                "provider": "rustfs",
                "account_id": "acct",
                "bucket": "photos",
                "local_path": "/Users/me/CloudMounts/photos",
                "access_key_id": "key",
                "secret_access_key": "secret",
                "endpoint_url": "http://rustfs.local:7000"
            }"#,
        );

        assert_eq!(
            input.endpoint_parts(),
            Some(("http".to_string(), "rustfs.local:7000".to_string()))
        );
    }

    #[test]
    fn a_blank_endpoint_url_is_treated_as_absent() {
        let input = parse(
            r#"{
                "provider": "aws",
                "account_id": "acct",
                "bucket": "photos",
                "local_path": "/Users/me/CloudMounts/photos",
                "access_key_id": "key",
                "secret_access_key": "secret",
                "region": "us-east-1",
                "endpoint_url": "  "
            }"#,
        );

        assert!(input.endpoint_parts().is_none());
    }

    #[test]
    fn credentials_never_appear_in_debug_output() {
        let input = parse(
            r#"{
                "provider": "r2",
                "account_id": "acct",
                "bucket": "photos",
                "local_path": "/Users/me/CloudMounts/photos",
                "access_key_id": "AKIAREALKEY",
                "secret_access_key": "s3cr3t-do-not-log",
                "read_only": true
            }"#,
        );

        let rendered = format!("{:?}", input);
        assert!(!rendered.contains("AKIAREALKEY"), "{}", rendered);
        assert!(!rendered.contains("s3cr3t-do-not-log"), "{}", rendered);
        assert!(rendered.contains("***"), "{}", rendered);
        // Non-secret fields stay visible so the struct is still debuggable —
        // including every field added after this test was written.
        assert!(rendered.contains("photos"), "{}", rendered);
        assert!(rendered.contains("read_only"), "{}", rendered);
    }

    #[test]
    fn the_windows_suggestion_is_the_highest_free_drive_letter() {
        assert_eq!(first_free_drive(|_| false), "Z:");
        assert_eq!(first_free_drive(|letter| letter == 'Z'), "Y:");
        assert_eq!(first_free_drive(|letter| letter >= 'X'), "W:");
        // C: and below are never offered, so a fully taken machine falls back.
        assert_eq!(first_free_drive(|_| true), "Z:");
        assert_eq!(first_free_drive(|letter| letter > 'D'), "D:");
    }

    #[test]
    fn a_blank_local_path_falls_back_to_the_default() {
        let input = parse(
            r#"{
                "provider": "r2",
                "account_id": "acct",
                "bucket": "photos",
                "local_path": "",
                "access_key_id": "key",
                "secret_access_key": "secret"
            }"#,
        );

        assert!(non_empty(&input.local_path).is_none());
    }
}
