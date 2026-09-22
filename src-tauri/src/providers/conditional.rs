//! Verify optional conditional operations on isolated, disposable objects.
//! Compatible endpoints differ by deployment, not just by provider name.

use aws_sdk_s3::{
    config::http::HttpResponse,
    error::{ProvideErrorMetadata, SdkError},
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart},
    Client,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::OnceCell;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Condition {
    PutCreate,
    PutMatch,
    CompleteCreate,
    CompleteMatch,
    CopyCreate,
    CopyMatch,
    CopySource,
    PartCopySource,
    DeleteMatch,
}

const CAPABILITY_TTL: Duration = Duration::from_secs(300);
type Cache = HashMap<(String, String, Condition), Arc<OnceCell<(bool, Instant)>>>;
static CAPABILITIES: OnceLock<Mutex<Cache>> = OnceLock::new();

/// Scope includes credential identity. A changed account never inherits a
/// different deployment's result. Errors are not cached as capability answers.
pub async fn supported(
    client: &Client,
    bucket: &str,
    scope: &str,
    condition: Condition,
) -> Result<bool, String> {
    let cell = {
        let mut cache = CAPABILITIES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if cache.len() >= 64 {
            cache.retain(|_, cell| Arc::strong_count(cell) > 1);
        }
        let key = (scope.to_string(), bucket.to_string(), condition);
        if cache
            .get(&key)
            .and_then(|cell| cell.get())
            .is_some_and(|(_, checked)| checked.elapsed() >= CAPABILITY_TTL)
        {
            cache.remove(&key);
        }
        cache.entry(key).or_default().clone()
    };
    cell.get_or_try_init(|| async {
        // A probe is a long chain of SDK calls. Running it as its own task keeps
        // that chain off the caller's stack (rename → flush → upload), which an
        // unoptimised build otherwise overflows. Probe keys are unique per run
        // and cleanup is time-bounded, so a probe that outlives a cancelled
        // caller only finishes removing its own objects.
        let (client, bucket, scope) = (client.clone(), bucket.to_string(), scope.to_string());
        tokio::spawn(async move { probe(&client, &bucket, &scope, condition).await })
            .await
            .map_err(|error| format!("Capability probe stopped unexpectedly: {error}"))?
            .map(|answer| (answer, Instant::now()))
    })
    .await
    .map(|(answer, _)| *answer)
}

fn condition_rejected<T, E: ProvideErrorMetadata + std::error::Error + 'static>(
    result: Result<T, SdkError<E, HttpResponse>>,
) -> Result<bool, String> {
    match result {
        Ok(_) => Ok(false),
        Err(error) => match (
            error
                .raw_response()
                .map(|response| response.status().as_u16()),
            error.code(),
        ) {
            (Some(412), _) => Ok(true),
            (Some(501), _) | (_, Some("NotImplemented" | "UnsupportedOperation")) => Ok(false),
            _ => Err(probe_error(&error)),
        },
    }
}

fn probe_error<E: ProvideErrorMetadata + std::error::Error + 'static>(
    error: &SdkError<E, HttpResponse>,
) -> String {
    format!(
        "{}: Cannot verify storage conditions: {}",
        super::s3_client::s3_error_class(error, false).label(),
        super::s3_client::describe_s3_error(error)
    )
}

