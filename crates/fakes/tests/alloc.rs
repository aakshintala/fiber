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

use fakes::alloc::{CHAIN, Counting, LARGE, bytes_during, large_blocks_during};

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
fn a_block_made_before_the_scope_and_grown_inside_is_not_counted() {
    let mut before = large();
    let ((), peak) = large_blocks_during(|| {
        before.reserve_exact(2 * TWO_MIB);
        let inside = large();
        drop(black_box(inside));
    });
    drop(before);
    assert_eq!(peak, 1, "only the block made inside the scope");
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

#[test]
#[allow(clippy::panic, reason = "this test must unwind through an open scope")]
fn a_scope_that_unwinds_is_closed() {
    use std::panic::AssertUnwindSafe;
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        large_blocks_during(|| {
            // Without `Scope::drop` closing the scope, this block and the
            // scope's depth leak into the next scope on this thread.
            std::mem::forget(large());
            panic!("unwind through an open scope");
        })
    }));
    assert!(outcome.is_err(), "the inner closure panics");
    let ((), after) = large_blocks_during(|| drop(large()));
    assert_eq!(
        after, 1,
        "the unwound scope's block and depth must not leak into the next scope"
    );
}

const KIB: usize = 1024;

#[test]
fn a_hundred_kilobyte_vec_peaks_at_its_size() {
    let (bytes, measured) = bytes_during(|| black_box(Vec::<u8>::with_capacity(100_000)));
    assert_eq!(bytes.capacity(), 100_000);
    assert_eq!(measured.peak(), 100_000);
    drop(bytes);
}

#[test]
fn two_one_kib_vecs_alive_at_once_peak_at_both() {
    let ((), measured) = bytes_during(|| {
        let first = black_box(Vec::<u8>::with_capacity(KIB));
        let second = black_box(Vec::<u8>::with_capacity(KIB));
        assert_eq!(first.capacity() + second.capacity(), 2 * KIB);
        drop((first, second));
    });
    assert_eq!(measured.peak(), 2 * KIB);
}

#[test]
fn vecs_freed_in_turn_peak_at_one() {
    let ((), measured) = bytes_during(|| {
        drop(black_box(Vec::<u8>::with_capacity(KIB)));
        drop(black_box(Vec::<u8>::with_capacity(KIB)));
    });
    assert_eq!(measured.peak(), KIB);
}

#[test]
fn a_vec_grown_by_reserve_peaks_at_its_final_capacity() {
    let (grown, measured) = bytes_during(|| {
        let mut bytes = black_box(Vec::<u8>::with_capacity(1_000));
        bytes.reserve(4_000);
        bytes
    });
    let capacity = grown.capacity();
    assert!(capacity >= 4_000, "capacity {capacity}");
    assert_eq!(measured.peak(), capacity);
    drop(grown);
}

#[test]
fn peak_without_keeps_an_early_scratch_peak() {
    let scratch_len = 300 * KIB;
    let (text, measured) = bytes_during(|| {
        let mut text = black_box(String::new());
        let scratch = black_box(vec![0u8; scratch_len]);
        assert_eq!(scratch.len(), scratch_len);
        // Free the scratch while the text is still short of one `CHAIN`:
        // its buffer is already tracked, so the scratch's peak stays in
        // the text's peak-without.
        while text.len() < 48 * KIB {
            text.push('x');
        }
        assert!(text.capacity() >= CHAIN, "capacity {}", text.capacity());
        drop(scratch);
        while text.len() < 4 << 20 {
            text.push('y');
        }
        text
    });
    assert!(text.len() == 4 << 20);
    let excluded = measured.peak_without(text.as_ptr());
    assert!(
        excluded >= scratch_len,
        "peak {} without {}",
        measured.peak(),
        excluded
    );
    drop(text);
}

#[test]
fn peak_without_keeps_a_scratch_peak_from_before_the_slot() {
    let scratch_len = 300 * KIB;
    let (text, measured) = bytes_during(|| {
        let scratch = black_box(vec![0u8; scratch_len]);
        assert_eq!(scratch.len(), scratch_len);
        drop(scratch);
        // The text takes its slot only past `CHAIN`, after the scratch
        // is gone: the peak before the slot existed is a peak without it.
        let mut text = black_box(String::new());
        while text.len() < 4 << 20 {
            text.push('y');
        }
        text
    });
    assert!(text.len() == 4 << 20);
    let excluded = measured.peak_without(text.as_ptr());
    assert!(
        excluded >= scratch_len,
        "peak {} without {}",
        measured.peak(),
        excluded
    );
    drop(text);
}

