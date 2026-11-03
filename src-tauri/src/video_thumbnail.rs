//! Video thumbnails for the file grid and the preview poster.
//!
//! ffmpeg reads the object straight from its public or presigned URL and seeks
//! with HTTP range requests, so only the bytes around the requested frame (plus
//! the `moov` index, wherever it sits) are fetched instead of the whole video.
//! The frame at 1 s is used; a video shorter than that falls back to its first
//! frame.
//!
//! The app bundles a static ffmpeg as the `r2-ffmpeg` sidecar (fetched by
//! `scripts/fetch-ffmpeg.ts`, see `bundle.externalBin`), installed next to the
//! app executable, so thumbnails need nothing on the machine. A system ffmpeg
//! is the fallback: an app launched from Finder, the Dock, a desktop entry or
//! the Start menu does not inherit the shell's PATH, so the usual install
//! locations (Homebrew, MacPorts, Linuxbrew, Snap, winget, Scoop, Chocolatey)
//! are searched as well as PATH. When no ffmpeg is found the command fails
//! with [`FFMPEG_NOT_FOUND`]; the frontend then tries a `<video>` element and
//! finally says that ffmpeg is missing.

use base64::{engine::general_purpose::STANDARD, Engine};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// Error returned when no ffmpeg binary can be found.
pub const FFMPEG_NOT_FOUND: &str = "ffmpeg_not_found";
/// Seek position of the thumbnail frame, in seconds.
const THUMBNAIL_SEEK_SECONDS: &str = "1";
const DEFAULT_MAX_WIDTH: u32 = 480;
const MIN_WIDTH: u32 = 64;
const MAX_WIDTH: u32 = 1920;
/// Hard limit for one ffmpeg run; the process is killed when it expires.
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(45);
/// Network read timeout handed to ffmpeg, in microseconds.
const FFMPEG_RW_TIMEOUT_US: &str = "20000000";
const TIMED_OUT: &str = "ffmpeg timed out";
/// A missing ffmpeg is looked up again after this long, so installing it
/// while the app runs takes effect without a restart.
const NOT_FOUND_RECHECK: Duration = Duration::from_secs(30);

/// Grid scrolling can mount dozens of video tiles at once; keep the number of
/// concurrent ffmpeg processes small.
static FFMPEG_SLOTS: Semaphore = Semaphore::const_new(3);
static FFMPEG_LOOKUP: Mutex<Option<(Option<PathBuf>, Instant)>> = Mutex::new(None);

/// File name of the bundled sidecar next to the app executable. Not plain
/// `ffmpeg`: the Linux packages install it into `/usr/bin`, where a distro
/// ffmpeg would collide with it.
fn sidecar_file_name() -> &'static str {
    if cfg!(windows) {
        "r2-ffmpeg.exe"
    } else {
        "r2-ffmpeg"
    }
}

