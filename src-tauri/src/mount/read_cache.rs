//! Chunked read cache for a mounted bucket.
//!
//! An NFS client reads a file in transfers of at most 128 KiB (macOS) or 1 MiB
//! (Linux), and without a cache every one of those becomes its own ranged
//! `GetObject` — a full S3 round trip per transfer, which caps a sequential
//! read at a few megabytes per second no matter how fast the link is. Reads
//! are instead served from fixed-size chunks fetched once each: one chunk
//! covers dozens of client transfers, concurrent transfers into the same chunk
//! share a single fetch, and the filesystem prefetches ahead of a reader that
//! has proven sequential so the next chunk is already arriving while the
//! current one drains.
//!
//! The cache holds decoded bytes in memory, capped at [`MAX_CACHE_BYTES`] per
//! mount with least-recently-used eviction, and a chunk is only served for
//! [`CHUNK_TTL`] before it is re-fetched — the bucket can change underneath a
//! mount (the app's own uploads, another machine), and before this cache
//! existed every read saw those changes on the next round trip. Chunks are
//! keyed by NFS file id, not by object key, so a rename keeps its cache and
//! two files with one name over time never share bytes. Staged (locally
//! modified) files bypass this cache entirely — the staging file is newer than
//! anything in the bucket — and every mount-local path that changes an
//! object's content in the bucket drops the file's chunks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::OnceCell;

/// Bytes fetched per chunk. Large enough to amortize the per-request round
/// trip across dozens of client transfers, small enough that a random peek
/// into a big file does not cost tens of megabytes.
pub const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

/// Ceiling on cached bytes per mount before least-recently-used chunks go.
/// Deliberately modest — every active mount owns one of these caches, and a
/// desktop app cannot let three mounted buckets claim a gigabyte of heap.
pub const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// How long a fetched chunk may be served before it is fetched again,
/// matching the directory listing TTL so file content is never staler than
/// the listing that surrounds it. Without this, an object replaced from
/// outside the mount — the app's own upload pane, another machine — would
/// read back its old bytes forever.
pub const CHUNK_TTL: Duration = Duration::from_secs(30);

/// Chunks fetched ahead of a sequential reader.
pub const PREFETCH_CHUNKS: u64 = 2;

/// Prefetches in flight at once for one mount, demand reads not included.
pub const PREFETCH_CONCURRENCY: usize = 2;

/// Chunk index holding byte `offset`.
pub fn chunk_index(offset: u64) -> u64 {
    offset / CHUNK_SIZE
}

/// First byte of chunk `index`.
pub fn chunk_start(index: u64) -> u64 {
    index * CHUNK_SIZE
}

/// Length of chunk `index` of an object `object_size` bytes long — the final
/// chunk is short, and a chunk past the end is empty.
pub fn chunk_len(index: u64, object_size: u64) -> u64 {
    object_size
        .saturating_sub(chunk_start(index))
        .min(CHUNK_SIZE)
}

/// Inclusive chunk range a read of `[offset, end)` touches.
///
/// Callers guarantee `offset < end` (empty reads are answered before the cache
/// is involved). A read is at most 1 MiB, so the range spans at most two
/// chunks.
pub fn chunks_covering(offset: u64, end: u64) -> (u64, u64) {
    (chunk_index(offset), chunk_index(end.saturating_sub(1)))
}

/// Whether a read that just finished in `current` justifies prefetching the
/// chunks after it.
///
/// Only a reader that has *proven* sequential earns read-ahead: one that just
/// crossed a chunk boundary from the previous chunk, or one that has consumed
/// deep into the chunk it started in. A cold first touch — Finder generating
/// a preview, a HEAD-like peek — prefetches nothing, because on a metered
/// backend speculative megabytes for a 128 KiB read are pure cost.
pub fn should_prefetch(previous: Option<u64>, current: u64, bytes_into_chunk: u64) -> bool {
    match previous {
        Some(previous) if current == previous.wrapping_add(1) => true,
        Some(previous) if current == previous => bytes_into_chunk * 2 >= CHUNK_SIZE,
        _ => false,
    }
}

