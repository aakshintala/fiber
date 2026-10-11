//! Tests for the rewind note: what the session's tools did after the point,
//! read from the logged effects only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::Path;

use contract::events::{Event, ToolCallRequested, ToolCallStarted};
use contract::shapes::{DeclaredEffects, Effect};
use contract::{ActionId, Envelope, SCHEMA_VERSION, Seq, SessionId};
use serde_json::{Value, json};

use super::*;

fn requested(name: &str, arguments: Value) -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: name.to_owned(),
        arguments,
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

fn started(effects: Vec<Effect>, paths: Option<Vec<&str>>, arguments: Option<Value>) -> Event {
    Event::ToolCallStarted(ToolCallStarted {
        declared: DeclaredEffects {
            effects,
            reversible: true,
            paths: paths.map(|paths| paths.into_iter().map(str::to_owned).collect()),
        },
        arguments: arguments.and_then(|arguments| match arguments {
            Value::Object(map) => Some(map),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_) => None,
        }),
        changed_by: None,
    })
}

fn envelope(seq: u64, action: &str, event: &Event) -> Envelope {
    Envelope {
        kind: event.kind().to_owned(),
        session_id: SessionId("s_1".to_owned()),
        ts: 1_759_150_000_000 + seq,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(ActionId(action.to_owned())),
        seq: Some(Seq(seq)),
        payload: event.payload().unwrap(),
    }
}

