//! Backend identity and recovery decisions used by the real move worker.
use super::config::MoveConfig;
use crate::db::move_sessions::{MoveJournal, SourceIdentity};
use crate::providers::operation::{
    execute as execute_operation, AttemptError, OperationContext, OperationError, OperationKind,
};
use crate::providers::s3_client::StorageErrorClass;
use sha2::{Digest, Sha256};
use std::{sync::atomic::AtomicBool, time::Duration};

pub(crate) const SINGLE_COPY_LIMIT: u64 = 5 * 1024 * 1024 * 1024;
pub(crate) const TRANSFER_MARKER: &str = "r2-move-task";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TransferPlan {
    NoOp,
    SingleCopy,
    MultipartCopy,
    Relay,
    RejectConflict,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RecoveryStep {
    Transfer,
    ReconcileDestination,
    Complete,
    Reject,
}

pub(crate) fn recovery_step(stage: &str) -> RecoveryStep {
    match stage {
        "transferring" | "temporary_complete_unknown" => RecoveryStep::Transfer,
        "copied" | "delete_pending" | "delete_unknown" | "outcome_unknown" => {
            RecoveryStep::ReconcileDestination
        }
        "complete" => RecoveryStep::Complete,
        _ => RecoveryStep::Reject,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct BackendIdentity {
    namespace: String,
    principal: Option<String>,
}

fn endpoint(scheme: &str, host: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(&format!("{}://{}", scheme.trim(), host.trim()))
        .map_err(|_| "Invalid storage endpoint".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Invalid storage endpoint".to_string());
    }
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn partition(region: &str) -> &'static str {
    if region.starts_with("cn-") {
        "aws-cn"
    } else if region.starts_with("us-gov-") {
        "aws-us-gov"
    } else if region.starts_with("us-iso-") {
        "aws-iso"
    } else if region.starts_with("us-isob-") {
        "aws-iso-b"
    } else {
        "aws"
    }
}

fn identity(config: &MoveConfig) -> Result<BackendIdentity, String> {
    let (namespace, principal) = match config {
        MoveConfig::R2(cfg) => (format!("r2:{}", cfg.account_id), None),
        MoveConfig::Aws(cfg)
            if cfg
                .endpoint_host
                .as_deref()
                .is_none_or(|s| s.trim().is_empty()) =>
        {
            (format!("aws:{}", partition(&cfg.region)), None)
        }
        MoveConfig::Aws(cfg) => (
            endpoint(
                cfg.endpoint_scheme.as_deref().unwrap_or("https"),
                cfg.endpoint_host.as_deref().unwrap_or_default(),
            )?,
            Some(cfg.access_key_id.as_str()),
        ),
        MoveConfig::Minio(cfg) | MoveConfig::Rustfs(cfg) => (
            endpoint(&cfg.endpoint_scheme, &cfg.endpoint_host)?,
            Some(cfg.access_key_id.as_str()),
        ),
    };
    Ok(BackendIdentity {
        namespace,
        // Authentication identity may select a tenant on a compatible endpoint.
        // Keep credentials and even access-key identifiers out of journals/logs.
        principal: principal.map(|p| format!("{:x}", Sha256::digest(p.as_bytes()))),
    })
}

pub(crate) fn scope(config: &MoveConfig) -> Result<String, String> {
    let id = identity(config)?;
    Ok(format!(
        "{:x}",
        Sha256::digest(
            format!("{}\0{}", id.namespace, id.principal.unwrap_or_default()).as_bytes()
        )
    ))
}

pub(crate) fn native_aws(config: &MoveConfig) -> bool {
    matches!(config, MoveConfig::Aws(cfg) if cfg.endpoint_host.as_deref().is_none_or(|s| s.trim().is_empty()))
}

pub(crate) fn general_purpose_copy_bucket(config: &MoveConfig) -> bool {
    let bucket = config.bucket();
    !bucket.contains(':') && !bucket.ends_with("--x-s3")
}

pub(crate) fn compatible_multipart_copy_candidate(source: &MoveConfig, dest: &MoveConfig) -> bool {
    !matches!(dest, MoveConfig::R2(_))
        && general_purpose_copy_bucket(source)
        && general_purpose_copy_bucket(dest)
}

pub(crate) fn plan_transfer(
    source: &MoveConfig,
    dest: &MoveConfig,
    source_key: &str,
    dest_key: &str,
    size: u64,
) -> Result<TransferPlan, String> {
    let source_id = identity(source)?;
    let dest_id = identity(dest)?;
    if source_id.namespace == dest_id.namespace
        && source.bucket() == dest.bucket()
        && source_key == dest_key
    {
        return Ok(if source_id == dest_id {
            TransferPlan::NoOp
        } else {
            TransferPlan::RejectConflict
        });
    }
    if source_id != dest_id {
        return Ok(TransferPlan::Relay);
    }
    // S3 access points, directory buckets, and Outposts have additional routing
    // constraints. They do not use the general-purpose bucket copy plan. R2
    // stays eligible for a single conditional CopyObject; only its multipart
    // copy is excluded (see compatible_multipart_copy_candidate).
    if !general_purpose_copy_bucket(source) || !general_purpose_copy_bucket(dest) {
        return Ok(TransferPlan::Relay);
    }
    if size <= SINGLE_COPY_LIMIT {
        return Ok(TransferPlan::SingleCopy);
    }
    // R2 UploadPartCopy does not support source If-Match. Never assemble
    // potentially different source generations just to avoid a relay.
    Ok(if native_aws(source) && native_aws(dest) {
        TransferPlan::MultipartCopy
    } else {
        TransferPlan::Relay
    })
}

pub(crate) fn encoded_copy_source(bucket: &str, key: &str, version: Option<&str>) -> String {
    let mut encoded = format!(
        "{}/{}",
        urlencoding::encode(bucket),
        key.split('/')
            .map(|s| urlencoding::encode(s).into_owned())
            .collect::<Vec<_>>()
            .join("/")
    );
    if let Some(version) = version.filter(|v| !v.is_empty() && *v != "null") {
        encoded.push_str("?versionId=");
        encoded.push_str(&urlencoding::encode(version));
    }
    encoded
}

const HEAD_OPERATION_BUDGET: Duration = Duration::from_secs(30);
#[cfg(test)]
static NEVER_CANCELLED_HEAD: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static NEVER_PAUSED_HEAD: AtomicBool = AtomicBool::new(false);

fn head_operation_scope(config: &MoveConfig) -> String {
    format!("{}:{}", config.operation_endpoint(), config.bucket())
}

pub(crate) fn operation_error(operation: &str, error: OperationError) -> String {
    match error {
        OperationError::Cancelled | OperationError::Paused => error.to_string(),
        // The attempt already named a task status (a relay read's `needs_auth:`
        // or `conflict:`, or a pause seen mid-body); keep it as the status.
        OperationError::Failed { error, .. }
            if super::worker::FAILURE_STATUSES
                .iter()
                .any(|status| error.message.starts_with(&format!("{status}:"))) =>
        {
            error.message
        }
        other => format!("{}: {operation}: {other}", other.class().label()),
    }
}

pub(crate) fn storage_error(
    operation: &str,
    code: Option<&str>,
    status: Option<u16>,
    detail: impl std::fmt::Display,
    mutation: bool,
) -> String {
    let class = crate::providers::s3_client::classify_storage_error(code, status, mutation).label();
    format!("{class}: {operation}: {detail}")
}

pub(crate) async fn head_identity_checked(
    config: &MoveConfig,
    key: &str,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<
    Option<(
        SourceIdentity,
        aws_sdk_s3::operation::head_object::HeadObjectOutput,
    )>,
    String,
> {
    let client = config.client().await?;
    let endpoint = config.operation_endpoint();
    let scope = head_operation_scope(config);
    let context = OperationContext::new(
        OperationKind::Head,
        &endpoint,
        &scope,
        key,
        tokio::time::Instant::now() + HEAD_OPERATION_BUDGET,
        cancelled,
    )
    .with_pause(paused)
    .with_max_attempts(3);
    match execute_operation(&context, || async {
        client
            .head_object()
            .bucket(config.bucket())
            .key(key)
            .send()
            .await
            .map_err(|error| AttemptError::from_sdk(&error))
    })
    .await
    {
        Ok(head) => {
            let size = head
                .content_length()
                .filter(|n| *n >= 0)
                .ok_or_else(|| "Missing source content length".to_string())?
                as u64;
            let etag = head
                .e_tag()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Missing object ETag; source cannot be frozen".to_string())?
                .to_string();
            let version_id = head
                .version_id()
                .filter(|v| !v.is_empty() && *v != "null")
                .map(str::to_string);
            Ok(Some((
                SourceIdentity {
                    size,
                    etag,
                    version_id,
                },
                head,
            )))
        }
        Err(OperationError::Failed { error, .. })
            if matches!(
                error.message.as_str(),
                "NotFound" | "NoSuchKey" | "NoSuchVersion"
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(operation_error("HEAD", error)),
    }
}

#[cfg(test)]
pub(crate) async fn head_identity(
    config: &MoveConfig,
    key: &str,
) -> Result<
    Option<(
        SourceIdentity,
        aws_sdk_s3::operation::head_object::HeadObjectOutput,
    )>,
    String,
> {
    head_identity_checked(config, key, &NEVER_CANCELLED_HEAD, &NEVER_PAUSED_HEAD).await
}

pub(crate) fn verified_destination(
    journal: &MoveJournal,
    current: &SourceIdentity,
    marker: Option<&str>,
) -> Result<(), String> {
    if marker != Some(journal.task_id.as_str()) || current.size != journal.source.size {
        return Err("conflict: Destination does not match this move; source retained".to_string());
    }
    if journal
        .destination
        .as_ref()
        .is_some_and(|recorded| recorded != current)
    {
        return Err("conflict: Destination changed after copying; source retained".to_string());
    }
    Ok(())
}

/// A lost publication response leaves no destination ETag receipt. Another
/// writer may preserve our metadata while changing same-sized content, so a
/// marker/size match alone cannot authorize deleting the source.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn verify_unknown_content(
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    source_key: &str,
    dest_key: &str,
    source: &SourceIdentity,
    destination: &SourceIdentity,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<(), String> {
    let source_client = source_config.client().await?;
    let dest_client = dest_config.client().await?;
    verify_object_content(
        ObjectRead {
            client: &source_client,
            bucket: source_config.bucket(),
            key: source_key,
            identity: source,
            endpoint: source_config.operation_endpoint(),
            scope: head_operation_scope(source_config),
        },
        ObjectRead {
            client: &dest_client,
            bucket: dest_config.bucket(),
            key: dest_key,
            identity: destination,
            endpoint: dest_config.operation_endpoint(),
            scope: head_operation_scope(dest_config),
        },
        cancelled,
        paused,
    )
    .await
}

pub(crate) struct ObjectRead<'a> {
    pub client: &'a aws_sdk_s3::Client,
    pub bucket: &'a str,
    pub key: &'a str,
    pub identity: &'a SourceIdentity,
    pub endpoint: String,
    pub scope: String,
}

pub(crate) async fn verify_object_content(
    source: ObjectRead<'_>,
    destination: ObjectRead<'_>,
    cancelled: &AtomicBool,
    paused: &AtomicBool,
) -> Result<(), String> {
    let identity = format!(
        "{}:{}:{}|{}:{}:{}",
        source.key,
        source.identity.etag,
        source.identity.version_id.as_deref().unwrap_or_default(),
        destination.key,
        destination.identity.etag,
        destination
            .identity
            .version_id
            .as_deref()
            .unwrap_or_default()
    );
    let context = OperationContext::new(
        OperationKind::Get,
        &source.endpoint,
        &source.scope,
        &identity,
        tokio::time::Instant::now()
            + super::stream::protocol::attempt_timeout(source.identity.size),
        cancelled,
    )
    .with_pause(paused)
    .with_peer_endpoint(&destination.endpoint)
    .with_max_attempts(3);
    execute_operation(&context, || async {
        verify_object_content_once(&source, &destination).await
    })
    .await
    .map_err(verification_operation_error)
}

async fn verify_object_content_once(
    source: &ObjectRead<'_>,
    destination: &ObjectRead<'_>,
) -> Result<(), AttemptError> {
    use tokio::io::AsyncReadExt;
    let source_get = source
        .client
        .get_object()
        .bucket(source.bucket)
        .key(source.key)
        .if_match(&source.identity.etag)
        .set_version_id(source.identity.version_id.clone())
        .send();
    let dest_get = destination
        .client
        .get_object()
        .bucket(destination.bucket)
        .key(destination.key)
        .if_match(&destination.identity.etag)
        .set_version_id(destination.identity.version_id.clone())
        .send();
    let (source_get, dest_get) = tokio::join!(source_get, dest_get);
    let source_get = source_get.map_err(|error| AttemptError::from_sdk(&error))?;
    let dest_get = dest_get.map_err(|error| AttemptError::from_sdk(&error))?;
    if source_get.e_tag() != Some(source.identity.etag.as_str())
        || dest_get.e_tag() != Some(destination.identity.etag.as_str())
        || source_get.content_length() != Some(source.identity.size as i64)
        || dest_get.content_length() != Some(source.identity.size as i64)
    {
        return Err(AttemptError::new(
            StorageErrorClass::Conflict,
            "Object identity changed during uncertain-copy verification",
        ));
    }
    let mut source_body = source_get.body.into_async_read();
    let mut dest_body = dest_get.body.into_async_read();
    let mut left = vec![0; 1024 * 1024];
    let mut right = vec![0; 1024 * 1024];
    let mut remaining = source.identity.size;
    while remaining > 0 {
        let count = remaining.min(left.len() as u64) as usize;
        let (a, b) = tokio::join!(
            tokio::time::timeout(
                Duration::from_secs(30),
                source_body.read_exact(&mut left[..count])
            ),
            tokio::time::timeout(
                Duration::from_secs(30),
                dest_body.read_exact(&mut right[..count])
            )
        );
        a.map_err(|_| AttemptError::transient("Source verification stalled"))?
            .map_err(|e| AttemptError::transient(format!("Source verification failed: {e}")))?;
        b.map_err(|_| AttemptError::transient("Destination verification stalled"))?
            .map_err(|e| {
                AttemptError::transient(format!("Destination verification failed: {e}"))
            })?;
        if left[..count] != right[..count] {
            return Err(AttemptError::new(
                StorageErrorClass::Conflict,
                "Destination metadata matches but its content differs; source retained",
            ));
        }
        remaining -= count as u64;
    }
    let (a, b) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(30), source_body.read(&mut left[..1])),
        tokio::time::timeout(Duration::from_secs(30), dest_body.read(&mut right[..1]))
    );
    if a.map_err(|_| AttemptError::transient("Source verification stalled"))?
        .map_err(|e| AttemptError::transient(format!("Source verification failed: {e}")))?
        != 0
        || b.map_err(|_| AttemptError::transient("Destination verification stalled"))?
            .map_err(|e| AttemptError::transient(format!("Destination verification failed: {e}")))?
            != 0
    {
        return Err(AttemptError::new(
            StorageErrorClass::Conflict,
            "Verification response exceeded its object length",
        ));
    }
    Ok(())
}

fn verification_operation_error(error: OperationError) -> String {
    match error {
        OperationError::Cancelled | OperationError::Paused => error.to_string(),
        OperationError::Deadline { .. } => {
            "transient: Content verification exceeded its budget; source retained".into()
        }
        OperationError::Failed { error, .. } => match error.class {
            StorageErrorClass::Conflict => format!("conflict: {}", error.message),
            other => format!(
                "{}: Content verification failed: {}",
                other.label(),
                error.message
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_publication_verifies_content_even_when_metadata_and_size_match() {
        use crate::test_s3::{serve, Response};
        for changed in [false, true] {
            let fixture = serve(move |request| async move {
                let source = request
                    .path
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .ends_with("/source");
                let body = if !source && changed {
                    "modified"
                } else {
                    "original"
                };
                Response::xml(200, body).header(
                    "etag",
                    if source {
                        "\"source\""
                    } else {
                        "\"destination\""
                    },
                )
            })
            .await;
            let endpoint = fixture
                .endpoint
                .as_str()
                .strip_prefix("http://")
                .unwrap()
                .to_string();
            let config = MoveConfig::Minio(crate::providers::minio::MinioConfig {
                bucket: "bucket".into(),
                access_key_id: "fixture".into(),
                secret_access_key: "fixture-secret".into(),
                endpoint_scheme: "http".into(),
                endpoint_host: endpoint,
                force_path_style: true,
            });
            let source = SourceIdentity {
                size: 8,
                etag: "\"source\"".into(),
                version_id: None,
            };
            let destination = SourceIdentity {
                size: 8,
                etag: "\"destination\"".into(),
                version_id: None,
            };
            let result = verify_unknown_content(
                &config,
                &config,
                "source",
                "destination",
                &source,
                &destination,
                &AtomicBool::new(false),
                &AtomicBool::new(false),
            )
            .await;
            assert_eq!(result.is_err(), changed, "{result:?}");
            if let Err(error) = result {
                assert!(error.starts_with("conflict:"));
            }
            let requests = fixture.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests
                .iter()
                .all(|r| r.method == "GET" && r.headers.contains_key("if-match")));
        }
    }

    fn fixture_config(endpoint: &str) -> MoveConfig {
        MoveConfig::Minio(crate::providers::minio::MinioConfig {
            bucket: "bucket".into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            endpoint_scheme: "http".into(),
            endpoint_host: endpoint.strip_prefix("http://").unwrap().into(),
            force_path_style: true,
        })
    }

    #[tokio::test]
    async fn paired_verification_retries_body_transport_and_rechecks_both_objects() {
        use crate::test_s3::{serve, Response};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let broken_destination = Arc::new(AtomicUsize::new(0));
        let fixture = serve({
            let broken_destination = broken_destination.clone();
            move |request| {
                let broken_destination = broken_destination.clone();
                async move {
                    let source = request
                        .path
                        .split('?')
                        .next()
                        .unwrap_or_default()
                        .ends_with("/source");
                    if !source && broken_destination.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Response::xml(200, "orig")
                            .header("etag", "\"destination\"")
                            .header("content-length", "8");
                    }
                    Response::xml(200, "original").header(
                        "etag",
                        if source {
                            "\"source\""
                        } else {
                            "\"destination\""
                        },
                    )
                }
            }
        })
        .await;
        let config = fixture_config(&fixture.endpoint);
        let source = SourceIdentity {
            size: 8,
            etag: "\"source\"".into(),
            version_id: None,
        };
        let destination = SourceIdentity {
            size: 8,
            etag: "\"destination\"".into(),
            version_id: None,
        };
        verify_unknown_content(
            &config,
            &config,
            "source",
            "destination",
            &source,
            &destination,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.iter().filter(|r| r.method == "GET").count(), 4);
        assert_eq!(
            requests
                .iter()
                .filter(|r| r
                    .path
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .ends_with("/source"))
                .count(),
            2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r
                    .path
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .ends_with("/destination"))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn paired_verification_does_not_retry_auth_or_cancelled_work() {
        use crate::test_s3::{serve, Response};

        let fixture = serve(|request| async move {
            let source = request
                .path
                .split('?')
                .next()
                .unwrap_or_default()
                .ends_with("/source");
            if source {
                return Response::xml(403, "<Error><Code>AccessDenied</Code></Error>");
            }
            Response::xml(200, "original").header("etag", "\"destination\"")
        })
        .await;
        let config = fixture_config(&fixture.endpoint);
        let source = SourceIdentity {
            size: 8,
            etag: "\"source\"".into(),
            version_id: None,
        };
        let destination = SourceIdentity {
            size: 8,
            etag: "\"destination\"".into(),
            version_id: None,
        };
        let error = verify_unknown_content(
            &config,
            &config,
            "source",
            "destination",
            &source,
            &destination,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await
        .unwrap_err();
        assert!(error.starts_with("needs_auth:"), "{error}");
        assert_eq!(fixture.requests.lock().unwrap().len(), 2);

        let cancelled = AtomicBool::new(true);
        let before = fixture.requests.lock().unwrap().len();
        let error = verify_unknown_content(
            &config,
            &config,
            "source",
            "destination",
            &source,
            &destination,
            &cancelled,
            &AtomicBool::new(false),
        )
        .await
        .unwrap_err();
        assert!(error.starts_with("cancelled:"), "{error}");
        assert_eq!(fixture.requests.lock().unwrap().len(), before);
    }

    fn minio(host: &str, bucket: &str, principal: &str) -> MoveConfig {
        MoveConfig::Minio(crate::providers::minio::MinioConfig {
            bucket: bucket.into(),
            access_key_id: principal.into(),
            secret_access_key: "secret".into(),
            endpoint_scheme: "https".into(),
            endpoint_host: host.into(),
            force_path_style: true,
        })
    }
    fn aws(region: &str, bucket: &str) -> MoveConfig {
        MoveConfig::Aws(crate::providers::aws::AwsConfig {
            bucket: bucket.into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
            region: region.into(),
            endpoint_scheme: None,
            endpoint_host: None,
            force_path_style: false,
        })
    }
    #[test]
    fn separate_endpoints_cannot_copy_the_wrong_source() {
        assert_eq!(
            plan_transfer(
                &minio("a.example", "data", "key"),
                &minio("b.example", "target", "key"),
                "x",
                "x",
                42
            )
            .unwrap(),
            TransferPlan::Relay
        );
    }
    #[test]
    fn normalized_self_move_is_noop_and_ambiguous_tenant_rejected() {
        let source = minio("EXAMPLE.test:443/", "bucket", "one");
        assert_eq!(
            plan_transfer(
                &source,
                &minio("example.test", "bucket", "one"),
                "x",
                "x",
                0
            )
            .unwrap(),
            TransferPlan::NoOp
        );
        assert_eq!(
            plan_transfer(
                &source,
                &minio("example.test", "bucket", "two"),
                "x",
                "x",
                0
            )
            .unwrap(),
            TransferPlan::RejectConflict
        );
    }
    #[test]
    fn aws_cross_account_copy_uses_cloud_namespace_and_partition() {
        assert_eq!(
            plan_transfer(
                &aws("us-east-1", "a"),
                &aws("us-west-2", "b"),
                "x",
                "x",
                SINGLE_COPY_LIMIT + 1
            )
            .unwrap(),
            TransferPlan::MultipartCopy
        );
        assert_eq!(
            plan_transfer(&aws("us-east-1", "a"), &aws("cn-north-1", "b"), "x", "x", 1).unwrap(),
            TransferPlan::Relay
        );
    }
    #[test]
    fn aws_special_buckets_stay_relay_only() {
        for bucket in [
            "arn:aws:s3:us-east-1:123456789012:accesspoint/source",
            "data--x-s3",
        ] {
            assert_eq!(
                plan_transfer(
                    &aws("us-east-1", bucket),
                    &aws("us-east-1", "dest"),
                    "x",
                    "y",
                    SINGLE_COPY_LIMIT + 1
                )
                .unwrap(),
                TransferPlan::Relay
            );
            assert!(!compatible_multipart_copy_candidate(
                &aws("us-east-1", bucket),
                &aws("us-east-1", "dest")
            ));
        }
    }

    #[test]
    fn large_r2_requires_conditional_relay() {
        let r2 = |bucket: &str| {
            MoveConfig::R2(crate::r2::R2Config {
                account_id: "actual-account".into(),
                bucket: bucket.into(),
                access_key_id: "key".into(),
                secret_access_key: "secret".into(),
            })
        };
        assert_eq!(
            plan_transfer(&r2("a"), &r2("b"), "x", "x", SINGLE_COPY_LIMIT + 1).unwrap(),
            TransferPlan::Relay
        );
        assert!(!compatible_multipart_copy_candidate(&r2("a"), &r2("b")));
    }

    #[test]
    fn small_r2_moves_within_an_account_copy_server_side() {
        let r2 = |bucket: &str| {
            MoveConfig::R2(crate::r2::R2Config {
                account_id: "actual-account".into(),
                bucket: bucket.into(),
                access_key_id: "key".into(),
                secret_access_key: "secret".into(),
            })
        };
        for size in [0, 1024 * 1024, SINGLE_COPY_LIMIT] {
            assert_eq!(
                plan_transfer(&r2("a"), &r2("b"), "x", "x", size).unwrap(),
                TransferPlan::SingleCopy
            );
        }
    }
    #[test]
    fn copy_source_preserves_literal_special_characters_and_version() {
        assert_eq!(
            encoded_copy_source("bucket", "目录/a +%?#%2F.txt", Some("v+/=?")),
            "bucket/%E7%9B%AE%E5%BD%95/a%20%2B%25%3F%23%252F.txt?versionId=v%2B%2F%3D%3F"
        );
    }
    #[test]
    fn size_alone_does_not_verify_a_destination() {
        let identity = SourceIdentity {
            size: 4,
            etag: "a".into(),
            version_id: None,
        };
        let journal = MoveJournal {
            task_id: "task".into(),
            stage: "copied".into(),
            source: identity.clone(),
            source_scope: "a".into(),
            dest_scope: "b".into(),
            destination: Some(identity.clone()),
            retry: Default::default(),
            metrics: Default::default(),
        };
        assert!(verified_destination(&journal, &identity, None).is_err());
        assert!(verified_destination(&journal, &identity, Some("task")).is_ok());
        let mut changed = identity;
        changed.etag = "new".into();
        assert!(verified_destination(&journal, &changed, Some("task")).is_err());
    }
    #[test]
    fn failed_delete_and_uncertain_commit_never_restart_a_transfer() {
        for phase in [
            "copied",
            "delete_pending",
            "delete_unknown",
            "outcome_unknown",
        ] {
            assert_eq!(recovery_step(phase), RecoveryStep::ReconcileDestination);
        }
        assert_eq!(recovery_step("transferring"), RecoveryStep::Transfer);
        assert_eq!(recovery_step("complete"), RecoveryStep::Complete);
        assert_eq!(recovery_step("unknown-phase"), RecoveryStep::Reject);
    }

    async fn head_fixture(response: &'static str) -> (MoveConfig, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = MoveConfig::Minio(crate::providers::minio::MinioConfig {
            bucket: "bucket".into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture".into(),
            endpoint_scheme: "http".into(),
            endpoint_host: address.to_string(),
            force_path_style: true,
        });
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = Vec::new();
                let mut buffer = [0u8; 2048];
                loop {
                    let count = socket.read(&mut buffer).await.unwrap_or(0);
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|s| s == b"\r\n\r\n") {
                        break;
                    }
                }
                assert!(request.starts_with(b"HEAD /bucket/"));
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (config, task)
    }

    async fn head_sequence_fixture(
        responses: Vec<&'static str>,
    ) -> (
        MoveConfig,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = MoveConfig::Minio(crate::providers::minio::MinioConfig {
            bucket: "bucket".into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture".into(),
            endpoint_scheme: "http".into(),
            endpoint_host: address.to_string(),
            force_path_style: true,
        });
        let responses = Arc::new(responses);
        let attempts = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn({
            let responses = responses.clone();
            let attempts = attempts.clone();
            async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 2048];
                    loop {
                        let count = socket.read(&mut buffer).await.unwrap_or(0);
                        if count == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..count]);
                        if request.windows(4).any(|s| s == b"\r\n\r\n") {
                            break;
                        }
                    }
                    assert!(request.starts_with(b"HEAD /bucket/"));
                    let index = attempts.fetch_add(1, Ordering::SeqCst);
                    let response = responses
                        .get(index)
                        .or_else(|| responses.last())
                        .copied()
                        .unwrap();
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                }
            }
        });
        (config, attempts, task)
    }

    #[tokio::test]
    async fn real_sdk_head_retries_transient_once_and_does_not_retry_auth() {
        use std::sync::atomic::Ordering;
        let (config, attempts, task) = head_sequence_fixture(vec![
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nETag: \"source\"\r\nConnection: close\r\n\r\n",
        ])
        .await;
        let (identity, _) = head_identity(&config, "x").await.unwrap().unwrap();
        assert_eq!(identity.etag, "\"source\"");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        task.abort();

        let (config, attempts, task) = head_sequence_fixture(vec![
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nETag: \"source\"\r\nConnection: close\r\n\r\n",
        ])
        .await;
        assert!(head_identity(&config, "x")
            .await
            .unwrap_err()
            .starts_with("needs_auth:"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        task.abort();
    }

    #[tokio::test]
    async fn real_sdk_head_preserves_identity_and_never_treats_server_errors_as_missing() {
        let (config, task) = head_fixture("HTTP/1.1 200 OK\r\nContent-Length: 7\r\nETag: \"source\"\r\nx-amz-version-id: version-1\r\nConnection: close\r\n\r\n").await;
        let (identity, _) = head_identity(&config, "x").await.unwrap().unwrap();
        assert_eq!(identity.size, 7);
        assert_eq!(identity.etag, "\"source\"");
        assert_eq!(identity.version_id.as_deref(), Some("version-1"));
        task.abort();
        for (response, expected) in [
            (
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "transient:",
            ),
            (
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "needs_auth:",
            ),
        ] {
            let (config, task) = head_fixture(response).await;
            assert!(
                head_identity(&config, "x")
                    .await
                    .unwrap_err()
                    .starts_with(expected)
            );
            task.abort();
        }
        let (config, task) = head_fixture(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(head_identity(&config, "x").await.unwrap().is_none());
        task.abort();
    }
}
