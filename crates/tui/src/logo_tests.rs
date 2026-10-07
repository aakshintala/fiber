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

#[test]
fn width_cells_counts_pixels_a_blank_and_the_version() {
    assert_eq!(super::width_cells("0.0.1"), 32 + 1 + 5);
    assert_eq!(super::width_cells(""), 32 + 1);
}

#[test]
fn full_blocks_carry_no_background_and_two_tone_halves_carry_both() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    let version = "0.0.1";
    let wide = u16::try_from(super::width_cells(version)).unwrap_or(u16::MAX);
    let area = Rect::new(0, 0, wide.saturating_add(2), 4);
    let mut buf = Buffer::empty(area);
    super::draw(&mut buf, 0, 0, version);
    let mut full = 0;
    let mut half = 0;
    for y in 0..4 {
        for x in 0..wide {
            let cell = &buf[(x, y)];
            if cell.symbol() == "█" {
                // One colour on both halves: the foreground alone.
                assert_eq!(cell.bg, Color::Reset, "a full block at ({x}, {y})");
                full += 1;
            }
            if cell.symbol() == "▀" && cell.bg != Color::Reset {
                // Two colours: the top as foreground, the bottom behind.
                // Single-colour halves keep the default background.
                assert_ne!(cell.fg, Color::Reset, "a half block at ({x}, {y})");
                half += 1;
            }
        }
    }
    assert!(full > 0, "the letters draw full blocks");
    assert!(half > 0, "the counters shade two-tone halves");
}
