//! The `sandbox` command's take: flip, then report on the notice path.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::SessionId;
use contract::commands::SandboxArgs;
use contract::events::EventKind;
use contract::inbox::{Ack, Delivery};
use log::Log;

use super::{Applied, ApplySandbox, Availability, Cause, SANDBOX_OFF, SANDBOX_ON};
use crate::inbox::TurnInput;
use crate::{Loop, Model};
use fakes::{Deadline, NoRules};

const DEADLINE: Duration = Duration::from_secs(10);

/// A loop over `tools`: the one fixture every sandbox test starts from.
/// `started` is the empty-tools wrapper below; the seam tests pass a
/// registered `shell` whose definition the toggle flips.
fn started_with_tools(
    tools: Vec<(String, Arc<dyn contract::tool::Tool>)>,
    dir: &str,
) -> (Loop, Arc<Log>, log::Watcher, fakes::TempDir) {
    let home = fakes::TempDir::new(dir);
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(home.path().join("credentials")).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let (_inbox, rx) = mpsc::channel::<Delivery>();
    let looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::new(fakes::ScriptedProvider::new(Vec::new())),
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools,
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(NoRules),
            },
        },
        None,
    )
    .unwrap();
    let watched = log.watch();
    (looped, log, watched, home)
}

fn started() -> (Loop, Arc<Log>, log::Watcher, fakes::TempDir) {
    started_with_tools(Vec::new(), "fiber-loop-sandbox")
}

/// The seam answering `available`: records every requested flag.
fn available() -> (ApplySandbox, Arc<std::sync::Mutex<Vec<bool>>>) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let apply: ApplySandbox = Arc::new({
        let seen = Arc::clone(&seen);
        move |enabled| {
            seen.lock().unwrap().push(enabled);
            Applied {
                availability: Availability::Available,
                changed: true,
            }
        }
    });
    (apply, seen)
}

/// Admits one `sandbox` command idle and returns its answer. The toggle's
/// reply is an ephemeral `notice`: it reaches watchers, never the log.
fn admit(looped: &mut Loop, enabled: bool) -> contract::inbox::Answer {
    let mut input = TurnInput::of(Vec::new());
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    looped
        .admit_idle(Delivery::Sandbox(SandboxArgs { enabled }, ack), &mut input)
        .unwrap();
    assert!(
        input.pieces.is_empty(),
        "a sandbox toggle starts no turn idle"
    );
    Deadline::after(DEADLINE).recv(&rx).expect("the ack runs")
}

/// The toggle's reply off the watcher: the next line is its `notice`.
fn replied(watched: &mut log::Watcher) -> String {
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("the notice arrives in time")
        .expect("the log outlives the drain")
        .expect("the log ended before the notice");
    assert_eq!(line.kind(), EventKind::Notice);
    assert!(line.turn_id.is_none(), "the notice carries no turn");
    assert!(line.action_id.is_none(), "the notice carries no action");
    line.payload["message"]
        .as_str()
        .expect("a notice carries a message")
        .to_owned()
}

#[test]
fn enabling_an_available_sandbox_writes_the_on_notice_and_accepts() {
    let (mut looped, _log, mut watched, _home) = started();
    let (apply, seen) = available();
    looped.sandbox_apply = Some(apply);
    let answer = admit(&mut looped, true);
    assert!(
        matches!(answer, Ok(None)),
        "the toggle accepts with no result"
    );
    assert_eq!(*seen.lock().unwrap(), [true], "the seam flips first");
    assert_eq!(replied(&mut watched), SANDBOX_ON);
}

#[test]
fn disabling_writes_the_off_notice_and_accepts() {
    let (mut looped, _log, mut watched, _home) = started();
    let (apply, seen) = available();
    looped.sandbox_apply = Some(apply);
    let answer = admit(&mut looped, false);
    assert!(
        matches!(answer, Ok(None)),
        "the toggle accepts with no result"
    );
    assert_eq!(*seen.lock().unwrap(), [false]);
    assert_eq!(replied(&mut watched), SANDBOX_OFF);
}

