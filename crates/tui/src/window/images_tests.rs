//! Tests for the session's image ids: first sight wins, a refold
//! keeps them, and `clear` restarts them.

use contract::{Envelope, SessionId};
use serde_json::json;

use super::Pages;

#[global_allocator]
static ALLOC: fakes::alloc::Counting = fakes::alloc::Counting;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

fn envelope(kind: &str, payload: serde_json::Value) -> Envelope {
    Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

fn started(paths: &[&str]) -> Envelope {
    let content: Vec<serde_json::Value> = paths
        .iter()
        .map(|path| {
            json!({"type": "image", "path": path,
                "mime_type": "image/png", "width": 1280, "height": 800})
        })
        .collect();
    envelope(
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver", "content": content,
        }]}),
    )
}

fn completed(path: &str) -> Envelope {
    envelope(
        "tool_call_completed",
        json!({"status": "completed",
            "content": [{"type": "image", "path": path,
                "mime_type": "image/png", "width": 640, "height": 480}]}),
    )
}

fn steered(path: &str) -> Envelope {
    envelope(
        "steering_applied",
        json!({"content": [{"type": "image", "path": path,
                "mime_type": "image/png", "width": 320, "height": 200}],
            "source": "driver"}),
    )
}

fn pages() -> Pages {
    Pages::new(60)
}

#[test]
fn ids_follow_first_sight_and_survive_a_refold() {
    let mut pages = pages();
    // Two paths take 1 and 2, in the order first seen.
    pages.apply(&started(&["artifacts/b.png", "artifacts/a.png"]));
    assert_eq!(
        pages.image(1).map(|part| part.path.clone()),
        Some("artifacts/b.png".to_owned())
    );
    assert_eq!(
        pages.image(2).map(|part| part.path.clone()),
        Some("artifacts/a.png".to_owned())
    );
    // A repeated path keeps its id; a new one takes the next.
    pages.apply(&completed("artifacts/a.png"));
    pages.apply(&completed("artifacts/c.png"));
    assert_eq!(
        pages.image(2).map(|part| part.path.clone()),
        Some("artifacts/a.png".to_owned())
    );
    assert_eq!(
        pages.image(3).map(|part| part.path.clone()),
        Some("artifacts/c.png".to_owned())
    );
    // A steering message's image is noted too.
    pages.apply(&steered("artifacts/s.png"));
    assert_eq!(
        pages.image(4).map(|part| part.path.clone()),
        Some("artifacts/s.png".to_owned())
    );
    // A page refold notes the same paths again and keeps every id:
    // `load` notes each line it folds, and seen paths keep their ids.
    pages.load(
        &SessionId(SESSION.to_owned()),
        &[started(&["artifacts/b.png", "artifacts/a.png"])],
    );
    assert_eq!(
        pages.image(1).map(|part| part.path.clone()),
        Some("artifacts/b.png".to_owned())
    );
    assert_eq!(
        pages.image(2).map(|part| part.path.clone()),
        Some("artifacts/a.png".to_owned())
    );
    assert_eq!(pages.image(5), None);
    // `clear` restarts at 1 with the new session.
    pages.clear();
    assert_eq!(pages.image(1), None);
    pages.apply(&started(&["artifacts/z.png"]));
    assert_eq!(
        pages.image(1).map(|part| part.path.clone()),
        Some("artifacts/z.png".to_owned())
    );
}

#[test]
fn clear_keeps_the_text_sizing() {
    let mut pages = pages();
    pages.apply(&started(&["artifacts/a.png"]));
    pages.clear();
    assert_eq!(pages.images.sizing, crate::image::Sizing::Text);
}

#[test]
fn an_image_line_counts_one_row_and_names_its_target() {
    use crate::app::Target;
    let mut pages = pages();
    pages.apply(&started(&["artifacts/a.png"]));
    let rows: Vec<(String, Option<Target>)> = pages
        .rows()
        .into_iter()
        .map(|(line, target)| (line.to_string(), target))
        .collect();
    let found = rows
        .iter()
        .filter(|(text, _)| text.contains("a.png"))
        .collect::<Vec<_>>();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "▣ a.png · 1280×800");
    assert!(matches!(found[0].1, Some(Target::Image(1))));
}

/// The other segment: the chain's first session.
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

#[test]
fn one_path_noted_under_two_sessions_draws_two_ids() {
    let mut pages = pages();
    pages.note_images(&started(&["artifacts/shot.png"]));
    let mut ancestor = started(&["artifacts/shot.png"]);
    ancestor.session_id = SessionId(OTHER.to_owned());
    pages.note_images(&ancestor);
    let one = pages
        .images
        .id(&SessionId(SESSION.to_owned()), "artifacts/shot.png")
        .expect("the own image draws");
    let two = pages
        .images
        .id(&SessionId(OTHER.to_owned()), "artifacts/shot.png")
        .expect("the ancestor image draws");
    assert_ne!(one, two, "one path in two segments draws two ids");
    assert_eq!(
        pages.image(one).map(|part| part.session.clone()),
        Some(SessionId(SESSION.to_owned()))
    );
    assert_eq!(
        pages.image(two).map(|part| part.session.clone()),
        Some(SessionId(OTHER.to_owned()))
    );
    // A refold keeps both ids.
    pages.note_images(&ancestor);
    assert_eq!(
        pages
            .images
            .id(&SessionId(OTHER.to_owned()), "artifacts/shot.png"),
        Some(two)
    );
}

