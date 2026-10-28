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
    Steered::new(text.to_owned(), ts).rows(width, zone, &crate::image::Layout::default(), &mut out);
    let (lines, _) = out.into_parts();
    lines.iter().map(|(line, _)| line.to_string()).collect()
}

#[test]
fn the_rule_reads_steer_and_the_time() {
    // 1_760_000_000_000 is 08:53 UTC, 04:53 in New York: the label's time
    // is the zone's. "steer" is bold in the attention tint, the time dim,
    // and the dashes fill the width in the rule tint
    // (`docs/tui.md`, "Turns").
    for (zone, time) in [(TimeZone::UTC, "08:53"), (new_york(), "04:53")] {
        let mut out = Rows::default();
        Steered::new("use x".to_owned(), 1_760_000_000_000).rows(
            40,
            &zone,
            &crate::image::Layout::default(),
            &mut out,
        );
        let (lines, _) = out.into_parts();
        let (rule, _) = &lines[0];
        assert_eq!(rule.spans.len(), 3);
        assert_eq!(rule.spans[0].content, "steer");
        assert!(rule.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(
            rule.spans[0].style.fg,
            Some(crate::theme::Role::Attention.color())
        );
        assert_eq!(rule.spans[1].content, format!(" · {time} "));
        assert!(rule.spans[1].style.add_modifier.contains(Modifier::DIM));
        // The label is 5 + 9 cells; past one space the rule fills the
        // width: exactly 26 dashes at width 40.
        assert_eq!(rule.spans[2].content, "─".repeat(26));
        assert_eq!(
            rule.spans[2].style.fg,
            Some(crate::theme::Role::Rule.color())
        );
    }
    let utc = drawn("use x", 1_760_000_000_000, 40, &TimeZone::UTC);
    assert!(utc[0].starts_with("steer · 08:53 "), "{}", &utc[0]);
    assert_eq!(utc[1], "use x");
    let york = drawn("use x", 1_760_000_000_000, 40, &new_york());
    assert!(york[0].starts_with("steer · 04:53 "), "{}", &york[0]);
    assert_eq!(york[1], "use x");
}

#[test]
fn the_rule_fills_the_width_after_one_space() {
    // The label is 14 cells; past it the rule fills the width, and with
    // no room left no rule span draws (`docs/tui.md`, "Turns").
    assert_eq!(drawn("use x", 0, 13, &TimeZone::UTC)[0], "steer · 00:00 ");
    assert_eq!(drawn("use x", 0, 14, &TimeZone::UTC)[0], "steer · 00:00 ");
    assert_eq!(drawn("use x", 0, 15, &TimeZone::UTC)[0], "steer · 00:00 ─");
    assert_eq!(
        drawn("use x", 0, 17, &TimeZone::UTC)[0],
        "steer · 00:00 ───"
    );
    for width in [14, 15, 17, 20, 40] {
        let mut out = Rows::default();
        Steered::new("use x".to_owned(), 0).rows(
            width,
            &TimeZone::UTC,
            &crate::image::Layout::default(),
            &mut out,
        );
        let (lines, _) = out.into_parts();
        for (line, _) in &lines {
            assert!(line.width() <= usize::from(width), "width {width}: {line}");
        }
    }
    // A width that leaves no dash draws the label alone, with no rule span.
    let mut out = Rows::default();
    Steered::new("use x".to_owned(), 0).rows(
        14,
        &TimeZone::UTC,
        &crate::image::Layout::default(),
        &mut out,
    );
    let (lines, _) = out.into_parts();
    assert_eq!(lines[0].0.spans.len(), 2);
}

#[test]
fn a_time_jiff_cannot_hold_leaves_steer_and_the_rule() {
    // Past jiff's range the label is bare `steer`, then one space and the
    // dashes (`docs/tui.md`, "Turns").
    let rows = drawn("use x", u64::MAX, 20, &TimeZone::UTC);
    assert_eq!(rows[0], format!("steer {}", "─".repeat(14)));
    assert_eq!(rows[1], "use x");
}

#[test]
fn the_text_is_bold_and_wraps_with_its_joins() {
    // The message's text is bold, wrapped with its joins
    // (`docs/tui.md`, "Turns").
    let mut out = Rows::default();
    Steered::new("aaaaaa bbbbbbbbbb".to_owned(), 0).rows(
        10,
        &TimeZone::UTC,
        &crate::image::Layout::default(),
        &mut out,
    );
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
    Steered::new("one\ntwo".to_owned(), 0).rows(
        40,
        &TimeZone::UTC,
        &crate::image::Layout::default(),
        &mut out,
    );
    let (lines, texts) = out.into_parts();
    assert_eq!(lines.len(), 3);
    assert_eq!(texts[2].join, Join::Break);
}

#[test]
fn a_blank_message_draws_the_rule_alone() {
    // Nothing to say: the rule alone (`docs/tui.md`, "Turns").
    for text in ["", "  "] {
        let mut out = Rows::default();
        Steered::new(text.to_owned(), 0).rows(
            20,
            &TimeZone::UTC,
            &crate::image::Layout::default(),
            &mut out,
        );
        let (lines, _) = out.into_parts();
        assert_eq!(lines.len(), 1, "text {text:?}");
    }
}

#[test]
fn the_rule_copies_without_its_dashes() {
    // The rule's dashes are tail cells, so a copy reads the label alone
    // (`docs/tui.md`, "Selection and copy").
    let mut out = Rows::default();
    Steered::new("use x".to_owned(), 0).rows(
        20,
        &TimeZone::UTC,
        &crate::image::Layout::default(),
        &mut out,
    );
    let (lines, texts) = out.into_parts();
    let logical: Vec<String> = crate::logical::logical(&lines, &texts)
        .iter()
        .map(|line| line.text.clone())
        .collect();
    assert_eq!(logical, ["steer · 00:00", "use x"]);
}

#[test]
fn the_rule_ends_at_the_width_it_is_given() {
    // No stripe, no indent past the card's padding: the rule's dashes run
    // to the given width (`docs/tui.md`, "Turns").
    let mut out = Rows::default();
    Steered::new("use x".to_owned(), 0).rows(
        20,
        &TimeZone::UTC,
        &crate::image::Layout::default(),
        &mut out,
    );
    let (lines, _) = out.into_parts();
    assert_eq!(lines[0].0.width(), 20);
    for (line, _) in &lines {
        let shown = line.to_string();
        assert!(!shown.starts_with(' '), "{shown:?}");
        assert!(!shown.contains('▌') && !shown.contains('▐'), "{shown:?}");
    }
}

#[test]
fn a_rule_with_no_dashes_has_no_tail() {
    // The label alone has nothing to leave out of a copy; past it the tail
    // is the space and the dashes (`docs/tui.md`, "Selection and copy").
    for (width, tail) in [(14, 0), (15, 2), (20, 7)] {
        let mut out = Rows::default();
        Steered::new("use x".to_owned(), 0).rows(
            width,
            &TimeZone::UTC,
            &crate::image::Layout::default(),
            &mut out,
        );
        let (_, texts) = out.into_parts();
        assert_eq!(texts[0].tail, tail, "width {width}");
    }
}

#[test]
fn the_steer_entry_carries_text_images_and_ts() {
    use crate::ext::TurnEntry;
    let plain = Steered::new("wait".to_owned(), 7);
    let TurnEntry::Steer { text, images, ts } = plain.turn_entry() else {
        panic!("a steer entry");
    };
    assert_eq!(text, "wait");
    assert!(images.is_empty());
    assert_eq!(ts, 7);
    let imaged = Steered::with_images(
        "see".to_owned(),
        vec![crate::image::Part {
            session: contract::SessionId("s".into()),
            path: "artifacts/a.png".to_owned(),
            mime_type: "image/png".to_owned(),
            width: 10,
            height: 8,
        }],
        9,
    );
    let TurnEntry::Steer { text, images, ts } = imaged.turn_entry() else {
        panic!("a steer entry");
    };
    assert_eq!(text, "see");
    assert_eq!(ts, 9);
    // (`docs/tui.md`, "The slots", `steer`: its images in order.)
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].path, "artifacts/a.png");
    assert_eq!((images[0].width, images[0].height), (10, 8));
}

