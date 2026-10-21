//! The hook keys table in `docs/extensions.md` ("What a hook is given and
//! returns") against the code: the table states only keys the chains give
//! or read, and the chains give and read only keys the table states.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::SessionId;
use contract::clock::Wake;
use contract::events::{CallStatus, HandoffTrigger, InputItem, Notice};
use contract::hook::{
    AfterToolCall, BeforeHandoff, BeforeMessage, BeforeModelCall, BeforeToolCall, Hooks,
    MessageSender, SessionStart, StartReason, TurnEnd, TurnStart,
};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Process, Tokens, Usage};
use contract::tool::Cancel;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, Value, json};

use super::super::SessionExtensions;
use crate::{Origin, Request, plan};
use fakes::FakeLock;

struct Home {
    root: fakes::TempDir,
}

impl Home {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-hook-keys");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs `fiber.test/<short>`, whose entry script is `init`.
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

    /// The session's extensions, loaded on a thread under [`WAIT`]: the
    /// fake clock never ends a wait the runtime does not end itself.
    #[track_caller]
    fn load(&self) -> Arc<SessionExtensions> {
        let config = Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: Vec::new(),
        })
        .unwrap();
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        Arc::new(bounded(move || {
            SessionExtensions::load(&home, &config, FakeClock::new(), locks, None)
        }))
    }
}

/// A hook at `point` whose `run` is `body`.
fn hook(point: &str, body: &str) -> String {
    format!(
        "fiber.hook(\"{point}\", {{ timeout = 1000, on_failure = \"non-blocking\",\n\
           run = function(call) {body} end }})\n"
    )
}

/// How long a test waits for a load or a chain before failing.
const WAIT: Duration = Duration::from_secs(5);

/// Runs `f` on its own thread and waits for it under [`WAIT`], so a runtime
/// that never answers fails the test instead of hanging it.
#[track_caller]
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(f());
    });
    Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the extensions")
}

/// The hook keys table: each row's point with the backticked key names from
/// its given and return columns.
fn table_rows() -> Vec<(String, Vec<String>, Vec<String>)> {
    let doc = include_str!("../../../../docs/extensions.md");
    let section = doc
        .split("### What a hook is given and returns")
        .nth(1)
        .unwrap();
    let mut rows = Vec::new();
    for line in section
        .lines()
        .skip_while(|line| !line.starts_with('|'))
        .skip(2)
    {
        if !line.starts_with('|') {
            break;
        }
        let mut columns = line.split('|').map(str::trim);
        columns.next();
        let (Some(point), Some(given), Some(returned)) =
            (columns.next(), columns.next(), columns.next())
        else {
            continue;
        };
        rows.push((
            backticked(point).remove(0),
            backticked(given),
            backticked(returned),
        ));
    }
    rows
}

/// Every `` `name` `` in `cell`, in order.
fn backticked(cell: &str) -> Vec<String> {
    cell.split('`')
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, name)| name.to_owned())
        .collect()
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

/// Every return key the decoders accept, per point: what the table must
/// document, and what the test exercises against the code.
const SUPPORTED: &[(&str, &[&str])] = &[
    ("session_start", &["context"]),
    ("before_message", &["content", "context", "refuse"]),
    ("turn_start", &["context"]),
    ("before_tool", &["arguments", "refuse"]),
    ("before_model_call", &["refuse"]),
    ("after_tool", &["content", "details", "artifact"]),
    ("turn_end", &["message"]),
    ("before_handoff", &["note"]),
];

/// A hook returning `key`'s valid value at `point`. Panics on a key
/// without a value here, so a new decoder key updates this test.
fn returning(point: &str, key: &str) -> String {
    let body = match (point, key) {
        ("session_start", "context") => "return { context = \"ctx\" }".to_owned(),
        ("turn_start", "context") => "return { context = \"ctx\" }".to_owned(),
        ("before_model_call", "refuse") => "return { refuse = \"no\" }".to_owned(),
        ("turn_end", "message") => "return { message = \"go\" }".to_owned(),
        ("before_message", "content") => {
            "return { content = {{ type = \"text\", text = \"hi\" }} }".to_owned()
        }
        ("before_message", "context") => "return { context = \"ctx\" }".to_owned(),
        ("before_message", "refuse") => "return { refuse = \"no\" }".to_owned(),
        ("before_tool", "arguments") => "return { arguments = {} }".to_owned(),
        ("before_tool", "refuse") => "return { refuse = \"no\" }".to_owned(),
        ("after_tool", "content") => "return { content = \"y\" }".to_owned(),
        ("after_tool", "details") => "return { details = { n = 1 } }".to_owned(),
        ("after_tool", "artifact") => "return { artifact = \"a\" }".to_owned(),
        ("before_handoff", "note") => "return { note = \"Next.\" }".to_owned(),
        _ => panic!("no valid `{key}` value for `{point}`"),
    };
    hook(point, &body)
}

