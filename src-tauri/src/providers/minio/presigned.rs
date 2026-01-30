use super::types::{create_minio_client, MinioConfig, MinioResult};
use aws_sdk_s3::presigning::PresigningConfig;
use std::time::Duration;

pub async fn generate_presigned_url(
    config: &MinioConfig,
    key: &str,
    expires_in_secs: u64,
) -> MinioResult<String> {
    let client = create_minio_client(config).await?;

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

pub async fn generate_presigned_put_url(
    config: &MinioConfig,
    key: &str,
    expires_in_secs: u64,
) -> MinioResult<String> {
    let client = create_minio_client(config).await?;

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