#[test]
fn an_unreported_unavailability_writes_its_notice_and_accepts() {
    let (mut looped, _log, mut watched, _home) = started();
    let notice = contract::events::Notice {
        code: contract::ErrorCode::ToolError,
        message: "sandbox unavailable: Landlock ABI is below 2".to_owned(),
        extension: None,
    };
    looped.sandbox_apply = Some(Arc::new(move |enabled| {
        assert!(enabled);
        Applied {
            availability: Availability::Unavailable {
                cause: Cause::AbiBelow2,
                notice: Some(notice.clone()),
            },
            changed: true,
        }
    }));
    let answer = admit(&mut looped, true);
    assert!(
        matches!(answer, Ok(None)),
        "the toggle accepts with no result"
    );
    assert_eq!(
        replied(&mut watched),
        "sandbox unavailable: Landlock ABI is below 2"
    );
}

#[test]
fn an_already_reported_unavailability_writes_nothing_and_accepts() {
    let (mut looped, _log, mut watched, _home) = started();
    looped.sandbox_apply = Some(Arc::new(|_| Applied {
        availability: Availability::Unavailable {
            cause: Cause::AbiBelow2,
            notice: None,
        },
        changed: false,
    }));
    let answer = admit(&mut looped, true);
    assert!(
        matches!(answer, Ok(None)),
        "the toggle accepts with no result"
    );
    // The ack runs after the append, on the same thread, so an empty queue
    // here means nothing was written: no wait, no clock.
    assert!(watched.try_recv().unwrap().is_none(), "no second line");
}

#[test]
fn a_toggle_after_close_rejects_closing() {
    let (mut looped, _log, _watched, _home) = started();
    let (apply, _) = available();
    looped.sandbox_apply = Some(apply);
    looped.closing = true;
    let answer = admit(&mut looped, true);
    assert!(
        matches!(
            &answer,
            Err(rejection) if rejection.code == contract::ErrorCode::Closing
        ),
        "after close the toggle is rejected: {answer:?}"
    );
}

#[test]
fn a_toggle_without_a_switch_is_refused() {
    let (mut looped, _log, _watched, _home) = started();
    assert!(looped.sandbox_apply.is_none());
    let answer = admit(&mut looped, true);
    assert!(
        matches!(
            &answer,
            Err(rejection) if rejection.code == contract::ErrorCode::InvalidArguments
        ),
        "with no seam the toggle is refused: {answer:?}"
    );
}

#[test]
fn every_cause_names_itself() {
    for (cause, name) in [
        (Cause::UnsupportedPlatform, "UnsupportedPlatform"),
        (Cause::AbiBelow2, "AbiBelow2"),
        (Cause::SandboxExecUnavailable, "SandboxExecUnavailable"),
        (Cause::DenyInsideWrite, "DenyInsideWrite"),
        (Cause::UnresolvableDeny, "UnresolvableDeny"),
        (Cause::UnresolvedWrite, "UnresolvedWrite"),
    ] {
        assert_eq!(format!("{cause:?}"), name);
    }
}

/// A shell-like tool whose definition reflects a shared enabled flag: the
/// sentence appears only when the flag is on, mirroring the real shell's
/// availability-qualified sentence. The flag is the shared handle the seam
/// must not hold across `definition()`.
struct ToggleShell {
    enabled: Arc<std::sync::Mutex<bool>>,
}

impl ToggleShell {
    fn description(enabled: bool) -> String {
        let mut text =
            "Runs a command in a new process session. `command` is the command.".to_owned();
        if enabled {
            text.push_str(
                " When the session sandbox is on and available, the call runs confined \
                 with no sockets and writes limited to the workspace, temp and granted paths; \
                 a call with `unconfined: true` or `detach` set runs outside the sandbox \
                 and is reviewed as with the sandbox off.",
            );
        }
        text
    }
}

