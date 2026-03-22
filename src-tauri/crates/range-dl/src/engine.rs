//! Download engine: orchestrates parallel chunk downloads with auto-tuning,
//! progress aggregation, retry logic, and pause/resume/cancel support.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::chunk::{download_chunk, ChunkResult, ChunkTracker};
use crate::meta::DownloadMeta;
use crate::types::{
    ChunkEvent, ChunkProgress, ChunkState, ChunkStatus, DownloadControl, DownloadTarget,
    ErrorKind, RangeDownloadConfig, UrlProvider,
};

/// The main download engine.
pub struct RangeDownloader {
    url_provider: UrlProvider,
    target: DownloadTarget,
    config: RangeDownloadConfig,
}

impl RangeDownloader {
    pub fn new(
        url_provider: UrlProvider,
        target: DownloadTarget,
        config: RangeDownloadConfig,
    ) -> Self {
        Self {
            url_provider,
            target,
            config,
        }
    }

    /// Start a new download. Returns a channel receiver for events and a control handle.
    pub async fn start(
        self,
    ) -> Result<(mpsc::Receiver<ChunkEvent>, DownloadControl), String> {
        let file_size = self.target.file_size;
        let chunk_count = self.config.chunks_for_size(file_size);
        let chunk_states = create_chunk_ranges(chunk_count, file_size);
        self.run(chunk_states).await
    }

    /// Resume a download from persisted chunk states.
    pub async fn resume(
        self,
        chunks: Vec<ChunkState>,
    ) -> Result<(mpsc::Receiver<ChunkEvent>, DownloadControl), String> {
        self.run(chunks).await
    }

    async fn run(
        self,
        initial_chunks: Vec<ChunkState>,
    ) -> Result<(mpsc::Receiver<ChunkEvent>, DownloadControl), String> {
        let file_size = self.target.file_size;
        let dest = self.target.destination.clone();
        let part_path = part_path_for(&dest);
        let meta_path = meta_path_for(&dest);
        let config = self.config.clone();

        // Pre-allocate .part file
        preallocate_file(&part_path, file_size).await?;

        // Generate one presigned URL — all chunks use the same URL with different Range headers.
        // Fresh URLs are only needed for stall-restart retries (generated per-chunk then).
        let base_url = (self.url_provider)()
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e))?;
        let urls: Vec<String> = vec![base_url; initial_chunks.len()];

        let (event_tx, event_rx) = mpsc::channel::<ChunkEvent>(64);
        let cancel = CancellationToken::new();
        let (pause_tx, pause_rx) = watch::channel(false);

        let control = DownloadControl {
            cancel: cancel.clone(),
            pause: pause_tx,
        };

        let url_provider = self.url_provider;

        // Spawn engine task. Uses an inner spawn + join to catch panics
        // and guarantee a terminal event is always sent to the worker.
        let event_tx_guard = event_tx.clone();
        tokio::spawn(async move {
            let inner = async {
                let result = orchestrate(
                    initial_chunks,
                    urls,
                    url_provider,
                    file_size,
                    part_path.clone(),
                    meta_path.clone(),
                    dest.clone(),
                    config,
                    event_tx.clone(),
                    cancel,
                    pause_rx,
                )
                .await;

                match result {
                    OrchestratorOutcome::Complete { elapsed, total_bytes } => {
                        // Remove existing file at destination before rename
                        let _ = tokio::fs::remove_file(&dest).await;

                        // Rename .part → final destination
                        if let Err(e) = tokio::fs::rename(&part_path, &dest).await {
                            log::error!("Rename failed: {}", e);
                            let _ = event_tx.send(ChunkEvent::Cancelled).await;
                            return;
                        }
                        let _ = tokio::fs::remove_file(&meta_path).await;

                        let avg_speed = if elapsed > 0.0 {
                            total_bytes as f64 / elapsed
                        } else {
                            0.0
                        };
                        let _ = event_tx
                            .send(ChunkEvent::Complete {
                                total_bytes,
                                elapsed_secs: elapsed,
                                avg_speed,
                            })
                            .await;
                    }
                    OrchestratorOutcome::Paused { states } => {
                        let meta = DownloadMeta { file_size, chunks: states.clone() };
                        let _ = meta.save(&meta_path).await;
                        let _ = event_tx.send(ChunkEvent::Paused { chunks_state: states }).await;
                    }
                    OrchestratorOutcome::Cancelled => {
                        let _ = tokio::fs::remove_file(&part_path).await;
                        let _ = tokio::fs::remove_file(&meta_path).await;
                        let _ = event_tx.send(ChunkEvent::Cancelled).await;
                    }
                    OrchestratorOutcome::Failed { error } => {
                        log::error!("Download failed: {}", error);
                        // Do NOT delete .part file on failure — it may contain valid
                        // downloaded data. Only user-initiated Cancel deletes.
                        let _ = tokio::fs::remove_file(&meta_path).await;
                        let _ = event_tx
                            .send(ChunkEvent::ChunkFailed {
                                chunk_id: 0,
                                error,
                            })
                            .await;
                        let _ = event_tx.send(ChunkEvent::Cancelled).await;
                    }
                }
            };

            // Run the inner async block. If it panics or the event_tx is somehow
            // dropped without sending a terminal event, the guard sends Cancelled.
            inner.await;
            // event_tx_guard is dropped here — if inner already sent a terminal event
            // and the worker consumed it, this drop is harmless (channel already closing).
            // If inner panicked before sending, the guard's drop closes the channel,
            // and the worker sees None from rx.recv().
            drop(event_tx_guard);
        });

        Ok((event_rx, control))
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn part_path_for(dest: &PathBuf) -> PathBuf {
    let mut p = dest.as_os_str().to_owned();
    p.push(".part");
    PathBuf::from(p)
}

