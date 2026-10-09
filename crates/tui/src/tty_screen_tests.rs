//! Tests for the bounded incremental terminal output state: one escape
//! parser across deltas feeding either an 80x24 grid or capped lines.

use super::*;

/// A grid fed `chunks` in order, one delta each.
fn fed_grid(chunks: &[&str]) -> Grid {
    let mut parser = Parser::new();
    let mut grid = Grid::new();
    for chunk in chunks {
        parser.feed(chunk, &mut grid);
    }
    grid
}

/// A lines sink fed `chunks` in order, one delta each.
fn fed_lines(chunks: &[&str]) -> Lines {
    let mut parser = Parser::new();
    let mut lines = Lines::new();
    for chunk in chunks {
        parser.feed(chunk, &mut lines);
    }
    lines
}

/// A grid's rows with trailing padding cut, for content comparisons.
fn text(grid: &Grid) -> Vec<String> {
    grid.rows(80, 24)
        .iter()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// A lines sink's rows with trailing padding cut.
fn words(lines: &Lines) -> Vec<String> {
    lines
        .rows(80, 1000)
        .iter()
        .map(|row| row.trim_end().to_owned())
        .filter(|row| !row.is_empty())
        .collect()
}

#[test]
fn plain_text_lands_on_the_first_row() {
    let grid = fed_grid(&["hi"]);
    let rows = text(&grid);
    assert_eq!(rows[0], "hi");
    assert!(rows[1..].iter().all(|row| row.is_empty()));
}

#[test]
fn line_feed_starts_a_new_row() {
    let grid = fed_grid(&["a\nb"]);
    let rows = text(&grid);
    assert_eq!(rows[0], "a");
    assert_eq!(rows[1], "b");
}

#[test]
fn carriage_return_overwrites_the_row() {
    let grid = fed_grid(&["10%\r20%"]);
    assert_eq!(text(&grid)[0], "20%");
}

#[test]
fn backspace_steps_back_one_cell() {
    let grid = fed_grid(&["ab\rc"]);
    assert_eq!(text(&grid)[0], "cb");
}

#[test]
fn tab_moves_to_the_next_stop_of_eight() {
    let grid = fed_grid(&["a\tb"]);
    assert_eq!(text(&grid)[0], "a       b");
}

#[test]
fn tab_at_the_edge_clamps_instead_of_wrapping() {
    let grid = fed_grid(&["\x1b[1;79H\tq"]);
    let rows = grid.rows(80, 24);
    assert!(rows[0].ends_with('q'));
    assert!(text(&grid)[1..].iter().all(|row| row.is_empty()));
}

#[test]
fn text_past_column_80_wraps() {
    let grid = fed_grid(&["x".repeat(80).as_str(), "y"]);
    let rows = text(&grid);
    assert_eq!(rows[0], "x".repeat(80));
    assert_eq!(rows[1], "y");
}

#[test]
fn text_past_the_last_row_scrolls_the_top_away() {
    let mut owned: Vec<String> = (1..=24).map(|n| format!("{n}\n")).collect();
    owned.push("25".to_owned());
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    let grid = fed_grid(&refs);
    let rows = text(&grid);
    assert_eq!(rows[0], "2");
    assert_eq!(rows[23], "25");
}

#[test]
fn erase_line_modes_clear_cells() {
    // `CSI K` with no parameter clears to the end of the line.
    let grid = fed_grid(&["hello\x1b[1;3H\x1b[K"]);
    assert_eq!(text(&grid)[0], "he");
    // `CSI 1K` clears the start of the line through the cursor.
    let grid = fed_grid(&["hello\x1b[1;3H\x1b[1K"]);
    assert_eq!(text(&grid)[0], "   lo");
    // `CSI 2K` clears the whole line.
    let grid = fed_grid(&["hello\x1b[2K"]);
    assert!(text(&grid)[0].is_empty());
}

#[test]
fn erase_display_clears_the_grid() {
    let grid = fed_grid(&["hello\nworld\x1b[2J"]);
    assert!(text(&grid).iter().all(|row| row.is_empty()));
}

#[test]
fn cursor_home_defaults_to_the_first_cell() {
    let grid = fed_grid(&["ab\x1b[Hc"]);
    assert_eq!(text(&grid)[0], "cb");
}

#[test]
fn cursor_position_moves_and_clamps() {
    let grid = fed_grid(&["\x1b[3;5Hx"]);
    assert_eq!(text(&grid)[2], "    x");
    let grid = fed_grid(&["\x1b[99;99Hy"]);
    let rows = grid.rows(80, 24);
    assert!(rows[23].ends_with('y'));
}

#[test]
fn cursor_moves_default_to_one_and_clamp() {
    // `CSI 2D` steps back two cells, so the next cell overwrites.
    let grid = fed_grid(&["abc\x1b[2DX"]);
    assert_eq!(text(&grid)[0], "aXc");
    // Moving past the edges clamps instead of panicking.
    let grid = fed_grid(&["\x1b[A\x1b[D\x1b[99B\x1b[99Cz"]);
    let rows = text(&grid);
    assert_eq!(rows[23].trim_end().len(), 80);
    assert!(rows[23].ends_with('z'));
}

#[test]
fn color_sequences_are_dropped() {
    let grid = fed_grid(&["a\x1b[31mb\x1b[0mc"]);
    assert_eq!(text(&grid)[0], "abc");
}

#[test]
fn operating_system_sequences_are_dropped() {
    let grid = fed_grid(&["a\x1b]0;title\x07b"]);
    assert_eq!(text(&grid)[0], "ab");
    let grid = fed_grid(&["a\x1b]0;title\x1b\\b"]);
    assert_eq!(text(&grid)[0], "ab");
}

#[test]
fn a_sequence_split_across_deltas_completes() {
    let grid = fed_grid(&["hello", "\x1b", "[2", "J", "\x1b[H", "after"]);
    let rows = text(&grid);
    assert_eq!(rows[0], "after");
    assert!(rows[1..].iter().all(|row| row.is_empty()));
}

#[test]
fn an_osc_split_across_deltas_leaves_no_trace() {
    let grid = fed_grid(&["a", "\x1b]0;ti", "tle\x07", "b"]);
    assert_eq!(text(&grid)[0], "ab");
}

#[test]
fn an_unterminated_sequence_at_the_end_draws_nothing() {
    let grid = fed_grid(&["a\x1b"]);
    assert_eq!(text(&grid)[0], "a");
    let grid = fed_grid(&["a\x1b[2"]);
    assert_eq!(text(&grid)[0], "a");
}

#[test]
fn a_huge_parameter_saturates_and_clamps() {
    let grid = fed_grid(&["\x1b[99999999Cz"]);
    assert!(text(&grid)[0].ends_with('z'));
    let grid = fed_grid(&["\x1b[99999999;99999999Hy"]);
    assert!(text(&grid)[23].ends_with('y'));
}

#[test]
fn wide_text_never_panics() {
    let grid = fed_grid(&["あいうえお"]);
    assert!(text(&grid)[0].contains("あいうえお"));
    let _ = grid.rows(3, 24);
}

#[test]
fn sixty_four_pending_bytes_still_dispatch() {
    // 64 parameter bytes then the final: the sequence still dispatches,
    // so the dropped SGR leaves only the text after it.
    let grid = fed_grid(&[format!("\x1b[{}mZ", "9".repeat(64)).as_str()]);
    assert_eq!(text(&grid)[0], "Z");
}

#[test]
fn sixty_five_pending_bytes_drop_and_draw_the_tail() {
    // The 65th pending byte returns the parser to the ground, so the
    // sequence's tail draws as text instead of vanishing.
    let grid = fed_grid(&[format!("\x1b[{}Z", "9".repeat(65)).as_str()]);
    let rows = text(&grid);
    assert!(rows[0].starts_with('9'));
    assert!(rows[0].ends_with('Z'));
}

#[test]
fn lines_overwrite_on_carriage_return() {
    let lines = fed_lines(&["10%\r20%"]);
    assert_eq!(words(&lines), ["20%"]);
}

#[test]
fn lines_treat_crlf_and_lf_alike() {
    let lines = fed_lines(&["a\r\nb"]);
    assert_eq!(words(&lines), ["a", "b"]);
    let lines = fed_lines(&["a\nb"]);
    assert_eq!(words(&lines), ["a", "b"]);
}

#[test]
fn lines_drop_escapes_across_a_split() {
    let lines = fed_lines(&["a\x1b", "[31mb"]);
    assert_eq!(words(&lines), ["ab"]);
}

#[test]
fn lines_keep_at_most_a_thousand() {
    let fed: Vec<String> = (0..1001).map(|n| format!("line {n}")).collect();
    let text = fed.join("\n");
    let lines = fed_lines(&[text.as_str()]);
    let rows = lines.rows(80, 2000);
    assert_eq!(rows.len(), 1000);
    assert!(rows[0].starts_with("line 1"));
    assert!(rows[999].starts_with("line 1000"));
}

#[test]
fn lines_cut_a_row_at_1024_characters() {
    let lines = fed_lines(&["y".repeat(1025).as_str()]);
    let rows = lines.rows(1024, 24);
    assert_eq!(rows[0].len(), 1024);
}

#[test]
fn rows_cut_to_the_body_and_pad_to_its_width() {
    let grid = fed_grid(&["hello"]);
    let rows = grid.rows(60, 20);
    assert_eq!(rows.len(), 20);
    assert!(rows.iter().all(|row| row.len() == 60));
    assert!(rows[0].starts_with("hello"));
    let rows = grid.rows(100, 30);
    assert_eq!(rows.len(), 24);
    assert!(rows.iter().all(|row| row.len() == 100));
}

#[test]
fn lines_draw_the_last_rows_that_fit() {
    let lines = fed_lines(&["one\ntwo\nthree"]);
    let rows = lines.rows(80, 2);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].starts_with("two"));
    assert!(rows[1].starts_with("three"));
}