/// A call whose result carried the image at `path`: a ledger row, so
/// inline sizing grids it.
fn called(path: &str) -> Vec<Envelope> {
    use contract::Seq;
    let image = json!({"type": "image", "path": path,
        "mime_type": "image/png", "width": 1280, "height": 800});
    let mut seq = 0;
    let mut acted = |kind: &str, action: Option<&str>, payload: serde_json::Value| {
        seq += 1;
        Envelope {
            kind: kind.to_owned(),
            session_id: SessionId(SESSION.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: action.map(|action| contract::ActionId(action.to_owned())),
            seq: Some(Seq(seq)),
            payload: payload.as_object().cloned().unwrap_or_default(),
        }
    };
    vec![
        acted(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "look"}]}]}),
        ),
        acted("step_started", None, json!({})),
        acted(
            "tool_call_requested",
            Some("a_t"),
            json!({"name": "read", "arguments": {"path": path}}),
        ),
        acted(
            "tool_call_completed",
            Some("a_t"),
            json!({"status": "completed", "content": [image]}),
        ),
    ]
}

#[test]
fn inline_sizing_counts_the_placeholder_grid() {
    use crate::image::Sizing;
    let mut pages = pages();
    for line in called("artifacts/a.png") {
        pages.apply(&line);
    }
    // The ledger draws once its group is open.
    assert!(
        pages.open(&crate::app::Target::Group(crate::turn::target_id("a_t"))),
        "the call's group opens"
    );
    let text = pages.rows().len();
    // 1280 by 800 at 60 columns with a 10 by 20 cell: 24 by 7
    // placeholders, then the caption line.
    pages.set_image_sizing(Sizing::Inline { cell: (10, 20) });
    let rows = pages.rows();
    assert_eq!(rows.len(), text + 7, "the grid replaces the line");
    let placeholders = rows
        .iter()
        .filter(|(line, _)| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().starts_with('\u{10EEEE}'))
        })
        .count();
    assert_eq!(placeholders, 7);
    assert!(
        rows.iter()
            .any(|(line, _)| line.to_string().ends_with("▣ a.png · 1280×800")),
        "the caption still counts its row"
    );
}

#[test]
fn set_image_sizing_recounts_only_on_change() {
    use crate::image::Sizing;
    let mut pages = pages();
    pages.apply(&started(&["artifacts/a.png"]));
    let recounts = pages.recounts;
    // The same sizing recounts nothing.
    pages.set_image_sizing(Sizing::Text);
    assert_eq!(pages.recounts, recounts);
    // Inline sizing recounts the resident page.
    pages.set_image_sizing(Sizing::Inline { cell: (10, 20) });
    assert!(pages.recounts > recounts, "the sizing change recounts");
    let recounts = pages.recounts;
    pages.set_image_sizing(Sizing::Inline { cell: (10, 20) });
    assert_eq!(pages.recounts, recounts);
    // A new cell size recounts again.
    pages.set_image_sizing(Sizing::Inline { cell: (8, 16) });
    assert!(pages.recounts > recounts, "the new cell recounts");
    let recounts = pages.recounts;
    pages.set_image_sizing(Sizing::Text);
    assert!(pages.recounts > recounts, "sizing back recounts");
}

#[test]
fn flat_reports_text_and_draws_the_one_line() {
    use crate::image::Sizing;
    let mut pages = pages();
    for line in called("artifacts/a.png") {
        pages.apply(&line);
    }
    assert!(
        pages.open(&crate::app::Target::Group(crate::turn::target_id("a_t"))),
        "the call's group opens"
    );
    pages.set_image_sizing(Sizing::Inline { cell: (10, 20) });
    let inline = pages.rows().len();
    assert!(inline > 2, "the grid takes rows: {inline}");
    // Flat reports text, so no display fetch goes out for the page.
    pages.set_flat(true);
    assert_eq!(pages.image_sizing(), Sizing::Text);
    // The flat seam draws the image as its one clickable line.
    let rows = pages.rows();
    let found: Vec<_> = rows
        .iter()
        .filter(|(line, _)| line.to_string().contains("▣ a.png"))
        .collect();
    assert_eq!(found.len(), 1);
    assert!(found[0].0.to_string().ends_with("▣ a.png · 1280×800"));
    assert!(rows.len() < inline, "the flat page loses the grid rows");
}

