use crate::db::cache_scope::{self, CacheConfig, CacheScope};
use crate::db::{self, CachedFile};
use crate::providers::aws;
use crate::providers::minio;
use crate::providers::operation::{
    self, execute as execute_operation, AttemptError, Backoff, OperationContext, OperationError,
    OperationKind,
};
use crate::providers::s3_client::{describe_s3_error, StorageErrorClass};
use crate::r2;
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Output;
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
    pub cache_cursor: Option<String>,
    pub page_index: Option<usize>,
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

#[tauri::command]
pub async fn get_prefix_cache_page(input: LazyListInput) -> Result<Option<FolderPage>, String> {
    let started = tokio::time::Instant::now();
    let scope = ListScope::new(&input);
    let cache_scope = CacheScope::capture(&cache_config(&input))
        .await
        .map_err(|e| e.to_string())?;
    let snapshot = cache_scope::read_prefix_page(
        &cache_scope,
        &input.bucket,
        &input.prefix,
        input.cache_cursor.as_deref(),
        1000,
    )
    .await
    .map_err(|e| format!("DB error: {e}"))?;
    let complete_index = snapshot.full_sync
        && snapshot
            .skipped_prefixes
            .as_ref()
            .is_some_and(|skipped| !is_under_skipped_prefix(&input.prefix, skipped));
    let cache_complete = snapshot.prefix_time.is_some() || complete_index;
    if !cache_complete {
        return Ok(None);
    }
    let fresh = snapshot.freshness_time.is_some_and(|time| {
        let age = chrono::Utc::now().timestamp() - time;
        (0..DIRECTORY_TTL_SECS).contains(&age)
    });
    let next_cursor = snapshot.page.next_cursor;
    let complete = next_cursor.is_none();
    let cache_ms = elapsed_ms(started);
    let timing = ListTiming {
        cache_ms,
        native_elapsed_ms: cache_ms,
        emit_ms: Some(0.0),
        ..ListTiming::default()
    };
    Ok(Some(FolderPage {
        scope,
        page: ListPage {
            timing,
            files: snapshot.page.files.iter().map(LazyFileItem::from).collect(),
            folders: snapshot.page.folders,
            page_index: input.page_index.unwrap_or(0),
            next_cursor,
            complete,
            from_cache: true,
            freshness: if !complete {
                "partial"
            } else if fresh {
                "fresh"
            } else {
                "stale"
            },
        },
    }))
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
    let snapshot =
        cache_scope::read_prefix_page(cache_scope, &input.bucket, &input.prefix, None, 1000)
            .await
            .map_err(|e| format!("DB error: {e}"))?;
    let prefix_time = snapshot.prefix_time;
    let complete_index = snapshot.full_sync
        && snapshot
            .skipped_prefixes
            .as_ref()
            .is_some_and(|skipped| !is_under_skipped_prefix(&input.prefix, skipped));
    let cache_complete = prefix_time.is_some() || complete_index;
    let page = snapshot.page;
    if !cache_complete {
        return Ok(None);
    }
    let complete = page.next_cursor.is_none();
    let fresh = snapshot.freshness_time.is_some_and(|time| {
        let age = chrono::Utc::now().timestamp() - time;
        (0..DIRECTORY_TTL_SECS).contains(&age)
    });
    let mut result = LazyListResult {
        timing: ListTiming::default(),
        scope,
        files: page.files.iter().map(LazyFileItem::from).collect(),
        folders: page.folders,
        complete,
        from_cache: true,
        freshness: if !complete {
            "partial"
        } else if fresh {
            "fresh"
        } else if cache_complete {
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

async fn emit_fresh_cached_prefix_stream(
    app: &tauri::AppHandle,
    input: &LazyListInput,
    scope: &ListScope,
    cache_scope: &CacheScope,
    consumer_started: tokio::time::Instant,
    cache_ms: f64,
    cancellation: &RequestCancellation,
) -> Result<Option<Arc<LazyListResult>>, String> {
    let mut cursor: Option<String> = None;
    let mut page_index = 0;
    let mut files = Vec::new();
    let mut folders = Vec::new();
    let mut emit_ms = 0.0;
    let mut total_cache_ms = cache_ms;
    loop {
        if !cancellation.active() {
            return Err("S3 list cancelled".into());
        }
        let page_started = tokio::time::Instant::now();
        let snapshot = cache_scope::read_prefix_page(
            cache_scope,
            &input.bucket,
            &input.prefix,
            cursor.as_deref(),
            1000,
        )
        .await
        .map_err(|e| format!("DB error: {e}"))?;
        total_cache_ms += elapsed_ms(page_started);
        let complete_index = snapshot.full_sync
            && snapshot
                .skipped_prefixes
                .as_ref()
                .is_some_and(|skipped| !is_under_skipped_prefix(&input.prefix, skipped));
        let cache_complete = snapshot.prefix_time.is_some() || complete_index;
        let fresh = snapshot.freshness_time.is_some_and(|time| {
            let age = chrono::Utc::now().timestamp() - time;
            (0..DIRECTORY_TTL_SECS).contains(&age)
        });
        if !cache_complete || !fresh {
            return Ok(None);
        }
        let next_cursor = snapshot.page.next_cursor.clone();
        let complete = next_cursor.is_none();
        let page_files: Vec<LazyFileItem> =
            snapshot.page.files.iter().map(LazyFileItem::from).collect();
        let page_folders = snapshot.page.folders;
        files.extend(page_files.iter().cloned());
        folders.extend(page_folders.iter().cloned());
        let timing = ListTiming {
            cache_ms: total_cache_ms,
            native_elapsed_ms: elapsed_ms(consumer_started),
            ..ListTiming::default()
        };
        emit_ms += emit_folder_page(
            app,
            FolderPage {
                scope: scope.clone(),
                page: ListPage {
                    timing,
                    files: page_files,
                    folders: page_folders,
                    page_index,
                    next_cursor: next_cursor
                        .as_ref()
                        .map(|_| format!("cache:{}", page_index + 1)),
                    complete,
                    from_cache: true,
                    freshness: "fresh",
                },
            },
            consumer_started,
        )?;
        if complete {
            let timing = ListTiming {
                cache_ms: total_cache_ms,
                native_elapsed_ms: elapsed_ms(consumer_started),
                emit_ms: Some(emit_ms),
                ..ListTiming::default()
            };
            return Ok(Some(Arc::new(LazyListResult {
                timing,
                scope: scope.clone(),
                files,
                folders,
                complete: true,
                from_cache: true,
                freshness: "fresh",
            })));
        }
        cursor = next_cursor;
        page_index += 1;
    }
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
    match provider {
        "r2" => format!("r2:{}", input.account_id.trim().to_ascii_lowercase()),
        "aws"
            if input
                .endpoint_host
                .as_deref()
                .is_none_or(|host| host.trim().is_empty()) =>
        {
            format!(
                "aws:{}",
                input
                    .region
                    .as_deref()
                    .unwrap_or("us-east-1")
                    .trim()
                    .to_ascii_lowercase()
            )
        }
        "aws" => crate::move_transfer::config::physical_operation_endpoint(
            input.endpoint_scheme.as_deref().unwrap_or("https"),
            input.endpoint_host.as_deref().unwrap_or_default(),
            &input.bucket,
        ),
        "minio" | "rustfs" => crate::move_transfer::config::physical_operation_endpoint(
            input.endpoint_scheme.as_deref().unwrap_or("http"),
            input.endpoint_host.as_deref().unwrap_or_default(),
            &input.bucket,
        ),
        _ => format!("r2:{}", input.account_id.trim().to_ascii_lowercase()),
    }
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

async fn join_prefix_flight(
    input: LazyListInput,
    cache_scope: CacheScope,
) -> Result<FlightLease, String> {
    // Captured before the first request: a local write to the folder advances
    // it, so a listing that may predate the write publishes stale and a request
    // made after the write starts its own listing instead of sharing this one.
    let generation =
        cache_scope::capture_prefix_generation(&cache_scope, &input.bucket, &input.prefix)
            .await
            .map_err(|e| format!("DB error: {e}"))?;
    // An account edited away and back must not join the obsolete revision's
    // in-flight result even when its credentials happen to match again.
    let key = format!(
        "{}:{}:{generation}",
        prefix_flight_key(&input),
        cache_scope.revision
    );
    Ok(join_prefix_flight_with(
        input,
        key,
        move |input, owner| async move {
            cache_scope::in_scope(cache_scope, fetch_prefix(input, &owner, generation)).await
        },
    ))
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
        cache_ms = elapsed_ms(cache_started);
        if emit_pages {
            if let Some(result) = emit_fresh_cached_prefix_stream(
                &app,
                &input,
                &scope,
                &cache_scope,
                started,
                cache_ms,
                &cancellation,
            )
            .await?
            {
                return Ok(result);
            }
        } else {
            let cache = read_prefix_cache_scoped(&input, scope.clone(), &cache_scope).await?;
            if let Some(mut cache) = cache.filter(|cache| cache.freshness == "fresh") {
                if !cancellation.active() {
                    return Err("S3 list cancelled".into());
                }
                cache.timing.cache_ms = cache_ms;
                cache.timing.emit_ms = Some(0.0);
                cache.timing.native_elapsed_ms = elapsed_ms(started);
                return Ok(Arc::new(cache));
            }
        }
    }
    if !cancellation.active() {
        return Err("S3 list cancelled".into());
    }
    let lease = join_prefix_flight(input, cache_scope).await?;
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

#[allow(dead_code)]
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
    listed_generation: i64,
) -> Result<LazyListResult, String> {
    let client = create_client_for_input(&input).await?;
    let endpoint = endpoint_scope(&input);
    let scheduler = endpoint_scheduler(&endpoint);
    let now = chrono::Utc::now().timestamp();
    let mut freshness = "fresh";
    let mut files = Vec::new();
    let mut folders = Vec::new();
    let mut continuation_token: Option<String> = None;
    let mut seen_tokens = HashSet::new();
    let mut seen_files = HashSet::new();
    let mut seen_folders = HashSet::new();
    let mut page_index = 0;
    loop {
        let operation_scope = format!("{}:{}", input.bucket, input.prefix);
        let response = list_with_shared_executor(
            FOREGROUND_LIST_RETRY,
            &endpoint,
            &operation_scope,
            "",
            &scheduler,
            false,
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
                async move { request.send().await }
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
            let published_fresh = db::prefix_sync::replace_complete_prefix(
                &input.bucket,
                &input.account_id,
                &input.prefix,
                &files,
                &folders,
                listed_generation,
            )
            .await
            .map_err(|e| format!("Failed to cache complete listing: {e}"))?;
            if !published_fresh {
                // The folder changed locally while it was being listed.
                freshness = "stale";
                page.freshness = freshness;
            }
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
        freshness,
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
#[allow(dead_code)]
fn skipped_prefixes_key(bucket: &str, account_id: &str) -> String {
    format!("skipped_prefixes:{account_id}:{bucket}")
}

/// Records what a completed sync skipped, clearing the note when it skipped
/// nothing — so a bucket heals itself once the provider is fixed.
#[allow(dead_code)]
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
    /// The executor's jitter caps: 1×, 2×, 4×… the initial backoff, capped.
    fn backoff(&self) -> Backoff {
        Backoff {
            initial: self.initial_backoff,
            max: self.max_backoff,
        }
    }
}

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

fn record_operation_delta(
    before: operation::OperationMetrics,
    measurements: Option<&ListMeasurements>,
) {
    let Some(measurements) = measurements else {
        return;
    };
    let after = operation::metrics();
    measurements.queue_ns.fetch_add(
        after
            .queue_us
            .saturating_sub(before.queue_us)
            .saturating_mul(1000),
        Ordering::Relaxed,
    );
    measurements.network_ns.fetch_add(
        after
            .network_us
            .saturating_sub(before.network_us)
            .saturating_mul(1000),
        Ordering::Relaxed,
    );
    measurements.backoff_ns.fetch_add(
        after
            .backoff_us
            .saturating_sub(before.backoff_us)
            .saturating_mul(1000),
        Ordering::Relaxed,
    );
}

fn list_failure_from_operation(error: OperationError) -> ListFailure {
    match error {
        OperationError::Cancelled | OperationError::Paused => ListFailure::Cancelled,
        OperationError::Deadline { last } => {
            let mut message = "S3 list exceeded its 30 second request budget".to_string();
            if let Some(last) = last {
                message.push_str("; last attempt: ");
                message.push_str(&last.message);
            }
            ListFailure::Failed(message)
        }
        OperationError::Failed { error, attempts } => {
            let prefix = if error.class == StorageErrorClass::Transient && attempts > 1 {
                format!("S3 list failed after {attempts} attempts: ")
            } else {
                "S3 list failed: ".to_string()
            };
            ListFailure::Failed(format!("{prefix}{}", error.message))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn list_with_shared_executor<T, E, Fut>(
    policy: ListRetryPolicy,
    endpoint: &str,
    scope: &str,
    identity: &str,
    scheduler: &EndpointScheduler,
    background: bool,
    measurements: Option<&ListMeasurements>,
    is_active: impl Fn() -> bool,
    send_page: impl Fn() -> Fut,
) -> Result<T, ListFailure>
where
    E: std::error::Error + ProvideErrorMetadata + 'static,
    Fut: Future<Output = Result<T, SdkError<E, HttpResponse>>>,
{
    let cancelled = AtomicBool::new(false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let before = operation::metrics();
    let operation = async {
        let _background = if background {
            let queue_measure = measurements.map(|m| MeasureInterval::new(&m.queue_ns));
            let permit = scheduler
                .background
                .acquire()
                .await
                .expect("private semaphore is never closed");
            drop(queue_measure);
            Some(permit)
        } else {
            None
        };
        let context = OperationContext::new(
            OperationKind::List,
            endpoint,
            scope,
            identity,
            deadline,
            &cancelled,
        )
        .with_max_attempts(policy.max_attempts)
        .with_backoff(policy.backoff());
        execute_operation(&context, || {
            let page = send_page();
            async move {
                page.await.map_err(|error| {
                    let class = crate::providers::s3_client::s3_error_class(&error, false);
                    let message = describe_s3_error(&error);
                    let retry_after = error
                        .raw_response()
                        .and_then(|response| response.headers().get("retry-after"))
                        .and_then(parse_retry_after_header)
                        .unwrap_or_default();
                    AttemptError::new(class, message).with_retry_after(retry_after)
                })
            }
        })
        .await
    };
    tokio::pin!(operation);
    let cancel_probe = async {
        loop {
            if !is_active() {
                cancelled.store(true, Ordering::SeqCst);
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::pin!(cancel_probe);
    let result = tokio::select! {
        biased;
        _ = &mut cancel_probe => Err(OperationError::Cancelled),
        result = &mut operation => result,
    };
    record_operation_delta(before, measurements);
    result.map_err(list_failure_from_operation)
}

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
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => return Err(ListFailure::Failed("S3 list exceeded its 30 second request budget".into())),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            result = &mut future => return Ok(result),
        }
    }
}

fn parse_retry_after_header(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    (date.with_timezone(&chrono::Utc) - chrono::Utc::now())
        .to_std()
        .ok()
}

const ENDPOINT_LIST_CAPACITY: usize = 4;
const BACKGROUND_LIST_CAPACITY: usize = ENDPOINT_LIST_CAPACITY - 1;
struct EndpointScheduler {
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
        background: tokio::sync::Semaphore::new(BACKGROUND_LIST_CAPACITY),
    });
    schedulers.insert(scope.into(), Arc::downgrade(&scheduler));
    scheduler
}

/// Background LIST work first takes a UI-only quota, leaving one shared
/// operation-executor control permit for foreground LIST/HEAD callers on the
/// same physical endpoint. The operation executor owns retries, retry-after,
/// total deadline, endpoint admission and cancellation during queued/network
/// phases.

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
    let sync_run = db::begin_sync(&bucket, &account_id)
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
    let endpoint = endpoint_scope(&input);
    let scheduler = endpoint_scheduler(&endpoint);

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

            let operation_scope = format!("{}:{}", bucket, current_prefix);
            let response = match list_with_shared_executor(
                BACKGROUND_LIST_RETRY,
                &endpoint,
                &operation_scope,
                "",
                &scheduler,
                true,
                None,
                || is_background_run_active(run_id),
                || {
                    let request = create_request();
                    async move { request.send().await }
                },
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
                db::store_file_batch(&bucket, &account_id, &sync_run, &batch)
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

    // Finish sync atomically: swap files, rebuild tree, publish skipped-prefix
    // metadata, clear old prefix freshness, and advance the full-sync generation.
    db::finish_sync_with_metadata(
        &bucket,
        &account_id,
        &sync_run,
        fetched_count,
        &folder_keys,
        &skipped_prefixes,
    )
    .await
    .map_err(|e| format!("Failed to finish sync: {}", e))?;

    if !is_background_run_active(run_id) {
        return Ok(BackgroundSyncResult {
            scope: scope.clone(),
            total_objects: fetched_count,
            total_bytes: fetched_bytes,
            cancelled: true,
            skipped_prefixes,
        });
    }

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
    use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
    use tokio::time::Instant;

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let waits: Vec<Duration> = (0..6)
            .map(|failed_attempt| BACKGROUND_LIST_RETRY.backoff().cap(failed_attempt))
            .collect();

        assert_eq!(waits, [1, 2, 4, 8, 16, 16].map(Duration::from_secs));
        assert_eq!(
            FOREGROUND_LIST_RETRY.backoff().cap(0),
            Duration::from_millis(500)
        );
        assert_eq!(
            FOREGROUND_LIST_RETRY.backoff().cap(1),
            Duration::from_secs(1)
        );
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

    #[test]
    fn endpoint_scope_normalizes_to_physical_endpoint_without_bucket_identity() {
        let mut input = LazyListInput {
            account_id: "ACCOUNT".into(),
            bucket: "bucket-a".into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
            prefix: String::new(),
            provider: Some("aws".into()),
            endpoint_scheme: Some("HTTPS".into()),
            endpoint_host: Some("Example.COM/tenant/bucket-a".into()),
            force_path_style: Some(true),
            region: Some("US-EAST-1".into()),
            force_refresh: None,
            request_id: None,
            generation: None,
            cache_cursor: None,
            page_index: None,
            run_id: None,
        };
        assert_eq!(endpoint_scope(&input), "https://example.com/tenant");
        // Another bucket on the same server shares the scope, whether or not
        // its endpoint was entered with the bucket as the last path segment.
        input.bucket = "bucket-b".into();
        input.endpoint_host = Some("example.com/tenant/bucket-b".into());
        assert_eq!(endpoint_scope(&input), "https://example.com/tenant");
        input.endpoint_host = Some("example.com/tenant".into());
        assert_eq!(endpoint_scope(&input), "https://example.com/tenant");
        input.endpoint_host = None;
        assert_eq!(endpoint_scope(&input), "aws:us-east-1");
        input.provider = Some("r2".into());
        assert_eq!(endpoint_scope(&input), "r2:account");
    }

    #[tokio::test]
    async fn background_quota_preserves_foreground_capacity_and_isolates_endpoints() {
        let scheduler = endpoint_scheduler("test-background-reserve");
        let same = endpoint_scheduler("test-background-reserve");
        let other = endpoint_scheduler("test-independent-endpoint");
        assert!(Arc::ptr_eq(&scheduler, &same));

        let mut permits = Vec::new();
        for _ in 0..BACKGROUND_LIST_CAPACITY {
            permits.push(scheduler.background.acquire().await.unwrap());
        }
        assert!(scheduler.background.try_acquire().is_err());
        assert_eq!(
            other.background.available_permits(),
            BACKGROUND_LIST_CAPACITY
        );

        let result = list_with_shared_executor::<
            _,
            aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error,
            _,
        >(
            FOREGROUND_LIST_RETRY,
            "test-background-reserve",
            "bucket:",
            "",
            &scheduler,
            false,
            None,
            || true,
            || async { Ok::<_, SdkError<ListObjectsV2Error, HttpResponse>>("foreground-page") },
        )
        .await;
        assert_eq!(result, Ok("foreground-page"));
    }

    #[tokio::test]
    async fn ui_list_and_head_share_the_physical_endpoint_control_limit() {
        const SHARED_CONTROL_LIMIT: usize = 4;
        let endpoint = "https://shared-control.example";
        let scheduler = endpoint_scheduler(endpoint);
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut tasks = Vec::new();

        for index in 0..8 {
            let active = active.clone();
            let peak = peak.clone();
            let scheduler = scheduler.clone();
            tasks.push(tokio::spawn(async move {
                let scope = format!("bucket:list-{index}");
                list_with_shared_executor::<
                    _,
                    aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error,
                    _,
                >(
                    FOREGROUND_LIST_RETRY,
                    endpoint,
                    &scope,
                    "",
                    &scheduler,
                    false,
                    None,
                    || true,
                    || {
                        let active = active.clone();
                        let peak = peak.clone();
                        async move {
                            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(current, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            active.fetch_sub(1, Ordering::SeqCst);
                            Ok::<_, SdkError<ListObjectsV2Error, HttpResponse>>(())
                        }
                    },
                )
                .await
                .unwrap();
            }));
        }
        for index in 0..8 {
            let active = active.clone();
            let peak = peak.clone();
            let cancelled = cancelled.clone();
            tasks.push(tokio::spawn(async move {
                let scope = format!("bucket:head-{index}");
                let context = OperationContext::new(
                    OperationKind::Head,
                    endpoint,
                    &scope,
                    "head",
                    tokio::time::Instant::now() + Duration::from_secs(30),
                    &cancelled,
                )
                .with_max_attempts(1);
                execute_operation(&context, || {
                    let active = active.clone();
                    let peak = peak.clone();
                    async move {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok::<_, AttemptError>(())
                    }
                })
                .await
                .unwrap();
            }));
        }

        for task in tasks {
            task.await.unwrap();
        }
        assert!(
            peak.load(Ordering::SeqCst) <= SHARED_CONTROL_LIMIT,
            "LIST and HEAD exceeded the shared control limit"
        );
    }

    #[tokio::test]
    async fn cancelling_a_queued_list_does_not_dispatch_the_request() {
        let endpoint = "https://queued-cancel.example";
        let scheduler = endpoint_scheduler(endpoint);
        let blockers_cancelled = Arc::new(AtomicBool::new(false));
        let blockers_active = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let mut blockers = Vec::new();
        for index in 0..4 {
            let cancelled = blockers_cancelled.clone();
            let active = blockers_active.clone();
            let release = release.clone();
            blockers.push(tokio::spawn(async move {
                let scope = format!("bucket:blocker-{index}");
                let context = OperationContext::new(
                    OperationKind::Head,
                    endpoint,
                    &scope,
                    "head",
                    tokio::time::Instant::now() + Duration::from_secs(30),
                    &cancelled,
                )
                .with_max_attempts(1);
                execute_operation(&context, || {
                    let active = active.clone();
                    let release = release.clone();
                    async move {
                        active.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok::<_, AttemptError>(())
                    }
                })
                .await
                .unwrap();
            }));
        }
        let started = tokio::time::Instant::now();
        while blockers_active.load(Ordering::SeqCst) < 4 {
            assert!(started.elapsed() < Duration::from_secs(1));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let active = Arc::new(AtomicBool::new(true));
        let cancel = active.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            cancel.store(false, Ordering::SeqCst);
        });
        let sent = Arc::new(AtomicUsize::new(0));
        let result: Result<(), ListFailure> = list_with_shared_executor::<
            _,
            aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error,
            _,
        >(
            FOREGROUND_LIST_RETRY,
            endpoint,
            "bucket:list",
            "",
            &scheduler,
            false,
            None,
            || active.load(Ordering::SeqCst),
            || {
                let sent = sent.clone();
                async move {
                    sent.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, SdkError<ListObjectsV2Error, HttpResponse>>(())
                }
            },
        )
        .await;

        release.notify_waiters();
        for blocker in blockers {
            blocker.await.unwrap();
        }
        assert_eq!(result, Err(ListFailure::Cancelled));
        assert_eq!(sent.load(Ordering::SeqCst), 0);
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
            cache_cursor: None,
            page_index: None,
            run_id: None,
        };
        let fixture_flight = |input: LazyListInput| {
            let key = prefix_flight_key(&input);
            join_prefix_flight_with(input, key, |input, owner| async move {
                fetch_prefix(input, &owner, 0).await
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

    fn list_page_xml(keys: &[&str], next: Option<&str>) -> String {
        let contents: String = keys
            .iter()
            .map(|key| format!("<Contents><Key>{key}</Key><Size>1</Size></Contents>"))
            .collect();
        let truncation = match next {
            Some(token) => format!(
                "<IsTruncated>true</IsTruncated><NextContinuationToken>{token}</NextContinuationToken>"
            ),
            None => "<IsTruncated>false</IsTruncated>".into(),
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>fence</Name>{truncation}{contents}</ListBucketResult>"#
        )
    }

    async fn flight_result(flight: &PrefixFlight) -> Result<Arc<LazyListResult>, String> {
        let started = Instant::now();
        loop {
            let result = flight.state.lock().unwrap().result.clone();
            if let Some(result) = result {
                return result;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "listing hung");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn listing_overlapping_a_local_delete_is_neither_published_nor_shared_as_fresh() {
        use crate::test_s3::{serve, Response};
        const ACCOUNT: &str = "listing-fence-account";
        const BUCKET: &str = "fence";
        let deleted = Arc::new(AtomicBool::new(false));
        let second_page = Arc::new(tokio::sync::Semaphore::new(0));
        let fixture = {
            let deleted = deleted.clone();
            let second_page = second_page.clone();
            serve(move |request| {
                let deleted = deleted.clone();
                let second_page = second_page.clone();
                async move {
                    if request.method == "DELETE" {
                        deleted.store(true, Ordering::SeqCst);
                        return Response::empty(204);
                    }
                    if request.path.contains("continuation-token=") {
                        second_page.acquire().await.unwrap().forget();
                        return Response::xml(200, &list_page_xml(&["z.txt"], None));
                    }
                    let keys: &[&str] = if deleted.load(Ordering::SeqCst) {
                        &["a.txt"]
                    } else {
                        &["a.txt", "k.txt"]
                    };
                    Response::xml(200, &list_page_xml(keys, Some("page-2")))
                }
            })
            .await
        };
        let host = fixture
            .endpoint
            .strip_prefix("http://")
            .unwrap()
            .to_string();
        crate::db::init_test_db().await;
        crate::db::get_connection()
            .unwrap()
            .lock()
            .await
            .execute(
                "INSERT INTO minio_accounts (id, access_key_id, secret_access_key, endpoint_scheme, endpoint_host, force_path_style, created_at, updated_at)
                 VALUES (?1, 'fixture', 'fixture-secret', 'http', ?2, 1, 0, 0)",
                turso::params![ACCOUNT, host.clone()],
            )
            .await
            .unwrap();
        let input = LazyListInput {
            account_id: ACCOUNT.into(),
            bucket: BUCKET.into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            prefix: String::new(),
            provider: Some("minio".into()),
            endpoint_scheme: Some("http".into()),
            endpoint_host: Some(host.clone()),
            force_path_style: Some(true),
            region: None,
            force_refresh: Some(true),
            request_id: None,
            generation: None,
            cache_cursor: None,
            page_index: None,
            run_id: None,
        };
        let scope = CacheScope::capture(&cache_config(&input)).await.unwrap();

        let first = join_prefix_flight(input.clone(), scope.clone())
            .await
            .unwrap();
        let started = Instant::now();
        while first.0.state.lock().unwrap().pages.is_empty() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "first page hung"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(first.0.state.lock().unwrap().pages[0]
            .files
            .iter()
            .any(|file| file.key == "k.txt"));
        // The user deletes k.txt after page one was listed: the delete command
        // removes it on the provider and from the cache while page two is
        // still in flight.
        crate::commands::delete_minio_object_with(
            crate::commands::MinioConfigInput {
                account_id: ACCOUNT.into(),
                bucket: BUCKET.into(),
                access_key_id: "fixture".into(),
                secret_access_key: "fixture-secret".into(),
                endpoint_scheme: "http".into(),
                endpoint_host: host,
                force_path_style: true,
            },
            "k.txt".into(),
            &crate::commands::upload_cache::RecordedCacheEvents::default(),
        )
        .await
        .unwrap();
        assert!(deleted.load(Ordering::SeqCst));
        second_page.add_permits(16);
        let overlapped = flight_result(&first.0).await.unwrap();
        assert_eq!(overlapped.freshness, "stale");

        // Its rows are written without a fresh marker: nothing vouches for them
        // (there is no full index here either), so the next open re-lists.
        // Other tests share this connection: keep the lock until the statement
        // is dropped, or its step and reset race theirs ("concurrent use").
        let marker = {
            let conn = crate::db::get_connection().unwrap().lock().await;
            let mut rows = conn
                .query(
                    "SELECT last_synced_at, listed_at, file_count FROM prefix_sync_times
                     WHERE bucket = ?1 AND account_id = ?2 AND prefix = ''",
                    turso::params![BUCKET, ACCOUNT],
                )
                .await
                .unwrap();
            let row = rows.next().await.unwrap().unwrap();
            (
                row.get::<i64>(0).unwrap(),
                row.get::<i64>(1).unwrap(),
                row.get::<i64>(2).unwrap(),
            )
        };
        assert_eq!(marker, (0, 0, 3));
        let cached = read_prefix_cache_scoped(&input, ListScope::new(&input), &scope)
            .await
            .unwrap();
        assert!(
            cached.is_none(),
            "a listing that overlapped a local delete was served from cache"
        );

        // A request made after the delete must list again, not share the
        // pre-delete flight, and its listing is then fresh without the file.
        let later = join_prefix_flight(input.clone(), scope.clone())
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&first.0, &later.0));
        let relisted = flight_result(&later.0).await.unwrap();
        assert!(!relisted.files.iter().any(|file| file.key == "k.txt"));
        let cached = read_prefix_cache_scoped(&input, ListScope::new(&input), &scope)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cached.freshness, "fresh");
        assert!(!cached.files.iter().any(|file| file.key == "k.txt"));
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
