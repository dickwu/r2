use aws_config::Region;
use aws_credential_types::Credentials;
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

#[cfg(test)]
mod tests {
    use super::describe_s3_error;
    use aws_sdk_s3::config::http::HttpResponse;
    use aws_sdk_s3::error::{ErrorMetadata, SdkError};
    use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
    use aws_sdk_s3::primitives::SdkBody;

    type ListError = SdkError<ListObjectsV2Error, HttpResponse>;

    #[test]
    fn service_errors_show_the_code_and_message_instead_of_service_error() {
        let inner = ListObjectsV2Error::generic(
            ErrorMetadata::builder()
                .code("AccessDenied")
                .message("Access Denied")
                .build(),
        );
        let raw = HttpResponse::new(403.try_into().unwrap(), SdkBody::empty());
        let error: ListError = SdkError::service_error(inner, raw);

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
}
