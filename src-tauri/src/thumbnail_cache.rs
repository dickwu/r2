//! On-disk thumbnail cache for the file grid.
//!
//! Thumbnails (video frames and downscaled images) are stored as lossy WebP
//! files under `<app cache dir>/thumbnails/`, sharded by the first two hex
//! digits of their name. Each file is indexed by a `thumbnail_cache` row in
//! the app database (see `db::thumbnails`) recording the object identity it
//! was rendered from: account, bucket, key, plus etag or size and
//! last-modified. The presigned query of a URL is never part of the key, so a
//! thumbnail survives URL re-signing and app restarts.
//!
//! Lookup policy:
//! * metadata matches the row and the file exists: serve the file (no network,
//!   no ffmpeg, no decode);
//! * the object changed (other etag/size/time): render again, replace the row
//!   and delete the old file;
//! * a folder listing or bucket sync shows an object changed or deleted:
//!   `db::file_cache`/`db::prefix_sync` call [`apply_reconciled`], which drops
//!   the rows and files and emits [`INVALIDATED_EVENT`] so the grid can
//!   regenerate changed thumbnails right away;
//! * deleting or moving an object drops its thumbnail.
//!
//! Eviction runs off the UI thread at startup and at most once a minute after
//! writes: rows unused for [`MAX_AGE_SECS`] go first, then the least recently
//! used until the total is under the size limit (default 500 MB, adjustable in
//! Settings). Files are written to a temp file in the same directory and
//! renamed into place, so a crash never leaves a half-written thumbnail under
//! a name the index points at.
//!
//! Images are decoded in-process by the `image` crate (JPEG, PNG, GIF first
//! frame, WebP, BMP, ICO, TIFF); anything it cannot decode (HEIC/HEIF, AVIF,
//! PSD, JPEG XL, ...) is handed to the bundled ffmpeg. Images larger than
//! [`IMAGE_MAX_BYTES`] are not thumbnailed: the grid shows the original, as it
//! did before thumbnails existed.

use crate::db::thumbnails::{self, Lookup, Reconciled, ThumbnailObject, ThumbnailStats};
use crate::video_thumbnail;
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::Semaphore;

/// Images larger than this are shown as the original instead. A phone or
/// camera photo is typically 2-15 MB; beyond 32 MB the download and decode
/// take seconds and hundreds of MB of memory for a 480 px tile.
pub const IMAGE_MAX_BYTES: u64 = 32 * 1024 * 1024;
pub const IMAGE_TOO_LARGE: &str = "image_too_large";
pub const DEFAULT_MAX_BYTES: i64 = 500 * 1024 * 1024;
const MIN_LIMIT_BYTES: i64 = 50 * 1024 * 1024;
const MAX_LIMIT_BYTES: i64 = 20 * 1024 * 1024 * 1024;
/// Thumbnails not shown for this long are evicted.
pub const MAX_AGE_SECS: i64 = 30 * 24 * 60 * 60;
/// A hit only rewrites `last_used_at` once it is this old.
const TOUCH_INTERVAL_SECS: i64 = 10 * 60;
/// Minimum gap between two eviction passes triggered by writes.
const EVICT_INTERVAL_SECS: i64 = 60;
const LIMIT_KEY: &str = "thumbnail_cache_max_bytes";
const WEBP_QUALITY: f32 = 78.0;
const IMAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Decoded-pixel guard: a tiny file can claim a gigantic canvas.
const MAX_DIMENSION: u32 = 20_000;
const MAX_DECODE_ALLOC: u64 = 768 * 1024 * 1024;

pub const INVALIDATED_EVENT: &str = "thumbnail-cache-invalidated";
pub const CLEARED_EVENT: &str = "thumbnail-cache-cleared";

/// Decoding and resizing is CPU and memory heavy; a grid can ask for dozens.
static IMAGE_SLOTS: Semaphore = Semaphore::const_new(4);
static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();
static APP: OnceLock<AppHandle> = OnceLock::new();
static LAST_EVICTION: AtomicI64 = AtomicI64::new(0);
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn dev_log(message: impl FnOnce() -> String) {
    if cfg!(debug_assertions) {
        eprintln!("[thumbnail-cache] {}", message());
    }
}

