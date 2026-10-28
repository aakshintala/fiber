//! The `alloc-stats` build's global allocator (`docs/performance.md`,
//! "Measuring"): [`Counting`] forwards every call to [`System`] and feeds
//! the calling thread's `log` counters, so an `op_timing` line can carry
//! that operation's allocation fields. The module compiles only in the
//! `alloc-stats` build; a default build names no allocator here.

use std::alloc::{GlobalAlloc, Layout, System};

/// The counting global allocator for the `alloc-stats` build: every call
/// reaches [`System`] with the caller's arguments, and the size joins the
/// calling thread's `log` totals.
pub(crate) struct Counting;

#[allow(
    unsafe_code,
    reason = "a global allocator is an unsafe trait, and forwarding to System calls its unsafe methods"
)]
// SAFETY: every call forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees are the caller's. The recording
// beside it writes only plain integers in a thread-local that needs no
// destructor, never allocates and never panics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `layout` is passed on as `alloc` requires.
        let block = unsafe { System.alloc(layout) };
        if !block.is_null() {
            log::alloc_stats::record_alloc(layout.size());
        }
        block
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `layout` is passed on as `alloc_zeroed`
        // requires.
        let block = unsafe { System.alloc_zeroed(layout) };
        if !block.is_null() {
            log::alloc_stats::record_alloc(layout.size());
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        log::alloc_stats::record_dealloc(layout.size());
        // SAFETY: `block` was allocated by `System` through this allocator
        // with `layout`, as the caller guarantees.
        unsafe { System.dealloc(block, layout) }
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `block`, `layout` and `new_size` are the caller's, passed
        // on as `realloc` requires.
        let moved = unsafe { System.realloc(block, layout, new_size) };
        if !moved.is_null() {
            log::alloc_stats::record_realloc(layout.size(), new_size);
        }
        moved
    }
}
