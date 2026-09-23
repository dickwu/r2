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
        let contents = r#"{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "connector",
  "description": "Capability for the tauri-connector plugin",
  "windows": ["main"],
  "permissions": ["connector:default"]
}
"#;
        // Rewriting an identical file bumps its mtime, which sends `tauri dev`'s
        // file watcher into a rebuild loop — only write when stale.
        let up_to_date = std::fs::read_to_string(connector_cap)
            .map(|existing| existing == contents)
            .unwrap_or(false);
        if !up_to_date {
            std::fs::write(connector_cap, contents).expect("failed to write connector capability");
        }
    } else if connector_cap.exists() {
        std::fs::remove_file(connector_cap).ok();
    }

    let mut attributes = tauri_build::Attributes::new();
    // Every binary this package links needs Common Controls v6: the window
    // and dialog stack imports v6-only exports such as TaskDialogIndirect.
    // tauri-build embeds its manifest into the app binary only, so a test
    // binary bound the legacy comctl32 5.82 and failed to load with
    // 0xc0000139. Embed the same manifest through the linker for every
    // target — app and test harnesses alike — and not a second time here.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }
    // `tauri_build::build()` with the attributes above.
    if let Err(error) = tauri_build::try_build(attributes) {
        println!("{error:#}");
        std::process::exit(1);
    }
}
