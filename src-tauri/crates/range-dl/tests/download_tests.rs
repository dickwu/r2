//! Integration tests for the range-dl crate using wiremock mock HTTP server.

use std::time::Duration;

use range_dl::{
    ChunkEvent, ChunkState, ChunkStatus, DownloadTarget, RangeDownloadConfig, RangeDownloader,
    UrlProvider,
};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Create a UrlProvider that returns the given URL.
fn url_provider_for(url: String) -> UrlProvider {
    Box::new(move || {
        let u = url.clone();
        Box::pin(async move { Ok(u) })
    })
}

/// Generate test data of the given size.
fn test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 256) as u8).collect()
}

/// Setup a mock server that handles Range requests correctly.
async fn setup_range_server(data: &[u8]) -> MockServer {
    let server = MockServer::start().await;
    let data = data.to_vec();

    // Mount a handler that respects Range headers
    Mock::given(method("GET"))
        .respond_with(move |req: &wiremock::Request| {
            let range_header = req
                .headers
                .get("Range")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");

            if range_header.starts_with("bytes=") {
                let range_str = &range_header[6..];
                let parts: Vec<&str> = range_str.split('-').collect();
                let start: usize = parts[0].parse().unwrap_or(0);
                let end: usize = if parts.len() > 1 && !parts[1].is_empty() {
                    parts[1].parse::<usize>().unwrap_or(data.len() - 1)
                } else {
                    data.len() - 1
                };
                let end = end.min(data.len() - 1);
                let slice = &data[start..=end];

                ResponseTemplate::new(206)
                    .set_body_bytes(slice.to_vec())
                    .append_header(
                        "Content-Range",
                        format!("bytes {}-{}/{}", start, end, data.len()),
                    )
                    .append_header("Content-Length", slice.len().to_string())
            } else {
                ResponseTemplate::new(200)
                    .set_body_bytes(data.clone())
                    .append_header("Content-Length", data.len().to_string())
            }
        })
        .mount(&server)
        .await;

    server
}

/// Setup a mock server that always fails download requests.
async fn setup_failure_server(status: u16, body: &'static str) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(status).set_body_bytes(body.as_bytes().to_vec()))
        .mount(&server)
        .await;

    server
}

#[tokio::test]
async fn test_single_stream_small_file() {
    // Files < 10MB should use single stream (1 chunk)
    let data = test_data(1024 * 1024); // 1 MB
    let server = setup_range_server(&data).await;
    let url = server.uri();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("small_file.bin");

    let config = RangeDownloadConfig {
        min_chunk_size: 8 * 1024 * 1024,
        max_chunks: 8,
        ..Default::default()
    };

    let downloader = RangeDownloader::new(
        url_provider_for(url),
        DownloadTarget {
            file_size: data.len() as u64,
            destination: dest.clone(),
        },
        config,
    );

    let (mut rx, _control) = downloader.start().await.unwrap();

    let mut completed = false;
    while let Some(event) = rx.recv().await {
        match event {
            ChunkEvent::Complete { total_bytes, .. } => {
                assert_eq!(total_bytes, data.len() as u64);
                completed = true;
                break;
            }
            ChunkEvent::ChunkFailed { error, .. } => {
                panic!("Chunk failed: {}", error);
            }
            _ => {}
        }
    }

    assert!(completed, "Download should have completed");
    assert!(dest.exists(), "Final file should exist");

    let downloaded = tokio::fs::read(&dest).await.unwrap();
    assert_eq!(downloaded.len(), data.len());
    assert_eq!(downloaded, data);
}

#[tokio::test]
async fn test_chunked_download_large_file() {
    // Files >= 10MB should use chunked download
    let data = test_data(12 * 1024 * 1024); // 12 MB
    let server = setup_range_server(&data).await;
    let url = server.uri();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("large_file.bin");

    let config = RangeDownloadConfig {
        min_chunk_size: 4 * 1024 * 1024, // Lower threshold for testing
        max_chunks: 4,
        ..Default::default()
    };

    let downloader = RangeDownloader::new(
        url_provider_for(url),
        DownloadTarget {
            file_size: data.len() as u64,
            destination: dest.clone(),
        },
        config,
    );

    let (mut rx, _control) = downloader.start().await.unwrap();

    let mut completed = false;
    let mut saw_progress = false;
    while let Some(event) = rx.recv().await {
        match event {
            ChunkEvent::Progress { chunks, .. } => {
                saw_progress = true;
                // Should have multiple chunks
                assert!(!chunks.is_empty());
            }
            ChunkEvent::Complete { total_bytes, .. } => {
                assert_eq!(total_bytes, data.len() as u64);
                completed = true;
                break;
            }
            ChunkEvent::ChunkFailed { error, .. } => {
                panic!("Chunk failed: {}", error);
            }
            _ => {}
        }
    }

    assert!(completed, "Download should have completed");
    assert!(saw_progress, "Should have received progress events");
    assert!(dest.exists(), "Final file should exist");

    let downloaded = tokio::fs::read(&dest).await.unwrap();
    assert_eq!(downloaded.len(), data.len(), "File size should match");
    assert_eq!(downloaded, data, "File content should match byte-for-byte");
}