fn ffmpeg_file_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// Every path ffmpeg may live at, in lookup order: the `R2_FFMPEG` override,
/// the bundled sidecar in `exe_dir`, the PATH, then each platform's usual
/// install directories.
fn candidate_paths(env: impl Fn(&str) -> Option<OsString>, exe_dir: Option<&Path>) -> Vec<PathBuf> {
    let name = ffmpeg_file_name();
    let mut out: Vec<PathBuf> = Vec::new();

    if let Some(explicit) = env("R2_FFMPEG").filter(|v| !v.is_empty()) {
        // `R2_FFMPEG=off` turns native thumbnails off (the webview player
        // fallback is used), which is how a machine without ffmpeg behaves.
        if explicit == "off" {
            return out;
        }
        out.push(PathBuf::from(explicit));
    }
    if let Some(dir) = exe_dir {
        out.push(dir.join(sidecar_file_name()));
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = env("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }

    let home = env("HOME")
        .or_else(|| env("USERPROFILE"))
        .map(PathBuf::from);
    if cfg!(target_os = "macos") {
        dirs.extend(
            ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"]
                .iter()
                .map(PathBuf::from),
        );
    } else if cfg!(windows) {
        if let Some(local) = env("LOCALAPPDATA") {
            dirs.push(
                PathBuf::from(local)
                    .join("Microsoft")
                    .join("WinGet")
                    .join("Links"),
            );
        }
        if let Some(home) = &home {
            dirs.push(home.join("scoop").join("shims"));
        }
        if let Some(data) = env("ProgramData") {
            dirs.push(PathBuf::from(data).join("chocolatey").join("bin"));
        }
        if let Some(pf) = env("ProgramFiles") {
            dirs.push(PathBuf::from(pf).join("ffmpeg").join("bin"));
        }
        dirs.push(PathBuf::from(r"C:\ffmpeg\bin"));
    } else {
        dirs.extend(
            [
                "/usr/bin",
                "/usr/local/bin",
                "/snap/bin",
                "/home/linuxbrew/.linuxbrew/bin",
            ]
            .iter()
            .map(PathBuf::from),
        );
    }
    if let Some(home) = &home {
        if !cfg!(windows) {
            dirs.push(home.join(".local").join("bin"));
        }
    }

    for dir in dirs {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// The ffmpeg binary to use. A hit is kept for the session; a miss is retried
/// after [`NOT_FOUND_RECHECK`].
pub(crate) fn locate_ffmpeg() -> Option<PathBuf> {
    let mut guard = FFMPEG_LOOKUP.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((found, at)) = guard.as_ref() {
        match found {
            Some(path) if path.is_file() => return Some(path.clone()),
            None if at.elapsed() < NOT_FOUND_RECHECK => return None,
            _ => {}
        }
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let found = candidate_paths(|key| std::env::var_os(key), exe_dir.as_deref())
        .into_iter()
        .find(|p| p.is_file());
    match &found {
        Some(path) => log::info!("video thumbnails: using ffmpeg at {}", path.display()),
        None => log::warn!("video thumbnails: no ffmpeg found (bundled sidecar or system)"),
    }
    *guard = Some((found.clone(), Instant::now()));
    found
}

fn is_remote(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// System CA bundles, most common first. The bundled static ffmpeg builds use
/// OpenSSL on macOS and Linux with a CA path baked in at build time that does
/// not exist on users' machines, so without `-ca_file` every https source
/// fails with "certificate verify failed". The Windows build uses GnuTLS,
/// which falls back to the Windows certificate store, so it needs no file.
#[cfg(not(windows))]
const SYSTEM_CA_FILES: &[&str] = &[
    "/etc/ssl/cert.pem",                  // macOS, Alpine, Arch
    "/etc/ssl/certs/ca-certificates.crt", // Debian, Ubuntu
    "/etc/pki/tls/certs/ca-bundle.crt",   // Fedora, RHEL
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/ca-bundle.pem", // openSUSE
];
#[cfg(windows)]
const SYSTEM_CA_FILES: &[&str] = &[];

/// The CA bundle to hand ffmpeg: `SSL_CERT_FILE` when it names a file (the
/// usual way to add corporate roots), else the first system bundle found.
fn pick_ca_file(env_file: Option<OsString>, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    env_file
        .map(PathBuf::from)
        .into_iter()
        .chain(SYSTEM_CA_FILES.iter().map(PathBuf::from))
        .find(|p| exists(p))
}

fn ca_file() -> Option<&'static Path> {
    static CA_FILE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    CA_FILE
        .get_or_init(|| pick_ca_file(std::env::var_os("SSL_CERT_FILE"), Path::is_file))
        .as_deref()
}

/// ffmpeg arguments that write one JPEG frame to stdout (near-lossless: the
/// cache re-encodes it to WebP). `seek` is an input
/// option (before `-i`), so for a remote file ffmpeg jumps there with a range
/// request instead of reading everything before it.
fn ffmpeg_args(
    source: &str,
    seek: Option<&str>,
    max_width: u32,
    ca_file: Option<&Path>,
) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if is_remote(source) {
        args.extend(["-rw_timeout".to_string(), FFMPEG_RW_TIMEOUT_US.to_string()]);
        if let Some(ca_file) = ca_file {
            args.extend([
                "-ca_file".to_string(),
                ca_file.to_string_lossy().into_owned(),
            ]);
        }
    }
    if let Some(seek) = seek {
        args.extend(["-ss".to_string(), seek.to_string()]);
    }
    args.extend(
        [
            "-i",
            source,
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-frames:v",
            "1",
            "-vf",
            &format!("scale='min({max_width},iw)':-2"),
            "-c:v",
            "mjpeg",
            "-q:v",
            "2",
            "-f",
            "image2pipe",
            "pipe:1",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args
}

enum FrameOutcome {
    Frame(Vec<u8>),
    /// ffmpeg exited cleanly but produced nothing: the seek went past the end.
    Empty,
}

/// Removes the source (a presigned URL carries its signature in the query)
/// from ffmpeg's stderr before it ends up in an error message.
fn redact_stderr(stderr: &[u8], source: &str) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.replace(source, "<source>");
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    // Anything else that still looks like a signed query string goes too.
    match line.find("X-Amz-") {
        Some(idx) => format!("{}<redacted>", &line[..idx]),
        None => line,
    }
}

async fn run_ffmpeg(
    ffmpeg: &PathBuf,
    source: &str,
    seek: Option<&str>,
    max_width: u32,
) -> Result<FrameOutcome, String> {
    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.args(ffmpeg_args(source, seek, max_width, ca_file()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: no console window flashes up per thumbnail.
        cmd.creation_flags(0x0800_0000);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start ffmpeg: {e}"))?;
    let output = tokio::time::timeout(FFMPEG_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| TIMED_OUT.to_string())?
        .map_err(|e| format!("ffmpeg failed: {e}"))?;

    if !output.stdout.is_empty() && output.status.success() {
        return Ok(FrameOutcome::Frame(output.stdout));
    }
    if output.status.success() {
        return Ok(FrameOutcome::Empty);
    }
    Err(format!(
        "ffmpeg failed: {}",
        redact_stderr(&output.stderr, source)
    ))
}

/// Clamp a requested thumbnail width to what the grid can use.
pub(crate) fn clamp_width(max_width: Option<u32>) -> u32 {
    max_width
        .unwrap_or(DEFAULT_MAX_WIDTH)
        .clamp(MIN_WIDTH, MAX_WIDTH)
}

async fn ffmpeg_path() -> Result<PathBuf, String> {
    tokio::task::spawn_blocking(locate_ffmpeg)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| FFMPEG_NOT_FOUND.to_string())
}

/// The frame at 1 s of a video (first frame when it is shorter) as JPEG
/// bytes, at most `width` pixels wide. `source` is an http(s) URL or a path.
pub(crate) async fn render_video_frame(source: &str, width: u32) -> Result<Vec<u8>, String> {
    if source.trim().is_empty() {
        return Err("No video source".to_string());
    }
    let ffmpeg = ffmpeg_path().await?;
    let _slot = FFMPEG_SLOTS
        .acquire()
        .await
        .map_err(|e| format!("Thumbnail queue closed: {e}"))?;

    let first = run_ffmpeg(&ffmpeg, source, Some(THUMBNAIL_SEEK_SECONDS), width).await;
    match first {
        Ok(FrameOutcome::Frame(bytes)) => Ok(bytes),
        // A network stall would only stall again; don't double the wait.
        Err(e) if e == TIMED_OUT => Err(e),
        // Shorter than 1 s, or a seek ffmpeg could not satisfy: first frame.
        Ok(FrameOutcome::Empty) | Err(_) => match run_ffmpeg(&ffmpeg, source, None, width).await? {
            FrameOutcome::Frame(bytes) => Ok(bytes),
            FrameOutcome::Empty => Err("Video has no decodable frame".to_string()),
        },
    }
}

/// The first frame of a still image ffmpeg can decode but the Rust decoders
/// cannot (HEIC, AVIF, PSD, JPEG XL, ... as far as the bundled build supports
/// them), as JPEG bytes at most `width` pixels wide.
pub(crate) async fn render_still_frame(source: &Path, width: u32) -> Result<Vec<u8>, String> {
    let ffmpeg = ffmpeg_path().await?;
    let _slot = FFMPEG_SLOTS
        .acquire()
        .await
        .map_err(|e| format!("Thumbnail queue closed: {e}"))?;
    let source = source.to_string_lossy();
    match run_ffmpeg(&ffmpeg, &source, None, width).await? {
        FrameOutcome::Frame(bytes) => Ok(bytes),
        FrameOutcome::Empty => Err("Image has no decodable frame".to_string()),
    }
}

/// Renders the frame at 1 s of a video (first frame when it is shorter) as a
/// JPEG `data:` URL, without the disk cache. `source` is an http(s) URL or a
/// local path. The grid uses `get_thumbnail`, which caches.
#[tauri::command]
pub async fn generate_video_thumbnail(
    source: String,
    max_width: Option<u32>,
) -> Result<String, String> {
    let frame = render_video_frame(&source, clamp_width(max_width)).await?;
    Ok(format!("data:image/jpeg;base64,{}", STANDARD.encode(frame)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn override_comes_first_then_path() {
        let path = std::env::join_paths(["/custom/bin"]).unwrap();
        let paths = candidate_paths(
            env_from(&[
                ("R2_FFMPEG", "/opt/ff/ffmpeg"),
                ("PATH", path.to_str().unwrap()),
            ]),
            Some(Path::new("/app/bin")),
        );
        assert_eq!(paths[0], PathBuf::from("/opt/ff/ffmpeg"));
        assert_eq!(paths[1], Path::new("/app/bin").join(sidecar_file_name()));
        assert_eq!(
            paths[2],
            PathBuf::from("/custom/bin").join(ffmpeg_file_name())
        );
    }

    #[test]
    fn bundled_sidecar_wins_over_system_ffmpeg() {
        let path = std::env::join_paths(["/usr/bin", "/opt/homebrew/bin"]).unwrap();
        let paths = candidate_paths(
            env_from(&[("PATH", path.to_str().unwrap())]),
            Some(Path::new("/Applications/r2.app/Contents/MacOS")),
        );
        assert_eq!(
            paths[0],
            Path::new("/Applications/r2.app/Contents/MacOS").join(sidecar_file_name())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finder_path_still_finds_homebrew() {
        // The PATH a Finder-launched app gets.
        let paths = candidate_paths(env_from(&[("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")]), None);
        assert!(paths.contains(&PathBuf::from("/opt/homebrew/bin/ffmpeg")));
        assert!(paths.contains(&PathBuf::from("/usr/local/bin/ffmpeg")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_searches_package_manager_dirs() {
        let paths = candidate_paths(
            env_from(&[
                ("LOCALAPPDATA", r"C:\Users\u\AppData\Local"),
                ("USERPROFILE", r"C:\Users\u"),
            ]),
            None,
        );
        assert!(paths.contains(&PathBuf::from(
            r"C:\Users\u\AppData\Local\Microsoft\WinGet\Links\ffmpeg.exe"
        )));
        assert!(paths.contains(&PathBuf::from(r"C:\Users\u\scoop\shims\ffmpeg.exe")));
    }

    #[test]
    fn override_off_disables_lookup() {
        let paths = candidate_paths(
            env_from(&[("R2_FFMPEG", "off"), ("PATH", "/usr/bin")]),
            Some(Path::new("/app/bin")),
        );
        assert!(paths.is_empty());
    }

    #[test]
    fn candidates_are_deduplicated() {
        let path = std::env::join_paths(["/a", "/a"]).unwrap();
        let paths = candidate_paths(env_from(&[("PATH", path.to_str().unwrap())]), None);
        let target = PathBuf::from("/a").join(ffmpeg_file_name());
        assert_eq!(paths.iter().filter(|p| **p == target).count(), 1);
    }

    #[test]
    fn seek_is_an_input_option() {
        let args = ffmpeg_args("https://x/v.mp4?X-Amz-Signature=s", Some("1"), 480, None);
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(ss < input, "-ss must come before -i for fast range seeking");
        assert_eq!(args[ss + 1], "1");
        assert!(args.contains(&"-rw_timeout".to_string()));
        assert!(args.contains(&"scale='min(480,iw)':-2".to_string()));
    }

    #[test]
    fn first_frame_fallback_has_no_seek_and_local_has_no_rw_timeout() {
        let args = ffmpeg_args("/tmp/v.mp4", None, 480, Some(Path::new("/ca.pem")));
        assert!(!args.contains(&"-ss".to_string()));
        assert!(!args.contains(&"-rw_timeout".to_string()));
        assert!(!args.contains(&"-ca_file".to_string()));
    }

    #[test]
    fn remote_sources_get_the_ca_file_before_the_input() {
        let args = ffmpeg_args(
            "https://x/v.mp4",
            Some("1"),
            480,
            Some(Path::new("/ca.pem")),
        );
        let ca = args.iter().position(|a| a == "-ca_file").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(ca < input);
        assert_eq!(args[ca + 1], "/ca.pem");
        let args = ffmpeg_args("https://x/v.mp4", Some("1"), 480, None);
        assert!(!args.contains(&"-ca_file".to_string()));
    }

    #[test]
    fn ca_file_prefers_ssl_cert_file_then_system_bundles() {
        let custom = OsString::from("/corp/roots.pem");
        let all = |_: &Path| true;
        assert_eq!(
            pick_ca_file(Some(custom.clone()), all),
            Some(PathBuf::from("/corp/roots.pem"))
        );
        // A stale SSL_CERT_FILE falls through to the system bundles.
        let system_only = |p: &Path| p != Path::new("/corp/roots.pem");
        assert_eq!(
            pick_ca_file(Some(custom), system_only),
            SYSTEM_CA_FILES.first().map(PathBuf::from)
        );
        assert_eq!(pick_ca_file(None, |_: &Path| false), None);
    }

    #[test]
    fn stderr_never_carries_the_signed_url() {
        let src = "https://acct.r2.cloudflarestorage.com/b/v.mp4?X-Amz-Signature=abc";
        let err = format!("{src}: Server returned 403 Forbidden\n");
        let out = redact_stderr(err.as_bytes(), src);
        assert_eq!(out, "<source>: Server returned 403 Forbidden");
        let other = "https://other/v.mp4?X-Amz-Credential=k: 403";
        assert!(!redact_stderr(other.as_bytes(), src).contains("Credential"));
    }

    /// Needs ffmpeg on the machine; skipped otherwise.
    #[tokio::test]
    async fn renders_frame_at_one_second_and_short_video_fallback() {
        let Some(ffmpeg) = locate_ffmpeg() else {
            eprintln!("ffmpeg not installed; skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("r2-vthumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, secs) in [("long.mp4", "3"), ("short.mp4", "0.4")] {
            let path = dir.join(name);
            let status = std::process::Command::new(&ffmpeg)
                .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
                .arg(format!("testsrc=duration={secs}:size=320x240:rate=25"))
                .args(["-pix_fmt", "yuv420p"])
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success());
            let url = generate_video_thumbnail(path.to_string_lossy().into_owned(), Some(160))
                .await
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(url.starts_with("data:image/jpeg;base64,"), "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
