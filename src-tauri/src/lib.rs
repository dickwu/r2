use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::Manager;

mod account;
mod db;
mod upload;

/// Old R2Config format (from tauri-plugin-store)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OldBucketConfig {
    name: String,
    #[serde(rename = "publicDomain")]
    public_domain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OldR2Config {
    #[serde(rename = "accountId")]
    account_id: String,
    token: String,
    #[serde(rename = "accessKeyId")]
    access_key_id: Option<String>,
    #[serde(rename = "secretAccessKey")]
    secret_access_key: Option<String>,
    bucket: String,
    buckets: Option<Vec<OldBucketConfig>>,
    #[serde(rename = "publicDomain")]
    public_domain: Option<String>,
}

/// Migrate from old store format to SQLite
fn migrate_from_store(app_data_dir: &PathBuf) {
    // Check if migration is needed
    match db::has_accounts() {
        Ok(true) => {
            log::info!("Accounts already exist, skipping migration");
            return;
        }
        Ok(false) => {
            log::info!("No accounts found, checking for old store...");
        }
        Err(e) => {
            log::error!("Failed to check accounts: {}", e);
            return;
        }
    }

    // Try to read old store file
    let store_path = app_data_dir.join("r2-config.json");
    if !store_path.exists() {
        log::info!("No old store file found at {:?}", store_path);
        return;
    }

    log::info!("Found old store file, migrating...");

    // Read and parse the store file
    let store_content = match std::fs::read_to_string(&store_path) {
        Ok(content) => content,
        Err(e) => {
            log::error!("Failed to read store file: {}", e);
            return;
        }
    };

    // The store file contains a JSON object with a "config" key
    #[derive(Deserialize)]
    struct StoreWrapper {
        config: Option<OldR2Config>,
    }

    let store: StoreWrapper = match serde_json::from_str(&store_content) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to parse store file: {}", e);
            return;
        }
    };

    let old_config = match store.config {
        Some(c) => c,
        None => {
            log::info!("No config in store file");
            return;
        }
    };

    // Validate required fields
    let access_key_id = match &old_config.access_key_id {
        Some(k) if !k.is_empty() => k.clone(),
        _ => {
            log::warn!("Old config missing access_key_id, skipping migration");
            return;
        }
    };

    let secret_access_key = match &old_config.secret_access_key {
        Some(s) if !s.is_empty() => s.clone(),
        _ => {
            log::warn!("Old config missing secret_access_key, skipping migration");
            return;
        }
    };

    // Create account
    if let Err(e) = db::create_account(&old_config.account_id, None) {
        log::error!("Failed to create account: {}", e);
        return;
    }
    log::info!("Created account: {}", old_config.account_id);

    // Create token
    let token = match db::create_token(
        &old_config.account_id,
        Some("Default"),
        &old_config.token,
        &access_key_id,
        &secret_access_key,
    ) {
        Ok(t) => t,
        Err(e) => {
            log::error!("Failed to create token: {}", e);
            return;
        }
    };
    log::info!("Created token with ID: {}", token.id);

    // Create buckets
    let buckets: Vec<(String, Option<String>)> = if let Some(old_buckets) = &old_config.buckets {
        old_buckets
            .iter()
            .map(|b| (b.name.clone(), b.public_domain.clone()))
            .collect()
    } else {
        // Just use the current bucket
        vec![(old_config.bucket.clone(), old_config.public_domain.clone())]
    };

    if let Err(e) = db::save_buckets_for_token(token.id, &buckets) {
        log::error!("Failed to save buckets: {}", e);
        return;
    }
    log::info!("Created {} bucket(s)", buckets.len());

    // Set current selection
    if let Err(e) = db::set_current_selection(token.id, &old_config.bucket) {
        log::error!("Failed to set current selection: {}", e);
        return;
    }

    log::info!("Migration completed successfully!");
    
    // Optionally rename the old store file to mark it as migrated
    let backup_path = app_data_dir.join("r2-config.json.migrated");
    if let Err(e) = std::fs::rename(&store_path, &backup_path) {
        log::warn!("Failed to rename old store file: {}", e);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_video_thumbnail::init())
        .setup(|app| {
            // Initialize database in app data directory
            let app_data_dir = app.path().app_data_dir().expect("Failed to get app data dir");
            std::fs::create_dir_all(&app_data_dir).expect("Failed to create app data dir");
            
            let db_path: PathBuf = app_data_dir.join("uploads.db");
            db::init_db(&db_path).expect("Failed to initialize database");
            
            // Migrate from old store format if needed
            migrate_from_store(&app_data_dir);
            
            // Clean up old sessions on startup
            if let Err(e) = db::cleanup_old_sessions() {
                eprintln!("Failed to cleanup old sessions: {}", e);
            }
            
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Upload commands
            upload::upload_file,
            upload::cancel_upload,
            upload::get_file_info,
            upload::get_folder_files,
            upload::get_pending_uploads,
            upload::get_upload_session,
            upload::get_session_progress,
            upload::delete_upload_session,
            upload::cleanup_old_sessions,
            upload::check_resumable_upload,
            // Account commands
            account::list_accounts,
            account::create_account,
            account::update_account,
            account::delete_account,
            // Token commands
            account::list_tokens,
            account::create_token,
            account::update_token,
            account::delete_token,
            account::get_token,
            // Bucket commands
            account::list_buckets,
            account::save_buckets,
            account::update_bucket,
            account::delete_bucket,
            // State commands
            account::get_current_config,
            account::set_current_token,
            account::set_current_bucket,
            account::has_accounts,
            account::get_all_accounts_with_tokens,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
