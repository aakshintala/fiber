//! Per-thread allocation counters for the `alloc-stats` build
//! (`docs/performance.md`, "Measuring"): bytes allocated, allocation count,
//! and live bytes on the calling thread. The counters are cumulative totals
//! with saturating arithmetic, read through [`snapshot`]; a timer
//! differences two snapshots to attribute one operation's allocations to the
//! thread that ran it.
//!
//! Recording is per thread by mechanism: a `thread_local!` cell plus
//! `try_with` on the calling thread, so no handle to another thread's
//! counters ever exists. A block allocated on one thread and freed on
//! another lowers the freeing thread's live total; the isolation test pins
//! only that one thread's explicit records never move another thread's
//! totals. A `realloc` counts one allocation of the new size and adjusts
//! live by the signed size difference.

use std::cell::Cell;

/// One thread's cumulative totals. Plain integers only: the cell needs no
/// destructor, never allocates and never panics.
#[derive(Debug, Default)]
struct Counters {
    /// Bytes allocated on this thread so far.
    alloc_bytes: Cell<u64>,
    /// Allocations made on this thread so far.
    allocs: Cell<u64>,
    /// Bytes allocated on this thread and not yet freed: live bytes,
    /// signed, negative when the thread freed more than it allocated.
    live_bytes: Cell<i64>,
}

impl Counters {
    const fn new() -> Self {
        Self {
            alloc_bytes: Cell::new(0),
            allocs: Cell::new(0),
            live_bytes: Cell::new(0),
        }
    }
}

thread_local! {
    static COUNTERS: Counters = const { Counters::new() };
}

/// Runs `change` on this thread's counters. Skipped while the thread is
/// being torn down, so it never panics.
fn update(change: impl FnOnce(&Counters)) {
    let _skipped = COUNTERS.try_with(change);
}

/// The calling thread's cumulative allocation totals: a plain copy the
/// caller differences against a later one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Bytes allocated on this thread so far.
    pub alloc_bytes: u64,
    /// Allocations made on this thread so far.
    pub allocs: u64,
    /// Bytes allocated on this thread and not yet freed: live bytes,
    /// signed, negative when the thread freed more than it allocated.
    pub live_bytes: i64,
}

/// The calling thread's totals now. During teardown the cell is gone and
/// the snapshot reads zero.
pub fn snapshot() -> Snapshot {
    let mut snapshot = Snapshot::default();
    let _skipped = COUNTERS.try_with(|counters| {
        snapshot = Snapshot {
            alloc_bytes: counters.alloc_bytes.get(),
            allocs: counters.allocs.get(),
            live_bytes: counters.live_bytes.get(),
        };
    });
    snapshot
}

/// Adds an allocation of `size` bytes to the calling thread's totals: the
/// full size joins bytes allocated and live bytes, and the count grows by
/// one.
pub fn record_alloc(size: usize) {
    let bytes = u64::try_from(size).unwrap_or(u64::MAX);
    let live = i64::try_from(size).unwrap_or(i64::MAX);
    update(|counters| {
        counters
            .alloc_bytes
            .set(counters.alloc_bytes.get().saturating_add(bytes));
        counters.allocs.set(counters.allocs.get().saturating_add(1));
        counters
            .live_bytes
            .set(counters.live_bytes.get().saturating_add(live));
    });
}

/// Subtracts a free of `size` bytes from the calling thread's live bytes.
/// The cumulative allocation totals do not move.
pub fn record_dealloc(size: usize) {
    let live = i64::try_from(size).unwrap_or(i64::MAX);
    update(|counters| {
        counters
            .live_bytes
            .set(counters.live_bytes.get().saturating_sub(live));
    });
}

/// Adds a `realloc` from `old_size` to `new_size` bytes to the calling
/// thread's totals: one allocation of the new size joins bytes allocated
/// and the count, and live bytes move by the signed size difference.
pub fn record_realloc(old_size: usize, new_size: usize) {
    let bytes = u64::try_from(new_size).unwrap_or(u64::MAX);
    let delta = i64::try_from(new_size)
        .unwrap_or(i64::MAX)
        .saturating_sub(i64::try_from(old_size).unwrap_or(i64::MAX));
    update(|counters| {
        counters
            .alloc_bytes
            .set(counters.alloc_bytes.get().saturating_add(bytes));
        counters.allocs.set(counters.allocs.get().saturating_add(1));
        counters
            .live_bytes
            .set(counters.live_bytes.get().saturating_add(delta));
    });
}
