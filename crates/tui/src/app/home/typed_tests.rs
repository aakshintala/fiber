//! Tests for the picker's typed path: splitting, matching, joining,
//! queueing, answers, resolve, disconnect and focus.

use super::{Focus, Resolved, Typed, join, matching, split};
use serde_json::json;

/// A mint handing out `c_1`, `c_2`, … in order.
struct Mint {
    next: usize,
}

impl Mint {
    fn run(&mut self, typed: &mut Typed) -> Vec<(String, String)> {
        typed.due(|| {
            self.next += 1;
            format!("c_{}", self.next)
        })
    }
}

fn mint() -> Mint {
    Mint { next: 0 }
}

/// Types `text` one character at a time.
fn type_text(typed: &mut Typed, text: &str) {
    for ch in text.chars() {
        typed.push(ch);
    }
}

/// A `directories` result listing `names` at `path`.
fn result(names: &[&str], path: &str) -> serde_json::Value {
    json!({"directories": names, "path": path})
}

/// Answers `id` with `names` at `path`.
fn answer(typed: &mut Typed, id: &str, names: &[&str], path: &str) -> bool {
    typed.answered(id, Some(&result(names, path)))
}

#[test]
fn split_names_the_directory_and_the_name() {
    assert_eq!(split("/a/b"), Some(("/a/", "b")));
    assert_eq!(split("~/w"), Some(("~/", "w")));
    assert_eq!(split("/"), Some(("/", "")));
    assert_eq!(split("~/"), Some(("~/", "")));
    assert_eq!(split("/a/"), Some(("/a/", "")));
    assert_eq!(split("~/a/b"), Some(("~/a/", "b")));
}

#[test]
fn split_rejects_text_without_a_directory() {
    assert_eq!(split("~"), None);
    assert_eq!(split(""), None);
    assert_eq!(split("a/b"), None);
    assert_eq!(split("~x/y"), None);
    assert_eq!(split("~x"), None);
    assert_eq!(split("foo"), None);
}

#[test]
fn matching_filters_by_prefix_case_sensitively() {
    let names = vec!["a1".to_owned(), "a2".to_owned(), "b".to_owned()];
    let got: Vec<&str> = matching(&names, "a").collect();
    assert_eq!(got, vec!["a1", "a2"]);
    let got: Vec<&str> = matching(&names, "A").collect();
    assert!(got.is_empty());
}

#[test]
fn matching_hides_hidden_names_until_the_dot() {
    let names = vec![".git".to_owned(), "a1".to_owned()];
    let got: Vec<&str> = matching(&names, "").collect();
    assert_eq!(got, vec!["a1"]);
    let got: Vec<&str> = matching(&names, ".").collect();
    assert_eq!(got, vec![".git"]);
    let got: Vec<&str> = matching(&names, ".g").collect();
    assert_eq!(got, vec![".git"]);
}

#[test]
fn matching_keeps_the_hub_order() {
    let names = vec!["b".to_owned(), "a".to_owned()];
    let got: Vec<&str> = matching(&names, "").collect();
    assert_eq!(got, vec!["b", "a"]);
}

#[test]
fn join_trims_the_path_before_adding_the_name() {
    assert_eq!(join("/a/", "b"), "/a/b");
    assert_eq!(join("/a", "b"), "/a/b");
    assert_eq!(join("/", "b"), "/b");
    assert_eq!(join("//", "b"), "/b");
}

#[test]
fn resolve_opens_root_from_a_held_listing() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    assert_eq!(sent.len(), 1);
    assert!(answer(&mut typed, &sent[0].0, &[], "/"));
    assert!(matches!(typed.resolve(), Resolved::Open(path) if path == "/"));
}

#[test]
fn resolve_opens_root_from_a_double_slash() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "//");
    let sent = ids.run(&mut typed);
    assert_eq!(sent.len(), 2);
    assert!(answer(&mut typed, &sent[1].0, &[], "/"));
    assert!(matches!(typed.resolve(), Resolved::Open(path) if path == "/"));
}