impl contract::tool::Tool for ToggleShell {
    fn definition(&self) -> contract::provider::ToolDefinition {
        let enabled = *self.enabled.lock().unwrap();
        contract::provider::ToolDefinition {
            name: "shell".to_owned(),
            description: Self::description(enabled),
            input_schema: serde_json::json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        panic!("effects not used in sandbox seam tests")
    }

    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        panic!("run not used in sandbox seam tests")
    }
}

/// A loop with a registered `shell` tool backed by `enabled`: the seam's
/// refresh target. The toggle closure flips the same flag the tool reads.
fn started_with_shell(
    enabled: Arc<std::sync::Mutex<bool>>,
) -> (Loop, Arc<Log>, log::Watcher, fakes::TempDir) {
    let shell: Arc<dyn contract::tool::Tool> = Arc::new(ToggleShell {
        enabled: Arc::clone(&enabled),
    });
    started_with_tools(
        vec![("builtin".to_owned(), shell)],
        "fiber-loop-sandbox-seam",
    )
}

/// The `/tools` rows the loop publishes, in publish order: the window
/// test reads back what `publish_preamble_rows` declared.
#[derive(Default)]
struct Rows {
    declare: Mutex<Vec<(String, Option<contract::events::ToolInfo>)>>,
}

impl Rows {
    fn setter(rows: &Arc<Rows>) -> crate::ToolRows {
        let declare_rows = Arc::clone(rows);
        crate::ToolRows {
            declare: Arc::new(move |name, info| {
                declare_rows
                    .declare
                    .lock()
                    .unwrap()
                    .push((name.to_owned(), info));
            }),
            state: Arc::new(|_, _| {}),
        }
    }

    /// The latest published `bytes` for `name`: what a `/tools` read
    /// would answer right now.
    fn bytes(&self, name: &str) -> u64 {
        self.declare
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(seen, _)| seen.as_str() == name)
            .and_then(|(_, info)| info.as_ref())
            .expect("the row is declared")
            .bytes
    }
}

fn flipping_apply(
    enabled: Arc<std::sync::Mutex<bool>>,
) -> (ApplySandbox, Arc<std::sync::Mutex<Vec<bool>>>) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let apply: ApplySandbox = Arc::new({
        let seen = Arc::clone(&seen);
        let enabled = Arc::clone(&enabled);
        move |next| {
            seen.lock().unwrap().push(next);
            let mut guard = enabled.lock().unwrap();
            let previous = *guard;
            *guard = next;
            let changed = previous != next;
            drop(guard);
            Applied {
                availability: Availability::Available,
                changed,
            }
        }
    });
    (apply, seen)
}

fn log_lines(looped: &Loop) -> Vec<contract::Envelope> {
    log::read(looped.log.dir()).unwrap()
}

fn preamble_reasons(looped: &Loop) -> Vec<String> {
    log_lines(looped)
        .into_iter()
        .filter(|line| line.kind() == EventKind::PreambleBuilt)
        .map(|line| {
            line.payload["reason"]
                .as_str()
                .expect("preamble_built carries a reason")
                .to_owned()
        })
        .collect()
}

fn registered_description(looped: &Loop) -> String {
    looped
        .tools
        .get("shell")
        .expect("shell stays registered")
        .2
        .description
        .clone()
}