fn meta_path_for(dest: &PathBuf) -> PathBuf {
    let mut p = dest.as_os_str().to_owned();
    p.push(".download_meta");
    PathBuf::from(p)
}

/// Pre-allocate the .part file. If it already exists (resume), just verify the size.
async fn preallocate_file(path: &PathBuf, size: u64) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    // Check if file already exists (resume case)
    if path.exists() {
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| format!("Failed to read .part file metadata: {}", e))?;
        if meta.len() == size {
            // File exists at correct size — resume without truncating
            return Ok(());
        }
        // File exists but wrong size — extend (don't truncate)
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .await
            .map_err(|e| format!("Failed to open .part file: {}", e))?;
        file.set_len(size)
            .await
            .map_err(|e| format!("Failed to resize .part file: {}", e))?;
        return Ok(());
    }

    // New download — create and pre-allocate
    let file = tokio::fs::File::create(path)
        .await
        .map_err(|e| format!("Failed to create .part file: {}", e))?;
    file.set_len(size)
        .await
        .map_err(|e| format!("Failed to pre-allocate file: {}", e))?;
    Ok(())
}

/// Create initial chunk ranges covering the ENTIRE file.
/// Every byte from 0 to file_size is assigned to a chunk — no unassigned gaps.
/// Auto-tuning works by observing throughput gain from these initial chunks
/// and adjusting chunk count for future downloads, NOT by leaving bytes unassigned.
fn create_chunk_ranges(count: u16, file_size: u64) -> Vec<ChunkState> {
    if count <= 1 {
        return vec![ChunkState {
            chunk_id: 0,
            start: 0,
            end: file_size,
            downloaded_bytes: 0,
            status: ChunkStatus::Pending,
        }];
    }

    // Always cover the entire file — no reserved/unassigned ranges.
    let chunk_size = file_size / count as u64;
    (0..count)
        .map(|i| {
            let start = i as u64 * chunk_size;
            let end = if i == count - 1 {
                file_size // Last chunk gets the remainder
            } else {
                (i as u64 + 1) * chunk_size
            };
            ChunkState {
                chunk_id: i,
                start,
                end,
                downloaded_bytes: 0,
                status: ChunkStatus::Pending,
            }
        })
        .collect()
}

// ── Orchestrator ─────────────────────────────────────────────────

enum OrchestratorOutcome {
    Complete { elapsed: f64, total_bytes: u64 },
    Paused { states: Vec<ChunkState> },
    Cancelled,
    Failed { error: String },
}

