//! Shared transfer progress utilities.
//!
//! `SpeedWindow` produces smooth instantaneous transfer rates from a sliding
//! sample window (instead of sluggish cumulative averages that also break on
//! resumed transfers). `ThrottleGate` rate-limits high-frequency progress
//! emissions so per-network-chunk streams don't flood the Tauri IPC bridge.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How far back the speed window looks when computing the rate.
const SPEED_WINDOW: Duration = Duration::from_secs(2);
/// Weight of the newest window measurement in the smoothed rate.
const SPEED_SMOOTHING: f64 = 0.4;

/// Sliding-window speed tracker producing smooth instantaneous byte rates.
pub struct SpeedWindow {
    inner: Mutex<SpeedWindowState>,
}

struct SpeedWindowState {
    samples: VecDeque<(Instant, u64)>,
    smoothed: f64,
}

impl SpeedWindow {
    /// Window seeded at zero transferred bytes.
    pub fn new() -> Self {
        Self::with_baseline(0)
    }

    /// Window seeded at `baseline` cumulative bytes (resumed transfers).
    pub fn with_baseline(baseline: u64) -> Self {
        let mut samples = VecDeque::with_capacity(64);
        samples.push_back((Instant::now(), baseline));
        Self {
            inner: Mutex::new(SpeedWindowState {
                samples,
                smoothed: 0.0,
            }),
        }
    }

    /// Record the current cumulative byte count and return the smoothed
    /// bytes/sec rate over the recent window.
    pub fn sample(&self, cumulative_bytes: u64) -> f64 {
        let now = Instant::now();
        let mut state = self.inner.lock().unwrap();
        state.samples.push_back((now, cumulative_bytes));

        while state.samples.len() > 2 {
            let (t, _) = state.samples[0];
            if now.duration_since(t) > SPEED_WINDOW {
                state.samples.pop_front();
            } else {
                break;
            }
        }

        let (t0, b0) = state.samples[0];
        let dt = now.duration_since(t0).as_secs_f64();
        if dt < 0.05 {
            return state.smoothed;
        }
        let raw = cumulative_bytes.saturating_sub(b0) as f64 / dt;
        state.smoothed = if state.smoothed == 0.0 {
            raw
        } else {
            state.smoothed * (1.0 - SPEED_SMOOTHING) + raw * SPEED_SMOOTHING
        };
        state.smoothed
    }
}

impl Default for SpeedWindow {
    fn default() -> Self {
        Self::new()
    }
}

/// Lock-free gate allowing an action at most once per interval.
/// The first call always passes.
pub struct ThrottleGate {
    started: Instant,
    interval_ms: u64,
    /// Millis-since-start + 1 of the last pass; 0 means "never passed".
    last_ms: AtomicU64,
}

impl ThrottleGate {
    pub fn new(interval: Duration) -> Self {
        Self {
            started: Instant::now(),
            interval_ms: interval.as_millis() as u64,
            last_ms: AtomicU64::new(0),
        }
    }

    /// Returns true when the caller may proceed, consuming the slot.
    pub fn try_pass(&self) -> bool {
        let now = self.started.elapsed().as_millis() as u64 + 1;
        let last = self.last_ms.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < self.interval_ms {
            return false;
        }
        self.last_ms
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_window_reports_rate_over_window() {
        let window = SpeedWindow::new();
        std::thread::sleep(Duration::from_millis(120));
        let speed = window.sample(1_200_000);
        // ~1.2MB over ~0.12s ≈ 10MB/s; allow generous slack for CI timing.
        assert!(
            speed > 1_000_000.0,
            "speed should be in MB/s range, got {speed}"
        );
    }

    #[test]
    fn speed_window_ignores_resumed_baseline() {
        let window = SpeedWindow::with_baseline(50_000_000);
        std::thread::sleep(Duration::from_millis(120));
        let speed = window.sample(50_100_000);
        // Only the 100KB delta counts, not the 50MB baseline.
        assert!(
            speed < 5_000_000.0,
            "baseline must not inflate speed, got {speed}"
        );
    }

    #[test]
    fn throttle_gate_first_call_passes_then_blocks() {
        let gate = ThrottleGate::new(Duration::from_millis(500));
        assert!(gate.try_pass());
        assert!(!gate.try_pass());
    }
}