/// Set the cache directory and app handle, then sweep and evict in the
/// background. Called once from setup.
pub fn init(app: &AppHandle, cache_root: PathBuf) {
    let dir = cache_root.join("thumbnails");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("thumbnail cache: cannot create {}: {e}", dir.display());
    }
    let _ = CACHE_DIR.set(dir);
    let _ = APP.set(app.clone());
    tauri::async_runtime::spawn(async {
        // Let startup work (database recovery, first listing) go first.
        tokio::time::sleep(Duration::from_secs(5)).await;
        if let Err(e) = sweep_orphans().await {
            log::warn!("thumbnail cache: sweep failed: {e}");
        }
        run_eviction().await;
    });
}

fn cache_dir() -> Option<&'static Path> {
    CACHE_DIR.get().map(PathBuf::as_path)
}

// ============ Keys and files ============

/// File path (relative to the cache dir) of the thumbnail of one version of
/// an object. Derived from the normalized identity only, never from a URL.
pub fn file_path_for(object: &ThumbnailObject, kind: &str) -> String {
    let object = object.normalized();
    let mut hash = Sha256::new();
    let size = object.size.map(|s| s.to_string()).unwrap_or_default();
    for part in [
        kind,
        object.account_id.as_str(),
        object.bucket.as_str(),
        object.key.as_str(),
        object.etag.as_deref().unwrap_or(""),
        size.as_str(),
        object.last_modified.as_deref().unwrap_or(""),
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    let digest: [u8; 32] = hash.finalize().into();
    let hex = hex::encode(digest);
    format!("{}/{}.webp", &hex[..2], &hex[2..34])
}

/// Resolve an index path inside `dir`, refusing anything that could escape it.
fn resolve(dir: &Path, relative: &str) -> Option<PathBuf> {
    let mut out = dir.to_path_buf();
    for part in relative.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(['\\', ':']) {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// Write `bytes` to `dir/relative` atomically: a temp file in the same
/// directory renamed over the target (rename replaces on every platform).
fn write_atomic(dir: &Path, relative: &str, bytes: &[u8]) -> std::io::Result<()> {
    let target = resolve(dir, relative)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad cache path"))?;
    let parent = target.parent().unwrap_or(dir);
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &target));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn remove_files_in(dir: &Path, files: &[String]) {
    for file in files {
        if let Some(path) = resolve(dir, file) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => log::warn!("thumbnail cache: cannot remove {}: {e}", path.display()),
            }
        }
    }
}

pub fn remove_files(files: &[String]) {
    if let Some(dir) = cache_dir() {
        remove_files_in(dir, files);
    }
}

// ============ Invalidation from listings and writes ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangedObject {
    key: String,
    kind: String,
    size: Option<i64>,
    last_modified: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InvalidatedPayload {
    account_id: String,
    bucket: String,
    changed: Vec<ChangedObject>,
    removed: Vec<String>,
}

/// Delete the files of reconciled rows and tell the frontend which objects
/// changed (to regenerate) or went away (to forget).
pub fn apply_reconciled(bucket: &str, account_id: &str, reconciled: Reconciled) {
    if reconciled.is_empty() {
        return;
    }
    remove_files(&reconciled.files);
    dev_log(|| {
        format!(
            "invalidated {bucket}: {} changed, {} removed",
            reconciled.changed.len(),
            reconciled.removed.len()
        )
    });
    if let Some(app) = APP.get() {
        let payload = InvalidatedPayload {
            account_id: account_id.to_string(),
            bucket: bucket.to_string(),
            changed: reconciled
                .changed
                .into_iter()
                .map(|e| ChangedObject {
                    key: e.key,
                    kind: e.kind,
                    size: e.size,
                    last_modified: e.last_modified,
                })
                .collect(),
            removed: reconciled.removed,
        };
        let _ = app.emit(INVALIDATED_EVENT, payload);
    }
}