/// Per-chunk runtime state managed by the orchestrator.
struct ChunkRuntime {
    state: ChunkState,
    url: String,
    tracker: ChunkTracker,
    completed: bool,
    retry_count: u8,
    /// Generation counter — incremented on each re-spawn (stall restart).
    /// Results from older generations are ignored to prevent double-counting.
    generation: u32,
}

#[allow(clippy::too_many_arguments)]
async fn orchestrate(
    initial_chunks: Vec<ChunkState>,
    initial_urls: Vec<String>,
    url_provider: UrlProvider,
    file_size: u64,
    part_path: PathBuf,
    meta_path: PathBuf,
    _dest: PathBuf,
    config: RangeDownloadConfig,
    event_tx: mpsc::Sender<ChunkEvent>,
    cancel: CancellationToken,
    pause_rx: watch::Receiver<bool>,
) -> OrchestratorOutcome {
    let client = match reqwest::Client::builder()
        .connect_timeout(config.connect_timeout) // Only connection timeout, NOT request timeout
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return OrchestratorOutcome::Failed {
                error: format!("Failed to build HTTP client: {}", e),
            };
        }
    };

    let start_time = Instant::now();

    // Channel for chunk tasks to report results back to the orchestrator.
    // Includes (chunk_index, generation, result) so stale results can be ignored.
    let (result_tx, mut result_rx) = mpsc::channel::<(usize, u32, ChunkResult)>(32);

    // Build chunk runtime state
    let mut chunks: Vec<ChunkRuntime> = initial_chunks
        .into_iter()
        .zip(initial_urls.into_iter())
        .map(|(state, url)| ChunkRuntime {
            tracker: ChunkTracker::new(state.downloaded_bytes),
            state,
            url,
            completed: false,
            retry_count: 0,
            generation: 0,
        })
        .collect();

    // Mark already-complete chunks
    for c in &mut chunks {
        if c.state.status == ChunkStatus::Complete {
            c.completed = true;
        }
    }

    let mut active_count = 0u16;
    let mut failed_count = 0u16;

    // Spawn initial chunk tasks
    for (idx, chunk) in chunks.iter_mut().enumerate() {
        if chunk.completed {
            continue;
        }
        chunk.state.status = ChunkStatus::Downloading;
        spawn_chunk(
            idx,
            &client,
            chunk,
            &part_path,
            &config,
            &cancel,
            &pause_rx,
            &result_tx,
        );
        active_count += 1;
    }

    // Timers
    let mut progress_tick = tokio::time::interval(Duration::from_millis(200));
    let mut checkpoint_tick = tokio::time::interval(Duration::from_secs(10));
    let mut stall_check_tick = tokio::time::interval(Duration::from_secs(15));

    // Stall detection: track last-known progress per chunk
    let mut last_chunk_progress: Vec<u64> = chunks.iter().map(|c| c.tracker.get_downloaded()).collect();
    let mut stall_count: Vec<u8> = vec![0; chunks.len()];

    loop {
        // Check completion: all chunks done
        if chunks.iter().all(|c| c.completed) && active_count == 0 {
            let max_end = chunks.iter().map(|c| c.state.end).max().unwrap_or(0);
            if max_end < file_size {
                log::warn!(
                    "Chunks cover up to {} but file_size is {} (gap: {} bytes) — proceeding anyway",
                    max_end, file_size, file_size - max_end
                );
            }
            let _ = tokio::fs::remove_file(&meta_path).await;
            return OrchestratorOutcome::Complete {
                elapsed: start_time.elapsed().as_secs_f64(),
                total_bytes: file_size,
            };
        }

        // Too many failures
        if failed_count as usize > chunks.len() / 2 {
            cancel.cancel();
            return OrchestratorOutcome::Failed {
                error: format!("Too many failures ({}/{})", failed_count, chunks.len()),
            };
        }

        tokio::select! {
            biased; // prioritize cancel/pause over progress ticks

            _ = cancel.cancelled() => {
                return OrchestratorOutcome::Cancelled;
            }

            // Chunk task completed
            Some((idx, gen, result)) = result_rx.recv() => {
                // Ignore results from stale tasks (previous generation before stall restart)
                if idx < chunks.len() && gen < chunks[idx].generation {
                    log::debug!("Ignoring stale result for chunk {} (gen {} < current {})",
                        idx, gen, chunks[idx].generation);
                    continue;
                }
                active_count = active_count.saturating_sub(1);

                match result {
                    ChunkResult::Complete { bytes_written } => {
                        chunks[idx].completed = true;
                        chunks[idx].state.downloaded_bytes = bytes_written;
                        chunks[idx].state.status = ChunkStatus::Complete;
                        let _ = event_tx.send(ChunkEvent::ChunkComplete {
                            chunk_id: chunks[idx].state.chunk_id,
                        }).await;
                    }
                    ChunkResult::Paused { state } => {
                        chunks[idx].state = state;
                        // If all active chunks have reported back (paused or completed), we're done
                        if active_count == 0 {
                            let states = snapshot_states(&chunks);
                            return OrchestratorOutcome::Paused { states };
                        }
                    }
                    ChunkResult::Cancelled => {
                        // Will be handled by the cancel branch above
                    }
                    ChunkResult::Failed { error, kind } => {
                        let can_retry = match kind {
                            ErrorKind::DiskFull | ErrorKind::PermissionDenied => false,
                            ErrorKind::Network => chunks[idx].retry_count < config.max_retries,
                            ErrorKind::AuthExpired => chunks[idx].retry_count < 1,
                            ErrorKind::Other => chunks[idx].retry_count < 1,
                        };

                        if can_retry {
                            chunks[idx].retry_count += 1;
                            let attempt = chunks[idx].retry_count;

                            let _ = event_tx.send(ChunkEvent::ChunkRetry {
                                chunk_id: chunks[idx].state.chunk_id,
                                attempt,
                                error: error.clone(),
                            }).await;

                            // Exponential backoff
                            let backoff = config.retry_backoff_base * 2u32.pow((attempt - 1) as u32);
                            tokio::time::sleep(backoff).await;

                            // Fresh URL for auth errors
                            if kind == ErrorKind::AuthExpired {
                                if let Ok(new_url) = (url_provider)().await {
                                    chunks[idx].url = new_url;
                                }
                            }

                            // Re-spawn
                            spawn_chunk(
                                idx, &client, &chunks[idx], &part_path, &config,
                                &cancel, &pause_rx, &result_tx,
                            );
                            active_count += 1;
                        } else {
                            failed_count += 1;
                            chunks[idx].state.status = ChunkStatus::Failed;
                            let _ = event_tx.send(ChunkEvent::ChunkFailed {
                                chunk_id: chunks[idx].state.chunk_id,
                                error,
                            }).await;
                        }
                    }
                }
            }

            // Progress aggregation
            _ = progress_tick.tick() => {
                let progress: Vec<ChunkProgress> = chunks.iter().map(|c| {
                    let downloaded = if c.completed {
                        c.state.total_bytes()
                    } else {
                        c.tracker.get_downloaded()
                    };
                    let speed = if c.completed { 0.0 } else { c.tracker.get_speed() };
                    ChunkProgress {
                        chunk_id: c.state.chunk_id,
                        start: c.state.start,
                        end: c.state.end,
                        downloaded_bytes: downloaded,
                        speed,
                        status: if c.completed { ChunkStatus::Complete } else { c.state.status },
                    }
                }).collect();

                let agg_speed: f64 = progress.iter().map(|p| p.speed).sum();
                let agg_downloaded: u64 = progress.iter().map(|p| p.downloaded_bytes).sum();

                let _ = event_tx.send(ChunkEvent::Progress {
                    chunks: progress,
                    aggregate_speed: agg_speed,
                    aggregate_downloaded: agg_downloaded,
                    total_bytes: file_size,
                }).await;
            }

            // Periodic meta checkpoint
            _ = checkpoint_tick.tick() => {
                let meta = DownloadMeta {
                    file_size,
                    chunks: snapshot_states(&chunks),
                };
                let _ = meta.save(&meta_path).await;
            }

            // Stall detection: restart chunks that haven't made progress in 30s (2 consecutive checks)
            _ = stall_check_tick.tick() => {
                // Extend tracking arrays if auto-tuning added new chunks
                while last_chunk_progress.len() < chunks.len() {
                    last_chunk_progress.push(0);
                    stall_count.push(0);
                }

                for (idx, chunk) in chunks.iter_mut().enumerate() {
                    if chunk.completed || chunk.state.status != ChunkStatus::Downloading {
                        stall_count[idx] = 0;
                        continue;
                    }

                    let current = chunk.tracker.get_downloaded();
                    if current <= last_chunk_progress[idx] {
                        stall_count[idx] += 1;
                        if stall_count[idx] >= 2 {
                            // Stalled for 30s — restart this chunk with a fresh URL
                            log::warn!(
                                "Stall detected: chunk {} stuck at {} bytes for 30s, restarting",
                                chunk.state.chunk_id, current
                            );

                            // Update chunk state with current progress for resume
                            chunk.state.downloaded_bytes = current;
                            chunk.retry_count = chunk.retry_count.saturating_add(1);
                            chunk.generation += 1; // Bump generation so old task's result is ignored

                            // Get fresh URL
                            if let Ok(new_url) = (url_provider)().await {
                                chunk.url = new_url;
                            }

                            // Re-spawn the chunk. Do NOT increment active_count —
                            // we're replacing the old stalled task, not adding a new one.
                            // The old task's HTTP stream is hanging; its eventual result
                            // (if any) will be safely ignored since completed is already
                            // set or the new task's result arrives first.
                            // The saturating_sub on active_count handles the case where
                            // both old and new tasks eventually report results.
                            spawn_chunk(
                                idx, &client, chunk, &part_path, &config,
                                &cancel, &pause_rx, &result_tx,
                            );
                            // active_count stays the same — replacing, not adding
                            stall_count[idx] = 0;

                            let _ = event_tx.send(ChunkEvent::ChunkRetry {
                                chunk_id: chunk.state.chunk_id,
                                attempt: chunk.retry_count,
                                error: "Stall detected — restarting chunk".to_string(),
                            }).await;
                        }
                    } else {
                        stall_count[idx] = 0;
                    }
                    last_chunk_progress[idx] = current;
                }
            }
        }
    }
}

