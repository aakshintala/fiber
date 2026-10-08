//! A counting allocator for tests that measure how many large blocks a piece
//! of work holds at once. `fakes` declares no `#[global_allocator]`: a test
//! binary that measures declares
//! `static ALLOC: fakes::alloc::Counting = fakes::alloc::Counting;` itself,
//! so no other binary changes allocator.
//!
//! Counting is by address, per thread, while a scope is open on that thread.
//! A large block the thread allocates is recorded; freeing a recorded block
//! on that thread removes it; freeing any other block changes nothing. A
//! recorded block freed on another thread stays counted, so a count can be
//! too high but never too low. A block that `realloc` serves by copying is
//! one block: the copy is not seen.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::RefCell;

/// A block is large while its layout's size is at least this: 1 MiB.
pub const LARGE: usize = 1 << 20;

/// How many large blocks one thread's scopes can track alive at once.
const SLOTS: usize = 64;

/// Forwards every call to [`System`] and records large blocks for
/// [`large_blocks_during`].
pub struct Counting;

/// One thread's bookkeeping. It never allocates: a fixed table and counters.
struct Tracked {
    /// Open scopes on this thread.
    depth: usize,
    /// The recorded blocks' addresses; 0 is an empty slot.
    slots: [usize; SLOTS],
    /// How many slots are filled.
    live: usize,
    /// The most filled at once since the innermost open scope began.
    peak: usize,
    /// A block was not recorded because every slot was filled.
    overflowed: bool,
}

impl Tracked {
    const fn new() -> Self {
        Self {
            depth: 0,
            slots: [0; SLOTS],
            live: 0,
            peak: 0,
            overflowed: false,
        }
    }

    fn record(&mut self, address: usize) {
        if self.depth == 0 {
            return;
        }
        match self.slots.iter_mut().find(|slot| **slot == 0) {
            Some(slot) => {
                *slot = address;
                self.live += 1;
                self.peak = self.peak.max(self.live);
            }
            None => self.overflowed = true,
        }
    }

    /// Removes `address` if it is recorded, saying whether it was.
    fn remove(&mut self, address: usize) -> bool {
        match self.slots.iter_mut().find(|slot| **slot == address) {
            Some(slot) => {
                *slot = 0;
                self.live -= 1;
                true
            }
            None => false,
        }
    }
}

thread_local! {
    static TRACKED: RefCell<Tracked> = const { RefCell::new(Tracked::new()) };
}

/// Runs `change` on this thread's bookkeeping. Skipped while the thread is
/// being torn down or the bookkeeping is already borrowed, so it never
/// panics.
fn tracked(change: impl FnOnce(&mut Tracked)) {
    let _skipped = TRACKED.try_with(|cell| {
        if let Ok(mut tracked) = cell.try_borrow_mut() {
            change(&mut tracked);
        }
    });
}

fn is_large(size: usize) -> bool {
    size >= LARGE
}

#[allow(
    unsafe_code,
    reason = "a global allocator is an unsafe trait, and forwarding to System calls its unsafe methods"
)]
// SAFETY: every call forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees are the caller's. The bookkeeping
// beside it reads and writes only plain integers in a thread-local that
// needs no destructor, never allocates and never panics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `layout` is passed on as `alloc` requires.
        let block = unsafe { System.alloc(layout) };
        if !block.is_null() && is_large(layout.size()) {
            tracked(|tracked| tracked.record(block.addr()));
        }
        block
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `layout` is passed on as `alloc_zeroed`
        // requires.
        let block = unsafe { System.alloc_zeroed(layout) };
        if !block.is_null() && is_large(layout.size()) {
            tracked(|tracked| tracked.record(block.addr()));
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        if is_large(layout.size()) {
            tracked(|tracked| {
                tracked.remove(block.addr());
            });
        }
        // SAFETY: `block` was allocated by `System` through this allocator
        // with `layout`, as the caller guarantees.
        unsafe { System.dealloc(block, layout) }
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `block`, `layout` and `new_size` are the caller's, passed
        // on as `realloc` requires.
        let moved = unsafe { System.realloc(block, layout, new_size) };
        if moved.is_null() {
            return moved;
        }
        let (was, now) = (is_large(layout.size()), is_large(new_size));
        if was || now {
            tracked(|tracked| {
                let recorded = was && tracked.remove(block.addr());
                if now && (recorded || !was) {
                    tracked.record(moved.addr());
                }
            });
        }
        moved
    }
}

/// Runs `f` and returns its value with the most large blocks this thread
/// recorded alive at once while it ran. Scopes nest: an inner scope counts
/// the blocks alive when it began, and the outer's peak is at least the
/// inner's. Without [`Counting`] installed nothing is recorded and the
/// count is 0, so a test asserting a low count first asserts that one
/// large block counts 1.
///
/// # Panics
///
/// After `f` returns, when more large blocks were alive at once than the
/// table holds.
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
pub fn large_blocks_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let mut outer = 0;
    tracked(|tracked| {
        tracked.depth += 1;
        outer = tracked.peak;
        tracked.peak = tracked.live;
    });
    let mut scope = Scope {
        outer,
        closed: false,
    };
    let value = f();
    let (peak, overflowed) = scope.close();
    assert!(
        !overflowed,
        "more than {SLOTS} large blocks were alive at once; the count is too low"
    );
    (value, peak)
}

/// An open scope, closed when `f` returns or, if it unwinds, when dropped.
struct Scope {
    outer: usize,
    closed: bool,
}

impl Scope {
    /// Closes the scope, giving its peak and whether the table overflowed.
    fn close(&mut self) -> (usize, bool) {
        self.closed = true;
        let outer = self.outer;
        let mut closed = (0, false);
        tracked(|tracked| {
            let peak = tracked.peak;
            tracked.peak = outer.max(peak);
            tracked.depth = tracked.depth.saturating_sub(1);
            closed = (peak, tracked.overflowed);
            if tracked.depth == 0 {
                *tracked = Tracked::new();
            }
        });
        closed
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if !self.closed {
            self.close();
        }
    }
}
