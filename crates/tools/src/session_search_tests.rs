use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use contract::session_search::{Found, Hit, Label, Query, Scan};
use contract::shapes::{ContentPart, Effect};
use contract::tool::{Bound, Cancel, EffectsError, Output, Tool};
use contract::{ErrorCode, Seq, SessionId};
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

use super::{SessionSearch, utc};

/// A scan that records each query and answers with a fixed `Found`, and
/// fires `cancels` while it runs when one is given.
struct FakeScan {
    queries: Mutex<Vec<Query>>,
    found: Found,
    cancels: Option<CancelToken>,
}

impl FakeScan {
    fn new(found: Found) -> Arc<Self> {
        Arc::new(Self {
            queries: Mutex::new(Vec::new()),
            found,
            cancels: None,
        })
    }

    fn queries(&self) -> Vec<Query> {
        self.queries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Scan for FakeScan {
    fn scope(&self, all_projects: bool) -> PathBuf {
        if all_projects {
            PathBuf::from("/h/projects/")
        } else {
            PathBuf::from("/h/projects/-w/")
        }
    }

    fn scan(&self, query: &Query, _cancel: &dyn Cancel) -> Found {
        self.queries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(query.clone());
        if let Some(token) = &self.cancels {
            token.cancel();
        }
        self.found.clone()
    }
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn tool(scan: &Arc<FakeScan>) -> SessionSearch {
    let scan: Arc<dyn Scan> = scan.clone();
    SessionSearch::new(scan)
}

fn run(scan: &Arc<FakeScan>, arguments: Value) -> Output {
    tool(scan).run(&args(arguments), &CancelToken::new(), &Recorder::default())
}

fn text(output: &Output) -> String {
    match output.content.as_slice() {
        [ContentPart::Text { text }] => text.clone(),
        other => panic!("one text part, got {other:?}"),
    }
}

fn hit(id: &str, name: &str, seq: u64, ts: u64, label: Label, snippet: &str) -> Hit {
    Hit {
        session_id: SessionId(id.to_owned()),
        name: name.to_owned(),
        seq: Seq(seq),
        ts,
        label,
        snippet: snippet.to_owned(),
        log: PathBuf::from(format!("/h/projects/-w/sessions/{id}/events.jsonl")),
        artifact: None,
    }
}

#[test]
fn it_declares_reads_on_the_scope_it_searches() {
    let search = tool(&FakeScan::new(Found::default()));
    for (arguments, path) in [
        (json!({"text": "x"}), "/h/projects/-w/"),
        (
            json!({"text": "x", "all_projects": false}),
            "/h/projects/-w/",
        ),
        (json!({"text": "x", "all_projects": true}), "/h/projects/"),
    ] {
        let effects = search.effects(&args(arguments.clone())).unwrap();

        assert_eq!(effects.declared.effects, [Effect::Reads], "{arguments}");
        assert!(effects.declared.reversible, "{arguments}");
        assert_eq!(
            effects.declared.paths,
            Some(vec![path.to_owned()]),
            "{arguments}"
        );
        assert_eq!(effects.subject.as_deref(), Some(""), "{arguments}");
        assert_eq!(effects.prefix, None, "{arguments}");
    }
}

#[test]
fn effects_refuse_an_all_projects_that_is_not_a_boolean() {
    let search = tool(&FakeScan::new(Found::default()));

    let refused = search.effects(&args(json!({"text": "x", "all_projects": "yes"})));

    assert!(
        matches!(refused, Err(EffectsError::Arguments(ref message)) if message.contains("all_projects")),
        "{refused:?}"
    );
}

#[test]
fn run_passes_the_defaults_and_the_given_values_to_the_scan() {
    let scan = FakeScan::new(Found::default());

    run(&scan, json!({"text": "retry"}));
    run(
        &scan,
        json!({"text": "retry", "all_projects": true, "limit": 3}),
    );
    run(
        &scan,
        json!({"text": "retry", "all_projects": false, "limit": 0}),
    );

    assert_eq!(
        scan.queries(),
        [
            Query {
                text: "retry".to_owned(),
                all_projects: false,
                limit: 20,
            },
            Query {
                text: "retry".to_owned(),
                all_projects: true,
                limit: 3,
            },
            Query {
                text: "retry".to_owned(),
                all_projects: false,
                limit: 0,
            },
        ]
    );
}

#[test]
fn bad_arguments_fail_invalid_arguments_without_scanning() {
    let scan = FakeScan::new(Found::default());
    for arguments in [
        json!({}),
        json!({"text": 3}),
        json!({"text": "x", "limit": "3"}),
        json!({"text": "x", "limit": 1.5}),
        json!({"text": "x", "limit": -1}),
        json!({"text": "x", "all_projects": "yes"}),
    ] {
        let output = run(&scan, arguments.clone());

        assert_eq!(
            output.error.map(|failure| failure.code),
            Some(ErrorCode::InvalidArguments),
            "{arguments}"
        );
    }
    assert_eq!(scan.queries(), []);
}

#[test]
fn no_hits_names_the_scope_searched() {
    let scan = FakeScan::new(Found::default());

    let own = text(&run(&scan, json!({"text": "retry budget"})));
    let all = text(&run(
        &scan,
        json!({"text": "retry budget", "all_projects": true}),
    ));

    assert_eq!(
        own,
        "No hits for \"retry budget\" in this project's sessions.\n"
    );
    assert_eq!(
        all,
        "No hits for \"retry budget\" in any project's sessions.\n"
    );
}

#[test]
fn hits_print_best_first_with_where_to_read_them() {
    let mut artifact = hit(
        "s_past",
        "retry work",
        4,
        1_700_000_004_000,
        Label::ToolOutput,
        "full output: RETRY BUDGET exhausted",
    );
    artifact.artifact = Some(PathBuf::from(
        "/h/projects/-w/sessions/s_past/artifacts/call_1.txt",
    ));
    let scan = FakeScan::new(Found {
        hits: vec![
            hit(
                "s_0193",
                "find the retry budget",
                7,
                1_791_367_203_000,
                Label::ToolInput,
                "retry budget",
            ),
            hit(
                "s_past",
                "retry work",
                1,
                1_700_000_001_999,
                Label::Message,
                "We keep the Retry Budget at 3",
            ),
            artifact,
        ],
        total: 3,
        problems: Vec::new(),
        more_problems: 0,
    });

    let output = run(&scan, json!({"text": "retry budget"}));

    assert_eq!(output.error, None);
    assert_eq!(
        text(&output),
        "3 of 3 hits for \"retry budget\", best first.\n\
         1. tool_input, session s_0193 \"find the retry budget\", seq 7, 2026-10-07T10:00:03Z\n\
         \x20  read /h/projects/-w/sessions/s_0193/events.jsonl from offset 8\n\
         \x20  retry budget\n\
         2. message, session s_past \"retry work\", seq 1, 2023-11-14T22:13:21Z\n\
         \x20  read /h/projects/-w/sessions/s_past/events.jsonl from offset 2\n\
         \x20  We keep the Retry Budget at 3\n\
         3. tool_output, session s_past \"retry work\", seq 4, 2023-11-14T22:13:24Z\n\
         \x20  read /h/projects/-w/sessions/s_past/events.jsonl from offset 5\n\
         \x20  artifact /h/projects/-w/sessions/s_past/artifacts/call_1.txt\n\
         \x20  full output: RETRY BUDGET exhausted\n"
    );
}

#[test]
fn the_header_counts_hits_past_the_limit() {
    let scan = FakeScan::new(Found {
        hits: vec![hit("s_a", "a", 2, 0, Label::Message, "x")],
        total: 3,
        ..Found::default()
    });
    let none_kept = FakeScan::new(Found {
        total: 3,
        ..Found::default()
    });

    let one = text(&run(&scan, json!({"text": "x", "limit": 1})));
    let zero = text(&run(&none_kept, json!({"text": "x", "limit": 0})));

    assert!(
        one.starts_with("1 of 3 hits for \"x\", best first.\n"),
        "{one}"
    );
    assert_eq!(zero, "0 of 3 hits for \"x\", best first.\n");
}

#[test]
fn a_name_longer_than_80_characters_is_cut() {
    let eighty = "é".repeat(80);
    let eighty_one = "é".repeat(81);
    let scan = FakeScan::new(Found {
        hits: vec![
            hit("s_a", &eighty, 2, 0, Label::Message, "x"),
            hit("s_b", &eighty_one, 1, 0, Label::Message, "x"),
        ],
        total: 2,
        ..Found::default()
    });

    let shown = text(&run(&scan, json!({"text": "x"})));

    assert!(
        shown.contains(&format!("session s_a \"{eighty}\", seq 2")),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("session s_b \"{eighty}…\", seq 1")),
        "{shown}"
    );
}

#[test]
fn control_characters_print_as_spaces_on_one_line() {
    let scan = FakeScan::new(Found {
        hits: vec![hit(
            "s_a",
            "two\nlines",
            2,
            0,
            Label::ToolOutput,
            "a\nb\tc\r\u{1b}d",
        )],
        total: 1,
        ..Found::default()
    });

    let shown = text(&run(&scan, json!({"text": "a\nb"})));

    assert_eq!(
        shown,
        "1 of 1 hits for \"a b\", best first.\n\
         1. tool_output, session s_a \"two lines\", seq 2, 1970-01-01T00:00:00Z\n\
         \x20  read /h/projects/-w/sessions/s_a/events.jsonl from offset 3\n\
         \x20  a b c  d\n"
    );
}

#[test]
fn problems_follow_the_hits_with_the_rest_counted() {
    let scan = FakeScan::new(Found {
        hits: vec![hit("s_a", "a", 2, 0, Label::Message, "x")],
        total: 1,
        problems: vec![
            "Could not read: /h/projects/-w/sessions/s_b/events.jsonl, line 3: bad".to_owned(),
            "/h/projects/-w/sessions/s_c is a link".to_owned(),
        ],
        more_problems: 5,
    });
    let without_more = FakeScan::new(Found {
        problems: vec!["/h/projects/-w/sessions/s_c is a link".to_owned()],
        ..Found::default()
    });

    let shown = text(&run(&scan, json!({"text": "x"})));
    let one = text(&run(&without_more, json!({"text": "x"})));

    assert!(
        shown.ends_with(
            "   x\n\
             Could not read: /h/projects/-w/sessions/s_b/events.jsonl, line 3: bad\n\
             /h/projects/-w/sessions/s_c is a link\n\
             And 5 more problems.\n"
        ),
        "{shown}"
    );
    assert_eq!(
        one,
        "No hits for \"x\" in this project's sessions.\n\
         /h/projects/-w/sessions/s_c is a link\n"
    );
}

#[test]
fn a_cancel_seen_after_the_scan_answers_cancelled() {
    let token = CancelToken::new();
    let scan = Arc::new(FakeScan {
        queries: Mutex::new(Vec::new()),
        found: Found {
            hits: vec![hit("s_a", "a", 2, 0, Label::Message, "x")],
            total: 1,
            ..Found::default()
        },
        cancels: Some(token.clone()),
    });

    let output = tool(&scan).run(&args(json!({"text": "x"})), &token, &Recorder::default());

    assert_eq!(text(&output), "Cancelled before it finished.\n");
    assert_eq!(output.error, None);
    assert_eq!(scan.queries().len(), 1);
}

#[test]
fn its_result_is_bounded_like_any_other() {
    let search = tool(&FakeScan::new(Found::default()));

    assert_eq!(search.bound(), Bound::DEFAULT);
}

#[test]
fn utc_prints_seconds_since_the_epoch_as_a_utc_time() {
    for (seconds, printed) in [
        (0, "1970-01-01T00:00:00Z"),
        (951_868_799, "2000-02-29T23:59:59Z"),
        (946_684_799, "1999-12-31T23:59:59Z"),
        (946_684_800, "2000-01-01T00:00:00Z"),
        (1_791_367_203, "2026-10-07T10:00:03Z"),
        (4_107_542_400, "2100-03-01T00:00:00Z"),
    ] {
        assert_eq!(utc(seconds), printed, "{seconds}");
    }
}
