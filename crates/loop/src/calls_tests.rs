//! The tool seam's registration, fast paths, what a person's allow remembers,
//! and the job lines a call returns.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::RequestId;
use contract::SessionId;
use contract::clock::Wake;
use contract::commands::{Remember, RememberScope, ReplyAnswer};
use contract::emit::Emit;
use contract::events::{
    DecidedBy, Decision, Grant, JobCompleted, JobStarted, Outcome, RuleOffer, TextDelta,
    ToolCallArgumentsDelta, ToolCallRequested, ToolReplaced, TurnOutcome,
};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{Delta, Input, ModelRequest, Provider, ReplyAction, ToolDefinition};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Process, Sender};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{ActionId, CommandId, Envelope, ErrorCode, JobId};
use serde_json::{Map, Value, json};

use super::{fast_path, register};
use crate::permission::{Verdict, judge};
use crate::{Loop, Model};
use log::Log;

/// A tool known only by its name and description.
struct Named(&'static str, &'static str);

impl Tool for Named {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.0.to_owned(),
            description: self.1.to_owned(),
            input_schema: json!({"type": "object"}),
            deferred: false,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        unreachable!("registration never asks for effects")
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn contract::emit::Emit) -> Output {
        unreachable!("registration never runs a tool")
    }
}

fn by(registered_by: &str, tool: Named) -> (String, Arc<dyn Tool>) {
    (registered_by.to_owned(), Arc::new(tool))
}

#[test]
fn each_tool_is_registered_with_who_registered_it() {
    let (tools, replaced) = register(vec![
        by("builtin", Named("read", "Reads.")),
        by("github", Named("mcp__github__search", "Searches.")),
    ]);
    let owners: Vec<(&str, &str)> = tools
        .iter()
        .map(|(name, (by, _, _))| (name.as_str(), by.as_str()))
        .collect();
    assert_eq!(
        owners,
        [("mcp__github__search", "github"), ("read", "builtin")]
    );
    assert!(replaced.is_empty());
}

#[test]
fn a_later_tool_of_a_taken_name_replaces_the_earlier_and_is_recorded() {
    let (tools, replaced) = register(vec![
        by("builtin", Named("read", "Reads.")),
        by("lint", Named("read", "Reads, linted.")),
        by("builtin", Named("write", "Writes.")),
        by("audit", Named("read", "Reads, audited.")),
    ]);
    let (owner, _, definition) = &tools["read"];
    assert_eq!(owner, "audit");
    assert_eq!(definition.description, "Reads, audited.");
    assert_eq!(tools.len(), 2);
    let step = |from: &str, to: &str| ToolReplaced {
        name: "read".to_owned(),
        from: from.to_owned(),
        to: to.to_owned(),
    };
    assert_eq!(replaced, [step("builtin", "lint"), step("lint", "audit")]);
}

