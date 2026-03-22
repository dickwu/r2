//! Single chunk download logic: HTTP Range GET → seek-based write to file.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use reqwest::Client;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::types::{classify_error, ChunkState, ChunkStatus, ErrorKind, RangeDownloadConfig};

/// Result of a single chunk download attempt.
pub(crate) enum ChunkResult {
    /// Chunk completed successfully.
    Complete { bytes_written: u64 },
    /// Chunk was paused — state saved for resume.
    Paused { state: ChunkState },
    /// Chunk was cancelled.
    Cancelled,
    /// Chunk failed with a classified error.
    Failed { error: String, kind: ErrorKind },
}

/// Shared progress counter for a chunk (read by the progress aggregator).
pub(crate) struct ChunkTracker {
    pub downloaded_bytes: Arc<AtomicU64>,
    pub speed: Arc<AtomicU64>, // stored as f64 bits via to_bits/from_bits
}

impl ChunkTracker {
    pub fn new(initial_bytes: u64) -> Self {
        Self {
            downloaded_bytes: Arc::new(AtomicU64::new(initial_bytes)),
            speed: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn get_downloaded(&self) -> u64 {
        self.downloaded_bytes.load(Ordering::Relaxed)
    }

    pub fn get_speed(&self) -> f64 {
        f64::from_bits(self.speed.load(Ordering::Relaxed))
    }
}

/// Download a single chunk of a file using an HTTP Range request.
///
/// Each chunk opens its own file handle and seeks to the correct offset.
/// This avoids shared cursor contention across parallel chunks.
pub(crate) async fn download_chunk(
    client: &Client,
    url: &str,
    state: &ChunkState,
    dest_path: &std::path::Path,
    config: &RangeDownloadConfig,
    tracker: &ChunkTracker,
    cancel: &CancellationToken,
    pause: &watch::Receiver<bool>,
) -> ChunkResult {
    let resume_offset = state.resume_offset();
    let end_byte = state.end.saturating_sub(1); // Range header is inclusive on both ends

    // Build HTTP request with Range header
    let range_header = format!("bytes={}-{}", resume_offset, end_byte);

    let response = match client
        .get(url)
        .header("Range", &range_header)
        .timeout(config.connect_timeout)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            return ChunkResult::Failed {
                error: format!("HTTP request failed: {}", e),
                kind: ErrorKind::Network,
            };
        }
    };

    // Verify we got a successful range response
    let status = response.status();
    if !status.is_success() && status.as_u16() != 206 {
        let text = response.text().await.unwrap_or_default();
        let err = format!("HTTP {}: {}", status.as_u16(), text);
        return ChunkResult::Failed {
            kind: classify_error(&err),
            error: err,
        };
    }

    // Open file handle for this chunk (independent handle, independent cursor)
    let mut file = match OpenOptions::new().write(true).open(dest_path).await {
        Ok(f) => f,
        Err(e) => {
            let err = e.to_string();
            return ChunkResult::Failed {
                kind: classify_error(&err),
                error: format!("Failed to open file: {}", err),
            };
        }
    };

    // Seek to our write position
    if let Err(e) = file.seek(SeekFrom::Start(resume_offset)).await {
        let err = e.to_string();
        return ChunkResult::Failed {
            kind: classify_error(&err),
            error: format!("Failed to seek: {}", err),
        };
    }

    // Stream the response body with buffered writes
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;

    let mut write_buffer = Vec::with_capacity(config.write_buffer_size);
    let mut bytes_written = state.downloaded_bytes;
    let mut pause_rx = pause.clone();

    // Speed calculation: use a sliding window (last 3 seconds) for responsive speed display.
    // Total-average (bytes/elapsed) becomes misleadingly low when a chunk is nearly done
    // and the last few percent trickle in slowly.
    let mut speed_window_start = Instant::now();
    let mut speed_window_bytes: u64 = 0;

    loop {
        tokio::select! {
            // Check for cancellation
            _ = cancel.cancelled() => {
                // Flush buffer before exiting
                if !write_buffer.is_empty() {
                    let _ = file.write_all(&write_buffer).await;
                }
                return ChunkResult::Cancelled;
            }
            // Check for pause signal
            Ok(()) = pause_rx.changed() => {
                if *pause_rx.borrow() {
                    // Flush buffer
                    if !write_buffer.is_empty() {
                        if let Err(e) = file.write_all(&write_buffer).await {
                            let err = e.to_string();
                            return ChunkResult::Failed {
                                kind: classify_error(&err),
                                error: format!("Failed to flush on pause: {}", err),
                            };
                        }
                        write_buffer.clear();
                    }
                    let _ = file.flush().await;

                    return ChunkResult::Paused {
                        state: ChunkState {
                            chunk_id: state.chunk_id,
                            start: state.start,
                            end: state.end,
                            downloaded_bytes: bytes_written,
                            status: ChunkStatus::Paused,
                        },
                    };
                }
            }
            // Read next chunk from stream
            chunk = stream.next() => {
                match chunk {
                    Some(Ok(data)) => {
                        let data_len = data.len() as u64;
                        write_buffer.extend_from_slice(&data);
                        bytes_written += data_len;
                        speed_window_bytes += data_len;

                        // Update tracker atomics
                        tracker.downloaded_bytes.store(bytes_written, Ordering::Relaxed);

                        // Calculate speed using a 3-second sliding window for responsive display.
                        // Reset the window every 3 seconds so the speed reflects recent throughput,
                        // not the ever-declining total average.
                        let window_elapsed = speed_window_start.elapsed().as_secs_f64();
                        let speed = if window_elapsed > 0.1 {
                            speed_window_bytes as f64 / window_elapsed
                        } else {
                            0.0
                        };
                        tracker.speed.store(speed.to_bits(), Ordering::Relaxed);

                        // Reset window every 3 seconds
                        if window_elapsed >= 3.0 {
                            speed_window_start = Instant::now();
                            speed_window_bytes = 0;
                        }

                        // Flush buffer when it reaches target size
                        if write_buffer.len() >= config.write_buffer_size {
                            if let Err(e) = file.write_all(&write_buffer).await {
                                let err = e.to_string();
                                return ChunkResult::Failed {
                                    kind: classify_error(&err),
                                    error: format!("Write failed: {}", err),
                                };
                            }
                            write_buffer.clear();
                        }
                    }
                    Some(Err(e)) => {
                        // Flush what we have so far (for resume)
                        if !write_buffer.is_empty() {
                            let _ = file.write_all(&write_buffer).await;
                        }
                        let err = e.to_string();
                        return ChunkResult::Failed {
                            kind: classify_error(&err),
                            error: format!("Stream error: {}", err),
                        };
                    }
                    None => {
                        // Stream finished — flush remaining buffer
                        if !write_buffer.is_empty() {
                            if let Err(e) = file.write_all(&write_buffer).await {
                                let err = e.to_string();
                                return ChunkResult::Failed {
                                    kind: classify_error(&err),
                                    error: format!("Final flush failed: {}", err),
                                };
                            }
                        }
                        if let Err(e) = file.flush().await {
                            let err = e.to_string();
                            return ChunkResult::Failed {
                                kind: classify_error(&err),
                                error: format!("File flush failed: {}", err),
                            };
                        }

                        tracker.downloaded_bytes.store(bytes_written, Ordering::Relaxed);
                        return ChunkResult::Complete { bytes_written };
                    }
                }
            }
        }
    }
}