#[test]
fn peak_without_a_scratch_kept_to_the_end_stays_near_the_scratch() {
    let scratch_len = 300 * KIB;
    let (kept, measured) = bytes_during(|| {
        let mut text = black_box(String::new());
        let scratch = black_box(vec![0u8; scratch_len]);
        while text.len() < 4 << 20 {
            text.push('y');
        }
        (text, scratch)
    });
    let excluded = measured.peak_without(kept.0.as_ptr());
    assert!(
        excluded <= scratch_len + CHAIN,
        "peak without the output {excluded}, scratch {scratch_len}"
    );
    assert!(
        excluded >= scratch_len,
        "peak without the output {excluded}, scratch {scratch_len}"
    );
    drop(kept);
}

#[test]
fn peak_without_an_unknown_address_is_the_plain_peak() {
    let ((), measured) = bytes_during(|| {
        drop(black_box(Vec::<u8>::with_capacity(100_000)));
    });
    assert_eq!(
        measured.peak_without(usize::MAX as *const u8),
        measured.peak()
    );
}

#[test]
fn a_block_made_before_the_scope_and_freed_inside_lowers_the_total() {
    let before = black_box(Vec::<u8>::with_capacity(2 * KIB));
    let ((), measured) = bytes_during(|| {
        let inside = black_box(Vec::<u8>::with_capacity(KIB));
        assert_eq!(inside.capacity(), KIB);
        drop(before);
        assert_eq!(inside.capacity(), KIB);
        drop(inside);
    });
    assert_eq!(measured.peak(), KIB, "the lowering leaves the inside peak");
    let before = black_box(Vec::<u8>::with_capacity(KIB));
    let capacity = before.capacity();
    let ((), measured) = bytes_during(|| {
        assert_eq!(before.capacity(), capacity);
        drop(before);
    });
    assert_eq!(measured.peak(), 0, "the peak never drops below 0");
}

#[test]
fn more_tracked_blocks_than_the_table_holds_panics_naming_the_overflow() {
    let outcome = std::panic::catch_unwind(|| {
        bytes_during(|| {
            let blocks: Vec<Vec<u8>> = (0..65)
                .map(|_| black_box(Vec::with_capacity(CHAIN)))
                .collect();
            drop(blocks);
        })
    });
    let payload = outcome.expect_err("an overflow panics");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(message.contains("more than 64"), "{message}");
    let ((), after) = bytes_during(|| drop(black_box(Vec::<u8>::with_capacity(KIB))));
    assert_eq!(after.peak(), KIB, "the next scope counts again");
}

#[test]
fn byte_scopes_nest() {
    let ((inner, kept), outer) = bytes_during(|| {
        let kept = black_box(Vec::<u8>::with_capacity(KIB));
        let ((), inner) = bytes_during(|| {
            let first = black_box(Vec::<u8>::with_capacity(KIB));
            let second = black_box(Vec::<u8>::with_capacity(KIB));
            drop((first, second));
        });
        (inner.peak(), kept)
    });
    drop(kept);
    assert_eq!(inner, 3 * KIB);
    assert!(outer.peak() >= 3 * KIB, "outer {}", outer.peak());
}

#[test]
#[allow(clippy::panic, reason = "this test must unwind through an open scope")]
fn a_byte_scope_that_unwinds_is_closed() {
    use std::panic::AssertUnwindSafe;
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        bytes_during(|| {
            std::mem::forget(black_box(Vec::<u8>::with_capacity(KIB)));
            panic!("unwind through an open byte scope");
        })
    }));
    assert!(outcome.is_err(), "the inner closure panics");
    let ((), after) = bytes_during(|| drop(black_box(Vec::<u8>::with_capacity(KIB))));
    assert_eq!(
        after.peak(),
        KIB,
        "the unwound scope must not leak into the next scope"
    );
}
