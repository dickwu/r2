use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::Client;

pub type S3Result<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub struct S3ClientConfig<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub region: &'a str,
    pub endpoint_url: Option<&'a str>,
    pub force_path_style: bool,
}

pub fn create_s3_client(config: &S3ClientConfig<'_>) -> S3Result<Client> {
    let credentials = Credentials::new(
        config.access_key_id,
        config.secret_access_key,
        None,
        None,
        "s3-provider",
    );

    let mut builder = S3ConfigBuilder::new()
        .credentials_provider(credentials)
        .region(Region::new(config.region.to_string()));

    if let Some(endpoint_url) = config.endpoint_url {
        builder = builder.endpoint_url(endpoint_url);
    }

    if config.force_path_style {
        builder = builder.force_path_style(true);
    }

    let s3_config = builder.build();
    Ok(Client::from_conf(s3_config))
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
    use super::{describe_s3_error, is_transient_s3_error};
    use aws_sdk_s3::config::http::HttpResponse;
    use aws_sdk_s3::error::{ConnectorError, ErrorMetadata, SdkError};
    use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
    use aws_sdk_s3::primitives::SdkBody;

    type ListError = SdkError<ListObjectsV2Error, HttpResponse>;

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