#[tokio::test]
async fn test_cancel_download() {
    let data = test_data(20 * 1024 * 1024); // 20 MB
    let server = setup_range_server(&data).await;
    let url = server.uri();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("cancel_file.bin");
    let part_path = dir.path().join("cancel_file.bin.part");

    let config = RangeDownloadConfig {
        min_chunk_size: 4 * 1024 * 1024,
        max_chunks: 4,
        ..Default::default()
    };

    let downloader = RangeDownloader::new(
        url_provider_for(url),
        DownloadTarget {
            file_size: data.len() as u64,
            destination: dest.clone(),
        },
        config,
    );

    let (mut rx, control) = downloader.start().await.unwrap();

    // Wait for first progress, then cancel
    let mut saw_progress = false;
    while let Some(event) = rx.recv().await {
        match event {
            ChunkEvent::Progress { .. } if !saw_progress => {
                saw_progress = true;
                // Cancel after first progress
                control.cancel.cancel();
            }
            ChunkEvent::Cancelled => {
                break;
            }
            _ => {}
        }
    }

    // .part file should be cleaned up
    // (Give a moment for cleanup)
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !part_path.exists(),
        ".part file should be deleted on cancel"
    );
    assert!(!dest.exists(), "Final file should not exist on cancel");
}

#[tokio::test]
async fn test_pause_and_resume() {
    let data = test_data(12 * 1024 * 1024); // 12 MB
    let server = setup_range_server(&data).await;
    let url = server.uri();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("pause_file.bin");

    let config = RangeDownloadConfig {
        min_chunk_size: 4 * 1024 * 1024,
        max_chunks: 2, // Use 2 chunks for simpler testing
        ..Default::default()
    };

    // Phase 1: Start and pause
    let downloader = RangeDownloader::new(
        url_provider_for(url.clone()),
        DownloadTarget {
            file_size: data.len() as u64,
            destination: dest.clone(),
        },
        config.clone(),
    );

    let (mut rx, control) = downloader.start().await.unwrap();

    let mut paused_states: Option<Vec<ChunkState>> = None;
    let mut saw_progress = false;

    while let Some(event) = rx.recv().await {
        match event {
            ChunkEvent::Progress { .. } if !saw_progress => {
                saw_progress = true;
                // Pause after first progress
                let _ = control.pause.send(true);
            }
            ChunkEvent::Paused { chunks_state } => {
                paused_states = Some(chunks_state);
                break;
            }
            _ => {}
        }
    }

    let states = paused_states.expect("Should have received paused states");
    assert!(!states.is_empty(), "Should have chunk states");

    // Phase 2: Resume from paused states
    let downloader2 = RangeDownloader::new(
        url_provider_for(url),
        DownloadTarget {
            file_size: data.len() as u64,
            destination: dest.clone(),
        },
        config,
    );

    let (mut rx2, _control2) = downloader2.resume(states).await.unwrap();

    let mut completed = false;
    while let Some(event) = rx2.recv().await {
        match event {
            ChunkEvent::Complete { total_bytes, .. } => {
                assert_eq!(total_bytes, data.len() as u64);
                completed = true;
                break;
            }
            ChunkEvent::ChunkFailed { error, .. } => {
                panic!("Resume chunk failed: {}", error);
            }
            _ => {}
        }
    }

    assert!(completed, "Resumed download should complete");
    assert!(dest.exists(), "Final file should exist after resume");

    let downloaded = tokio::fs::read(&dest).await.unwrap();
    assert_eq!(downloaded.len(), data.len());
    assert_eq!(downloaded, data, "Resumed file should match original");
}

