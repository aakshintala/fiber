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

use contract::events::{
    Empty, Environment, Event, FiberStarted, HandoffCompleted, HandoffStarted, HandoffTrigger,
    InputItem, Note, OpeningMessage, Outcome, TextCompleted, ToolCallRequested, ToolCallStarted,
    TurnStarted, Variables, VariablesSource,
};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender};
use contract::{ActionId, Envelope, SCHEMA_VERSION, Seq, SessionId};
use fakes::NoRules;
use serde_json::{Value, json};

use super::*;

#[test]
fn fork_workspace_is_the_exact_paragraph_and_does_not_refill_paths() {
    assert_eq!(
        fork_workspace("/fork/{parent_workspace}", "/parent/{workspace}"),
        "Your workspace is /fork/{parent_workspace}, a worktree of /parent/{workspace}. Paths above under /parent/{workspace} are at the same place under yours; work only in yours."
    );
    let notice = fork_workspace("/fork", "/parent");
    let task = fork_task(&SessionId("s_parent".into()), 1, "check it", &notice);
    assert!(
        task.contains(&format!("\n\n{notice}\n\nNobody is present")),
        "{task}"
    );
    assert!(task.ends_with("Your task:\n\ncheck it"));
    assert_eq!(task.matches(&notice).count(), 1);
}

#[test]
fn fork_task_is_the_ruled_message_with_the_unattended_paragraph() {
    assert_eq!(
        fork_task(&SessionId("s_parent00000001".into()), 1, "check it", ""),
        "Fiber: you are a fork of session s_parent00000001, started at delegate depth 1. The conversation above is your parent's, up to the point where it called `delegate_fork`. From here you work alone, and your final message goes back to your parent. Jobs started above belong to your parent, not to you.\n\nNobody is present to answer questions in this session. Work through to the end on your own judgment, and state every assumption you made in your final reply. Ask with `ask_user` only when you cannot go on without an answer: asking ends the run, and the caller answers by resuming the session.\n\nYour task:\n\ncheck it"
    );
}

#[test]
fn fork_task_names_its_depth() {
    assert!(
        fork_task(&SessionId("s_parent".into()), 2, "check it", "")
            .contains("started at delegate depth 2.")
    );
}

#[test]
fn fork_task_leaves_braces_in_the_prompt_alone() {
    let text = fork_task(
        &SessionId("s_parent".into()),
        1,
        "use {depth} and {unattended} and {prompt}",
        "",
    );
    assert!(text.ends_with("Your task:\n\nuse {depth} and {unattended} and {prompt}"));
    assert_eq!(text.matches("Your task:").count(), 1);
}

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
        always_reviewed: false,
        declared: DeclaredEffects {
            effects,
            reversible: true,
            paths: paths.map(|paths| paths.into_iter().map(str::to_owned).collect()),
            confined: false,
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
            supervised: false,
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
            supervised: false,
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
            factory: None,
            from: contract::shapes::Point {
                session_id: SessionId("s_parent00000001".to_owned()),
                seq: Seq(1),
                project: None,
            },
            worktree: None,
            how: Continuation::Rewind {
                note: String::new(),
                summary: None,
                name: None,
            },
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
            supervised: false,
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
            factory: None,
            from: contract::shapes::Point {
                session_id: SessionId("s_parent00000001".to_owned()),
                seq: Seq(2),
                project: None,
            },
            worktree: None,
            how: Continuation::Rewind {
                note: String::new(),
                summary: None,
                name: None,
            },
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
            factory: None,
            from: contract::shapes::Point {
                session_id: SessionId(parent_id.to_owned()),
                seq: Seq(point_seq),
                project: None,
            },
            worktree: None,
            how: Continuation::Rewind {
                note: String::new(),
                summary: None,
                name,
            },
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
        supervised: false,
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

use super::command::Target;
use crate::handoff::Carry;
/// Fold tests for a handoff inherited by position (`docs/events.md`,
/// "Rewind"): a handoff completed in the old session at or before the
/// point applies to the new session, and one after it does not. The logs
/// below are hand-written in the shape a live handoff writes: a turn's
/// input, `handoff_started`, the note's text, `handoff_completed` naming
/// the note's action, then the `opening_message`.
use contract::{ErrorCode, TurnId};

const HA: &str = "s_aaaaaaaaaaaaaaaa";
const HB: &str = "s_bbbbbbbbbbbbbbbb";

fn handoff_envelope(
    session: &str,
    seq: u64,
    turn: Option<&str>,
    action: Option<&str>,
    event: &Event,
) -> Envelope {
    Envelope {
        kind: event.kind().to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 1_759_150_000_000 + seq,
        schema_version: SCHEMA_VERSION,
        turn_id: turn.map(|turn| TurnId(turn.to_owned())),
        action_id: action.map(|action| ActionId(action.to_owned())),
        seq: Some(Seq(seq)),
        payload: event.payload().unwrap(),
    }
}

fn handoff_session_started(from: Option<(&str, u64)>) -> Event {
    Event::SessionStarted(contract::events::SessionStarted {
        workspace: "/w".to_owned(),
        variables: Variables {
            path: "/usr/bin".to_owned(),
            names: Vec::new(),
            source: VariablesSource::Inherited,
        },
        parent: None,
        forked_from: from.map(|(session, seq)| Point {
            session_id: SessionId(session.to_owned()),
            seq: Seq(seq),
            project: None,
        }),
        rewind: None,
        worktree: None,
        supervised: false,
    })
}

fn handoff_turn(text: &str) -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: None,
            },
            changed_by: None,
        }],
    })
}

