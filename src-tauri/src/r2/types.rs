//! R2 types and client creation

use aws_sdk_s3::Client;
use serde::{Deserialize, Serialize};
use crate::providers::s3_client::{create_s3_client, S3ClientConfig};

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
    let endpoint_url = format!("https://{}.r2.cloudflarestorage.com", config.account_id);
    let client = create_s3_client(&S3ClientConfig {
        access_key_id: &config.access_key_id,
        secret_access_key: &config.secret_access_key,
        region: "auto",
        endpoint_url: Some(endpoint_url.as_str()),
        force_path_style: true,
    })?;

    Ok(client)
}
