//! The `session_start`, `turn_start`, `before_model_call` and `turn_end`
//! chains (`docs/extensions.md`, "The hook points", "When several hooks
//! share a point" and "When a hook fails"): what each hook is shown, and
//! what its answer decides.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Weak, mpsc};
use std::thread;

use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::SessionId;
use contract::clock::Wake;
use contract::events::InputItem;
use contract::hook::{
    BeforeModelCall, CheckOutcome, Context, ContextAnswer, ContextOutcome, Hooks, SessionStart,
    StartReason, TurnEnd, TurnStart,
};
use contract::shapes::{ContentPart, Tokens, Usage};
use contract::tool::Cancel;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::json;

use super::super::SessionExtensions;
use crate::{Origin, Request, plan};
use fakes::FakeLock;

struct Home {
    root: fakes::TempDir,
}

impl Home {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-lifecycle-hooks");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs `fiber.test/<short>`, whose entry script is `init`. No
    /// fixture registers a provider, so the manifest declares hooks alone.
    fn install(&self, short: &str, init: &str) {
        let src = self.root.path().join("src").join(short);
        fs::create_dir_all(&src).unwrap();
        let manifest = json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1});
        fs::write(src.join("extension.json"), manifest.to_string()).unwrap();
        fs::write(src.join("init.lua"), init).unwrap();
        plan(
            &self.home(),
            &Request::Path(src),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    fn config(&self, overrides: &[&str]) -> Config {
        Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap()
    }

    /// The session's extensions, loaded on a thread under [`MUST_SUCCEED_WITHIN`]: the
    /// fake clock never ends a wait the runtime does not end itself.
    #[track_caller]
    fn load(&self, overrides: &[&str]) -> Arc<SessionExtensions> {
        let config = self.config(overrides);
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        Arc::new(bounded(move || {
            SessionExtensions::load(&home, &config, FakeClock::new(), locks, None)
        }))
    }
}

/// A cancel that never fires: the chain runs every hook.
struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

const NEVER: Never = Never;

/// A hook at `point` whose `run` is `body`.
fn hook(point: &str, on_failure: &str, body: &str) -> String {
    format!(
        "fiber.hook(\"{point}\", {{ timeout = 1000, on_failure = \"{on_failure}\",\n\
           run = function(call) {body} end }})\n"
    )
}

/// A hook at `point` in `phase` whose `run` is `body`.
fn phased_hook(point: &str, phase: &str, on_failure: &str, body: &str) -> String {
    format!(
        "fiber.hook(\"{point}\", {{ phase = \"{phase}\", timeout = 1000, on_failure = \"{on_failure}\",\n\
           run = function(call) {body} end }})\n"
    )
}

/// Runs `f` on its own thread and waits for it under [`MUST_SUCCEED_WITHIN`], so a runtime
/// that never answers fails the test instead of hanging it.
#[track_caller]
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(f());
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the extensions")
}

fn session_id() -> SessionId {
    SessionId("s_4c1d".into())
}

#[track_caller]
fn session_start(session: &Arc<SessionExtensions>, reason: StartReason) -> ContextAnswer {
    let session = Arc::clone(session);
    let id = session_id();
    bounded(move || {
        session.session_start(&SessionStart {
            reason,
            session_id: &id,
            workspace: "/w",
            cancel: &NEVER,
        })
    })
}

#[track_caller]
fn turn_start(session: &Arc<SessionExtensions>, input: Vec<InputItem>) -> ContextAnswer {
    let session = Arc::clone(session);
    bounded(move || {
        session.turn_start(&TurnStart {
            input: &input,
            cancel: &NEVER,
        })
    })
}

fn usage(cost: Option<f64>) -> Usage {
    Usage {
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 5,
        },
        cost,
        subscription_cost: 0.0,
    }
}

#[track_caller]
fn model_call(session: &Arc<SessionExtensions>, cost: Option<f64>) -> contract::hook::CheckAnswer {
    let session = Arc::clone(session);
    let used = usage(cost);
    bounded(move || {
        session.before_model_call(&BeforeModelCall {
            model: "anthropic/claude-x",
            usage: &used,
            cancel: &NEVER,
        })
    })
}

#[track_caller]
fn turn_end(session: &Arc<SessionExtensions>, reply: &str) -> ContextAnswer {
    let session = Arc::clone(session);
    let reply = reply.to_owned();
    bounded(move || {
        session.turn_end(&TurnEnd {
            reply: &reply,
            cancel: &NEVER,
        })
    })
}