fn handoff_step() -> Event {
    Event::StepStarted(Empty {})
}

fn handoff_begun() -> Event {
    Event::HandoffStarted(HandoffStarted {
        trigger: HandoffTrigger::Auto,
    })
}

fn handoff_note_text(text: &str) -> Event {
    Event::TextCompleted(TextCompleted {
        text: text.to_owned(),
        provider_item: None,
    })
}

fn handoff_done(outcome: Outcome, note: Option<&str>) -> Event {
    Event::HandoffCompleted(HandoffCompleted {
        outcome,
        error: None,
        note: note.map(|action| Note::Actions {
            note: vec![ActionId(action.to_owned())],
        }),
        tokens_before: 101,
        instructions: None,
    })
}

fn handoff_opening() -> Event {
    Event::OpeningMessage(OpeningMessage {
        environment: Environment {
            date: "2023-11-14".to_owned(),
            os: "test-os".to_owned(),
            arch: "test-arch".to_owned(),
            shell: "/bin/sh".to_owned(),
            workspace: "/w".to_owned(),
            git: None,
            session_log: "/w/sessions/s_aaaaaaaaaaaaaaaa/events.jsonl".to_owned(),
        },
        instruction_files: Vec::new(),
        extension_sections: Vec::new(),
        skills: Vec::new(),
    })
}

/// The old session: turn 1, a handoff turn 2 carrying "the handoff note",
/// then turn 3. The handoff turn's `turn_started` is seq 3, its completion
/// seq 6, the opening seq 7.
fn handoff_log() -> Vec<Envelope> {
    vec![
        handoff_envelope(HA, 0, None, None, &handoff_session_started(None)),
        handoff_envelope(HA, 1, Some("t_1"), None, &handoff_turn("one")),
        handoff_envelope(HA, 2, Some("t_1"), None, &handoff_step()),
        handoff_envelope(HA, 3, Some("t_2"), None, &handoff_turn("two")),
        handoff_envelope(HA, 4, Some("t_2"), None, &handoff_begun()),
        handoff_envelope(
            HA,
            5,
            Some("t_2"),
            Some("a_note"),
            &handoff_note_text("the handoff note"),
        ),
        handoff_envelope(
            HA,
            6,
            Some("t_2"),
            None,
            &handoff_done(Outcome::Completed, Some("a_note")),
        ),
        handoff_envelope(HA, 7, None, None, &handoff_opening()),
        handoff_envelope(HA, 8, Some("t_2"), None, &handoff_step()),
        handoff_envelope(HA, 9, Some("t_3"), None, &handoff_turn("three")),
        handoff_envelope(HA, 10, Some("t_3"), None, &handoff_step()),
    ]
}