/// One cached chunk. The cell starts empty and is filled by whichever reader
/// gets there first; everyone else awaits the same fetch instead of issuing
/// their own.
pub struct Chunk {
    pub cell: OnceCell<Arc<Vec<u8>>>,
    /// When the fetch that fills this chunk began, for [`CHUNK_TTL`].
    fetched_at: Instant,
    /// Filled byte count, recorded under the cache lock by `note_filled` so
    /// every removal path frees exactly what was accounted — `cell.get()`
    /// would still be `None` in the window between accounting and the cell's
    /// own store.
    len: AtomicU64,
    /// Cache tick of the last touch, for least-recently-used eviction.
    last_access: AtomicU64,
}

/// Per-mount chunk registry with byte accounting.
///
/// The mutex only ever guards map bookkeeping — never a fetch — so it is a
/// plain `std::sync::Mutex` that no task holds across an await.
pub struct ReadCache {
    inner: Mutex<CacheInner>,
}

struct CacheInner {
    chunks: HashMap<(u64, u64), Arc<Chunk>>,
    /// Chunk index of the last read served per file, for the sequential-read
    /// detection behind [`should_prefetch`].
    last_read: HashMap<u64, u64>,
    /// Bytes held by accounted (filled) chunks.
    total_bytes: u64,
    tick: u64,
}

impl ReadCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                chunks: HashMap::new(),
                last_read: HashMap::new(),
                total_bytes: 0,
                tick: 0,
            }),
        }
    }

    /// The cache state, poison-proof: a panic elsewhere must degrade reads,
    /// not turn every later read into another panic. The guarded state has no
    /// invariant a partial update could break — the worst case is a byte
    /// count that eviction re-converges.
    fn lock(&self) -> MutexGuard<'_, CacheInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The slot for `(file, index)`, created empty on first sight and
    /// re-created when the existing chunk has outlived [`CHUNK_TTL`].
    /// Touching a slot marks it most recently used.
    pub fn slot(&self, file: u64, index: u64) -> Arc<Chunk> {
        self.slot_at(file, index, Instant::now())
    }

    /// [`Self::slot`] against an explicit clock, so the TTL boundary is
    /// testable without waiting it out.
    fn slot_at(&self, file: u64, index: u64, now: Instant) -> Arc<Chunk> {
        let mut inner = self.lock();
        inner.tick += 1;
        let tick = inner.tick;

        if let Some(existing) = inner.chunks.get(&(file, index)) {
            // Only a *filled* chunk expires: an in-flight fetch is as fresh
            // as a replacement would be.
            if existing.cell.initialized()
                && now.saturating_duration_since(existing.fetched_at) >= CHUNK_TTL
            {
                let freed = existing.len.load(Ordering::Relaxed);
                inner.chunks.remove(&(file, index));
                inner.total_bytes = inner.total_bytes.saturating_sub(freed);
            }
        }

        let slot = inner
            .chunks
            .entry((file, index))
            .or_insert_with(|| {
                Arc::new(Chunk {
                    cell: OnceCell::new(),
                    fetched_at: Instant::now(),
                    len: AtomicU64::new(0),
                    last_access: AtomicU64::new(0),
                })
            })
            .clone();
        slot.last_access.store(tick, Ordering::Relaxed);
        slot
    }

    /// Whether a live (unexpired) slot exists — filled or mid-fetch — without
    /// creating one. Prefetchers use this so probing costs nothing.
    pub fn is_present(&self, file: u64, index: u64) -> bool {
        let inner = self.lock();
        match inner.chunks.get(&(file, index)) {
            Some(slot) => !slot.cell.initialized() || slot.fetched_at.elapsed() < CHUNK_TTL,
            None => false,
        }
    }

    /// Records the chunk a read just finished in and answers whether the
    /// chunks after it deserve prefetching. See [`should_prefetch`].
    pub fn note_read(&self, file: u64, current: u64, bytes_into_chunk: u64) -> bool {
        let mut inner = self.lock();
        let previous = inner.last_read.insert(file, current);
        should_prefetch(previous, current, bytes_into_chunk)
    }

    /// Records that `slot`'s cell was just filled with `len` bytes, then
    /// evicts least-recently-used chunks until the cache fits its cap again.
    ///
    /// Called from inside the cell's init closure, so it runs exactly once per
    /// fill. The bytes only count when `slot` is still the one in the map: an
    /// invalidation that raced the fetch already dropped it, and accounting an
    /// orphan would inflate the total for the life of the mount. Eviction only
    /// drops map entries: a reader holding the chunk's `Arc` keeps the bytes
    /// alive until it is done with them.
    pub fn note_filled(&self, file: u64, index: u64, slot: &Arc<Chunk>, len: u64) {
        let mut inner = self.lock();
        // Recorded under the lock so every removal path sees the same number
        // this fill adds to the total.
        slot.len.store(len, Ordering::Relaxed);
        let still_mapped = inner
            .chunks
            .get(&(file, index))
            .map(|mapped| Arc::ptr_eq(mapped, slot))
            .unwrap_or(false);
        if still_mapped {
            inner.total_bytes = inner.total_bytes.saturating_add(len);
        }

        while inner.total_bytes > MAX_CACHE_BYTES {
            let oldest = inner
                .chunks
                .iter()
                .filter(|(key, slot)| **key != (file, index) && slot.cell.initialized())
                .min_by_key(|(_, slot)| slot.last_access.load(Ordering::Relaxed))
                .map(|(key, _)| *key);
            let Some(key) = oldest else {
                break;
            };
            if let Some(slot) = inner.chunks.remove(&key) {
                let freed = slot.len.load(Ordering::Relaxed);
                inner.total_bytes = inner.total_bytes.saturating_sub(freed);
            }
        }
    }

    /// Drops one slot, used when its fetch failed (so the next read retries)
    /// or when its content proved stale.
    pub fn remove_slot(&self, file: u64, index: u64) {
        let mut inner = self.lock();
        if let Some(slot) = inner.chunks.remove(&(file, index)) {
            let freed = slot.len.load(Ordering::Relaxed);
            inner.total_bytes = inner.total_bytes.saturating_sub(freed);
        }
    }

    /// Drops every chunk of one file, used whenever the object's content in
    /// the bucket is replaced — a finished upload, a delete, a truncate — so a
    /// later read cannot see the old bytes.
    pub fn forget_file(&self, file: u64) {
        let mut inner = self.lock();
        let doomed: Vec<(u64, u64)> = inner
            .chunks
            .keys()
            .filter(|(f, _)| *f == file)
            .copied()
            .collect();
        for key in doomed {
            if let Some(slot) = inner.chunks.remove(&key) {
                let freed = slot.len.load(Ordering::Relaxed);
                inner.total_bytes = inner.total_bytes.saturating_sub(freed);
            }
        }
        inner.last_read.remove(&file);
    }

    #[cfg(test)]
    fn cached_bytes(&self) -> u64 {
        self.lock().total_bytes
    }

    #[cfg(test)]
    fn chunk_count(&self) -> usize {
        self.lock().chunks.len()
    }
}