#[test]
fn a_flood_of_open_sequences_still_finishes() {
    let mut parser = Parser::new();
    let mut grid = Grid::new();
    for _ in 0..64 {
        parser.feed(&"\x1b[".repeat(512), &mut grid);
    }
    assert!(text(&grid).iter().all(|row| row.is_empty()));
}

#[test]
fn output_feeds_a_grid_or_lines_and_draws_rows() {
    let mut tty = Output::tty();
    tty.feed("hi\x1b[2J\x1b[Hbye");
    let rows = tty.rows(80, 24);
    assert!(rows[0].starts_with("bye"));
    let mut plain = Output::plain();
    plain.feed("hi\x1b[31mho");
    let rows = plain.rows(80, 24);
    assert!(rows[0].starts_with("hiho"));
}

#[test]
fn esc_aborts_a_pending_sequence() {
    // An `ESC` inside a CSI drops it: the clear still dispatches.
    let grid = fed_grid(&["hi\x1b[99\x1b[2J\x1b[Hbye"]);
    assert_eq!(text(&grid)[0], "bye");
    // A doubled `ESC` restarts the sequence instead of grounding it.
    let grid = fed_grid(&["\x1b\x1b[2Jbye"]);
    assert_eq!(text(&grid)[0], "bye");
}

#[test]
fn private_mark_sequences_are_dropped_whole() {
    // The `?` is not a parameter: the whole sequence drops and the next
    // cell draws at the cursor, not at a parsed column.
    let grid = fed_grid(&["\x1b[1;?HX"]);
    assert_eq!(text(&grid)[0], "X");
}

