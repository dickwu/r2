use std::path::PathBuf;
use tauri::Manager;

mod db;
mod upload;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
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
            
            // Clean up old sessions on startup
            if let Err(e) = db::cleanup_old_sessions() {
                eprintln!("Failed to cleanup old sessions: {}", e);
            }
            
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            upload::upload_file,
            upload::cancel_upload,
            upload::get_file_info,
            upload::get_pending_uploads,
            upload::get_upload_session,
            upload::get_session_progress,
            upload::delete_upload_session,
            upload::cleanup_old_sessions,
            upload::check_resumable_upload
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
