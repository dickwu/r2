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
        let ceiling = self.config.chunks_ceiling_for_size(file_size);
        let initial_count = if ceiling <= 1 { 1 } else { 2u16.min(ceiling) };
        let chunk_states = create_chunk_ranges(initial_count, file_size, ceiling);
        self.run(chunk_states, ceiling).await
    }

    /// Resume a download from persisted chunk states.
    pub async fn resume(
        self,
        chunks: Vec<ChunkState>,
    ) -> Result<(mpsc::Receiver<ChunkEvent>, DownloadControl), String> {
        let ceiling = self.config.chunks_ceiling_for_size(self.target.file_size);
        self.run(chunks, ceiling).await
    }

    async fn run(
        self,
        initial_chunks: Vec<ChunkState>,
        ceiling: u16,
    ) -> Result<(mpsc::Receiver<ChunkEvent>, DownloadControl), String> {
        let file_size = self.target.file_size;
        let dest = self.target.destination.clone();
        let part_path = part_path_for(&dest);
        let meta_path = meta_path_for(&dest);
        let config = self.config.clone();

        // Pre-allocate .part file
        preallocate_file(&part_path, file_size).await?;

        // Generate URLs upfront for initial chunks
        let mut urls: Vec<String> = Vec::with_capacity(initial_chunks.len());
        for _ in &initial_chunks {
            let url = (self.url_provider)()
                .await
                .map_err(|e| format!("Failed to generate presigned URL: {}", e))?;
            urls.push(url);
        }

        let (event_tx, event_rx) = mpsc::channel::<ChunkEvent>(64);
        let cancel = CancellationToken::new();
        let (pause_tx, pause_rx) = watch::channel(false);

        let control = DownloadControl {
            cancel: cancel.clone(),
            pause: pause_tx,
        };

        let url_provider = self.url_provider;

        tokio::spawn(async move {
            let result = orchestrate(
                initial_chunks,
                urls,
                url_provider,
                ceiling,
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
                    // Rename .part → final destination
                    if let Err(e) = tokio::fs::rename(&part_path, &dest).await {
                        let _ = event_tx
                            .send(ChunkEvent::ChunkFailed {
                                chunk_id: 0,
                                error: format!(
                                    "Rename failed: {}. File saved as {}",
                                    e,
                                    part_path.display()
                                ),
                            })
                            .await;
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
                    let meta = DownloadMeta {
                        file_size,
                        chunks: states.clone(),
                    };
                    let _ = meta.save(&meta_path).await;
                    let _ = event_tx
                        .send(ChunkEvent::Paused {
                            chunks_state: states,
                        })
                        .await;
                }
                OrchestratorOutcome::Cancelled => {
                    let _ = tokio::fs::remove_file(&part_path).await;
                    let _ = tokio::fs::remove_file(&meta_path).await;
                    let _ = event_tx.send(ChunkEvent::Cancelled).await;
                }
                OrchestratorOutcome::Failed { error } => {
                    let _ = tokio::fs::remove_file(&part_path).await;
                    let _ = tokio::fs::remove_file(&meta_path).await;
                    let _ = event_tx
                        .send(ChunkEvent::ChunkFailed {
                            chunk_id: 0,
                            error,
                        })
                        .await;
                }
            }
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

/// Create initial chunk ranges with reserved space for auto-tuning.
fn create_chunk_ranges(count: u16, file_size: u64, ceiling: u16) -> Vec<ChunkState> {
    if count <= 1 {
        return vec![ChunkState {
            chunk_id: 0,
            start: 0,
            end: file_size,
            downloaded_bytes: 0,
            status: ChunkStatus::Pending,
        }];
    }

    // If auto-tuning is possible (count < ceiling), assign only a fraction of the file
    // to initial chunks. The unassigned tail is reserved for future chunks.
    let assigned_end = if count < ceiling {
        let fraction = count as f64 / ceiling as f64;
        let computed = (file_size as f64 * fraction) as u64;
        // At least min_chunk_size per chunk
        computed.max(count as u64 * 1024 * 1024).min(file_size)
    } else {
        file_size
    };

    let chunk_size = assigned_end / count as u64;
    (0..count)
        .map(|i| {
            let start = i as u64 * chunk_size;
            let end = if i == count - 1 {
                assigned_end
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
}

#[allow(clippy::too_many_arguments)]
async fn orchestrate(
    initial_chunks: Vec<ChunkState>,
    initial_urls: Vec<String>,
    url_provider: UrlProvider,
    ceiling: u16,
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
        .timeout(Duration::from_secs(3600))
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

    // Channel for chunk tasks to report results back to the orchestrator
    let (result_tx, mut result_rx) = mpsc::channel::<(usize, ChunkResult)>(32);

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
    let mut autotune_tick = tokio::time::interval(Duration::from_secs(5));
    let autotune_possible = chunks.len() < ceiling as usize && ceiling > 1;
    let mut autotune_active = autotune_possible;
    let mut last_throughput: Option<f64> = None;

    loop {
        // Check completion
        if chunks.iter().all(|c| c.completed) && active_count == 0 {
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
            Some((idx, result)) = result_rx.recv() => {
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

            // Auto-tuning
            _ = autotune_tick.tick(), if autotune_active => {
                let current_speed: f64 = chunks.iter()
                    .filter(|c| !c.completed)
                    .map(|c| c.tracker.get_speed())
                    .sum();

                if let Some(prev) = last_throughput {
                    let gain = if prev > 0.0 { current_speed / prev } else { 2.0 };
                    if gain < 1.2 {
                        autotune_active = false;
                        log::info!("Auto-tune: stopped at {} chunks (gain {:.0}%)",
                            chunks.len(), (gain - 1.0) * 100.0);
                        continue;
                    }
                }
                last_throughput = Some(current_speed);

                let current_count = chunks.len() as u16;
                if current_count >= ceiling {
                    autotune_active = false;
                    continue;
                }

                // Find unassigned range
                let max_assigned = chunks.iter().map(|c| c.state.end).max().unwrap_or(0);
                if max_assigned >= file_size {
                    autotune_active = false;
                    continue;
                }

                let unassigned = file_size - max_assigned;
                let new_count = current_count.min(ceiling - current_count);
                if new_count == 0 || unassigned < config.min_chunk_size {
                    autotune_active = false;
                    continue;
                }

                let new_chunk_size = unassigned / new_count as u64;

                for i in 0..new_count {
                    let cid = current_count + i;
                    let start = max_assigned + i as u64 * new_chunk_size;
                    let end = if i == new_count - 1 { file_size } else { max_assigned + (i as u64 + 1) * new_chunk_size };

                    let url = match (url_provider)().await {
                        Ok(u) => u,
                        Err(e) => {
                            log::warn!("Auto-tune URL failed for chunk {}: {}", cid, e);
                            autotune_active = false;
                            break;
                        }
                    };

                    let state = ChunkState {
                        chunk_id: cid,
                        start,
                        end,
                        downloaded_bytes: 0,
                        status: ChunkStatus::Downloading,
                    };

                    let runtime = ChunkRuntime {
                        tracker: ChunkTracker::new(0),
                        state: state.clone(),
                        url,
                        completed: false,
                        retry_count: 0,
                    };

                    chunks.push(runtime);
                    let idx = chunks.len() - 1;
                    spawn_chunk(
                        idx, &client, &chunks[idx], &part_path, &config,
                        &cancel, &pause_rx, &result_tx,
                    );
                    active_count += 1;
                }

                log::info!("Auto-tune: scaled to {} chunks", chunks.len());
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
    result_tx: &mpsc::Sender<(usize, ChunkResult)>,
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

    let tracker = ChunkTracker {
        downloaded_bytes: downloaded,
        speed,
    };

    tokio::spawn(async move {
        let result = download_chunk(&client, &url, &state, &path, &cfg, &tracker, &cancel, &pause).await;
        let _ = tx.send((idx, result)).await;
    });
}