/// Calls `point`'s chain with an input filling every optional key, and
/// returns the notices.
fn ask_full(session: &Arc<SessionExtensions>, point: &str) -> Vec<Notice> {
    let session = Arc::clone(session);
    let point = point.to_owned();
    bounded(move || match point.as_str() {
        "session_start" => {
            let id = SessionId("s_4c1d".into());
            session
                .session_start(&SessionStart {
                    reason: StartReason::Handoff,
                    session_id: &id,
                    workspace: "/w",
                    cancel: &NEVER,
                })
                .notices
        }
        "turn_start" => {
            let input = vec![
                InputItem::Message {
                    content: vec![ContentPart::Text { text: "x".into() }],
                    sender: contract::shapes::Sender {
                        origin: contract::shapes::Origin::Driver,
                        command_id: Some(contract::CommandId("c_1".into())),
                    },
                    changed_by: None,
                },
                InputItem::ShellCommand {
                    seq: contract::Seq(1),
                },
                InputItem::Jobs {
                    job_ids: vec![contract::JobId("j_1".into())],
                },
                InputItem::Handoff {
                    command_id: contract::CommandId("c_2".into()),
                },
            ];
            session
                .turn_start(&TurnStart {
                    input: &input,
                    cancel: &NEVER,
                })
                .notices
        }
        "before_model_call" => {
            let usage = Usage {
                tokens: Tokens {
                    input: 10,
                    cache_read: 1,
                    cache_write: [("5m".to_owned(), 2)].into_iter().collect(),
                    output: 5,
                },
                cost: Some(0.01),
                subscription_cost: 0.5,
            };
            session
                .before_model_call(&BeforeModelCall {
                    model: "anthropic/claude-x",
                    usage: &usage,
                    cancel: &NEVER,
                })
                .notices
        }
        "turn_end" => {
            session
                .turn_end(&TurnEnd {
                    reply: "Done.",
                    cancel: &NEVER,
                })
                .notices
        }
        "before_message" => {
            let content = vec![ContentPart::Text { text: "x".into() }];
            let from = SessionId("s_aaaaaaaaaaaaaaaa".into());
            let parent = SessionId("s_bbbbbbbbbbbbbbbb".into());
            session
                .before_message(&BeforeMessage {
                    content: &content,
                    sender: Some(MessageSender {
                        from_session_id: &from,
                        parent_session_id: Some(&parent),
                    }),
                })
                .notices
        }
        "before_handoff" => {
            let conversation = vec![contract::provider::Input::User {
                text: "hi".into(),
                images: Vec::new(),
            }];
            session
                .before_handoff(&BeforeHandoff {
                    trigger: HandoffTrigger::Auto,
                    instructions: Some("Focus."),
                    conversation: &conversation,
                    cancel: &NEVER,
                })
                .notices
        }
        "before_tool" => {
            let arguments = Map::new();
            let declared = DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: false,
                paths: Some(vec!["/r/a".into()]),
            };
            session
                .before_tool(&BeforeToolCall {
                    tool: "read",
                    arguments: &arguments,
                    declared: &declared,
                })
                .notices
        }
        _ => {
            let arguments = Map::new();
            let details = json!({"n": 1});
            let process = Process {
                exit_code: Some(0),
                signal: None,
                timed_out: false,
            };
            session
                .after_tool(&AfterToolCall {
                    tool: "read",
                    arguments: &arguments,
                    status: CallStatus::Completed,
                    content: "x",
                    details: Some(&details),
                    process: Some(&process),
                    spilled: None,

                    not_regular: false,
                    delivery: None,
                })
                .notices
        }
    })
}

