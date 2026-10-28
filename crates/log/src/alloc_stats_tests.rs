//! The per-thread allocation counters (`super::alloc_stats`): the
//! isolation test below. The test-only mirror forwards every call to
//! [`System`] and records through the production API; the test calls it
//! explicitly and never installs it as a global allocator, so it cannot
//! conflict with other test allocators.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;

use super::alloc_stats::{self, Snapshot};

/// The wall-clock limit on the channel receive below.
const LIMIT: Duration = Duration::from_secs(30);

/// A test-only counting allocator: every call reaches [`System`] with the
/// caller's arguments, and the size joins the calling thread's production
/// totals.
struct Mirror;

#[allow(
    unsafe_code,
    reason = "a test-only GlobalAlloc mirror, and forwarding to System calls its unsafe methods"
)]
// SAFETY: every call forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees are the caller's. The recording
// beside it writes only plain integers in a thread-local that needs no
// destructor, never allocates and never panics.
unsafe impl GlobalAlloc for Mirror {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `layout` is passed on as `alloc` requires.
        let block = unsafe { System.alloc(layout) };
        if !block.is_null() {
            alloc_stats::record_alloc(layout.size());
        }
        block
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `layout` is passed on as `alloc_zeroed`
        // requires.
        let block = unsafe { System.alloc_zeroed(layout) };
        if !block.is_null() {
            alloc_stats::record_alloc(layout.size());
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        alloc_stats::record_dealloc(layout.size());
        // SAFETY: `block` was allocated through this mirror with `layout`,
        // as the caller guarantees.
        unsafe { System.dealloc(block, layout) }
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `block`, `layout` and `new_size` are the caller's, passed
        // on as `realloc` requires.
        let moved = unsafe { System.realloc(block, layout, new_size) };
        if !moved.is_null() {
            alloc_stats::record_realloc(layout.size(), new_size);
        }
        moved
    }
}

/// One thread's explicit mirror records stay on that thread: a child's
/// allocation moves only the child's totals by its size, freeing it moves
/// only the child's live total below its start, and the parent's totals
/// never move.
#[test]
#[allow(
    unsafe_code,
    reason = "the test drives its GlobalAlloc mirror by hand, never installing it"
)]
fn per_thread_counters_stay_on_their_thread() {
    const SIZE: usize = 64;
    let layout = Layout::from_size_align(SIZE, 8).unwrap();
    let parent_before = alloc_stats::snapshot();
    let (done_tx, done_rx) = mpsc::channel::<(Snapshot, Snapshot, Snapshot)>();
    let child = thread::Builder::new()
        .name("log-test-alloc".to_owned())
        .spawn(move || {
            let mirror = Mirror;
            let before_alloc = alloc_stats::snapshot();
            // SAFETY: `layout` came from `from_size_align`, so it is valid
            // for `alloc`, and the block is checked before use.
            let block = unsafe { mirror.alloc(layout) };
            assert!(!block.is_null());
            let before_free = alloc_stats::snapshot();
            // SAFETY: `block` came from this mirror's `alloc` with the
            // same `layout`, and is live until this free.
            unsafe { mirror.dealloc(block, layout) };
            let after_free = alloc_stats::snapshot();
            done_tx
                .send((before_alloc, before_free, after_free))
                .unwrap();
        })
        .unwrap();
    let (before_alloc, before_free, after_free) =
        Deadline::after(LIMIT).recv_or_fail(&done_rx, "child allocation report");
    child.join().unwrap();
    let size_u64 = u64::try_from(SIZE).unwrap();
    assert_eq!(before_free.alloc_bytes - before_alloc.alloc_bytes, size_u64);
    assert_eq!(before_free.allocs - before_alloc.allocs, 1);
    assert_eq!(
        after_free.live_bytes - before_free.live_bytes,
        -i64::try_from(SIZE).unwrap()
    );
    assert_eq!(alloc_stats::snapshot(), parent_before);
}

/// A `realloc` from a smaller to a larger size counts one allocation of
/// the new size and moves live bytes by the signed size difference: the
/// bytes delta is the new size, the count delta is one, and the live
/// delta is new minus old.
#[test]
#[allow(
    unsafe_code,
    reason = "the test drives its GlobalAlloc mirror by hand, never installing it"
)]
fn realloc_from_smaller_to_larger_counts_new_size() {
    const OLD_SIZE: usize = 32;
    const NEW_SIZE: usize = 96;
    let old_layout = Layout::from_size_align(OLD_SIZE, 8).unwrap();
    let new_layout = Layout::from_size_align(NEW_SIZE, 8).unwrap();
    let mirror = Mirror;
    // SAFETY: `old_layout` came from `from_size_align`, so it is valid
    // for `alloc`, and the block is checked before use.
    let block = unsafe { mirror.alloc(old_layout) };
    assert!(!block.is_null());
    let before = alloc_stats::snapshot();
    // SAFETY: `block` came from this mirror's `alloc` with `old_layout`
    // and is live, so it is valid for `realloc` with the larger size.
    let moved = unsafe { mirror.realloc(block, old_layout, NEW_SIZE) };
    assert!(!moved.is_null());
    let after = alloc_stats::snapshot();
    assert_eq!(
        after.alloc_bytes - before.alloc_bytes,
        u64::try_from(NEW_SIZE).unwrap()
    );
    assert_eq!(after.allocs - before.allocs, 1);
    assert_eq!(
        after.live_bytes - before.live_bytes,
        i64::try_from(NEW_SIZE).unwrap() - i64::try_from(OLD_SIZE).unwrap()
    );
    // SAFETY: `moved` came from this mirror's `realloc` with the new
    // size, and is live until this free.
    unsafe { mirror.dealloc(moved, new_layout) };
}