/// A fresh workspace, symlinks resolved, holding `real/` and a link `out`
/// to a directory outside it.
fn workspace() -> (fakes::TempDir, PathBuf) {
    let root = fakes::TempDir::new("fiber-calls");
    let canon = root.path().canonicalize().unwrap();
    let workspace = canon.join("ws");
    std::fs::create_dir_all(workspace.join("real")).unwrap();
    std::fs::create_dir_all(canon.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(canon.join("elsewhere"), workspace.join("out")).unwrap();
    (root, workspace)
}

fn declared(effects: &[Effect], paths: Option<&[&str]>) -> DeclaredEffects {
    DeclaredEffects {
        effects: effects.to_vec(),
        reversible: false,
        paths: paths.map(|p| p.iter().map(|s| (*s).to_owned()).collect()),
    }
}

#[test]
fn reads_and_no_effect_take_the_fast_path_wherever_they_point() {
    let (_root, ws) = workspace();
    assert!(fast_path(&declared(&[], None), &ws));
    assert!(fast_path(
        &declared(&[Effect::Reads], Some(&["/etc/hosts"])),
        &ws
    ));
}

#[test]
fn a_write_takes_the_fast_path_only_inside_the_workspace_and_outside_git_and_fiber() {
    let (_root, ws) = workspace();
    let writes = |paths: &[&str]| {
        fast_path(
            &declared(&[Effect::Reads, Effect::Writes], Some(paths)),
            &ws,
        )
    };
    let inside = ws.join("real/new/file.rs").display().to_string();
    assert!(writes(&["real/a.rs", "new.rs", &inside]));
    for outside in [
        "/tmp/x",
        "../x",
        "real/../../x",
        // Through a link that leaves the workspace.
        "out/x",
        "out/../x",
        // `..` past a directory that does not exist yet.
        "new/../real/x",
        ".git/config",
        "real/.git/hooks/pre-commit",
        ".fiber/config.json",
    ] {
        assert!(!writes(&[outside]), "{outside}");
    }
    assert!(!writes(&["real/a.rs", ".git/x"]));
    // No paths, or none declared, is reviewed.
    assert!(!writes(&[]));
    assert!(!fast_path(&declared(&[Effect::Writes], None), &ws));
}

#[test]
fn executes_and_network_never_take_the_fast_path() {
    let (_root, ws) = workspace();
    for effect in [Effect::Executes, Effect::Network] {
        assert!(!fast_path(
            &declared(&[Effect::Reads, effect], Some(&["real/a"])),
            &ws
        ));
    }
}

/// Standing rules in memory that record what was remembered, and optionally
/// fail: what a project remember is checked against.
#[derive(Default)]
struct FakeRules {
    /// Every (`tool`, `prefix`, `session`) remembered, in order.
    remembered: Mutex<Vec<(String, String, SessionId)>>,
    /// What `remember` fails with, once set.
    fail: Mutex<Option<RulesError>>,
}

impl FakeRules {
    /// `remember` fails with `error` from now on.
    fn fail(&self, error: RulesError) {
        *self.fail.lock().unwrap() = Some(error);
    }
}

impl Rules for FakeRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, tool: &str, prefix: &str, session: &SessionId) -> Result<(), RulesError> {
        if let Some(error) = self.fail.lock().unwrap().clone() {
            return Err(error);
        }
        self.remembered
            .lock()
            .unwrap()
            .push((tool.into(), prefix.into(), session.clone()));
        Ok(())
    }
}

/// A loop on a fresh log with the fakes clock, holding `rules`: the session
/// is `s_test`. The returned directories keep the log's files alive.
fn start(rules: Arc<FakeRules>) -> (Loop, fakes::TempDir, PathBuf, PathBuf) {
    let home = fakes::TempDir::new("fiber-remember");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let (_inbox, rx) = mpsc::channel::<Delivery>();
    let rules: Arc<dyn Rules> = rules;
    let prompt_clock: Arc<dyn contract::clock::Clock> = clock;
    let looped = Loop::start(
        log,
        Arc::new(fakes::ScriptedProvider::new(Vec::new())),
        Model {
            reference: "fake/model".into(),
            cost: None,
            subscription: false,
        },
        crate::prompt::PromptInputs::new(
            home.path().to_path_buf(),
            "/bin/sh".into(),
            home.path()
                .join("s_test/events.jsonl")
                .display()
                .to_string(),
            prompt_clock,
        ),
        rx,
        Vec::new(),
        crate::Permissions {
            workspace: workspace.display().to_string(),
            credentials: credentials.clone(),
            rules,
        },
    )
    .unwrap();
    (looped, home, workspace, credentials)
}

/// The rule a reviewer will offer (#294): the subject `npm test --watch`,
/// widened to `npm test`.
fn offer() -> RuleOffer {
    RuleOffer {
        subject: "npm test --watch".into(),
        prefix: "npm test".into(),
    }
}

/// An allow remembering `prefix` in `scope`.
fn allow(scope: RememberScope, prefix: &str) -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: Some(Remember {
            scope,
            prefix: prefix.into(),
        }),
    }
}

/// An `executes` call of `tool` with `subject`, declaring no paths.
fn judged(subject: &str) -> Effects {
    Effects {
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
        },
        subject: Some(subject.into()),
        prefix: None,
    }
}

#[test]
fn a_session_remember_adds_a_grant_the_next_call_judged_matches() {
    let (mut looped, _home, workspace, credentials) = start(Arc::new(FakeRules::default()));
    let answered = looped
        .answered(
            "shell",
            Some(&offer()),
            &allow(RememberScope::Session, "npm test"),
        )
        .unwrap();
    assert_eq!(
        answered.grant,
        Some(Grant {
            tool: "shell".into(),
            prefix: "npm test".into(),
        })
    );
    assert_eq!(answered.rule, None);
    assert_eq!(answered.reason, None);
    // The next call judged in the same step matches the added grant.
    let standing: Result<StandingRules, RulesError> = Ok(StandingRules::default());
    let verdict = judge(
        "shell",
        &judged("npm test --watch"),
        &standing,
        &looped.grants,
        &workspace,
        &credentials,
    );
    assert!(matches!(
        verdict,
        Verdict::Allow(Some(DecidedBy::SessionGrant))
    ));
}