#[test]
fn resolve_opens_a_trailing_slash_from_its_parent_listing() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a/b/");
    let sent = ids.run(&mut typed);
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/", "/a/", "/a/b/"]
    );
    assert!(answer(&mut typed, &sent[2].0, &[], "/a/b/"));
    assert!(answer(&mut typed, &sent[0].0, &["b"], "/"));
    // The parent names `b`, but `/a/` is not listed yet: loading.
    assert!(matches!(typed.resolve(), Resolved::Loading));
    assert!(answer(&mut typed, &sent[1].0, &["b"], "/a/"));
    assert!(matches!(typed.resolve(), Resolved::Open(path) if path == "/a/b"));
}

#[test]
fn resolve_reports_missing_when_the_parent_omits_the_name() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a/b/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[2].0, &[], "/a/b/"));
    assert!(answer(&mut typed, &sent[1].0, &["c"], "/a/"));
    assert!(matches!(typed.resolve(), Resolved::Missing));
    assert!(typed.missing());
}

#[test]
fn resolve_waits_for_the_current_listing_before_the_parent() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a/b/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[1].0, &["b"], "/a/"));
    // `/a/` names `b`, but `/a/b/` itself is not listed yet: loading.
    assert!(matches!(typed.resolve(), Resolved::Loading));
    assert!(!typed.missing());
}

#[test]
fn resolve_reports_missing_when_the_parent_listing_failed() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a/b/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[2].0, &[], "/a/b/"));
    assert!(typed.rejected(&sent[1].0));
    assert!(matches!(typed.resolve(), Resolved::Missing));
}

#[test]
fn resolve_reports_missing_without_a_parent_lookup_for_a_full_name() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a/zz");
    let sent = ids.run(&mut typed);
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/", "/a/"]
    );
    assert!(answer(&mut typed, &sent[1].0, &[], "/a/"));
    assert!(matches!(typed.resolve(), Resolved::Missing));
    assert!(typed.queued.is_empty());
}

#[test]
fn resolve_reports_missing_for_relative_text_with_no_request() {
    for text in ["foo", "~x/", "~x"] {
        let mut typed = Typed::new();
        let mut ids = mint();
        type_text(&mut typed, text);
        assert!(ids.run(&mut typed).is_empty());
        assert!(matches!(typed.resolve(), Resolved::Missing));
        assert!(typed.missing(), "{text} shows no such directory");
        assert!(typed.queued.is_empty());
    }
}

#[test]
fn resolve_opens_home_once_its_listing_answers() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "~");
    assert!(ids.run(&mut typed).is_empty());
    assert!(matches!(typed.resolve(), Resolved::Loading));
    assert_eq!(typed.queued, &["~/".to_owned()]);
    // A second Enter before the answer queues nothing more.
    assert!(matches!(typed.resolve(), Resolved::Loading));
    assert_eq!(typed.queued, &["~/".to_owned()]);
    let sent = ids.run(&mut typed);
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["~/"]
    );
    // In flight, Enter queues nothing.
    assert!(matches!(typed.resolve(), Resolved::Loading));
    assert!(answer(&mut typed, &sent[0].0, &[], "/Users/me/"));
    assert!(matches!(typed.resolve(), Resolved::Open(path) if path == "/Users/me"));
}

#[test]
fn resolve_opens_root_for_a_home_path_of_slash() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "~");
    assert!(matches!(typed.resolve(), Resolved::Loading));
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &[], "/"));
    assert!(matches!(typed.resolve(), Resolved::Open(path) if path == "/"));
}

#[test]
fn resolve_opens_a_home_completion_through_the_listed_path() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "~/tree/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &["tree"], "/root/"));
    assert!(answer(&mut typed, &sent[1].0, &[], "/root/tree/"));
    assert!(matches!(typed.resolve(), Resolved::Open(path) if path == "/root/tree"));
}

#[test]
fn resolve_reports_missing_when_the_home_listing_failed() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "~");
    assert!(matches!(typed.resolve(), Resolved::Loading));
    let sent = ids.run(&mut typed);
    assert!(typed.rejected(&sent[0].0));
    assert!(matches!(typed.resolve(), Resolved::Missing));
    assert!(typed.missing());
}

