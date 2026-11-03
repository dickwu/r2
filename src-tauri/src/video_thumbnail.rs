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
fn locate_ffmpeg() -> Option<PathBuf> {
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

/// ffmpeg arguments that write one JPEG frame to stdout. `seek` is an input
/// option (before `-i`), so for a remote file ffmpeg jumps there with a range
/// request instead of reading everything before it.
fn ffmpeg_args(source: &str, seek: Option<&str>, max_width: u32) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if is_remote(source) {
        args.extend(["-rw_timeout".to_string(), FFMPEG_RW_TIMEOUT_US.to_string()]);
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
            "4",
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
    cmd.args(ffmpeg_args(source, seek, max_width))
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

/// Renders the frame at 1 s of a video (first frame when it is shorter) as a
/// JPEG `data:` URL. `source` is an http(s) URL or a local path.
#[tauri::command]
pub async fn generate_video_thumbnail(
    source: String,
    max_width: Option<u32>,
) -> Result<String, String> {
    if source.trim().is_empty() {
        return Err("No video source".to_string());
    }
    let ffmpeg = tokio::task::spawn_blocking(locate_ffmpeg)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| FFMPEG_NOT_FOUND.to_string())?;
    let width = max_width
        .unwrap_or(DEFAULT_MAX_WIDTH)
        .clamp(MIN_WIDTH, MAX_WIDTH);

    let _slot = FFMPEG_SLOTS
        .acquire()
        .await
        .map_err(|e| format!("Thumbnail queue closed: {e}"))?;

    let first = run_ffmpeg(&ffmpeg, &source, Some(THUMBNAIL_SEEK_SECONDS), width).await;
    let frame = match first {
        Ok(FrameOutcome::Frame(bytes)) => bytes,
        // A network stall would only stall again; don't double the wait.
        Err(e) if e == TIMED_OUT => return Err(e),
        // Shorter than 1 s, or a seek ffmpeg could not satisfy: first frame.
        Ok(FrameOutcome::Empty) | Err(_) => {
            match run_ffmpeg(&ffmpeg, &source, None, width).await? {
                FrameOutcome::Frame(bytes) => bytes,
                FrameOutcome::Empty => return Err("Video has no decodable frame".to_string()),
            }
        }
    };
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
        let args = ffmpeg_args("https://x/v.mp4?X-Amz-Signature=s", Some("1"), 480);
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(ss < input, "-ss must come before -i for fast range seeking");
        assert_eq!(args[ss + 1], "1");
        assert!(args.contains(&"-rw_timeout".to_string()));
        assert!(args.contains(&"scale='min(480,iw)':-2".to_string()));
    }

    #[test]
    fn first_frame_fallback_has_no_seek_and_local_has_no_rw_timeout() {
        let args = ffmpeg_args("/tmp/v.mp4", None, 480);
        assert!(!args.contains(&"-ss".to_string()));
        assert!(!args.contains(&"-rw_timeout".to_string()));
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
