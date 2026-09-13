use crate::db::cache_scope::{self, CacheConfig, CacheScope};
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
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;
use tauri::Emitter;

// ============ Types ============

#[derive(Clone, Deserialize)]
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
    pub request_id: Option<String>,
    pub generation: Option<u64>,
    pub run_id: Option<String>,
}

fn cache_config(input: &LazyListInput) -> CacheConfig {
    let provider = input.provider.clone().unwrap_or_else(|| "r2".into());
    CacheConfig {
        force_path_style: if provider == "r2" {
            true
        } else {
            input.force_path_style.unwrap_or(provider != "aws")
        },
        provider,
        account_id: input.account_id.clone(),
        access_key_id: input.access_key_id.clone(),
        secret_access_key: input.secret_access_key.clone(),
        region: input.region.clone(),
        endpoint_scheme: input.endpoint_scheme.clone(),
        endpoint_host: input.endpoint_host.clone(),
    }
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
pub struct ListScope {
    pub provider: String,
    pub account_id: String,
    pub bucket: String,
    pub prefix: String,
    pub request_id: String,
    pub generation: u64,
}

impl ListScope {
    fn new(input: &LazyListInput) -> Self {
        Self {
            provider: input.provider.clone().unwrap_or_else(|| "r2".into()),
            account_id: input.account_id.clone(),
            bucket: input.bucket.clone(),
            prefix: input.prefix.clone(),
            request_id: input.request_id.clone().unwrap_or_else(|| {
                format!(
                    "list-{}",
                    FOREGROUND_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
                )
            }),
            generation: input.generation.unwrap_or(0),
        }
    }
}

/// Native timing snapshots are cumulative, not per-page deltas. Queue,
/// network, backoff and DB intervals belong to the shared prefix flight;
/// cache and native elapsed intervals belong to the invoking consumer. A
/// joining consumer can therefore observe shared work predating its request.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ListTiming {
    pub queue_ms: f64,
    pub network_ms: f64,
    pub backoff_ms: f64,
    pub db_ms: f64,
    pub cache_ms: f64,
    pub native_elapsed_ms: f64,
    pub shared_flight: bool,
    /// Monotonic flight-start-to-publication time; absent for cache pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_ready_ms: Option<f64>,
    /// Wall clock immediately before native emission, not a transport duration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emit_started_unix_ms: Option<f64>,
    /// Reply only: synchronous native serialization/enqueue time. This excludes
    /// IPC transport, webview event handling, sorting, React and rendering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emit_ms: Option<f64>,
}

#[derive(Default)]
struct ListMeasurements {
    queue_ns: AtomicU64,
    network_ns: AtomicU64,
    backoff_ns: AtomicU64,
    db_ns: AtomicU64,
}
impl ListMeasurements {
    fn snapshot(&self) -> ListTiming {
        let ms = |value: &AtomicU64| value.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        ListTiming {
            queue_ms: ms(&self.queue_ns),
            network_ms: ms(&self.network_ns),
            backoff_ms: ms(&self.backoff_ns),
            db_ms: ms(&self.db_ns),
            ..ListTiming::default()
        }
    }
}

/// Records elapsed work even when cancellation drops a permit wait, request,
/// backoff or DB future. It neither polls nor changes that future's lifetime.
struct MeasureInterval<'a> {
    counter: &'a AtomicU64,
    started: tokio::time::Instant,
}
impl<'a> MeasureInterval<'a> {
    fn new(counter: &'a AtomicU64) -> Self {
        Self {
            counter,
            started: tokio::time::Instant::now(),
        }
    }
}
impl Drop for MeasureInterval<'_> {
    fn drop(&mut self) {
        let nanos = self.started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.counter.fetch_add(nanos, Ordering::Relaxed);
    }
}