#[test]
fn a_project_remember_appends_a_rule_and_names_it_on_the_line() {
    let rules = Arc::new(FakeRules::default());
    let (mut looped, _home, _workspace, _credentials) = start(Arc::clone(&rules));
    let answered = looped
        .answered(
            "shell",
            Some(&offer()),
            &allow(RememberScope::Project, "npm test"),
        )
        .unwrap();
    assert_eq!(answered.grant, None);
    assert_eq!(
        answered.rule,
        Some(Grant {
            tool: "shell".into(),
            prefix: "npm test".into(),
        })
    );
    assert_eq!(answered.reason, None);
    assert_eq!(
        *rules.remembered.lock().unwrap(),
        [(
            "shell".to_owned(),
            "npm test".to_owned(),
            SessionId("s_test".into())
        )]
    );
}

#[test]
fn a_failing_project_remember_still_allows_the_call_and_says_why() {
    let rules = Arc::new(FakeRules::default());
    rules.fail(RulesError("projects/k/rules:1: bad line".into()));
    let (mut looped, _home, _workspace, _credentials) = start(Arc::clone(&rules));
    let answered = looped
        .answered(
            "shell",
            Some(&offer()),
            &allow(RememberScope::Project, "npm test"),
        )
        .unwrap();
    assert_eq!(answered.grant, None);
    assert_eq!(answered.rule, None);
    let reason = answered.reason.unwrap();
    assert!(reason.contains("could not be saved"), "{reason}");
    assert!(reason.contains("projects/k/rules:1: bad line"), "{reason}");
    assert!(rules.remembered.lock().unwrap().is_empty());
}

#[test]
fn a_standing_ask_passes_no_offer_so_a_remember_does_not_fit() {
    let (mut looped, _home, _workspace, _credentials) = start(Arc::new(FakeRules::default()));
    assert!(
        looped
            .answered("shell", None, &allow(RememberScope::Session, "npm test"))
            .is_none()
    );
    assert!(looped.grants.is_empty());
}

#[test]
fn a_persons_allow_line_carries_what_was_remembered() {
    let remembered = |grant: Option<Grant>, rule: Option<Grant>, reason: Option<String>| {
        super::Answered {
            decision: Decision::Allow,
            feedback: None,
            grant,
            rule,
            reason,
        }
        .allow(RequestId("r_1".into()))
    };
    let grant = Grant {
        tool: "shell".into(),
        prefix: "npm test".into(),
    };
    // A remembered session grant rides `grant`.
    let line = remembered(Some(grant.clone()), None, None);
    assert_eq!(line.request_id, Some(RequestId("r_1".into())));
    assert_eq!(line.decision, Decision::Allow);
    assert_eq!(line.decided_by, DecidedBy::Person);
    assert_eq!(line.grant, Some(grant));
    assert_eq!(line.rule, None);
    assert_eq!(line.reason, None);
    // A remembered project rule rides `rule`.
    let line = remembered(
        None,
        Some(Grant {
            tool: "shell".into(),
            prefix: "npm test".into(),
        }),
        None,
    );
    assert_eq!(line.grant, None);
    assert_eq!(
        line.rule,
        Some(Grant {
            tool: "shell".into(),
            prefix: "npm test".into(),
        })
    );
    assert_eq!(line.reason, None);
    // A rule that could not be saved rides `reason`, with no `rule`.
    let line = remembered(None, None, Some("The rule could not be saved.".into()));
    assert_eq!(line.grant, None);
    assert_eq!(line.rule, None);
    assert_eq!(line.reason, Some("The rule could not be saved.".into()));
}

const TURN_DEADLINE: Duration = Duration::from_secs(10);

/// A tool whose result carries `jobs` and nothing else the model sees.
struct Lines {
    name: &'static str,
    jobs: Vec<contract::jobs::JobRecord>,
}

impl Tool for Lines {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.to_owned(),
            description: "Returns job lines.".to_owned(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
            deferred: false,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(reads())
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "ok\n".into(),
            }],
            jobs: self.jobs.clone(),
            ..Output::default()
        }
    }
}

/// Wakes the condvar a cancelled call waits on. The call holds that mutex
/// across the check and the wait, so a cancel cannot notify nobody.
struct Nudge(Arc<(Mutex<bool>, Condvar)>);

