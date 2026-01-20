use super::minio_commands::{
    self, ListObjectsInput as MinioListObjectsInput, MinioConfigInput, SyncResult, UploadResult,
};
use crate::providers::rustfs;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct RustfsConfigInput {
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint_scheme: String,
    pub endpoint_host: String,
    pub force_path_style: bool,
}

impl From<RustfsConfigInput> for MinioConfigInput {
    fn from(input: RustfsConfigInput) -> Self {
        let _ = input.force_path_style;
        MinioConfigInput {
            account_id: input.account_id,
            bucket: input.bucket,
            access_key_id: input.access_key_id,
            secret_access_key: input.secret_access_key,
            endpoint_scheme: input.endpoint_scheme,
            endpoint_host: input.endpoint_host,
            force_path_style: true,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListObjectsInput {
    pub config: RustfsConfigInput,
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    pub continuation_token: Option<String>,
    pub max_keys: Option<i32>,
}

fn to_minio_list_input(input: ListObjectsInput) -> MinioListObjectsInput {
    MinioListObjectsInput {
        config: input.config.into(),
        prefix: input.prefix,
        delimiter: input.delimiter,
        continuation_token: input.continuation_token,
        max_keys: input.max_keys,
    }
}

#[tauri::command]
pub async fn list_rustfs_buckets(
    account_id: String,
    access_key_id: String,
    secret_access_key: String,
    endpoint_scheme: String,
    endpoint_host: String,
    _force_path_style: bool,
) -> Result<Vec<rustfs::RustfsBucket>, String> {
    minio_commands::list_minio_buckets(
        account_id,
        access_key_id,
        secret_access_key,
        endpoint_scheme,
        endpoint_host,
        true,
    )
    .await
}

#[tauri::command]
pub async fn list_rustfs_objects(input: ListObjectsInput) -> Result<rustfs::ListObjectsResult, String> {
    minio_commands::list_minio_objects(to_minio_list_input(input)).await
}

#[tauri::command]
pub async fn list_all_rustfs_objects(
    config: RustfsConfigInput,
    app: tauri::AppHandle,
) -> Result<Vec<rustfs::RustfsObject>, String> {
    minio_commands::list_all_minio_objects(config.into(), app).await
}

#[tauri::command]
pub async fn sync_rustfs_bucket(
    config: RustfsConfigInput,
    app: tauri::AppHandle,
) -> Result<SyncResult, String> {
    minio_commands::sync_minio_bucket(config.into(), app).await
}

#[tauri::command]
pub async fn list_folder_rustfs_objects(
    config: RustfsConfigInput,
    prefix: Option<String>,
    app: tauri::AppHandle,
) -> Result<rustfs::ListObjectsResult, String> {
    minio_commands::list_folder_minio_objects(config.into(), prefix, app).await
}

#[tauri::command]
pub async fn delete_rustfs_object(
    config: RustfsConfigInput,
    key: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    minio_commands::delete_minio_object(config.into(), key, app).await
}

#[tauri::command]
pub async fn batch_delete_rustfs_objects(
    config: RustfsConfigInput,
    keys: Vec<String>,
    app: tauri::AppHandle,
) -> Result<minio_commands::BatchDeleteResult, String> {
    minio_commands::batch_delete_minio_objects(config.into(), keys, app).await
}

#[tauri::command]
pub async fn rename_rustfs_object(
    config: RustfsConfigInput,
    old_key: String,
    new_key: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    minio_commands::rename_minio_object(config.into(), old_key, new_key, app).await
}

#[tauri::command]
pub async fn batch_move_rustfs_objects(
    config: RustfsConfigInput,
    operations: Vec<minio_commands::MoveOperation>,
    app: tauri::AppHandle,
) -> Result<minio_commands::BatchMoveResult, String> {
    minio_commands::batch_move_minio_objects(config.into(), operations, app).await
}

#[tauri::command]
pub async fn generate_rustfs_signed_url(
    config: RustfsConfigInput,
    key: String,
    expires_in_secs: Option<u64>,
) -> Result<String, String> {
    minio_commands::generate_minio_signed_url(config.into(), key, expires_in_secs).await
}

#[tauri::command]
pub async fn upload_rustfs_content(
    config: RustfsConfigInput,
    key: String,
    content: String,
    content_type: Option<String>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    minio_commands::upload_minio_content(config.into(), key, content, content_type, app).await
}

#[tauri::command]
pub async fn upload_rustfs_file(
    app: tauri::AppHandle,
    task_id: String,
    file_path: String,
    key: String,
    content_type: Option<String>,
    account_id: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
    endpoint_scheme: String,
    endpoint_host: String,
    _force_path_style: bool,
) -> Result<UploadResult, String> {
    minio_commands::upload_minio_file(
        app,
        task_id,
        file_path,
        key,
        content_type,
        account_id,
        bucket,
        access_key_id,
        secret_access_key,
        endpoint_scheme,
        endpoint_host,
        true,
    )
    .await
}
