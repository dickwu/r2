use std::path::Path;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

mod account;
mod commands;
mod db;
mod download;
mod providers;
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
        .buttons(MessageDialogButtons::OkCancelCustom(
            "OK".to_string(),
            "View on GitHub".to_string(),
        ))
        .show(move |result| {
            // If Cancel (View on GitHub) was clicked, open the website
            if !result {
                let _ = tauri_plugin_opener::open_url(APP_WEBSITE, None::<&str>);
            }
        });
}

async fn confirm_db_reset<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    db_path: &Path,
    error: &str,
) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let message = format!(
        "Failed to load local database:\n{}\n\nLocation: {}\n\nRemove the database file and recreate it? This will clear local cache and sessions.",
        error,
        db_path.display()
    );

    app.dialog()
        .message(&message)
        .title(APP_NAME)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Remove DB File".to_string(),
            "Quit".to_string(),
        ))
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });

    rx.await.unwrap_or(false)
}

async fn init_db_with_recovery<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    db_path: &Path,
) -> Result<(), String> {
    if let Err(err) = db::init_db(db_path).await {
        let should_reset = confirm_db_reset(app, db_path, &err.to_string()).await;
        if !should_reset {
            return Err(format!("Database initialization failed: {}", err));
        }

        if let Err(remove_err) = std::fs::remove_file(db_path) {
            if remove_err.kind() != std::io::ErrorKind::NotFound {
                return Err(format!(
                    "Failed to remove database file {}: {}",
                    db_path.display(),
                    remove_err
                ));
            }
        }

        db::init_db(db_path)
            .await
            .map_err(|e| format!("Failed to reinitialize database: {}", e))?;
    }

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_video_thumbnail::init())
        .setup(|app| {
            // Initialize database in app data directory
            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to get app data dir");
            std::fs::create_dir_all(&app_data_dir).expect("Failed to create app data dir");

            let db_path = app_data_dir.join("uploads-turso.db");

            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            let app_handle = app.handle();
            let init_result =
                rt.block_on(async { init_db_with_recovery(&app_handle, &db_path).await });

            if let Err(err) = init_result {
                let exit_handle = app_handle.clone();
                app_handle
                    .dialog()
                    .message(&err)
                    .title(APP_NAME)
                    .buttons(MessageDialogButtons::OkCancelCustom(
                        "OK".to_string(),
                        "Quit".to_string(),
                    ))
                    .show(move |_| {
                        exit_handle.exit(1);
                    });
                return Ok(());
            }

            rt.block_on(async {
                // Clean up old sessions on startup
                if let Err(e) = db::cleanup_old_sessions().await {
                    eprintln!("Failed to cleanup old sessions: {}", e);
                }
            });

            // Setup custom application menu (macOS menu bar)
            #[cfg(target_os = "macos")]
            {
                let app_menu_about =
                    MenuItem::with_id(app, "app_about", "About R2 Uploader", true, None::<&str>)?;
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
                    &[
                        &app_menu_about,
                        &separator,
                        &hide,
                        &hide_others,
                        &show_all,
                        &separator2,
                        &quit,
                    ],
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
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "about" => {
                        show_about_dialog(app);
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
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
            r2::commands::upload_file_sdk,
            // Account commands
            account::list_accounts,
            account::create_account,
            account::update_account,
            account::delete_account,
            account::list_aws_accounts,
            account::create_aws_account,
            account::update_aws_account,
            account::delete_aws_account,
            account::list_minio_accounts,
            account::create_minio_account,
            account::update_minio_account,
            account::delete_minio_account,
            account::list_rustfs_accounts,
            account::create_rustfs_account,
            account::update_rustfs_account,
            account::delete_rustfs_account,
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
            account::list_aws_bucket_configs,
            account::save_aws_bucket_configs,
            account::list_minio_bucket_configs,
            account::save_minio_bucket_configs,
            account::list_rustfs_bucket_configs,
            account::save_rustfs_bucket_configs,
            // State commands
            account::get_current_config,
            account::set_current_token,
            account::set_current_aws_bucket,
            account::set_current_minio_bucket,
            account::set_current_rustfs_bucket,
            account::set_current_bucket,
            account::has_accounts,
            account::get_all_accounts_with_tokens,
            account::get_all_aws_accounts_with_buckets,
            account::get_all_minio_accounts_with_buckets,
            account::get_all_rustfs_accounts_with_buckets,
            // R2 commands
            commands::list_r2_buckets,
            commands::list_r2_objects,
            commands::list_all_r2_objects,
            commands::list_folder_r2_objects,
            commands::delete_r2_object,
            commands::batch_delete_r2_objects,
            commands::rename_r2_object,
            commands::batch_move_r2_objects,
            commands::generate_signed_url,
            commands::upload_r2_content,
            commands::sync_bucket,
            // AWS commands
            commands::list_aws_buckets,
            commands::list_aws_objects,
            commands::list_all_aws_objects,
            commands::list_folder_aws_objects,
            commands::delete_aws_object,
            commands::batch_delete_aws_objects,
            commands::rename_aws_object,
            commands::batch_move_aws_objects,
            commands::generate_aws_signed_url,
            commands::upload_aws_content,
            commands::upload_aws_file,
            commands::sync_aws_bucket,
            // MinIO commands
            commands::list_minio_buckets,
            commands::list_minio_objects,
            commands::list_all_minio_objects,
            commands::list_folder_minio_objects,
            commands::delete_minio_object,
            commands::batch_delete_minio_objects,
            commands::rename_minio_object,
            commands::batch_move_minio_objects,
            commands::generate_minio_signed_url,
            commands::upload_minio_content,
            commands::upload_minio_file,
            commands::sync_minio_bucket,
            // RustFS commands
            commands::list_rustfs_buckets,
            commands::list_rustfs_objects,
            commands::list_all_rustfs_objects,
            commands::list_folder_rustfs_objects,
            commands::delete_rustfs_object,
            commands::batch_delete_rustfs_objects,
            commands::rename_rustfs_object,
            commands::batch_move_rustfs_objects,
            commands::generate_rustfs_signed_url,
            commands::upload_rustfs_content,
            commands::upload_rustfs_file,
            commands::sync_rustfs_bucket,
            // Cache commands
            commands::store_all_files,
            commands::get_all_cached_files,
            commands::search_cached_files,
            commands::calculate_folder_size,
            commands::build_directory_tree,
            commands::get_directory_node,
            commands::get_all_directory_nodes,
            commands::clear_file_cache,
            commands::get_folder_contents,
            commands::fetch_url_bytes,
            // Download commands
            download::commands::create_download_task,
            download::commands::start_download_queue,
            download::commands::start_all_downloads,
            download::commands::pause_all_downloads,
            download::commands::pause_download,
            download::commands::resume_download,
            download::commands::cancel_download,
            download::commands::delete_download_task,
            download::commands::get_download_tasks,
            download::commands::clear_finished_downloads,
            download::commands::clear_all_downloads,
            download::commands::select_download_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