impl Wake for Nudge {
    fn wake(&self) {
        let (lock, cv) = &*self.0;
        let _guard = lock.lock().unwrap();
        cv.notify_all();
    }
}

/// A call that returns `jobs` only after the turn cancels it.
struct Hold {
    jobs: Vec<contract::jobs::JobRecord>,
    started: Arc<(Mutex<bool>, Condvar)>,
}

impl Tool for Hold {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "jobber".to_owned(),
            description: "Returns job lines after cancel.".to_owned(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
            deferred: false,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(reads())
    }

    fn run(&self, _: &Map<String, Value>, cancel: &dyn Cancel, _: &dyn Emit) -> Output {
        let (lock, cv) = &*self.started;
        let mut guard = lock.lock().unwrap();
        *guard = true;
        cv.notify_all();
        let wake: Arc<dyn Wake> = Arc::new(Nudge(Arc::clone(&self.started)));
        cancel.subscribe(Arc::downgrade(&wake));
        while !cancel.is_cancelled() {
            let _alive = &wake;
            guard = cv.wait(guard).unwrap();
        }
        Output {
            content: vec![ContentPart::Text {
                text: "ok\n".into(),
            }],
            jobs: self.jobs.clone(),
            ..Output::default()
        }
    }
}

fn reads() -> Effects {
    Effects {
        declared: DeclaredEffects {
            effects: vec![Effect::Reads],
            reversible: true,
            paths: None,
        },
        subject: Some(String::new()),
        prefix: None,
    }
}

/// A reply that calls `names`, each with `{}`, after `text`.
fn calls(text: &str, names: &[&str]) -> fakes::Scripted {
    let mut end = fakes::reply(text);
    let mut deltas = vec![Delta::Text(TextDelta { text: text.into() })];
    for (index, name) in names.iter().enumerate() {
        let arguments = json!({});
        deltas.push(Delta::ToolCallArguments(ToolCallArgumentsDelta {
            index: u32::try_from(index).unwrap(),
            name: Some((*name).into()),
            text: arguments.to_string(),
        }));
        end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
            name: (*name).into(),
            arguments,
            provider_id: None,
            repair: None,
            ran_by: None,
        }));
    }
    fakes::Scripted {
        deltas,
        end: Ok(end),
    }
}

fn started_line(id: &str) -> contract::jobs::JobRecord {
    contract::jobs::JobRecord::Started(JobStarted {
        job_id: JobId(id.into()),
        tool: Some("shell".into()),
        extension: None,
        description: "npm test".into(),
        output_path: format!("artifacts/{id}.log"),
    })
}

fn failed_line(id: &str) -> contract::jobs::JobRecord {
    contract::jobs::JobRecord::Completed(JobCompleted {
        job_id: JobId(id.into()),
        status: Outcome::Failed,
        error: Some(Failure {
            code: ErrorCode::NonzeroExit,
            message: "Exit code 1.".into(),
            retry_after: None,
            provider: None,
        }),
        process: Some(Process {
            exit_code: Some(1),
            signal: None,
            timed_out: false,
        }),
        output_tail: Some("1 failing\n".into()),
    })
}

struct Ran {
    outcome: Option<TurnOutcome>,
    requests: Vec<ModelRequest>,
    lines: Vec<Envelope>,
    homes: Vec<String>,
}

