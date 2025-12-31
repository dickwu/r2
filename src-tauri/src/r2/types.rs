//! R2 types and client creation

use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::Client;
use serde::{Deserialize, Serialize};

pub type R2Result<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct R2Config {
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct R2Object {
    pub key: String,
    pub size: i64,
    pub last_modified: String,
    pub etag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct R2Bucket {
    pub name: String,
    pub creation_date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListObjectsResult {
    pub objects: Vec<R2Object>,
    pub folders: Vec<String>,
    pub truncated: bool,
    pub continuation_token: Option<String>,
}

/// Create an S3 client configured for Cloudflare R2
pub async fn create_r2_client(config: &R2Config) -> R2Result<Client> {
    let credentials = Credentials::new(
        &config.access_key_id,
        &config.secret_access_key,
        None,
        None,
        "r2-provider",
    );

    let endpoint_url = format!("https://{}.r2.cloudflarestorage.com", config.account_id);

    let s3_config = S3ConfigBuilder::new()
        .credentials_provider(credentials)
        .region(Region::new("auto"))
        .endpoint_url(endpoint_url)
        .force_path_style(true)
        .build();

    Ok(Client::from_conf(s3_config))
}