/// A steering message's rows through a seam drawing `fake`.
fn seamed(
    steered: &Steered,
    width: u16,
    fake: &std::rc::Rc<crate::ext::tests::SlotRenderer>,
    flat: bool,
) -> Vec<crate::turn::Row> {
    use crate::local_time::new_york;
    let mut out =
        crate::rows::Rows::default().with_seam(crate::ext::tests::seam(fake, "s", 0, flat));
    steered.rows(
        width,
        &new_york(),
        &crate::image::Layout::default(),
        &mut out,
    );
    out.into_parts().0
}

fn replaced() -> std::rc::Rc<crate::ext::tests::SlotRenderer> {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    std::rc::Rc::new(SlotRenderer::filling(
        &["steer"],
        SlotScript::Replace(vec![vec![raw("custom")]]),
    ))
}

#[test]
fn a_steer_replacement_draws_instead_of_the_rule() {
    let steered = Steered::new("wait".to_owned(), 7);
    let fake = replaced();
    let rows = seamed(&steered, 40, &fake, false);
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    // (`docs/tui.md`, "How a TUI extension runs": each drawn item is cached by input and width.)
    assert_eq!(fake.calls.borrow().len(), 1);
    assert_eq!(fake.calls.borrow()[0].1, "steer");
    assert!(
        shown.iter().any(|row| row.starts_with("custom")),
        "{shown:?}"
    );
    assert!(!shown.iter().any(|row| row.contains("wait")), "{shown:?}");
}

