use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tauri::Manager;
use tempfile::Builder;

mod db;
mod upload;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
async fn get_video_thumbnail(url: String) -> Result<String, String> {
    // Create temp file for thumbnail output
    let thumbnail_file = Builder::new()
        .suffix(".jpg")
        .tempfile()
        .map_err(|e| format!("Failed to create temp file: {}", e))?;
    let thumbnail_path = thumbnail_file.path().to_path_buf();

    // Use ffmpeg to extract a frame at 1 second
    // ffmpeg handles HTTP URLs directly with efficient range requests
    let output = Command::new("ffmpeg")
        .args([
            "-y",                    // Overwrite output
            "-ss", "1",              // Seek to 1 second
            "-i", &url,              // Input URL
            "-vframes", "1",         // Extract 1 frame
            "-vf", "scale=320:-1",   // Scale to 320px width, maintain aspect
            "-q:v", "2",             // High quality JPEG
            thumbnail_path.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("ffmpeg not found. Please install ffmpeg: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg failed: {}", stderr));
    }

    // Read thumbnail and encode as base64
    let thumbnail_bytes = fs::read(&thumbnail_path)
        .map_err(|e| format!("Failed to read thumbnail: {}", e))?;

    let base64_str = BASE64.encode(&thumbnail_bytes);

    Ok(format!("data:image/jpeg;base64,{}", base64_str))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
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
            greet,
            get_video_thumbnail,
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
