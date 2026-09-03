use crate::db::{self, CachedFile};
use crate::providers::aws;
use crate::providers::minio;
use crate::providers::s3_client::{describe_s3_error, is_transient_s3_error};
use crate::r2;
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::operation::list_objects_v2::builders::ListObjectsV2FluentBuilder;
use aws_sdk_s3::operation::list_objects_v2::{ListObjectsV2Error, ListObjectsV2Output};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tauri::Emitter;

// ============ Types ============

#[derive(Debug, Deserialize)]
pub struct LazyListInput {
    pub account_id: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub prefix: String, // "" for root, "folder/" for subfolder
    // Provider-aware fields (all optional for backward compatibility)
    pub provider: Option<String>,
    pub endpoint_scheme: Option<String>,
    pub endpoint_host: Option<String>,
    pub force_path_style: Option<bool>,
    pub region: Option<String>,
    pub force_refresh: Option<bool>,
}

// ============ Provider-Aware Client Factory ============

async fn create_client_for_input(input: &LazyListInput) -> Result<aws_sdk_s3::Client, String> {
    let provider = input.provider.as_deref().unwrap_or("r2");
    match provider {
        "minio" | "rustfs" => {
            let config = minio::MinioConfig {
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
                endpoint_scheme: input
                    .endpoint_scheme
                    .clone()
                    .unwrap_or_else(|| "http".into()),
                endpoint_host: input.endpoint_host.clone().unwrap_or_default(),
                force_path_style: input.force_path_style.unwrap_or(true),
            };
            minio::create_minio_client(&config)
                .await
                .map_err(|e| format!("Failed to create {} client: {}", provider, e))
        }
        "aws" => {
            let config = aws::AwsConfig {
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
                region: input.region.clone().unwrap_or_else(|| "us-east-1".into()),
                endpoint_scheme: input.endpoint_scheme.clone(),
                endpoint_host: input.endpoint_host.clone(),
                force_path_style: input.force_path_style.unwrap_or(false),
            };
            aws::create_aws_client(&config)
                .await
                .map_err(|e| format!("Failed to create aws client: {}", e))
        }
        _ => {
            let config = r2::R2Config {
                account_id: input.account_id.clone(),
                bucket: input.bucket.clone(),
                access_key_id: input.access_key_id.clone(),
                secret_access_key: input.secret_access_key.clone(),
            };
            r2::create_r2_client(&config)
                .await
                .map_err(|e| format!("Failed to create r2 client: {}", e))
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LazyListResult {
    pub files: Vec<LazyFileItem>,
    pub folders: Vec<String>,
    pub prefix: String,
    pub from_cache: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LazyFileItem {
    pub key: String,
    pub name: String,
    pub size: i64,
    pub last_modified: String,
}

// ============ list_prefix Command ============

/// Lazy-list a single prefix using delimiter="/".
/// If cache is fresh (< 60s), serves from SQLite. Otherwise hits S3.
#[tauri::command]
pub async fn list_prefix(
    input: LazyListInput,
    app: tauri::AppHandle,
) -> Result<LazyListResult, String> {
    let bucket = &input.bucket;
    let account_id = &input.account_id;
    let prefix = &input.prefix;

    const STALE_THRESHOLD_SECS: i64 = 60;

    if !input.force_refresh.unwrap_or(false) {
        // A completed full sync makes the local cache authoritative for the
        // whole bucket — background sync and incremental cache updates keep it
        // fresh, so browsing never needs to wait on a network LIST. Without a
        // full sync, fall back to the per-prefix lazy TTL.
        let skipped = load_skipped_prefixes(bucket, account_id).await;
        let cache_is_authoritative = match &skipped {
            Some(skipped) if !is_under_skipped_prefix(prefix, skipped) => {
                db::has_full_sync(bucket, account_id)
                    .await
                    .map_err(|e| format!("DB error: {}", e))?
            }
            // Either the last sync could not read this folder, so the cache
            // holds nothing for it, or which folders those are is unknown.
            // Neither may be answered from a cache claiming to be complete:
            // that shows an empty folder and implies the data is gone.
            _ => false,
        };

        let serve_cache = if cache_is_authoritative {
            true
        } else {
            // The per-prefix TTL still applies, so a folder listed moments ago
            // is not re-fetched; a folder the sync skipped has no such record
            // and goes to the network.
            let cached_time = db::prefix_sync::get_prefix_sync_time(bucket, account_id, prefix)
                .await
                .map_err(|e| format!("DB error: {}", e))?;
            let now = chrono::Utc::now().timestamp();
            matches!(cached_time, Some(synced_at) if now - synced_at < STALE_THRESHOLD_SECS)
        };

        if serve_cache {
            let contents = db::get_folder_contents(bucket, account_id, prefix)
                .await
                .map_err(|e| format!("DB error: {}", e))?;

            return Ok(LazyListResult {
                files: contents
                    .files
                    .into_iter()
                    .map(|f| LazyFileItem {
                        name: f.name,
                        key: f.key,
                        size: f.size,
                        last_modified: f.last_modified,
                    })
                    .collect(),
                folders: contents.folders,
                prefix: prefix.clone(),
                from_cache: true,
            });
        }
    }

    // Cache is stale or missing -- fetch from S3
    let now = chrono::Utc::now().timestamp();
    let client = create_client_for_input(&input).await?;

    // Paginate with delimiter to get immediate children only
    let mut all_files: Vec<CachedFile> = Vec::new();
    let mut all_folders: Vec<String> = Vec::new();
    let mut continuation_token: Option<String> = None;
    let mut page_count = 0;

    loop {
        let create_request = || {
            let mut request = client
                .list_objects_v2()
                .bucket(bucket)
                .delimiter("/")
                .max_keys(1000);

            if !prefix.is_empty() {
                request = request.prefix(prefix);
            }

            if let Some(token) = &continuation_token {
                request = request.continuation_token(token);
            }

            request
        };

        let response = list_with_retry(
            FOREGROUND_LIST_RETRY,
            || true,
            || send_locked(create_request()),
        )
        .await
        .map_err(|failure| match failure {
            ListFailure::Failed(message) => message,
            ListFailure::Cancelled => "S3 list cancelled".to_string(),
        })?;

        page_count += 1;

        // Collect files (objects at this level)
        for obj in response.contents() {
            if let Some(key) = obj.key() {
                let key = key.to_string();
                if key.ends_with('/') {
                    continue; // Skip folder marker objects
                }
                let (parent_path, name) = db::parse_key(&key);
                all_files.push(CachedFile {
                    bucket: bucket.clone(),
                    account_id: account_id.clone(),
                    key,
                    parent_path,
                    name,
                    size: obj.size().unwrap_or(0),
                    last_modified: obj
                        .last_modified()
                        .map(|dt| dt.to_string())
                        .unwrap_or_default(),
                    synced_at: now,
                });
            }
        }

        // Collect folders (common prefixes)
        for cp in response.common_prefixes() {
            if let Some(p) = cp.prefix() {
                all_folders.push(p.to_string());
            }
        }

        // Emit progress for multi-page prefixes
        if page_count > 1 {
            let _ = app.emit(
                "folder-load-progress",
                serde_json::json!({
                    "pages": page_count,
                    "items": all_files.len() + all_folders.len(),
                }),
            );
        }

        let is_truncated = response.is_truncated().unwrap_or(false);
        if !is_truncated {
            break;
        }
        continuation_token = response.next_continuation_token().map(|s| s.to_string());
    }

    // Cache results in SQLite
    db::upsert_prefix_files(bucket, account_id, prefix, &all_files)
        .await
        .map_err(|e| format!("Failed to cache files: {}", e))?;

    // Upsert folder entries into directory_tree for this prefix
    for folder in &all_folders {
        db::ensure_directory_node(bucket, account_id, folder)
            .await
            .map_err(|e| format!("Failed to upsert directory node: {}", e))?;
    }

    // Record sync time
    db::prefix_sync::set_prefix_sync_time(
        bucket,
        account_id,
        prefix,
        all_files.len() as i32,
        all_folders.len() as i32,
    )
    .await
    .map_err(|e| format!("Failed to record sync time: {}", e))?;

    let result = LazyListResult {
        files: all_files
            .into_iter()
            .map(|f| LazyFileItem {
                name: f.name,
                key: f.key,
                size: f.size,
                last_modified: f.last_modified,
            })
            .collect(),
        folders: all_folders,
        prefix: prefix.clone(),
        from_cache: false,
    };

    Ok(result)
}

// ============ Background Sync (Task 3) ============

// Global cancellation token for background sync (one per app)
static BACKGROUND_CANCEL: LazyLock<Arc<AtomicBool>> =
    LazyLock::new(|| Arc::new(AtomicBool::new(false)));
static S3_LIST_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));
static BACKGROUND_RUN_ID: AtomicU64 = AtomicU64::new(0);
static BACKGROUND_SYNC_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

fn is_background_run_active(run_id: u64) -> bool {
    BACKGROUND_RUN_ID.load(Ordering::SeqCst) == run_id && !BACKGROUND_CANCEL.load(Ordering::SeqCst)
}

// ============ Unlistable Prefixes ============

/// Where a completed sync records the prefixes it could not read.
fn skipped_prefixes_key(bucket: &str, account_id: &str) -> String {
    format!("skipped_prefixes:{account_id}:{bucket}")
}

/// The prefixes the last completed sync could not read.
///
/// `None` means the answer is unknown — the read failed, or the stored value is
/// not one this build understands. That is deliberately not the same as "the
/// sync skipped nothing". A skipped folder is cached as empty, so treating an
/// unknown as an empty list would serve that folder from cache and show it as
/// empty, which is the failure this whole mechanism exists to prevent. The
/// caller fails closed on `None`.
async fn load_skipped_prefixes(bucket: &str, account_id: &str) -> Option<Vec<String>> {
    match db::app_state::get_app_state(&skipped_prefixes_key(bucket, account_id)).await {
        // No row at all is a real answer: the last sync read every prefix.
        Ok(None) => Some(Vec::new()),
        Ok(Some(value)) => serde_json::from_str::<Vec<String>>(&value).ok(),
        Err(_) => None,
    }
}

/// Records what a completed sync skipped, clearing the note when it skipped
/// nothing — so a bucket heals itself once the provider is fixed.
async fn store_skipped_prefixes(bucket: &str, account_id: &str, skipped: &[String]) {
    let key = skipped_prefixes_key(bucket, account_id);
    if skipped.is_empty() {
        let _ = db::app_state::delete_app_state(&key).await;
        return;
    }
    // A write that fails here is the one case the fail-closed read cannot
    // catch: no row is stored, so the next read returns a confident "nothing
    // was skipped" and the authoritative cache serves the skipped folder as
    // empty. Rather than leave that claim standing, retract it — the bucket
    // keeps its rows but stops asserting it holds everything, so browsing
    // lists live until a later sync gets the record written.
    let recorded = match serde_json::to_string(skipped) {
        Ok(value) => db::app_state::set_app_state(&key, &value).await.is_ok(),
        Err(_) => false,
    };
    if !recorded {
        eprintln!(
            "Could not record {} unlistable prefix(es) for {bucket}; \
             dropping the full-sync marker so browsing re-lists instead",
            skipped.len()
        );
        let _ = db::clear_full_sync_marker(bucket, account_id).await;
    }

    // `finish_sync` has just dropped every live row for this bucket, including
    // any a skipped folder still had from an earlier successful listing, but
    // that folder's freshness record lives in another table and would outlive
    // them. Left alone, a folder browsed moments before the sync skipped it
    // would read as fresh and serve nothing. Clearing the records costs
    // nothing here: a completed sync makes the cache authoritative, so the
    // freshness path is only consulted for the skipped folders themselves.
    let _ = db::prefix_sync::clear_prefix_sync_times(bucket, account_id).await;
}

/// Whether `prefix` is the folder a sync could not read, or sits under one.
///
/// Such a folder is cached as empty, which is indistinguishable from a folder
/// that really is empty — so it must never be served from cache. Opening it
/// re-lists it live, which reports the provider's error honestly and starts
/// working again on its own once the provider does.
fn is_under_skipped_prefix(prefix: &str, skipped: &[String]) -> bool {
    skipped.iter().any(|s| prefix.starts_with(s.as_str()))
}

// ============ List Retry ============

/// How long a listing keeps trying against a provider that is failing right now.
///
/// The SDK already retries each call a few times within a couple of seconds;
/// this layer is for the outage that outlasts that. Attempts are spread out
/// exponentially, from `initial_backoff` up to `max_backoff`.
#[derive(Debug, Clone, Copy)]
struct ListRetryPolicy {
    /// Attempts in total, counting the first.
    max_attempts: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl ListRetryPolicy {
    /// The wait after `failed_attempts` failures in a row: 1×, 2×, 4×… the
    /// initial backoff, capped.
    fn backoff(&self, failed_attempts: u32) -> Duration {
        let doublings = failed_attempts.saturating_sub(1).min(MAX_BACKOFF_DOUBLINGS);
        self.initial_backoff
            .saturating_mul(1 << doublings)
            .min(self.max_backoff)
    }
}

/// Ceiling on the left shift in `backoff`, so a policy with a large
/// `max_attempts` cannot overflow it. Neither policy here comes close — six
/// attempts reach four doublings — so this guards future ones, not these.
const MAX_BACKOFF_DOUBLINGS: u32 = 16;

/// Someone is looking at an empty folder while this runs: 0.5s, then 1s, then
/// give up and let them see the error.
const FOREGROUND_LIST_RETRY: ListRetryPolicy = ListRetryPolicy {
    max_attempts: 3,
    initial_backoff: Duration::from_millis(500),
    max_backoff: Duration::from_secs(1),
};

/// A crawl takes minutes anyway; waiting out a half-minute outage (1+2+4+8+16s)
/// beats starting it over.
const BACKGROUND_LIST_RETRY: ListRetryPolicy = ListRetryPolicy {
    max_attempts: 6,
    initial_backoff: Duration::from_secs(1),
    max_backoff: Duration::from_secs(16),
};

/// Why a listing stopped without a page.
#[derive(Debug, PartialEq)]
enum ListFailure {
    /// `is_active` turned false during a backoff.
    Cancelled,
    /// A message for the user.
    Failed(String),
}

/// Sends the page request until it succeeds, the error is one that will not
/// go away, the policy is used up, or `is_active` turns false during a wait.
async fn list_with_retry<T, E, Fut>(
    policy: ListRetryPolicy,
    is_active: impl Fn() -> bool,
    mut send_page: impl FnMut() -> Fut,
) -> Result<T, ListFailure>
where
    E: std::error::Error + ProvideErrorMetadata + 'static,
    Fut: Future<Output = Result<T, SdkError<E, HttpResponse>>>,
{
    let max_attempts = policy.max_attempts.max(1);
    let mut first_failure: Option<String> = None;
    let mut attempt = 0;

    loop {
        attempt += 1;
        let error = match send_page().await {
            Ok(page) => return Ok(page),
            Err(error) => error,
        };
        let description = describe_s3_error(&error);

        if !is_transient_s3_error(&error) {
            // A permanent error can arrive after transient ones. Naming only the
            // last would hide that the provider was already failing, which is
            // the difference between "bad credentials" and "a bad patch".
            let mut message = format!("S3 list failed: {description}");
            if let Some(first) = first_failure.filter(|first| *first != description) {
                message.push_str("; first attempt: ");
                message.push_str(&first);
            }
            return Err(ListFailure::Failed(message));
        }

        if attempt >= max_attempts {
            let mut message = format!("S3 list failed after {attempt} attempts: {description}");
            if let Some(first) = first_failure.filter(|first| *first != description) {
                message.push_str("; first attempt: ");
                message.push_str(&first);
            }
            return Err(ListFailure::Failed(message));
        }

        let backoff = policy.backoff(attempt);
        // eprintln, not log::warn — the app registers no `log` backend, so the
        // macro would discard the one line that explains a slow or failed sync.
        eprintln!(
            "S3 list attempt {attempt} of {max_attempts} failed, retrying in {backoff:?}: {description}"
        );
        first_failure.get_or_insert(description);

        if !sleep_while_active(backoff, &is_active).await {
            return Err(ListFailure::Cancelled);
        }
    }
}

/// Waits out `duration`, unless `is_active` turns false first; says which.
async fn sleep_while_active(duration: Duration, is_active: &impl Fn() -> bool) -> bool {
    /// The longest a cancelled run keeps sleeping.
    const CHECK_INTERVAL: Duration = Duration::from_millis(250);

    let deadline = tokio::time::Instant::now() + duration;
    loop {
        if !is_active() {
            return false;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return true;
        }
        tokio::time::sleep(remaining.min(CHECK_INTERVAL)).await;
    }
}

/// Sends one page request while holding the list lock, so listings do not
/// compete with each other at the provider. The backoff between attempts
/// happens outside it, where another listing can get through.
///
/// The error is the SDK's own, handed straight to the retry loop; boxing it
/// here would only move the cost, so the large-error lint is set aside.
#[allow(clippy::result_large_err)]
async fn send_locked(
    request: ListObjectsV2FluentBuilder,
) -> Result<ListObjectsV2Output, SdkError<ListObjectsV2Error, HttpResponse>> {
    let _list_guard = S3_LIST_LOCK.lock().await;
    request.send().await
}

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundSyncProgress {
    pub objects_fetched: usize,
    pub bytes_fetched: i64,
    pub estimated_total: Option<usize>,
    pub is_running: bool,
    pub speed: f64, // objects/second
}

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundSyncResult {
    pub total_objects: usize,
    pub total_bytes: i64,
    pub cancelled: bool,
    /// Prefixes the provider would not list. The rest of the bucket still
    /// synced; these are named so the cause is visible instead of silent.
    pub skipped_prefixes: Vec<String>,
}

#[tauri::command]
pub async fn start_background_sync(
    input: LazyListInput,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let run_id = BACKGROUND_RUN_ID.fetch_add(1, Ordering::SeqCst) + 1;

    // Reset cancellation flag
    BACKGROUND_CANCEL.store(false, Ordering::SeqCst);

    // Spawn background task -- returns immediately
    tokio::spawn(async move {
        let result = run_background_sync(input, app.clone(), run_id).await;
        let is_active = is_background_run_active(run_id);

        match result {
            Ok(sync_result) if is_active && !sync_result.cancelled => {
                let _ = app.emit("background-sync-complete", sync_result);
            }
            Err(e) if is_active => {
                let _ = app.emit("background-sync-error", e);
            }
            _ => {}
        }
    });

    Ok(())
}

async fn run_background_sync(
    input: LazyListInput,
    app: tauri::AppHandle,
    run_id: u64,
) -> Result<BackgroundSyncResult, String> {
    let _sync_guard = BACKGROUND_SYNC_LOCK.lock().await;

    let bucket = input.bucket.clone();
    let account_id = input.account_id.clone();

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            total_objects: 0,
            total_bytes: 0,
            cancelled: true,
            skipped_prefixes: Vec::new(),
        });
    }

    // Begin sync (staging table)
    db::begin_sync(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to begin sync: {}", e))?;

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            total_objects: 0,
            total_bytes: 0,
            cancelled: true,
            skipped_prefixes: Vec::new(),
        });
    }

    // Create S3 client (provider-aware)
    let client = create_client_for_input(&input).await?;

    // Spawn store task
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<CachedFile>>(8);
    let store_bucket = bucket.clone();
    let store_account_id = account_id.clone();
    let store_handle = tokio::spawn(async move {
        let mut stored_count: usize = 0;
        while let Some(batch) = rx.recv().await {
            if !is_background_run_active(run_id) {
                break;
            }

            let batch_len = batch.len();
            db::store_file_batch(&store_bucket, &store_account_id, &batch)
                .await
                .map_err(|e| format!("Failed to store files: {}", e))?;
            stored_count += batch_len;
        }
        Ok::<usize, String>(stored_count)
    });

    // Fetch loop with progress emission
    let mut fetched_count: usize = 0;
    let mut fetched_bytes: i64 = 0;
    let mut folder_keys: Vec<String> = Vec::new();
    let start_time = std::time::Instant::now();
    let use_delimiter_crawl = input.provider.as_deref() == Some("rustfs");

    let mut pending_prefixes: VecDeque<String> = VecDeque::from([String::new()]);
    let mut seen_prefixes: HashSet<String> = HashSet::from([String::new()]);
    let mut skipped_prefixes: Vec<String> = Vec::new();
    // A delimiter crawl walks thousands of prefixes, and a progress event per
    // page drives a store write and a re-render each time. Emitting every one
    // floods the UI faster than React settles, so they are paced; the final
    // totals ride on `background-sync-complete` regardless.
    let mut last_progress_emit: Option<std::time::Instant> = None;
    const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(150);

    while let Some(current_prefix) = pending_prefixes.pop_front() {
        let mut continuation_token: Option<String> = None;

        loop {
            if !is_background_run_active(run_id) {
                drop(tx);
                return Ok(BackgroundSyncResult {
                    total_objects: fetched_count,
                    total_bytes: fetched_bytes,
                    cancelled: true,
                    skipped_prefixes,
                });
            }

            let create_request = || {
                let mut request = client.list_objects_v2().bucket(&bucket).max_keys(1000);

                if use_delimiter_crawl {
                    request = request.delimiter("/");
                    if !current_prefix.is_empty() {
                        request = request.prefix(&current_prefix);
                    }
                }

                if let Some(token) = &continuation_token {
                    request = request.continuation_token(token);
                }

                request
            };

            let response = match list_with_retry(
                BACKGROUND_LIST_RETRY,
                || is_background_run_active(run_id),
                || send_locked(create_request()),
            )
            .await
            {
                Ok(response) => response,
                Err(ListFailure::Failed(message)) => {
                    // One folder the provider will not list must not cost the
                    // whole bucket: everything already fetched is still worth
                    // caching, and the folder is named in the result rather
                    // than lost in a failed sync. The root is the exception —
                    // without it there is nothing to sync at all. A flat crawl
                    // cannot skip either, because its pages are a continuation
                    // chain, and a gap in that chain silently drops objects.
                    if use_delimiter_crawl && !current_prefix.is_empty() {
                        eprintln!("Skipping unlistable prefix {current_prefix}: {message}");
                        skipped_prefixes.push(current_prefix.clone());
                        break;
                    }
                    return Err(message);
                }
                Err(ListFailure::Cancelled) => {
                    drop(tx);
                    return Ok(BackgroundSyncResult {
                        total_objects: fetched_count,
                        total_bytes: fetched_bytes,
                        cancelled: true,
                        skipped_prefixes,
                    });
                }
            };

            if !is_background_run_active(run_id) {
                drop(tx);
                return Ok(BackgroundSyncResult {
                    total_objects: fetched_count,
                    total_bytes: fetched_bytes,
                    cancelled: true,
                    skipped_prefixes,
                });
            }

            let is_truncated = response.is_truncated().unwrap_or(false);
            let next_token = response.next_continuation_token().map(|s| s.to_string());
            let now = chrono::Utc::now().timestamp();

            let mut batch: Vec<CachedFile> = Vec::new();
            for obj in response.contents() {
                if let Some(key) = obj.key() {
                    let key = key.to_string();
                    if key.ends_with('/') {
                        folder_keys.push(key);
                    } else {
                        let (parent_path, name) = db::parse_key(&key);
                        batch.push(CachedFile {
                            bucket: bucket.clone(),
                            account_id: account_id.clone(),
                            key,
                            parent_path,
                            name,
                            size: obj.size().unwrap_or(0),
                            last_modified: obj
                                .last_modified()
                                .map(|dt| dt.to_string())
                                .unwrap_or_default(),
                            synced_at: now,
                        });
                    }
                }
            }

            if use_delimiter_crawl {
                for cp in response.common_prefixes() {
                    if let Some(prefix) = cp.prefix() {
                        let prefix = prefix.to_string();
                        folder_keys.push(prefix.clone());
                        if seen_prefixes.insert(prefix.clone()) {
                            pending_prefixes.push_back(prefix);
                        }
                    }
                }
            }

            fetched_count += batch.len();
            fetched_bytes += batch.iter().map(|f| f.size).sum::<i64>();

            // Calculate speed
            let elapsed = start_time.elapsed().as_secs_f64().max(0.001);
            let speed = fetched_count as f64 / elapsed;

            // Emit progress at most every PROGRESS_EMIT_INTERVAL (always the
            // first page, so the UI leaves "starting" immediately).
            let due =
                last_progress_emit.is_none_or(|last| last.elapsed() >= PROGRESS_EMIT_INTERVAL);
            if due {
                last_progress_emit = Some(std::time::Instant::now());
                let _ = app.emit(
                    "background-sync-progress",
                    BackgroundSyncProgress {
                        objects_fetched: fetched_count,
                        bytes_fetched: fetched_bytes,
                        estimated_total: {
                            let has_pending_prefixes =
                                use_delimiter_crawl && !pending_prefixes.is_empty();
                            if is_truncated || has_pending_prefixes {
                                None
                            } else {
                                Some(fetched_count)
                            }
                        },
                        is_running: true,
                        speed,
                    },
                );
            }

            if !batch.is_empty() {
                tx.send(batch)
                    .await
                    .map_err(|_| "Store task crashed".to_string())?;
            }

            if !is_truncated {
                break;
            }
            continuation_token = next_token;
        }

        if !use_delimiter_crawl {
            break;
        }
    }

    // Wait for store task
    drop(tx);
    let stored_count = store_handle
        .await
        .map_err(|e| format!("Store task panicked: {}", e))?
        .map_err(|e| format!("Store failed: {}", e))?;

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            total_objects: fetched_count,
            total_bytes: fetched_bytes,
            cancelled: true,
            skipped_prefixes,
        });
    }

    // Finish sync (swap staging -> live)
    db::finish_sync(&bucket, &account_id, stored_count)
        .await
        .map_err(|e| format!("Failed to finish sync: {}", e))?;

    // Written with the swap, not after it: `finish_sync` is what makes the
    // cache authoritative, and any folder missing from it must be known before
    // browsing can trust it.
    store_skipped_prefixes(&bucket, &account_id, &skipped_prefixes).await;

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            total_objects: fetched_count,
            total_bytes: fetched_bytes,
            cancelled: true,
            skipped_prefixes,
        });
    }

    // Build directory tree
    db::build_directory_tree_from_db(&bucket, &account_id, &folder_keys, None::<fn(usize, usize)>)
        .await
        .map_err(|e| format!("Failed to build tree: {}", e))?;

    if !skipped_prefixes.is_empty() {
        eprintln!(
            "Sync finished with {} unlistable prefix(es): {}",
            skipped_prefixes.len(),
            skipped_prefixes.join(", ")
        );
    }

    Ok(BackgroundSyncResult {
        total_objects: fetched_count,
        total_bytes: fetched_bytes,
        cancelled: false,
        skipped_prefixes,
    })
}