/// Calls `point`'s chain with a plain input, and returns the notices.
fn ask_plain(session: &Arc<SessionExtensions>, point: &str) -> Vec<Notice> {
    let session = Arc::clone(session);
    let point = point.to_owned();
    bounded(move || match point.as_str() {
        "session_start" => {
            let id = SessionId("s_4c1d".into());
            session
                .session_start(&SessionStart {
                    reason: StartReason::Start,
                    session_id: &id,
                    workspace: "/w",
                    cancel: &NEVER,
                })
                .notices
        }
        "turn_start" => {
            let input = vec![InputItem::Jobs {
                job_ids: vec![contract::JobId("j_1".into())],
            }];
            session
                .turn_start(&TurnStart {
                    input: &input,
                    cancel: &NEVER,
                })
                .notices
        }
        "before_model_call" => {
            let usage = Usage::default();
            session
                .before_model_call(&BeforeModelCall {
                    model: "anthropic/claude-x",
                    usage: &usage,
                    cancel: &NEVER,
                })
                .notices
        }
        "turn_end" => {
            session
                .turn_end(&TurnEnd {
                    reply: "Done.",
                    cancel: &NEVER,
                })
                .notices
        }
        "before_message" => {
            let content = vec![ContentPart::Text { text: "x".into() }];
            session
                .before_message(&BeforeMessage {
                    content: &content,
                    sender: None,
                })
                .notices
        }
        "before_handoff" => {
            session
                .before_handoff(&BeforeHandoff {
                    trigger: HandoffTrigger::Auto,
                    instructions: None,
                    conversation: &[],
                    cancel: &NEVER,
                })
                .notices
        }
        "before_tool" => {
            let arguments = Map::from_iter([("path".to_owned(), Value::String("a".into()))]);
            let declared = DeclaredEffects {
                effects: Vec::new(),
                reversible: false,
                paths: None,
            };
            session
                .before_tool(&BeforeToolCall {
                    tool: "write",
                    arguments: &arguments,
                    declared: &declared,
                })
                .notices
        }
        _ => {
            let arguments = Map::new();
            session
                .after_tool(&AfterToolCall {
                    tool: "read",
                    arguments: &arguments,
                    status: CallStatus::Completed,
                    content: "x",
                    details: None,
                    process: None,
                    spilled: None,

                    not_regular: false,
                    delivery: None,
                })
                .notices
        }
    })
}

#[test]
fn the_table_names_the_eight_points() {
    let points: Vec<String> = table_rows().iter().map(|row| row.0.clone()).collect();
    assert_eq!(
        points,
        [
            "session_start",
            "before_message",
            "turn_start",
            "before_tool",
            "before_model_call",
            "after_tool",
            "turn_end",
            "before_handoff"
        ]
    );
}

#[test]
fn each_point_is_given_exactly_the_tables_keys() {
    for (point, mut given, _) in table_rows() {
        given.sort();
        let home = Home::new();
        // `note` is shown only from the second hook on: the first hook
        // returns one, and the probe errors with what it was shown.
        let probe = "local keys = {} for k in pairs(call) do keys[#keys + 1] = k end \
             table.sort(keys) error(table.concat(keys, \",\"))";
        if point == "before_handoff" {
            home.install("a", &hook(&point, r#"return { note = "earlier" }"#));
            home.install("b", &hook(&point, probe));
        } else {
            home.install(
                "a",
                &hook(
                    &point,
                    "local keys = {} for k in pairs(call) do keys[#keys + 1] = k end \
                     table.sort(keys) error(table.concat(keys, \",\"))",
                ),
            );
        }
        let notices = ask_full(&home.load(), &point);
        assert_eq!(notices.len(), 1, "{point}");
        assert_eq!(notices[0].code, ErrorCode::HookFailed);
        let shown = notices[0].message.split(": ").last().unwrap();
        assert_eq!(
            shown.split(',').collect::<Vec<_>>(),
            given,
            "{point}: the hook is given exactly the table's keys"
        );
    }
}

#[test]
fn each_supported_return_key_is_accepted_and_matches_the_table() {
    let rows = table_rows();
    for (point, keys) in SUPPORTED {
        for key in *keys {
            let home = Home::new();
            home.install("a", &returning(point, key));
            let notices = ask_plain(&home.load(), point);
            assert!(notices.is_empty(), "{point} `{key}`: {notices:?}");
        }
        let mut documented: Vec<String> =
            rows.iter().find(|row| row.0 == *point).unwrap().2.clone();
        documented.sort();
        let mut keys: Vec<String> = keys.iter().map(|key| (*key).to_owned()).collect();
        keys.sort();
        assert_eq!(
            keys, documented,
            "{point}: the table documents every supported return key"
        );
        // A key the table does not name fails the hook.
        let home = Home::new();
        home.install("a", &hook(point, "return { bogus = 1 }"));
        let notices = ask_plain(&home.load(), point);
        assert_eq!(notices.len(), 1, "{point} `bogus` fails");
    }
}
