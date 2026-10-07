//! Tests for the pixel logo's bitmaps and the half-block rule.

use super::{B, E, F, I, LETTERS, R, WAVE, cell, pixels};

#[test]
fn the_bitmaps_are_eight_pixels_tall_and_mark_counters_only_inside_b_and_e() {
    for bitmap in [&WAVE, &F, &I, &B, &E, &R] {
        assert_eq!(bitmap.len(), 8, "eight pixel rows tall");
        let width = bitmap[0].len();
        for row in bitmap {
            assert_eq!(row.len(), width, "every row as wide as the first");
            for mark in row.chars() {
                assert!(matches!(mark, '#' | '.' | ' '), "ink, a counter or empty");
            }
        }
    }
    // Counters appear only inside `b` and `e`.
    for bitmap in [&WAVE, &F, &I, &R] {
        for row in bitmap {
            assert!(!row.contains('.'), "no counter outside b and e");
        }
    }
    for bitmap in [&B, &E] {
        assert!(
            bitmap.iter().any(|row| row.contains('.')),
            "b and e shade a counter"
        );
    }
    // The strip is the wave then the five letters: 32 pixel columns of
    // eight rows each.
    let strip = pixels();
    assert_eq!(LETTERS.len(), 5);
    assert_eq!(strip.len(), 32);
    for column in &strip {
        assert_eq!(column.len(), 8);
    }
}

#[test]
fn two_pixels_make_one_cell_by_the_half_block_rule() {
    assert_eq!(cell(' ', ' ', true), ' ');
    assert_eq!(cell('#', '#', true), '█');
    assert_eq!(cell('#', ' ', true), '▀');
    assert_eq!(cell(' ', '#', true), '▄');
    assert_eq!(cell('#', '.', true), '▀');
    assert_eq!(cell('#', '#', false), '▀');
}
