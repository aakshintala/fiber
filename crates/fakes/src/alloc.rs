//! A counting allocator for tests that measure how many large blocks a piece
//! of work holds at once, and how many bytes it holds. `fakes` declares no
//! `#[global_allocator]`: a test binary that measures declares
//! `static ALLOC: fakes::alloc::Counting = fakes::alloc::Counting;` itself,
//! so no other binary changes allocator.
//!
//! Counting is by address, per thread, while a scope is open on that thread.
//! A large block the thread allocates is recorded; freeing a recorded block
//! on that thread removes it; freeing any other block changes nothing. A
//! recorded block freed on another thread stays counted, so a count can be
//! too high but never too low. A block that `realloc` serves by copying is
//! one block: the copy is not seen.
//!
//! Beside the large blocks, every allocation adds its size to a running
//! byte total for the thread, every free subtracts its size, and a
//! `realloc` adds the difference, all counted from the scope's start. The
//! peak never drops below 0: a block made before the scope and freed
//! inside it lowers the total. Each block of at least [`CHAIN`] also takes
//! a slot in a fixed table of 64, which follows the block through every
//! `realloc`; on every allocation event each slot records the peak of the
//! total without its own current size, so a test can read the peak without
//! the one output buffer it kept.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::RefCell;

/// A block is large while its layout's size is at least this: 1 MiB.
pub const LARGE: usize = 1 << 20;

/// A block is byte-tracked while its layout's size is at least this:
/// 64 KiB.
pub const CHAIN: usize = 64 << 10;

/// How many large blocks one thread's scopes can track alive at once.
const SLOTS: usize = 64;

/// Forwards every call to [`System`] and records large blocks for
/// [`large_blocks_during`].
pub struct Counting;

/// One thread's bookkeeping. It never allocates: fixed tables and counters.
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
    /// Bytes allocated on this thread and not yet freed, from the
    /// outermost scope's start; freeing a block made before the scope
    /// lowers it, even below 0.
    total: i64,
    /// The most bytes held at once since the innermost open scope began,
    /// never below 0.
    byte_peak: usize,
    /// The byte-tracked blocks' addresses, sizes and peaks-without.
    byte_slots: [ByteSlot; SLOTS],
    /// A block of at least [`CHAIN`] found no empty byte slot.
    byte_overflowed: bool,
}

/// One byte-tracked block: its address, its current size and the peak of
/// the total without that size at every allocation event since the block
/// took the slot. Plain integers only.
#[derive(Clone, Copy)]
struct ByteSlot {
    addr: usize,
    size: usize,
    peak_without: usize,
}

impl ByteSlot {
    const EMPTY: Self = Self {
        addr: 0,
        size: 0,
        peak_without: 0,
    };
}