/// Best-effort reconcile used by the listing and write paths: a failure here
/// must never fail the cache operation that triggered it.
pub async fn reconcile_after_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    scope: thumbnails::Scope<'_>,
) {
    match thumbnails::reconcile_on(conn, bucket, account_id, scope).await {
        Ok(reconciled) => apply_reconciled(bucket, account_id, reconciled),
        Err(e) => log::warn!("thumbnail cache: reconcile failed: {e}"),
    }
}

/// Best-effort removal of the thumbnails of deleted or moved-away keys.
pub async fn forget_keys_on(
    conn: &turso::Connection,
    bucket: &str,
    account_id: &str,
    keys: &[&str],
) {
    match thumbnails::remove_keys_on(conn, bucket, account_id, keys).await {
        Ok(files) if !files.is_empty() => {
            remove_files(&files);
            if let Some(app) = APP.get() {
                let payload = InvalidatedPayload {
                    account_id: account_id.to_string(),
                    bucket: bucket.to_string(),
                    changed: Vec::new(),
                    removed: keys.iter().map(|k| k.to_string()).collect(),
                };
                let _ = app.emit(INVALIDATED_EVENT, payload);
            }
        }
        Ok(_) => {}
        Err(e) => log::warn!("thumbnail cache: forget failed: {e}"),
    }
}

// ============ Rendering ============

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
}

impl Kind {
    fn parse(kind: &str) -> Result<Self, String> {
        match kind {
            "image" => Ok(Kind::Image),
            "video" => Ok(Kind::Video),
            other => Err(format!("Unknown thumbnail kind: {other}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Video => "video",
        }
    }
}

fn is_remote(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(IMAGE_FETCH_TIMEOUT)
            .build()
            .unwrap_or_default()
    })
}

