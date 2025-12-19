use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{
    Manager,
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

mod account;
mod db;
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
