//! Tests for a reply's shared render slot: clones share one slot until
//! either changes its text.

use super::Reply;

const TABLE: &str = "| a | b |\n|---|---|\n| c | d |\n";

#[test]
fn rendering_a_clone_fills_the_originals_slot() {
    let reply = Reply::new("hello".to_owned(), 1);
    let clone = reply.clone();
    assert_eq!(reply.cached_at(), None);
    clone.rendered(60);
    assert_eq!(reply.cached_at(), Some(60));
    assert_eq!(clone.cached_at(), Some(60));
}

#[test]
fn a_clones_push_leaves_the_originals_text() {
    let reply = Reply::new("hello".to_owned(), 1);
    let mut clone = reply.clone();
    clone.push(" there");
    assert_eq!(
        clone.rendered(60),
        Reply::new("hello there".to_owned(), 2).rendered(60)
    );
    assert_eq!(
        reply.rendered(60),
        Reply::new("hello".to_owned(), 3).rendered(60)
    );
}

#[test]
fn a_clones_set_leaves_the_originals_text() {
    let reply = Reply::new("hello".to_owned(), 1);
    let mut clone = reply.clone();
    clone.set("goodbye".to_owned());
    assert_eq!(
        clone.rendered(60),
        Reply::new("goodbye".to_owned(), 2).rendered(60)
    );
    assert_eq!(
        reply.rendered(60),
        Reply::new("hello".to_owned(), 3).rendered(60)
    );
}

#[test]
fn a_new_width_renders_again_for_both_clones() {
    let reply = Reply::new("hello".to_owned(), 1);
    let clone = reply.clone();
    reply.rendered(60);
    clone.rendered(80);
    assert_eq!(reply.cached_at(), Some(80));
    assert_eq!(clone.cached_at(), Some(80));
    assert_eq!(reply.rendered(80), clone.rendered(80));
}

#[test]
fn push_empties_only_the_changed_replys_slot() {
    let reply = Reply::new("hello".to_owned(), 1);
    reply.rendered(60);
    let mut clone = reply.clone();
    clone.push("!");
    assert_eq!(clone.cached_at(), None);
    assert_eq!(reply.cached_at(), Some(60));
}

#[test]
fn set_empties_only_the_changed_replys_slot() {
    let reply = Reply::new("hello".to_owned(), 1);
    reply.rendered(60);
    let mut clone = reply.clone();
    clone.set("goodbye".to_owned());
    assert_eq!(clone.cached_at(), None);
    assert_eq!(reply.cached_at(), Some(60));
}

#[test]
fn a_table_in_the_source_reads_as_table_sensitive() {
    let reply = Reply::new(TABLE.to_owned(), 1);
    assert_eq!(reply.table_cached(), None);
    assert!(reply.has_table());
    assert_eq!(reply.table_cached(), Some(true));
    // The answer is cached: parsing runs once.
    assert!(reply.has_table());
    assert_eq!(reply.table_cached(), Some(true));
}

#[test]
fn table_looking_text_without_a_table_reads_as_width_stable() {
    // Fenced code with pipes is no table, as the parser knows blocks.
    let fenced = Reply::new("```\n| a | b |\n|---|---|\n```\n".to_owned(), 1);
    assert!(!fenced.has_table());
    assert_eq!(fenced.table_cached(), Some(false));
    // Escaped pipes are no table either.
    let escaped = Reply::new("a \\| b \\| c\n".to_owned(), 2);
    assert!(!escaped.has_table());
    // Strikethrough alone, parsed with the renderer's options, is none.
    let struck = Reply::new("~~gone~~\n".to_owned(), 3);
    assert!(!struck.has_table());
    // Plain prose stays width-stable.
    let prose = Reply::new("just some words\n".to_owned(), 4);
    assert!(!prose.has_table());
}

#[test]
fn a_clone_shares_the_table_answer_until_it_changes_its_text() {
    let reply = Reply::new(TABLE.to_owned(), 1);
    let clone = reply.clone();
    assert!(clone.has_table());
    // The clone's parse filled the shared slot.
    assert_eq!(reply.table_cached(), Some(true));
    let mut clone = reply.clone();
    clone.set("plain words\n".to_owned());
    assert!(!clone.has_table());
    // The original still holds its own answer.
    assert!(reply.has_table());
    assert_eq!(reply.table_cached(), Some(true));
}

