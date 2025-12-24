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
}

#[tauri::command]
pub async fn save_buckets(token_id: i64, buckets: Vec<BucketInput>) -> Result<Vec<db::Bucket>, String> {
    let bucket_data: Vec<(String, Option<String>)> = buckets
        .into_iter()
        .map(|b| (b.name, b.public_domain))
        .collect();
    
    db::save_buckets_for_token(token_id, &bucket_data)
        .await
        .map_err(|e| format!("Failed to save buckets: {}", e))
}

#[tauri::command]
pub async fn update_bucket(id: i64, public_domain: Option<String>) -> Result<(), String> {
    db::update_bucket(id, public_domain.as_deref())
        .await
        .map_err(|e| format!("Failed to update bucket: {}", e))
}

#[tauri::command]
pub async fn delete_bucket(id: i64) -> Result<(), String> {
    db::delete_bucket(id).await.map_err(|e| format!("Failed to delete bucket: {}", e))
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
