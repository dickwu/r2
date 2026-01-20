use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
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
