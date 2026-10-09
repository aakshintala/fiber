//! Tests for the steering rule: the labelled rule over bold text
//! (`docs/tui.md`, "Turns").

use jiff::tz::TimeZone;
use ratatui::style::Modifier;

use super::Steered;
use crate::local_time::new_york;
use crate::rows::{Join, Rows};

/// The rows `text` steered at `ts` draws at `width` in `zone`.
fn drawn(text: &str, ts: u64, width: u16, zone: &TimeZone) -> Vec<String> {
    let mut out = Rows::default();
    Steered::new(text.to_owned(), ts).rows(width, zone, &mut out);
    let (lines, _) = out.into_parts();
    lines.iter().map(|(line, _)| line.to_string()).collect()
}

#[test]
fn the_rule_reads_steer_and_the_time() {
    // 1_760_000_000_000 is 08:53 UTC, 04:53 in New York: the label's time
    // is the zone's (`docs/tui.md`, "Turns").
    let utc = drawn("use x", 1_760_000_000_000, 40, &TimeZone::UTC);
    assert!(utc[0].starts_with("steer · 08:53 "), "{}", &utc[0]);
    assert_eq!(utc[1], "use x");
    let york = drawn("use x", 1_760_000_000_000, 40, &new_york());
    assert!(york[0].starts_with("steer · 04:53 "), "{}", &york[0]);
    assert_eq!(york[1], "use x");
}

#[test]
fn the_rule_fills_the_width_after_one_space() {
    // The label is 13 cells; past one space the rule fills the width
    // (`docs/tui.md`, "Turns").
    assert_eq!(drawn("use x", 0, 13, &TimeZone::UTC)[0], "steer · 00:00");
    assert_eq!(drawn("use x", 0, 14, &TimeZone::UTC)[0], "steer · 00:00");
    assert_eq!(drawn("use x", 0, 15, &TimeZone::UTC)[0], "steer · 00:00 ─");
    assert_eq!(
        drawn("use x", 0, 17, &TimeZone::UTC)[0],
        "steer · 00:00 ───"
    );
    for width in [14, 15, 17, 20, 40] {
        let mut out = Rows::default();
        Steered::new("use x".to_owned(), 0).rows(width, &TimeZone::UTC, &mut out);
        let (lines, _) = out.into_parts();
        for (line, _) in &lines {
            assert!(line.width() <= usize::from(width), "width {width}: {line}");
        }
    }
}

#[test]
fn the_rule_is_dim_and_its_dashes_muted() {
    // The label is dim, the dashes muted (`docs/tui.md`, "Turns").
    let mut out = Rows::default();
    Steered::new("use x".to_owned(), 0).rows(20, &TimeZone::UTC, &mut out);
    let (lines, _) = out.into_parts();
    let (rule, _) = &lines[0];
    assert_eq!(rule.spans.len(), 2);
    assert!(rule.spans[0].style.add_modifier.contains(Modifier::DIM));
    assert_eq!(rule.spans[0].content, "steer · 00:00");
    assert_eq!(
        rule.spans[1].style.fg,
        Some(crate::theme::Role::Muted.color())
    );
}

#[test]
fn a_time_jiff_cannot_hold_leaves_the_label_bare() {
    // Past jiff's range the label is bare `steer`, then dashes
    // (`docs/tui.md`, "Turns").
    let rows = drawn("use x", u64::MAX, 20, &TimeZone::UTC);
    assert_eq!(rows[0], format!("steer {}", "─".repeat(14)));
    assert_eq!(rows[1], "use x");
}

#[test]
fn the_text_is_bold_and_wraps_with_its_joins() {
    // The message's text is bold, wrapped with its joins
    // (`docs/tui.md`, "Turns").
    let mut out = Rows::default();
    Steered::new("aaaaaa bbbbbbbbbb".to_owned(), 0).rows(10, &TimeZone::UTC, &mut out);
    let (lines, texts) = out.into_parts();
    assert_eq!(lines.len(), 3);
    for (line, _) in lines.iter().skip(1) {
        assert!(
            line.spans
                .iter()
                .all(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
    }
    assert_eq!(lines[1].0.to_string(), "aaaaaa");
    assert_eq!(lines[2].0.to_string(), "bbbbbbbbbb");
    assert_eq!(texts[1].join, Join::Break);
    assert_eq!(texts[2].join, Join::WrapSpace);
    // A two-line message starts its second row as its own line.
    let mut out = Rows::default();
    Steered::new("one\ntwo".to_owned(), 0).rows(40, &TimeZone::UTC, &mut out);
    let (lines, texts) = out.into_parts();
    assert_eq!(lines.len(), 3);
    assert_eq!(texts[2].join, Join::Break);
}

#[test]
fn a_blank_message_draws_the_rule_alone() {
    // Nothing to say: the rule alone (`docs/tui.md`, "Turns").
    for text in ["", "  "] {
        let mut out = Rows::default();
        Steered::new(text.to_owned(), 0).rows(20, &TimeZone::UTC, &mut out);
        let (lines, _) = out.into_parts();
        assert_eq!(lines.len(), 1, "text {text:?}");
    }
}

#[test]
fn the_rule_copies_without_its_dashes() {
    // The rule's dashes are tail cells, so a copy reads the label alone
    // (`docs/tui.md`, "Selection and copy").
    let mut out = Rows::default();
    Steered::new("use x".to_owned(), 0).rows(20, &TimeZone::UTC, &mut out);
    let (lines, texts) = out.into_parts();
    let logical: Vec<String> = crate::logical::logical(&lines, &texts)
        .iter()
        .map(|line| line.text.clone())
        .collect();
    assert_eq!(logical, ["steer · 00:00", "use x"]);
}

#[test]
fn nothing_is_indented_and_nothing_is_striped() {
    // No stripe, no indent (`docs/tui.md`, "Turns").
    let mut out = Rows::default();
    Steered::new("use x".to_owned(), 0).rows(20, &TimeZone::UTC, &mut out);
    let (lines, _) = out.into_parts();
    for (line, _) in &lines {
        let shown = line.to_string();
        assert!(!shown.starts_with(' '), "{shown:?}");
        assert!(!shown.contains('▌') && !shown.contains('▐'), "{shown:?}");
    }
}