/// Read an image, giving up as soon as it is known to exceed the cap.
async fn fetch_image(source: &str) -> Result<Vec<u8>, String> {
    if !is_remote(source) {
        let len = tokio::fs::metadata(source)
            .await
            .map_err(|e| format!("Cannot read image: {e}"))?
            .len();
        if len > IMAGE_MAX_BYTES {
            return Err(IMAGE_TOO_LARGE.to_string());
        }
        return tokio::fs::read(source)
            .await
            .map_err(|e| format!("Cannot read image: {e}"));
    }
    let response = http_client()
        .get(source)
        .send()
        .await
        .map_err(|e| format!("Image request failed: {}", e.without_url()))?;
    if !response.status().is_success() {
        return Err(format!(
            "Image request failed: HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|len| len > IMAGE_MAX_BYTES)
    {
        return Err(IMAGE_TOO_LARGE.to_string());
    }
    let mut out = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Image download failed: {}", e.without_url()))?;
        if out.len() as u64 + chunk.len() as u64 > IMAGE_MAX_BYTES {
            return Err(IMAGE_TOO_LARGE.to_string());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    use image::ImageDecoder;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// Downscale to at most `width` wide (never upscale; very tall images are
/// bounded too) and encode as lossy WebP, keeping alpha when there is any.
fn to_webp(img: image::DynamicImage, width: u32) -> Result<Vec<u8>, String> {
    let max_height = width.saturating_mul(3);
    let img = if img.width() > width || img.height() > max_height {
        img.thumbnail(width, max_height)
    } else {
        img
    };
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("Image is empty".to_string());
    }
    let encoded = if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        webp::Encoder::from_rgba(&rgba, w, h)
            .encode(WEBP_QUALITY)
            .to_vec()
    } else {
        let rgb = img.to_rgb8();
        webp::Encoder::from_rgb(&rgb, w, h)
            .encode(WEBP_QUALITY)
            .to_vec()
    };
    if encoded.is_empty() {
        return Err("WebP encoding failed".to_string());
    }
    Ok(encoded)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("Thumbnail worker failed: {e}"))?
}

async fn render_image(source: &str, width: u32) -> Result<Vec<u8>, String> {
    let _slot = IMAGE_SLOTS
        .acquire()
        .await
        .map_err(|e| format!("Thumbnail queue closed: {e}"))?;
    let bytes = fetch_image(source).await?;
    let bytes = std::sync::Arc::new(bytes);
    let input = bytes.clone();
    match blocking(move || decode_image(&input).and_then(|img| to_webp(img, width))).await {
        Ok(webp) => Ok(webp),
        Err(decode_error) => {
            // Not a format the Rust decoders know: let ffmpeg try. It needs a
            // seekable file (HEIF is an ISO-BMFF container).
            let Some(dir) = cache_dir() else {
                return Err(format!("Unsupported image: {decode_error}"));
            };
            let tmp = dir.join(format!(
                ".tmp-src-{}-{}",
                std::process::id(),
                TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            tokio::fs::write(&tmp, bytes.as_slice())
                .await
                .map_err(|e| format!("Unsupported image: {decode_error} ({e})"))?;
            let frame = video_thumbnail::render_still_frame(&tmp, width).await;
            let _ = tokio::fs::remove_file(&tmp).await;
            let frame = frame.map_err(|e| format!("Unsupported image: {decode_error} ({e})"))?;
            blocking(move || decode_image(&frame).and_then(|img| to_webp(img, width))).await
        }
    }
}

async fn render_video(source: &str, width: u32) -> Result<Vec<u8>, String> {
    let frame = video_thumbnail::render_video_frame(source, width).await?;
    blocking(move || decode_image(&frame).and_then(|img| to_webp(img, width))).await
}

async fn render(kind: Kind, source: &str, width: u32) -> Result<Vec<u8>, String> {
    match kind {
        Kind::Image => render_image(source, width).await,
        Kind::Video => render_video(source, width).await,
    }
}

// ============ Commands ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailResult {
    pub data_url: String,
    /// True when served from the disk cache without rendering.
    pub from_cache: bool,
}

fn data_url(bytes: &[u8]) -> String {
    format!("data:image/webp;base64,{}", STANDARD.encode(bytes))
}

/// Serve the cached file when it was rendered from this version of the object.
async fn read_cached(dir: &Path, object: &ThumbnailObject) -> Option<Vec<u8>> {
    let conn = crate::db::get_connection().ok()?.lock().await;
    let entry = match thumbnails::lookup_on(&conn, object).await {
        Ok(Lookup::Fresh(entry)) => entry,
        _ => return None,
    };
    let path = resolve(dir, &entry.file_path)?;
    match tokio::fs::read(&path).await {
        Ok(bytes) if !bytes.is_empty() => {
            if let Err(e) =
                thumbnails::touch_on(&conn, object, now_secs(), TOUCH_INTERVAL_SECS).await
            {
                log::warn!("thumbnail cache: touch failed: {e}");
            }
            Some(bytes)
        }
        // The file went missing (cleared by hand, disk cleanup): render again.
        _ => None,
    }
}

async fn store(
    dir: &Path,
    object: &ThumbnailObject,
    kind: Kind,
    bytes: &[u8],
) -> Result<(), String> {
    let relative = file_path_for(object, kind.as_str());
    {
        let dir = dir.to_path_buf();
        let relative = relative.clone();
        let bytes = bytes.to_vec();
        blocking(move || write_atomic(&dir, &relative, &bytes).map_err(|e| e.to_string())).await?;
    }
    let previous = {
        let conn = crate::db::get_connection()
            .map_err(|e| e.to_string())?
            .lock()
            .await;
        thumbnails::upsert_on(
            &conn,
            object,
            kind.as_str(),
            &relative,
            bytes.len() as i64,
            now_secs(),
        )
        .await
        .map_err(|e| e.to_string())?
    };
    if let Some(previous) = previous {
        remove_files_in(dir, &[previous]);
    }
    schedule_eviction();
    Ok(())
}

/// Thumbnail of an image or video as a WebP `data:` URL. With `object` (the
/// listing's identity of the file) the result is served from and stored in
/// the disk cache; without it, it is only rendered.
#[tauri::command]
pub async fn get_thumbnail(
    kind: String,
    source: String,
    max_width: Option<u32>,
    object: Option<ThumbnailObject>,
) -> Result<ThumbnailResult, String> {
    let kind = Kind::parse(&kind)?;
    let width = video_thumbnail::clamp_width(max_width);
    let started = Instant::now();
    let cached = match (cache_dir(), &object) {
        (Some(dir), Some(object)) => Some((dir, object)),
        _ => None,
    };

    if let Some((dir, object)) = cached {
        if let Some(bytes) = read_cached(dir, object).await {
            dev_log(|| {
                format!(
                    "disk hit {} {} ({} B, {} ms)",
                    kind.as_str(),
                    object.key,
                    bytes.len(),
                    started.elapsed().as_millis()
                )
            });
            return Ok(ThumbnailResult {
                data_url: data_url(&bytes),
                from_cache: true,
            });
        }
    }
    if source.trim().is_empty() {
        return Err("No thumbnail source".to_string());
    }
    if kind == Kind::Image {
        if let Some(size) = object.as_ref().and_then(|o| o.size) {
            if size as u64 > IMAGE_MAX_BYTES {
                return Err(IMAGE_TOO_LARGE.to_string());
            }
        }
    }

    let bytes = render(kind, &source, width).await?;
    if let Some((dir, object)) = cached {
        if let Err(e) = store(dir, object, kind, &bytes).await {
            log::warn!("thumbnail cache: store failed: {e}");
        }
        dev_log(|| {
            format!(
                "generated {} {} ({} B, {} ms)",
                kind.as_str(),
                object.key,
                bytes.len(),
                started.elapsed().as_millis()
            )
        });
    }
    Ok(ThumbnailResult {
        data_url: data_url(&bytes),
        from_cache: false,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatsResponse {
    pub count: i64,
    pub bytes: i64,
    pub max_bytes: i64,
    pub image_max_bytes: u64,
}

async fn limit_bytes() -> i64 {
    match crate::db::app_state::get_app_state(LIMIT_KEY).await {
        Ok(Some(value)) => value
            .parse::<i64>()
            .ok()
            .map(clamp_limit)
            .unwrap_or(DEFAULT_MAX_BYTES),
        _ => DEFAULT_MAX_BYTES,
    }
}

fn clamp_limit(bytes: i64) -> i64 {
    bytes.clamp(MIN_LIMIT_BYTES, MAX_LIMIT_BYTES)
}

async fn stats() -> Result<ThumbnailStats, String> {
    let conn = crate::db::get_connection()
        .map_err(|e| e.to_string())?
        .lock()
        .await;
    thumbnails::stats_on(&conn).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_thumbnail_cache_stats() -> Result<CacheStatsResponse, String> {
    let stats = stats().await?;
    Ok(CacheStatsResponse {
        count: stats.count,
        bytes: stats.bytes,
        max_bytes: limit_bytes().await,
        image_max_bytes: IMAGE_MAX_BYTES,
    })
}

#[tauri::command]
pub async fn set_thumbnail_cache_limit(max_bytes: i64) -> Result<CacheStatsResponse, String> {
    let max_bytes = clamp_limit(max_bytes);
    crate::db::app_state::set_app_state(LIMIT_KEY, &max_bytes.to_string())
        .await
        .map_err(|e| e.to_string())?;
    run_eviction().await;
    get_thumbnail_cache_stats().await
}

/// Empty the index and the directory. Returns what was removed.
pub async fn clear_in(dir: &Path) -> Result<ThumbnailStats, String> {
    let removed = {
        let conn = crate::db::get_connection()
            .map_err(|e| e.to_string())?
            .lock()
            .await;
        thumbnails::clear_on(&conn)
            .await
            .map_err(|e| e.to_string())?
    };
    let dir = dir.to_path_buf();
    blocking(move || {
        // Remove the shard directories one by one: a file another process
        // (an antivirus scanner on Windows) holds open must not abort the rest.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let _ = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
            }
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())
    })
    .await?;
    Ok(removed)
}

#[tauri::command]
pub async fn clear_thumbnail_cache() -> Result<CacheStatsResponse, String> {
    let dir = cache_dir().ok_or("Thumbnail cache is not ready")?;
    let removed = clear_in(dir).await?;
    dev_log(|| format!("cleared {} thumbnails ({} B)", removed.count, removed.bytes));
    if let Some(app) = APP.get() {
        let _ = app.emit(CLEARED_EVENT, ());
    }
    get_thumbnail_cache_stats().await
}

// ============ Maintenance ============

/// Evict now: aged rows, then least recently used down to 90% of the limit.
pub async fn evict_in(dir: &Path, max_bytes: i64, now: i64) -> Result<usize, String> {
    let files = {
        let conn = crate::db::get_connection()
            .map_err(|e| e.to_string())?
            .lock()
            .await;
        thumbnails::evict_on(&conn, max_bytes, max_bytes / 10 * 9, MAX_AGE_SECS, now)
            .await
            .map_err(|e| e.to_string())?
    };
    let count = files.len();
    if count > 0 {
        let dir = dir.to_path_buf();
        blocking(move || {
            remove_files_in(&dir, &files);
            Ok(())
        })
        .await?;
    }
    Ok(count)
}

async fn run_eviction() {
    let Some(dir) = cache_dir() else { return };
    LAST_EVICTION.store(now_secs(), Ordering::Relaxed);
    let limit = limit_bytes().await;
    match evict_in(dir, limit, now_secs()).await {
        Ok(0) => {}
        Ok(n) => dev_log(|| format!("evicted {n} thumbnails")),
        Err(e) => log::warn!("thumbnail cache: eviction failed: {e}"),
    }
}

/// Evict in the background after a write, at most once per interval.
fn schedule_eviction() {
    let now = now_secs();
    let last = LAST_EVICTION.load(Ordering::Relaxed);
    if now - last < EVICT_INTERVAL_SECS {
        return;
    }
    if LAST_EVICTION
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        tauri::async_runtime::spawn(run_eviction());
    }
}

/// Delete files the index does not know (left by a crash between rename and
/// upsert, or by a database reset) and stale temp files.
pub async fn sweep_orphans() -> Result<usize, String> {
    let Some(dir) = cache_dir() else { return Ok(0) };
    sweep_orphans_in(dir).await
}

pub async fn sweep_orphans_in(dir: &Path) -> Result<usize, String> {
    let known = {
        let conn = crate::db::get_connection()
            .map_err(|e| e.to_string())?
            .lock()
            .await;
        thumbnails::file_paths_on(&conn)
            .await
            .map_err(|e| e.to_string())?
    };
    let dir = dir.to_path_buf();
    blocking(move || {
        let mut removed = 0;
        let stale_tmp = std::time::SystemTime::now() - Duration::from_secs(3600);
        for shard in std::fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let shard_path = shard.path();
            let shard_name = shard.file_name().to_string_lossy().into_owned();
            if !shard_path.is_dir() {
                let old = shard
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t < stale_tmp)
                    .unwrap_or(true);
                if shard_name.starts_with(".tmp-")
                    && old
                    && std::fs::remove_file(&shard_path).is_ok()
                {
                    removed += 1;
                }
                continue;
            }
            for file in std::fs::read_dir(&shard_path)
                .into_iter()
                .flatten()
                .flatten()
            {
                let name = file.file_name().to_string_lossy().into_owned();
                let relative = format!("{shard_name}/{name}");
                let orphan = if name.starts_with(".tmp-") {
                    file.metadata()
                        .and_then(|m| m.modified())
                        .map(|t| t < stale_tmp)
                        .unwrap_or(true)
                } else {
                    !known.contains(&relative)
                };
                if orphan && std::fs::remove_file(file.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(key: &str, size: i64, lm: &str) -> ThumbnailObject {
        ThumbnailObject {
            account_id: "acct".into(),
            bucket: "bkt".into(),
            key: key.into(),
            etag: None,
            size: Some(size),
            last_modified: Some(lm.into()),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "r2-thumb-{name}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_path_depends_on_identity_not_on_format_or_url() {
        let a = file_path_for(&obj("a/b.jpg", 10, "2026-11-01T00:00:00.000Z"), "image");
        let same = file_path_for(&obj("a/b.jpg", 10, "2026-11-01T00:00:00Z"), "image");
        assert_eq!(a, same);
        assert_ne!(
            a,
            file_path_for(&obj("a/b.jpg", 11, "2026-11-01T00:00:00Z"), "image")
        );
        assert_ne!(
            a,
            file_path_for(&obj("a/c.jpg", 10, "2026-11-01T00:00:00Z"), "image")
        );
        assert_ne!(
            a,
            file_path_for(&obj("a/b.jpg", 10, "2026-11-01T00:00:00Z"), "video")
        );
        let mut other_bucket = obj("a/b.jpg", 10, "2026-11-01T00:00:00Z");
        other_bucket.bucket = "other".into();
        assert_ne!(a, file_path_for(&other_bucket, "image"));
        // Length-prefixed parts: no ambiguity from concatenation.
        let mut x = obj("b", 1, "t");
        x.bucket = "a".into();
        let mut y = obj("", 1, "t");
        y.bucket = "ab".into();
        assert_ne!(file_path_for(&x, "image"), file_path_for(&y, "image"));
        // Sharded, hex, WebP.
        let (shard, name) = a.split_once('/').unwrap();
        assert_eq!(shard.len(), 2);
        assert!(name.ends_with(".webp"));
        assert!(a
            .trim_end_matches(".webp")
            .chars()
            .all(|c| c == '/' || c.is_ascii_hexdigit()));
    }

    #[test]
    fn resolve_never_escapes_the_cache_dir() {
        let dir = Path::new("/cache");
        assert_eq!(
            resolve(dir, "ab/cd.webp"),
            Some(PathBuf::from("/cache").join("ab").join("cd.webp"))
        );
        assert_eq!(resolve(dir, "../x"), None);
        assert_eq!(resolve(dir, "/etc/passwd"), None);
        assert_eq!(resolve(dir, "ab\\..\\x"), None);
        assert_eq!(resolve(dir, "C:/x"), None);
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp_files() {
        let dir = temp_dir("atomic");
        write_atomic(&dir, "ab/x.webp", b"one").unwrap();
        write_atomic(&dir, "ab/x.webp", b"two").unwrap();
        assert_eq!(
            std::fs::read(dir.join("ab").join("x.webp")).unwrap(),
            b"two"
        );
        let names: Vec<_> = std::fs::read_dir(dir.join("ab"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["x.webp".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sample_png(width: u32, height: u32, alpha: bool) -> Vec<u8> {
        let img: image::DynamicImage = if alpha {
            image::RgbaImage::from_fn(width, height, |x, y| {
                image::Rgba([x as u8, y as u8, 90, 128])
            })
            .into()
        } else {
            image::RgbImage::from_fn(width, height, |x, y| image::Rgb([x as u8, y as u8, 90]))
                .into()
        };
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn images_are_downscaled_to_webp_and_never_upscaled() {
        for alpha in [false, true] {
            let img = decode_image(&sample_png(1200, 800, alpha)).unwrap();
            let webp = to_webp(img, 480).unwrap();
            assert_eq!(&webp[..4], b"RIFF");
            assert_eq!(&webp[8..12], b"WEBP");
            let back = image::load_from_memory(&webp).unwrap();
            assert_eq!((back.width(), back.height()), (480, 320));
        }
        let small = to_webp(decode_image(&sample_png(100, 50, false)).unwrap(), 480).unwrap();
        let back = image::load_from_memory(&small).unwrap();
        assert_eq!((back.width(), back.height()), (100, 50));
        // A very tall strip is bounded in height too.
        let tall = to_webp(decode_image(&sample_png(600, 6000, false)).unwrap(), 480).unwrap();
        let back = image::load_from_memory(&tall).unwrap();
        assert!(back.height() <= 1440 && back.width() <= 480);
    }

    #[test]
    fn garbage_is_rejected_by_the_rust_decoders() {
        assert!(decode_image(b"not an image").is_err());
    }

    #[tokio::test]
    async fn oversized_local_images_are_refused_before_reading() {
        let dir = temp_dir("big");
        let path = dir.join("big.png");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(IMAGE_MAX_BYTES + 1).unwrap();
        let err = fetch_image(path.to_str().unwrap()).await.unwrap_err();
        assert_eq!(err, IMAGE_TOO_LARGE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Store, hit, change, evict, sweep and clear against the process-wide
    /// test database (one test, so the shared index is not raced).
    #[tokio::test]
    async fn store_serve_replace_evict_sweep_and_clear() {
        crate::db::init_test_db().await;
        let dir = temp_dir("flow");
        {
            let conn = crate::db::get_connection().unwrap().lock().await;
            thumbnails::clear_on(&conn).await.unwrap();
        }
        let v1 = obj("pics/cat.png", 10, "2026-11-01T00:00:00Z");
        store(&dir, &v1, Kind::Image, b"webp-v1").await.unwrap();
        assert_eq!(
            read_cached(&dir, &v1).await.as_deref(),
            Some(&b"webp-v1"[..])
        );
        let v1_file = resolve(&dir, &file_path_for(&v1, "image")).unwrap();
        assert!(v1_file.is_file());

        // The object changed: no hit; storing the new version replaces the file.
        let v2 = obj("pics/cat.png", 12, "2026-11-02T00:00:00Z");
        assert_eq!(read_cached(&dir, &v2).await, None);
        store(&dir, &v2, Kind::Image, b"webp-v2").await.unwrap();
        assert!(!v1_file.exists(), "the old version's file is deleted");
        assert_eq!(
            read_cached(&dir, &v2).await.as_deref(),
            Some(&b"webp-v2"[..])
        );

        // A missing file is a miss, not an error.
        let v2_file = resolve(&dir, &file_path_for(&v2, "image")).unwrap();
        std::fs::remove_file(&v2_file).unwrap();
        assert_eq!(read_cached(&dir, &v2).await, None);
        store(&dir, &v2, Kind::Image, &[2u8; 40]).await.unwrap();

        // 104 bytes over a 100-byte limit: dropping the least recently used
        // entry brings the total under the 90-byte target.
        let other = obj("pics/dog.png", 1, "t");
        store(&dir, &other, Kind::Image, &[7u8; 64]).await.unwrap();
        {
            let conn = crate::db::get_connection().unwrap().lock().await;
            conn.execute(
                "UPDATE thumbnail_cache SET last_used_at = 1 WHERE key = 'pics/cat.png'",
                (),
            )
            .await
            .unwrap();
        }
        assert_eq!(evict_in(&dir, 100, now_secs()).await.unwrap(), 1);
        assert!(!v2_file.exists());
        assert_eq!(
            stats().await.unwrap(),
            ThumbnailStats {
                count: 1,
                bytes: 64
            }
        );

        // Orphans and stale temp files are swept; indexed files stay.
        write_atomic(&dir, "zz/orphan.webp", b"x").unwrap();
        assert_eq!(sweep_orphans_in(&dir).await.unwrap(), 1);
        assert!(resolve(&dir, &file_path_for(&other, "image"))
            .unwrap()
            .is_file());

        // Clearing removes rows and files.
        let removed = clear_in(&dir).await.unwrap();
        assert_eq!(
            removed,
            ThumbnailStats {
                count: 1,
                bytes: 64
            }
        );
        assert_eq!(
            stats().await.unwrap(),
            ThumbnailStats { count: 0, bytes: 0 }
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Needs ffmpeg on the machine; skipped otherwise.
    #[tokio::test]
    async fn video_frames_become_webp() {
        let dir = temp_dir("video");
        let path = dir.join("clip.mp4");
        let Some(ffmpeg) = video_thumbnail::locate_ffmpeg() else {
            eprintln!("ffmpeg not installed; skipping");
            return;
        };
        let status = std::process::Command::new(&ffmpeg)
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=640x360:rate=25",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let webp = render(Kind::Video, path.to_str().unwrap(), 320)
            .await
            .unwrap();
        assert_eq!(&webp[8..12], b"WEBP");
        let back = image::load_from_memory(&webp).unwrap();
        assert_eq!(back.width(), 320);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