#[test]
fn a_flip_refreshes_the_registered_shell_bytes_with_reason_sandbox_notice_first() {
    let enabled = Arc::new(std::sync::Mutex::new(false));
    let (mut looped, _log, mut watched, _home) = started_with_shell(Arc::clone(&enabled));
    let (apply, _) = flipping_apply(Arc::clone(&enabled));
    looped.sandbox_apply = Some(apply);
    // Hold a warming request so the seam must drop it.
    looped.warming.set_cap(Some(1));
    let request = contract::provider::ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".to_owned(),
        cache_lifetime: contract::events::CacheLifetime::FiveMinutes,
        cache_key: String::new(),
        conversation: Vec::new().into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        media: Default::default(),
    };
    let now = looped.log.clock().now();
    looped.warming.record(&request, now);
    assert!(looped.warming.held().is_some(), "warming holds a request");
    let answer = admit(&mut looped, true);
    assert!(matches!(answer, Ok(None)), "the flip accepts: {answer:?}");
    // The reply arrives first on the watcher: notice-before-build, since the
    // seam rebuilds after the notice append. A swapped order would deliver
    // the build first and fail the kind check below.
    // (`docs/invocation.md`, "What each command does": the toggle reports its reply;
    // `docs/events.md`, "`preamble_built`": the sandbox build follows the toggle.)
    assert!(
        replied(&mut watched) == SANDBOX_ON,
        "the reply arrives first"
    );
    // The seam rebuilt with the sandbox reason after the notice.
    // (`docs/events.md`, "`preamble_built`": a `sandbox` build follows a toggle that
    // changed the setting.)
    assert_eq!(preamble_reasons(&looped), vec!["sandbox".to_owned()]);
    assert_eq!(
        looped.preamble_reason,
        contract::events::PreambleReason::Sandbox
    );
    // Registered bytes equal the live definition with the sentence.
    let live = ToggleShell::description(true);
    assert_eq!(registered_description(&looped), live);
    // Warming dropped the held request.
    assert!(looped.warming.held().is_none(), "the seam stops warming");
    // The durable build carries the sandbox reason; the ephemeral notice is
    // watcher-only, so its order is pinned by the reply arriving first above.
    let lines = log_lines(&looped);
    assert!(
        lines.iter().any(|line| {
            line.kind() == EventKind::PreambleBuilt && line.payload["reason"] == "sandbox"
        }),
        "the sandbox build is logged"
    );
    // Sent bytes carry the fresh shell description.
    let built = lines
        .iter()
        .find(|line| line.kind() == EventKind::PreambleBuilt)
        .expect("a build is logged");
    let tools = built.payload["tools"].as_array().expect("tools are sent");
    let shell = tools
        .iter()
        .find(|tool| tool["name"] == "shell")
        .expect("the shell is sent");
    assert_eq!(
        shell["definition"]["description"],
        serde_json::Value::String(live)
    );
}

#[test]
fn a_flip_off_refreshes_the_registered_shell_bytes_back() {
    let enabled = Arc::new(std::sync::Mutex::new(true));
    let (mut looped, _log, mut watched, _home) = started_with_shell(Arc::clone(&enabled));
    let (apply, _) = flipping_apply(Arc::clone(&enabled));
    looped.sandbox_apply = Some(apply);
    let answer = admit(&mut looped, false);
    assert!(matches!(answer, Ok(None)), "the flip accepts: {answer:?}");
    assert_eq!(replied(&mut watched), SANDBOX_OFF);
    // (`docs/events.md`, "`preamble_built`": a `sandbox` build follows a toggle that
    // changed the setting.)
    assert_eq!(preamble_reasons(&looped), vec!["sandbox".to_owned()]);
    assert_eq!(
        registered_description(&looped),
        ToggleShell::description(false)
    );
}

#[test]
fn a_repeated_available_toggle_replies_with_no_rebuild() {
    let enabled = Arc::new(std::sync::Mutex::new(true));
    let (mut looped, _log, mut watched, _home) = started_with_shell(Arc::clone(&enabled));
    // A no-op toggle: the value does not flip, so the seam is skipped.
    let apply: ApplySandbox = Arc::new({
        let enabled = Arc::clone(&enabled);
        move |next| {
            let guard = enabled.lock().unwrap();
            let changed = *guard != next;
            drop(guard);
            Applied {
                availability: Availability::Available,
                changed,
            }
        }
    });
    looped.sandbox_apply = Some(apply);
    let builds_before = preamble_reasons(&looped).len();
    let answer = admit(&mut looped, true);
    assert!(matches!(answer, Ok(None)), "the no-op accepts: {answer:?}");
    // (`docs/invocation.md`, "What each command does": a toggle on an already-on setting is a
    // no-op with its reply.)
    assert_eq!(replied(&mut watched), SANDBOX_ON, "a no-op still replies");
    // (`docs/events.md`, "`preamble_built`": a toggle that changes nothing builds nothing.)
    assert_eq!(
        preamble_reasons(&looped).len(),
        builds_before,
        "a no-op builds nothing"
    );
}