fn elapsed_ms(started: tokio::time::Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

#[derive(Debug, Clone, Serialize)]
pub struct LazyListResult {
    pub timing: ListTiming,
    #[serde(flatten)]
    pub scope: ListScope,
    pub files: Vec<LazyFileItem>,
    pub folders: Vec<String>,
    pub complete: bool,
    pub from_cache: bool,
    pub freshness: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct LazyFileItem {
    pub key: String,
    pub name: String,
    pub size: i64,
    pub last_modified: String,
}

impl From<&CachedFile> for LazyFileItem {
    fn from(file: &CachedFile) -> Self {
        Self {
            key: file.key.clone(),
            name: file.name.clone(),
            size: file.size,
            last_modified: file.last_modified.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderPage {
    #[serde(flatten)]
    pub scope: ListScope,
    #[serde(flatten)]
    pub page: ListPage,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListPage {
    pub timing: ListTiming,
    pub files: Vec<LazyFileItem>,
    pub folders: Vec<String>,
    pub page_index: usize,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub from_cache: bool,
    pub freshness: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderLoadSummary {
    pub timing: ListTiming,
    #[serde(flatten)]
    pub scope: ListScope,
    pub complete: bool,
    pub from_cache: bool,
    pub freshness: &'static str,
    pub total_items: usize,
}

const DIRECTORY_TTL_SECS: i64 = 60;
static FOREGROUND_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static FOREGROUND_CANCEL: LazyLock<Mutex<HashMap<String, Weak<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A known complete index can be stale. Its timestamp never makes a directory
/// freshly listed forever; prefix freshness and index completeness are distinct.
#[tauri::command]
pub async fn get_prefix_cache(input: LazyListInput) -> Result<Option<LazyListResult>, String> {
    read_prefix_cache(&input, ListScope::new(&input)).await
}

async fn read_prefix_cache(
    input: &LazyListInput,
    scope: ListScope,
) -> Result<Option<LazyListResult>, String> {
    let started = tokio::time::Instant::now();
    let cache_scope = CacheScope::capture(&cache_config(input))
        .await
        .map_err(|e| e.to_string())?;
    let mut result = read_prefix_cache_scoped(input, scope, &cache_scope).await?;
    if let Some(result) = &mut result {
        result.timing.cache_ms = elapsed_ms(started);
        result.timing.native_elapsed_ms = result.timing.cache_ms;
    }
    Ok(result)
}

async fn read_prefix_cache_scoped(
    input: &LazyListInput,
    scope: ListScope,
    cache_scope: &CacheScope,
) -> Result<Option<LazyListResult>, String> {
    let started = tokio::time::Instant::now();
    let snapshot = cache_scope::read_prefix_snapshot(cache_scope, &input.bucket, &input.prefix)
        .await
        .map_err(|e| format!("DB error: {e}"))?;
    let prefix_time = snapshot.prefix_time;
    let complete_index = snapshot.full_sync
        && snapshot
            .skipped_prefixes
            .as_ref()
            .is_some_and(|skipped| !is_under_skipped_prefix(&input.prefix, skipped));
    let contents = snapshot.contents;
    let complete = prefix_time.is_some() || complete_index;
    if !complete && contents.files.is_empty() && contents.folders.is_empty() {
        return Ok(None);
    }
    let fresh = prefix_time.is_some_and(|time| {
        let age = chrono::Utc::now().timestamp() - time;
        (0..DIRECTORY_TTL_SECS).contains(&age)
    });
    let mut result = LazyListResult {
        timing: ListTiming::default(),
        scope,
        files: contents.files.iter().map(LazyFileItem::from).collect(),
        folders: contents.folders,
        complete,
        from_cache: true,
        freshness: if fresh {
            "fresh"
        } else if complete {
            "stale"
        } else {
            "partial"
        },
    };
    result.timing.cache_ms = elapsed_ms(started);
    result.timing.native_elapsed_ms = result.timing.cache_ms;
    result.timing.emit_ms = Some(0.0);
    Ok(Some(result))
}

struct RequestCancellation {
    request_id: String,
    cancelled: Arc<AtomicBool>,
}
impl RequestCancellation {
    fn register(request_id: &str) -> Result<Self, String> {
        let mut requests = FOREGROUND_CANCEL.lock().unwrap_or_else(|e| e.into_inner());
        requests.retain(|_, value| value.strong_count() > 0);
        if requests.get(request_id).and_then(Weak::upgrade).is_some() {
            return Err("Folder request_id is already active".into());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        requests.insert(request_id.into(), Arc::downgrade(&cancelled));
        Ok(Self {
            request_id: request_id.into(),
            cancelled,
        })
    }
    fn active(&self) -> bool {
        !self.cancelled.load(Ordering::SeqCst)
    }
}
impl Drop for RequestCancellation {
    fn drop(&mut self) {
        FOREGROUND_CANCEL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.request_id);
    }
}

#[tauri::command]
pub async fn cancel_prefix_list(request_id: String) -> Result<(), String> {
    if let Some(cancelled) = FOREGROUND_CANCEL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&request_id)
        .and_then(Weak::upgrade)
    {
        cancelled.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[derive(Default)]
struct FlightState {
    pages: Vec<Arc<ListPage>>,
    result: Option<Result<Arc<LazyListResult>, String>>,
}
struct PrefixFlight {
    started: tokio::time::Instant,
    measurements: ListMeasurements,
    state: Mutex<FlightState>,
    changed: tokio::sync::watch::Sender<u64>,
    consumers: AtomicUsize,
}
impl PrefixFlight {
    fn publish(&self, mut page: ListPage) {
        page.timing = self.measurements.snapshot();
        page.timing.page_ready_ms = Some(elapsed_ms(self.started));
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pages
            .push(Arc::new(page));
        self.changed.send_modify(|version| *version += 1);
    }
    fn active(&self) -> bool {
        self.consumers.load(Ordering::SeqCst) > 0
    }
}
struct FlightLease(Arc<PrefixFlight>, bool);
impl Drop for FlightLease {
    fn drop(&mut self) {
        self.0.consumers.fetch_sub(1, Ordering::SeqCst);
    }
}
static PREFIX_FLIGHTS: LazyLock<Mutex<HashMap<String, Weak<PrefixFlight>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn endpoint_scope(input: &LazyListInput) -> String {
    let provider = input.provider.as_deref().unwrap_or("r2");
    let host = match provider {
        "minio" | "rustfs" => input.endpoint_host.clone().unwrap_or_default(),
        "aws" => input.endpoint_host.clone().unwrap_or_else(|| {
            format!(
                "s3.{}.amazonaws.com",
                input.region.as_deref().unwrap_or("us-east-1")
            )
        }),
        _ => format!("{}.r2.cloudflarestorage.com", input.account_id),
    };
    let scheme =
        input
            .endpoint_scheme
            .as_deref()
            .unwrap_or(if matches!(provider, "minio" | "rustfs") {
                "http"
            } else {
                "https"
            });
    format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.trim_end_matches('/').to_ascii_lowercase()
    )
}

fn prefix_flight_key(input: &LazyListInput) -> String {
    // Credentials participate in equality without becoming a plaintext map key.
    let mut hash = Sha256::new();
    for field in [
        endpoint_scope(input),
        input.provider.clone().unwrap_or_else(|| "r2".into()),
        input.account_id.clone(),
        input.bucket.clone(),
        input.prefix.clone(),
        input.region.clone().unwrap_or_default(),
        input.access_key_id.clone(),
        input.secret_access_key.clone(),
        input.force_path_style.unwrap_or(false).to_string(),
    ] {
        hash.update((field.len() as u64).to_le_bytes());
        hash.update(field.as_bytes());
    }
    hex::encode(hash.finalize())
}

fn join_prefix_flight(input: LazyListInput, cache_scope: CacheScope) -> FlightLease {
    // An account edited away and back must not join the obsolete revision's
    // in-flight result even when its credentials happen to match again.
    let key = format!("{}:{}", prefix_flight_key(&input), cache_scope.revision);
    join_prefix_flight_with(input, key, move |input, owner| async move {
        cache_scope::in_scope(cache_scope, fetch_prefix(input, &owner)).await
    })
}

fn join_prefix_flight_with<F, Fut>(input: LazyListInput, key: String, fetch: F) -> FlightLease
where
    F: FnOnce(LazyListInput, Arc<PrefixFlight>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<LazyListResult, String>> + Send + 'static,
{
    let mut flights = PREFIX_FLIGHTS.lock().unwrap_or_else(|e| e.into_inner());
    flights.retain(|_, weak| weak.strong_count() > 0);
    if let Some(flight) = flights.get(&key).and_then(Weak::upgrade).filter(|flight| {
        flight
            .consumers
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                if count == 0 {
                    None
                } else {
                    count.checked_add(1)
                }
            })
            .is_ok()
    }) {
        return FlightLease(flight, true);
    }
    let (changed, _) = tokio::sync::watch::channel(0);
    let flight = Arc::new(PrefixFlight {
        started: tokio::time::Instant::now(),
        measurements: ListMeasurements::default(),
        state: Mutex::new(FlightState::default()),
        changed,
        consumers: AtomicUsize::new(1),
    });
    flights.insert(key, Arc::downgrade(&flight));
    let owner = flight.clone();
    tokio::spawn(async move {
        let result = fetch(input, owner.clone()).await.map(Arc::new);
        owner.state.lock().unwrap_or_else(|e| e.into_inner()).result = Some(result);
        owner.changed.send_modify(|version| *version += 1);
    });
    FlightLease(flight, false)
}

async fn list_prefix_internal(
    input: LazyListInput,
    app: tauri::AppHandle,
    emit_pages: bool,
) -> Result<Arc<LazyListResult>, String> {
    let started = tokio::time::Instant::now();
    let scope = ListScope::new(&input);
    let cancellation = RequestCancellation::register(&scope.request_id)?;
    let cache_started = tokio::time::Instant::now();
    let cache_scope = CacheScope::capture(&cache_config(&input))
        .await
        .map_err(|e| e.to_string())?;
    let mut cache_ms = elapsed_ms(cache_started);
    if !input.force_refresh.unwrap_or(false) {
        let cache = read_prefix_cache_scoped(&input, scope.clone(), &cache_scope).await?;
        cache_ms = elapsed_ms(cache_started);
        if let Some(mut cache) = cache.filter(|cache| cache.freshness == "fresh") {
            if !cancellation.active() {
                return Err("S3 list cancelled".into());
            }
            cache.timing.cache_ms = cache_ms;
            cache.timing.emit_ms = Some(if emit_pages {
                emit_cached_pages(&app, &cache, started)?
            } else {
                0.0
            });
            cache.timing.native_elapsed_ms = elapsed_ms(started);
            return Ok(Arc::new(cache));
        }
    }
    if !cancellation.active() {
        return Err("S3 list cancelled".into());
    }
    let lease = join_prefix_flight(input, cache_scope);
    let mut changed = lease.0.changed.subscribe();
    let mut delivered = 0;
    let mut emit_ms = 0.0;
    loop {
        if !cancellation.active() {
            return Err("S3 list cancelled".into());
        }
        let (pages, result) = {
            let state = lease.0.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.pages[delivered..].to_vec(), state.result.clone())
        };
        for page in pages {
            if emit_pages {
                let mut page = (*page).clone();
                page.timing.cache_ms = cache_ms;
                page.timing.shared_flight = lease.1;
                emit_ms += emit_folder_page(
                    &app,
                    FolderPage {
                        scope: scope.clone(),
                        page,
                    },
                    started,
                )?;
            }
            delivered += 1;
        }
        if let Some(result) = result {
            // Shared work retains its measurements; each consumer gets its own
            // scope, cache interval and native delivery elapsed time.
            return result.map(|result| {
                let mut result = LazyListResult {
                    scope,
                    ..(*result).clone()
                };
                result.timing.cache_ms = cache_ms;
                result.timing.native_elapsed_ms = elapsed_ms(started);
                result.timing.shared_flight = lease.1;
                result.timing.emit_ms = Some(emit_ms);
                Arc::new(result)
            });
        }
        tokio::select! {
            _ = changed.changed() => {},
            _ = tokio::time::sleep(Duration::from_millis(50)) => {},
        }
    }
}

fn emit_folder_page(
    app: &tauri::AppHandle,
    mut event: FolderPage,
    consumer_started: tokio::time::Instant,
) -> Result<f64, String> {
    event.page.timing.native_elapsed_ms = elapsed_ms(consumer_started);
    event.page.timing.emit_started_unix_ms =
        Some(chrono::Utc::now().timestamp_micros() as f64 / 1000.0);
    // A page cannot include the cost of its own emission. Only the completed
    // calls are accumulated in the final reply's emit_ms field.
    event.page.timing.emit_ms = None;
    let started = tokio::time::Instant::now();
    app.emit("folder-page", event)
        .map_err(|e| format!("Failed to deliver folder page: {e}"))?;
    Ok(elapsed_ms(started))
}

fn emit_cached_pages(
    app: &tauri::AppHandle,
    cache: &LazyListResult,
    consumer_started: tokio::time::Instant,
) -> Result<f64, String> {
    let total = cache.files.len() + cache.folders.len();
    let page_count = total.max(1).div_ceil(1000);
    let mut emit_ms = 0.0;
    for index in 0..page_count {
        let start = index * 1000;
        let end = ((index + 1) * 1000).min(total);
        let folders_start = start.min(cache.folders.len());
        let folders_end = end.min(cache.folders.len());
        let files_start = start.saturating_sub(cache.folders.len());
        let files_end = end.saturating_sub(cache.folders.len());
        let complete = index + 1 == page_count;
        emit_ms += emit_folder_page(
            app,
            FolderPage {
                scope: cache.scope.clone(),
                page: ListPage {
                    timing: cache.timing.clone(),
                    files: cache.files[files_start..files_end].to_vec(),
                    folders: cache.folders[folders_start..folders_end].to_vec(),
                    page_index: index,
                    next_cursor: (!complete).then(|| format!("cache:{}", index + 1)),
                    complete,
                    from_cache: true,
                    freshness: cache.freshness,
                },
            },
            consumer_started,
        )?;
    }
    Ok(emit_ms)
}

/// Compatibility command for callers requiring a complete aggregate.
#[tauri::command]
pub async fn list_prefix(
    input: LazyListInput,
    app: tauri::AppHandle,
) -> Result<LazyListResult, String> {
    list_prefix_internal(input, app, false)
        .await
        .map(|result| (*result).clone())
}

/// Pages arrive while S3 is still listing. The completion reply stays small.
#[tauri::command]
pub async fn list_prefix_stream(
    input: LazyListInput,
    app: tauri::AppHandle,
) -> Result<FolderLoadSummary, String> {
    let result = list_prefix_internal(input, app, true).await?;
    Ok(FolderLoadSummary {
        timing: result.timing.clone(),
        scope: result.scope.clone(),
        complete: result.complete,
        from_cache: result.from_cache,
        freshness: result.freshness,
        total_items: result.files.len() + result.folders.len(),
    })
}

/// A truncated response must advance the cursor. Never loop back to page one,
/// accept a repeated page as complete, or infer removals from a partial chain.
fn next_page_cursor(
    response: &ListObjectsV2Output,
    seen: &mut HashSet<String>,
) -> Result<Option<String>, String> {
    if !response.is_truncated().unwrap_or(false) {
        return Ok(None);
    }
    let token = response
        .next_continuation_token()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            "S3 returned a truncated listing without a continuation token".to_string()
        })?;
    if !seen.insert(token.to_string()) {
        return Err("S3 returned a repeated continuation token".into());
    }
    Ok(Some(token.to_string()))
}

async fn fetch_prefix(
    input: LazyListInput,
    flight: &PrefixFlight,
) -> Result<LazyListResult, String> {
    let client = create_client_for_input(&input).await?;
    let scheduler = endpoint_scheduler(&endpoint_scope(&input));
    let now = chrono::Utc::now().timestamp();
    let mut files = Vec::new();
    let mut folders = Vec::new();
    let mut continuation_token: Option<String> = None;
    let mut seen_tokens = HashSet::new();
    let mut seen_files = HashSet::new();
    let mut seen_folders = HashSet::new();
    let mut page_index = 0;
    loop {
        let response = list_with_retry_measured(
            FOREGROUND_LIST_RETRY,
            Some(&flight.measurements),
            || flight.active(),
            || {
                let request = client
                    .list_objects_v2()
                    .bucket(&input.bucket)
                    .delimiter("/")
                    .max_keys(1000)
                    .set_prefix((!input.prefix.is_empty()).then(|| input.prefix.clone()))
                    .set_continuation_token(continuation_token.clone());
                send_scheduled_measured(request, &scheduler, false, Some(&flight.measurements))
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        let next_cursor = next_page_cursor(&response, &mut seen_tokens)?;
        let mut page = ListPage {
            timing: ListTiming::default(),
            files: Vec::new(),
            folders: Vec::new(),
            page_index,
            complete: next_cursor.is_none(),
            next_cursor: next_cursor.clone(),
            from_cache: false,
            freshness: "fresh",
        };
        for object in response.contents() {
            if let Some(key) = object.key().filter(|key| !key.ends_with('/')) {
                let (parent_path, name) = db::parse_key(key);
                if parent_path != input.prefix {
                    return Err("S3 returned an object outside the requested directory".into());
                }
                if !seen_files.insert(key.to_string()) {
                    continue;
                }
                let file = CachedFile {
                    bucket: input.bucket.clone(),
                    account_id: input.account_id.clone(),
                    key: key.into(),
                    parent_path,
                    name,
                    size: object.size().unwrap_or(0),
                    last_modified: object
                        .last_modified()
                        .map(|date| date.to_string())
                        .unwrap_or_default(),
                    synced_at: now,
                };
                page.files.push(LazyFileItem::from(&file));
                files.push(file);
            }
        }
        for prefix in response.common_prefixes() {
            if let Some(prefix) = prefix.prefix() {
                let suffix = prefix.strip_prefix(&input.prefix).unwrap_or("");
                if suffix.is_empty()
                    || !suffix.ends_with('/')
                    || suffix[..suffix.len() - 1].contains('/')
                {
                    return Err("S3 returned a prefix outside the requested directory".into());
                }
                if seen_folders.insert(prefix.to_string()) {
                    page.folders.push(prefix.into());
                    folders.push(prefix.into());
                }
            }
        }
        if !flight.active() {
            return Err("S3 list cancelled".into());
        }
        if page.complete {
            let _measure = MeasureInterval::new(&flight.measurements.db_ns);
            db::prefix_sync::replace_complete_prefix(
                &input.bucket,
                &input.account_id,
                &input.prefix,
                &files,
                &folders,
            )
            .await
            .map_err(|e| format!("Failed to cache complete listing: {e}"))?;
        }
        flight.publish(page);
        if next_cursor.is_none() {
            break;
        }
        continuation_token = next_cursor;
        page_index += 1;
    }
    Ok(LazyListResult {
        timing: flight.measurements.snapshot(),
        scope: ListScope::new(&input),
        files: files.iter().map(LazyFileItem::from).collect(),
        folders,
        complete: true,
        from_cache: false,
        freshness: "fresh",
    })
}

// ============ Background Sync (Task 3) ============

// Global cancellation token for background sync (one per app)
static BACKGROUND_CANCEL: LazyLock<Arc<AtomicBool>> =
    LazyLock::new(|| Arc::new(AtomicBool::new(false)));

static BACKGROUND_RUN_ID: AtomicU64 = AtomicU64::new(0);
static BACKGROUND_SCOPE: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));
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
/// Attempts use full jitter within an exponentially increasing cap. A shared
/// 30-second deadline includes queueing, SDK attempts, and Retry-After waits;
/// shared client configuration owns the SDK wire-attempt limit.
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

/// Foreground requests use at most three attempts with 0.5s and 1s jitter caps.
const FOREGROUND_LIST_RETRY: ListRetryPolicy = ListRetryPolicy {
    max_attempts: 3,
    initial_backoff: Duration::from_millis(500),
    max_backoff: Duration::from_secs(1),
};

/// Background requests may retry six times within the same total deadline.
const BACKGROUND_LIST_RETRY: ListRetryPolicy = ListRetryPolicy {
    max_attempts: 6,
    initial_backoff: Duration::from_secs(1),
    max_backoff: Duration::from_secs(16),
};

/// Why a listing stopped without a page.
#[derive(Debug, PartialEq)]
enum ListFailure {
    /// The consumer cancelled during queueing, network I/O, or backoff.
    Cancelled,
    /// A message for the user.
    Failed(String),
}

impl std::fmt::Display for ListFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("S3 list cancelled"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

/// LIST is read-only: dropping an in-flight attempt cannot commit a mutation.
/// The deadline covers the queue and the SDK request, including SDK retries.
async fn while_active<T>(
    future: impl Future<Output = T>,
    is_active: &impl Fn() -> bool,
    deadline: tokio::time::Instant,
) -> Result<T, ListFailure> {
    tokio::pin!(future);
    loop {
        if !is_active() {
            return Err(ListFailure::Cancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ListFailure::Failed(
                "S3 list exceeded its 30 second request budget".into(),
            ));
        }
        tokio::select! {
            biased;
            result = &mut future => return Ok(result),
            _ = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + Duration::from_millis(50))) => {},
        }
    }
}

fn jittered_backoff(cap: Duration) -> Duration {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0x9e3779b97f4a7c15);
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let mut sample = SEQUENCE.fetch_add(0x9e3779b97f4a7c15, Ordering::Relaxed) ^ clock;
    sample ^= sample >> 12;
    sample ^= sample << 25;
    sample ^= sample >> 27;
    Duration::from_nanos(
        sample.wrapping_mul(0x2545f4914f6cdd1d)
            % (cap.as_nanos().min(u64::MAX as u128 - 1) as u64 + 1),
    )
}

fn retry_after<E>(error: &SdkError<E, HttpResponse>) -> Option<Duration> {
    let header = error.raw_response()?.headers().get("retry-after")?;
    if let Ok(seconds) = header.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = chrono::DateTime::parse_from_rfc2822(header).ok()?;
    (date.with_timezone(&chrono::Utc) - chrono::Utc::now())
        .to_std()
        .ok()
}

/// Sends the page request until it succeeds, the error is one that will not
/// go away, the policy is used up, or `is_active` turns false during a wait.
async fn list_with_retry<T, E, Fut>(
    policy: ListRetryPolicy,
    is_active: impl Fn() -> bool,
    send_page: impl FnMut() -> Fut,
) -> Result<T, ListFailure>
where
    E: std::error::Error + ProvideErrorMetadata + 'static,
    Fut: Future<Output = Result<T, SdkError<E, HttpResponse>>>,
{
    list_with_retry_measured(policy, None, is_active, send_page).await
}

async fn list_with_retry_measured<T, E, Fut>(
    policy: ListRetryPolicy,
    measurements: Option<&ListMeasurements>,
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    loop {
        if !is_active() {
            return Err(ListFailure::Cancelled);
        }
        attempt += 1;
        let error = match while_active(send_page(), &is_active, deadline).await? {
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

        let backoff =
            jittered_backoff(policy.backoff(attempt)).max(retry_after(&error).unwrap_or_default());
        // eprintln, not log::warn — the app registers no `log` backend, so the
        // macro would discard the one line that explains a slow or failed sync.
        eprintln!(
            "S3 list attempt {attempt} of {max_attempts} failed, retrying in {backoff:?}: {description}"
        );
        first_failure.get_or_insert(description);

        let _measure =
            measurements.map(|measurements| MeasureInterval::new(&measurements.backoff_ns));
        while_active(tokio::time::sleep(backoff), &is_active, deadline).await?;
    }
}

const ENDPOINT_LIST_CAPACITY: usize = 4;
const BACKGROUND_LIST_CAPACITY: usize = ENDPOINT_LIST_CAPACITY - 1;
struct EndpointScheduler {
    total: tokio::sync::Semaphore,
    background: tokio::sync::Semaphore,
}
static ENDPOINT_SCHEDULERS: LazyLock<Mutex<HashMap<String, Weak<EndpointScheduler>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn endpoint_scheduler(scope: &str) -> Arc<EndpointScheduler> {
    let mut schedulers = ENDPOINT_SCHEDULERS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    schedulers.retain(|_, weak| weak.strong_count() > 0);
    if let Some(scheduler) = schedulers.get(scope).and_then(Weak::upgrade) {
        return scheduler;
    }
    let scheduler = Arc::new(EndpointScheduler {
        total: tokio::sync::Semaphore::new(ENDPOINT_LIST_CAPACITY),
        background: tokio::sync::Semaphore::new(BACKGROUND_LIST_CAPACITY),
    });
    schedulers.insert(scope.into(), Arc::downgrade(&scheduler));
    scheduler
}

/// Background work first takes its own quota, leaving one total permit for
/// foreground consumers. The enclosing retry future cancels both permit waits
/// and the network request; permits are always released before retry backoff.
#[allow(clippy::result_large_err)]
async fn send_scheduled(
    request: ListObjectsV2FluentBuilder,
    scheduler: &EndpointScheduler,
    background: bool,
) -> Result<ListObjectsV2Output, SdkError<ListObjectsV2Error, HttpResponse>> {
    send_scheduled_measured(request, scheduler, background, None).await
}

#[allow(clippy::result_large_err)]
async fn send_scheduled_measured(
    request: ListObjectsV2FluentBuilder,
    scheduler: &EndpointScheduler,
    background: bool,
    measurements: Option<&ListMeasurements>,
) -> Result<ListObjectsV2Output, SdkError<ListObjectsV2Error, HttpResponse>> {
    let queue_measure =
        measurements.map(|measurements| MeasureInterval::new(&measurements.queue_ns));
    let _background = if background {
        Some(
            scheduler
                .background
                .acquire()
                .await
                .expect("private semaphore is never closed"),
        )
    } else {
        None
    };
    let _total = scheduler
        .total
        .acquire()
        .await
        .expect("private semaphore is never closed");
    drop(queue_measure);
    let _network_measure =
        measurements.map(|measurements| MeasureInterval::new(&measurements.network_ns));
    request.send().await
}

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundScope {
    pub provider: String,
    pub account_id: String,
    pub bucket: String,
    pub prefix: String,
    pub run_id: String,
}
impl BackgroundScope {
    fn new(input: &LazyListInput) -> Self {
        Self {
            provider: input.provider.clone().unwrap_or_else(|| "r2".into()),
            account_id: input.account_id.clone(),
            bucket: input.bucket.clone(),
            prefix: input.prefix.clone(),
            run_id: input.run_id.clone().unwrap_or_default(),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct BackgroundSyncError {
    #[serde(flatten)]
    pub scope: BackgroundScope,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundSyncProgress {
    #[serde(flatten)]
    pub scope: BackgroundScope,
    pub objects_fetched: usize,
    pub bytes_fetched: i64,
    pub estimated_total: Option<usize>,
    pub is_running: bool,
    pub speed: f64, // objects/second
}

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundSyncResult {
    #[serde(flatten)]
    pub scope: BackgroundScope,
    pub total_objects: usize,
    pub total_bytes: i64,
    pub cancelled: bool,
    /// Prefixes the provider would not list. The rest of the bucket still
    /// synced; these are named so the cause is visible instead of silent.
    pub skipped_prefixes: Vec<String>,
}

#[tauri::command]
pub async fn start_background_sync(
    mut input: LazyListInput,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let cache_scope = CacheScope::capture(&cache_config(&input))
        .await
        .map_err(|e| e.to_string())?;
    let (run_id, public_run_id) = {
        let mut current = BACKGROUND_SCOPE.lock().unwrap_or_else(|e| e.into_inner());
        let run_id = BACKGROUND_RUN_ID.fetch_add(1, Ordering::SeqCst) + 1;
        let public_run_id = input
            .run_id
            .clone()
            .unwrap_or_else(|| format!("sync-{run_id}"));
        input.run_id = Some(public_run_id.clone());
        *current = Some(public_run_id.clone());
        BACKGROUND_CANCEL.store(false, Ordering::SeqCst);
        (run_id, public_run_id)
    };
    tokio::spawn(async move {
        let scope = BackgroundScope::new(&input);
        let result =
            cache_scope::in_scope(cache_scope, run_background_sync(input, app.clone(), run_id))
                .await;
        let active = is_background_run_active(run_id);
        match result {
            Ok(sync_result) if active && !sync_result.cancelled => {
                let _ = app.emit("background-sync-complete", sync_result);
            }
            Err(error) if active => {
                let _ = app.emit(
                    "background-sync-error",
                    BackgroundSyncError { scope, error },
                );
            }
            _ => {
                let _ = app.emit("background-sync-cancelled", scope);
            }
        }
    });
    Ok(public_run_id)
}

async fn run_background_sync(
    input: LazyListInput,
    app: tauri::AppHandle,
    run_id: u64,
) -> Result<BackgroundSyncResult, String> {
    let scope = BackgroundScope::new(&input);
    let _sync_guard = while_active(
        BACKGROUND_SYNC_LOCK.lock(),
        &|| is_background_run_active(run_id),
        tokio::time::Instant::now() + Duration::from_secs(30),
    )
    .await
    .map_err(|e| e.to_string())?;

    let bucket = input.bucket.clone();
    let account_id = input.account_id.clone();

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            scope: scope.clone(),
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
            scope: scope.clone(),
            total_objects: 0,
            total_bytes: 0,
            cancelled: true,
            skipped_prefixes: Vec::new(),
        });
    }

    // Create S3 client (provider-aware)
    let client = create_client_for_input(&input).await?;
    let scheduler = endpoint_scheduler(&endpoint_scope(&input));

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
        let mut seen_tokens = HashSet::new();

        loop {
            if !is_background_run_active(run_id) {
                return Ok(BackgroundSyncResult {
                    scope: scope.clone(),
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
                || send_scheduled(create_request(), &scheduler, true),
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
                    return Ok(BackgroundSyncResult {
                        scope: scope.clone(),
                        total_objects: fetched_count,
                        total_bytes: fetched_bytes,
                        cancelled: true,
                        skipped_prefixes,
                    });
                }
            };

            if !is_background_run_active(run_id) {
                return Ok(BackgroundSyncResult {
                    scope: scope.clone(),
                    total_objects: fetched_count,
                    total_bytes: fetched_bytes,
                    cancelled: true,
                    skipped_prefixes,
                });
            }

            let is_truncated = response.is_truncated().unwrap_or(false);
            let next_token = next_page_cursor(&response, &mut seen_tokens)?;
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
                        scope: scope.clone(),
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
                db::store_file_batch(&bucket, &account_id, &batch)
                    .await
                    .map_err(|e| format!("Failed to store files: {e}"))?;
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

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            scope: scope.clone(),
            total_objects: fetched_count,
            total_bytes: fetched_bytes,
            cancelled: true,
            skipped_prefixes,
        });
    }

    // Finish sync (swap staging -> live)
    db::finish_sync(&bucket, &account_id, fetched_count)
        .await
        .map_err(|e| format!("Failed to finish sync: {}", e))?;

    // Written with the swap, not after it: `finish_sync` is what makes the
    // cache authoritative, and any folder missing from it must be known before
    // browsing can trust it.
    // The swapped index must not inherit freshness from an older delimiter listing.
    db::prefix_sync::clear_prefix_sync_times(&bucket, &account_id)
        .await
        .map_err(|e| format!("Failed to invalidate directory freshness: {e}"))?;
    store_skipped_prefixes(&bucket, &account_id, &skipped_prefixes).await;

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            scope: scope.clone(),
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
        scope,
        total_objects: fetched_count,
        total_bytes: fetched_bytes,
        cancelled: false,
        skipped_prefixes,
    })
}

