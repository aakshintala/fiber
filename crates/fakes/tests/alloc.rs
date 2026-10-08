//! `fakes::alloc::Counting`'s own tests. This binary installs it as the
//! global allocator and holds nothing else, so no other test binary changes
//! allocator.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code, helpers included"
)]

use std::hint::black_box;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::alloc::{Counting, LARGE, large_blocks_during};

#[global_allocator]
static ALLOC: Counting = Counting;

const TWO_MIB: usize = 2 << 20;
const HALF_MIB: usize = 512 << 10;

/// How long a test waits for another thread's block or acknowledgement.
const HANDOFF: Duration = Duration::from_secs(10);

fn large() -> Vec<u8> {
    black_box(Vec::with_capacity(TWO_MIB))
}

#[test]
fn one_large_block_from_alloc_counts_one() {
    let ((), peak) = large_blocks_during(|| drop(large()));
    assert_eq!(peak, 1);
}

#[test]
fn one_large_zeroed_block_counts_one() {
    let ((), peak) = large_blocks_during(|| drop(black_box(vec![0u8; TWO_MIB])));
    assert_eq!(peak, 1);
}

#[test]
fn two_large_blocks_alive_at_once_count_two() {
    let ((), peak) = large_blocks_during(|| {
        let first = large();
        let second = large();
        drop((first, second));
    });
    assert_eq!(peak, 2);
}

#[test]
fn blocks_freed_in_turn_count_one() {
    let ((), peak) = large_blocks_during(|| {
        drop(large());
        drop(large());
    });
    assert_eq!(peak, 1);
}

#[test]
fn a_block_under_a_mib_counts_nothing() {
    let ((), peak) = large_blocks_during(|| {
        drop(black_box(Vec::<u8>::with_capacity(HALF_MIB)));
        drop(black_box(Vec::<u8>::with_capacity(LARGE - 1)));
    });
    assert_eq!(peak, 0);
}

#[test]
fn a_block_of_exactly_a_mib_is_large() {
    let ((), peak) = large_blocks_during(|| drop(black_box(Vec::<u8>::with_capacity(LARGE))));
    assert_eq!(peak, 1);
}

#[test]
fn a_block_grown_past_a_mib_counts_one() {
    let (grown, peak) = large_blocks_during(|| {
        let mut bytes = black_box(Vec::<u8>::with_capacity(HALF_MIB));
        bytes.reserve_exact(TWO_MIB);
        bytes
    });
    assert!(grown.capacity() >= TWO_MIB);
    assert_eq!(peak, 1);
}

#[test]
fn a_block_shrunk_under_a_mib_is_no_longer_counted() {
    let ((), peak) = large_blocks_during(|| {
        let mut bytes = large();
        bytes.shrink_to(HALF_MIB);
        assert!(bytes.capacity() < LARGE);
        let other = large();
        drop((bytes, other));
    });
    assert_eq!(peak, 1);
}

#[test]
fn a_large_block_moved_by_realloc_counts_once() {
    let ((), peak) = large_blocks_during(|| {
        let mut bytes = large();
        bytes.reserve_exact(2 * TWO_MIB);
        drop(bytes);
        drop(large());
    });
    assert_eq!(peak, 1);
}

#[test]
fn a_block_made_before_the_scope_and_freed_inside_changes_nothing() {
    let before = large();
    let ((), peak) = large_blocks_during(|| {
        drop(before);
        let first = large();
        let second = large();
        drop((first, second));
    });
    assert_eq!(peak, 2);
}

#[test]
fn a_block_made_on_another_thread_and_freed_here_changes_nothing() {
    let (send, receive) = mpsc::channel();
    thread::spawn(move || send.send(large()).unwrap_or(()));
    let theirs = receive
        .recv_timeout(HANDOFF)
        .expect("waited for the other thread's block");
    let ((), peak) = large_blocks_during(|| {
        drop(theirs);
        let mine = black_box(vec![1u8; TWO_MIB]);
        let copy = mine.clone();
        drop((mine, copy));
    });
    assert_eq!(peak, 2);
}

#[test]
fn a_block_freed_on_another_thread_stays_counted() {
    let ((), peak) = large_blocks_during(|| {
        let mine = large();
        let (send, receive) = mpsc::channel();
        thread::spawn(move || {
            drop(mine);
            send.send(()).unwrap_or(());
        });
        receive
            .recv_timeout(HANDOFF)
            .expect("waited for the other thread to free the block");
        drop(large());
    });
    assert_eq!(peak, 2);
}

#[test]
fn scopes_nest() {
    let ((inner, kept), outer) = large_blocks_during(|| {
        let kept = large();
        let ((), inner) = large_blocks_during(|| {
            let first = large();
            let second = large();
            drop((first, second));
        });
        (inner, kept)
    });
    drop(kept);
    assert_eq!(inner, 3);
    assert!(outer >= 3, "outer {outer}");
}

#[test]
fn a_new_scope_starts_from_nothing() {
    let (kept, first) = large_blocks_during(large);
    assert_eq!(first, 1);
    let ((), second) = large_blocks_during(|| drop(large()));
    drop(kept);
    assert_eq!(second, 1);
}

#[test]
fn more_blocks_than_the_table_holds_panic_naming_the_overflow() {
    let outcome = std::panic::catch_unwind(|| {
        large_blocks_during(|| {
            let blocks: Vec<Vec<u8>> = (0..65)
                .map(|_| black_box(Vec::with_capacity(LARGE)))
                .collect();
            drop(blocks);
        })
    });
    let payload = outcome.expect_err("an overflow panics");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(message.contains("more than 64 large blocks"), "{message}");
    let ((), after) = large_blocks_during(|| drop(large()));
    assert_eq!(after, 1, "the next scope counts again");
}