impl Tracked {
    const fn new() -> Self {
        Self {
            depth: 0,
            slots: [0; SLOTS],
            live: 0,
            peak: 0,
            overflowed: false,
            total: 0,
            byte_peak: 0,
            byte_slots: [ByteSlot::EMPTY; SLOTS],
            byte_overflowed: false,
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

    /// Adds an allocation of `size` at `address` to the byte total,
    /// slotting the block while it is at least [`CHAIN`].
    fn byte_alloc(&mut self, size: usize, address: usize) {
        if self.depth == 0 {
            return;
        }
        self.total = add_size(self.total, size);
        if size >= CHAIN {
            match self.byte_slots.iter_mut().find(|slot| slot.addr == 0) {
                Some(slot) => {
                    // The peak before the slot existed is a peak without
                    // that slot.
                    let without = self.byte_peak;
                    *slot = ByteSlot {
                        addr: address,
                        size,
                        peak_without: without,
                    };
                }
                None => self.byte_overflowed = true,
            }
        }
        self.refresh_bytes();
    }

    /// Subtracts a free of `size` at `address` from the byte total,
    /// emptying the block's slot.
    fn byte_free(&mut self, address: usize, size: usize) {
        if self.depth == 0 {
            return;
        }
        self.total = self
            .total
            .saturating_sub(i64::try_from(size).unwrap_or(i64::MAX));
        if let Some(slot) = self.byte_slots.iter_mut().find(|slot| slot.addr == address) {
            *slot = ByteSlot::EMPTY;
        }
        self.refresh_bytes();
    }

    /// Adds a `realloc` from `old_size` at `old` to `new_size` at `new` to
    /// the byte total. A slotted block keeps its slot and its peak-without
    /// at its new address; a block that shrinks below [`CHAIN`] leaves
    /// its slot; a block that grows past it from below takes one. A block
    /// grown before the scope was never slotted and stays unslotted, as
    /// large blocks do.
    fn byte_realloc(&mut self, old: usize, old_size: usize, new: usize, new_size: usize) {
        if self.depth == 0 {
            return;
        }
        self.total = add_size(self.total, new_size)
            .saturating_sub(i64::try_from(old_size).unwrap_or(i64::MAX));
        if old_size >= CHAIN {
            if let Some(slot) = self.byte_slots.iter_mut().find(|slot| slot.addr == old) {
                if new_size >= CHAIN {
                    slot.addr = new;
                    slot.size = new_size;
                } else {
                    *slot = ByteSlot::EMPTY;
                }
            }
        } else if new_size >= CHAIN {
            match self.byte_slots.iter_mut().find(|slot| slot.addr == 0) {
                Some(slot) => {
                    // The peak before the slot existed is a peak without
                    // that slot.
                    let without = self.byte_peak;
                    *slot = ByteSlot {
                        addr: new,
                        size: new_size,
                        peak_without: without,
                    };
                }
                None => self.byte_overflowed = true,
            }
        }
        self.refresh_bytes();
    }

    /// Folds the current total into the byte peak and into every live
    /// slot's peak-without: at most 64 integer operations.
    fn refresh_bytes(&mut self) {
        self.byte_peak = self.byte_peak.max(peak_value(self.total));
        for slot in self.byte_slots.iter_mut() {
            if slot.addr != 0 {
                let without = peak_value(
                    self.total
                        .saturating_sub(i64::try_from(slot.size).unwrap_or(i64::MAX)),
                );
                slot.peak_without = slot.peak_without.max(without);
            }
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

/// Adds `size` bytes to a running total, saturating: no allocation is
/// near `i64::MAX`.
fn add_size(total: i64, size: usize) -> i64 {
    total.saturating_add(i64::try_from(size).unwrap_or(i64::MAX))
}

/// A running total as a peak value: below 0 counts as 0.
fn peak_value(total: i64) -> usize {
    usize::try_from(total.max(0)).unwrap_or(usize::MAX)
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
        if !block.is_null() {
            tracked(|tracked| {
                if is_large(layout.size()) {
                    tracked.record(block.addr());
                }
                tracked.byte_alloc(layout.size(), block.addr());
            });
        }
        block
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `layout` is passed on as `alloc_zeroed`
        // requires.
        let block = unsafe { System.alloc_zeroed(layout) };
        if !block.is_null() {
            tracked(|tracked| {
                if is_large(layout.size()) {
                    tracked.record(block.addr());
                }
                tracked.byte_alloc(layout.size(), block.addr());
            });
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        tracked(|tracked| {
            if is_large(layout.size()) {
                tracked.remove(block.addr());
            }
            tracked.byte_free(block.addr(), layout.size());
        });
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
        tracked(|tracked| {
            if was || now {
                let recorded = was && tracked.remove(block.addr());
                if now && (recorded || !was) {
                    tracked.record(moved.addr());
                }
            }
            tracked.byte_realloc(block.addr(), layout.size(), moved.addr(), new_size);
        });
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
pub fn large_blocks_during<T>(f: impl FnOnce() -> T) -> Counted<T> {
    let mut scope = Scope::open();
    let value = f();
    let (peak, overflowed) = scope.close();
    assert!(
        !overflowed,
        "more than {SLOTS} large blocks were alive at once; the count is too low"
    );
    (value, peak)
}

/// A scope's value with the most large blocks alive at once while it ran.
pub type Counted<T> = (T, usize);

/// A scope's value with the byte peaks [`bytes_during`] measured.
pub type Metered<T> = (T, Bytes);

/// The byte peaks one [`bytes_during`] scope measured. It copies out the
/// slot table when the scope ends, so [`Bytes::peak_without`] reads no
/// thread state. Without [`Counting`] installed both peaks are 0.
#[derive(Clone, Copy, Debug)]
pub struct Bytes {
    peak: usize,
    withouts: [(usize, usize); SLOTS],
}

impl Bytes {
    /// The most bytes allocated on this thread and not yet freed at once
    /// while the scope ran, counted from the scope's start and never
    /// below 0.
    pub fn peak(self) -> usize {
        self.peak
    }

    /// The same peak with the slotted block living at `address` at the
    /// scope's end left out at every moment it was slotted; an address
    /// that is no slot's gives [`Bytes::peak`].
    pub fn peak_without(self, address: *const u8) -> usize {
        self.withouts
            .iter()
            .find(|(slot, _)| *slot == address.addr())
            .map_or(self.peak, |(_, without)| *without)
    }
}

/// Runs `f` and returns its value with the byte peaks this thread
/// measured while it ran. Scopes nest as [`large_blocks_during`]'s do.
/// Without [`Counting`] installed nothing is recorded and both peaks
/// are 0.
///
/// # Panics
///
/// After `f` returns, when more byte-tracked blocks were alive at once
/// than the table holds.
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
pub fn bytes_during<T>(f: impl FnOnce() -> T) -> Metered<T> {
    let mut scope = Scope::open();
    let value = f();
    let measured = scope.close_bytes();
    assert!(
        !measured.1,
        "more than {SLOTS} byte-tracked blocks were alive at once; the byte peak is too low"
    );
    (value, measured.0)
}

/// An open scope, closed when `f` returns or, if it unwinds, when dropped.
struct Scope {
    outer: usize,
    outer_bytes: usize,
    closed: bool,
}

impl Scope {
    /// Opens a scope for either measurement: both peaks checkpoint the
    /// outer scope's and restart from what is alive now, so scopes nest
    /// whichever way they combine.
    fn open() -> Self {
        let mut scope = Self {
            outer: 0,
            outer_bytes: 0,
            closed: false,
        };
        tracked(|tracked| {
            tracked.depth += 1;
            scope.outer = tracked.peak;
            tracked.peak = tracked.live;
            scope.outer_bytes = tracked.byte_peak;
            tracked.byte_peak = peak_value(tracked.total);
        });
        scope
    }

    /// Closes the scope, giving its peak and whether the table overflowed.
    fn close(&mut self) -> (usize, bool) {
        self.closed = true;
        let outer = self.outer;
        let mut closed = (0, false);
        tracked(|tracked| {
            let peak = tracked.peak;
            tracked.peak = outer.max(peak);
            closed = (peak, tracked.overflowed);
            self.release_depth(tracked);
        });
        closed
    }

    /// Closes the scope, giving the byte peaks and whether the byte table
    /// overflowed.
    fn close_bytes(&mut self) -> (Bytes, bool) {
        self.closed = true;
        let outer = self.outer;
        let outer_bytes = self.outer_bytes;
        let mut closed = (
            Bytes {
                peak: 0,
                withouts: [(0, 0); SLOTS],
            },
            false,
        );
        tracked(|tracked| {
            let peak = tracked.peak;
            tracked.peak = outer.max(peak);
            let measured = Bytes {
                peak: tracked.byte_peak,
                withouts: tracked
                    .byte_slots
                    .map(|slot| (slot.addr, slot.peak_without)),
            };
            tracked.byte_peak = outer_bytes.max(tracked.byte_peak);
            closed = (measured, tracked.byte_overflowed);
            self.release_depth(tracked);
        });
        closed
    }

    /// Drops the scope's depth, resetting every table past the outermost.
    fn release_depth(&self, tracked: &mut Tracked) {
        tracked.depth = tracked.depth.saturating_sub(1);
        if tracked.depth == 0 {
            *tracked = Tracked::new();
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if !self.closed {
            self.close();
        }
    }
}
