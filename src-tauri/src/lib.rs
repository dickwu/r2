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
            
            let db_path = app_data_dir.join("uploads-turso.db");
            
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async {
                db::init_db(&db_path).await.expect("Failed to initialize database");
                
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
            cache::search_cached_files,
            cache::calculate_folder_size,
            cache::build_directory_tree,
            cache::get_directory_node,
            cache::get_all_directory_nodes,
            cache::clear_file_cache,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
