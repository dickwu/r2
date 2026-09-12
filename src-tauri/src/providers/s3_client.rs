use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::config::{retry::RetryConfig, timeout::TimeoutConfig};
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::Client;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_CACHED_CLIENTS: usize = 32;
type ClientCache = HashMap<[u8; 32], (Client, Instant)>;
static CLIENTS: OnceLock<Mutex<ClientCache>> = OnceLock::new();

pub type S3Result<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageErrorClass {
    Transient,
    NeedsAuth,
    Conflict,
    NotFound,
    OutcomeUnknown,
    Permanent,
}

impl StorageErrorClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::NeedsAuth => "needs_auth",
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Permanent => "error",
        }
    }
}

pub fn classify_storage_error(
    code: Option<&str>,
    status: Option<u16>,
    mutation: bool,
) -> StorageErrorClass {
    use StorageErrorClass::*;
    match (code.unwrap_or_default(), status) {
        ("PreconditionFailed" | "ConditionalRequestConflict", _) | (_, Some(409 | 412)) => Conflict,
        ("AccessDenied" | "InvalidAccessKeyId" | "SignatureDoesNotMatch" | "ExpiredToken", _)
        | (_, Some(401 | 403)) => NeedsAuth,
        ("NoSuchKey" | "NotFound" | "NoSuchUpload" | "NoSuchVersion", _) | (_, Some(404)) => {
            NotFound
        }
        (_, Some(501)) => Permanent,
        (_, None | Some(408 | 500..=599)) if mutation => OutcomeUnknown,
        (code, _) if TRANSIENT_ERROR_CODES.contains(&code) => Transient,
        (_, None | Some(408 | 429 | 500..=599)) => Transient,
        _ => Permanent,
    }
}

pub fn s3_error_class<E: ProvideErrorMetadata>(
    error: &SdkError<E, HttpResponse>,
    mutation: bool,
) -> StorageErrorClass {
    if matches!(error, SdkError::ConstructionFailure(_)) {
        return StorageErrorClass::Permanent;
    }
    classify_storage_error(
        error.code(),
        error.raw_response().map(|r| r.status().as_u16()),
        mutation,
    )
}

/// One retry owner for replayable operations (HEAD/GET/LIST or the same
/// immutable multipart part). Publications and deletion require reconciliation.
#[allow(clippy::result_large_err)] // Preserve the SDK's typed error and response metadata.
pub async fn retry_idempotent<T, E, F, Fut>(
    attempts: u32,
    mut send: F,
) -> Result<T, SdkError<E, HttpResponse>>
where
    E: ProvideErrorMetadata,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, SdkError<E, HttpResponse>>>,
{
    retry_idempotent_with_budget(attempts, Duration::from_secs(30), &mut send).await
}

#[allow(clippy::result_large_err)] // This is an SDK-compatible retry boundary.
pub async fn retry_idempotent_with_budget<T, E, F, Fut>(
    attempts: u32,
    budget: Duration,
    mut send: F,
) -> Result<T, SdkError<E, HttpResponse>>
where
    E: ProvideErrorMetadata,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, SdkError<E, HttpResponse>>>,
{
    let deadline = tokio::time::Instant::now() + budget;
    for attempt in 0..attempts.clamp(1, 3) {
        let result = tokio::time::timeout_at(deadline, send())
            .await
            .unwrap_or_else(|_| {
                Err(SdkError::timeout_error(
                    "Storage operation exhausted its configured deadline",
                ))
            });
        match result {
            Ok(output) => return Ok(output),
            Err(error) => {
                if !is_transient_s3_error(&error) || attempt + 1 >= attempts.clamp(1, 3) {
                    return Err(error);
                }
                let retry_after = error
                    .raw_response()
                    .and_then(|r| r.headers().get("retry-after"))
                    .and_then(|value| {
                        value
                            .parse::<u64>()
                            .ok()
                            .map(Duration::from_secs)
                            .or_else(|| {
                                chrono::DateTime::parse_from_rfc2822(value).ok().map(|at| {
                                    Duration::from_secs(
                                        (at.timestamp() - chrono::Utc::now().timestamp()).max(0)
                                            as u64,
                                    )
                                })
                            })
                    })
                    .unwrap_or_default();
                let jitter = (chrono::Utc::now().timestamp_subsec_nanos() as u64)
                    % ((250u64 << attempt) + 1);
                let wait = Duration::from_millis(jitter).max(retry_after);
                if tokio::time::Instant::now() + wait >= deadline {
                    return Err(error);
                }
                tokio::time::sleep(wait).await;
            }
        }
    }
    unreachable!("bounded retry loop always returns")
}

pub struct S3ClientConfig<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub region: &'a str,
    pub endpoint_url: Option<&'a str>,
    pub force_path_style: bool,
}