#[test]
fn typing_queues_one_listing_per_directory() {
    let mut typed = Typed::new();
    let mut ids = mint();
    for ch in ['/', 'a', 'b', '/', 'c'] {
        typed.push(ch);
    }
    assert_eq!(typed.text(), "/ab/c");
    let sent = ids.run(&mut typed);
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/", "/ab/"]
    );
}

#[test]
fn typing_inside_a_name_queues_nothing_more() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/tmp/f");
    let sent = ids.run(&mut typed);
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/", "/tmp/"]
    );
    type_text(&mut typed, "i");
    assert!(ids.run(&mut typed).is_empty());
}

#[test]
fn bare_tilde_and_relative_text_queue_nothing() {
    let mut typed = Typed::new();
    let mut ids = mint();
    typed.push('~');
    assert!(ids.run(&mut typed).is_empty());
    typed.push('x');
    assert!(ids.run(&mut typed).is_empty());
}

#[test]
fn typing_tilde_slash_queues_the_home_listing() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "~/");
    let sent = ids.run(&mut typed);
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["~/"]
    );
}

#[test]
fn a_held_in_flight_or_queued_directory_is_not_queued_twice() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a/");
    let sent = ids.run(&mut typed);
    // `/` and `/a/` are in flight: retyping queues nothing.
    assert_eq!(typed.text(), "/a/");
    assert!(ids.run(&mut typed).is_empty());
    assert!(answer(&mut typed, &sent[0].0, &["a"], "/"));
    // `/` is held now: backspacing and retyping queues nothing for it.
    assert!(typed.pop());
    assert!(typed.pop());
    assert_eq!(typed.text(), "/");
    assert!(ids.run(&mut typed).is_empty());
    // `/a/` is still in flight: typing back queues nothing.
    type_text(&mut typed, "a/");
    assert!(ids.run(&mut typed).is_empty());
    assert!(answer(&mut typed, &sent[1].0, &[], "/a/"));
    assert!(ids.run(&mut typed).is_empty());
}

#[test]
fn backspace_to_empty_clears_the_queue_and_keeps_the_rest() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a");
    assert_eq!(ids.run(&mut typed).len(), 1);
    assert!(typed.pop());
    assert_eq!(typed.text(), "/");
    assert!(!typed.pop());
    assert!(typed.queued.is_empty());
    assert_eq!(typed.inflight.len(), 1);
}

#[test]
fn due_drains_in_order_with_the_minted_ids() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a/b/");
    let sent = ids.run(&mut typed);
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].0, "c_1");
    assert_eq!(sent[1].0, "c_2");
    assert_eq!(sent[2].0, "c_3");
    assert_eq!(
        typed.inflight,
        &[
            ("c_1".to_owned(), "/".to_owned()),
            ("c_2".to_owned(), "/a/".to_owned()),
            ("c_3".to_owned(), "/a/b/".to_owned()),
        ]
    );
}

#[test]
fn answers_store_a_well_formed_listing() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &["b", "a"], "/x/"));
    assert_eq!(typed.completions(), vec!["b", "a"]);
}

#[test]
fn answers_store_an_empty_listing_for_rejections_and_malformed_results() {
    let malformed = [
        json!({"path": "/x/"}),
        json!({"directories": "a", "path": "/x/"}),
        json!({"directories": ["a", 3], "path": "/x/"}),
        json!({"directories": ["a"]}),
        json!({"directories": ["a"], "path": 3}),
        json!({"directories": ["a"], "path": "x/"}),
        json!({"directories": ["a"], "path": ""}),
        json!(["a"]),
        json!(3),
    ];
    for result in malformed {
        let mut typed = Typed::new();
        let mut ids = mint();
        type_text(&mut typed, "/");
        let sent = ids.run(&mut typed);
        assert!(typed.answered(&sent[0].0, Some(&result)));
        assert!(typed.completions().is_empty(), "result {result}");
        // A failed listing is held: retyping asks nothing more.
        assert!(!typed.pop());
        type_text(&mut typed, "/");
        assert!(ids.run(&mut typed).is_empty(), "result {result}");
    }
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    assert!(typed.rejected(&sent[0].0));
    assert!(typed.completions().is_empty());
}