#[test]
fn a_repeated_unavailable_on_says_its_line_at_most_once() {
    let (mut looped, _log, mut watched, _home) = started();
    let notice = contract::events::Notice {
        code: contract::ErrorCode::ToolError,
        message: "sandbox unavailable: Landlock ABI is below 2".to_owned(),
        extension: None,
    };
    let reported = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // First enable flips on and reports once; the second is a no-op that
    // still consults the latch, so no second line appears.
    for first in [true, false] {
        let notice = notice.clone();
        let reported = Arc::clone(&reported);
        let enabled = Arc::new(std::sync::Mutex::new(!first));
        looped.sandbox_apply = Some(Arc::new(move |next| {
            let changed = {
                let mut guard = enabled.lock().unwrap();
                let previous = *guard;
                *guard = next;
                previous != next
            };
            let already = reported.swap(true, std::sync::atomic::Ordering::SeqCst);
            Applied {
                availability: Availability::Unavailable {
                    cause: Cause::AbiBelow2,
                    notice: (!already).then(|| notice.clone()),
                },
                changed,
            }
        }));
        let answer = admit(&mut looped, true);
        assert!(matches!(answer, Ok(None)), "the toggle accepts: {answer:?}");
        if first {
            // (`docs/invocation.md`, "What each command does": enabling while unavailable reports
            // one `sandbox unavailable` notice.)
            assert_eq!(
                replied(&mut watched),
                "sandbox unavailable: Landlock ABI is below 2"
            );
            // Drain the flip's rebuild so the second toggle's silence is
            // observable: the first toggle still takes the seam.
            let build = watched
                .recv_timeout(DEADLINE)
                .expect("the rebuild follows the notice")
                .expect("the log outlives the drain")
                .expect("the log ended before the build");
            assert_eq!(build.kind(), EventKind::PreambleBuilt);
            // (`docs/events.md`, "`preamble_built`": the flip's rebuild carries reason
            // `sandbox`.)
            assert_eq!(build.payload["reason"], "sandbox");
        } else {
            assert!(
                watched.try_recv().unwrap().is_none(),
                "the second line never appears"
            );
        }
    }
}

#[test]
fn an_interleaved_flip_between_snapshot_and_publish_publishes_fresh_bytes() {
    let enabled = Arc::new(std::sync::Mutex::new(false));
    let (mut looped, _log, _watched, _home) = started_with_shell(Arc::clone(&enabled));
    let (apply, _) = flipping_apply(Arc::clone(&enabled));
    looped.sandbox_apply = Some(apply);
    // Park a pause that flips the flag to the opposite value from a second
    // thread between the snapshot and the publish: the toggle already set
    // the flag to true before the snapshot, so setting it back to false
    // forces a real mismatch and the seam must publish the fresh bytes.
    crate::turn::pause_sandbox_between_snapshot_and_publish(Box::new({
        let enabled = Arc::clone(&enabled);
        move || {
            let flipped = std::thread::spawn({
                let enabled = Arc::clone(&enabled);
                move || {
                    *enabled.lock().unwrap() = false;
                }
            });
            flipped.join().unwrap();
        }
    }));
    let answer = admit(&mut looped, true);
    assert!(matches!(answer, Ok(None)), "the flip accepts: {answer:?}");
    // The publish carries the post-interleave bytes, not the snapshot.
    assert_eq!(
        registered_description(&looped),
        ToggleShell::description(false),
        "the publish is fresh after the interleave"
    );
    // (`docs/events.md`, "`preamble_built`": the flip still builds once with reason
    // `sandbox`.)
    assert_eq!(preamble_reasons(&looped), vec!["sandbox".to_owned()]);
}