#[test]
fn push_and_set_invalidate_the_table_answer() {
    // Pushing a table onto prose introduces one.
    let mut reply = Reply::new("plain words\n".to_owned(), 1);
    assert!(!reply.has_table());
    reply.push(TABLE);
    assert_eq!(reply.table_cached(), None);
    assert!(reply.has_table());
    // Setting prose back removes it.
    reply.set("plain words\n".to_owned());
    assert_eq!(reply.table_cached(), None);
    assert!(!reply.has_table());
    // A push that leaves the answer detaches the clone's slot.
    let mut clone = Reply::new("plain words\n".to_owned(), 2);
    let shared = clone.clone();
    clone.push(" more\n");
    assert_eq!(clone.table_cached(), None);
    assert_eq!(shared.table_cached(), None);
    assert!(!clone.has_table());
    assert!(!shared.has_table());
}

/// The reply's rows through a seam drawing `fake`.
fn seamed(
    reply: &Reply,
    width: u16,
    fake: &std::rc::Rc<crate::ext::tests::SlotRenderer>,
    flat: bool,
) -> Vec<crate::turn::Row> {
    let mut out =
        crate::rows::Rows::default().with_seam(crate::ext::tests::seam(fake, "s", 0, flat));
    reply.rows(width, &mut out);
    out.into_parts().0
}

fn replaced() -> std::rc::Rc<crate::ext::tests::SlotRenderer> {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Replace(vec![vec![raw("custom")]]),
    ))
}

#[test]
fn a_reply_replacement_draws_instead_of_the_text() {
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = replaced();
    let rows = seamed(&reply, 40, &fake, false);
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    // (`docs/tui.md`, "How a TUI extension runs": each drawn item is cached by input and width.)
    assert_eq!(fake.calls.borrow().len(), 1);
    assert_eq!(fake.calls.borrow()[0].1, "reply");
    assert!(
        shown.iter().any(|row| row.starts_with("custom")),
        "{shown:?}"
    );
    assert!(!shown.iter().any(|row| row.contains("Hello")), "{shown:?}");
}

#[test]
fn a_reply_decorator_draws_the_built_in_text_under_its_badge() {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Decorate {
            badge: vec![vec![raw("BADGE")]],
            trim: 5,
        },
    ));
    let rows = seamed(&reply, 40, &fake, false);
    assert_eq!(fake.builtins.borrow().as_slice(), &[35]);
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        shown.iter().any(|row| row.starts_with("BADGE")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|row| row.contains("Hello")), "{shown:?}");
}

#[test]
fn a_reply_error_falls_back_with_one_failure() {
    use crate::ext::FailureKind;
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Fail {
            kind: FailureKind::HookFailed,
            message: "boom".into(),
        },
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    for _ in 0..2 {
        let mut out = crate::rows::Rows::default().with_seam(bound.clone());
        reply.rows(40, &mut out);
        let shown: Vec<String> = out
            .into_parts()
            .0
            .iter()
            .map(|(line, _)| line.to_string())
            .collect();
        assert!(shown.iter().any(|row| row.contains("Hello")), "{shown:?}");
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
    assert_eq!(listed[0].0, "reply");
    assert_eq!(listed[0].1.kind, FailureKind::HookFailed);
}

#[test]
fn a_reply_timeout_lists_one_extension_failed() {
    use crate::ext::FailureKind;
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Fail {
            kind: FailureKind::ExtensionFailed,
            message: "slow".into(),
        },
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    let mut out = crate::rows::Rows::default().with_seam(bound.clone());
    reply.rows(40, &mut out);
    let listed = bound.cache.take_failures();
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].1.kind, FailureKind::ExtensionFailed);
}

#[test]
fn a_reply_replacement_carries_no_copy_target() {
    let reply = Reply::new("```sh\necho hi\n```".to_owned(), 7);
    // The built-in places one copy target on the code block's header.
    let mut plain = crate::rows::Rows::default();
    reply.rows(40, &mut plain);
    let (built, _) = plain.into_parts();
    assert!(
        built
            .iter()
            .any(|(_, target)| matches!(target, Some(crate::app::Target::Copy { .. }))),
        "the built-in names its code block"
    );
    let fake = replaced();
    let rows = seamed(&reply, 40, &fake, false);
    assert!(!rows.is_empty());
    for (line, target) in &rows {
        assert_eq!(*target, None, "{line:?} keeps no copy target");
    }
}

#[test]
fn a_flat_reply_replacement_drops_every_style() {
    use crate::ext::tests::{SlotRenderer, SlotScript};
    use ratatui::style::Style;
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Replace(vec![vec![crate::ext::RawSpan {
            text: "custom".into(),
            bold: true,
            fg: Some("accent".into()),
            ..crate::ext::RawSpan::default()
        }]]),
    ));
    let rows = seamed(&reply, 40, &fake, true);
    // (`docs/tui.md`, "How a TUI extension runs": in screen-reader mode the terminal drops every style.)
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(shown.iter().any(|row| row.contains("custom")), "{shown:?}");
    for (line, _) in &rows {
        for span in &line.spans {
            assert_eq!(span.style, Style::default(), "{span:?}");
        }
    }
}