#[test]
fn answers_for_unknown_ids_change_nothing() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    assert!(!typed.answered("c_999", Some(&result(&["a"], "/"))));
    assert!(!typed.rejected("c_999"));
    assert!(typed.completions().is_empty());
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &["a"], "/"));
    // The same id twice: the second answers nothing.
    assert!(!answer(&mut typed, &sent[0].0, &["b"], "/"));
    assert_eq!(typed.completions(), vec!["a"]);
}

#[test]
fn disconnect_requeues_in_flight_directories_in_send_order() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a/");
    let sent = ids.run(&mut typed);
    assert_eq!(sent.len(), 2);
    typed.disconnected();
    assert!(typed.inflight.is_empty());
    assert_eq!(typed.queued, &["/".to_owned(), "/a/".to_owned()]);
    // Old ids match nothing after the disconnect.
    assert!(!answer(&mut typed, &sent[0].0, &["a"], "/"));
    let resent = ids.run(&mut typed);
    assert_eq!(resent.len(), 2);
    assert_ne!(resent[0].0, sent[0].0);
    assert_eq!(
        resent
            .iter()
            .map(|(_, dir)| dir.as_str())
            .collect::<Vec<_>>(),
        vec!["/", "/a/"]
    );
}

#[test]
fn disconnect_keeps_queued_directories_behind_the_retried_ones() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    assert_eq!(sent.len(), 1);
    type_text(&mut typed, "a/");
    assert_eq!(typed.queued, &["/a/".to_owned()]);
    typed.disconnected();
    assert_eq!(typed.queued, &["/".to_owned(), "/a/".to_owned()]);
}

#[test]
fn disconnect_keeps_the_cache() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &["a"], "/"));
    typed.disconnected();
    let resent = ids.run(&mut typed);
    assert_eq!(
        resent
            .iter()
            .map(|(_, dir)| dir.as_str())
            .collect::<Vec<_>>(),
        vec!["/a/"]
    );
}

#[test]
fn forget_drops_in_flight_ids_and_keeps_the_cache() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &["a"], "/"));
    typed.forget();
    assert!(typed.inflight.is_empty());
    assert!(typed.queued.is_empty());
    assert!(!answer(&mut typed, &sent[1].0, &["x"], "/a/"));
    // The cache stays: back at `/`, its held listing asks nothing more.
    assert!(typed.pop());
    assert!(typed.pop());
    assert_eq!(typed.text(), "/");
    assert!(ids.run(&mut typed).is_empty());
}

#[test]
fn a_keystroke_focuses_the_first_matching_completion() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &["a1", "a2"], "/"));
    assert_eq!(typed.focus(), &Focus::Completion("a1".to_owned()));
    typed.push('a');
    assert_eq!(typed.text(), "/a");
    assert_eq!(typed.focus(), &Focus::Completion("a1".to_owned()));
    typed.push('2');
    assert_eq!(typed.focus(), &Focus::Completion("a2".to_owned()));
    typed.push('z');
    assert_eq!(typed.focus(), &Focus::Nothing);
}

#[test]
fn a_late_listing_focuses_nothing_into_its_first_completion() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    assert_eq!(typed.focus(), &Focus::Nothing);
    assert!(answer(&mut typed, &sent[0].0, &["a1", "a2"], "/"));
    assert_eq!(typed.focus(), &Focus::Completion("a1".to_owned()));
}

#[test]
fn a_late_listing_keeps_a_focused_recent() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    typed.move_focus(true, 2);
    assert_eq!(typed.focus(), &Focus::Recent(0));
    assert!(answer(&mut typed, &sent[0].0, &["a1", "a2"], "/"));
    assert_eq!(typed.focus(), &Focus::Recent(0));
}

#[test]
fn arrows_walk_completions_then_recents_and_clamp() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &["a1", "a2"], "/"));
    typed.move_focus(true, 2);
    assert_eq!(typed.focus(), &Focus::Completion("a2".to_owned()));
    typed.move_focus(true, 2);
    assert_eq!(typed.focus(), &Focus::Recent(0));
    typed.move_focus(true, 2);
    assert_eq!(typed.focus(), &Focus::Recent(1));
    typed.move_focus(true, 2);
    assert_eq!(typed.focus(), &Focus::Recent(1));
    typed.move_focus(false, 2);
    assert_eq!(typed.focus(), &Focus::Recent(0));
    typed.move_focus(false, 2);
    assert_eq!(typed.focus(), &Focus::Completion("a2".to_owned()));
    typed.move_focus(false, 2);
    assert_eq!(typed.focus(), &Focus::Completion("a1".to_owned()));
    typed.move_focus(false, 2);
    assert_eq!(typed.focus(), &Focus::Completion("a1".to_owned()));
}