fn run_turn(
    tools: Vec<Arc<dyn Tool>>,
    script: Vec<fakes::Scripted>,
    cancel: Arc<crate::TurnCancel>,
) -> Ran {
    let home = fakes::TempDir::new("fiber-job-lines");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let provider = Arc::new(fakes::ScriptedProvider::new(script));
    let (inbox, rx) = mpsc::channel();
    inbox
        .send(Delivery::Prompt(
            Message {
                content: vec![ContentPart::Text { text: "go".into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_go".into())),
                },
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
    let rules: Arc<dyn Rules> = Arc::new(FakeRules::default());
    let mut looped = Loop::start(
        log,
        Arc::clone(&provider) as Arc<dyn Provider>,
        Model {
            reference: "fake/model".into(),
            cost: None,
            subscription: false,
        },
        crate::prompt::PromptInputs::new(
            home.path().to_path_buf(),
            "/bin/sh".into(),
            home.path()
                .join("s_test/events.jsonl")
                .display()
                .to_string(),
            Arc::clone(&clock),
        ),
        rx,
        tools
            .into_iter()
            .map(|tool| ("builtin".to_owned(), tool))
            .collect(),
        crate::Permissions {
            workspace: workspace.display().to_string(),
            credentials,
            rules,
        },
    )
    .unwrap()
    .cancelled_by(cancel);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done_tx.send(looped.turn());
    });
    let outcome = done_rx
        .recv_timeout(TURN_DEADLINE)
        .expect("the turn ended")
        .unwrap();
    drop(inbox);
    let lines = log::read(&home.path().join("s_test")).unwrap();
    Ran {
        outcome,
        requests: provider.requests(),
        lines,
        homes: home_spellings(home.path()),
    }
}

fn home_spellings(path: &std::path::Path) -> Vec<String> {
    let raw = path.display().to_string();
    let canonical = path
        .canonicalize()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| raw.clone());
    let mut homes = vec![raw, canonical];
    homes.sort_by_key(|home| std::cmp::Reverse(home.len()));
    homes.dedup();
    homes
}

fn scrub(text: &str, homes: &[String]) -> String {
    let mut text = text.to_owned();
    for home in homes {
        text = text.replace(home.as_str(), "$HOME");
    }
    text
}

/// The conversation with this run's paths and action ids removed, so two
/// runs can be compared. Job lines render nothing, so they must not appear.
fn scrubbed(request: &ModelRequest, homes: &[String]) -> (Option<usize>, Vec<Input>) {
    let mut conversation = request.conversation.clone();
    for input in &mut conversation {
        match input {
            Input::User { text } => *text = scrub(text, homes),
            Input::Assistant {
                text,
                provider_item,
                ..
            } => {
                *text = scrub(text, homes);
                if let Some(item) = provider_item {
                    *item = serde_json::from_str(&scrub(&item.to_string(), homes)).unwrap();
                }
            }
            Input::Reasoning {
                text,
                provider_item,
                ..
            } => {
                *text = scrub(text, homes);
                if let Some(item) = provider_item {
                    *item = serde_json::from_str(&scrub(&item.to_string(), homes)).unwrap();
                }
            }
            Input::ToolCall { action_id, call } => {
                action_id.0 = "a_".into();
                call.arguments =
                    serde_json::from_str(&scrub(&call.arguments.to_string(), homes)).unwrap();
            }
            Input::ToolResult {
                action_id, text, ..
            } => {
                action_id.0 = "a_".into();
                *text = scrub(text, homes);
            }
        }
    }
    (request.previous_end, conversation)
}

fn durable(lines: &[Envelope]) -> Vec<&Envelope> {
    lines.iter().filter(|line| line.seq.is_some()).collect()
}

fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|line| line.kind.as_str()).collect()
}

/// `job_started`, `job_completed`, `tool_call_completed` for `id`, consecutive
/// and under one action.
fn assert_job_triplet(lines: &[Envelope], id: &str, status: &str) -> ActionId {
    let lines = durable(lines);
    let index = lines
        .iter()
        .position(|line| line.kind == "job_started" && line.payload["job_id"] == id)
        .unwrap_or_else(|| panic!("no job_started for {id}"));
    let three = &lines[index..index + 3];
    assert_eq!(three[0].kind, "job_started");
    assert_eq!(three[1].kind, "job_completed");
    assert_eq!(three[2].kind, "tool_call_completed");
    let action = three[0].action_id.clone().unwrap();
    assert!(action.0.starts_with("a_"), "{}", action.0);
    assert_eq!(three[1].action_id.as_ref(), Some(&action));
    assert_eq!(three[2].action_id.as_ref(), Some(&action));
    assert_eq!(three[0].payload["tool"], "shell");
    assert!(three[0].payload.get("extension").is_none());
    assert_eq!(three[0].payload["description"], "npm test");
    assert_eq!(
        three[0].payload["output_path"],
        format!("artifacts/{id}.log")
    );
    assert_eq!(three[1].payload["job_id"], id);
    assert_eq!(three[1].payload["status"], "failed");
    assert_eq!(three[1].payload["error"]["code"], "nonzero_exit");
    assert_eq!(three[1].payload["error"]["message"], "Exit code 1.");
    assert_eq!(three[1].payload["process"]["exit_code"], 1);
    assert_eq!(three[1].payload["process"]["timed_out"], false);
    assert!(three[1].payload["process"].get("signal").is_none());
    assert_eq!(three[1].payload["output_tail"], "1 failing\n");
    assert_eq!(three[2].payload["status"], status);
    assert_eq!(three[2].payload["content"][0]["text"], "ok\n");
    action
}

