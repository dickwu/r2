//! R2 presigned URL generation

use super::types::{create_r2_client, R2Config, R2Result};
use aws_sdk_s3::presigning::PresigningConfig;
use std::time::Duration;

/// Generate a presigned URL for object access
pub async fn generate_presigned_url(
    config: &R2Config,
    key: &str,
    expires_in_secs: u64,
) -> R2Result<String> {
    let client = create_r2_client(config).await?;

    let presigning_config = PresigningConfig::builder()
        .expires_in(Duration::from_secs(expires_in_secs))
        .build()?;

    let presigned_request = client
        .get_object()
        .bucket(&config.bucket)
        .key(key)
        .presigned(presigning_config)
        .await?;

    Ok(presigned_request.uri().to_string())
}

/// Generate a presigned URL for uploading an object (PUT)
pub async fn generate_presigned_put_url(
    config: &R2Config,
    key: &str,
    expires_in_secs: u64,
) -> R2Result<String> {
    let client = create_r2_client(config).await?;

    let presigning_config = PresigningConfig::builder()
        .expires_in(Duration::from_secs(expires_in_secs))
        .build()?;

    let presigned_request = client
        .put_object()
        .bucket(&config.bucket)
        .key(key)
        .presigned(presigning_config)
        .await?;

    Ok(presigned_request.uri().to_string())
}