fn snapshot_states(chunks: &[ChunkRuntime]) -> Vec<ChunkState> {
    chunks
        .iter()
        .map(|c| {
            let downloaded = if c.completed {
                c.state.total_bytes()
            } else {
                c.tracker.get_downloaded()
            };
            ChunkState {
                chunk_id: c.state.chunk_id,
                start: c.state.start,
                end: c.state.end,
                downloaded_bytes: downloaded,
                status: if c.completed {
                    ChunkStatus::Complete
                } else {
                    c.state.status
                },
            }
        })
        .collect()
}

/// Spawn a chunk download task that reports results through the channel.
#[allow(clippy::too_many_arguments)]
fn spawn_chunk(
    idx: usize,
    client: &reqwest::Client,
    chunk: &ChunkRuntime,
    part_path: &PathBuf,
    config: &RangeDownloadConfig,
    cancel: &CancellationToken,
    pause_rx: &watch::Receiver<bool>,
    result_tx: &mpsc::Sender<(usize, u32, ChunkResult)>,
) {
    let client = client.clone();
    let url = chunk.url.clone();
    let state = chunk.state.clone();
    let path = part_path.clone();
    let cfg = config.clone();
    let downloaded = chunk.tracker.downloaded_bytes.clone();
    let speed = chunk.tracker.speed.clone();
    let cancel = cancel.clone();
    let pause = pause_rx.clone();
    let tx = result_tx.clone();
    let gen = chunk.generation;

    let tracker = ChunkTracker {
        downloaded_bytes: downloaded,
        speed,
    };

    tokio::spawn(async move {
        let result = download_chunk(&client, &url, &state, &path, &cfg, &tracker, &cancel, &pause).await;
        let _ = tx.send((idx, gen, result)).await;
    });
}