#[test]
fn a_calls_job_lines_are_written_before_its_completion_and_render_nothing() {
    let id = "j_5e10c0ffee123456";
    let with = run_turn(
        vec![Arc::new(Lines {
            name: "jobber",
            jobs: vec![started_line(id), failed_line(id)],
        })],
        vec![
            calls("Checking.", &["jobber"]),
            fakes::Scripted::text("Done."),
        ],
        Arc::new(crate::TurnCancel::default()),
    );
    let without = run_turn(
        vec![Arc::new(Lines {
            name: "jobber",
            jobs: Vec::new(),
        })],
        vec![
            calls("Checking.", &["jobber"]),
            fakes::Scripted::text("Done."),
        ],
        Arc::new(crate::TurnCancel::default()),
    );
    assert_eq!(with.outcome, Some(TurnOutcome::Completed));
    assert_eq!(without.outcome, Some(TurnOutcome::Completed));
    assert_eq!(
        kinds(&with.lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "job_started",
            "job_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(
        kinds(&without.lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_job_triplet(&with.lines, id, "completed");
    assert_eq!(with.requests.len(), 2);
    assert_eq!(
        scrubbed(&with.requests[1], &with.homes),
        scrubbed(&without.requests[1], &without.homes)
    );
}

#[test]
fn two_calls_write_their_job_lines_in_request_order() {
    let first = "j_aaaaaaaaaaaaaaaa";
    let second = "j_bbbbbbbbbbbbbbbb";
    let ran = run_turn(
        vec![
            Arc::new(Lines {
                name: "first",
                jobs: vec![started_line(first), failed_line(first)],
            }),
            Arc::new(Lines {
                name: "second",
                jobs: vec![started_line(second), failed_line(second)],
            }),
        ],
        vec![
            calls("Checking.", &["first", "second"]),
            fakes::Scripted::text("Done."),
        ],
        Arc::new(crate::TurnCancel::default()),
    );
    assert_eq!(ran.outcome, Some(TurnOutcome::Completed));
    assert_eq!(
        kinds(&ran.lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_started",
            "job_started",
            "job_completed",
            "tool_call_completed",
            "job_started",
            "job_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let lines = durable(&ran.lines);
    let first_at = lines
        .iter()
        .position(|line| line.kind == "job_started")
        .unwrap();
    let second_at = lines
        .iter()
        .rposition(|line| line.kind == "job_started")
        .unwrap();
    assert_eq!(lines[first_at].payload["job_id"], first);
    assert_eq!(lines[second_at].payload["job_id"], second);
    let first_action = lines[first_at].action_id.clone().unwrap();
    let second_action = lines[second_at].action_id.clone().unwrap();
    assert_ne!(first_action, second_action);
    for offset in 0..3 {
        assert_eq!(
            lines[first_at + offset].action_id.as_ref(),
            Some(&first_action)
        );
        assert_eq!(
            lines[second_at + offset].action_id.as_ref(),
            Some(&second_action)
        );
    }
    assert!(lines[second_at].seq > lines[first_at + 2].seq);
}

#[test]
fn a_cancelled_call_still_writes_its_job_lines() {
    let id = "j_5e10c0ffee123456";
    let started = Arc::new((Mutex::new(false), Condvar::new()));
    let watch = Arc::clone(&started);
    let cancel = Arc::new(crate::TurnCancel::default());
    let cancel_watch = Arc::clone(&cancel);
    let watcher = thread::spawn(move || {
        let (lock, cv) = &*watch;
        let guard = lock.lock().unwrap();
        let (guard, timeout) = cv
            .wait_timeout_while(guard, TURN_DEADLINE, |started| !*started)
            .unwrap();
        assert!(!timeout.timed_out() && *guard, "the call did not start");
        drop(guard);
        assert!(cancel_watch.cancel());
    });
    let ran = run_turn(
        vec![Arc::new(Hold {
            jobs: vec![started_line(id), failed_line(id)],
            started,
        })],
        vec![
            calls("Checking.", &["jobber"]),
            fakes::Scripted::text("Done."),
        ],
        cancel,
    );
    watcher.join().unwrap();
    assert_eq!(ran.outcome, Some(TurnOutcome::Interrupted));
    assert_eq!(
        kinds(&ran.lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "job_started",
            "job_completed",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    assert_job_triplet(&ran.lines, id, "cancelled");
}