#[tokio::test]
async fn test_terminal_failure_emits_failed_event_not_cancelled() {
    let file_size = 12 * 1024 * 1024;
    let server = setup_failure_server(500, "boom").await;
    let url = server.uri();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("failed_file.bin");

    let config = RangeDownloadConfig {
        min_chunk_size: 4 * 1024 * 1024,
        max_chunks: 4,
        max_retries: 0,
        ..Default::default()
    };

    let downloader = RangeDownloader::new(
        url_provider_for(url),
        DownloadTarget {
            file_size,
            destination: dest.clone(),
        },
        config,
    );

    let (mut rx, _control) = downloader.start().await.unwrap();

    let mut terminal_error: Option<String> = None;
    let mut saw_cancelled = false;

    while let Some(event) = rx.recv().await {
        match event {
            ChunkEvent::Failed { error } => {
                terminal_error = Some(error);
                break;
            }
            ChunkEvent::Cancelled => {
                saw_cancelled = true;
                break;
            }
            ChunkEvent::Complete { .. } => {
                panic!("Download should not complete successfully");
            }
            _ => {}
        }
    }

    assert!(
        !saw_cancelled,
        "Terminal failure should not be reported as cancelled"
    );

    let error = terminal_error.expect("Download should emit a terminal failure");
    assert!(
        error.contains("All remaining chunks failed") && error.contains("HTTP 500"),
        "Unexpected terminal failure: {error}"
    );
    assert!(
        !dest.exists(),
        "Final destination file should not exist after failure"
    );
}

#[tokio::test]
async fn test_chunks_for_size() {
    let config = RangeDownloadConfig::default();

    // < 8 MB (min_chunk_size) → single stream
    assert_eq!(config.chunks_for_size(5 * 1024 * 1024), 1);

    // 16 MB → 2 chunks (16/8=2)
    assert_eq!(config.chunks_for_size(16 * 1024 * 1024), 2);

    // 50 MB → 6 chunks (50/8=6.25, rounded down)
    assert_eq!(config.chunks_for_size(50 * 1024 * 1024), 6);

    // 100 MB → 8 chunks (100/8=12, capped by max_chunks=8)
    assert_eq!(config.chunks_for_size(100 * 1024 * 1024), 8);

    // 1 GB → 8 (capped by default max_chunks=8)
    assert_eq!(config.chunks_for_size(1024 * 1024 * 1024), 8);

    // With higher max_chunks
    let config2 = RangeDownloadConfig {
        max_chunks: 32,
        ..Default::default()
    };
    // 1 GB / 8 MB = 128 chunks, capped at 32
    assert_eq!(config2.chunks_for_size(1024 * 1024 * 1024), 32);
}

#[tokio::test]
async fn test_error_classification() {
    use range_dl::{classify_error, ErrorKind};

    assert_eq!(
        classify_error("No space left on device"),
        ErrorKind::DiskFull
    );
    assert_eq!(classify_error("disk full"), ErrorKind::DiskFull);
    assert_eq!(
        classify_error("Permission denied"),
        ErrorKind::PermissionDenied
    );
    assert_eq!(classify_error("HTTP 403 Forbidden"), ErrorKind::AuthExpired);
    assert_eq!(classify_error("connection timeout"), ErrorKind::Network);
    assert_eq!(
        classify_error("connection reset by peer"),
        ErrorKind::Network
    );
    assert_eq!(classify_error("something unexpected"), ErrorKind::Other);
}

#[tokio::test]
async fn test_meta_save_and_load() {
    use range_dl::meta::DownloadMeta;

    let dir = tempfile::tempdir().unwrap();
    let meta_path = dir.path().join("test.download_meta");

    let meta = DownloadMeta {
        file_size: 1024 * 1024,
        chunks: vec![
            ChunkState {
                chunk_id: 0,
                start: 0,
                end: 512 * 1024,
                downloaded_bytes: 256 * 1024,
                status: ChunkStatus::Paused,
            },
            ChunkState {
                chunk_id: 1,
                start: 512 * 1024,
                end: 1024 * 1024,
                downloaded_bytes: 0,
                status: ChunkStatus::Pending,
            },
        ],
    };

    meta.save(&meta_path).await.unwrap();
    assert!(meta_path.exists());

    let loaded = DownloadMeta::load(&meta_path).await.unwrap();
    assert_eq!(loaded.file_size, 1024 * 1024);
    assert_eq!(loaded.chunks.len(), 2);
    assert_eq!(loaded.chunks[0].downloaded_bytes, 256 * 1024);
    assert_eq!(loaded.chunks[0].status, ChunkStatus::Paused);
    assert_eq!(loaded.chunks[1].status, ChunkStatus::Pending);
}