fn object_key(scope: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut hash = Sha256::new();
    hash.update(scope.as_bytes());
    hash.update(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_le_bytes(),
    );
    hash.update(NEXT.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    hash.update(std::process::id().to_le_bytes());
    format!(".r2-operation-checks/{:x}", hash.finalize())
}

async fn probe(
    client: &Client,
    bucket: &str,
    scope: &str,
    condition: Condition,
) -> Result<bool, String> {
    let key = object_key(scope);
    let source = format!("{key}-source");
    let mut upload_id = None;
    let result = tokio::time::timeout(Duration::from_secs(45), async {
        let initial = client
            .put_object()
            .bucket(bucket)
            .key(&key)
            .body(ByteStream::from_static(b"original"))
            .send()
            .await
            .map_err(|e| probe_error(&e))?;
        let etag = initial
            .e_tag()
            .filter(|s| !s.is_empty())
            .ok_or("Capability probe did not return an object identity")?
            .to_string();
        let rejected = match condition {
            Condition::PutMatch => condition_rejected(
                client
                    .put_object()
                    .bucket(bucket)
                    .key(&key)
                    .if_match("\"r2-deliberately-nonmatching-etag\"")
                    .body(ByteStream::from_static(b"replacement"))
                    .send()
                    .await,
            )?,
            Condition::PutCreate => condition_rejected(
                client
                    .put_object()
                    .bucket(bucket)
                    .key(&key)
                    .if_none_match("*")
                    .body(ByteStream::from_static(b"replacement"))
                    .send()
                    .await,
            )?,
            Condition::DeleteMatch => condition_rejected(
                client
                    .delete_object()
                    .bucket(bucket)
                    .key(&key)
                    .if_match("\"r2-deliberately-nonmatching-etag\"")
                    .send()
                    .await,
            )?,
            Condition::CopyCreate | Condition::CopyMatch | Condition::CopySource => {
                client
                    .put_object()
                    .bucket(bucket)
                    .key(&source)
                    .body(ByteStream::from_static(b"replacement"))
                    .send()
                    .await
                    .map_err(|e| probe_error(&e))?;
                let request = client
                    .copy_object()
                    .bucket(bucket)
                    .key(&key)
                    .copy_source(format!("{}/{}", urlencoding::encode(bucket), source));
                let request = if condition == Condition::CopyCreate {
                    request.if_none_match("*")
                } else if condition == Condition::CopyMatch {
                    request.if_match("\"r2-deliberately-nonmatching-etag\"")
                } else {
                    request.copy_source_if_match("\"r2-deliberately-nonmatching-etag\"")
                };
                condition_rejected(request.send().await)?
            }
            Condition::CompleteCreate | Condition::CompleteMatch | Condition::PartCopySource => {
                let created = client
                    .create_multipart_upload()
                    .bucket(bucket)
                    .key(&key)
                    .send()
                    .await
                    .map_err(|e| probe_error(&e))?;
                let id = created
                    .upload_id()
                    .ok_or("Capability probe omitted multipart ID")?
                    .to_string();
                upload_id = Some(id.clone());
                if condition == Condition::PartCopySource {
                    client
                        .put_object()
                        .bucket(bucket)
                        .key(&source)
                        .body(ByteStream::from_static(b"replacement"))
                        .send()
                        .await
                        .map_err(|e| probe_error(&e))?;
                    let rejected = condition_rejected(
                        client
                            .upload_part_copy()
                            .bucket(bucket)
                            .key(&key)
                            .upload_id(&id)
                            .part_number(1)
                            .copy_source(format!("{}/{}", urlencoding::encode(bucket), source))
                            .copy_source_if_match("\"r2-deliberately-nonmatching-etag\"")
                            .send()
                            .await,
                    )?;
                    if !rejected {
                        return Ok(false);
                    }
                    let parts = client
                        .list_parts()
                        .bucket(bucket)
                        .key(&key)
                        .upload_id(&id)
                        .send()
                        .await
                        .map_err(|e| probe_error(&e))?;
                    return Ok(parts.parts().is_empty());
                }
                let uploaded = client
                    .upload_part()
                    .bucket(bucket)
                    .key(&key)
                    .upload_id(&id)
                    .part_number(1)
                    .body(ByteStream::from_static(b"replacement"))
                    .send()
                    .await
                    .map_err(|e| probe_error(&e))?;
                let part = CompletedPart::builder()
                    .part_number(1)
                    .e_tag(
                        uploaded
                            .e_tag()
                            .ok_or("Capability probe omitted part identity")?,
                    )
                    .build();
                let request = client
                    .complete_multipart_upload()
                    .bucket(bucket)
                    .key(&key)
                    .upload_id(id)
                    .multipart_upload(CompletedMultipartUpload::builder().parts(part).build());
                let request = if condition == Condition::CompleteCreate {
                    request.if_none_match("*")
                } else {
                    request.if_match("\"r2-deliberately-nonmatching-etag\"")
                };
                condition_rejected(request.send().await)?
            }
        };
        if !rejected {
            return Ok(false);
        }
        // A 412 alone is insufficient if a proxy produced it after committing.
        let head = client
            .head_object()
            .bucket(bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| probe_error(&e))?;
        Ok(head.e_tag() == Some(etag.as_str()) && head.content_length() == Some(8))
    })
    .await
    .map_err(|_| {
        "transient: Storage capability probe timed out; user objects were not touched".to_string()
    })
    .and_then(|result| result);
    // Cleanup has its own small budget and only addresses probe-owned keys.
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        if let Some(id) = upload_id {
            let _ = client
                .abort_multipart_upload()
                .bucket(bucket)
                .key(&key)
                .upload_id(id)
                .send()
                .await;
        }
        let _ = client.delete_object().bucket(bucket).key(&key).send().await;
        let _ = client
            .delete_object()
            .bucket(bucket)
            .key(&source)
            .send()
            .await;
    })
    .await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_s3::{serve, Response};

    #[tokio::test]
    async fn temporary_probe_failures_are_not_cached_as_unsupported() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fixture = serve({
            let attempts = attempts.clone();
            move |request| {
                let attempts = attempts.clone();
                async move {
                    if request.headers.contains_key("if-none-match") {
                        return match attempts.fetch_add(1, Ordering::SeqCst) {
                            0 => Response::xml(403, "<Error><Code>AccessDenied</Code></Error>"),
                            1 => Response::xml(503, "<Error><Code>SlowDown</Code></Error>"),
                            _ => {
                                Response::xml(412, "<Error><Code>PreconditionFailed</Code></Error>")
                            }
                        };
                    }
                    Response::empty(if request.method == "DELETE" { 204 } else { 200 })
                        .header("etag", "\"original\"")
                        .header(
                            "content-length",
                            if request.method == "HEAD" { 8 } else { 0 },
                        )
                }
            }
        })
        .await;
        let scope = format!("temporary-errors-{}", fixture.endpoint);
        for _ in 0..2 {
            assert!(
                supported(&fixture.client, "bucket", &scope, Condition::PutCreate)
                    .await
                    .is_err()
            );
        }
        for _ in 0..2 {
            assert!(
                supported(&fixture.client, "bucket", &scope, Condition::PutCreate)
                    .await
                    .unwrap()
            );
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn expired_capabilities_are_rechecked() {
        let fixture = serve(|request| async move {
            if request.headers.contains_key("if-none-match") {
                return Response::xml(412, "<Error><Code>PreconditionFailed</Code></Error>");
            }
            Response::empty(if request.method == "DELETE" { 204 } else { 200 })
                .header("etag", "\"original\"")
                .header(
                    "content-length",
                    if request.method == "HEAD" { 8 } else { 0 },
                )
        })
        .await;
        let scope = format!("expired-{}", fixture.endpoint);
        let cell = OnceCell::new();
        cell.set((false, Instant::now() - CAPABILITY_TTL)).unwrap();
        CAPABILITIES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .insert(
                (scope.clone(), "bucket".into(), Condition::PutCreate),
                Arc::new(cell),
            );
        assert!(
            supported(&fixture.client, "bucket", &scope, Condition::PutCreate)
                .await
                .unwrap()
        );
        assert!(!fixture.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn accepts_only_conditions_that_reject_without_changing_the_object() {
        let fixture = serve(|request| async move {
            if request.headers.contains_key("if-none-match") || request.headers.contains_key("if-match") || request.headers.contains_key("x-amz-copy-source-if-match") {
                return Response::xml(412,"<Error><Code>PreconditionFailed</Code></Error>");
            }
            if request.method == "HEAD" { return Response::empty(200).header("etag","\"original\"").header("content-length",8); }
            if request.method == "POST" { return Response::xml(200,"<InitiateMultipartUploadResult><UploadId>probe-upload</UploadId></InitiateMultipartUploadResult>"); }
            Response::empty(if request.method == "DELETE" {204} else {200}).header("etag","\"original\"")
        }).await;
        for condition in [
            Condition::PutCreate,
            Condition::CopyCreate,
            Condition::CopySource,
            Condition::CompleteCreate,
            Condition::DeleteMatch,
        ] {
            assert!(
                probe(&fixture.client, "bucket", "test-scope", condition)
                    .await
                    .unwrap(),
                "{condition:?}"
            );
        }
        let requests = fixture.requests.lock().unwrap();
        assert!(requests
            .iter()
            .any(|r| r.method == "POST" && r.headers.contains_key("if-none-match")));
        assert!(requests
            .iter()
            .all(|r| r.path.starts_with("/bucket/.r2-operation-checks/")));
    }

    #[tokio::test]
    async fn ignoring_a_condition_never_enables_it() {
        let fixture = serve(|request| async move {
            Response::empty(if request.method == "DELETE" { 204 } else { 200 })
                .header("etag", "\"original\"")
        })
        .await;
        assert!(!probe(
            &fixture.client,
            "bucket",
            "ignored-condition",
            Condition::PutCreate
        )
        .await
        .unwrap());
        assert!(!probe(
            &fixture.client,
            "bucket",
            "ignored-condition",
            Condition::DeleteMatch
        )
        .await
        .unwrap());
    }

    #[tokio::test]
    async fn a_rejection_after_a_side_effect_is_not_a_capability() {
        let fixture = serve(|request| async move {
            if request.headers.contains_key("if-none-match") {
                return Response::xml(412, "<Error><Code>PreconditionFailed</Code></Error>");
            }
            Response::empty(200)
                .header(
                    "etag",
                    if request.method == "HEAD" {
                        "\"changed\""
                    } else {
                        "\"original\""
                    },
                )
                .header(
                    "content-length",
                    if request.method == "HEAD" { 8 } else { 0 },
                )
        })
        .await;
        assert!(!probe(
            &fixture.client,
            "bucket",
            "lying-proxy",
            Condition::PutCreate
        )
        .await
        .unwrap());
    }
}