#[test]
fn unknown_erase_modes_leave_the_cells() {
    let grid = fed_grid(&["hello\x1b[5J\x1b[5K"]);
    assert_eq!(text(&grid)[0], "hello");
}

#[test]
fn an_osc_backslash_without_esc_is_content() {
    // Only `ESC \` ends the sequence: a bare backslash is dropped with it.
    let grid = fed_grid(&["a\x1b]0;a\\b\x07c"]);
    assert_eq!(text(&grid)[0], "ac");
}

#[test]
fn an_osc_esc_followed_by_text_stays_inside() {
    // The `ESC` only closes before a backslash: the text after it is
    // still dropped with the sequence.
    let grid = fed_grid(&["a\x1b]0\x1bX\\b"]);
    assert_eq!(text(&grid)[0], "a");
}

#[test]
fn control_bytes_are_dropped() {
    let grid = fed_grid(&["a\x07\x7fb"]);
    assert_eq!(text(&grid)[0], "ab");
}

#[test]
fn lines_expand_tabs() {
    let lines = fed_lines(&["a\tb"]);
    assert_eq!(words(&lines), ["a       b"]);
}

#[test]
fn an_over_long_osc_drops_and_draws_the_tail() {
    let grid = fed_grid(&[format!("\x1b]{}Z", "x".repeat(65)).as_str()]);
    let row = text(&grid)[0].clone();
    assert!(row.starts_with('x'));
    assert!(row.ends_with('Z'));
}

