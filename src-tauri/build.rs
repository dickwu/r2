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

    tauri_build::build()
}