#[test]
fn a_failed_image_recounts_to_its_one_line() {
    use crate::image::Sizing;
    let mut pages = pages();
    for line in called("artifacts/a.png") {
        pages.apply(&line);
    }
    assert!(
        pages.open(&crate::app::Target::Group(crate::turn::target_id("a_t"))),
        "the call's group opens"
    );
    pages.set_image_sizing(Sizing::Inline { cell: (10, 20) });
    let inline = pages.rows().len();
    assert!(inline > 2, "the grid takes rows: {inline}");
    // Failing the image recounts its resident page to the one-line form.
    let recounts = pages.recounts;
    pages.fail_image(&SessionId(SESSION.to_owned()), "artifacts/a.png");
    assert!(pages.recounts > recounts, "the failure recounts");
    let rows = pages.rows();
    assert!(rows.len() < inline, "the failed image loses the grid rows");
    let found: Vec<_> = rows
        .iter()
        .filter(|(line, _)| line.to_string().contains("▣ a.png"))
        .collect();
    assert_eq!(found.len(), 1);
    assert!(found[0].0.to_string().ends_with("▣ a.png · 1280×800"));
    // The failure names one image in one session.
    assert!(pages.image_failed(&SessionId(SESSION.to_owned()), "artifacts/a.png"));
    assert!(!pages.image_failed(&SessionId(SESSION.to_owned()), "artifacts/b.png"));
    // Clearing the session drops the failure with the layout.
    pages.clear();
    assert!(!pages.image_failed(&SessionId(SESSION.to_owned()), "artifacts/a.png"));
}

/// An unrelated large line never reaches the image decoder: folding it
/// adds no image and holds no payload-sized allocation. The fixture is
/// built before measurement, so only the fold itself is counted.
#[test]
fn an_unrelated_large_line_adds_no_image_and_holds_no_payload_sized_allocation() {
    use std::hint::black_box;
    // The counting allocator is installed: one large block counts one,
    // so a low count below cannot pass vacuously without it.
    let ((), probe) = fakes::alloc::bytes_during(|| {
        drop(black_box(Vec::<u8>::with_capacity(fakes::alloc::LARGE)));
    });
    assert!(
        probe.peak() >= fakes::alloc::LARGE,
        "the probe peaks at its block: {}",
        probe.peak()
    );
    let mut pages = pages();
    pages.note_images(&started(&["artifacts/a.png"]));
    let seeded = pages
        .images
        .id(&SessionId(SESSION.to_owned()), "artifacts/a.png")
        .expect("the seeded image draws");
    let big = "x".repeat(fakes::alloc::LARGE + 1);
    let unrelated = envelope("text_completed", json!({"text": big}));
    drop(big);
    let ((), bytes) = fakes::alloc::bytes_during(|| pages.note_images(black_box(&unrelated)));
    assert!(
        bytes.peak() < fakes::alloc::LARGE,
        "the unrelated fold peaks below one payload: {}",
        bytes.peak()
    );
    assert_eq!(
        pages
            .images
            .id(&SessionId(SESSION.to_owned()), "artifacts/a.png"),
        Some(seeded),
        "the seeded image keeps its id"
    );
    // The draw path resolves the seeded id to its path: the fold noted
    // nothing else (`docs/tui.md`, "Images").
    assert_eq!(
        pages.image(seeded).map(|part| part.path.clone()),
        Some("artifacts/a.png".to_owned()),
        "the seeded image still draws"
    );
}

/// An unknown kind adds no image: the fold returns before decoding.
#[test]
fn an_unknown_kind_adds_no_image() {
    let mut pages = pages();
    pages.note_images(&started(&["artifacts/a.png"]));
    let seeded = pages
        .images
        .id(&SessionId(SESSION.to_owned()), "artifacts/a.png")
        .expect("the seeded image draws");
    pages.note_images(&envelope("some_future_kind", json!({"text": "hi"})));
    assert_eq!(
        pages
            .images
            .id(&SessionId(SESSION.to_owned()), "artifacts/a.png"),
        Some(seeded),
        "the seeded image keeps its id"
    );
    // The draw path resolves the seeded id to its path: the fold noted
    // nothing else (`docs/tui.md`, "Images").
    assert_eq!(
        pages.image(seeded).map(|part| part.path.clone()),
        Some("artifacts/a.png".to_owned()),
        "the seeded image still draws"
    );
}

/// A malformed image-bearing line adds no image: a payload that fails
/// to decode notes nothing.
#[test]
fn a_malformed_image_line_adds_no_image() {
    let mut pages = pages();
    pages.note_images(&started(&["artifacts/a.png"]));
    let seeded = pages
        .images
        .id(&SessionId(SESSION.to_owned()), "artifacts/a.png")
        .expect("the seeded image draws");
    pages.note_images(&envelope(
        "tool_call_completed",
        json!({"status": "completed", "content": "not-an-array"}),
    ));
    assert_eq!(
        pages
            .images
            .id(&SessionId(SESSION.to_owned()), "artifacts/a.png"),
        Some(seeded),
        "the seeded image keeps its id"
    );
    // The draw path resolves the seeded id to its path: the fold noted
    // nothing else (`docs/tui.md`, "Images").
    assert_eq!(
        pages.image(seeded).map(|part| part.path.clone()),
        Some("artifacts/a.png".to_owned()),
        "the seeded image still draws"
    );
}
