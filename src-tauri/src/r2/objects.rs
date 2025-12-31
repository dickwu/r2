//! R2 object operations (delete, copy, rename)

use super::types::{create_r2_client, R2Config, R2Result};
use aws_sdk_s3::types::{Delete, ObjectIdentifier};

/// Delete a single object
pub async fn delete_object(config: &R2Config, key: &str) -> R2Result<()> {
    let client = create_r2_client(config).await?;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(key)
        .send()
        .await?;
    Ok(())
}

/// Delete multiple objects in batch
#[allow(dead_code)]
pub async fn delete_objects(config: &R2Config, keys: Vec<String>) -> R2Result<()> {
    if keys.is_empty() {
        return Ok(());
    }

    let client = create_r2_client(config).await?;

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

/// Copy an object (used for rename)
pub async fn copy_object(config: &R2Config, source_key: &str, dest_key: &str) -> R2Result<()> {
    let client = create_r2_client(config).await?;
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

/// Rename an object (copy then delete)
pub async fn rename_object(config: &R2Config, old_key: &str, new_key: &str) -> R2Result<()> {
    copy_object(config, old_key, new_key).await?;
    delete_object(config, old_key).await?;
    Ok(())
}
