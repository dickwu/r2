use crate::db;
use serde::{Deserialize, Serialize};

// ============ Account Commands ============

#[tauri::command]
pub async fn list_accounts() -> Result<Vec<db::Account>, String> {
    db::list_accounts().await.map_err(|e| format!("Failed to list accounts: {}", e))
}

#[tauri::command]
pub async fn create_account(id: String, name: Option<String>) -> Result<db::Account, String> {
    db::create_account(&id, name.as_deref()).await.map_err(|e| format!("Failed to create account: {}", e))
}

#[tauri::command]
pub async fn update_account(id: String, name: Option<String>) -> Result<(), String> {
    db::update_account(&id, name.as_deref()).await.map_err(|e| format!("Failed to update account: {}", e))
}

#[tauri::command]
pub async fn delete_account(id: String) -> Result<(), String> {
    db::delete_account(&id).await.map_err(|e| format!("Failed to delete account: {}", e))
}

// ============ Token Commands ============

#[derive(Debug, Deserialize)]
pub struct CreateTokenInput {
    pub account_id: String,
    pub name: Option<String>,
    pub api_token: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[tauri::command]
pub async fn list_tokens(account_id: String) -> Result<Vec<db::Token>, String> {
    db::list_tokens_by_account(&account_id).await.map_err(|e| format!("Failed to list tokens: {}", e))
}

#[tauri::command]
pub async fn create_token(input: CreateTokenInput) -> Result<db::Token, String> {
    db::create_token(
        &input.account_id,
        input.name.as_deref(),
        &input.api_token,
        &input.access_key_id,
        &input.secret_access_key,
    )
    .await
    .map_err(|e| format!("Failed to create token: {}", e))
}

#[derive(Debug, Deserialize)]
pub struct UpdateTokenInput {
    pub id: i64,
    pub name: Option<String>,
    pub api_token: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[tauri::command]
pub async fn update_token(input: UpdateTokenInput) -> Result<(), String> {
    db::update_token(
        input.id,
        input.name.as_deref(),
        &input.api_token,
        &input.access_key_id,
        &input.secret_access_key,
    )
    .await
    .map_err(|e| format!("Failed to update token: {}", e))
}

#[tauri::command]
pub async fn delete_token(id: i64) -> Result<(), String> {
    db::delete_token(id).await.map_err(|e| format!("Failed to delete token: {}", e))
}

#[tauri::command]
pub async fn get_token(id: i64) -> Result<Option<db::Token>, String> {
    db::get_token(id).await.map_err(|e| format!("Failed to get token: {}", e))
}

// ============ Bucket Commands ============

#[tauri::command]
pub async fn list_buckets(token_id: i64) -> Result<Vec<db::Bucket>, String> {
    db::list_buckets_by_token(token_id).await.map_err(|e| format!("Failed to list buckets: {}", e))
}

#[derive(Debug, Deserialize)]
pub struct BucketInput {
    pub name: String,
    pub public_domain: Option<String>,
    pub public_domain_scheme: Option<String>,
}

#[tauri::command]
pub async fn save_buckets(token_id: i64, buckets: Vec<BucketInput>) -> Result<Vec<db::Bucket>, String> {
    let bucket_data: Vec<(String, Option<String>, Option<String>)> = buckets
        .into_iter()
        .map(|b| (b.name, b.public_domain, b.public_domain_scheme))
        .collect();
    
    db::save_buckets_for_token(token_id, &bucket_data)
        .await
        .map_err(|e| format!("Failed to save buckets: {}", e))
}

#[tauri::command]
pub async fn update_bucket(
    id: i64,
    public_domain: Option<String>,
    public_domain_scheme: Option<String>,
) -> Result<(), String> {
    db::update_bucket(id, public_domain.as_deref(), public_domain_scheme.as_deref())
        .await
        .map_err(|e| format!("Failed to update bucket: {}", e))
}

#[tauri::command]
pub async fn delete_bucket(id: i64) -> Result<(), String> {
    db::delete_bucket(id).await.map_err(|e| format!("Failed to delete bucket: {}", e))
}

// ============ AWS Account Commands ============

#[derive(Debug, Deserialize)]
pub struct CreateAwsAccountInput {
    pub name: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
    pub endpoint_scheme: Option<String>,
    pub endpoint_host: Option<String>,
    pub force_path_style: bool,
}

#[tauri::command]
pub async fn list_aws_accounts() -> Result<Vec<db::AwsAccount>, String> {
    db::list_aws_accounts().await.map_err(|e| format!("Failed to list AWS accounts: {}", e))
}

#[tauri::command]
pub async fn create_aws_account(input: CreateAwsAccountInput) -> Result<db::AwsAccount, String> {
    db::create_aws_account(
        input.name.as_deref(),
        &input.access_key_id,
        &input.secret_access_key,
        &input.region,
        input.endpoint_scheme.as_deref().unwrap_or("https"),
        input.endpoint_host.as_deref(),
        input.force_path_style,
    )
    .await
    .map_err(|e| format!("Failed to create AWS account: {}", e))
}

#[derive(Debug, Deserialize)]
pub struct UpdateAwsAccountInput {
    pub id: String,
    pub name: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
    pub endpoint_scheme: Option<String>,
    pub endpoint_host: Option<String>,
    pub force_path_style: bool,
}

#[tauri::command]
pub async fn update_aws_account(input: UpdateAwsAccountInput) -> Result<(), String> {
    db::update_aws_account(
        &input.id,
        input.name.as_deref(),
        &input.access_key_id,
        &input.secret_access_key,
        &input.region,
        input.endpoint_scheme.as_deref().unwrap_or("https"),
        input.endpoint_host.as_deref(),
        input.force_path_style,
    )
    .await
    .map_err(|e| format!("Failed to update AWS account: {}", e))
}

#[tauri::command]
pub async fn delete_aws_account(id: String) -> Result<(), String> {
    db::delete_aws_account(&id)
        .await
        .map_err(|e| format!("Failed to delete AWS account: {}", e))
}

#[derive(Debug, Deserialize)]
pub struct AwsBucketInput {
    pub name: String,
    pub public_domain_scheme: Option<String>,
    pub public_domain_host: Option<String>,
}

#[tauri::command]
pub async fn list_aws_bucket_configs(account_id: String) -> Result<Vec<db::AwsBucket>, String> {
    db::list_aws_buckets_by_account(&account_id)
        .await
        .map_err(|e| format!("Failed to list AWS bucket configs: {}", e))
}

#[tauri::command]
pub async fn save_aws_bucket_configs(
    account_id: String,
    buckets: Vec<AwsBucketInput>,
) -> Result<Vec<db::AwsBucket>, String> {
    let bucket_data: Vec<(String, Option<String>, Option<String>)> = buckets
        .into_iter()
        .map(|b| (b.name, b.public_domain_scheme, b.public_domain_host))
        .collect();

    db::save_aws_buckets_for_account(&account_id, &bucket_data)
        .await
        .map_err(|e| format!("Failed to save AWS bucket configs: {}", e))
}

// ============ MinIO Account Commands ============

#[derive(Debug, Deserialize)]
pub struct CreateMinioAccountInput {
    pub name: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint_scheme: String,
    pub endpoint_host: String,
    pub force_path_style: bool,
}

#[tauri::command]
pub async fn list_minio_accounts() -> Result<Vec<db::MinioAccount>, String> {
    db::list_minio_accounts().await.map_err(|e| format!("Failed to list MinIO accounts: {}", e))
}

#[tauri::command]
pub async fn create_minio_account(input: CreateMinioAccountInput) -> Result<db::MinioAccount, String> {
    db::create_minio_account(
        input.name.as_deref(),
        &input.access_key_id,
        &input.secret_access_key,
        &input.endpoint_scheme,
        &input.endpoint_host,
        input.force_path_style,
    )
    .await
    .map_err(|e| format!("Failed to create MinIO account: {}", e))
}

#[derive(Debug, Deserialize)]
pub struct UpdateMinioAccountInput {
    pub id: String,
    pub name: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint_scheme: String,
    pub endpoint_host: String,
    pub force_path_style: bool,
}

#[tauri::command]
pub async fn update_minio_account(input: UpdateMinioAccountInput) -> Result<(), String> {
    db::update_minio_account(
        &input.id,
        input.name.as_deref(),
        &input.access_key_id,
        &input.secret_access_key,
        &input.endpoint_scheme,
        &input.endpoint_host,
        input.force_path_style,
    )
    .await
    .map_err(|e| format!("Failed to update MinIO account: {}", e))
}

#[tauri::command]
pub async fn delete_minio_account(id: String) -> Result<(), String> {
    db::delete_minio_account(&id)
        .await
        .map_err(|e| format!("Failed to delete MinIO account: {}", e))
}

#[derive(Debug, Deserialize)]
pub struct MinioBucketInput {
    pub name: String,
    pub public_domain_scheme: Option<String>,
    pub public_domain_host: Option<String>,
}

#[tauri::command]
pub async fn list_minio_bucket_configs(account_id: String) -> Result<Vec<db::MinioBucket>, String> {
    db::list_minio_buckets_by_account(&account_id)
        .await
        .map_err(|e| format!("Failed to list MinIO bucket configs: {}", e))
}

#[tauri::command]
pub async fn save_minio_bucket_configs(
    account_id: String,
    buckets: Vec<MinioBucketInput>,
) -> Result<Vec<db::MinioBucket>, String> {
    let bucket_data: Vec<(String, Option<String>, Option<String>)> = buckets
        .into_iter()
        .map(|b| (b.name, b.public_domain_scheme, b.public_domain_host))
        .collect();

    db::save_minio_buckets_for_account(&account_id, &bucket_data)
        .await
        .map_err(|e| format!("Failed to save MinIO bucket configs: {}", e))
}

// ============ RustFS Account Commands ============

#[derive(Debug, Deserialize)]
pub struct CreateRustfsAccountInput {
    pub name: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint_scheme: String,
    pub endpoint_host: String,
}

#[tauri::command]
pub async fn list_rustfs_accounts() -> Result<Vec<db::RustfsAccount>, String> {
    db::list_rustfs_accounts().await.map_err(|e| format!("Failed to list RustFS accounts: {}", e))
}

#[tauri::command]
pub async fn create_rustfs_account(
    input: CreateRustfsAccountInput,
) -> Result<db::RustfsAccount, String> {
    db::create_rustfs_account(
        input.name.as_deref(),
        &input.access_key_id,
        &input.secret_access_key,
        &input.endpoint_scheme,
        &input.endpoint_host,
        true,
    )
    .await
    .map_err(|e| format!("Failed to create RustFS account: {}", e))
}

#[derive(Debug, Deserialize)]
pub struct UpdateRustfsAccountInput {
    pub id: String,
    pub name: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint_scheme: String,
    pub endpoint_host: String,
}

#[tauri::command]
pub async fn update_rustfs_account(input: UpdateRustfsAccountInput) -> Result<(), String> {
    db::update_rustfs_account(
        &input.id,
        input.name.as_deref(),
        &input.access_key_id,
        &input.secret_access_key,
        &input.endpoint_scheme,
        &input.endpoint_host,
        true,
    )
    .await
    .map_err(|e| format!("Failed to update RustFS account: {}", e))
}

#[tauri::command]
pub async fn delete_rustfs_account(id: String) -> Result<(), String> {
    db::delete_rustfs_account(&id)
        .await
        .map_err(|e| format!("Failed to delete RustFS account: {}", e))
}

#[derive(Debug, Deserialize)]
pub struct RustfsBucketInput {
    pub name: String,
    pub public_domain_scheme: Option<String>,
    pub public_domain_host: Option<String>,
}

#[tauri::command]
pub async fn list_rustfs_bucket_configs(
    account_id: String,
) -> Result<Vec<db::RustfsBucket>, String> {
    db::list_rustfs_buckets_by_account(&account_id)
        .await
        .map_err(|e| format!("Failed to list RustFS bucket configs: {}", e))
}

#[tauri::command]
pub async fn save_rustfs_bucket_configs(
    account_id: String,
    buckets: Vec<RustfsBucketInput>,
) -> Result<Vec<db::RustfsBucket>, String> {
    let bucket_data: Vec<(String, Option<String>, Option<String>)> = buckets
        .into_iter()
        .map(|b| (b.name, b.public_domain_scheme, b.public_domain_host))
        .collect();

    db::save_rustfs_buckets_for_account(&account_id, &bucket_data)
        .await
        .map_err(|e| format!("Failed to save RustFS bucket configs: {}", e))
}

// ============ State Commands ============

#[tauri::command]
pub async fn get_current_config() -> Result<Option<db::CurrentConfig>, String> {
    db::get_current_config().await.map_err(|e| format!("Failed to get current config: {}", e))
}

#[tauri::command]
pub async fn set_current_token(token_id: i64, bucket_name: String) -> Result<(), String> {
    db::set_current_selection(token_id, &bucket_name)
        .await
        .map_err(|e| format!("Failed to set current token: {}", e))
}

#[tauri::command]
pub async fn set_current_aws_bucket(account_id: String, bucket_name: String) -> Result<(), String> {
    db::set_current_aws_selection(&account_id, &bucket_name)
        .await
        .map_err(|e| format!("Failed to set current AWS bucket: {}", e))
}

#[tauri::command]
pub async fn set_current_minio_bucket(account_id: String, bucket_name: String) -> Result<(), String> {
    db::set_current_minio_selection(&account_id, &bucket_name)
        .await
        .map_err(|e| format!("Failed to set current MinIO bucket: {}", e))
}

#[tauri::command]
pub async fn set_current_rustfs_bucket(account_id: String, bucket_name: String) -> Result<(), String> {
    db::set_current_rustfs_selection(&account_id, &bucket_name)
        .await
        .map_err(|e| format!("Failed to set current RustFS bucket: {}", e))
}

#[tauri::command]
pub async fn set_current_bucket(bucket_name: String) -> Result<(), String> {
    db::set_app_state("current_bucket", &bucket_name)
        .await
        .map_err(|e| format!("Failed to set current bucket: {}", e))
}

#[tauri::command]
pub async fn has_accounts() -> Result<bool, String> {
    db::has_accounts().await.map_err(|e| format!("Failed to check accounts: {}", e))
}

// ============ Full Account Data (for sidebar) ============

#[derive(Debug, Serialize)]
pub struct AccountWithTokens {
    pub account: db::Account,
    pub tokens: Vec<TokenWithBuckets>,
}

#[derive(Debug, Serialize)]
pub struct TokenWithBuckets {
    pub token: db::Token,
    pub buckets: Vec<db::Bucket>,
}

#[derive(Debug, Serialize)]
pub struct AwsAccountWithBuckets {
    pub account: db::AwsAccount,
    pub buckets: Vec<db::AwsBucket>,
}

#[derive(Debug, Serialize)]
pub struct MinioAccountWithBuckets {
    pub account: db::MinioAccount,
    pub buckets: Vec<db::MinioBucket>,
}

#[derive(Debug, Serialize)]
pub struct RustfsAccountWithBuckets {
    pub account: db::RustfsAccount,
    pub buckets: Vec<db::RustfsBucket>,
}

#[tauri::command]
pub async fn get_all_accounts_with_tokens() -> Result<Vec<AccountWithTokens>, String> {
    let accounts = db::list_accounts().await.map_err(|e| format!("Failed to list accounts: {}", e))?;
    
    let mut result = Vec::new();
    for account in accounts {
        let tokens = db::list_tokens_by_account(&account.id)
            .await
            .map_err(|e| format!("Failed to list tokens: {}", e))?;
        
        let mut tokens_with_buckets = Vec::new();
        for token in tokens {
            let buckets = db::list_buckets_by_token(token.id)
                .await
                .map_err(|e| format!("Failed to list buckets: {}", e))?;
            tokens_with_buckets.push(TokenWithBuckets { token, buckets });
        }
        
        result.push(AccountWithTokens {
            account,
            tokens: tokens_with_buckets,
        });
    }
    
    Ok(result)
}

#[tauri::command]
pub async fn get_all_aws_accounts_with_buckets() -> Result<Vec<AwsAccountWithBuckets>, String> {
    let accounts = db::list_aws_accounts().await.map_err(|e| format!("Failed to list AWS accounts: {}", e))?;

    let mut result = Vec::new();
    for account in accounts {
        let buckets = db::list_aws_buckets_by_account(&account.id)
            .await
            .map_err(|e| format!("Failed to list AWS bucket configs: {}", e))?;
        result.push(AwsAccountWithBuckets { account, buckets });
    }

    Ok(result)
}

#[tauri::command]
pub async fn get_all_minio_accounts_with_buckets() -> Result<Vec<MinioAccountWithBuckets>, String> {
    let accounts = db::list_minio_accounts().await.map_err(|e| format!("Failed to list MinIO accounts: {}", e))?;

    let mut result = Vec::new();
    for account in accounts {
        let buckets = db::list_minio_buckets_by_account(&account.id)
            .await
            .map_err(|e| format!("Failed to list MinIO bucket configs: {}", e))?;
        result.push(MinioAccountWithBuckets { account, buckets });
    }

    Ok(result)
}

#[tauri::command]
pub async fn get_all_rustfs_accounts_with_buckets() -> Result<Vec<RustfsAccountWithBuckets>, String> {
    let accounts = db::list_rustfs_accounts().await.map_err(|e| format!("Failed to list RustFS accounts: {}", e))?;

    let mut result = Vec::new();
    for account in accounts {
        let buckets = db::list_rustfs_buckets_by_account(&account.id)
            .await
            .map_err(|e| format!("Failed to list RustFS buckets: {}", e))?;
        result.push(RustfsAccountWithBuckets { account, buckets });
    }

    Ok(result)
}