#[tauri::command]
pub async fn cancel_background_sync(run_id: Option<String>) -> Result<(), String> {
    let current = BACKGROUND_SCOPE.lock().unwrap_or_else(|e| e.into_inner());
    if run_id
        .as_ref()
        .is_some_and(|run_id| current.as_ref() != Some(run_id))
    {
        return Ok(());
    }
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
        assert!(started.elapsed() <= Duration::from_secs(1 + 2));
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
        assert!(started.elapsed() <= Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn a_folder_listing_caps_its_jittered_backoff_at_a_second_and_a_half() {
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
        assert!(started.elapsed() <= Duration::from_millis(500 + 1000));
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
        let active = Arc::new(AtomicBool::new(true));
        let cancel = active.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.store(false, Ordering::SeqCst);
        });
        let started = Instant::now();
        let result: Result<(), _> = list_with_retry(
            BACKGROUND_LIST_RETRY,
            || active.load(Ordering::SeqCst),
            || {
                calls.set(calls.get() + 1);
                let inner =
                    ListObjectsV2Error::generic(ErrorMetadata::builder().code("SlowDown").build());
                let mut raw = HttpResponse::new(503.try_into().unwrap(), SdkBody::empty());
                raw.headers_mut().insert("retry-after", "10");
                async move { Err(SdkError::service_error(inner, raw)) }
            },
        )
        .await;
        assert_eq!(result, Err(ListFailure::Cancelled));
        assert_eq!(calls.get(), 1);
        assert!(started.elapsed() <= Duration::from_millis(150));
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_drops_a_pending_network_future() {
        let active = Arc::new(AtomicBool::new(true));
        let cancel = active.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.store(false, Ordering::SeqCst);
        });
        let started = Instant::now();
        let result = list_with_retry(
            FOREGROUND_LIST_RETRY,
            || active.load(Ordering::SeqCst),
            std::future::pending::<Result<(), ListError>>,
        )
        .await;
        assert_eq!(result, Err(ListFailure::Cancelled));
        assert!(started.elapsed() <= Duration::from_millis(150));
    }

    #[tokio::test(start_paused = true)]
    async fn queue_and_network_share_the_total_budget() {
        let started = Instant::now();
        let result = list_with_retry(
            FOREGROUND_LIST_RETRY,
            || true,
            std::future::pending::<Result<(), ListError>>,
        )
        .await;
        assert!(matches!(result, Err(ListFailure::Failed(message)) if message.contains("budget")));
        assert_eq!(started.elapsed(), Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_is_honoured_without_exceeding_total_budget() {
        let calls = Cell::new(0);
        let started = Instant::now();
        let result: Result<(), _> = list_with_retry(
            FOREGROUND_LIST_RETRY,
            || true,
            || {
                calls.set(calls.get() + 1);
                let inner =
                    ListObjectsV2Error::generic(ErrorMetadata::builder().code("SlowDown").build());
                let mut raw = HttpResponse::new(503.try_into().unwrap(), SdkBody::empty());
                raw.headers_mut().insert("retry-after", "120");
                async move { Err(SdkError::service_error(inner, raw)) }
            },
        )
        .await;
        assert!(matches!(result, Err(ListFailure::Failed(message)) if message.contains("budget")));
        assert_eq!(calls.get(), 1);
        assert_eq!(started.elapsed(), Duration::from_secs(30));
    }

    #[test]
    fn invalid_pagination_never_restarts_or_claims_completeness() {
        let mut seen = HashSet::new();
        let missing = ListObjectsV2Output::builder().is_truncated(true).build();
        assert!(next_page_cursor(&missing, &mut seen)
            .unwrap_err()
            .contains("without"));
        let blank = ListObjectsV2Output::builder()
            .is_truncated(true)
            .next_continuation_token("")
            .build();
        assert!(next_page_cursor(&blank, &mut seen).is_err());
        let page = ListObjectsV2Output::builder()
            .is_truncated(true)
            .next_continuation_token("cursor")
            .build();
        assert_eq!(
            next_page_cursor(&page, &mut seen).unwrap(),
            Some("cursor".into())
        );
        assert!(next_page_cursor(&page, &mut seen)
            .unwrap_err()
            .contains("repeated"));
        let complete = ListObjectsV2Output::builder().is_truncated(false).build();
        assert_eq!(next_page_cursor(&complete, &mut seen).unwrap(), None);
    }

    #[tokio::test]
    async fn background_quota_preserves_foreground_capacity_and_isolates_endpoints() {
        let scheduler = endpoint_scheduler("test-background-reserve");
        let same = endpoint_scheduler("test-background-reserve");
        let other = endpoint_scheduler("test-independent-endpoint");
        assert!(Arc::ptr_eq(&scheduler, &same));
        let mut permits = Vec::new();
        for _ in 0..BACKGROUND_LIST_CAPACITY {
            permits.push((
                scheduler.background.acquire().await.unwrap(),
                scheduler.total.acquire().await.unwrap(),
            ));
        }
        assert!(scheduler.background.try_acquire().is_err());
        assert!(scheduler.total.try_acquire().is_ok());
        assert_eq!(other.total.available_permits(), ENDPOINT_LIST_CAPACITY);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_queued_consumer_leaves_no_permit_or_request() {
        let scheduler = endpoint_scheduler("test-cancel-queue");
        let _occupied = scheduler
            .total
            .acquire_many(ENDPOINT_LIST_CAPACITY as u32)
            .await
            .unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let cancel = active.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.store(false, Ordering::SeqCst);
        });
        let sent = Cell::new(false);
        let result: Result<(), ListFailure> = while_active(
            async {
                let _permit = scheduler.total.acquire().await.unwrap();
                sent.set(true);
            },
            &|| active.load(Ordering::SeqCst),
            Instant::now() + Duration::from_secs(30),
        )
        .await;
        assert_eq!(result, Err(ListFailure::Cancelled));
        assert!(!sent.get());
    }

    #[test]
    fn singleflight_consumers_cancel_independently() {
        let (changed, _) = tokio::sync::watch::channel(0);
        let flight = Arc::new(PrefixFlight {
            started: tokio::time::Instant::now(),
            measurements: ListMeasurements::default(),
            state: Mutex::new(FlightState::default()),
            changed,
            consumers: AtomicUsize::new(2),
        });
        let first = FlightLease(flight.clone(), false);
        let second = FlightLease(flight.clone(), false);
        drop(first);
        assert!(flight.active());
        drop(second);
        assert!(!flight.active());
    }
    #[tokio::test]
    async fn first_http_page_is_shared_before_slow_second_page_and_cancel_stops_io() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (second_tx, second_rx) = tokio::sync::oneshot::channel();
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 2048];
                let count = first.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            server_requests.fetch_add(1, Ordering::SeqCst);
            let body = r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>test</Name><IsTruncated>true</IsTruncated><NextContinuationToken>second</NextContinuationToken><Contents><Key>first.txt</Key><Size>7</Size></Contents></ListBucketResult>"#;
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/xml\r\nConnection: close\r\n\r\n{}", body.len(), body);
            first.write_all(response.as_bytes()).await.unwrap();
            drop(first);
            let (mut second, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 2048];
            assert!(second.read(&mut buffer).await.unwrap() > 0);
            server_requests.fetch_add(1, Ordering::SeqCst);
            second_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let input = LazyListInput {
            account_id: "local-fixture".into(),
            bucket: "test".into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            prefix: String::new(),
            provider: Some("minio".into()),
            endpoint_scheme: Some("http".into()),
            endpoint_host: Some(address.to_string()),
            force_path_style: Some(true),
            region: None,
            force_refresh: Some(true),
            request_id: None,
            generation: None,
            run_id: None,
        };
        let fixture_flight = |input: LazyListInput| {
            let key = prefix_flight_key(&input);
            join_prefix_flight_with(input, key, |input, owner| async move {
                fetch_prefix(input, &owner).await
            })
        };
        let first = fixture_flight(input.clone());
        let second = fixture_flight(input);
        assert!(Arc::ptr_eq(&first.0, &second.0));
        assert!(!first.1);
        assert!(second.1);
        tokio::time::timeout(Duration::from_secs(10), second_rx)
            .await
            .unwrap()
            .unwrap();
        let shared = first.0.clone();
        {
            let state = shared.state.lock().unwrap();
            assert_eq!(state.pages.len(), 1);
            assert_eq!(state.pages[0].files[0].key, "first.txt");
            assert!(!state.pages[0].complete);
            assert!(state.pages[0].timing.network_ms > 0.0);
            assert!(
                state.pages[0].timing.page_ready_ms.unwrap() >= state.pages[0].timing.network_ms
            );
            assert_eq!(state.pages[0].timing.db_ms, 0.0);
            assert!(state.result.is_none());
        }
        drop(first);
        assert!(shared.active());
        drop(second);
        let started = Instant::now();
        loop {
            let result = shared.state.lock().unwrap().result.clone();
            if let Some(result) = result {
                assert!(matches!(result, Err(message) if message.contains("cancelled")));
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(1));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        server.abort();
    }
    #[tokio::test]
    async fn stale_background_cleanup_cannot_cancel_a_newer_run() {
        let old_id = BACKGROUND_RUN_ID.load(Ordering::SeqCst);
        let old_cancel = BACKGROUND_CANCEL.load(Ordering::SeqCst);
        let old_scope = BACKGROUND_SCOPE
            .lock()
            .unwrap()
            .replace("current-test-run".into());
        BACKGROUND_CANCEL.store(false, Ordering::SeqCst);
        cancel_background_sync(Some("older-test-run".into()))
            .await
            .unwrap();
        let unchanged = BACKGROUND_RUN_ID.load(Ordering::SeqCst) == old_id
            && !BACKGROUND_CANCEL.load(Ordering::SeqCst);
        cancel_background_sync(Some("current-test-run".into()))
            .await
            .unwrap();
        let cancelled = BACKGROUND_RUN_ID.load(Ordering::SeqCst) == old_id + 1
            && BACKGROUND_CANCEL.load(Ordering::SeqCst);
        BACKGROUND_RUN_ID.store(old_id, Ordering::SeqCst);
        BACKGROUND_CANCEL.store(old_cancel, Ordering::SeqCst);
        *BACKGROUND_SCOPE.lock().unwrap() = old_scope;
        assert!(unchanged);
        assert!(cancelled);
    }

    #[test]
    fn foreground_and_background_events_carry_their_origin_scope() {
        let scope = BackgroundScope {
            provider: "aws".into(),
            account_id: "account-A".into(),
            bucket: "bucket-A".into(),
            prefix: String::new(),
            run_id: "run-A".into(),
        };
        let event = serde_json::to_value(BackgroundSyncError {
            scope,
            error: "offline".into(),
        })
        .unwrap();
        assert_eq!(event["provider"], "aws");
        assert_eq!(event["account_id"], "account-A");
        assert_eq!(event["bucket"], "bucket-A");
        assert_eq!(event["run_id"], "run-A");
        assert_eq!(event["error"], "offline");
        let event = serde_json::to_value(FolderPage {
            scope: ListScope {
                provider: "r2".into(),
                account_id: "account-B".into(),
                bucket: "bucket-B".into(),
                prefix: "folder/".into(),
                request_id: "request-B".into(),
                generation: 42,
            },
            page: ListPage {
                timing: ListTiming::default(),
                files: Vec::new(),
                folders: Vec::new(),
                page_index: 0,
                next_cursor: None,
                complete: true,
                from_cache: true,
                freshness: "stale",
            },
        })
        .unwrap();
        assert_eq!(event["request_id"], "request-B");
        assert_eq!(event["generation"], 42);
        assert_eq!(event["prefix"], "folder/");
        assert_eq!(event["complete"], true);
        assert_eq!(event["from_cache"], true);
    }
    #[tokio::test(start_paused = true)]
    async fn timing_records_partial_wait_when_cancellation_drops_the_future() {
        let measurements = ListMeasurements::default();
        let result = tokio::time::timeout(Duration::from_millis(125), async {
            let _measure = MeasureInterval::new(&measurements.queue_ns);
            std::future::pending::<()>().await;
        })
        .await;
        assert!(result.is_err());
        let timing = measurements.snapshot();
        assert_eq!(timing.queue_ms, 125.0);
        assert_eq!(timing.network_ms, 0.0);
        assert_eq!(timing.backoff_ms, 0.0);
        assert_eq!(timing.db_ms, 0.0);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_timing_counts_actual_sleep_without_changing_the_attempt_budget() {
        let measurements = ListMeasurements::default();
        let started = Instant::now();
        let calls = Cell::new(0);
        let result = list_with_retry_measured(
            FOREGROUND_LIST_RETRY,
            Some(&measurements),
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
        assert!((measurements.snapshot().backoff_ms - elapsed_ms(started)).abs() < 0.001);
        assert!(measurements.snapshot().backoff_ms <= 1500.0);
    }

    #[tokio::test(start_paused = true)]
    async fn page_timing_snapshots_are_cumulative_and_do_not_change_on_later_work() {
        let (changed, _) = tokio::sync::watch::channel(0);
        let flight = PrefixFlight {
            started: Instant::now(),
            measurements: ListMeasurements::default(),
            state: Mutex::new(FlightState::default()),
            changed,
            consumers: AtomicUsize::new(2),
        };
        {
            let _measure = MeasureInterval::new(&flight.measurements.queue_ns);
            tokio::time::advance(Duration::from_millis(4)).await;
        }
        {
            let _measure = MeasureInterval::new(&flight.measurements.network_ns);
            tokio::time::advance(Duration::from_millis(8)).await;
        }
        let page = ListPage {
            timing: ListTiming::default(),
            files: Vec::new(),
            folders: Vec::new(),
            page_index: 0,
            next_cursor: Some("next".into()),
            complete: false,
            from_cache: false,
            freshness: "fresh",
        };
        flight.publish(page.clone());
        {
            let _measure = MeasureInterval::new(&flight.measurements.db_ns);
            tokio::time::advance(Duration::from_millis(3)).await;
        }
        flight.publish(ListPage {
            page_index: 1,
            next_cursor: None,
            complete: true,
            ..page
        });
        let state = flight.state.lock().unwrap();
        assert_eq!(state.pages[0].timing.queue_ms, 4.0);
        assert_eq!(state.pages[0].timing.network_ms, 8.0);
        assert_eq!(state.pages[0].timing.db_ms, 0.0);
        assert_eq!(state.pages[0].timing.page_ready_ms, Some(12.0));
        assert_eq!(state.pages[1].timing.queue_ms, 4.0);
        assert_eq!(state.pages[1].timing.network_ms, 8.0);
        assert_eq!(state.pages[1].timing.db_ms, 3.0);
        assert_eq!(state.pages[1].timing.page_ready_ms, Some(15.0));
        // Publication metrics have no consumer-local or emission measurements.
        assert_eq!(state.pages[1].timing.native_elapsed_ms, 0.0);
        assert_eq!(state.pages[1].timing.emit_ms, None);
        assert_eq!(state.pages[1].timing.emit_started_unix_ms, None);
    }

    #[test]
    fn timing_serialization_keeps_reply_emission_cost_separate_from_page_timestamp() {
        let reply = serde_json::to_value(ListTiming {
            emit_ms: Some(2.5),
            cache_ms: 3.25,
            ..ListTiming::default()
        })
        .unwrap();
        assert_eq!(reply["emit_ms"], 2.5);
        assert_eq!(reply["cache_ms"], 3.25);
        assert!(reply.get("page_ready_ms").is_none());
        assert!(reply.get("emit_started_unix_ms").is_none());
        let page = serde_json::to_value(ListTiming {
            page_ready_ms: Some(15.0),
            emit_started_unix_ms: Some(1_700_000_000_000.0),
            ..ListTiming::default()
        })
        .unwrap();
        assert_eq!(page["page_ready_ms"], 15.0);
        assert!(page.get("emit_ms").is_none());
    }
    #[test]
    fn cache_scope_uses_the_same_path_style_defaults_as_listing_clients() {
        let input = |provider: &str, path_style: Option<bool>| {
            serde_json::from_value::<LazyListInput>(serde_json::json!({
            "provider": provider, "account_id": "account", "bucket": "bucket", "prefix": "",
            "access_key_id": "key", "secret_access_key": "secret", "force_path_style": path_style,
        })).unwrap()
        };
        assert!(cache_config(&input("r2", None)).force_path_style);
        assert!(cache_config(&input("r2", Some(false))).force_path_style);
        assert!(!cache_config(&input("aws", None)).force_path_style);
        assert!(cache_config(&input("minio", None)).force_path_style);
        assert!(cache_config(&input("rustfs", None)).force_path_style);
        assert!(!cache_config(&input("minio", Some(false))).force_path_style);
    }
}
