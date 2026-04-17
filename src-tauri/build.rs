fn main() {
    // Help the build system find FFmpeg libraries on Windows
    #[cfg(target_os = "windows")]
    {
        if let Ok(vcpkg_root) = std::env::var("VCPKG_ROOT") {
            println!(
                "cargo:rustc-link-search=native={}/installed/x64-windows/lib",
                vcpkg_root
            );
        }
        if let Ok(ffmpeg_dir) = std::env::var("FFMPEG_DIR") {
            println!("cargo:rustc-link-search=native={}/lib", ffmpeg_dir);
        }
    }

    // Expose connector permissions only in connector-enabled debug builds.
    let connector_cap = std::path::Path::new("capabilities/connector.json");
    if cfg!(feature = "connector") {
        std::fs::write(
            connector_cap,
            r#"{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "connector",
  "description": "Capability for the tauri-connector plugin",
  "windows": ["main"],
  "permissions": ["connector:default"]
}
"#,
        )
        .expect("failed to write connector capability");
    } else if connector_cap.exists() {
        std::fs::remove_file(connector_cap).ok();
    }

    tauri_build::build()
}
