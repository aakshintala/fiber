//! Tests for the session's image ids: first sight wins, a refold
//! keeps them, and `clear` restarts them.

use contract::{Envelope, SessionId};
use serde_json::json;

use super::Pages;

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
        pages.images.parts.get(&1u32).map(|part| part.path.clone()),
        Some("artifacts/b.png".to_owned())
    );
    assert_eq!(
        pages.images.parts.get(&2u32).map(|part| part.path.clone()),
        Some("artifacts/a.png".to_owned())
    );
    // A repeated path keeps its id; a new one takes the next.
    pages.apply(&completed("artifacts/a.png"));
    pages.apply(&completed("artifacts/c.png"));
    assert_eq!(
        pages.images.parts.get(&2u32).map(|part| part.path.clone()),
        Some("artifacts/a.png".to_owned())
    );
    assert_eq!(
        pages.images.parts.get(&3u32).map(|part| part.path.clone()),
        Some("artifacts/c.png".to_owned())
    );
    // A steering message's image is noted too.
    pages.apply(&steered("artifacts/s.png"));
    assert_eq!(
        pages.images.parts.get(&4u32).map(|part| part.path.clone()),
        Some("artifacts/s.png".to_owned())
    );
    // A page refold notes the same paths again and keeps every id:
    // `load` notes each line it folds, and seen paths keep their ids.
    pages.load(&[started(&["artifacts/b.png", "artifacts/a.png"])]);
    assert_eq!(
        pages.images.parts.get(&1u32).map(|part| part.path.clone()),
        Some("artifacts/b.png".to_owned())
    );
    assert_eq!(
        pages.images.parts.get(&2u32).map(|part| part.path.clone()),
        Some("artifacts/a.png".to_owned())
    );
    assert_eq!(pages.images.parts.get(&5u32), None);
    // `clear` restarts at 1 with the new session.
    pages.clear();
    assert_eq!(pages.images.parts.get(&1u32), None);
    pages.apply(&started(&["artifacts/z.png"]));
    assert_eq!(
        pages.images.parts.get(&1u32).map(|part| part.path.clone()),
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