#[test]
fn clearing_a_reversed_range_clears_nothing() {
    let mut grid = Grid::new();
    let mut parser = Parser::new();
    parser.feed("hello", &mut grid);
    grid.clear_row(0, 5, 2);
    grid.clear_row(99, 0, 80);
    let rows = text(&grid);
    assert_eq!(rows[0], "hello");
    assert!(rows[1..].iter().all(|row| row.is_empty()));
}

#[test]
fn charset_designations_are_dropped_whole() {
    // The final byte never draws as text.
    for introducer in ["(", ")", "*", "+"] {
        let sequence = format!("a\x1b{introducer}Bb");
        let grid = fed_grid(&[sequence.as_str()]);
        assert_eq!(text(&grid)[0], "ab", "{introducer}");
    }
}

#[test]
fn a_charset_split_across_deltas_completes() {
    let grid = fed_grid(&["a\x1b", "(B", "b"]);
    assert_eq!(text(&grid)[0], "ab");
    let grid = fed_grid(&["a\x1b(", "B", "b"]);
    assert_eq!(text(&grid)[0], "ab");
}

#[test]
fn a_charset_esc_starts_the_next_sequence() {
    // An `ESC` after the introducer is the next sequence, not the final:
    // the clear still dispatches.
    let grid = fed_grid(&["\x1b(\x1b[2Jbye"]);
    assert_eq!(text(&grid)[0], "bye");
}

#[test]
fn st_terminated_sequences_are_dropped_until_st() {
    // DCS, SOS, PM and APC run until `ST` (`ESC \`); a `BEL` inside is
    // content, still dropped with the sequence.
    for introducer in ['P', 'X', '^', '_'] {
        let sequence = format!("a\x1b{introducer}payload\x1b\\b");
        let grid = fed_grid(&[sequence.as_str()]);
        assert_eq!(text(&grid)[0], "ab", "{introducer}");
    }
    let grid = fed_grid(&["a\x1bPpay\x07load\x1b\\b"]);
    assert_eq!(text(&grid)[0], "ab");
}

#[test]
fn an_st_split_across_deltas_completes() {
    let grid = fed_grid(&["a\x1bPpay", "load\x1b", "\\b"]);
    assert_eq!(text(&grid)[0], "ab");
    let grid = fed_grid(&["a", "\x1b", "Pq", "r\x1b\\", "b"]);
    assert_eq!(text(&grid)[0], "ab");
}

#[test]
fn an_st_over_long_drops_and_draws_the_tail() {
    // 64 pending bytes stay inside the sequence, so nothing draws.
    let grid = fed_grid(&[format!("\x1bP{}Z", "x".repeat(63)).as_str()]);
    assert!(text(&grid)[0].is_empty());
    // The 65th pending byte returns the parser to the ground, so the
    // tail draws as text instead of vanishing.
    let grid = fed_grid(&[format!("\x1bP{}Z", "x".repeat(64)).as_str()]);
    let row = text(&grid)[0].clone();
    assert!(row.ends_with('Z'));
    let grid = fed_grid(&[format!("\x1bP{}Z", "x".repeat(65)).as_str()]);
    let row = text(&grid)[0].clone();
    assert!(row.starts_with('x'));
    assert!(row.ends_with('Z'));
}

#[test]
fn an_st_over_long_drops_across_deltas() {
    let filler = "x".repeat(65);
    let grid = fed_grid(&["\x1bP", filler.as_str(), "Z"]);
    let row = text(&grid)[0].clone();
    assert!(row.starts_with('x'));
    assert!(row.ends_with('Z'));
}