#[test]
fn a_steer_decorator_draws_the_built_in_rule_under_its_badge() {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    let steered = Steered::new("wait".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["steer"],
        SlotScript::Decorate {
            badge: vec![vec![raw("BADGE")]],
            trim: 5,
        },
    ));
    let rows = seamed(&steered, 40, &fake, false);
    assert_eq!(fake.builtins.borrow().as_slice(), &[35]);
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        shown.iter().any(|row| row.starts_with("BADGE")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|row| row.contains("steer")), "{shown:?}");
}

#[test]
fn a_steer_error_falls_back_with_one_failure() {
    use crate::ext::FailureKind;
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let steered = Steered::new("wait".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["steer"],
        SlotScript::Fail {
            kind: FailureKind::HookFailed,
            message: "boom".into(),
        },
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    for _ in 0..2 {
        use crate::local_time::new_york;
        let mut out = crate::rows::Rows::default().with_seam(bound.clone());
        steered.rows(40, &new_york(), &crate::image::Layout::default(), &mut out);
        let shown: Vec<String> = out
            .into_parts()
            .0
            .iter()
            .map(|(line, _)| line.to_string())
            .collect();
        assert!(shown.iter().any(|row| row.contains("wait")), "{shown:?}");
    }
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(
        fake.calls.borrow().len(),
        1,
        "the failure calls nothing again"
    );
    let listed = bound.cache.take_failures();
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "steer");
    assert_eq!(listed[0].1.kind, FailureKind::HookFailed);
}

#[test]
fn a_flat_steer_replacement_drops_every_style() {
    use crate::ext::tests::{SlotRenderer, SlotScript};
    use ratatui::style::Style;
    let steered = Steered::new("wait".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["steer"],
        SlotScript::Replace(vec![vec![crate::ext::RawSpan {
            text: "custom".into(),
            bold: true,
            fg: Some("accent".into()),
            ..crate::ext::RawSpan::default()
        }]]),
    ));
    let rows = seamed(&steered, 40, &fake, true);
    // (`docs/tui.md`, "How a TUI extension runs": in screen-reader mode the terminal drops every style.)
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(shown.iter().any(|row| row.contains("custom")), "{shown:?}");
    for (line, _) in &rows {
        for span in &line.spans {
            assert_eq!(span.style, Style::default(), "{span:?}");
        }
    }
}
