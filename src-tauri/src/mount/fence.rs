use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FencePath {
    key: String,
    prefix: bool,
}

impl FencePath {
    pub(super) fn exact(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            prefix: false,
        }
    }

    pub(super) fn prefix(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            prefix: true,
        }
    }

    fn overlaps(&self, other: &Self) -> bool {
        if self.prefix && other.prefix {
            return self.key.starts_with(&other.key) || other.key.starts_with(&self.key);
        }
        if self.prefix {
            return other.key == self.key || other.key.starts_with(&self.key);
        }
        if other.prefix {
            return self.key == other.key || self.key.starts_with(&other.key);
        }
        self.key == other.key
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ActiveFence {
    paths: Vec<FencePath>,
    shared: bool,
}

#[derive(Default)]
struct FenceState {
    active: Vec<ActiveFence>,
}

#[derive(Default, Debug, Clone, Serialize)]
pub(crate) struct FenceMetrics {
    pub(crate) waits: u64,
    pub(crate) wait_ms: u64,
    pub(crate) hold_ms: u64,
}

#[derive(Default)]
pub(super) struct NamespaceFences {
    state: Mutex<FenceState>,
    metrics: Mutex<FenceMetrics>,
    changed: Notify,
}

pub(super) struct FenceGuard {
    fences: Arc<NamespaceFences>,
    paths: Vec<FencePath>,
    shared: bool,
    acquired_at: Instant,
}

impl NamespaceFences {
    pub(super) async fn acquire(self: &Arc<Self>, paths: Vec<FencePath>) -> FenceGuard {
        self.acquire_inner(paths, false).await
    }

    pub(super) async fn acquire_shared(self: &Arc<Self>, paths: Vec<FencePath>) -> FenceGuard {
        self.acquire_inner(paths, true).await
    }

    async fn acquire_inner(
        self: &Arc<Self>,
        mut paths: Vec<FencePath>,
        shared: bool,
    ) -> FenceGuard {
        paths.sort_by(|a, b| a.key.cmp(&b.key).then(a.prefix.cmp(&b.prefix)));
        paths.dedup();
        let started = Instant::now();
        loop {
            let notified = self.changed.notified();
            let acquired = {
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                if state.active.iter().any(|held| {
                    !(held.shared && shared)
                        && held
                            .paths
                            .iter()
                            .any(|held| paths.iter().any(|path| held.overlaps(path)))
                }) {
                    false
                } else {
                    state.active.push(ActiveFence {
                        paths: paths.clone(),
                        shared,
                    });
                    true
                }
            };
            if acquired {
                let waited = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                if waited > 0 {
                    let mut metrics = self
                        .metrics
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    metrics.waits = metrics.waits.saturating_add(1);
                    metrics.wait_ms = metrics.wait_ms.saturating_add(waited);
                }
                return FenceGuard {
                    fences: self.clone(),
                    paths,
                    shared,
                    acquired_at: Instant::now(),
                };
            }
            notified.await;
        }
    }

    pub(super) fn metrics(&self) -> FenceMetrics {
        let metrics = self
            .metrics
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        FenceMetrics {
            waits: metrics.waits,
            wait_ms: metrics.wait_ms,
            hold_ms: metrics.hold_ms,
        }
    }
}

impl Drop for FenceGuard {
    fn drop(&mut self) {
        let held = self.acquired_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
        {
            let mut state = self
                .fences
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(index) = state
                .active
                .iter()
                .position(|held| held.paths == self.paths && held.shared == self.shared)
            {
                state.active.remove(index);
            }
        }
        {
            let mut metrics = self
                .fences
                .metrics
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            metrics.hold_ms = metrics.hold_ms.saturating_add(held);
        }
        self.fences.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn unrelated_subtree_proceeds_while_prefix_is_fenced() {
        let fences = Arc::new(NamespaceFences::default());
        let _held = fences.acquire(vec![FencePath::prefix("a/")]).await;

        let b = fences
            .clone()
            .acquire(vec![FencePath::exact("b/file")])
            .await;
        drop(b);

        let waiting = Arc::new(AtomicBool::new(false));
        let waiter = tokio::spawn({
            let fences = fences.clone();
            let waiting = waiting.clone();
            async move {
                waiting.store(true, Ordering::SeqCst);
                fences.acquire(vec![FencePath::exact("a/file")]).await
            }
        });
        while !waiting.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(25), waiter)
                .await
                .is_err(),
            "overlapping child must wait behind prefix publication fence"
        );
    }

    #[tokio::test]
    async fn same_key_shared_leases_do_not_block_each_other() {
        let fences = Arc::new(NamespaceFences::default());
        let _first = fences.acquire_shared(vec![FencePath::exact("same")]).await;
        let second_fences = fences.clone();
        let second = second_fences.acquire_shared(vec![FencePath::exact("same")]);
        tokio::time::timeout(Duration::from_millis(25), second)
            .await
            .expect("same-key data IO leases should be shared");
    }

    #[tokio::test]
    async fn sorted_multi_path_acquire_does_not_deadlock_opposing_renames() {
        let fences = Arc::new(NamespaceFences::default());
        let first = tokio::spawn({
            let fences = fences.clone();
            async move {
                let _guard = fences
                    .acquire(vec![FencePath::prefix("a/"), FencePath::prefix("b/")])
                    .await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        let second = tokio::spawn({
            let fences = fences.clone();
            async move {
                let _guard = fences
                    .acquire(vec![FencePath::prefix("b/"), FencePath::prefix("a/")])
                    .await;
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            first.await.unwrap();
            second.await.unwrap();
        })
        .await
        .expect("opposing multi-prefix acquires must serialize, not deadlock");
        assert!(fences.metrics().waits >= 1);
    }
}