#[test]
fn rows_answer_old_bytes_after_append_before_publish() {
    let enabled = Arc::new(std::sync::Mutex::new(false));
    let rows = Arc::new(Rows::default());
    let (looped, _log, _watched, _home) = started_with_shell(Arc::clone(&enabled));
    let mut looped = looped.tool_rows(Rows::setter(&rows));
    let (apply, _) = flipping_apply(Arc::clone(&enabled));
    looped.sandbox_apply = Some(apply);
    // The first flip completes: its build is appended and its rows are
    // published, so the second flip's window opens over known sizes.
    let answer = admit(&mut looped, true);
    assert!(matches!(answer, Ok(None)), "the flip accepts: {answer:?}");
    // (`docs/events.md`, "`preamble_built`": a `sandbox` build follows a toggle that
    // changed the setting.)
    assert_eq!(preamble_reasons(&looped), vec!["sandbox".to_owned()]);
    let on_bytes = rows.bytes("shell");
    assert!(
        registered_description(&looped).contains("confined"),
        "the first flip refreshes"
    );
    // Park the second flip's append-to-publish window: the new build is
    // already appended when the rows still answer the first flip's bytes.
    // The append (`Event::PreambleBuilt` in `crates/loop/src/turn.rs`)
    // runs before `publish_preamble_rows` on the same thread, so a reader
    // in that gap sees the new event with stale rows: that gap is the
    // window the `/tools` polls race across.
    let log_dir = looped.log.dir().to_path_buf();
    let saw_window = Arc::new(std::sync::atomic::AtomicBool::new(false));
    crate::turn::pause_preamble_between_append_and_publish(Box::new({
        let rows = Arc::clone(&rows);
        let saw_window = Arc::clone(&saw_window);
        move || {
            saw_window.store(true, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                rows.bytes("shell"),
                on_bytes,
                "the rows still answer the old size after the append"
            );
            let built: Vec<String> = log::read(&log_dir)
                .expect("the log reads mid-seam")
                .into_iter()
                .filter(|line| line.kind() == EventKind::PreambleBuilt)
                .map(|line| {
                    line.payload["reason"]
                        .as_str()
                        .expect("preamble_built carries a reason")
                        .to_owned()
                })
                .collect();
            // (`docs/events.md`, "`preamble_built`": a `sandbox` build follows a toggle that
            // changed the setting.)
            assert_eq!(
                built,
                vec!["sandbox".to_owned(), "sandbox".to_owned()],
                "the new build is appended before the publish"
            );
        }
    }));
    let answer = admit(&mut looped, false);
    assert!(
        matches!(answer, Ok(None)),
        "the flip back accepts: {answer:?}"
    );
    assert!(
        saw_window.load(std::sync::atomic::Ordering::SeqCst),
        "the append-to-publish window ran"
    );
    // (`docs/events.md`, "`preamble_built`": a `sandbox` build follows a toggle that
    // changed the setting.)
    assert_eq!(
        preamble_reasons(&looped),
        vec!["sandbox".to_owned(), "sandbox".to_owned()]
    );
    let off_bytes = rows.bytes("shell");
    assert_ne!(on_bytes, off_bytes, "the flip back republishes");
    assert!(
        !registered_description(&looped).contains("confined"),
        "the flip back refreshes"
    );
}

#[test]
fn an_in_flight_declaration_keeps_its_value_across_the_toggle() {
    let enabled = Arc::new(std::sync::Mutex::new(false));
    let (mut looped, _log, _watched, _home) = started_with_shell(Arc::clone(&enabled));
    // A call declared before the toggle keeps its earlier declaration: the
    // seam refreshes the registry for the next step but never rewrites an
    // in-flight decision.
    let before = looped
        .tools
        .get("shell")
        .expect("shell is registered")
        .2
        .description
        .clone();
    assert!(!before.contains("confined"), "the old bytes read off");
    let (apply, _) = flipping_apply(Arc::clone(&enabled));
    looped.sandbox_apply = Some(apply);
    let answer = admit(&mut looped, true);
    assert!(matches!(answer, Ok(None)), "the flip accepts: {answer:?}");
    assert!(
        !before.contains("confined"),
        "the in-flight value never changes"
    );
    assert!(
        registered_description(&looped).contains("confined"),
        "the next step observes the refreshed bytes"
    );
}