impl Default for ReadCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- chunk arithmetic ----

    #[test]
    fn offsets_map_into_four_mebibyte_chunks() {
        assert_eq!(chunk_index(0), 0);
        assert_eq!(chunk_index(CHUNK_SIZE - 1), 0);
        assert_eq!(chunk_index(CHUNK_SIZE), 1);
        assert_eq!(chunk_start(3), 3 * CHUNK_SIZE);
    }

    #[test]
    fn the_final_chunk_is_short_and_chunks_past_the_end_are_empty() {
        let size = CHUNK_SIZE * 2 + 100;
        assert_eq!(chunk_len(0, size), CHUNK_SIZE);
        assert_eq!(chunk_len(1, size), CHUNK_SIZE);
        assert_eq!(chunk_len(2, size), 100);
        assert_eq!(chunk_len(3, size), 0);
        assert_eq!(chunk_len(0, 0), 0);
    }

    #[test]
    fn chunks_cover_every_byte_of_the_object_exactly_once() {
        for size in [1, CHUNK_SIZE - 1, CHUNK_SIZE, CHUNK_SIZE * 3 + 7] {
            let mut covered = 0;
            for index in 0.. {
                let len = chunk_len(index, size);
                if len == 0 {
                    break;
                }
                assert_eq!(chunk_start(index), covered);
                covered += len;
            }
            assert_eq!(covered, size);
        }
    }

    #[test]
    fn a_read_touches_the_chunks_its_bytes_live_in() {
        // Entirely inside the first chunk.
        assert_eq!(chunks_covering(0, 131072), (0, 0));
        // Ending exactly on a boundary stays in the earlier chunk.
        assert_eq!(chunks_covering(CHUNK_SIZE - 131072, CHUNK_SIZE), (0, 0));
        // Straddling a boundary needs both sides.
        assert_eq!(chunks_covering(CHUNK_SIZE - 1, CHUNK_SIZE + 1), (0, 1));
        assert_eq!(chunks_covering(CHUNK_SIZE, CHUNK_SIZE + 1), (1, 1));
    }

    // ---- prefetch policy ----

    #[test]
    fn a_cold_first_touch_earns_no_prefetch() {
        // Finder preview, `file` magic sniff, a HEAD-like peek: one read into
        // a file the cache has never seen must not cost speculative chunks.
        assert!(!should_prefetch(None, 0, 131072));
        assert!(!should_prefetch(None, 5, CHUNK_SIZE));
    }

    #[test]
    fn crossing_a_chunk_boundary_sequentially_earns_prefetch() {
        assert!(should_prefetch(Some(0), 1, 1));
        assert!(should_prefetch(Some(6), 7, 131072));
    }

    #[test]
    fn reading_deep_into_the_same_chunk_earns_prefetch() {
        assert!(!should_prefetch(Some(0), 0, CHUNK_SIZE / 2 - 1));
        assert!(should_prefetch(Some(0), 0, CHUNK_SIZE / 2));
        assert!(should_prefetch(Some(3), 3, CHUNK_SIZE));
    }

    #[test]
    fn a_random_seek_earns_no_prefetch() {
        // A media player jumping around, or two readers interleaved.
        assert!(!should_prefetch(Some(50), 10, CHUNK_SIZE));
        assert!(!should_prefetch(Some(0), 2, CHUNK_SIZE));
        // Reading backwards is not sequential either.
        assert!(!should_prefetch(Some(5), 4, CHUNK_SIZE));
    }

    #[test]
    fn note_read_remembers_the_chunk_per_file() {
        let cache = ReadCache::new();
        // First touch of each file: no prefetch.
        assert!(!cache.note_read(1, 0, CHUNK_SIZE));
        assert!(!cache.note_read(2, 0, 100));
        // File 1 crosses into chunk 1 → sequential.
        assert!(cache.note_read(1, 1, 100));
        // File 2's history is its own: a jump stays cold.
        assert!(!cache.note_read(2, 9, CHUNK_SIZE));
        // Forgetting the file clears its read history too.
        cache.forget_file(1);
        assert!(!cache.note_read(1, 2, CHUNK_SIZE));
    }

    // ---- slot lifecycle ----

    fn filled_slot(cache: &ReadCache, file: u64, index: u64, len: usize) {
        let slot = cache.slot(file, index);
        slot.cell.set(Arc::new(vec![0u8; len])).expect("fresh cell");
        cache.note_filled(file, index, &slot, len as u64);
    }

    #[test]
    fn the_same_slot_is_returned_while_a_fetch_is_in_flight() {
        let cache = ReadCache::new();
        let first = cache.slot(1, 0);
        let second = cache.slot(1, 0);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(cache.chunk_count(), 1);
    }

    #[test]
    fn presence_probes_do_not_create_slots() {
        let cache = ReadCache::new();
        assert!(!cache.is_present(1, 0));
        assert_eq!(cache.chunk_count(), 0);
        cache.slot(1, 0);
        assert!(cache.is_present(1, 0));
    }

    #[test]
    fn filling_a_chunk_is_accounted_and_forgetting_the_file_releases_it() {
        let cache = ReadCache::new();
        filled_slot(&cache, 1, 0, 100);
        filled_slot(&cache, 1, 1, 50);
        filled_slot(&cache, 2, 0, 25);
        assert_eq!(cache.cached_bytes(), 175);

        cache.forget_file(1);
        assert_eq!(cache.cached_bytes(), 25);
        assert!(!cache.is_present(1, 0));
        assert!(!cache.is_present(1, 1));
        assert!(cache.is_present(2, 0));
    }

    #[test]
    fn a_failed_fetch_leaves_no_slot_behind() {
        let cache = ReadCache::new();
        cache.slot(1, 0);
        cache.remove_slot(1, 0);
        assert!(!cache.is_present(1, 0));
        assert_eq!(cache.cached_bytes(), 0);
    }

    #[test]
    fn the_cache_evicts_its_least_recently_used_chunks_first() {
        let cache = ReadCache::new();
        let big = MAX_CACHE_BYTES / 2;
        filled_slot(&cache, 1, 0, big as usize); // oldest
        filled_slot(&cache, 1, 1, big as usize);
        // Touch the oldest so the middle chunk becomes the eviction victim.
        cache.slot(1, 0);

        filled_slot(&cache, 1, 2, big as usize); // pushes past the cap
        assert!(
            cache.is_present(1, 0),
            "the recently touched chunk survives"
        );
        assert!(!cache.is_present(1, 1), "the stale chunk is the one to go");
        assert!(
            cache.is_present(1, 2),
            "the chunk just filled is never evicted"
        );
        assert!(cache.cached_bytes() <= MAX_CACHE_BYTES);
    }

    #[test]
    fn an_in_flight_chunk_is_never_evicted() {
        let cache = ReadCache::new();
        cache.slot(7, 0); // empty cell: a fetch is in flight
        filled_slot(&cache, 1, 0, MAX_CACHE_BYTES as usize);
        filled_slot(&cache, 1, 1, 100);
        // The in-flight slot survives even though the cache is over its cap.
        assert!(cache.is_present(7, 0));
    }

    #[test]
    fn a_fill_that_lost_a_race_with_invalidation_is_not_accounted() {
        let cache = ReadCache::new();
        let orphan = cache.slot(1, 0);
        // The file was invalidated while the fetch was in flight, and a later
        // read already opened a fresh slot for the same chunk.
        cache.forget_file(1);
        let fresh = cache.slot(1, 0);
        assert!(!Arc::ptr_eq(&orphan, &fresh));

        orphan.cell.set(Arc::new(vec![0u8; 64])).expect("cell");
        cache.note_filled(1, 0, &orphan, 64);
        assert_eq!(
            cache.cached_bytes(),
            0,
            "an orphaned fill must not inflate the total"
        );
    }

    #[test]
    fn eviction_frees_what_accounting_recorded_even_before_the_cell_stores() {
        // The narrow window between note_filled and the cell's own store: a
        // removal in that window must free the same bytes the fill added, or
        // the total drifts upward for the life of the mount.
        let cache = ReadCache::new();
        let slot = cache.slot(1, 0);
        cache.note_filled(1, 0, &slot, 4096); // cell deliberately never set
        assert_eq!(cache.cached_bytes(), 4096);
        cache.remove_slot(1, 0);
        assert_eq!(cache.cached_bytes(), 0);
    }

    #[test]
    fn eviction_only_counts_filled_chunks() {
        let cache = ReadCache::new();
        for index in 0..10 {
            cache.slot(1, index);
        }
        assert_eq!(cache.cached_bytes(), 0, "empty cells hold no bytes");
    }

    #[test]
    fn an_expired_chunk_is_replaced_rather_than_served() {
        let cache = ReadCache::new();
        filled_slot(&cache, 1, 0, 100);

        // Still fresh: the same filled slot comes back.
        let fresh = cache.slot_at(1, 0, Instant::now());
        assert!(fresh.cell.initialized());
        assert_eq!(cache.cached_bytes(), 100);

        // Past the TTL: the slot is replaced by an empty one — the next read
        // fetches current bytes — and the stale bytes leave the accounting.
        let replaced = cache.slot_at(1, 0, Instant::now() + CHUNK_TTL);
        assert!(!replaced.cell.initialized(), "stale content must not serve");
        assert!(!Arc::ptr_eq(&fresh, &replaced));
        assert_eq!(cache.cached_bytes(), 0);
    }

    #[test]
    fn an_in_flight_fetch_never_expires() {
        let cache = ReadCache::new();
        let inflight = cache.slot(1, 0); // cell stays empty
        let same = cache.slot_at(1, 0, Instant::now() + CHUNK_TTL * 2);
        assert!(
            Arc::ptr_eq(&inflight, &same),
            "an unfilled slot is as fresh as its replacement would be"
        );
    }
}