fn write_log(dir: &Path, lines: &[Envelope]) {
    fs::create_dir_all(dir).unwrap();
    let mut text = String::new();
    for line in lines {
        text.push_str(&serde_json::to_string(line).unwrap());
        text.push('\n');
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

/// One call after the point: its action, the tool's name, the requested
/// arguments, its effects, its paths, and the arguments that ran, when a
/// hook rewrote them.
type Call<'a> = (
    &'a str,
    &'a str,
    Value,
    Vec<Effect>,
    Option<Vec<&'a str>>,
    Option<Value>,
);

/// `tool_call_requested` lines by action, with `started` lines after the
/// point in `calls`.
fn log_with(dir: &Path, calls: &[Call<'_>]) {
    let mut lines = vec![envelope(
        0,
        "a_0",
        &Event::SessionStarted(contract::events::SessionStarted {
            workspace: "/w".to_owned(),
            variables: contract::events::Variables {
                path: "/usr/bin".to_owned(),
                names: Vec::new(),
                source: contract::events::VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree: None,
        }),
    )];
    for (index, (action, name, arguments, effects, paths, ran)) in calls.iter().enumerate() {
        let seq = u64::try_from(index).unwrap() + 1;
        lines.push(envelope(seq, action, &requested(name, arguments.clone())));
        lines.push(envelope(
            seq + 100,
            action,
            &started(effects.clone(), paths.clone(), ran.clone()),
        ));
    }
    write_log(dir, &lines);
}

fn note_for(calls: &[Call<'_>]) -> String {
    let home = fakes::TempDir::new("loop-rewind-note");
    let dir = home.path().join("s_1");
    log_with(&dir, calls);
    rewind_note(&dir, Seq(100)).unwrap()
}

#[test]
fn no_tool_call_after_the_point_names_nothing() {
    let note = note_for(&[(
        "a_1",
        "read",
        json!({"city": "Paris"}),
        vec![Effect::Reads],
        None,
        None,
    )]);
    assert!(note.contains("No file was written and no command was run after this point."));
}

#[test]
fn paths_written_after_the_point_are_listed() {
    let note = note_for(&[(
        "a_1",
        "write",
        json!({"city": "Paris"}),
        vec![Effect::Writes],
        Some(vec!["/w/a.txt"]),
        None,
    )]);
    assert!(note.contains("Files written after this point:\n- /w/a.txt"));
    assert!(!note.contains("Commands run"));
}

#[test]
fn commands_run_after_the_point_are_listed_with_their_arguments() {
    let note = note_for(&[(
        "a_1",
        "shell",
        json!({"command": "make"}),
        vec![Effect::Executes],
        None,
        None,
    )]);
    assert!(note.contains("Commands run after this point that may have changed files:"));
    assert!(note.contains(r#"- shell: {"command":"make"}"#));
    assert!(!note.contains("Files written"));
}

#[test]
fn written_paths_and_commands_join_with_a_blank_line() {
    let note = note_for(&[
        (
            "a_1",
            "write",
            json!({"city": "Paris"}),
            vec![Effect::Writes],
            Some(vec!["/w/a.txt"]),
            None,
        ),
        (
            "a_2",
            "shell",
            json!({"command": "make"}),
            vec![Effect::Executes],
            None,
            None,
        ),
    ]);
    assert!(note.contains("- /w/a.txt\n\nCommands run after this point"));
}

#[test]
fn duplicate_paths_are_kept_once_in_first_seen_order() {
    let note = note_for(&[
        (
            "a_1",
            "write",
            json!({"city": "Paris"}),
            vec![Effect::Writes],
            Some(vec!["/w/b.txt", "/w/a.txt"]),
            None,
        ),
        (
            "a_2",
            "write",
            json!({"city": "Paris"}),
            vec![Effect::Writes],
            Some(vec!["/w/a.txt", "/w/c.txt"]),
            None,
        ),
    ]);
    assert!(note.contains("- /w/b.txt\n- /w/a.txt\n- /w/c.txt"));
}

#[test]
fn a_writes_call_with_no_paths_is_listed_with_the_commands() {
    let note = note_for(&[(
        "a_1",
        "write",
        json!({"city": "Paris"}),
        vec![Effect::Writes],
        None,
        None,
    )]);
    assert!(note.contains(r#"- write: {"city":"Paris"}"#));
    assert!(!note.contains("Files written"));
}

#[test]
fn a_writes_call_with_an_empty_path_list_is_listed_like_one_with_none() {
    // Empty `paths` names no file, so the call is listed with the commands,
    // as a call with no `paths` is. A non-empty list is listed by path.
    let call = |paths: Option<Vec<&str>>| {
        note_for(&[(
            "a_1",
            "write",
            json!({"city": "Paris"}),
            vec![Effect::Writes],
            paths,
            None,
        )])
    };
    let empty = call(Some(Vec::new()));
    assert_eq!(empty, call(None));
    assert!(empty.contains(r#"- write: {"city":"Paris"}"#));
    assert_ne!(empty, call(Some(vec!["/w/a.txt"])));
}

#[test]
fn hook_rewritten_arguments_win_over_the_requested_ones() {
    let note = note_for(&[(
        "a_1",
        "shell",
        json!({"command": "make check"}),
        vec![Effect::Executes],
        None,
        Some(json!({"command": "make"})),
    )]);
    assert!(note.contains(r#"- shell: {"command":"make"}"#));
    assert!(!note.contains("make check"));
}

#[test]
fn a_call_before_the_point_is_not_listed() {
    let home = fakes::TempDir::new("loop-rewind-note-point");
    let dir = home.path().join("s_1");
    log_with(
        &dir,
        &[(
            "a_1",
            "shell",
            json!({"command": "make"}),
            vec![Effect::Executes],
            None,
            None,
        )],
    );
    let note = rewind_note(&dir, Seq(101)).unwrap();
    assert!(note.contains("No file was written and no command was run after this point."));
}

/// Standing rules that allow nothing and remember nothing.
struct NoRules;

impl contract::rules::Rules for NoRules {
    fn read(&self) -> Result<contract::rules::StandingRules, contract::rules::RulesError> {
        Ok(contract::rules::StandingRules {
            global: Vec::new(),
            project: Vec::new(),
        })
    }

    fn remember(
        &self,
        _tool: &str,
        _prefix: &str,
        _session: &contract::SessionId,
    ) -> Result<(), contract::rules::RulesError> {
        Ok(())
    }
}

#[test]
fn a_rewound_loop_takes_its_thinking_and_its_trigger_from_the_logged_build() {
    // The build's thinking level is the session's own choice, and the
    // trigger is set without waiting for a preamble build that never
    // comes.
    use contract::events::{
        CacheLifetime, PreambleBuilt, PreambleReason, SessionStarted, Variables, VariablesSource,
    };
    let root = fakes::TempDir::new("loop-rewound-trigger");
    let home = root.path().to_path_buf();
    let parent = home.join("s_parent00000001");
    std::fs::create_dir_all(&parent).unwrap();
    let line = |kind: &str, seq: u64, event: &Event| Envelope {
        kind: kind.to_owned(),
        session_id: SessionId("s_parent00000001".to_owned()),
        ts: 1_759_150_000_000 + seq,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(seq)),
        payload: event.payload().unwrap(),
    };
    let built = Event::PreambleBuilt(PreambleBuilt {
        reason: PreambleReason::Start,
        model: "fake/model-1".to_owned(),
        context_window: 200_000,
        trigger_at: None,
        budget: None,
        thinking: Some("high".to_owned()),
        tool_choice: "auto".to_owned(),
        cache_lifetime: CacheLifetime::OneHour,
        credential: None,
        system_prompt: String::new(),
        tools: Vec::new(),
        replaced: Vec::new(),
    });
    let mut text = serde_json::to_string(&line(
        "session_started",
        0,
        &Event::SessionStarted(SessionStarted {
            workspace: "/w".to_owned(),
            variables: Variables {
                path: "/usr/bin".to_owned(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree: None,
        }),
    ))
    .unwrap();
    text.push('\n');
    text.push_str(&serde_json::to_string(&line("preamble_built", 1, &built)).unwrap());
    text.push('\n');
    std::fs::write(parent.join("events.jsonl"), text).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let log = std::sync::Arc::new(
        log::Log::create(&home, SessionId("s_child00000001".into()), clock.clone()).unwrap(),
    );
    let dir = log.dir().to_path_buf();
    let clock: std::sync::Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = crate::PromptInputs::new(
        home.clone(),
        "/bin/sh".to_owned(),
        dir.join("events.jsonl").display().to_string(),
        clock,
        100_000,
    );
    prompt.credential = Some("work".to_owned());
    let (tx, rx) = std::sync::mpsc::channel();
    let _tx = tx;
    let rules: std::sync::Arc<dyn contract::rules::Rules> = std::sync::Arc::new(NoRules);
    let looped = Loop::rewound(
        crate::Session {
            log,
            provider: std::sync::Arc::new(fakes::ScriptedProvider::new(Vec::new())),
            model: crate::Model {
                reference: "fake/model-1".to_owned(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: home.display().to_string(),
                credentials: home.clone(),
                credential_files: Vec::new(),
                rules,
            },
        },
        Rewound {
            from: contract::shapes::Point {
                session_id: SessionId("s_parent00000001".to_owned()),
                seq: Seq(1),
            },
            note: String::new(),
            worktree: None,
            name: None,
        },
    )
    .unwrap();
    assert_eq!(looped.chosen, Some(contract::ThinkingLevel::High));
    // The default handoff settings against a 100,000-token window: the
    // window fraction loses to the token trigger.
    assert_eq!(looped.handoff.trigger_at, Some(70_000));
}

#[test]
fn a_rewound_loop_writes_no_model_changed_for_a_later_label_switch() {
    // The fold sees a credential `model_changed` after the point, but the
    // rewound session continues the logged build verbatim, so it writes no
    // `model_changed` (`docs/events.md`, "Rewind").
    use contract::events::{
        CacheLifetime, ModelChanged, ModelSettings, PreambleBuilt, PreambleReason, SessionStarted,
        SwitchSource, Variables, VariablesSource,
    };
    let root = fakes::TempDir::new("loop-rewound-credential");
    let home = root.path().to_path_buf();
    let parent = home.join("s_parent00000001");
    std::fs::create_dir_all(&parent).unwrap();
    let line = |kind: &str, seq: u64, event: &Event| Envelope {
        kind: kind.to_owned(),
        session_id: SessionId("s_parent00000001".to_owned()),
        ts: 1_759_150_000_000 + seq,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(seq)),
        payload: event.payload().unwrap(),
    };
    let built = Event::PreambleBuilt(PreambleBuilt {
        reason: PreambleReason::Start,
        model: "fake/model-1".to_owned(),
        context_window: 200_000,
        trigger_at: None,
        budget: None,
        thinking: None,
        tool_choice: "auto".to_owned(),
        cache_lifetime: CacheLifetime::OneHour,
        credential: Some("work".to_owned()),
        system_prompt: String::new(),
        tools: Vec::new(),
        replaced: Vec::new(),
    });
    let switched = Event::ModelChanged(ModelChanged {
        before: ModelSettings {
            model: "fake/model-1".to_owned(),
            thinking: None,
            cache_lifetime: CacheLifetime::OneHour,
            credential: Some("work".to_owned()),
        },
        after: ModelSettings {
            model: "fake/model-1".to_owned(),
            thinking: None,
            cache_lifetime: CacheLifetime::OneHour,
            credential: Some("home".to_owned()),
        },
        source: SwitchSource::Driver,
    });
    let mut text = serde_json::to_string(&line(
        "session_started",
        0,
        &Event::SessionStarted(SessionStarted {
            workspace: "/w".to_owned(),
            variables: Variables {
                path: "/usr/bin".to_owned(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree: None,
        }),
    ))
    .unwrap();
    text.push('\n');
    text.push_str(&serde_json::to_string(&line("preamble_built", 1, &built)).unwrap());
    text.push('\n');
    text.push_str(&serde_json::to_string(&line("model_changed", 2, &switched)).unwrap());
    text.push('\n');
    std::fs::write(parent.join("events.jsonl"), text).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let log = std::sync::Arc::new(
        log::Log::create(&home, SessionId("s_child00000001".into()), clock.clone()).unwrap(),
    );
    let dir = log.dir().to_path_buf();
    let clock: std::sync::Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = crate::PromptInputs::new(
        home.clone(),
        "/bin/sh".to_owned(),
        dir.join("events.jsonl").display().to_string(),
        clock,
        100_000,
    );
    prompt.credential = Some("work".to_owned());
    let (tx, rx) = std::sync::mpsc::channel();
    let _tx = tx;
    let rules: std::sync::Arc<dyn contract::rules::Rules> = std::sync::Arc::new(NoRules);
    Loop::rewound(
        crate::Session {
            log,
            provider: std::sync::Arc::new(fakes::ScriptedProvider::new(Vec::new())),
            model: crate::Model {
                reference: "fake/model-1".to_owned(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: home.display().to_string(),
                credentials: home.clone(),
                credential_files: Vec::new(),
                rules,
            },
        },
        Rewound {
            from: contract::shapes::Point {
                session_id: SessionId("s_parent00000001".to_owned()),
                seq: Seq(2),
            },
            note: String::new(),
            worktree: None,
            name: None,
        },
    )
    .unwrap();
    let kinds: Vec<String> = log::read(&dir)
        .unwrap()
        .iter()
        .map(|line| line.kind.clone())
        .collect();
    // The fold ends on the switch's `after`, but the session continues
    // the build at the point, whose label is unchanged: no `model_changed`.
    assert_eq!(kinds, ["session_started"]);
}

/// A parent log holding `events` in order, and the rewound child loop
/// continuing it from `point_seq`: the child's directory and loop. Copies
/// the `Loop::rewound` setup above: the session-started preamble the fold
/// needs, the injected fake clock, and the standing rules that allow
/// nothing.
fn rewound_from_parent(
    home: &Path,
    parent_id: &str,
    child_id: &str,
    events: Vec<(&str, Event)>,
    point_seq: u64,
) -> (std::path::PathBuf, Loop) {
    let parent = home.join(parent_id);
    std::fs::create_dir_all(&parent).unwrap();
    let mut text = String::new();
    for (index, (kind, event)) in events.iter().enumerate() {
        let seq = u64::try_from(index).unwrap();
        let line = Envelope {
            kind: (*kind).to_owned(),
            session_id: SessionId(parent_id.to_owned()),
            ts: 1_759_150_000_000 + seq,
            schema_version: SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: Some(Seq(seq)),
            payload: event.payload().unwrap(),
        };
        text.push_str(&serde_json::to_string(&line).unwrap());
        text.push('\n');
    }
    std::fs::write(parent.join("events.jsonl"), text).unwrap();
    // The copy under test: the parent's latest name rides the start.
    let name = latest_name(&parent);
    let clock = fakes::clock::FakeClock::new();
    let log = std::sync::Arc::new(
        log::Log::create(home, SessionId(child_id.to_owned()), clock.clone()).unwrap(),
    );
    let dir = log.dir().to_path_buf();
    let clock: std::sync::Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = crate::PromptInputs::new(
        home.to_path_buf(),
        "/bin/sh".to_owned(),
        dir.join("events.jsonl").display().to_string(),
        clock,
        100_000,
    );
    prompt.credential = Some("work".to_owned());
    let (tx, rx) = std::sync::mpsc::channel();
    let _tx = tx;
    let rules: std::sync::Arc<dyn contract::rules::Rules> = std::sync::Arc::new(NoRules);
    let looped = Loop::rewound(
        crate::Session {
            log,
            provider: std::sync::Arc::new(fakes::ScriptedProvider::new(Vec::new())),
            model: crate::Model {
                reference: "fake/model-1".to_owned(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: home.display().to_string(),
                credentials: home.to_path_buf(),
                credential_files: Vec::new(),
                rules,
            },
        },
        Rewound {
            from: contract::shapes::Point {
                session_id: SessionId(parent_id.to_owned()),
                seq: Seq(point_seq),
            },
            note: String::new(),
            worktree: None,
            name,
        },
    )
    .unwrap();
    (dir, looped)
}

fn parent_started() -> Event {
    use contract::events::{SessionStarted, Variables, VariablesSource};
    Event::SessionStarted(SessionStarted {
        workspace: "/w".to_owned(),
        variables: Variables {
            path: "/usr/bin".to_owned(),
            names: Vec::new(),
            source: VariablesSource::Inherited,
        },
        parent: None,
        forked_from: None,
        rewind: None,
        worktree: None,
    })
}

fn parent_named(name: &str, by: contract::events::NamedBy) -> Event {
    Event::SessionNamed(contract::events::SessionNamed {
        name: Some(name.to_owned()),
        by,
    })
}

fn parent_built() -> Event {
    use contract::events::{CacheLifetime, PreambleBuilt, PreambleReason};
    Event::PreambleBuilt(PreambleBuilt {
        reason: PreambleReason::Start,
        model: "fake/model-1".to_owned(),
        context_window: 200_000,
        trigger_at: None,
        budget: None,
        thinking: None,
        tool_choice: "auto".to_owned(),
        cache_lifetime: CacheLifetime::OneHour,
        credential: None,
        system_prompt: String::new(),
        tools: Vec::new(),
        replaced: Vec::new(),
    })
}

fn pinned(looped_dir: &Path) -> bool {
    use contract::events::{CallStatus, Control};
    let folded = crate::resume::resumed(looped_dir).unwrap();
    let control = Control {
        handoff: None,
        questions: None,
        skill: None,
        name: Some("a later name".to_owned()),
    };
    matches!(
        folded.naming.judge(CallStatus::Completed, Some(&control)),
        crate::naming::Named::Pinned(_)
    )
}

#[test]
fn a_rewound_loop_copies_the_parents_person_name_and_pins() {
    // The name goes with the conversation: the new log holds the parent's
    // `session_named` verbatim after its `session_started`, and the pin
    // holds, so a later `name_session` call is refused.
    use contract::events::NamedBy;
    let root = fakes::TempDir::new("loop-rewound-name");
    let home = root.path().to_path_buf();
    let (dir, _looped) = rewound_from_parent(
        &home,
        "s_parent00000001",
        "s_child00000001",
        vec![
            ("session_started", parent_started()),
            ("session_named", parent_named("Mine", NamedBy::Person)),
        ],
        1,
    );
    let lines = log::read(&dir).unwrap();
    let kinds: Vec<String> = lines.iter().map(|line| line.kind.clone()).collect();
    assert_eq!(kinds, ["session_started", "session_named"]);
    let Some(Event::SessionNamed(named)) = Event::from_envelope(&lines[1]).unwrap() else {
        panic!("the second line is the copied session_named");
    };
    assert_eq!(named.name.as_deref(), Some("Mine"));
    assert_eq!(named.by, NamedBy::Person);
    assert!(pinned(&dir));
}

#[test]
fn a_rewound_loop_copies_the_latest_session_named_past_other_lines() {
    // Reading from the end backwards, past a non-name line: the latest
    // `session_named` wins, `by` included, so the model's later name
    // replaces the person's earlier one.
    use contract::events::NamedBy;
    let root = fakes::TempDir::new("loop-rewound-latest-name");
    let home = root.path().to_path_buf();
    let (dir, _looped) = rewound_from_parent(
        &home,
        "s_parent00000001",
        "s_child00000001",
        vec![
            ("session_started", parent_started()),
            ("session_named", parent_named("Mine", NamedBy::Person)),
            ("preamble_built", parent_built()),
            ("session_named", parent_named("Later", NamedBy::Model)),
        ],
        3,
    );
    let lines = log::read(&dir).unwrap();
    let kinds: Vec<String> = lines.iter().map(|line| line.kind.clone()).collect();
    assert_eq!(kinds, ["session_started", "session_named"]);
    let Some(Event::SessionNamed(named)) = Event::from_envelope(&lines[1]).unwrap() else {
        panic!("the second line is the copied session_named");
    };
    assert_eq!(named.name.as_deref(), Some("Later"));
    assert_eq!(named.by, NamedBy::Model);
    assert!(!pinned(&dir));
}

#[test]
fn a_rewound_loop_with_an_unnamed_parent_writes_no_session_named() {
    // No `session_named` in the parent: the new log holds only its
    // `session_started`, and nothing pins the session.
    let root = fakes::TempDir::new("loop-rewound-no-name");
    let home = root.path().to_path_buf();
    let (dir, _looped) = rewound_from_parent(
        &home,
        "s_parent00000001",
        "s_child00000001",
        vec![
            ("session_started", parent_started()),
            ("preamble_built", parent_built()),
        ],
        1,
    );
    let lines = log::read(&dir).unwrap();
    let kinds: Vec<String> = lines.iter().map(|line| line.kind.clone()).collect();
    assert_eq!(kinds, ["session_started"]);
    assert!(!pinned(&dir));
}

/// `latest_name` reads the latest `session_named` in the log in `dir`,
/// past any other line: the last line here names nothing, but the latest
/// name still wins, `by` included.
#[test]
fn latest_name_reads_the_latest_session_named_past_other_lines() {
    use contract::events::NamedBy;
    let root = fakes::TempDir::new("loop-latest-name");
    let dir = root.path().join("s_1");
    write_log(
        &dir,
        &[
            envelope(0, "a_0", &parent_started()),
            envelope(1, "a_1", &parent_named("First", NamedBy::Person)),
            envelope(2, "a_2", &parent_built()),
            envelope(3, "a_3", &parent_named("Second", NamedBy::Model)),
            envelope(4, "a_4", &parent_built()),
        ],
    );
    assert_eq!(
        latest_name(&dir),
        Some(contract::events::SessionNamed {
            name: Some("Second".to_owned()),
            by: NamedBy::Model,
        })
    );
}

/// `latest_name` is `None` when the log holds no `session_named`, or no
/// lines at all.
#[test]
fn latest_name_is_none_without_a_session_named() {
    let root = fakes::TempDir::new("loop-latest-no-name");
    let dir = root.path().join("s_1");
    write_log(
        &dir,
        &[
            envelope(0, "a_0", &parent_started()),
            envelope(1, "a_1", &parent_built()),
        ],
    );
    assert!(latest_name(&dir).is_none());
    let empty = root.path().join("s_empty");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(empty.join("events.jsonl"), "").unwrap();
    assert!(latest_name(&empty).is_none());
}

/// `latest_name` is `None` when the log cannot be read: no directory, or
/// a file where the directory should be.
#[test]
fn latest_name_is_none_when_the_log_cannot_be_read() {
    let root = fakes::TempDir::new("loop-latest-unreadable");
    assert!(latest_name(&root.path().join("s_missing")).is_none());
    let file = root.path().join("s_file");
    std::fs::write(&file, "not a directory").unwrap();
    assert!(latest_name(&file).is_none());
}