fn added(answer: &ContextAnswer) -> Vec<Context> {
    let ContextOutcome::Added(context) = &answer.outcome else {
        panic!("the hooks add context: {:?}", answer.outcome);
    };
    context.clone()
}

#[test]
fn two_session_start_hooks_add_context_in_run_order() {
    let home = Home::new();
    home.install(
        "a",
        &hook(
            "session_start",
            "non-blocking",
            "return { context = \"A\" }",
        ),
    );
    home.install(
        "b",
        &hook(
            "session_start",
            "non-blocking",
            "return { context = \"B\" }",
        ),
    );
    let answer = session_start(&home.load(&[]), StartReason::Handoff);
    assert_eq!(
        added(&answer),
        vec![
            Context {
                state: Vec::new(),
                extension: "fiber.test/a".into(),
                text: "A".into(),
            },
            Context {
                state: Vec::new(),
                extension: "fiber.test/b".into(),
                text: "B".into(),
            },
        ]
    );
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn nil_and_an_empty_table_add_no_session_start_context() {
    let home = Home::new();
    home.install("a", &hook("session_start", "non-blocking", "return nil"));
    home.install("b", &hook("session_start", "non-blocking", "return {}"));
    let answer = session_start(&home.load(&[]), StartReason::Start);
    assert_eq!(added(&answer), Vec::new());
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_session_start_hook_is_shown_why_the_session_started() {
    for (reason, word) in [
        (StartReason::Start, "start"),
        (StartReason::Resume, "resume"),
        (StartReason::Fork, "fork"),
        (StartReason::Rewind, "rewind"),
        (StartReason::Handoff, "handoff"),
    ] {
        let home = Home::new();
        home.install(
            "a",
            &hook(
                "session_start",
                "non-blocking",
                "return { context = call.reason .. \":\" .. call.session_id .. \":\" .. call.workspace }",
            ),
        );
        let answer = session_start(&home.load(&[]), reason);
        assert_eq!(
            added(&answer),
            vec![Context {
                state: Vec::new(),
                extension: "fiber.test/a".into(),
                text: format!("{word}:s_4c1d:/w"),
            }],
            "{word}"
        );
    }
}

#[test]
fn a_session_start_hook_is_shown_exactly_reason_session_id_and_workspace() {
    let home = Home::new();
    home.install(
        "a",
        &hook(
            "session_start",
            "non-blocking",
            "local keys = {} for k in pairs(call) do keys[#keys + 1] = k end \
             table.sort(keys) error(table.concat(keys, \",\"))",
        ),
    );
    let answer = session_start(&home.load(&[]), StartReason::Resume);
    assert_eq!(answer.notices.len(), 1);
    assert!(
        answer.notices[0]
            .message
            .contains("reason,session_id,workspace"),
        "{}",
        answer.notices[0].message
    );
}

#[test]
fn a_turn_start_hook_is_shown_the_turns_input_without_unknown_items() {
    let home = Home::new();
    home.install(
        "a",
        &hook(
            "turn_start",
            "non-blocking",
            "if call.input[1].source ~= \"driver\" then error(\"source\") end \
             if call.input[1].command_id ~= \"c_1\" then error(\"command\") end \
             if call.input[1].content[1].text ~= \"hi\" then error(\"text\") end \
             local kinds = {} for _, item in ipairs(call.input) do kinds[#kinds + 1] = item.type end \
             return { context = table.concat(kinds, \",\") }",
        ),
    );
    let input = vec![
        InputItem::Message {
            content: vec![ContentPart::Text { text: "hi".into() }],
            sender: contract::shapes::Sender {
                origin: contract::shapes::Origin::Driver,
                command_id: Some(contract::CommandId("c_1".into())),
            },
            changed_by: None,
        },
        InputItem::Jobs {
            job_ids: vec![contract::JobId("j_1".into())],
        },
        InputItem::Handoff {
            command_id: contract::CommandId("c_2".into()),
        },
        InputItem::Unknown,
    ];
    let answer = turn_start(&home.load(&[]), input);
    assert_eq!(
        added(&answer),
        vec![Context {
            state: Vec::new(),
            extension: "fiber.test/a".into(),
            text: "message,jobs,handoff".into(),
        }]
    );
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_non_blocking_failure_keeps_the_chain_and_names_the_extension() {
    let home = Home::new();
    home.install(
        "a",
        &hook("session_start", "non-blocking", "error(\"kaput\")"),
    );
    home.install(
        "b",
        &hook(
            "session_start",
            "non-blocking",
            "return { context = \"late\" }",
        ),
    );
    let answer = session_start(&home.load(&[]), StartReason::Start);
    assert_eq!(
        added(&answer),
        vec![Context {
            state: Vec::new(),
            extension: "fiber.test/b".into(),
            text: "late".into(),
        }]
    );
    assert_eq!(answer.notices.len(), 1);
    let notice = &answer.notices[0];
    assert_eq!(notice.code, ErrorCode::HookFailed);
    assert_eq!(notice.extension.as_deref(), Some("fiber.test/a"));
    assert!(
        notice
            .message
            .contains("The `session_start` hook failed, so its change was dropped: "),
        "{}",
        notice.message
    );
    assert!(notice.message.contains("kaput"), "{}", notice.message);
}

#[test]
fn a_blocking_failure_drops_the_earlier_contexts_and_keeps_the_notices() {
    let home = Home::new();
    home.install(
        "a",
        &hook("turn_start", "non-blocking", "error(\"wobble\")"),
    );
    home.install(
        "b",
        &hook(
            "turn_start",
            "non-blocking",
            "return { context = \"early\" }",
        ),
    );
    home.install("c", &hook("turn_start", "blocking", "error(\"kaput\")"));
    let answer = turn_start(&home.load(&[]), Vec::new());
    let ContextOutcome::Failed(failure) = &answer.outcome else {
        panic!("the hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/c");
    assert!(failure.message.contains("kaput"), "{}", failure.message);
    assert_eq!(answer.notices.len(), 1, "{:?}", answer.notices);
    assert_eq!(answer.notices[0].extension.as_deref(), Some("fiber.test/a"));
}

#[test]
fn a_bad_context_return_fails_the_hook() {
    for (point, body, why) in [
        ("session_start", "return { context = \"\" }", "`context`"),
        ("session_start", "return { context = 3 }", "`context`"),
        (
            "session_start",
            "return { refuse = \"x\" }",
            "`refuse`, which is not `context`",
        ),
        (
            "session_start",
            "return { message = \"x\" }",
            "`message`, which is not `context`",
        ),
        ("turn_start", "return { bogus = 1 }", "`bogus`"),
        ("turn_start", "return 3", "not a table"),
        (
            "turn_start",
            "return {{ context = \"x\" }}",
            "a list, not a table",
        ),
    ] {
        let home = Home::new();
        home.install("a", &hook(point, "blocking", body));
        let answer = if point == "session_start" {
            session_start(&home.load(&[]), StartReason::Start)
        } else {
            turn_start(&home.load(&[]), Vec::new())
        };
        let ContextOutcome::Failed(failure) = &answer.outcome else {
            panic!("{point} {body} fails: {:?}", answer.outcome);
        };
        assert_eq!(failure.extension, "fiber.test/a", "{body}");
        assert!(failure.message.contains(why), "{body}: {}", failure.message);
        assert!(answer.notices.is_empty(), "{body}: {:?}", answer.notices);
    }
}

#[test]
fn hooks_order_turn_start_reorders_only_that_point() {
    fn appending(point: &str, tag: &str) -> String {
        let read = if point == "turn_start" {
            "call.input"
        } else {
            "call.reason"
        };
        hook(
            point,
            "non-blocking",
            &format!("local _ = {read} return {{ context = \"{tag}\" }}"),
        )
    }
    let home = Home::new();
    home.install(
        "a",
        &(appending("turn_start", "a") + &appending("session_start", "a")),
    );
    home.install(
        "b",
        &(appending("turn_start", "b") + &appending("session_start", "b")),
    );
    let session = home.load(&[r#"hooks.order.turn_start=["fiber.test/b"]"#]);
    let answer = turn_start(&session, Vec::new());
    assert_eq!(
        added(&answer)
            .iter()
            .map(|kept| kept.text.clone())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
    // The `session_start` order is untouched by the `turn_start` order.
    let answer = session_start(&session, StartReason::Start);
    assert_eq!(
        added(&answer)
            .iter()
            .map(|kept| kept.text.clone())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}

#[test]
fn sanitize_hooks_run_before_transform_hooks_at_turn_start() {
    let home = Home::new();
    home.install(
        "s",
        &phased_hook(
            "turn_start",
            "sanitize",
            "non-blocking",
            "return { context = \"S\" }",
        ),
    );
    home.install(
        "t",
        &phased_hook(
            "turn_start",
            "transform",
            "non-blocking",
            "return { context = \"T\" }",
        ),
    );
    let answer = turn_start(&home.load(&[]), Vec::new());
    assert_eq!(
        added(&answer)
            .iter()
            .map(|kept| kept.text.clone())
            .collect::<Vec<_>>(),
        ["S", "T"]
    );
}

#[test]
fn a_check_hook_at_turn_start_is_not_registered() {
    let home = Home::new();
    home.install(
        "a",
        &phased_hook(
            "turn_start",
            "check",
            "blocking",
            "return { context = \"never\" }",
        ),
    );
    let session = home.load(&[]);
    let answer = turn_start(&session, Vec::new());
    assert_eq!(added(&answer), Vec::new());
    let refused = "a `check` hook exists only at `before_message`, `before_tool`";
    assert!(
        session
            .notices()
            .iter()
            .any(|notice| notice.message.contains(refused)),
        "{:?}",
        session.notices()
    );
}

#[test]
fn a_before_model_call_hook_is_shown_the_model_and_usage() {
    let home = Home::new();
    home.install(
        "a",
        &hook(
            "before_model_call",
            "non-blocking",
            "local keys = {} for k in pairs(call) do keys[#keys + 1] = k end \
             table.sort(keys) error(table.concat(keys, \",\"))",
        ),
    );
    let session = home.load(&[]);
    let answer = model_call(&session, Some(0.01));
    assert_eq!(answer.outcome, CheckOutcome::Passed);
    assert_eq!(answer.notices.len(), 1);
    assert!(
        answer.notices[0].message.contains("model,usage"),
        "{}",
        answer.notices[0].message
    );
    let home = Home::new();
    home.install(
        "a",
        &hook(
            "before_model_call",
            "non-blocking",
            "error(call.model .. \"|\" .. tostring(call.usage.cost) .. \"|\" \
             .. tostring(call.usage.subscription_cost) .. \"|\" \
             .. tostring(call.usage.tokens.input) .. \",\" \
             .. tostring(call.usage.tokens.cache_read) .. \",\" \
             .. tostring(call.usage.tokens.output))",
        ),
    );
    let answer = model_call(&home.load(&[]), Some(0.01));
    assert_eq!(answer.notices.len(), 1);
    assert!(
        answer.notices[0]
            .message
            .contains("anthropic/claude-x|0.01|0.0|10,0,5"),
        "{}",
        answer.notices[0].message
    );
}

#[test]
fn an_unknown_cost_is_absent_in_lua() {
    let home = Home::new();
    home.install(
        "a",
        &hook(
            "before_model_call",
            "non-blocking",
            "if call.usage.cost ~= nil then error(\"cost present\") end",
        ),
    );
    let answer = model_call(&home.load(&[]), None);
    assert_eq!(answer.outcome, CheckOutcome::Passed);
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_refusal_ends_the_chain_before_a_later_hook() {
    let home = Home::new();
    home.install(
        "a",
        &hook(
            "before_model_call",
            "non-blocking",
            "return { refuse = \"team budget spent\" }",
        ),
    );
    home.install(
        "b",
        &hook(
            "before_model_call",
            "non-blocking",
            "return { refuse = \"second\" }",
        ),
    );
    let answer = model_call(&home.load(&[]), Some(0.01));
    assert_eq!(
        answer.outcome,
        CheckOutcome::Refused(contract::hook::Refusal {
            extension: "fiber.test/a".into(),
            reason: "team budget spent".into(),
        })
    );
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_check_hook_runs_after_transform_hooks_at_before_model_call() {
    let home = Home::new();
    home.install(
        "a",
        &phased_hook(
            "before_model_call",
            "check",
            "non-blocking",
            "return { refuse = \"from-check\" }",
        ),
    );
    home.install(
        "b",
        &phased_hook(
            "before_model_call",
            "transform",
            "non-blocking",
            "return { refuse = \"from-transform\" }",
        ),
    );
    let answer = model_call(&home.load(&[]), Some(0.01));
    assert_eq!(
        answer.outcome,
        CheckOutcome::Refused(contract::hook::Refusal {
            extension: "fiber.test/b".into(),
            reason: "from-transform".into(),
        })
    );
}

#[test]
fn a_bad_refuse_return_fails_the_hook() {
    for (body, why) in [
        ("return { refuse = \"\" }", "`refuse`"),
        (
            "return { context = \"x\" }",
            "`context`, which is not `refuse`",
        ),
        ("return { bogus = 1 }", "`bogus`"),
        ("return 3", "not a table"),
    ] {
        let home = Home::new();
        home.install("a", &hook("before_model_call", "blocking", body));
        let answer = model_call(&home.load(&[]), Some(0.01));
        let CheckOutcome::Failed(failure) = &answer.outcome else {
            panic!("{body} fails: {:?}", answer.outcome);
        };
        assert_eq!(failure.extension, "fiber.test/a", "{body}");
        assert!(failure.message.contains(why), "{body}: {}", failure.message);
    }
}

#[test]
fn a_blocking_before_model_call_failure_fails_without_refusing() {
    let home = Home::new();
    home.install(
        "a",
        &hook("before_model_call", "blocking", "error(\"kaput\")"),
    );
    let answer = model_call(&home.load(&[]), Some(0.01));
    let CheckOutcome::Failed(failure) = &answer.outcome else {
        panic!("the hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/a");
    assert!(failure.message.contains("kaput"), "{}", failure.message);
}

#[test]
fn two_turn_end_hooks_continue_the_turn_in_run_order() {
    let home = Home::new();
    home.install(
        "a",
        &hook("turn_end", "non-blocking", "return { message = \"A\" }"),
    );
    home.install(
        "b",
        &hook("turn_end", "non-blocking", "return { message = \"B\" }"),
    );
    let answer = turn_end(&home.load(&[]), "Done.");
    assert_eq!(
        added(&answer),
        vec![
            Context {
                state: Vec::new(),
                extension: "fiber.test/a".into(),
                text: "A".into(),
            },
            Context {
                state: Vec::new(),
                extension: "fiber.test/b".into(),
                text: "B".into(),
            },
        ]
    );
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_turn_end_hook_is_shown_the_models_final_reply() {
    let home = Home::new();
    home.install(
        "a",
        &hook(
            "turn_end",
            "non-blocking",
            "return { message = \"got:\" .. call.reply }",
        ),
    );
    let answer = turn_end(&home.load(&[]), "Done.");
    assert_eq!(
        added(&answer),
        vec![Context {
            state: Vec::new(),
            extension: "fiber.test/a".into(),
            text: "got:Done.".into(),
        }]
    );
}

#[test]
fn a_turn_end_hook_with_no_message_ends_the_turn() {
    let home = Home::new();
    home.install("a", &hook("turn_end", "non-blocking", "return nil"));
    home.install("b", &hook("turn_end", "non-blocking", "return {}"));
    let answer = turn_end(&home.load(&[]), "Done.");
    assert_eq!(added(&answer), Vec::new());
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_bad_message_return_fails_the_hook() {
    for (body, why) in [
        ("return { message = \"\" }", "`message`"),
        ("return { message = {} }", "`message`"),
        ("return { message = 3 }", "`message`"),
        (
            "return { context = \"x\" }",
            "`context`, which is not `message`",
        ),
        ("return { bogus = 1 }", "`bogus`"),
    ] {
        let home = Home::new();
        home.install("a", &hook("turn_end", "blocking", body));
        let answer = turn_end(&home.load(&[]), "Done.");
        let ContextOutcome::Failed(failure) = &answer.outcome else {
            panic!("{body} fails: {:?}", answer.outcome);
        };
        assert_eq!(failure.extension, "fiber.test/a", "{body}");
        assert!(failure.message.contains(why), "{body}: {}", failure.message);
    }
}

#[test]
fn a_blocking_turn_end_failure_drops_the_earlier_message() {
    let home = Home::new();
    home.install(
        "a",
        &hook("turn_end", "non-blocking", "return { message = \"early\" }"),
    );
    home.install("b", &hook("turn_end", "blocking", "error(\"kaput\")"));
    let answer = turn_end(&home.load(&[]), "Done.");
    let ContextOutcome::Failed(failure) = &answer.outcome else {
        panic!("the hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/b");
    assert!(failure.message.contains("kaput"), "{}", failure.message);
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

/// The workspace file a marking hook writes when it runs.
fn marker(home: &Home, name: &str) -> PathBuf {
    home.root.path().join("workspace").join(name)
}

/// A hook body that writes `name` into the workspace, so a test can see
/// which hooks ran.
fn mark(home: &Home, name: &str) -> String {
    format!("host.fs.write(\"{}\", \"x\")", marker(home, name).display())
}

#[test]
fn a_cancel_set_before_a_before_model_call_chain_runs_only_its_first_hook() {
    let home = Home::new();
    home.install(
        "a",
        &hook("before_model_call", "non-blocking", &mark(&home, "a_ran")),
    );
    home.install(
        "b",
        &hook("before_model_call", "non-blocking", &mark(&home, "b_ran")),
    );
    let session = home.load(&[]);
    let cancel = fakes::CancelToken::new();
    cancel.cancel();
    let used = usage(Some(0.01));
    let answer = bounded(move || {
        session.before_model_call(&BeforeModelCall {
            model: "anthropic/claude-x",
            usage: &used,
            cancel: &cancel,
        })
    });
    assert_eq!(answer.outcome, CheckOutcome::Passed);
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
    assert!(marker(&home, "a_ran").exists(), "the first hook runs");
    assert!(
        !marker(&home, "b_ran").exists(),
        "the cancel stops the chain before the second hook"
    );
}

#[test]
fn a_cancel_set_before_a_session_start_chain_runs_only_its_first_hook() {
    let home = Home::new();
    home.install(
        "a",
        &hook("session_start", "non-blocking", &mark(&home, "a_ran")),
    );
    home.install(
        "b",
        &hook("session_start", "non-blocking", &mark(&home, "b_ran")),
    );
    let session = home.load(&[]);
    let cancel = fakes::CancelToken::new();
    cancel.cancel();
    let id = contract::SessionId("s_1".into());
    let workspace = "/work".to_owned();
    let answer = bounded(move || {
        session.session_start(&SessionStart {
            reason: StartReason::Start,
            session_id: &id,
            workspace: &workspace,
            cancel: &cancel,
        })
    });
    assert_eq!(answer.outcome, ContextOutcome::Added(Vec::new()));
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
    assert!(marker(&home, "a_ran").exists(), "the first hook runs");
    assert!(
        !marker(&home, "b_ran").exists(),
        "the cancel stops the chain before the second hook"
    );
}

/// Two non-blocking failures in one chain answer one grouped notice: the
/// count and the first item up front, every item with its extension in
/// the list, and no extension on the group itself.
fn assert_grouped_pair(notices: &[contract::events::Notice]) {
    assert_eq!(notices.len(), 1, "{notices:?}");
    let notice = &notices[0];
    assert_eq!(notice.code, ErrorCode::HookFailed);
    assert_eq!(notice.extension, None);
    assert!(
        notice.message.starts_with("2 hooks failed: "),
        "{}",
        notice.message
    );
    assert!(
        notice.message.contains("(extension `fiber.test/a`)"),
        "{}",
        notice.message
    );
    assert!(
        notice.message.contains("(extension `fiber.test/b`)"),
        "{}",
        notice.message
    );
}

#[test]
fn two_non_blocking_turn_start_failures_group_by_code() {
    let home = Home::new();
    home.install(
        "a",
        &hook("turn_start", "non-blocking", "error(\"boom-a\")"),
    );
    home.install(
        "b",
        &hook("turn_start", "non-blocking", "error(\"boom-b\")"),
    );
    let answer = turn_start(&home.load(&[]), Vec::new());
    assert!(added(&answer).is_empty(), "{:?}", answer.outcome);
    assert_grouped_pair(&answer.notices);
}

#[test]
fn two_non_blocking_before_model_call_failures_group_by_code() {
    let home = Home::new();
    home.install(
        "a",
        &hook("before_model_call", "non-blocking", "error(\"boom-a\")"),
    );
    home.install(
        "b",
        &hook("before_model_call", "non-blocking", "error(\"boom-b\")"),
    );
    let answer = model_call(&home.load(&[]), None);
    assert!(
        matches!(answer.outcome, CheckOutcome::Passed),
        "{:?}",
        answer.outcome
    );
    assert_grouped_pair(&answer.notices);
}

#[test]
fn a_later_before_model_call_hook_refuses_after_an_earlier_pass() {
    let home = Home::new();
    home.install(
        "a",
        &hook("before_model_call", "non-blocking", &mark(&home, "a_ran")),
    );
    home.install(
        "b",
        &hook(
            "before_model_call",
            "non-blocking",
            "return { refuse = \"team budget spent\" }",
        ),
    );
    let answer = model_call(&home.load(&[]), Some(0.01));
    assert_eq!(
        answer.outcome,
        CheckOutcome::Refused(contract::hook::Refusal {
            extension: "fiber.test/b".into(),
            reason: "team budget spent".into(),
        })
    );
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
    assert!(marker(&home, "a_ran").exists(), "the first hook ran");
}
