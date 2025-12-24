use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{
    Manager,
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

mod account;
mod cache;
mod db;
mod r2;
mod upload;

const APP_NAME: &str = "R2 Uploader";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const APP_DESCRIPTION: &str = "A desktop application for managing Cloudflare R2 storage buckets.";
const APP_AUTHOR: &str = "Peilin Wu";
const APP_LICENSE: &str = "MIT";
const APP_COPYRIGHT: &str = "© 2025 Peilin Wu";
const APP_WEBSITE: &str = "https://github.com/dickwu/r2";

fn show_about_dialog<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let about_message = format!(
        "{}\n\nVersion: {}\nAuthor: {}\nLicense: {}\n{}",
        APP_DESCRIPTION, APP_VERSION, APP_AUTHOR, APP_LICENSE, APP_COPYRIGHT
    );
    
    app.dialog()
        .message(&about_message)
        .title(APP_NAME)
        .buttons(MessageDialogButtons::OkCancelCustom("OK".to_string(), "View on GitHub".to_string()))
        .show(move |result| {
            // If Cancel (View on GitHub) was clicked, open the website
            if !result {
                let _ = tauri_plugin_opener::open_url(APP_WEBSITE, None::<&str>);
            }
        });
}

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

/// Migrate data from old rusqlite database to new Turso database
/// Note: Assumes new Turso database is already initialized
async fn migrate_sqlite_to_turso(old_db_path: &PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use rusqlite::Connection;
    
    log::info!("Opening old rusqlite database...");
    let old_conn = Connection::open(old_db_path).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    
    // Migrate accounts
    log::info!("Migrating accounts...");
    let mut stmt = old_conn.prepare("SELECT id, name, created_at, updated_at FROM accounts")
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    let accounts: Vec<(String, Option<String>, i64, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    
    for (id, name, _created_at, _updated_at) in &accounts {
        db::create_account(id, name.as_deref()).await?;
    }
    log::info!("Migrated {} accounts", accounts.len());
    
    // Migrate tokens
    log::info!("Migrating tokens...");
    let mut stmt = old_conn.prepare(
        "SELECT id, account_id, name, api_token, access_key_id, secret_access_key, created_at, updated_at FROM tokens"
    ).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    let tokens: Vec<(i64, String, Option<String>, String, String, String, i64, i64)> = stmt
        .query_map([], |row| Ok((
            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
            row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?
        )))
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    
    let mut token_id_map = std::collections::HashMap::new();
    for (old_id, account_id, name, api_token, access_key_id, secret_access_key, _created_at, _updated_at) in tokens {
        let new_token = db::create_token(&account_id, name.as_deref(), &api_token, &access_key_id, &secret_access_key).await?;
        token_id_map.insert(old_id, new_token.id);
    }
    log::info!("Migrated {} tokens", token_id_map.len());
    
    // Migrate buckets
    log::info!("Migrating buckets...");
    let mut stmt = old_conn.prepare(
        "SELECT id, token_id, name, public_domain, created_at, updated_at FROM buckets"
    ).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    let buckets: Vec<(i64, i64, String, Option<String>, i64, i64)> = stmt
        .query_map([], |row| Ok((
            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?
        )))
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    
    for (_old_id, old_token_id, name, public_domain, _created_at, _updated_at) in &buckets {
        if let Some(&new_token_id) = token_id_map.get(old_token_id) {
            db::create_bucket(new_token_id, name, public_domain.as_deref()).await?;
        }
    }
    log::info!("Migrated {} buckets", buckets.len());
    
    // Migrate app_state
    log::info!("Migrating app state...");
    let mut stmt = old_conn.prepare("SELECT key, value FROM app_state")
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    let states: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    
    for (key, value) in &states {
        // Update token ID if this is current_token_id
        if key == "current_token_id" {
            if let Ok(old_token_id) = value.parse::<i64>() {
                if let Some(&new_token_id) = token_id_map.get(&old_token_id) {
                    db::set_app_state(key, &new_token_id.to_string()).await?;
                }
            }
        } else {
            db::set_app_state(key, value).await?;
        }
    }
    log::info!("Migrated {} app state entries", states.len());
    
    // Note: We don't migrate upload_sessions and completed_parts as they are temporary/resumable data
    // Users can restart those uploads if needed
    
    // Backup old database
    let backup_path = old_db_path.with_extension("db.backup");
    std::fs::rename(old_db_path, &backup_path)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    log::info!("Backed up old database to {:?}", backup_path);
    
    Ok(())
}

/// Migrate from old store format to SQLite
async fn migrate_from_store(app_data_dir: &PathBuf) {
    // Check if migration is needed
    match db::has_accounts().await {
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
    if let Err(e) = db::create_account(&old_config.account_id, None).await {
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
    ).await {
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

    if let Err(e) = db::save_buckets_for_token(token.id, &buckets).await {
        log::error!("Failed to save buckets: {}", e);
        return;
    }
    log::info!("Created {} bucket(s)", buckets.len());

    // Set current selection
    if let Err(e) = db::set_current_selection(token.id, &old_config.bucket).await {
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
            
            let old_db_path = app_data_dir.join("uploads.db");
            let new_db_path = app_data_dir.join("uploads-turso.db");
            
            // Handle migration from rusqlite to Turso
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async {
                // Initialize Turso database first
                db::init_db(&new_db_path).await.expect("Failed to initialize database");
                
                // Check if we need to migrate
                if old_db_path.exists() && !old_db_path.to_string_lossy().ends_with(".backup") {
                    log::info!("Migrating from rusqlite to Turso database...");
                    if let Err(e) = migrate_sqlite_to_turso(&old_db_path).await {
                        log::error!("Migration failed: {}", e);
                        panic!("Failed to migrate database: {}", e);
                    }
                    log::info!("Migration completed successfully");
                }
                
                // Migrate from old store format if needed (for very old installations)
                migrate_from_store(&app_data_dir).await;
                
                // Clean up old sessions on startup
                if let Err(e) = db::cleanup_old_sessions().await {
                    eprintln!("Failed to cleanup old sessions: {}", e);
                }
            });


            // Setup custom application menu (macOS menu bar)
            #[cfg(target_os = "macos")]
            {
                let app_menu_about = MenuItem::with_id(app, "app_about", "About R2 Uploader", true, None::<&str>)?;
                let separator = PredefinedMenuItem::separator(app)?;
                let hide = PredefinedMenuItem::hide(app, Some("Hide"))?;
                let hide_others = PredefinedMenuItem::hide_others(app, Some("Hide Others"))?;
                let show_all = PredefinedMenuItem::show_all(app, Some("Show All"))?;
                let separator2 = PredefinedMenuItem::separator(app)?;
                let quit = PredefinedMenuItem::quit(app, Some("Quit"))?;
                
                let app_submenu = Submenu::with_items(
                    app,
                    "R2 Uploader",
                    true,
                    &[&app_menu_about, &separator, &hide, &hide_others, &show_all, &separator2, &quit],
                )?;
                
                let edit_menu = Submenu::with_items(
                    app,
                    "Edit",
                    true,
                    &[
                        &PredefinedMenuItem::undo(app, Some("Undo"))?,
                        &PredefinedMenuItem::redo(app, Some("Redo"))?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::cut(app, Some("Cut"))?,
                        &PredefinedMenuItem::copy(app, Some("Copy"))?,
                        &PredefinedMenuItem::paste(app, Some("Paste"))?,
                        &PredefinedMenuItem::select_all(app, Some("Select All"))?,
                    ],
                )?;
                
                let window_menu = Submenu::with_items(
                    app,
                    "Window",
                    true,
                    &[
                        &PredefinedMenuItem::minimize(app, Some("Minimize"))?,
                        &PredefinedMenuItem::maximize(app, Some("Zoom"))?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::close_window(app, Some("Close"))?,
                    ],
                )?;
                
                let app_menu = Menu::with_items(app, &[&app_submenu, &edit_menu, &window_menu])?;
                app.set_menu(app_menu)?;
                
                app.on_menu_event(|app, event| {
                    if event.id.as_ref() == "app_about" {
                        show_about_dialog(app);
                    }
                });
            }

            // Setup system tray
            let about_item = MenuItem::with_id(app, "about", "About", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&about_item, &quit_item])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    match event.id.as_ref() {
                        "about" => {
                            show_about_dialog(app);
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;
            
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
            // R2 SDK upload command (clean AWS SDK implementation)
            r2::upload_file_sdk,
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
            // R2 commands
            cache::list_r2_buckets,
            cache::list_r2_objects,
            cache::list_all_r2_objects,
            cache::delete_r2_object,
            cache::rename_r2_object,
            cache::generate_signed_url,
            // Cache commands
            cache::store_all_files,
            cache::get_all_cached_files,
            cache::calculate_folder_size,
            cache::build_directory_tree,
            cache::get_directory_node,
            cache::get_all_directory_nodes,
            cache::clear_file_cache,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