pub fn create_s3_client(config: &S3ClientConfig<'_>) -> S3Result<Client> {
    // Credential rotation and every routing option select a different pool.
    // Store a digest, never raw credential material, in the cache index.
    let key = client_key(config);
    let mut clients = CLIENTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((client, used)) = clients.get_mut(&key) {
        *used = Instant::now();
        return Ok(client.clone());
    }
    let credentials = Credentials::new(
        config.access_key_id,
        config.secret_access_key,
        None,
        None,
        "s3-provider",
    );

    let mut builder = S3ConfigBuilder::new()
        .credentials_provider(credentials)
        .region(Region::new(config.region.to_string()))
        // LIST, relay and staging own their classified, cancellable retry
        // budgets. Hidden SDK retries would multiply those attempts and can
        // repeat a mutation whose result is unknown.
        .retry_config(RetryConfig::standard().with_max_attempts(1))
        .timeout_config(
            TimeoutConfig::builder()
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(30))
                .operation_attempt_timeout(Duration::from_secs(120))
                .operation_timeout(Duration::from_secs(120))
                .build(),
        );

    if let Some(endpoint_url) = config.endpoint_url {
        builder = builder.endpoint_url(endpoint_url);
    }

    if config.force_path_style {
        builder = builder.force_path_style(true);
    }

    let s3_config = builder.build();
    let client = Client::from_conf(s3_config);
    if clients.len() >= MAX_CACHED_CLIENTS {
        if let Some(oldest) = clients
            .iter()
            .min_by_key(|(_, (_, used))| *used)
            .map(|(key, _)| *key)
        {
            clients.remove(&oldest);
        }
    }
    clients.insert(key, (client.clone(), Instant::now()));
    Ok(client)
}

fn client_key(config: &S3ClientConfig<'_>) -> [u8; 32] {
    let mut hash = Sha256::new();
    for value in [
        config.access_key_id,
        config.secret_access_key,
        config.region,
        config.endpoint_url.unwrap_or(""),
    ] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    hash.update([
        u8::from(config.endpoint_url.is_some()),
        u8::from(config.force_path_style),
    ]);
    hash.finalize().into()
}

/// One line describing an S3 failure for a person.
///
/// The outermost `Display` of an SDK error is only ever the variant name —
/// "service error", "dispatch failure" — so the service's own code and message
/// win when there is one; that is the part a user can act on. Otherwise the
/// cause chain is walked, which is where a connection failure keeps its
/// explanation.
pub fn describe_s3_error<E, R>(error: &SdkError<E, R>) -> String
where
    E: std::error::Error + ProvideErrorMetadata + 'static,
    R: std::fmt::Debug,
{
    if let Some(message) = error.message() {
        return match error.code() {
            Some(code) => format!("{}: {}", code, message),
            None => message.to_string(),
        };
    }

    let mut description = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        description.push_str(": ");
        description.push_str(&cause.to_string());
        source = cause.source();
    }
    description
}

/// Error codes that mean "try again later" even when the status does not say
/// so — AWS, for one, sends `RequestTimeout` as a 400.
const TRANSIENT_ERROR_CODES: &[&str] = &[
    "InternalError",
    "RequestThrottled",
    "RequestThrottledException",
    "RequestTimeout",
    "RequestTimeoutException",
    "ServiceUnavailable",
    "SlowDown",
    "SlowDownRead",
    "SlowDownWrite",
    "Throttling",
    "ThrottlingException",
    "TooManyRequests",
    "TooManyRequestsException",
];

