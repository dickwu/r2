use super::types::{create_aws_client, AwsConfig, AwsResult};
use aws_sdk_s3::types::{Delete, ObjectIdentifier};

pub async fn delete_object(config: &AwsConfig, key: &str) -> AwsResult<()> {
    let client = create_aws_client(config).await?;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(key)
        .send()
        .await?;
    Ok(())
}

pub async fn delete_objects(config: &AwsConfig, keys: Vec<String>) -> AwsResult<()> {
    if keys.is_empty() {
        return Ok(());
    }

    let client = create_aws_client(config).await?;

    let objects: Vec<ObjectIdentifier> = keys
        .iter()
        .map(|key| ObjectIdentifier::builder().key(key).build().unwrap())
        .collect();

    let delete = Delete::builder().set_objects(Some(objects)).build()?;

    client
        .delete_objects()
        .bucket(&config.bucket)
        .delete(delete)
        .send()
        .await?;

    Ok(())
}

pub async fn copy_object(config: &AwsConfig, source_key: &str, dest_key: &str) -> AwsResult<()> {
    let client = create_aws_client(config).await?;
    let copy_source = format!("{}/{}", config.bucket, source_key);

    client
        .copy_object()
        .bucket(&config.bucket)
        .copy_source(copy_source)
        .key(dest_key)
        .send()
        .await?;

    Ok(())
}

pub async fn rename_object(config: &AwsConfig, old_key: &str, new_key: &str) -> AwsResult<()> {
    copy_object(config, old_key, new_key).await?;
    delete_object(config, old_key).await?;
    Ok(())
}