#[test]
fn down_from_nothing_focuses_the_first_row() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/x");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &[], "/"));
    assert_eq!(typed.focus(), &Focus::Nothing);
    typed.move_focus(true, 2);
    assert_eq!(typed.focus(), &Focus::Recent(0));
}

#[test]
fn a_late_listing_keeps_a_deeper_recent() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    typed.move_focus(true, 2);
    typed.move_focus(true, 2);
    assert_eq!(typed.focus(), &Focus::Recent(1));
    assert!(answer(&mut typed, &sent[0].0, &["a1"], "/"));
    assert_eq!(typed.focus(), &Focus::Recent(1));
}

#[test]
fn up_from_nothing_stays_above_the_list() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/x");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &[], "/"));
    typed.move_focus(false, 2);
    assert_eq!(typed.focus(), &Focus::Nothing);
}

#[test]
fn tab_completes_the_focused_completion_with_a_trailing_slash() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &["a1", "a2"], "/"));
    assert!(typed.tab());
    assert_eq!(typed.text(), "/a1/");
    let asked = ids.run(&mut typed);
    assert_eq!(
        asked
            .iter()
            .map(|(_, dir)| dir.as_str())
            .collect::<Vec<_>>(),
        vec!["/a1/"]
    );
    assert_eq!(typed.focus(), &Focus::Nothing);
    assert!(answer(&mut typed, &asked[0].0, &["deep"], "/a1/"));
    // Typing into the completed directory focuses its first entry.
    typed.push('d');
    assert_eq!(typed.focus(), &Focus::Completion("deep".to_owned()));
}

#[test]
fn tab_onto_a_held_directory_focuses_its_first_completion() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &["a1"], "/"));
    // Answer the queued `/a1/` listing before Tab, so it is held.
    assert!(typed.tab());
    let asked = ids.run(&mut typed);
    assert!(answer(&mut typed, &asked[0].0, &["deep"], "/a1/"));
    // Back to `/` and Tab again: `/a1/` is held, so focus lands at once.
    assert!(typed.pop());
    assert!(typed.pop());
    assert!(typed.pop());
    assert_eq!(typed.text(), "/");
    assert_eq!(typed.focus(), &Focus::Completion("a1".to_owned()));
    assert!(typed.tab());
    assert_eq!(typed.text(), "/a1/");
    assert_eq!(typed.focus(), &Focus::Completion("deep".to_owned()));
    assert!(ids.run(&mut typed).is_empty());
}

#[test]
fn tab_without_a_focused_completion_changes_nothing() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/x");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &[], "/"));
    assert!(!typed.tab());
    assert_eq!(typed.text(), "/x");
    assert!(ids.run(&mut typed).is_empty());
    typed.move_focus(true, 1);
    assert_eq!(typed.focus(), &Focus::Recent(0));
    assert!(!typed.tab());
    assert_eq!(typed.text(), "/x");
}

#[test]
fn missing_clears_on_the_next_keystroke() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/zz");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &[], "/"));
    assert!(matches!(typed.resolve(), Resolved::Missing));
    assert!(typed.missing());
    typed.push('z');
    assert!(!typed.missing());
    assert!(matches!(typed.resolve(), Resolved::Missing));
    assert!(typed.missing());
    typed.pop();
    assert!(!typed.missing());
}

#[test]
fn the_recent_is_the_focused_recent_index() {
    let mut typed = Typed::new();
    let mut ids = mint();
    type_text(&mut typed, "/a");
    let sent = ids.run(&mut typed);
    assert!(answer(&mut typed, &sent[0].0, &[], "/"));
    assert_eq!(typed.recent(), None);
    typed.move_focus(true, 2);
    assert_eq!(typed.recent(), Some(0));
    typed.move_focus(true, 2);
    assert_eq!(typed.recent(), Some(1));
}