/// Whether a failure is one that passes on its own — a provider that is briefly
/// unavailable or throttling, or a connection that dropped — rather than one
/// the caller has to change something about: credentials, bucket, the request.
///
/// The HTTP status is the main signal: S3-compatible servers agree on 5xx and
/// 429 for "not now" far more than they agree on error codes, and whatever sits
/// in front of one — Cloudflare's edge, for R2 — answers with 52x statuses and
/// no S3 error code at all. The code list covers the ones sent with some other
/// status.
pub fn is_transient_s3_error<E>(error: &SdkError<E, HttpResponse>) -> bool
where
    E: ProvideErrorMetadata,
{
    match error {
        // Deliberately wider than the SDK's own classifier, which retries
        // neither of these. A LIST is idempotent, so a truncated or unparseable
        // response, and a connector error the transport declined to categorise,
        // are both worth another try. Do not narrow this to match the SDK
        // without checking the caller is still only ever listing.
        SdkError::TimeoutError(_) | SdkError::ResponseError(_) => true,
        // Anything but a user error is the network: a connection that timed
        // out, was refused, or closed before the response was complete.
        SdkError::DispatchFailure(failure) => !failure.is_user(),
        SdkError::ServiceError(service_error) => {
            let status = service_error.raw().status().as_u16();
            // Any server-side failure except "not implemented", which a retry
            // cannot change; 429 is the provider asking for a pause.
            (status >= 500 && status != 501)
                || status == 429
                || service_error
                    .err()
                    .code()
                    .is_some_and(|code| TRANSIENT_ERROR_CODES.contains(&code))
        }
        // The request could not even be built; building it again gives the same one.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{create_s3_client, describe_s3_error, is_transient_s3_error, S3ClientConfig};
    use aws_sdk_s3::config::http::HttpResponse;
    use aws_sdk_s3::error::{ConnectorError, ErrorMetadata, SdkError};
    use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
    use aws_sdk_s3::primitives::SdkBody;

    type ListError = SdkError<ListObjectsV2Error, HttpResponse>;

    #[test]
    fn clients_bound_sdk_retry_and_network_time() {
        let client = create_s3_client(&S3ClientConfig {
            access_key_id: "test",
            secret_access_key: "test-secret",
            region: "us-east-1",
            endpoint_url: Some("http://127.0.0.1:1"),
            force_path_style: true,
        })
        .unwrap();
        let config = client.config();
        assert_eq!(config.retry_config().unwrap().max_attempts(), 1);
        let timeout = config.timeout_config().expect("explicit timeout budget");
        assert!(timeout.operation_timeout().is_some());
        assert!(timeout.operation_attempt_timeout().is_some());
        assert!(timeout.connect_timeout().is_some());
        assert!(timeout.read_timeout().is_some());
    }

    fn service_error(status: u16, code: &str, message: &str) -> ListError {
        let inner = ListObjectsV2Error::generic(
            ErrorMetadata::builder().code(code).message(message).build(),
        );
        let raw = HttpResponse::new(status.try_into().unwrap(), SdkBody::empty());
        SdkError::service_error(inner, raw)
    }

    /// A failure with no S3 error body, the way a proxy or CDN answers.
    fn bare_status(status: u16) -> ListError {
        let inner = ListObjectsV2Error::generic(ErrorMetadata::builder().build());
        let raw = HttpResponse::new(status.try_into().unwrap(), SdkBody::empty());
        SdkError::service_error(inner, raw)
    }

    #[test]
    fn service_errors_show_the_code_and_message_instead_of_service_error() {
        let error = service_error(403, "AccessDenied", "Access Denied");

        assert_eq!(describe_s3_error(&error), "AccessDenied: Access Denied");
    }

    #[test]
    fn other_failures_keep_their_cause_chain() {
        let error: ListError = SdkError::timeout_error("connect took too long");

        assert_eq!(
            describe_s3_error(&error),
            "request has timed out: connect took too long"
        );
    }

    #[test]
    fn a_5xx_is_transient_whatever_the_server_calls_it() {
        assert!(is_transient_s3_error(&service_error(
            503,
            "ServiceUnavailable",
            "The service is unavailable. Please retry."
        )));
        assert!(is_transient_s3_error(&service_error(
            503,
            "XMinioServerNotInitialized",
            "Server not initialized yet, please try again."
        )));
        assert!(is_transient_s3_error(&service_error(
            500,
            "InternalError",
            "We encountered an internal error. Please try again."
        )));
        // Cloudflare's edge in front of R2: "unknown error" and "origin timed out".
        assert!(is_transient_s3_error(&bare_status(520)));
        assert!(is_transient_s3_error(&bare_status(524)));
    }

    #[test]
    fn throttling_and_timeouts_are_transient_even_with_a_4xx_status() {
        assert!(is_transient_s3_error(&service_error(
            400,
            "RequestTimeout",
            "Your socket connection to the server was not read from or written to within the timeout period."
        )));
        assert!(is_transient_s3_error(&service_error(
            429,
            "TooManyRequests",
            "Too Many Requests"
        )));
    }

    #[test]
    fn mistakes_in_the_request_are_not_transient() {
        assert!(!is_transient_s3_error(&service_error(
            403,
            "AccessDenied",
            "Access Denied"
        )));
        assert!(!is_transient_s3_error(&service_error(
            404,
            "NoSuchBucket",
            "The specified bucket does not exist"
        )));
        assert!(!is_transient_s3_error(&service_error(
            501,
            "NotImplemented",
            "A header you provided implies functionality that is not implemented"
        )));
        let unbuildable: ListError = SdkError::construction_failure("no endpoint");
        assert!(!is_transient_s3_error(&unbuildable));
    }

    #[test]
    fn network_failures_are_transient_unless_the_request_itself_is_wrong() {
        let timed_out: ListError = SdkError::timeout_error("connect took too long");
        assert!(is_transient_s3_error(&timed_out));

        let reset: ListError =
            SdkError::dispatch_failure(ConnectorError::io("connection reset".into()));
        assert!(is_transient_s3_error(&reset));

        let closed: ListError = SdkError::dispatch_failure(ConnectorError::other(
            "connection closed before message completed".into(),
            None,
        ));
        assert!(is_transient_s3_error(&closed));

        let unsendable: ListError =
            SdkError::dispatch_failure(ConnectorError::user("body is not replayable".into()));
        assert!(!is_transient_s3_error(&unsendable));
    }
}