fn write_chain(sessions: &Path, session: &str, lines: &[Envelope]) {
    let dir = sessions.join(session);
    fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for line in lines {
        text.push_str(&serde_json::to_string(line).unwrap());
        text.push('\n');
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

/// Parent A holding the handoff log, child B forked from A at `at`: the
/// fold of B to its first line, and the window's rendered conversation.
fn folded_at(sessions: &Path, at: u64) -> (crate::resume::Resumed, Vec<contract::provider::Input>) {
    write_chain(sessions, HA, &handoff_log());
    write_chain(
        sessions,
        HB,
        &[handoff_envelope(
            HB,
            0,
            None,
            None,
            &handoff_session_started(Some((HA, at))),
        )],
    );
    let folded = crate::history::forked(&sessions.join(HB), Seq(0)).unwrap();
    let log = log::Log::open(
        sessions,
        SessionId(HB.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = crate::history::read_window(&log, folded.window, folded.end).unwrap();
    let conversation = crate::conversation::rebuild(&lines, "fake/model-1").unwrap();
    (folded, conversation)
}

/// The seed a resume window starts from when the parent segments held the
/// person's messages `texts`, each the first line of its segment: the
/// reviewer's index of them, and nothing else.
fn seeded(texts: &[&str]) -> Carry {
    Carry {
        person: texts
            .iter()
            .map(|text| {
                (
                    contract::events::KeptMessage {
                        seq: Seq(1),
                        item: 0,
                    },
                    user_input(text),
                )
            })
            .collect(),
        ..Carry::default()
    }
}

fn user_input(text: &str) -> contract::provider::Input {
    contract::provider::Input::User {
        text: text.to_owned(),
        images: Vec::new(),
    }
}

#[test]
fn a_handoff_before_the_point_applies_to_the_fork() {
    let home = fakes::TempDir::new("loop-handoff-before");
    let sessions = home.path().join("sessions");
    // Past the opening message, at a step boundary: the handoff is at or
    // before the point, so the fork inherits it.
    let (folded, conversation) = folded_at(&sessions, 8);
    // The window is turn 2's `turn_started` in the parent segment, and no
    // job or session log preceded it, so the seed is the default.
    assert_eq!(folded.window, (0, 3));
    assert_eq!(folded.seed, seeded(&["one"]));
    // The carried input, then the note, under the new opening message.
    assert_eq!(conversation.len(), 3);
    assert!(
        matches!(&conversation[0], contract::provider::Input::User { text, .. } if text.starts_with("This message is from Fiber")),
        "the opening message renders first: {conversation:?}"
    );
    assert_eq!(conversation[1], user_input("two"));
    assert_eq!(conversation[2], user_input("the handoff note"));
}

#[test]
fn a_handoff_after_the_point_does_not_apply_to_the_fork() {
    let home = fakes::TempDir::new("loop-handoff-after");
    let sessions = home.path().join("sessions");
    // At turn 1's closing step, before the handoff turn: the handoff is
    // after the point, so the fork keeps the earlier context.
    let (folded, conversation) = folded_at(&sessions, 2);
    assert_eq!(folded.window, (0, 0));
    assert_eq!(folded.seed, Carry::default());
    // The conversation the same lines render with the handoff lines past
    // the point deleted: turn 1's input alone, with no note anywhere.
    let kept: Vec<Envelope> = handoff_log()
        .into_iter()
        .filter(|line| line.seq.is_some_and(|seq| seq.0 <= 2))
        .collect();
    assert_eq!(
        conversation,
        crate::conversation::rebuild(&kept, "fake/model-1").unwrap()
    );
    assert_eq!(conversation, [user_input("one")]);
}

#[test]
fn a_point_on_the_handoffs_completion_applies_it() {
    let home = fakes::TempDir::new("loop-handoff-point");
    let sessions = home.path().join("sessions");
    // The point is inclusive: folded at the `handoff_completed` line
    // itself, the handoff applies.
    let (folded, conversation) = folded_at(&sessions, 6);
    assert_eq!(folded.window, (0, 3));
    assert_eq!(folded.seed, seeded(&["one"]));
    // The carried input and the note, with no opening message: that line
    // is past the point.
    assert_eq!(
        conversation,
        [user_input("two"), user_input("the handoff note")]
    );
    // One line earlier, on the note's own line, it does not: the
    // completion is past the point, so the window stays at the chain's
    // start and the note line is truncated back off it.
    let (earlier, before) = folded_at(&sessions, 5);
    assert_eq!(earlier.window, (0, 0));
    assert_eq!(earlier.seed, Carry::default());
    assert_eq!(before, [user_input("one"), user_input("two")]);
}

#[test]
fn the_command_door_refuses_the_handoffs_completion_line() {
    let home = fakes::TempDir::new("loop-handoff-door");
    let sessions = home.path().join("sessions");
    write_chain(&sessions, HA, &handoff_log());
    let dir = sessions.join(HA);
    let lines = log::read(&dir).unwrap();
    let target = Target {
        session: SessionId(HA.to_owned()),
        dir: dir.clone(),
        bound: None,
    };
    // The line after the completion is the opening message, so the
    // completion names no step boundary.
    let refused = super::command::point_for(&lines, &target, Some(Seq(6)), false).unwrap_err();
    assert_eq!(refused.code, ErrorCode::NotStepBoundary);
    // Neither does the note's own line: a handoff's lines are next.
    let refused = super::command::point_for(&lines, &target, Some(Seq(5)), false).unwrap_err();
    assert_eq!(refused.code, ErrorCode::NotStepBoundary);
    // A real boundary still opens: the step the handoff turn started.
    assert_eq!(
        super::command::point_for(&lines, &target, Some(Seq(2)), false).unwrap(),
        2
    );
}

#[test]
fn a_handoff_started_with_no_completion_does_not_apply() {
    // The process died mid-handoff: `handoff_started` and the note's text
    // are logged, then a new process's `fiber_started`, and the turn went
    // on. With no `handoff_completed` before the point, the handoff did
    // not take effect.
    let home = fakes::TempDir::new("loop-handoff-unfinished");
    let sessions = home.path().join("sessions");
    let died = Event::FiberStarted(FiberStarted {
        version: "0.0.1".to_owned(),
        resumed: false,
    });
    write_chain(
        &sessions,
        HA,
        &[
            handoff_envelope(HA, 0, None, None, &handoff_session_started(None)),
            handoff_envelope(HA, 1, Some("t_1"), None, &handoff_turn("one")),
            handoff_envelope(HA, 2, Some("t_1"), None, &handoff_step()),
            handoff_envelope(HA, 3, Some("t_2"), None, &handoff_turn("two")),
            handoff_envelope(HA, 4, Some("t_2"), None, &handoff_begun()),
            handoff_envelope(
                HA,
                5,
                Some("t_2"),
                Some("a_note"),
                &handoff_note_text("the lost note"),
            ),
            handoff_envelope(HA, 6, None, None, &died),
            handoff_envelope(HA, 7, Some("t_2"), None, &handoff_step()),
            handoff_envelope(HA, 8, Some("t_3"), None, &handoff_turn("three")),
        ],
    );
    write_chain(
        &sessions,
        HB,
        &[handoff_envelope(
            HB,
            0,
            None,
            None,
            &handoff_session_started(Some((HA, 8))),
        )],
    );
    let folded = crate::history::forked(&sessions.join(HB), Seq(0)).unwrap();
    assert_eq!(folded.window, (0, 0));
    assert_eq!(folded.seed, Carry::default());
    let log = log::Log::open(
        &sessions,
        SessionId(HB.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = crate::history::read_window(&log, folded.window, folded.end).unwrap();
    let conversation = crate::conversation::rebuild(&lines, "fake/model-1").unwrap();
    // The open window closes at the new process: the note lines never
    // stay, and the context is what it was before the handoff.
    assert_eq!(
        conversation,
        [user_input("one"), user_input("two"), user_input("three")]
    );
}

#[test]
fn a_handoff_with_another_outcome_does_not_apply() {
    // The handoff failed: its completion names the note's action, but the
    // outcome is not `completed`, so the fork keeps the earlier context.
    let home = fakes::TempDir::new("loop-handoff-failed");
    let sessions = home.path().join("sessions");
    write_chain(
        &sessions,
        HA,
        &[
            handoff_envelope(HA, 0, None, None, &handoff_session_started(None)),
            handoff_envelope(HA, 1, Some("t_1"), None, &handoff_turn("one")),
            handoff_envelope(HA, 2, Some("t_1"), None, &handoff_step()),
            handoff_envelope(HA, 3, Some("t_2"), None, &handoff_turn("two")),
            handoff_envelope(HA, 4, Some("t_2"), None, &handoff_begun()),
            handoff_envelope(
                HA,
                5,
                Some("t_2"),
                Some("a_note"),
                &handoff_note_text("the failed note"),
            ),
            handoff_envelope(
                HA,
                6,
                Some("t_2"),
                None,
                &handoff_done(Outcome::Failed, Some("a_note")),
            ),
            handoff_envelope(HA, 7, Some("t_2"), None, &handoff_step()),
            handoff_envelope(HA, 8, Some("t_3"), None, &handoff_turn("three")),
        ],
    );
    write_chain(
        &sessions,
        HB,
        &[handoff_envelope(
            HB,
            0,
            None,
            None,
            &handoff_session_started(Some((HA, 8))),
        )],
    );
    let folded = crate::history::forked(&sessions.join(HB), Seq(0)).unwrap();
    assert_eq!(folded.window, (0, 0));
    assert_eq!(folded.seed, Carry::default());
    let log = log::Log::open(
        &sessions,
        SessionId(HB.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = crate::history::read_window(&log, folded.window, folded.end).unwrap();
    let conversation = crate::conversation::rebuild(&lines, "fake/model-1").unwrap();
    assert_eq!(
        conversation,
        [user_input("one"), user_input("two"), user_input("three")]
    );
    assert!(
        conversation.iter().all(|input| !matches!(input, contract::provider::Input::User { text, .. } if text.contains("the failed note"))),
        "no note reaches the fork: {conversation:?}"
    );
}

const HC: &str = "s_cccccccccccccccc";

/// A chain A -> B -> C with the handoff in the middle segment B: the fold
/// of C to its first line, and the window's rendered conversation. B is
/// forked from A at 1; C is forked from B at `at`.
fn folded_chain(
    sessions: &Path,
    at: u64,
) -> (crate::resume::Resumed, Vec<contract::provider::Input>) {
    write_chain(
        sessions,
        HA,
        &[
            handoff_envelope(HA, 0, None, None, &handoff_session_started(None)),
            handoff_envelope(HA, 1, Some("t_a1"), None, &handoff_turn("a-one")),
        ],
    );
    write_chain(
        sessions,
        HB,
        &[
            handoff_envelope(HB, 0, None, None, &handoff_session_started(Some((HA, 1)))),
            handoff_envelope(HB, 1, Some("t_b1"), None, &handoff_turn("b-one")),
            handoff_envelope(HB, 2, Some("t_b1"), None, &handoff_step()),
            handoff_envelope(HB, 3, Some("t_b2"), None, &handoff_turn("b-two")),
            handoff_envelope(HB, 4, Some("t_b2"), None, &handoff_begun()),
            handoff_envelope(
                HB,
                5,
                Some("t_b2"),
                Some("a_mid"),
                &handoff_note_text("the middle note"),
            ),
            handoff_envelope(
                HB,
                6,
                Some("t_b2"),
                None,
                &handoff_done(Outcome::Completed, Some("a_mid")),
            ),
            handoff_envelope(HB, 7, None, None, &handoff_opening()),
            handoff_envelope(HB, 8, Some("t_b2"), None, &handoff_step()),
            handoff_envelope(HB, 9, Some("t_b3"), None, &handoff_turn("b-three")),
        ],
    );
    write_chain(
        sessions,
        HC,
        &[handoff_envelope(
            HC,
            0,
            None,
            None,
            &handoff_session_started(Some((HB, at))),
        )],
    );
    let folded = crate::history::forked(&sessions.join(HC), Seq(0)).unwrap();
    let log = log::Log::open(
        sessions,
        SessionId(HC.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = crate::history::read_window(&log, folded.window, folded.end).unwrap();
    let conversation = crate::conversation::rebuild(&lines, "fake/model-1").unwrap();
    (folded, conversation)
}

#[test]
fn a_handoff_in_a_middle_segment_applies_to_the_grandchild() {
    let home = fakes::TempDir::new("loop-handoff-middle");
    let sessions = home.path().join("sessions");
    // C continues B past the handoff: the window names B's segment and the
    // handoff turn's start there.
    let (folded, conversation) = folded_chain(&sessions, 8);
    assert_eq!(folded.window, (1, 3));
    assert_eq!(folded.seed, seeded(&["a-one", "b-one"]));
    assert_eq!(conversation.len(), 3);
    assert!(
        matches!(&conversation[0], contract::provider::Input::User { text, .. } if text.starts_with("This message is from Fiber")),
        "the opening message renders first: {conversation:?}"
    );
    assert_eq!(conversation[1], user_input("b-two"));
    assert_eq!(conversation[2], user_input("the middle note"));
    // C continued before the handoff instead: the handoff is after the
    // point, so the window stays at the chain's start.
    let (earlier, before) = folded_chain(&sessions, 2);
    assert_eq!(earlier.window, (0, 0));
    assert_eq!(earlier.seed, Carry::default());
    assert_eq!(before, [user_input("a-one"), user_input("b-one")]);
}

#[test]
fn of_two_handoffs_only_the_one_at_or_before_the_point_applies() {
    // Two completed handoffs with the fork between them: the first moves
    // the window, the second is past the point and changes nothing.
    let home = fakes::TempDir::new("loop-two-handoffs");
    let sessions = home.path().join("sessions");
    write_chain(
        &sessions,
        HA,
        &[
            handoff_envelope(HA, 0, None, None, &handoff_session_started(None)),
            handoff_envelope(HA, 1, Some("t_1"), None, &handoff_turn("one")),
            handoff_envelope(HA, 2, Some("t_1"), None, &handoff_step()),
            handoff_envelope(HA, 3, Some("t_2"), None, &handoff_turn("two")),
            handoff_envelope(HA, 4, Some("t_2"), None, &handoff_begun()),
            handoff_envelope(
                HA,
                5,
                Some("t_2"),
                Some("a_first"),
                &handoff_note_text("the first note"),
            ),
            handoff_envelope(
                HA,
                6,
                Some("t_2"),
                None,
                &handoff_done(Outcome::Completed, Some("a_first")),
            ),
            handoff_envelope(HA, 7, None, None, &handoff_opening()),
            handoff_envelope(HA, 8, Some("t_2"), None, &handoff_step()),
            handoff_envelope(HA, 9, Some("t_3"), None, &handoff_turn("three")),
            handoff_envelope(HA, 10, Some("t_3"), None, &handoff_begun()),
            handoff_envelope(
                HA,
                11,
                Some("t_3"),
                Some("a_second"),
                &handoff_note_text("the second note"),
            ),
            handoff_envelope(
                HA,
                12,
                Some("t_3"),
                None,
                &handoff_done(Outcome::Completed, Some("a_second")),
            ),
            handoff_envelope(HA, 13, None, None, &handoff_opening()),
            handoff_envelope(HA, 14, Some("t_3"), None, &handoff_step()),
            handoff_envelope(HA, 15, Some("t_4"), None, &handoff_turn("four")),
        ],
    );
    write_chain(
        &sessions,
        HB,
        &[handoff_envelope(
            HB,
            0,
            None,
            None,
            &handoff_session_started(Some((HA, 8))),
        )],
    );
    let folded = crate::history::forked(&sessions.join(HB), Seq(0)).unwrap();
    assert_eq!(folded.window, (0, 3));
    assert_eq!(folded.seed, seeded(&["one"]));
    let log = log::Log::open(
        &sessions,
        SessionId(HB.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = crate::history::read_window(&log, folded.window, folded.end).unwrap();
    let conversation = crate::conversation::rebuild(&lines, "fake/model-1").unwrap();
    // The first handoff's context only: its opening, the carried input
    // and its note, with no line of the second.
    assert_eq!(conversation.len(), 3);
    assert!(
        matches!(&conversation[0], contract::provider::Input::User { text, .. } if text.starts_with("This message is from Fiber")),
        "the first opening renders first: {conversation:?}"
    );
    assert_eq!(conversation[1], user_input("two"));
    assert_eq!(conversation[2], user_input("the first note"));
    assert!(
        conversation.iter().all(|input| !matches!(input, contract::provider::Input::User { text, .. } if text.contains("the second note") || text.contains("three"))),
        "nothing past the point reaches the fork: {conversation:?}"
    );
}

fn handoff_hook_context(text: &str) -> Event {
    Event::ContextAdded(contract::events::ContextAdded {
        text: text.to_owned(),
        extension: "fiber.test/session-start".to_owned(),
        hook: "session_start".to_owned(),
    })
}

/// The old session with a `session_start` hook context after the handoff's
/// opening message, then a third turn. The opening is seq 7, the context
/// seq 8.
fn hook_context_log() -> Vec<Envelope> {
    vec![
        handoff_envelope(HA, 0, None, None, &handoff_session_started(None)),
        handoff_envelope(HA, 1, Some("t_1"), None, &handoff_turn("one")),
        handoff_envelope(HA, 2, Some("t_1"), None, &handoff_step()),
        handoff_envelope(HA, 3, Some("t_2"), None, &handoff_turn("two")),
        handoff_envelope(HA, 4, Some("t_2"), None, &handoff_begun()),
        handoff_envelope(
            HA,
            5,
            Some("t_2"),
            Some("a_note"),
            &handoff_note_text("the handoff note"),
        ),
        handoff_envelope(
            HA,
            6,
            Some("t_2"),
            None,
            &handoff_done(Outcome::Completed, Some("a_note")),
        ),
        handoff_envelope(HA, 7, None, None, &handoff_opening()),
        handoff_envelope(HA, 8, None, None, &handoff_hook_context("the hook context")),
        handoff_envelope(HA, 9, Some("t_2"), None, &handoff_step()),
        handoff_envelope(HA, 10, Some("t_3"), None, &handoff_turn("three")),
    ]
}

fn folded_hook_context(sessions: &Path, at: u64) -> Vec<contract::provider::Input> {
    write_chain(sessions, HA, &hook_context_log());
    write_chain(
        sessions,
        HB,
        &[handoff_envelope(
            HB,
            0,
            None,
            None,
            &handoff_session_started(Some((HA, at))),
        )],
    );
    let folded = crate::history::forked(&sessions.join(HB), Seq(0)).unwrap();
    assert_eq!(folded.window, (0, 3));
    assert_eq!(folded.seed, seeded(&["one"]));
    let log = log::Log::open(
        sessions,
        SessionId(HB.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = crate::history::read_window(&log, folded.window, folded.end).unwrap();
    crate::conversation::rebuild(&lines, "fake/model-1").unwrap()
}

#[test]
fn a_hook_context_after_the_handoffs_opening_is_inherited() {
    let home = fakes::TempDir::new("loop-hook-context");
    let sessions = home.path().join("sessions");
    // Past the hook context: it sits between the opening message and the
    // carried input, as a live rebuild of the old log renders it.
    let conversation = folded_hook_context(&sessions, 9);
    assert_eq!(conversation.len(), 4);
    assert!(
        matches!(&conversation[0], contract::provider::Input::User { text, .. } if text.starts_with("This message is from Fiber")),
        "the opening message renders first: {conversation:?}"
    );
    assert_eq!(conversation[1], user_input("the hook context"));
    assert_eq!(conversation[2], user_input("two"));
    assert_eq!(conversation[3], user_input("the handoff note"));
    // On the opening message's own line: the handoff applies but the
    // context after it does not.
    let earlier = folded_hook_context(&sessions, 7);
    assert_eq!(earlier.len(), 3);
    assert!(
        matches!(&earlier[0], contract::provider::Input::User { text, .. } if text.starts_with("This message is from Fiber")),
        "the opening message still renders first: {earlier:?}"
    );
    assert_eq!(earlier[1], user_input("two"));
    assert_eq!(earlier[2], user_input("the handoff note"));
    assert!(
        earlier.iter().all(|input| !matches!(input, contract::provider::Input::User { text, .. } if text.contains("the hook context"))),
        "the context past the point stays out: {earlier:?}"
    );
}

#[test]
fn a_forked_loop_writes_no_model_changed_for_a_later_label_switch() {
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
            supervised: false,
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
            factory: None,
            from: contract::shapes::Point {
                session_id: SessionId("s_parent00000001".to_owned()),
                seq: Seq(2),
                project: None,
            },
            worktree: None,
            how: Continuation::Fork {
                parent: contract::events::Parent {
                    session_id: SessionId("s_parent00000001".into()),
                    delegate_id: contract::JobId("j_fork".into()),
                },
            },
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
    let lines = log::read(&dir).unwrap();
    assert_eq!(lines[0].payload["parent"]["delegate_id"], "j_fork");
    assert!(lines[0].payload.get("rewind").is_none());
}

/// The old log's last line for a rewound session: its kind with a payload.
fn last_line_of(kind: &str, payload: serde_json::Value) -> Envelope {
    serde_json::from_value(json!({
        "kind": kind, "session_id": "s_aaaaaaaaaaaaaaaa",
        "ts": 2, "schema_version": 1, "seq": 5, "payload": payload,
    }))
    .unwrap()
}

/// A `rewound` payload naming `new`, at `seq` 3, with no jobs.
fn rewound_payload(new: &str, from: Option<&str>) -> serde_json::Value {
    let mut payload = json!({"new_session_id": new, "seq": 3, "jobs": []});
    if let Some(from) = from {
        payload["from_session_id"] = from.into();
    }
    payload
}

#[test]
fn rewound_from_reads_the_old_session_and_its_seq() {
    let old = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let new = SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    assert_eq!(
        rewound_from(
            Some(last_line_of("rewound", rewound_payload(&new.0, None))),
            &new,
            &old
        )
        .unwrap(),
        (
            Point {
                session_id: old.clone(),
                seq: contract::Seq(3),
                project: None,
            },
            None
        )
    );
}

#[test]
fn rewound_from_reads_the_ancestor_an_ancestor_point_names() {
    let old = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let ancestor = SessionId("s_cccccccccccccccc".to_owned());
    let new = SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    assert_eq!(
        rewound_from(
            Some(last_line_of(
                "rewound",
                rewound_payload(&new.0, Some(&ancestor.0))
            )),
            &new,
            &old
        )
        .unwrap(),
        (
            Point {
                session_id: ancestor,
                seq: contract::Seq(3),
                project: None,
            },
            None
        )
    );
}

#[test]
fn rewound_from_reads_the_summary_for_the_restarted_process() {
    let old = SessionId("s_aaaaaaaaaaaaaaaa".into());
    let new = SessionId("s_bbbbbbbbbbbbbbbb".into());
    let mut payload = rewound_payload(&new.0, None);
    payload["summary"] = "Tried X.\nReverted it.".into();
    assert_eq!(
        rewound_from(Some(last_line_of("rewound", payload)), &new, &old).unwrap(),
        (
            Point {
                session_id: old,
                project: None,
                seq: contract::Seq(3)
            },
            Some("Tried X.\nReverted it.".into())
        )
    );
}

#[test]
fn rewound_from_refuses_anything_but_a_rewound_naming_this_session() {
    let old = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let new = SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    let other = SessionId("s_dddddddddddddddd".to_owned());
    let cases: Vec<(&str, Option<Envelope>)> = vec![
        ("an empty log", None),
        (
            "a last line that is not rewound",
            Some(last_line_of("fiber_exited", json!({}))),
        ),
        (
            "a rewound naming another session",
            Some(last_line_of("rewound", rewound_payload(&other.0, None))),
        ),
        (
            "a rewound whose payload does not parse",
            Some(last_line_of("rewound", json!({"new_session_id": new.0}))),
        ),
    ];
    for (what, last) in cases {
        match rewound_from(last, &new, &old) {
            Err(error) => {
                assert_eq!(error.code(), ErrorCode::InvalidArguments, "{what}");
                let message = error.to_string();
                assert!(
                    message.contains(&old.0) && message.contains(&new.0),
                    "{what}: {message}"
                );
            }
            Ok(point) => panic!("{what} gives a point: {point:?}"),
        }
    }
}
