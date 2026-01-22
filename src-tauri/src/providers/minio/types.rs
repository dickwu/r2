use crate::providers::s3_client::{create_s3_client, S3ClientConfig, S3Result};
use aws_sdk_s3::Client;
use serde::{Deserialize, Serialize};

pub type MinioResult<T> = S3Result<T>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinioConfig {
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint_scheme: String,
    pub endpoint_host: String,
    pub force_path_style: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinioObject {
    pub key: String,
    pub size: i64,
    pub last_modified: String,
    pub etag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinioBucket {
    pub name: String,
    pub creation_date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListObjectsResult {
    pub objects: Vec<MinioObject>,
    pub folders: Vec<String>,
    pub truncated: bool,
    pub continuation_token: Option<String>,
}

fn build_endpoint_url(config: &MinioConfig) -> String {
    format!("{}://{}", config.endpoint_scheme, config.endpoint_host)
}

pub async fn create_minio_client(config: &MinioConfig) -> MinioResult<Client> {
    let endpoint_url = build_endpoint_url(config);
    let client = create_s3_client(&S3ClientConfig {
        access_key_id: &config.access_key_id,
        secret_access_key: &config.secret_access_key,
        region: "us-east-1",
        endpoint_url: Some(endpoint_url.as_str()),
        force_path_style: config.force_path_style,
    })?;

    Ok(client)
}
