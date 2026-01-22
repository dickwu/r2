use crate::providers::s3_client::{create_s3_client, S3ClientConfig, S3Result};
use aws_sdk_s3::Client;
use serde::{Deserialize, Serialize};

pub type AwsResult<T> = S3Result<T>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsConfig {
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
    pub endpoint_scheme: Option<String>,
    pub endpoint_host: Option<String>,
    pub force_path_style: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsObject {
    pub key: String,
    pub size: i64,
    pub last_modified: String,
    pub etag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsBucket {
    pub name: String,
    pub creation_date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListObjectsResult {
    pub objects: Vec<AwsObject>,
    pub folders: Vec<String>,
    pub truncated: bool,
    pub continuation_token: Option<String>,
}

fn build_endpoint_url(config: &AwsConfig) -> Option<String> {
    let host = config.endpoint_host.as_ref()?.trim();
    if host.is_empty() {
        return None;
    }
    let scheme = config.endpoint_scheme.as_deref().unwrap_or("https");
    Some(format!("{}://{}", scheme, host))
}

pub async fn create_aws_client(config: &AwsConfig) -> AwsResult<Client> {
    let endpoint_url = build_endpoint_url(config);
    let client = create_s3_client(&S3ClientConfig {
        access_key_id: &config.access_key_id,
        secret_access_key: &config.secret_access_key,
        region: &config.region,
        endpoint_url: endpoint_url.as_deref(),
        force_path_style: config.force_path_style,
    })?;

    Ok(client)
}