#[tauri::command]
pub async fn cancel_background_sync() -> Result<(), String> {
    BACKGROUND_RUN_ID.fetch_add(1, Ordering::SeqCst);
    BACKGROUND_CANCEL.store(true, Ordering::SeqCst);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_s3::error::ErrorMetadata;
    use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
    use aws_sdk_s3::primitives::SdkBody;
    use std::cell::Cell;
    use tokio::time::Instant;

    type ListError = SdkError<ListObjectsV2Error, HttpResponse>;

    fn service_error(status: u16, code: &str, message: &str) -> ListError {
        let inner = ListObjectsV2Error::generic(
            ErrorMetadata::builder().code(code).message(message).build(),
        );
        let raw = HttpResponse::new(status.try_into().unwrap(), SdkBody::empty());
        SdkError::service_error(inner, raw)
    }

    fn unavailable() -> ListError {
        service_error(
            503,
            "ServiceUnavailable",
            "The service is unavailable. Please retry.",
        )
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let waits: Vec<Duration> = (1..=6)
            .map(|failed_attempts| BACKGROUND_LIST_RETRY.backoff(failed_attempts))
            .collect();

        assert_eq!(waits, [1, 2, 4, 8, 16, 16].map(Duration::from_secs));
    }

    // The tests below run on a paused clock: every sleep completes at once, and
    // `Instant::now()` still reports how long the real thing would have waited.

    #[tokio::test(start_paused = true)]
    async fn a_provider_that_recovers_is_waited_out() {
        let calls = Cell::new(0);
        let started = Instant::now();

        let result = list_with_retry(
            BACKGROUND_LIST_RETRY,
            || true,
            || {
                calls.set(calls.get() + 1);
                let outcome = if calls.get() < 3 {
                    Err(unavailable())
                } else {
                    Ok("page")
                };
                async move { outcome }
            },
        )
        .await;

        assert_eq!(result, Ok("page"));
        assert_eq!(calls.get(), 3);
        assert_eq!(started.elapsed(), Duration::from_secs(1 + 2));
    }

    #[tokio::test(start_paused = true)]
    async fn a_mistake_in_the_request_is_not_retried() {
        let calls = Cell::new(0);
        let started = Instant::now();

        let result: Result<(), _> = list_with_retry(
            BACKGROUND_LIST_RETRY,
            || true,
            || {
                calls.set(calls.get() + 1);
                async { Err(service_error(403, "AccessDenied", "Access Denied")) }
            },
        )
        .await;

        assert_eq!(
            result,
            Err(ListFailure::Failed(
                "S3 list failed: AccessDenied: Access Denied".into()
            ))
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn an_outage_that_outlasts_the_policy_is_reported_with_the_attempt_count() {
        let calls = Cell::new(0);
        let started = Instant::now();

        let result: Result<(), _> = list_with_retry(
            BACKGROUND_LIST_RETRY,
            || true,
            || {
                calls.set(calls.get() + 1);
                async { Err(unavailable()) }
            },
        )
        .await;

        assert_eq!(
            result,
            Err(ListFailure::Failed(
                "S3 list failed after 6 attempts: ServiceUnavailable: The service is unavailable. Please retry."
                    .into()
            ))
        );
        assert_eq!(calls.get(), 6);
        assert_eq!(started.elapsed(), Duration::from_secs(1 + 2 + 4 + 8 + 16));
    }

    #[tokio::test(start_paused = true)]
    async fn a_folder_listing_gives_up_after_a_second_and_a_half() {
        let calls = Cell::new(0);
        let started = Instant::now();

        let result: Result<(), _> = list_with_retry(
            FOREGROUND_LIST_RETRY,
            || true,
            || {
                calls.set(calls.get() + 1);
                let error = if calls.get() == 1 {
                    SdkError::timeout_error("connect took too long")
                } else {
                    unavailable()
                };
                async move { Err(error) }
            },
        )
        .await;

        assert_eq!(
            result,
            Err(ListFailure::Failed(
                "S3 list failed after 3 attempts: ServiceUnavailable: The service is unavailable. Please retry.; first attempt: request has timed out: connect took too long"
                    .into()
            ))
        );
        assert_eq!(calls.get(), 3);
        assert_eq!(started.elapsed(), Duration::from_millis(500 + 1000));
    }

    #[test]
    fn a_skipped_folder_and_everything_under_it_bypasses_the_cache() {
        let skipped = vec!["insurance-check/status/".to_string()];

        assert!(is_under_skipped_prefix("insurance-check/status/", &skipped));
        assert!(is_under_skipped_prefix(
            "insurance-check/status/2026/",
            &skipped
        ));
        // The parent listed fine and legitimately knows about the folder.
        assert!(!is_under_skipped_prefix("insurance-check/", &skipped));
        // A sibling sharing the name stem must not be diverted. This holds only
        // because a recorded prefix keeps the trailing slash that
        // `common_prefixes()` returns — do not normalise it away.
        assert!(!is_under_skipped_prefix(
            "insurance-check/status-archive/",
            &skipped
        ));
        assert!(!is_under_skipped_prefix("", &skipped));
        assert!(!is_under_skipped_prefix("documents/", &skipped));
        // A sync that skipped nothing never diverts anything.
        assert!(!is_under_skipped_prefix("insurance-check/status/", &[]));
    }

    #[tokio::test(start_paused = true)]
    async fn a_permanent_error_after_transient_ones_keeps_both() {
        let calls = Cell::new(0);

        let result: Result<(), _> = list_with_retry(
            BACKGROUND_LIST_RETRY,
            || true,
            || {
                calls.set(calls.get() + 1);
                let error = if calls.get() < 3 {
                    unavailable()
                } else {
                    service_error(403, "AccessDenied", "Access Denied")
                };
                async move { Err(error) }
            },
        )
        .await;

        assert_eq!(
            result,
            Err(ListFailure::Failed(
                "S3 list failed: AccessDenied: Access Denied; first attempt: ServiceUnavailable: The service is unavailable. Please retry."
                    .into()
            ))
        );
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_policy_promising_no_attempts_still_makes_one() {
        const NONE: ListRetryPolicy = ListRetryPolicy {
            max_attempts: 0,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(1),
        };
        let calls = Cell::new(0);

        let result: Result<(), _> = list_with_retry(
            NONE,
            || true,
            || {
                calls.set(calls.get() + 1);
                async { Err(unavailable()) }
            },
        )
        .await;

        assert_eq!(calls.get(), 1);
        assert!(matches!(result, Err(ListFailure::Failed(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_run_stops_partway_through_a_backoff() {
        let calls = Cell::new(0);
        let checks = Cell::new(0);
        let started = Instant::now();

        let result: Result<(), _> = list_with_retry(
            BACKGROUND_LIST_RETRY,
            || {
                checks.set(checks.get() + 1);
                checks.get() <= 2
            },
            || {
                calls.set(calls.get() + 1);
                async { Err(unavailable()) }
            },
        )
        .await;

        assert_eq!(result, Err(ListFailure::Cancelled));
        assert_eq!(calls.get(), 1);
        // Two checks passed, so two slices of the one-second backoff went by
        // before the third check saw the cancellation.
        assert_eq!(started.elapsed(), Duration::from_millis(500));
    }
}
