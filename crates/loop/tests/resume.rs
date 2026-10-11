//! Resuming a session (`docs/events.md`, "Resume"): the fixed results for
//! calls a crash left without one, and the state a resume folds back.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use support::History;
use support::{
    assistant, fiber_started, handoff_done, handoff_started, hosted_requested, job_completed,
    job_started, kinds_of, message_started, opening_of, result_of, texts,
};
use support::{completed_event, requested, standing_request, started, user_turn};

use contract::events::{Empty, Environment, Event, OpeningMessage};
use contract::provider::Input;
use contract::shapes::DeclaredEffects;
use contract::{ActionId, Envelope, SessionId};
use log::Log;
use serde_json::json;

const MODEL: &str = "fake/model-1";

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc;

use contract::events::{
    DecidedBy, Decision, Grant, PermissionResolved, ReviewerRef, SessionStarted, UsageRecorded,
    Variables, VariablesSource,
};
use contract::inbox::Delivery;
use contract::provider::Provider;
use contract::shapes::Tokens;
use contract::tool::Tool;
use contract::{ErrorCode, GenerationId};
use fakes::{Scripted, ScriptedProvider};
use r#loop::{Loop, Model};

/// `PromptInputs` over temp `home` with a fixed shell: the opening message
/// reads no real files.
fn resume_prompt(home: &std::path::Path) -> r#loop::PromptInputs {
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    r#loop::PromptInputs::new(
        home.to_path_buf(),
        "/bin/sh".into(),
        home.join("events.jsonl").display().to_string(),
        clock,
        fakes::CONTEXT_WINDOW,
    )
}

impl support::History {
    fn tid(&self) -> contract::TurnId {
        contract::TurnId("t_1".into())
    }

    fn write(&self, event: Event, action: Option<&str>) {
        self.log
            .append(&event, Some(self.tid()), action.map(|a| ActionId(a.into())))
            .unwrap();
    }

    /// Freezes the history: the lines a resume folds.
    fn freeze(&mut self) {
        self.history_len = self.lines().len();
    }

    fn model() -> Model {
        Model {
            reference: support::MODEL.into(),
            cost: None,
            subscription: false,
        }
    }

    /// `PromptInputs` over a temp home with a fixed shell: the opening
    /// message reads no real files.
    fn prompt(&self) -> r#loop::PromptInputs {
        resume_prompt(self.root.path())
    }

    fn resume(&mut self, tools: Vec<(String, Arc<dyn Tool>)>) -> Loop {
        // As `parts_with` does without `--credential`: the session keeps
        // the recorded label, so a resume that switches nothing writes no
        // `model_changed` (`docs/model-routing.md`, "Which credential a
        // session uses").
        let recorded = r#loop::resumed(&self.dir).unwrap().credential;
        self.resume_on(
            tools,
            recorded.as_deref(),
            contract::events::CacheLifetime::OneHour,
        )
    }

    /// As [`History::resume`], with the session on `credential` with
    /// `lifetime`: what a resume switching the credential label runs on.
    fn resume_on(
        &mut self,
        tools: Vec<(String, Arc<dyn Tool>)>,
        credential: Option<&str>,
        lifetime: contract::events::CacheLifetime,
    ) -> Loop {
        let mut prompt = self.prompt();
        prompt.credential = credential.map(str::to_owned);
        prompt.cache_lifetime = lifetime;
        Loop::resume(
            Arc::clone(&self.log),
            r#loop::resumed(&self.dir).unwrap(),
            Arc::clone(&self.provider) as Arc<dyn contract::provider::Provider>,
            Self::model(),
            prompt,
            self.inbox_rx.take().unwrap(),
            tools,
            r#loop::Permissions {
                workspace: self.workspace.clone(),
                credentials: self.credentials.clone(),
                credential_files: Vec::new(),
                rules: self.rules.clone(),
            },
        )
        .unwrap()
    }

    /// Sends `prompt`, then runs one turn, failing at [`support::DEADLINE`].
    fn run(&mut self, looped: Loop, prompt: &str) -> contract::events::TurnOutcome {
        self.inbox_tx
            .send(Delivery::Prompt(
                support::message(prompt),
                support::ignore(),
            ))
            .unwrap();
        let mut looped = looped;
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = looped.turn().unwrap().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        let (looped, outcome) = finished
            .recv_timeout(support::DEADLINE)
            .expect("turn ended");
        let _ = looped;
        outcome
    }

    /// The history's kinds, in order: the lines the resume folds.
    fn history_kinds(&self) -> Vec<String> {
        self.lines()[..self.history_len]
            .iter()
            .map(|l| l.kind.clone())
            .collect()
    }

    /// The lines written since [`History::freeze`], and their kinds.
    fn new_lines(&self) -> Vec<Envelope> {
        self.lines()[self.history_len..].to_vec()
    }

    fn new_kinds(&self) -> Vec<String> {
        self.new_lines().iter().map(|l| l.kind.clone()).collect()
    }

    fn usage(id: &str, model: &str, cost: Option<f64>) -> Event {
        Event::UsageRecorded(UsageRecorded {
            generation_id: GenerationId(id.into()),
            model: model.into(),
            tokens: Tokens {
                input: 10,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 3,
            },
            web_searches: None,
            cost,
            subscription: None,
            extension: None,
            origin_session_id: None,
            reviewer: None,
            input_bytes: 1,
            input_media: None,
        })
    }
}

#[test]
fn the_first_request_after_resume_carries_the_earlier_turn_the_fixed_result_and_the_prompt() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(Event::AssistantMessageStarted(Empty {}), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.freeze();
    assert_eq!(
        history.history_kinds(),
        [
            "session_started",
            "turn_started",
            "assistant_message_started",
            "tool_call_requested",
        ]
    );

    let looped = history.resume(Vec::new());
    let outcome = history.run(looped, "two");

    assert_eq!(outcome, contract::events::TurnOutcome::Completed);
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    assert_eq!(conversation.len(), 5);
    // The log holds no opening message, so the resume writes one at its
    // first turn, at the front of the context.
    assert!(
        matches!(&conversation[0], Input::User { text , ..} if text.starts_with("This message is from Fiber"))
    );
    assert!(matches!(&conversation[1], Input::User { text , ..} if text == "one"));
    let Input::ToolCall { action_id, .. } = &conversation[2] else {
        panic!("{conversation:?}");
    };
    assert_eq!(action_id.0, "a_1");
    let (id, text, is_error) = result_of(&conversation[3]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "It never ran.");
    assert!(is_error);
    assert!(matches!(&conversation[4], Input::User { text , ..} if text == "two"));
    // `sent` is the conversation's length at the dead process's last
    // `assistant_message_started`: the previous request ended after "one".
    assert_eq!(requests[0].previous_end, Some(1));
    // The cache key is the session's own id.
    assert_eq!(requests[0].cache_key, "s_1");

    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn resume_writes_no_session_started_and_seq_continues() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(requested("read", "Paris"), Some("a_1"));
    history.freeze();

    let looped = history.resume(Vec::new());
    history.run(looped, "two");

    let lines = history.lines();
    assert_eq!(
        lines.iter().filter(|l| l.kind == "session_started").count(),
        1
    );
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        [
            "session_started",
            "turn_started",
            "tool_call_requested",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let seqs: Vec<u64> = lines.iter().map(|l| l.seq.unwrap().0).collect();
    assert_eq!(seqs, (0..lines.len() as u64).collect::<Vec<_>>());
}

#[test]
fn a_session_grant_from_before_the_resume_is_honoured() {
    let mut tool = support::TestTool::declaring(
        "exec",
        "done",
        vec![contract::shapes::Effect::Executes],
        None,
    );
    tool.subject = Some("run tests".into());
    let tool = Arc::new(tool);
    let mut history = History::new(vec![
        support::tool_call_reply("Go.", &["exec"]),
        Scripted::text("Done."),
    ]);
    history.write(user_turn("one"), None);
    history.write(requested("exec", "Paris"), Some("a_1"));
    history.write(
        Event::PermissionResolved(PermissionResolved {
            request_id: None,
            decision: Decision::Allow,
            decided_by: DecidedBy::Person,
            reason: None,
            feedback: None,
            grant: Some(Grant {
                tool: "exec".into(),
                prefix: "run tests".into(),
            }),
            rule: None,
            reviewer: None,
        }),
        Some("a_1"),
    );
    history.write(started(), Some("a_1"));
    history.write(completed_event("done"), Some("a_1"));
    history.freeze();
    assert_eq!(
        history.history_kinds(),
        [
            "session_started",
            "turn_started",
            "tool_call_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
        ]
    );

    let looped = history.resume(vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)]);
    let outcome = history.run(looped, "two");

    assert_eq!(outcome, contract::events::TurnOutcome::Completed);
    assert_eq!(tool.ran().len(), 1);
    // No new `permission_requested`: the grant decided the call.
    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested"),
        "{:?}",
        history.new_kinds()
    );
    // The decision rode the grant.
    let resolved = history
        .new_lines()
        .into_iter()
        .find(|l| l.kind == "permission_resolved")
        .unwrap();
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(resolved.payload["decided_by"], "session_grant");
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_resolved",
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
}

#[test]
fn budget_counts_spend_from_before_the_resume() {
    let mut history = History::new(vec![]);
    history.write(user_turn("one"), None);
    history.write(History::usage("g1", support::MODEL, Some(5.0)), Some("a_1"));
    history.freeze();
    assert_eq!(
        history.history_kinds(),
        ["session_started", "turn_started", "usage_recorded"]
    );

    let looped = history.resume(Vec::new()).budget(Some(1.0));
    let outcome = history.run(looped, "two");

    assert_eq!(outcome, contract::events::TurnOutcome::Failed);
    assert!(history.provider.requests().is_empty());
    let kinds = history.new_kinds();
    assert_eq!(
        kinds,
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "turn_completed"
        ]
    );
    let completed = history
        .new_lines()
        .into_iter()
        .find(|l| l.kind == "turn_completed")
        .unwrap();
    assert_eq!(completed.payload["error"]["code"], "budget_exceeded");
}

#[test]
fn reviewer_denies_from_before_the_resume_count_toward_the_session_limit() {
    let mut history = History::new(vec![
        support::tool_call_reply("Go.", &["exec"]),
        Scripted::text("After."),
    ]);
    let mut tool = support::TestTool::declaring(
        "exec",
        "done",
        vec![contract::shapes::Effect::Executes],
        None,
    );
    tool.subject = Some("run tests".into());
    let tool = Arc::new(tool);
    for _ in 0..20 {
        history.write(
            Event::PermissionResolved(PermissionResolved {
                request_id: None,
                decision: Decision::Deny,
                decided_by: DecidedBy::Reviewer,
                reason: Some("no".into()),
                feedback: None,
                grant: None,
                rule: None,
                reviewer: Some(ReviewerRef {
                    model: "fake/reviewer-1".into(),
                    stage: 2,
                }),
            }),
            Some("a_old"),
        );
    }
    history.freeze();
    let mut expected = vec!["session_started".to_owned()];
    expected.extend(vec!["permission_resolved".to_owned(); 20]);
    assert_eq!(history.history_kinds(), expected);

    let reviewer = Arc::new(ScriptedProvider::new(vec![
        Scripted::text("check"),
        Scripted::text("block: it writes"),
    ]));
    let (tx, rx) = mpsc::channel();
    let looped = Loop::resume(
        Arc::clone(&history.log),
        r#loop::resumed(&history.dir).unwrap(),
        Arc::clone(&history.provider) as Arc<dyn Provider>,
        History::model(),
        history.prompt(),
        rx,
        vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)],
        r#loop::Permissions {
            workspace: history.workspace.clone(),
            credentials: history.credentials.clone(),
            credential_files: Vec::new(),
            rules: history.rules.clone(),
        },
    )
    .unwrap()
    .answerable(false)
    .reviewer(
        Ok(r#loop::Reviewer {
            provider: reviewer,
            model: Model {
                reference: "fake/reviewer-1".into(),
                cost: None,
                subscription: false,
            },
            cache_lifetime: contract::events::CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            thinking_levels: Vec::new(),
        }),
        r#loop::BlockLimits::default(),
    );
    history.inbox_tx = tx;
    let outcome = history.run(looped, "two");

    // The 21st session block escalates with no person to answer: the turn
    // fails `blocked`.
    assert_eq!(outcome, contract::events::TurnOutcome::Failed);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    let completed = history
        .new_lines()
        .into_iter()
        .find(|l| l.kind == "turn_completed")
        .unwrap();
    assert_eq!(completed.payload["error"]["code"], "blocked");
}

fn resume_after_prior_denials(decided_by: DecidedBy) -> (History, contract::events::TurnOutcome) {
    let mut history = History::new(vec![
        support::tool_call_reply("Go.", &["exec"]),
        Scripted::text("After."),
    ]);
    let mut tool = support::TestTool::declaring(
        "exec",
        "done",
        vec![contract::shapes::Effect::Executes],
        None,
    );
    tool.subject = Some("run tests".into());
    let tool = Arc::new(tool);
    for _ in 0..20 {
        history.write(
            Event::PermissionResolved(PermissionResolved {
                request_id: None,
                decision: Decision::Deny,
                decided_by,
                reason: Some("no".into()),
                feedback: None,
                grant: None,
                rule: None,
                reviewer: None,
            }),
            Some("a_old"),
        );
    }
    history.freeze();
    let mut expected = vec!["session_started".to_owned()];
    expected.extend(vec!["permission_resolved".to_owned(); 20]);
    assert_eq!(history.history_kinds(), expected);

    let reviewer = Arc::new(ScriptedProvider::new(vec![
        Scripted::text("check"),
        Scripted::text("block: it writes"),
    ]));
    let (tx, rx) = mpsc::channel();
    let looped = Loop::resume(
        Arc::clone(&history.log),
        r#loop::resumed(&history.dir).unwrap(),
        Arc::clone(&history.provider) as Arc<dyn Provider>,
        History::model(),
        history.prompt(),
        rx,
        vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)],
        r#loop::Permissions {
            workspace: history.workspace.clone(),
            credentials: history.credentials.clone(),
            credential_files: Vec::new(),
            rules: history.rules.clone(),
        },
    )
    .unwrap()
    .answerable(false)
    .reviewer(
        Ok(r#loop::Reviewer {
            provider: reviewer,
            model: Model {
                reference: "fake/reviewer-1".into(),
                cost: None,
                subscription: false,
            },
            cache_lifetime: contract::events::CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            thinking_levels: Vec::new(),
        }),
        r#loop::BlockLimits::default(),
    );
    history.inbox_tx = tx;
    let outcome = history.run(looped, "two");
    (history, outcome)
}

#[test]
fn no_reviewer_denies_from_before_the_resume_count_toward_the_session_limit() {
    let (history, outcome) = resume_after_prior_denials(DecidedBy::NoReviewer);

    // The 21st session block escalates with no person to answer: the turn
    // fails `blocked`.
    assert_eq!(outcome, contract::events::TurnOutcome::Failed);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    let completed = history
        .new_lines()
        .into_iter()
        .find(|l| l.kind == "turn_completed")
        .unwrap();
    assert_eq!(completed.payload["error"]["code"], "blocked");
}

#[test]
fn non_reviewer_denies_from_before_the_resume_do_not_count() {
    let (history, outcome) = resume_after_prior_denials(DecidedBy::Budget);

    // The first counted block denies the call; the turn completes.
    assert_eq!(outcome, contract::events::TurnOutcome::Completed);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn the_reviewers_first_request_contains_the_earlier_tool_calls() {
    let mut history = History::new(vec![
        support::tool_call_reply("Go.", &["exec"]),
        Scripted::text("Done."),
    ]);
    let mut tool = support::TestTool::declaring(
        "exec",
        "done",
        vec![contract::shapes::Effect::Executes],
        None,
    );
    tool.subject = Some("run tests".into());
    let tool = Arc::new(tool);
    history.write(user_turn("one"), None);
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(completed_event("Paris."), Some("a_1"));
    history.freeze();
    assert_eq!(
        history.history_kinds(),
        [
            "session_started",
            "turn_started",
            "tool_call_requested",
            "tool_call_completed",
        ]
    );

    let reviewer = Arc::new(ScriptedProvider::new(vec![Scripted::text("allow")]));
    let reviewer_provider = Arc::clone(&reviewer);
    let (tx, rx) = mpsc::channel();
    let looped = Loop::resume(
        Arc::clone(&history.log),
        r#loop::resumed(&history.dir).unwrap(),
        Arc::clone(&history.provider) as Arc<dyn Provider>,
        History::model(),
        history.prompt(),
        rx,
        vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)],
        r#loop::Permissions {
            workspace: history.workspace.clone(),
            credentials: history.credentials.clone(),
            credential_files: Vec::new(),
            rules: history.rules.clone(),
        },
    )
    .unwrap()
    .reviewer(
        Ok(r#loop::Reviewer {
            provider: reviewer,
            model: Model {
                reference: "fake/reviewer-1".into(),
                cost: None,
                subscription: false,
            },
            cache_lifetime: contract::events::CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            thinking_levels: Vec::new(),
        }),
        r#loop::BlockLimits::default(),
    );
    history.inbox_tx = tx;
    let outcome = history.run(looped, "two");

    assert_eq!(outcome, contract::events::TurnOutcome::Completed);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "permission_resolved",
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
    let requests = reviewer_provider.requests();
    assert_eq!(requests.len(), 1);
    // The reviewer's key is the session's id plus `reviewer`.
    assert_eq!(requests[0].cache_key, "s_1:reviewer");
    let conversation = &requests[0].conversation;
    assert!(matches!(&conversation[0], Input::User { text , ..} if text == "The person: one"));
    assert!(
        matches!(&conversation[1], Input::User { text , ..} if text.contains("\"read\"")),
        "{conversation:?}"
    );
}

#[test]
fn sent_counts_the_fixed_results_flushed_at_the_last_request() {
    // a1 is requested and never completed; the dead process's second
    // `assistant_message_started` flushes its fixed result into the rebuilt
    // conversation, so `sent` counts it.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(message_started(), Some("a_0"));
    history.freeze();
    assert_eq!(
        history.history_kinds(),
        [
            "session_started",
            "turn_started",
            "tool_call_requested",
            "assistant_message_started",
        ]
    );

    let looped = history.resume(Vec::new());
    history.run(looped, "two");

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    assert_eq!(conversation.len(), 5);
    let (id, text, _) = result_of(&conversation[3]);
    assert_eq!(id.0, "a_1");
    assert_eq!(text, "It never ran.");
    assert_eq!(requests[0].previous_end, Some(3));
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_resume_builds_the_preamble_with_reason_resume() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.freeze();

    let looped = history.resume(Vec::new());
    history.run(looped, "two");

    let new = history.new_lines();
    assert_eq!(new[0].kind, "preamble_built");
    assert_eq!(new[0].payload["reason"], "resume");
    assert_eq!(new[0].payload["model"], support::MODEL);
    // The resumed request carries the rebuilt prompt.
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(new[0].payload["system_prompt"], requests[0].system_prompt);
}

#[test]
fn a_resume_over_a_log_with_an_opening_message_writes_none() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    // The log already holds an opening message, written before the turn
    // and rendered from its logged fields only: `test-os` never ran, so
    // the conversation proves it.
    history.write(
        Event::OpeningMessage(OpeningMessage {
            environment: Environment {
                date: "2023-11-14".into(),
                os: "test-os".into(),
                arch: "test-arch".into(),
                shell: "/bin/sh".into(),
                workspace: history.workspace.clone(),
                git: None,
                session_log: "test-log".into(),
            },
            instruction_files: Vec::new(),
            extension_sections: Vec::new(),
            skills: Vec::new(),
        }),
        None,
    );
    history.write(user_turn("one"), None);
    history.freeze();

    let looped = history.resume(Vec::new());
    history.run(looped, "two");

    assert!(
        history
            .new_lines()
            .iter()
            .all(|line| line.kind != "opening_message"),
        "{:?}",
        history.new_kinds()
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    assert!(matches!(&conversation[0], Input::User { text , ..} if text.contains("test-os")));
    assert!(matches!(&conversation[1], Input::User { text , ..} if text == "one"));
    assert!(matches!(&conversation[2], Input::User { text , ..} if text == "two"));
}

#[test]
fn no_request_before_the_resume_leaves_previous_end_absent() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.freeze();
    assert_eq!(history.history_kinds(), ["session_started", "turn_started"]);

    let looped = history.resume(Vec::new());
    history.run(looped, "two");

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].previous_end, None);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn resumed_returns_the_first_workspace_and_the_last_model() {
    let history = History::new(vec![]);
    history.write(preamble(None), None);
    history.write(History::usage("g1", "fake/first", Some(1.0)), Some("a_1"));
    history.write(switched("fake/second", None, None), None);
    history.write(History::usage("g2", "fake/second", Some(2.0)), Some("a_2"));

    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.session, "s_1");
    assert_eq!(resumed.workspace, history.workspace);
    assert_eq!(resumed.model.as_deref(), Some("fake/second"));
}

#[test]
fn a_delegates_copied_usage_is_not_the_sessions_model() {
    let history = History::new(vec![]);
    history.write(preamble(None), None);
    history.write(History::usage("g1", MODEL, Some(1.0)), Some("a_1"));
    let mut copy = History::usage("g2", "other/x", Some(2.0));
    if let Event::UsageRecorded(recorded) = &mut copy {
        recorded.origin_session_id = Some(SessionId("s_delegate".into()));
    }
    history.write(copy, Some("a_2"));
    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.model.as_deref(), Some(MODEL));
}

#[test]
fn a_reviewers_usage_is_not_the_sessions_model() {
    let history = History::new(vec![]);
    history.write(preamble(None), None);
    history.write(History::usage("g1", MODEL, Some(1.0)), Some("a_1"));
    let mut review = History::usage("g2", "other/reviewer", Some(2.0));
    if let Event::UsageRecorded(recorded) = &mut review {
        recorded.reviewer = Some(contract::events::ReviewerUse::Handoff);
    }
    history.write(review, Some("a_2"));
    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.model.as_deref(), Some(MODEL));
}

#[test]
fn a_late_correction_of_an_earlier_model_s_call_leaves_the_switched_model() {
    let history = History::new(vec![]);
    history.write(History::usage("g1", "fake/first", None), Some("a_1"));
    history.write(switched("fake/second", None, None), None);
    // `g1`'s cost settles after the switch.
    history.write(History::usage("g1", "fake/first", Some(1.0)), Some("a_1"));
    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.model.as_deref(), Some("fake/second"));
}

#[test]
fn resumed_returns_no_model_for_a_log_with_none() {
    let history = History::new(vec![]);
    history.write(user_turn("one"), None);

    let lines = history.lines();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["session_started", "turn_started"]
    );
    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.session, "s_1");
    assert_eq!(resumed.workspace, history.workspace);
    assert_eq!(resumed.model, None);
}

fn preamble(credential: Option<&str>) -> Event {
    Event::PreambleBuilt(contract::events::PreambleBuilt {
        reason: contract::events::PreambleReason::Start,
        model: MODEL.into(),
        context_window: 0,
        trigger_at: None,
        budget: None,
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: contract::events::CacheLifetime::OneHour,
        credential: credential.map(str::to_owned),
        system_prompt: String::new(),
        tools: Vec::new(),
        replaced: Vec::new(),
    })
}

#[test]
fn resumed_returns_the_credential_label_of_the_last_preamble() {
    let history = History::new(vec![]);
    history.write(preamble(Some("work")), None);
    history.write(preamble(Some("personal")), None);
    history.write(user_turn("one"), None);
    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.credential.as_deref(), Some("personal"));
}

#[test]
fn resumed_returns_no_credential_for_a_log_with_no_label() {
    let history = History::new(vec![]);
    assert_eq!(r#loop::resumed(&history.dir).unwrap().credential, None);
    history.write(preamble(None), None);
    assert_eq!(r#loop::resumed(&history.dir).unwrap().credential, None);
}

#[test]
fn resumed_fails_log_corrupt_on_a_log_with_no_session_started() {
    let root = fakes::TempDir::new("fiber-resume");
    let log = Log::create(
        root.path(),
        SessionId("s_9".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(
        &user_turn("one"),
        Some(contract::TurnId("t_1".into())),
        None,
    )
    .unwrap();
    let dir = root.path().join("s_9");
    let lines = log::read(&dir).unwrap();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["turn_started"]
    );

    let error = match r#loop::resumed(&dir) {
        Ok(_) => panic!("a log with no session_started resumes"),
        Err(error) => error,
    };
    assert_eq!(error.code(), ErrorCode::LogCorrupt);
}

/// Appends `line` to the log in `dir` as raw JSON, past the writer: a line
/// whose envelope reads and whose payload need not.
fn append_raw(dir: &std::path::Path, line: &Envelope) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(line).unwrap()).unwrap();
}

/// A `text_completed` line at `seq` whose payload does not read as its kind.
fn unreadable_text(seq: u64) -> Envelope {
    Envelope {
        kind: "text_completed".into(),
        session_id: SessionId("s_9".into()),
        ts: 1,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: Some(contract::TurnId("t_1".into())),
        action_id: Some(ActionId("a_0".into())),
        seq: Some(contract::Seq(seq)),
        payload: json!({"text": 7}).as_object().unwrap().clone(),
    }
}

#[test]
fn resume_fails_log_corrupt_on_an_unreadable_line_in_its_window() {
    // The pass reads no `text_completed` payload, so the line passes it; the
    // window's rebuild reads it and refuses the resume.
    let root = fakes::TempDir::new("fiber-resume");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let created = Log::create(root.path(), SessionId("s_9".into()), Arc::clone(&clock)).unwrap();
    created
        .append(
            &Event::SessionStarted(SessionStarted {
                workspace: root.path().display().to_string(),
                variables: Variables {
                    path: String::new(),
                    names: Vec::new(),
                    source: VariablesSource::Inherited,
                },
                parent: None,
                forked_from: None,
                rewind: None,
                worktree: None,
            }),
            None,
            None,
        )
        .unwrap();
    created
        .append(
            &user_turn("one"),
            Some(contract::TurnId("t_1".into())),
            None,
        )
        .unwrap();
    drop(created);
    let dir = root.path().join("s_9");
    append_raw(&dir, &unreadable_text(2));
    let log = Arc::new(Log::open(root.path(), SessionId("s_9".into()), clock).unwrap());
    let resumed = r#loop::resumed(&dir).unwrap();
    let (_tx, rx) = mpsc::channel();

    let error = match Loop::resume(
        log,
        resumed,
        Arc::new(ScriptedProvider::new(vec![])) as Arc<dyn Provider>,
        History::model(),
        resume_prompt(root.path()),
        rx,
        Vec::new(),
        r#loop::Permissions {
            workspace: root.path().display().to_string(),
            credentials: root.path().to_path_buf(),
            credential_files: Vec::new(),
            rules: Arc::new(support::FakeRules::empty()),
        },
    ) {
        Ok(_) => panic!("a window with an unreadable line resumes"),
        Err(error) => error,
    };
    assert_eq!(error.code(), ErrorCode::LogCorrupt);
}

// #302 part 3: re-raising a pending approval on resume.

use std::sync::Mutex;

use contract::events::{
    AskStep, Escalation, FiberExited, Interaction, InteractionRequested, PermissionRequested,
    RuleOffer, TurnCompleted, TurnOutcome as CompletedOutcome,
};
use contract::inbox::{Ack, Answer};
use contract::shapes::{Effect, Usage};

/// A review approval for `action`, carrying `request_id`, an escalation and
/// a rule offer.
fn review_request(request_id: &str) -> Event {
    Event::PermissionRequested(PermissionRequested {
        request_id: contract::RequestId(request_id.into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: true,
            paths: None,
        },
        step: AskStep::Review {
            escalation: Some(Escalation::ConsecutiveBlocks {
                reason: "it writes".into(),
            }),
            rule: Some(RuleOffer {
                subject: "run tests".into(),
                prefix: "run tests".into(),
            }),
        },
    })
}

fn fiber_exited(suspended_on: Option<&str>) -> Event {
    Event::FiberExited(FiberExited {
        exit_code: 0,
        usage: Usage {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        final_message: None,
        error: None,
        suspended_on: suspended_on.map(|id| contract::RequestId(id.into())),
        questions: None,
    })
}

fn turn_completed() -> Event {
    Event::TurnCompleted(TurnCompleted {
        outcome: CompletedOutcome::Completed,
        error: None,
        questions: None,
    })
}

fn denied_resolved(request_id: Option<&str>) -> Event {
    Event::PermissionResolved(PermissionResolved {
        request_id: request_id.map(|id| contract::RequestId(id.into())),
        decision: Decision::Deny,
        decided_by: DecidedBy::StandingRule,
        reason: Some("No person can answer an approval in this session.".into()),
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
    })
}

/// A denial by `close` of a request pending when the session closed: no
/// decider, so no reviewer object and no rule (`docs/events.md`,
/// `permission_resolved`).
fn closed_resolved(request_id: &str) -> Event {
    Event::PermissionResolved(PermissionResolved {
        request_id: Some(contract::RequestId(request_id.into())),
        decision: Decision::Deny,
        decided_by: DecidedBy::Cancel,
        reason: Some("The session closed while waiting for an answer.".into()),
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
    })
}

fn allowed_resolved() -> Event {
    Event::PermissionResolved(PermissionResolved {
        request_id: None,
        decision: Decision::Allow,
        decided_by: DecidedBy::Person,
        reason: None,
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
    })
}

fn confirm_requested(request_id: &str) -> Event {
    Event::InteractionRequested(InteractionRequested {
        request_id: contract::RequestId(request_id.into()),
        interaction: Interaction::Confirm {
            prompt: "Proceed?".into(),
        },
        action_ids: None,
        extension: None,
        resumes: false,
    })
}

/// An acknowledgement that records its answer.
fn recording() -> (Ack, Arc<Mutex<Option<Answer>>>) {
    let seen: Arc<Mutex<Option<Answer>>> = Arc::new(Mutex::new(None));
    let back = Arc::clone(&seen);
    let ack = Ack(Box::new(move |answer| {
        *back.lock().unwrap() = Some(answer);
    }));
    (ack, seen)
}

fn is_accepted(seen: &Arc<Mutex<Option<Answer>>>) -> bool {
    matches!(&*seen.lock().unwrap(), Some(Ok(_)))
}

impl support::History {
    /// Resumes with `tools` as an unattended session answers: no person
    /// can answer an approval.
    fn resume_headless(&mut self, tools: Vec<(String, Arc<dyn Tool>)>) -> Loop {
        self.resume_headless_with_files(tools, Vec::new())
    }

    /// As [`History::resume_headless`], with `files` as the configured
    /// `file` credential sources (`docs/permissions.md`, "Credentials").
    fn resume_headless_with_files(
        &mut self,
        tools: Vec<(String, Arc<dyn Tool>)>,
        files: Vec<std::path::PathBuf>,
    ) -> Loop {
        let provider = Arc::clone(&self.provider) as Arc<dyn Provider>;
        self.resume_headless_on_with_files(provider, tools, files)
    }

    /// As [`History::resume_headless_with_files`], its model calls reaching
    /// `provider`.
    fn resume_headless_on_with_files(
        &mut self,
        provider: Arc<dyn Provider>,
        tools: Vec<(String, Arc<dyn Tool>)>,
        files: Vec<std::path::PathBuf>,
    ) -> Loop {
        Loop::resume(
            Arc::clone(&self.log),
            r#loop::resumed(&self.dir).unwrap(),
            provider,
            Self::model(),
            self.prompt(),
            self.inbox_rx.take().unwrap(),
            tools,
            r#loop::Permissions {
                workspace: self.workspace.clone(),
                credentials: self.credentials.clone(),
                credential_files: files,
                rules: self.rules.clone(),
            },
        )
        .unwrap()
        .answerable(false)
    }

    /// As [`History::resume_headless`], its model calls reaching `provider`.
    fn resume_headless_on(
        &mut self,
        provider: Arc<dyn Provider>,
        tools: Vec<(String, Arc<dyn Tool>)>,
    ) -> Loop {
        self.resume_headless_on_with_files(provider, tools, Vec::new())
    }

    /// Runs one turn on its own thread, returning the loop for the next
    /// turn, and failing the test past [`support::DEADLINE`].
    fn step(&mut self, mut looped: Loop) -> (Loop, Option<contract::events::TurnOutcome>) {
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = looped.turn().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        finished
            .recv_timeout(support::DEADLINE)
            .expect("the turn ended in time")
    }
}

/// A suspended history: one turn whose batch is `[a_1]` with a standing
/// ask `r_9` pending on it, then the process exited on that request.
fn suspended_history(script: Vec<Scripted>) -> History {
    let mut history = History::new(script);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
}

#[test]
fn an_open_batch_gets_no_fixed_result() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(requested("search", "Paris"), Some("a_2"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    // The finishing turn's request holds both calls and no prompt, and
    // neither call has a fixed result: the finishing turn completes them.
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    let calls: Vec<&str> = conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolCall { action_id, .. } => Some(action_id.0.as_str()),
            Input::ToolResult { .. }
            | Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. } => None,
        })
        .collect();
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(calls, ["a_1", "a_2"]);
    for input in conversation {
        if let Input::ToolResult { text, .. } = input {
            assert!(
                !text.contains("never ran") && !text.contains("unknown"),
                "{text}"
            );
        }
    }
}

#[test]
fn without_a_suspend_the_same_batch_gets_fixed_results() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(requested("search", "Paris"), Some("a_2"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let fixed: Vec<(&str, &str)> = requests[0]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolResult {
                action_id, text, ..
            } if text.contains("never ran") => Some((action_id.0.as_str(), text.as_str())),
            Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. } => None,
        })
        .collect();
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(fixed.len(), 2);
    assert_eq!(fixed[0].0, "a_1");
    assert_eq!(fixed[1].0, "a_2");
}

#[test]
fn a_call_outside_the_open_batch_still_gets_its_fixed_result() {
    // `a_0`'s call sits before the last `assistant_message_started`, so it
    // is outside the suspended batch; only it gets a fixed result.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(requested("read", "Paris"), Some("a_0"));
    history.write(message_started(), Some("a_9"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(requested("search", "Paris"), Some("a_2"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let fixed: Vec<&str> = requests[0]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolResult {
                action_id, text, ..
            } if text.contains("never ran") => Some(action_id.0.as_str()),
            Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. } => None,
        })
        .collect();
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(fixed, ["a_0"]);
}

#[test]
fn no_suspended_on_resumes_as_a_cut_short_turn() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(None), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    // Part 1's behaviour: no re-raise, the call keeps its fixed result.
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].conversation.iter().any(|input| matches!(
            input,
            Input::ToolResult { text, .. } if text.contains("never ran")
        )),
        "{:?}",
        requests[0].conversation
    );
}

#[test]
fn a_suspended_on_naming_an_unknown_request_resumes_as_cut_short() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_unknown")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_suspended_on_naming_an_interaction_resumes_as_cut_short() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(confirm_requested("r_7"), None);
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_7")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    // An interaction not logged `resumes: true` does not re-raise.
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
}

#[test]
fn an_already_resolved_request_resumes_as_cut_short() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(denied_resolved(Some("r_9")), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert_eq!(
        history
            .new_kinds()
            .iter()
            .filter(|k| *k == "permission_requested")
            .count(),
        0
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_line_after_fiber_exited_resumes_as_cut_short() {
    // A resumed process started a new turn and died: the last line is not
    // `fiber_exited`, so nothing re-raises.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.write(user_turn("two"), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(
            support::message("three"),
            support::ignore(),
        ))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_completed_turn_resumes_as_cut_short() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(turn_completed(), None);
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_request_outside_the_batch_resumes_as_cut_short() {
    // The request's action `a_9` was never requested after the last
    // `assistant_message_started`: the log is treated as cut short.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_9"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|k| k == "permission_requested")
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_suspended_turn_is_refused_then_the_prompt_runs_next() {
    let mut history = suspended_history(vec![Scripted::text("Hello."), Scripted::text("Second.")]);
    let (prompt_ack, prompt_seen) = recording();
    let (close_ack, close_seen) = recording();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), prompt_ack))
        .unwrap();
    history.inbox_tx.send(Delivery::Close(close_ack)).unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    let (looped, prompted) = history.step(looped);
    assert_eq!(prompted, Some(contract::events::TurnOutcome::Completed));
    let (_looped, closed) = history.step(looped);
    assert_eq!(closed, None);

    // Both deliveries were accepted: the prompt waited behind the
    // finishing turn instead of being rejected `busy`.
    assert!(is_accepted(&prompt_seen), "the prompt was accepted");
    assert!(is_accepted(&close_seen), "close was accepted");

    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let new = history.new_lines();
    // The re-raised request keeps its `request_id` on both lines.
    assert_eq!(new[2].payload["request_id"], "r_9");
    assert_eq!(new[3].payload["request_id"], "r_9");
    assert_eq!(new[3].payload["decision"], "deny");
    assert_eq!(new[3].payload["decided_by"], "cancel");
    assert_eq!(
        new[3].payload["reason"],
        "The session was resumed with nobody to answer."
    );
    assert!(new[3].payload.get("reviewer").is_none());
    assert_eq!(new[4].payload["status"], "denied");
    // The finished turn's `turn_completed` carries the original turn id,
    // and every line of it does.
    assert_eq!(new[2].turn_id.as_ref().unwrap().0, "t_1");
    assert_eq!(new[10].turn_id.as_ref().unwrap().0, "t_1");
    assert_eq!(new[10].payload["outcome"], "completed");
    for line in &new[2..=10] {
        assert_eq!(
            line.turn_id.as_ref().unwrap().0,
            "t_1",
            "every finished-turn line carries the original turn id: {}",
            line.kind
        );
    }
    // No resumed code path starts a call from before the resume.
    assert!(
        !new.iter().any(|l| l.kind == "tool_call_started"),
        "no tool_call_started"
    );

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 2);
    // The first request holds the call and the denial, and no prompt.
    let first = &requests[0].conversation;
    assert!(matches!(&first[2], Input::ToolCall { .. }), "{first:?}");
    assert!(
        matches!(&first[3], Input::ToolResult { text, .. } if text.contains("No person can answer")),
        "{first:?}"
    );
    assert!(
        !first
            .iter()
            .any(|input| matches!(input, Input::User { text , ..} if text == "two")),
        "{first:?}"
    );
    // The second request holds the denial and then the prompt.
    let second = &requests[1].conversation;
    assert!(
        second.iter().any(
            |input| matches!(input, Input::ToolResult { text, .. } if text.contains("No person can answer"))
        ),
        "{second:?}"
    );
    assert!(
        matches!(second.last().unwrap(), Input::User { text , ..} if text == "two"),
        "{second:?}"
    );
}

#[test]
fn a_suspended_call_touching_a_configured_credential_file_is_refused_without_asking() {
    // The call was suspended on a standing ask before its path became a
    // configured `file` credential source. The resume refuses it through
    // the credential deny before it runs, without asking again: no
    // `permission_requested` is raised.
    let keys = fakes::TempDir::new("fiber-resume-keys");
    let key = keys.path().join("openrouter");
    std::fs::write(&key, "sk-file-secret").unwrap();
    let tool = Arc::new(support::TestTool::declaring(
        "read",
        "sk-file-secret",
        vec![contract::shapes::Effect::Reads],
        Some(vec![key.display().to_string()]),
    ));
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless_with_files(
        vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)],
        vec![key.clone()],
    );
    let (looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let new = history.new_lines();
    assert_eq!(new[2].payload["decision"], "deny");
    assert_eq!(new[2].payload["decided_by"], "credential_deny");
    assert_eq!(
        new[2].payload["reason"],
        "The call touches a configured credential file."
    );
    assert_eq!(new[2].payload["request_id"], "r_9");
    assert_eq!(new[3].payload["status"], "denied");
    assert_eq!(new[3].payload["reason"], "credentials");
    assert!(
        !new.iter().any(|l| l.kind == "tool_call_started"),
        "a denied call never starts"
    );
    assert!(tool.ran().is_empty(), "a denied call never runs");
}

#[test]
fn a_resumed_credential_refusal_answers_its_request_and_spares_later_calls() {
    // The batch is `[a_1, a_2, a_3]` with the standing ask `r_9` pending
    // on `a_2`, which touches a configured `file` credential source. The
    // resume refuses `a_2` through the credential deny, answering `r_9`:
    // the call before it is cancelled, as the batch path does, but the
    // call after it is judged and runs normally (`docs/loop.md`, "Tool
    // calls that do not run").
    let keys = fakes::TempDir::new("fiber-resume-keys");
    let key = keys.path().join("openrouter");
    std::fs::write(&key, "sk-file-secret").unwrap();
    let first = reads("first");
    let denied = Arc::new(support::TestTool::declaring(
        "read",
        "sk-file-secret",
        vec![contract::shapes::Effect::Reads],
        Some(vec![key.display().to_string()]),
    ));
    let last = reads("last");
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("first", "Paris"), Some("a_1"));
    history.write(requested("read", "Paris"), Some("a_2"));
    history.write(requested("last", "Paris"), Some("a_3"));
    history.write(standing_request("r_9"), Some("a_2"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless_with_files(
        vec![
            ("builtin".into(), first.clone() as Arc<dyn Tool>),
            ("builtin".into(), denied.clone() as Arc<dyn Tool>),
            ("builtin".into(), last.clone() as Arc<dyn Tool>),
        ],
        vec![key.clone()],
    );
    let (looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let new = history.new_lines();
    // The refusal answers the previously raised request.
    assert_eq!(new[2].payload["request_id"], "r_9");
    assert_eq!(new[2].payload["decision"], "deny");
    assert_eq!(new[2].payload["decided_by"], "credential_deny");
    // In request order: the call before is cancelled, the credential call
    // denied, the call after completed. Only the call after started.
    let done: Vec<(String, String)> = new[4..7]
        .iter()
        .map(|line| {
            assert_eq!(line.kind, "tool_call_completed");
            (
                line.action_id.as_ref().unwrap().0.clone(),
                line.payload["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        done,
        [
            ("a_1".to_owned(), "cancelled".to_owned()),
            ("a_2".to_owned(), "denied".to_owned()),
            ("a_3".to_owned(), "completed".to_owned()),
        ]
    );
    let started: Vec<String> = history
        .new_lines()
        .into_iter()
        .filter(|line| line.kind == "tool_call_started")
        .map(|line| line.action_id.as_ref().unwrap().0.clone())
        .collect();
    assert_eq!(started, ["a_3".to_owned()]);
    assert!(
        first.ran().is_empty(),
        "a call before the refusal never runs"
    );
    assert!(denied.ran().is_empty(), "a denied call never runs");
    assert_eq!(last.ran().len(), 1, "a call after the refusal runs");
}

#[test]
fn a_cancel_reaches_a_call_running_after_a_resumed_credential_refusal() {
    // The credential-refusal branch arms cancellation with the refusal,
    // as the re-raise does: a later call that is still running sees the
    // cancel and the turn ends `interrupted` (`docs/loop.md`, "Interrupt").
    let keys = fakes::TempDir::new("fiber-resume-keys");
    let key = keys.path().join("openrouter");
    std::fs::write(&key, "sk-file-secret").unwrap();
    let denied = Arc::new(support::TestTool::declaring(
        "read",
        "sk-file-secret",
        vec![contract::shapes::Effect::Reads],
        Some(vec![key.display().to_string()]),
    ));
    let mut running = support::TestTool::reads("last", "Paris.");
    running.script = vec![support::Script::WaitCancel];
    let running = Arc::new(running);
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(requested("last", "Paris"), Some("a_2"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let cancel = Arc::new(r#loop::TurnCancel::default());
    let looped = history
        .resume_headless_with_files(
            vec![
                ("builtin".into(), denied.clone() as Arc<dyn Tool>),
                ("builtin".into(), running.clone() as Arc<dyn Tool>),
            ],
            vec![key.clone()],
        )
        .cancelled_by(Arc::clone(&cancel));
    let tap = support::Tap::new(&history.log);
    let cancelling = Arc::clone(&cancel);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            tap.wait_for("tool_call_started");
            assert!(
                cancelling.cancel(),
                "the refusal armed cancellation for the calls that follow"
            );
        });
        let (_looped, finishing) = history.step(looped);
        assert_eq!(finishing, Some(contract::events::TurnOutcome::Interrupted));
    });
    assert!(
        running.cancelled.lock().unwrap().contains(&true),
        "the running call saw the cancel"
    );
    assert!(denied.ran().is_empty(), "a denied call never runs");
}

#[test]
fn a_resumed_credential_refusal_hands_off_from_a_later_call() {
    // The credential-refusal branch runs its batch uncancelled, so a later
    // call whose result sets `control.handoff` restarts the context from
    // its note through `handoff_from_tools`, as in any step: the `!cancelled`
    // guard's effect, and the MISSED mutant that deletes the `!`.
    let keys = fakes::TempDir::new("fiber-resume-keys");
    let key = keys.path().join("openrouter");
    std::fs::write(&key, "sk-file-secret").unwrap();
    let first = reads("first");
    let denied = Arc::new(support::TestTool::declaring(
        "read",
        "sk-file-secret",
        vec![contract::shapes::Effect::Reads],
        Some(vec![key.display().to_string()]),
    ));
    let mut wrapup = support::TestTool::reads("wrapup", "");
    wrapup.output.content = Vec::new();
    wrapup.output.control = Some(contract::events::Control {
        handoff: Some("the note".into()),
        ..Default::default()
    });
    let wrapup = Arc::new(wrapup);
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("first", "Paris"), Some("a_1"));
    history.write(requested("read", "Paris"), Some("a_2"));
    history.write(requested("wrapup", "Paris"), Some("a_3"));
    history.write(standing_request("r_9"), Some("a_2"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless_with_files(
        vec![
            ("builtin".into(), first.clone() as Arc<dyn Tool>),
            ("builtin".into(), denied.clone() as Arc<dyn Tool>),
            ("builtin".into(), wrapup.clone() as Arc<dyn Tool>),
        ],
        vec![key.clone()],
    );
    let (_looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));

    assert!(
        first.ran().is_empty(),
        "a call before the refusal never runs"
    );
    assert!(denied.ran().is_empty(), "a denied call never runs");
    assert_eq!(wrapup.ran().len(), 1, "a call after the refusal runs");
    let handed = new_of(&history, "handoff_completed");
    assert_eq!(handed.len(), 1, "{:?}", history.new_kinds());
    assert_eq!(handed[0].payload["outcome"], "completed");
    assert_eq!(handed[0].payload["note"], json!(["a_3"]));
}

/// A tool whose calls return `control.questions` holding one question.
fn asking(name: &'static str) -> Arc<support::TestTool> {
    let mut tool = support::TestTool::reads(name, "Asked.");
    tool.output.control = Some(contract::events::Control {
        handoff: None,
        questions: Some(vec![contract::shapes::Question {
            header: "h".into(),
            question: "Which?".into(),
            options: Vec::new(),
            multi_select: None,
        }]),
        skill: None,
    });
    Arc::new(tool)
}

/// The one new `turn_completed` holds the asked question and no request
/// was sent.
fn ended_on_the_question(history: &History) {
    let ended = new_of(history, "turn_completed");
    assert_eq!(ended.len(), 1, "{:?}", history.new_kinds());
    assert_eq!(
        serde_json::Value::Object(ended[0].payload.clone()),
        json!({"outcome": "completed",
            "questions": [{"header": "h", "question": "Which?"}]})
    );
    assert!(history.provider.requests().is_empty());
    assert!(
        !history
            .new_kinds()
            .iter()
            .any(|kind| kind == "step_started"),
        "{:?}",
        history.new_kinds()
    );
}

#[test]
fn a_resumed_credential_refusal_ends_the_turn_on_a_later_calls_questions() {
    let keys = fakes::TempDir::new("fiber-resume-keys");
    let key = keys.path().join("openrouter");
    std::fs::write(&key, "sk-file-secret").unwrap();
    let denied = Arc::new(support::TestTool::declaring(
        "read",
        "sk-file-secret",
        vec![contract::shapes::Effect::Reads],
        Some(vec![key.display().to_string()]),
    ));
    let ask = asking("ask");
    let mut history = History::new(Vec::new());
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(requested("ask", "Paris"), Some("a_2"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless_with_files(
        vec![
            ("builtin".into(), denied.clone() as Arc<dyn Tool>),
            ("builtin".into(), ask.clone() as Arc<dyn Tool>),
        ],
        vec![key.clone()],
    );
    let (_looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    assert_eq!(ask.ran().len(), 1, "a call after the refusal runs");
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    ended_on_the_question(&history);
}

/// The one new `turn_completed` ends the turn `failed` with code
/// `blocked` and carries no `questions` key: the headless block budget
/// ends the turn before the batch's questions are processed.
fn ended_blocked(history: &History) {
    let ended = new_of(history, "turn_completed");
    assert_eq!(ended.len(), 1, "{:?}", history.new_kinds());
    assert_eq!(ended[0].payload["outcome"], "failed");
    assert_eq!(ended[0].payload["error"]["code"], "blocked");
    assert!(
        ended[0].payload.get("questions").is_none(),
        "{:?}",
        ended[0].payload
    );
}

/// Twenty reviewer denials, as `resume.rs` folds them: with the default
/// session limit the next block spends the budget.
fn twenty_prior_denials(history: &History) {
    for _ in 0..20 {
        history.write(
            Event::PermissionResolved(PermissionResolved {
                request_id: None,
                decision: Decision::Deny,
                decided_by: DecidedBy::Reviewer,
                reason: Some("no".into()),
                feedback: None,
                grant: None,
                rule: None,
                reviewer: Some(ReviewerRef {
                    model: "fake/reviewer-1".into(),
                    stage: 2,
                }),
            }),
            Some("a_old"),
        );
    }
}

/// A reviewer that blocks the next call it judges.
fn blocking_reviewer() -> Arc<ScriptedProvider> {
    Arc::new(ScriptedProvider::new(vec![
        Scripted::text("check"),
        Scripted::text("block: it writes"),
    ]))
}

/// Judges `looped`'s calls with [`blocking_reviewer`] under the default
/// limits.
fn with_blocking_reviewer(looped: Loop, reviewer: Arc<ScriptedProvider>) -> Loop {
    looped.reviewer(
        Ok(r#loop::Reviewer {
            provider: reviewer,
            model: Model {
                reference: "fake/reviewer-1".into(),
                cost: None,
                subscription: false,
            },
            cache_lifetime: contract::events::CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            thinking_levels: Vec::new(),
        }),
        r#loop::BlockLimits::default(),
    )
}

/// A tool the reviewer judges: it executes, so it is not a fast path.
fn judged(name: &'static str) -> Arc<support::TestTool> {
    let mut tool =
        support::TestTool::declaring(name, "done", vec![contract::shapes::Effect::Executes], None);
    tool.subject = Some("run tests".into());
    Arc::new(tool)
}

#[test]
fn a_resumed_credential_refusal_prefers_the_block_budget_over_later_questions() {
    let keys = fakes::TempDir::new("fiber-resume-keys");
    let key = keys.path().join("openrouter");
    std::fs::write(&key, "sk-file-secret").unwrap();
    let denied = Arc::new(support::TestTool::declaring(
        "read",
        "sk-file-secret",
        vec![contract::shapes::Effect::Reads],
        Some(vec![key.display().to_string()]),
    ));
    let blocked = judged("exec");
    let ask = asking("ask");
    let mut history = History::new(Vec::new());
    twenty_prior_denials(&history);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(requested("exec", "Paris"), Some("a_2"));
    history.write(requested("ask", "Paris"), Some("a_3"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let reviewer = blocking_reviewer();
    let (tx, rx) = mpsc::channel();
    let looped = with_blocking_reviewer(
        Loop::resume(
            Arc::clone(&history.log),
            r#loop::resumed(&history.dir).unwrap(),
            Arc::clone(&history.provider) as Arc<dyn Provider>,
            History::model(),
            history.prompt(),
            rx,
            vec![
                ("builtin".into(), denied.clone() as Arc<dyn Tool>),
                ("builtin".into(), blocked.clone() as Arc<dyn Tool>),
                ("builtin".into(), ask.clone() as Arc<dyn Tool>),
            ],
            r#loop::Permissions {
                workspace: history.workspace.clone(),
                credentials: history.credentials.clone(),
                credential_files: vec![key.clone()],
                rules: history.rules.clone(),
            },
        )
        .unwrap()
        .answerable(false),
        reviewer,
    );
    history.inbox_tx = tx;
    let (_looped, finishing) = history.step(looped);

    // The second call's block is the session's 21st with no person to
    // answer; the third call's questions never end the turn.
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Failed));
    assert!(blocked.ran().is_empty(), "the blocked call never ran");
    assert_eq!(ask.ran().len(), 1, "a call after the block still runs");
    assert!(history.provider.requests().is_empty());
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_resolved",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    ended_blocked(&history);
}

#[test]
fn a_cancel_after_a_resumed_credential_refusal_skips_the_tool_handoff() {
    // The batch ran with a note in hand but a cancel ended it, so the
    // `!cancelled` guard skips `handoff_from_tools`: no `handoff_completed`
    // is written, and the turn ends `interrupted`. Deleting the `!` hands
    // off instead, which this test forbids.
    let keys = fakes::TempDir::new("fiber-resume-keys");
    let key = keys.path().join("openrouter");
    std::fs::write(&key, "sk-file-secret").unwrap();
    let denied = Arc::new(support::TestTool::declaring(
        "read",
        "sk-file-secret",
        vec![contract::shapes::Effect::Reads],
        Some(vec![key.display().to_string()]),
    ));
    let mut wrapup = support::TestTool::reads("wrapup", "");
    wrapup.output.content = Vec::new();
    wrapup.output.control = Some(contract::events::Control {
        handoff: Some("the note".into()),
        ..Default::default()
    });
    let wrapup = Arc::new(wrapup);
    let mut slow = support::TestTool::reads("slow", "Paris.");
    slow.script = vec![support::Script::WaitCancel];
    let slow = Arc::new(slow);
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(requested("wrapup", "Paris"), Some("a_2"));
    history.write(requested("slow", "Paris"), Some("a_3"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let cancel = Arc::new(r#loop::TurnCancel::default());
    let looped = history
        .resume_headless_with_files(
            vec![
                ("builtin".into(), denied.clone() as Arc<dyn Tool>),
                ("builtin".into(), wrapup.clone() as Arc<dyn Tool>),
                ("builtin".into(), slow.clone() as Arc<dyn Tool>),
            ],
            vec![key.clone()],
        )
        .cancelled_by(Arc::clone(&cancel));
    let tap = support::Tap::new(&history.log);
    let cancelling = Arc::clone(&cancel);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            // Both waits carry the test's deadline: the first is the
            // refusal, the second the noted call. Cancelling only after the
            // second keeps the note while the slow call still waits, so the
            // cancel ends the batch with the note in hand.
            tap.wait_for("tool_call_completed");
            tap.wait_for("tool_call_completed");
            assert!(cancelling.cancel(), "the cancel ended the batch");
        });
        let (_looped, finishing) = history.step(looped);
        assert_eq!(finishing, Some(contract::events::TurnOutcome::Interrupted));
    });
    assert!(denied.ran().is_empty(), "a denied call never runs");
    assert_eq!(wrapup.ran().len(), 1, "the noted call ran");
    assert!(
        slow.cancelled.lock().unwrap().contains(&true),
        "the slow call saw the cancel"
    );
    // The note was in hand: the noted call completed with its handoff.
    let done: Vec<(String, String)> = new_of(&history, "tool_call_completed")
        .into_iter()
        .map(|line| {
            (
                line.action_id.as_ref().unwrap().0.clone(),
                line.payload["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        done,
        [
            ("a_1".to_owned(), "denied".to_owned()),
            ("a_2".to_owned(), "completed".to_owned()),
            ("a_3".to_owned(), "cancelled".to_owned()),
        ]
    );
    let noted = new_of(&history, "tool_call_completed")
        .into_iter()
        .find(|line| line.action_id.as_ref().unwrap().0 == "a_2")
        .unwrap();
    assert_eq!(noted.payload["control"], json!({"handoff": "the note"}));
    assert!(
        new_of(&history, "handoff_completed").is_empty(),
        "a cancel that ended the batch skips the tool handoff: {:?}",
        history.new_kinds()
    );
}

#[test]
fn a_three_call_batch_completes_in_request_order() {
    let mut history = History::new(vec![Scripted::text("Hello."), Scripted::text("Second.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(requested("exec", "Paris"), Some("a_2"));
    history.write(requested("search", "Paris"), Some("a_3"));
    // The first call was allowed in the log; the second is pending.
    history.write(allowed_resolved(), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_2"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let (prompt_ack, prompt_seen) = recording();
    let (close_ack, _) = recording();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), prompt_ack))
        .unwrap();
    history.inbox_tx.send(Delivery::Close(close_ack)).unwrap();

    let looped = history.resume_headless(Vec::new());
    let (looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    let (_looped, prompted) = history.step(looped);
    assert_eq!(prompted, Some(contract::events::TurnOutcome::Completed));
    assert!(is_accepted(&prompt_seen));

    let new = history.new_lines();
    assert_eq!(new[2].kind, "permission_requested");
    assert_eq!(new[3].kind, "permission_resolved");
    let completions: Vec<(&str, &str)> = new[4..7]
        .iter()
        .map(|line| {
            assert_eq!(line.kind, "tool_call_completed");
            (
                line.action_id.as_ref().unwrap().0.as_str(),
                line.payload["status"].as_str().unwrap(),
            )
        })
        .collect();
    // In request order: the allowed call is cancelled all the same, the
    // pending one denied, the third cancelled. None started.
    assert_eq!(
        completions,
        [
            ("a_1", "cancelled"),
            ("a_2", "denied"),
            ("a_3", "cancelled")
        ]
    );
    assert!(!new.iter().any(|l| l.kind == "tool_call_started"));
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_review_request_re_raises_with_its_escalation_and_offer() {
    let mut history = History::new(vec![Scripted::text("Hello."), Scripted::text("Second.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("exec", "Paris"), Some("a_1"));
    history.write(review_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    let before = history
        .lines()
        .iter()
        .find(|l| l.kind == "permission_requested")
        .unwrap()
        .payload
        .clone();

    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();
    let looped = history.resume_headless(Vec::new());
    let (looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    let new = history.new_lines();
    assert_eq!(new[2].kind, "permission_requested");
    assert_eq!(new[2].payload["request_id"], "r_9");
    assert_eq!(new[2].payload, before);
    assert_eq!(new[2].payload["step"], "review");
    assert!(new[2].payload.get("escalation").is_some());
    assert!(new[2].payload.get("rule").is_some());
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_tool_called_in_the_finishing_turn_runs() {
    let tool = Arc::new(support::TestTool::reads("read", "Paris."));
    let mut history = History::new(vec![
        support::tool_call_reply("Go.", &["read"]),
        Scripted::text("Done."),
        Scripted::text("Second."),
    ]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("exec", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), support::ignore()))
        .unwrap();
    let looped = history.resume_headless(vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)]);
    let (looped, finishing) = history.step(looped);
    assert_eq!(finishing, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    // The new call ran to completion inside the finishing turn.
    assert_eq!(tool.ran().len(), 1);
    let new = history.new_lines();
    let ran: Vec<&str> = new
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .map(|l| l.payload["status"].as_str().unwrap())
        .collect();
    // The suspended call denied, the new one completed.
    assert_eq!(ran, ["denied", "completed"]);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
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
}

#[test]
fn a_completed_call_in_the_window_is_not_in_the_batch() {
    // `a_0` was requested and completed after the last
    // `assistant_message_started`; only the pending `a_1` is finished.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_9"));
    history.write(requested("read", "Paris"), Some("a_0"));
    history.write(completed_event("Paris."), Some("a_0"));
    history.write(requested("exec", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless(Vec::new());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let _ = looped;

    // Exactly one new completion, for the pending call: the completed
    // call keeps its single result.
    let new = history.new_lines();
    let completions: Vec<&str> = new
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .map(|l| l.action_id.as_ref().unwrap().0.as_str())
        .collect();
    assert_eq!(completions, ["a_1"]);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let results: Vec<(&str, &str)> = requests[0]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolResult {
                action_id, text, ..
            } => Some((action_id.0.as_str(), text.as_str())),
            Input::ToolCall { .. }
            | Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. } => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "a_0");
    assert_eq!(results[0].1, "Paris.");
    assert_eq!(results[1].0, "a_1");
    assert!(results[1].1.contains("No person can answer"));
}

// Orphans: a job a crash left running is marked on open.

const ORPHANED: &str = "The process that ran this job died; it may still be running.";

/// The `orphaned` completion a resume writes for `id`.
fn assert_orphaned(line: &Envelope, id: &str) {
    assert_eq!(line.kind, "job_completed");
    assert_eq!(line.turn_id, None);
    assert_eq!(line.action_id, None);
    assert_eq!(
        serde_json::Value::Object(line.payload.clone()),
        json!({
            "job_id": id,
            "status": "failed",
            "error": {"code": "orphaned", "message": ORPHANED},
        })
    );
}

#[test]
fn a_job_with_no_completion_is_marked_orphaned_on_resume() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(requested("shell", "Paris"), Some("a_1"));
    history.write(job_started("j_a"), Some("a_1"));
    history.write(job_started("j_b"), Some("a_1"));
    history.write(job_started("j_c"), Some("a_1"));
    // As a `jobs wait` records it, under its call's action.
    history.write(job_completed("j_b"), Some("a_1"));
    history.write(completed_event("Started."), Some("a_1"));
    history.freeze();

    let looped = history.resume(Vec::new());
    // Written on open, before any turn, in start order.
    let marked = history.new_lines();
    assert_eq!(kinds_of(&marked), ["job_completed", "job_completed"]);
    assert_orphaned(&marked[0], "j_a");
    assert_orphaned(&marked[1], "j_c");
    let last = history.lines()[history.history_len - 1].seq.unwrap().0;
    assert_eq!(marked[0].seq.unwrap().0, last + 1);

    assert_eq!(
        history.run(looped, "two"),
        contract::events::TurnOutcome::Completed
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let users: Vec<&str> = requests[0]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } => Some(text.as_str()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. } => None,
        })
        .collect();
    let notice = |id: &str| format!("Fiber: background job {id} ended: failed.\n{ORPHANED}");
    assert_eq!(users[2], notice("j_a"));
    assert_eq!(users[3], notice("j_c"));
    assert_eq!(*users.last().unwrap(), "two");
    assert_eq!(
        history.new_kinds(),
        [
            "job_completed",
            "job_completed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_job_a_rewind_handed_on_is_not_marked() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(requested("shell", "Paris"), Some("a_1"));
    history.write(job_started("j_a"), Some("a_1"));
    history.write(completed_event("Started."), Some("a_1"));
    history
        .log
        .append(
            &Event::Rewound(contract::events::Rewound {
                new_session_id: SessionId("s_2".into()),
                seq: contract::Seq(1),
                from_session_id: None,
                jobs: vec![contract::JobId("j_a".into())],
            }),
            None,
            None,
        )
        .unwrap();
    history.freeze();

    let looped = history.resume(Vec::new());
    assert!(history.new_lines().is_empty());
    drop(looped);
}

#[test]
fn a_job_with_a_completion_is_not_marked() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(job_started("j_a"), Some("a_1"));
    history.write(job_completed("j_a"), None);
    history.freeze();

    let looped = history.resume(Vec::new());
    assert!(history.new_lines().is_empty());
    drop(looped);
}

#[test]
fn an_orphan_behind_a_suspended_batch_renders_after_its_results() {
    // The orphan line is written on open, as on every resume; the notice
    // joins the conversation after the open batch's results, live and on
    // rebuild alike, so no message separates a call from its result.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_9"));
    history.write(requested("shell", "Paris"), Some("a_0"));
    history.write(job_started("j_a"), Some("a_0"));
    history.write(completed_event("Started j_a."), Some("a_0"));
    history.write(requested("exec", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless(Vec::new());
    let marked = history.new_lines();
    assert_eq!(kinds_of(&marked), ["job_completed"]);
    assert_orphaned(&marked[0], "j_a");
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    drop(looped);

    assert_eq!(
        history.new_kinds(),
        [
            "job_completed",
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    assert!(matches!(
        conversation[conversation.len() - 2],
        Input::ToolResult { ref action_id, .. } if action_id.0 == "a_1"
    ));
    assert_eq!(
        conversation.last(),
        Some(&Input::User {
            text: format!("Fiber: background job j_a ended: failed.\n{ORPHANED}"),
            images: Vec::new()
        })
    );
    // A later resume renders the same conversation from the log.
    let rebuilt = r#loop::rebuild(&history.lines(), MODEL).unwrap();
    assert_eq!(&rebuilt[..conversation.len()], conversation.as_slice());
}

#[test]
fn a_second_resume_over_a_logged_orphan_keeps_it_after_the_results() {
    // The first resume logs the orphan and the process exits again on the
    // same request; the second resume reads the orphan from the log and
    // still sends it after the open batch's results.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_9"));
    history.write(requested("shell", "Paris"), Some("a_0"));
    history.write(job_started("j_a"), Some("a_0"));
    history.write(completed_event("Started j_a."), Some("a_0"));
    history.write(requested("exec", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);

    drop(history.resume_headless(Vec::new()));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    let (tx, rx) = mpsc::channel();
    history.inbox_tx = tx;
    history.inbox_rx = Some(rx);
    let looped = history.resume_headless(Vec::new());
    // The orphan is logged once.
    assert!(history.new_lines().is_empty());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    drop(looped);

    let lines = history.lines();
    assert_eq!(
        lines.iter().filter(|l| l.kind == "job_completed").count(),
        1
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    let rebuilt = r#loop::rebuild(&lines, MODEL).unwrap();
    assert_eq!(&rebuilt[..conversation.len()], conversation.as_slice());
    // The rebuild puts the opening message at index 0, so the a_1 call is
    // followed directly by its result, and the notice follows the result.
    let call = conversation
        .iter()
        .position(
            |input| matches!(input, Input::ToolCall { action_id, .. } if action_id.0 == "a_1"),
        )
        .unwrap();
    let result = conversation
        .iter()
        .position(
            |input| matches!(input, Input::ToolResult { action_id, .. } if action_id.0 == "a_1"),
        )
        .unwrap();
    assert_eq!(result, call + 1);
    assert_eq!(result, conversation.len() - 2);
    assert_eq!(
        conversation.last(),
        Some(&Input::User {
            text: format!("Fiber: background job j_a ended: failed.\n{ORPHANED}"),
            images: Vec::new()
        })
    );
}

// Handoff windows (`docs/handoff.md`, "Resume"): the rebuild renders what a
// handoff leaves in force.

/// A history holding an opening, one turn, and the note request's lines.
fn handoff_history(script: Vec<Scripted>) -> History {
    let history = History::new(script);
    history.write(opening_of("old-os"), None);
    history.write(user_turn("one"), None);
    history.write(handoff_started(), None);
    history.write(message_started(), Some("a_note"));
    history.write(assistant("the note"), Some("a_note"));
    history
}

fn resume_conversation(history: &mut History, prompt: &str) -> Vec<Input> {
    let looped = history.resume(Vec::new());
    history.run(looped, prompt);
    history
        .provider
        .requests()
        .last()
        .unwrap()
        .conversation
        .clone()
}

#[test]
fn a_resume_after_a_completed_handoff_sends_from_the_last_handoff() {
    let mut history = handoff_history(vec![Scripted::text("Hello.")]);
    history.write(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    history.write(opening_of("new-os"), None);
    history.freeze();

    let conversation = resume_conversation(&mut history, "two");

    let seen = texts(&conversation);
    assert!(seen[0].contains("new-os"), "{seen:?}");
    assert_eq!(seen[1..], ["one", "the note", "two"]);
    // The new context has had no request, and its opening is written once.
    assert_eq!(history.provider.requests()[0].previous_end, None);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_resume_after_a_crash_mid_handoff_sends_the_context_as_it_was() {
    let mut history = handoff_history(vec![Scripted::text("Two."), Scripted::text("Three.")]);
    // The process that resumes writes `fiber_started`.
    history.write(fiber_started(), None);
    history.freeze();

    let conversation = resume_conversation(&mut history, "two");

    let seen = texts(&conversation);
    assert!(seen[0].contains("old-os"), "{seen:?}");
    assert_eq!(seen[1..], ["one", "two"]);

    // A second resume, after that further turn, still holds it.
    let (tx, rx) = mpsc::channel();
    history.inbox_tx = tx;
    history.inbox_rx = Some(rx);
    history.write(fiber_started(), None);
    history.freeze();
    let conversation = resume_conversation(&mut history, "three");

    let seen = texts(&conversation);
    assert!(seen[0].contains("old-os"), "{seen:?}");
    assert_eq!(seen[1..4], ["one", "two", "Two."]);
    assert_eq!(seen.last().copied(), Some("three"));
}

#[test]
fn a_crash_between_the_completion_and_the_new_opening_writes_it_at_the_next_turn() {
    let mut history = handoff_history(vec![Scripted::text("Hello.")]);
    history.write(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    history.freeze();

    let conversation = resume_conversation(&mut history, "two");

    let seen = texts(&conversation);
    assert!(
        seen[0].starts_with("This message is from Fiber"),
        "{seen:?}"
    );
    assert!(seen[0].contains("Session log:"), "{seen:?}");
    assert!(!seen[0].contains("old-os"));
    assert_eq!(seen[1..], ["one", "the note", "two"]);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

/// The kinds of a resumed turn whose first reply calls a tool and whose
/// second is text.
const RESUMED_TURN: [&str; 15] = [
    "preamble_built",
    "turn_started",
    "step_started",
    "assistant_message_started",
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
];

/// A resumed turn whose first reply calls a tool with `tokens` in its
/// prompt: the context is past two thirds of the default trigger.
fn nudge_after_resume(nudged_before: bool) -> Vec<String> {
    let mut history = History::new(vec![
        support::with_tokens(support::tool_call_reply("", &["get_weather"]), 270_000, 0),
        Scripted::text("Done."),
    ]);
    history.write(opening_of("old-os"), None);
    history.write(user_turn("one"), None);
    if nudged_before {
        history.write(
            Event::ContextNudged(contract::events::ContextNudged {
                tokens: 270_000,
                trigger_at: 400_000,
            }),
            None,
        );
    }
    history.freeze();
    let looped = history.resume(vec![(
        "builtin".to_owned(),
        Arc::new(support::TestTool::reads("get_weather", "abcd")) as Arc<dyn Tool>,
    )]);
    history.run(looped, "two");
    history.new_kinds()
}

#[test]
fn a_context_nudged_before_the_crash_is_not_nudged_again() {
    assert_eq!(nudge_after_resume(true), RESUMED_TURN);
}

#[test]
fn a_context_not_nudged_before_the_crash_is_nudged_once_a_reply_measures_it() {
    let mut expected = RESUMED_TURN.to_vec();
    expected.insert(10, "context_nudged");
    assert_eq!(nudge_after_resume(false), expected);
}

#[test]
fn a_suspended_batch_leaves_a_hosted_call_out() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(hosted_requested(), Some("a_2"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();

    let looped = history.resume_headless(Vec::new());
    let (_looped, outcome) = history.step(looped);

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let new = history.new_lines();
    let done: Vec<_> = new
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .map(|l| l.action_id.clone().unwrap().0)
        .collect();
    assert_eq!(done, ["a_1"], "the hosted call is given no result");
    let conversation = &history.provider.requests()[0].conversation;
    assert!(conversation.iter().all(|i| !matches!(
        i,
        Input::Assistant {
            provider_item: Some(_),
            ..
        }
    )));
    assert!(conversation.iter().all(
        |i| !matches!(i, Input::ToolCall { action_id, .. } | Input::ToolResult { action_id, .. } if action_id.0 == "a_2")
    ));
}

#[test]
fn a_resumed_session_replays_a_logged_section_byte_for_byte() {
    let held = fakes::TempDir::new("fiber-resume-sections");
    let file = held.path().join("index.md");
    std::fs::write(&file, "- [[x]]").unwrap();
    let mut live = support::Session::sectioned(
        vec![Scripted::text("Done.")],
        vec![("fiber.test/notes".into(), vec![file.clone()], Some(5))],
    );
    live.inbox.send(support::delivery("hi")).unwrap();
    live.turn();
    let live_lines = live.lines();
    assert_eq!(
        live_lines
            .iter()
            .map(|l| l.kind.as_str())
            .collect::<Vec<_>>(),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let live_opening = live_lines
        .iter()
        .find(|line| line.kind == "opening_message")
        .unwrap();
    let Input::User {
        text: live_text, ..
    } = &live.requests()[0].conversation[0]
    else {
        panic!("not an opening message");
    };
    let live_text = live_text.clone();
    assert!(live_text.contains("# From the fiber.test/notes extension"));
    // The file is gone before the resume: identical bytes prove the
    // opening is rendered from the log, never the disk.
    std::fs::remove_file(&file).unwrap();
    let message: OpeningMessage =
        serde_json::from_value(serde_json::Value::Object(live_opening.payload.clone())).unwrap();
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(Event::OpeningMessage(message), None);
    history.write(user_turn("one"), None);
    history.freeze();

    let looped = history.resume(Vec::new());
    history.run(looped, "two");
    assert_eq!(
        history.history_kinds(),
        ["session_started", "opening_message", "turn_started"]
    );
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let Input::User { text, .. } = &requests[0].conversation[0] else {
        panic!("not an opening message");
    };
    assert_eq!(text, &live_text);
}

/// A provider that starts a shutdown on `cancel` as each call is made, then
/// answers from `inner`.
struct ShutsDown {
    inner: Arc<ScriptedProvider>,
    cancel: Arc<r#loop::TurnCancel>,
}

impl Provider for ShutsDown {
    fn call(
        &self,
        request: &contract::provider::ModelRequest,
    ) -> Box<dyn contract::provider::ModelCall> {
        self.cancel.shutdown(143);
        self.inner.call(request)
    }
}

/// `fiber_exited` under SIGTERM after the resumed process's lines.
fn exited_on_signal(history: &History) -> Envelope {
    let written =
        r#loop::fiber_exited(&history.log, &history.dir, Ok(()), true, Some(143)).unwrap();
    assert_eq!(written.code, 143);
    history.lines().pop().unwrap()
}

#[test]
fn a_shutdown_before_the_finishing_turn_writes_nothing_and_keeps_the_request() {
    let mut history = suspended_history(vec![Scripted::text("Done.")]);
    let cancel = Arc::new(r#loop::TurnCancel::default());
    let looped = history
        .resume_headless(Vec::new())
        .cancelled_by(Arc::clone(&cancel));
    r#loop::fiber_started(&history.log, "0.0.1", true).unwrap();
    cancel.shutdown(143);
    let (_looped, outcome) = history.step(looped);

    assert_eq!(outcome, None);
    // No preamble, no re-raise: only this process's `fiber_started`.
    assert_eq!(history.new_kinds(), ["fiber_started"]);
    assert!(history.provider.requests().is_empty());
    let exited = exited_on_signal(&history);
    assert_eq!(exited.payload["suspended_on"], "r_9");
}

#[test]
fn a_shutdown_after_the_refusal_ends_the_finishing_turn_interrupted() {
    let mut history = suspended_history(vec![Scripted::text("Done.")]);
    let cancel = Arc::new(r#loop::TurnCancel::default());
    let provider = Arc::new(ShutsDown {
        inner: Arc::clone(&history.provider),
        cancel: Arc::clone(&cancel),
    });
    // A prompt waiting behind the finishing turn is held aside, then
    // answered by the shutdown.
    let (prompt, answer) = recording();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("two"), prompt))
        .unwrap();
    let looped = history
        .resume_headless_on(provider, Vec::new())
        .cancelled_by(Arc::clone(&cancel));
    let (looped, outcome) = history.step(looped);

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Interrupted));
    let kinds = history.new_kinds();
    assert!(kinds.contains(&"permission_requested".to_owned()));
    assert!(kinds.contains(&"permission_resolved".to_owned()));
    assert_eq!(kinds.last().map(String::as_str), Some("turn_completed"));
    // The loop starts no other turn and rejects the held prompt.
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || done.send(looped.run().is_ok()).unwrap());
    assert!(
        finished
            .recv_timeout(support::DEADLINE)
            .expect("run returned")
    );
    match &*answer.lock().unwrap() {
        Some(Err(rejected)) => assert_eq!(rejected.code, contract::ErrorCode::Closing),
        other => panic!("the prompt was answered closing, not {other:?}"),
    }
    assert_eq!(
        history.new_kinds().last().map(String::as_str),
        Some("turn_completed")
    );
    let exited = exited_on_signal(&history);
    assert_eq!(exited.payload.get("suspended_on"), None);
}

// A resume after a completed handoff reads its window only (#874).

/// A history whose first context ends in a completed handoff: the early
/// turn's lines, then the handoff turn, the note and the new opening.
fn handed_off(script: Vec<Scripted>) -> History {
    let history = History::new(script);
    history.write(opening_of("old-os"), None);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(assistant("early answer"), Some("a_0"));
    history.write(user_turn("two"), None);
    history.write(handoff_started(), None);
    history.write(message_started(), Some("a_note"));
    history.write(assistant("the note"), Some("a_note"));
    history.write(
        handoff_done(contract::events::Outcome::Completed, &["a_note"]),
        None,
    );
    history.write(opening_of("new-os"), None);
    history
}

#[test]
fn a_resume_after_a_completed_handoff_sends_the_note_and_what_came_after_only() {
    let mut history = handed_off(vec![Scripted::text("Hello.")]);
    history.write(message_started(), Some("a_3"));
    history.write(assistant("after"), Some("a_3"));
    history.freeze();

    let looped = history.resume(Vec::new());
    history.run(looped, "three");

    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let seen: Vec<&str> = requests[0]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } | Input::Assistant { text, .. } => Some(text.as_str()),
            Input::Reasoning { .. } | Input::ToolCall { .. } | Input::ToolResult { .. } => None,
        })
        .collect();
    assert!(seen[0].contains("new-os"), "{seen:?}");
    assert_eq!(seen[1..], ["two", "the note", "after", "three"]);
}

#[test]
fn a_turn_suspended_after_a_handoff_re_raises_its_request_past_lines_written_after_the_pass() {
    // `main`'s order: the pass, then `fiber_started` and the like, then the
    // resume. The window ends where the pass ended, so the suspension it
    // read still stands.
    let mut history = handed_off(vec![Scripted::text("Hello.")]);
    history.write(user_turn("three"), None);
    history.write(message_started(), Some("a_5"));
    history.write(requested("read", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    let folded = r#loop::resumed(&history.dir).unwrap();
    history.write(fiber_started(), None);
    history.freeze();

    let looped = Loop::resume(
        Arc::clone(&history.log),
        folded,
        Arc::clone(&history.provider) as Arc<dyn Provider>,
        History::model(),
        history.prompt(),
        history.inbox_rx.take().unwrap(),
        Vec::new(),
        r#loop::Permissions {
            workspace: history.workspace.clone(),
            credentials: history.credentials.clone(),
            credential_files: Vec::new(),
            rules: history.rules.clone(),
        },
    )
    .unwrap()
    .answerable(false);
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    drop(looped);

    let raised: Vec<Envelope> = history
        .new_lines()
        .into_iter()
        .filter(|line| line.kind == "permission_requested")
        .collect();
    assert_eq!(raised.len(), 1, "{:?}", history.new_kinds());
    assert_eq!(raised[0].payload["request_id"], "r_9");
    assert_eq!(raised[0].action_id, Some(ActionId("a_1".into())));
}

// #614: a resumed session with a person to answer waits for the reply to
// its re-raised request instead of refusing it.

use contract::commands::{Reply, ReplyAnswer};

/// A suspended history whose batch is `names`, in order, as `a_1`, `a_2`,
/// …, with a standing ask `r_9` pending on `a_<action>`.
fn suspended_batch(script: Vec<Scripted>, names: &[&str], action: usize) -> History {
    let mut history = History::new(script);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    for (at, name) in names.iter().enumerate() {
        history.write(requested(name, "Paris"), Some(&format!("a_{}", at + 1)));
    }
    history.write(standing_request("r_9"), Some(&format!("a_{action}")));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    history
}

/// A reply to `request_id` deciding `decision`, acknowledged into the
/// returned slot.
fn reply_delivery(request_id: &str, decision: Decision) -> (Delivery, Arc<Mutex<Option<Answer>>>) {
    let (ack, seen) = recording();
    let reply = Reply {
        request_id: contract::RequestId(request_id.into()),
        answer: ReplyAnswer::Approval {
            decision,
            feedback: None,
            remember: None,
        },
    };
    (Delivery::Reply(reply, ack), seen)
}

/// Runs `send` once the log holds the re-raised `permission_requested`:
/// the signal the finishing turn is waiting for an answer. One
/// [`support::DEADLINE`].
fn on_reraise(
    history: &History,
    send: impl FnOnce() + Send + 'static,
) -> std::thread::JoinHandle<()> {
    let watcher = history.log.watch();
    std::thread::spawn(move || {
        support::read_until(watcher, "a re-raised permission_requested line", |line| {
            line.kind == "permission_requested"
        });
        send();
    })
}

/// The new lines of `kind`.
fn new_of(history: &History, kind: &str) -> Vec<Envelope> {
    history
        .new_lines()
        .into_iter()
        .filter(|line| line.kind == kind)
        .collect()
}

fn reads(name: &'static str) -> Arc<support::TestTool> {
    Arc::new(support::TestTool::reads(name, "Paris."))
}

fn tools_of(tools: &[&Arc<support::TestTool>]) -> Vec<(String, Arc<dyn Tool>)> {
    tools
        .iter()
        .map(|tool| ("builtin".to_owned(), Arc::clone(tool) as Arc<dyn Tool>))
        .collect()
}

#[test]
fn an_answerable_resume_runs_the_call_a_person_allows() {
    let mut history = suspended_batch(vec![Scripted::text("Done.")], &["act"], 1);
    let act = reads("act");
    let looped = history.resume(tools_of(&[&act]));
    let (delivery, seen) = reply_delivery("r_9", Decision::Allow);
    let inbox = history.inbox_tx.clone();
    let replied = on_reraise(&history, move || inbox.send(delivery).unwrap());
    let (_looped, outcome) = history.step(looped);
    replied.join().unwrap();

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    assert!(is_accepted(&seen), "the reply was accepted");
    let raised = new_of(&history, "permission_requested");
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].payload["request_id"], "r_9");
    let resolved = new_of(&history, "permission_resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["request_id"], "r_9");
    assert_eq!(resolved[0].payload["decision"], "allow");
    assert_eq!(resolved[0].payload["decided_by"], "person");
    assert_eq!(act.ran().len(), 1);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
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
    let done = new_of(&history, "tool_call_completed");
    assert_eq!(done[0].payload["status"], "completed");
    assert_eq!(done[0].action_id, Some(ActionId("a_1".into())));
}

#[test]
fn an_allowed_call_that_sets_control_handoff_hands_off_after_the_batch() {
    // The finishing turn's batch ran uncancelled: a call whose result set
    // `control.handoff` restarts the context from its note, as in any step.
    let mut history = suspended_batch(vec![Scripted::text("Done.")], &["wrapup"], 1);
    let mut wrapup = support::TestTool::reads("wrapup", "");
    wrapup.output.content = Vec::new();
    wrapup.output.control = Some(contract::events::Control {
        handoff: Some("the note".into()),
        ..Default::default()
    });
    let wrapup = Arc::new(wrapup);
    let looped = history.resume(tools_of(&[&wrapup]));
    let (delivery, seen) = reply_delivery("r_9", Decision::Allow);
    history.inbox_tx.send(delivery).unwrap();
    let (_looped, outcome) = history.step(looped);

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    assert!(is_accepted(&seen), "the reply was accepted");
    assert_eq!(wrapup.ran().len(), 1);
    let handed = new_of(&history, "handoff_completed");
    assert_eq!(handed.len(), 1, "{:?}", history.new_kinds());
    assert_eq!(handed[0].payload["outcome"], "completed");
    assert_eq!(handed[0].payload["note"], json!(["a_1"]));
}

#[test]
fn an_allowed_call_that_asks_ends_the_finishing_turn_with_its_questions() {
    let mut history = suspended_batch(Vec::new(), &["ask"], 1);
    let ask = asking("ask");
    let looped = history.resume(tools_of(&[&ask]));
    let (delivery, seen) = reply_delivery("r_9", Decision::Allow);
    history.inbox_tx.send(delivery).unwrap();
    let (_looped, outcome) = history.step(looped);

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    assert!(is_accepted(&seen), "the reply was accepted");
    assert_eq!(ask.ran().len(), 1);
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    ended_on_the_question(&history);
}

#[test]
fn an_answered_resume_prefers_the_block_budget_over_later_questions() {
    // The batch is `act`, judged below, then `exec` and `ask`. The reply
    // allows `act`; the `close` behind it ends the `exec` block's wait,
    // so the headless budget spends the session's 21st block and the
    // `ask` call's questions never end the turn.
    let mut history = History::new(Vec::new());
    twenty_prior_denials(&history);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("act", "Paris"), Some("a_1"));
    history.write(requested("exec", "Paris"), Some("a_2"));
    history.write(requested("ask", "Paris"), Some("a_3"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    let act = reads("act");
    let blocked = judged("exec");
    let ask = asking("ask");
    let reviewer = blocking_reviewer();
    let (tx, rx) = mpsc::channel();
    let looped = with_blocking_reviewer(
        Loop::resume(
            Arc::clone(&history.log),
            r#loop::resumed(&history.dir).unwrap(),
            Arc::clone(&history.provider) as Arc<dyn Provider>,
            History::model(),
            history.prompt(),
            rx,
            tools_of(&[&act, &blocked, &ask]),
            r#loop::Permissions {
                workspace: history.workspace.clone(),
                credentials: history.credentials.clone(),
                credential_files: Vec::new(),
                rules: history.rules.clone(),
            },
        )
        .unwrap(),
        reviewer,
    );
    history.inbox_tx = tx;
    let (delivery, seen) = reply_delivery("r_9", Decision::Allow);
    let (ack, closed) = recording();
    history.inbox_tx.send(delivery).unwrap();
    history.inbox_tx.send(Delivery::Close(ack)).unwrap();
    let (_looped, outcome) = history.step(looped);

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Failed));
    assert!(is_accepted(&seen), "the reply was accepted");
    assert!(is_accepted(&closed), "close was accepted");
    assert_eq!(act.ran().len(), 1);
    assert!(blocked.ran().is_empty(), "the blocked call never ran");
    assert_eq!(ask.ran().len(), 1, "a call after the block still runs");
    assert!(history.provider.requests().is_empty());
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "usage_recorded",
            "usage_recorded",
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    ended_blocked(&history);
}

#[test]
fn a_reply_queued_before_the_resume_answers_the_re_raised_request() {
    let mut history = suspended_batch(vec![Scripted::text("Done.")], &["act"], 1);
    let act = reads("act");
    let (delivery, seen) = reply_delivery("r_9", Decision::Allow);
    history.inbox_tx.send(delivery).unwrap();
    let looped = history.resume(tools_of(&[&act]));
    let (_looped, outcome) = history.step(looped);

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    assert!(is_accepted(&seen), "the held reply was accepted");
    let resolved = new_of(&history, "permission_resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["decided_by"], "person");
    assert_eq!(act.ran().len(), 1);
}

#[test]
fn an_answerable_resume_denies_the_call_a_person_refuses() {
    let mut history = suspended_batch(vec![Scripted::text("Done.")], &["act"], 1);
    let act = reads("act");
    let looped = history.resume(tools_of(&[&act]));
    let (delivery, seen) = reply_delivery("r_9", Decision::Deny);
    let inbox = history.inbox_tx.clone();
    let replied = on_reraise(&history, move || inbox.send(delivery).unwrap());
    let (_looped, outcome) = history.step(looped);
    replied.join().unwrap();

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    assert!(is_accepted(&seen));
    let resolved = new_of(&history, "permission_resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["request_id"], "r_9");
    assert_eq!(resolved[0].payload["decision"], "deny");
    assert_eq!(resolved[0].payload["decided_by"], "person");
    assert!(act.ran().is_empty());
    assert!(new_of(&history, "tool_call_started").is_empty());
    let done = new_of(&history, "tool_call_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "denied");
}

#[test]
fn a_reply_naming_another_request_is_stale_and_the_wait_goes_on() {
    let mut history = suspended_batch(vec![Scripted::text("Done.")], &["act"], 1);
    let act = reads("act");
    let looped = history.resume(tools_of(&[&act]));
    let (stale, stale_seen) = reply_delivery("r_other", Decision::Allow);
    let (good, good_seen) = reply_delivery("r_9", Decision::Allow);
    let inbox = history.inbox_tx.clone();
    let replied = on_reraise(&history, move || {
        inbox.send(stale).unwrap();
        inbox.send(good).unwrap();
    });
    let (_looped, outcome) = history.step(looped);
    replied.join().unwrap();

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    match &*stale_seen.lock().unwrap() {
        Some(Err(rejected)) => assert_eq!(rejected.code, ErrorCode::StaleRequest),
        other => panic!("the stale reply was rejected stale_request, not {other:?}"),
    }
    assert!(is_accepted(&good_seen));
    assert_eq!(new_of(&history, "permission_resolved").len(), 1);
    assert_eq!(act.ran().len(), 1);
}

#[test]
fn the_idle_delay_while_waiting_leaves_the_request_pending_again() {
    let mut history = suspended_batch(vec![Scripted::text("Done.")], &["act"], 1);
    let act = reads("act");
    let looped = history
        .resume(tools_of(&[&act]))
        .idle_exit(Some(std::time::Duration::from_secs(60)));
    r#loop::fiber_started(&history.log, "0.0.1", true).unwrap();
    let clock = Arc::clone(&history.clock);
    let deadline = contract::clock::Clock::now(clock.as_ref()) + std::time::Duration::from_secs(60);
    let inbox = history.inbox_tx.clone();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let mut looped = looped;
        done.send(looped.turn().unwrap()).unwrap();
    });
    assert!(
        clock.await_parked(deadline, support::DEADLINE),
        "the re-raised request's wait parks until the idle deadline"
    );
    clock.advance(std::time::Duration::from_secs(60));
    inbox.send(Delivery::Cancelled).unwrap();
    let outcome = finished
        .recv_timeout(support::DEADLINE)
        .expect("the finishing turn ended at the idle deadline");

    assert_eq!(outcome, None);
    assert!(act.ran().is_empty());
    assert_eq!(
        history.new_kinds(),
        [
            "fiber_started",
            "preamble_built",
            "opening_message",
            "permission_requested"
        ]
    );
    r#loop::fiber_exited(&history.log, &history.dir, Ok(()), false, None).unwrap();
    let exited = history.lines().pop().unwrap();
    assert_eq!(exited.payload["suspended_on"], "r_9");
}

#[test]
fn close_while_waiting_denies_the_re_raised_review_request_by_cancel() {
    // The suspended batch is `[a_1]` with a review request pending on it,
    // as `a_review_request_re_raises_with_its_escalation_and_offer` writes
    // it: the close denies the re-raised request itself, not by any rule.
    let mut history = History::new(vec![Scripted::text("Done.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("exec", "Paris"), Some("a_1"));
    history.write(review_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    let exec = reads("exec");
    let looped = history.resume(tools_of(&[&exec]));
    let (ack, closed) = recording();
    let inbox = history.inbox_tx.clone();
    let replied = on_reraise(&history, move || inbox.send(Delivery::Close(ack)).unwrap());
    let (_looped, outcome) = history.step(looped);
    replied.join().unwrap();

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    assert!(is_accepted(&closed), "close was accepted");
    let resolved = new_of(&history, "permission_resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[0].payload,
        closed_resolved("r_9").payload().unwrap()
    );
    assert!(exec.ran().is_empty());
    let done = new_of(&history, "tool_call_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "denied");
    assert_eq!(done[0].payload["reason"], "no_person");
}

#[test]
fn a_headless_resume_denies_the_re_raised_review_request_by_cancel() {
    let mut history = History::new(vec![Scripted::text("Done.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("exec", "Paris"), Some("a_1"));
    history.write(review_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    let exec = reads("exec");
    let looped = history.resume_headless(tools_of(&[&exec]));
    let (_looped, outcome) = history.step(looped);

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    let resolved = new_of(&history, "permission_resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["request_id"], "r_9");
    assert_eq!(resolved[0].payload["decision"], "deny");
    assert_eq!(resolved[0].payload["decided_by"], "cancel");
    assert_eq!(
        resolved[0].payload["reason"],
        "The session was resumed with nobody to answer."
    );
    assert!(resolved[0].payload.get("reviewer").is_none());
    assert!(exec.ran().is_empty());
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

/// A shutdown that closes the inbox while the re-raised request waits keeps
/// it pending: no denial is written, so the next resume raises it again
/// (`docs/invocation.md`, "Shutdown").
#[test]
fn a_shutdown_that_closes_the_inbox_while_waiting_keeps_the_re_raised_request() {
    // The suspended batch is `[a_1]` with a review request pending on it,
    // as `a_review_request_re_raises_with_its_escalation_and_offer` writes
    // it.
    let mut history = History::new(vec![Scripted::text("Done.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(requested("exec", "Paris"), Some("a_1"));
    history.write(review_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    let exec = reads("exec");
    let cancel = Arc::new(r#loop::TurnCancel::default());
    let looped = history
        .resume(tools_of(&[&exec]))
        .cancelled_by(Arc::clone(&cancel));
    // The resume owns the inbox's only sender through `inbox_rx`: moving
    // `inbox_tx` out leaves the drop below to disconnect the wait.
    let inbox = std::mem::replace(&mut history.inbox_tx, mpsc::channel().0);
    let watcher = history.log.watch();
    let clock = Arc::clone(&history.clock);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            support::read_until(watcher, "a re-raised permission_requested line", |line| {
                line.kind == "permission_requested"
            });
            // The wait parks in `recv` with no deadline, so the drop below
            // disconnects it: the shutdown's wake would end the wait
            // without a disconnect instead. The park wait takes
            // [`support::DEADLINE`].
            assert!(
                clock.await_parked_unbounded(support::DEADLINE),
                "the re-raised request's wait parked in recv"
            );
            cancel.shutdown(143);
            drop(inbox);
        });
        // The wait ends as the idle delay ends it: no turn outcome, and
        // nothing written past the re-raised request.
        let (_looped, outcome) = history.step(looped);
        assert_eq!(outcome, None);
    });
    assert!(exec.ran().is_empty());
    let new = history.new_lines();
    assert_eq!(new.last().unwrap().kind, "permission_requested");
    assert!(new.iter().all(|line| line.kind != "permission_resolved"));
    let exited = exited_on_signal(&history);
    assert_eq!(exited.payload["suspended_on"], "r_9");
}

#[test]
fn calls_before_the_action_are_cancelled_and_calls_after_it_run_after_the_reply() {
    let mut history = suspended_batch(vec![Scripted::text("Done.")], &["first", "act", "last"], 2);
    let first = reads("first");
    let act = reads("act");
    let last = reads("last");
    let looped = history.resume(tools_of(&[&first, &act, &last]));
    let (delivery, _seen) = reply_delivery("r_9", Decision::Allow);
    let inbox = history.inbox_tx.clone();
    let replied = on_reraise(&history, move || inbox.send(delivery).unwrap());
    let (_looped, outcome) = history.step(looped);
    replied.join().unwrap();

    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    assert!(
        first.ran().is_empty(),
        "a call before the action never runs"
    );
    assert_eq!(act.ran().len(), 1);
    assert_eq!(
        last.ran().len(),
        1,
        "a call after the action is judged and runs"
    );
    let started: Vec<Option<ActionId>> = new_of(&history, "tool_call_started")
        .into_iter()
        .map(|line| line.action_id)
        .collect();
    assert_eq!(
        started,
        [Some(ActionId("a_2".into())), Some(ActionId("a_3".into()))]
    );
    let done: Vec<(Option<ActionId>, String)> = new_of(&history, "tool_call_completed")
        .into_iter()
        .map(|line| {
            let status = line.payload["status"].as_str().unwrap().to_owned();
            (line.action_id, status)
        })
        .collect();
    assert_eq!(
        done,
        [
            (Some(ActionId("a_1".into())), "cancelled".to_owned()),
            (Some(ActionId("a_2".into())), "completed".to_owned()),
            (Some(ActionId("a_3".into())), "completed".to_owned()),
        ]
    );
    // Every decision line comes before any `tool_call_started`.
    let kinds = history.new_kinds();
    let last_decision = kinds
        .iter()
        .rposition(|k| k == "permission_resolved")
        .unwrap();
    let first_start = kinds.iter().position(|k| k == "tool_call_started").unwrap();
    assert!(last_decision < first_start, "{kinds:?}");
}

fn switched(after: &str, thinking: Option<&str>, credential: Option<&str>) -> Event {
    Event::ModelChanged(contract::events::ModelChanged {
        before: contract::events::ModelSettings {
            model: support::MODEL.into(),
            thinking: None,
            cache_lifetime: contract::events::CacheLifetime::OneHour,
            credential: Some("work".into()),
        },
        after: contract::events::ModelSettings {
            model: after.into(),
            thinking: thinking.map(str::to_owned),
            cache_lifetime: contract::events::CacheLifetime::OneHour,
            credential: credential.map(str::to_owned),
        },
        source: contract::events::SwitchSource::Driver,
    })
}

#[test]
fn resumed_folds_model_credential_and_thinking_from_the_last_switch() {
    let mut history = History::new(vec![]);
    history.write(preamble(Some("work")), None);
    history.write(
        switched("fake/second", Some("high"), Some("personal")),
        None,
    );
    history.write(switched("fake/third", None, None), None);
    history.freeze();
    assert_eq!(
        kinds_of(&history.lines()),
        vec![
            "session_started",
            "preamble_built",
            "model_changed",
            "model_changed"
        ]
    );
    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.model.as_deref(), Some("fake/third"));
    assert_eq!(resumed.credential, None);
    assert_eq!(resumed.thinking.as_deref(), None);
}

#[test]
fn resumed_keeps_the_switch_thinking_as_the_session_choice() {
    let mut history = History::new(vec![]);
    history.write(preamble(Some("work")), None);
    history.write(switched("fake/second", Some("high"), Some("work")), None);
    history.freeze();
    assert_eq!(
        kinds_of(&history.lines()),
        vec!["session_started", "preamble_built", "model_changed"]
    );
    let resumed = r#loop::resumed(&history.dir).unwrap();
    assert_eq!(resumed.thinking.as_deref(), Some("high"));
    assert_eq!(resumed.model.as_deref(), Some("fake/second"));
    assert_eq!(resumed.credential.as_deref(), Some("work"));
}

#[test]
fn resumed_thinking_seeds_the_session_choice_for_the_next_switch() {
    use std::sync::{Arc, Mutex};

    let mut history = History::new(vec![Scripted::text("Hello."), Scripted::text("Again.")]);
    history.write(preamble(Some("work")), None);
    history.write(switched("fake/second", Some("high"), Some("work")), None);
    history.freeze();

    let recorded: Arc<Mutex<Vec<Option<contract::ThinkingLevel>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&recorded);
    let next = Arc::new(ScriptedProvider::new(vec![Scripted::text("New.")]));
    let prepare: r#loop::Prepare = Arc::new(
        move |args: &contract::commands::ModelArgs,
              _label: Option<&str>,
              chosen: Option<contract::ThinkingLevel>| {
            seen.lock().unwrap().push(chosen);
            let thinking = match &args.thinking {
                Some(level) => Some(level.parse().map_err(|_| contract::inbox::Rejection {
                    code: contract::ErrorCode::InvalidArguments,
                    message: "bad thinking".into(),
                })?),
                None => chosen,
            };
            let kept = thinking.or(chosen);
            Ok(r#loop::Prepared {
                provider: Arc::clone(&next) as Arc<dyn contract::provider::Provider>,
                model: r#loop::Model {
                    reference: "fake/third".into(),
                    cost: None,
                    subscription: false,
                },
                thinking: kept,
                chosen: kept,
                credential: Some("work".into()),
                cache_lifetime: contract::events::CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: r#loop::HandoffSettings::default(),
                reviewer: Err(contract::shapes::Failure {
                    code: contract::ErrorCode::NoModel,
                    message: r#loop::NO_MODEL_MESSAGE.into(),
                    retry_after_ms: None,
                    provider: None,
                }),
                web_search: r#loop::Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
            })
        },
    );
    let looped = history
        .resume(Vec::new())
        .switcher(prepare, r#loop::Switchable { chosen: None });
    // A model-only switch after the resume is given the folded choice.
    history
        .inbox_tx
        .send(Delivery::Model(
            contract::commands::ModelArgs {
                model: "fake/third".into(),
                thinking: None,
            },
            support::ignore(),
        ))
        .unwrap();
    history
        .inbox_tx
        .send(Delivery::Prompt(support::message("go"), support::ignore()))
        .unwrap();
    let (looped, outcome) = history.step(looped);
    let _ = looped;
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    assert_eq!(
        *recorded.lock().unwrap(),
        vec![Some(contract::ThinkingLevel::High)],
        "the folded thinking seeds `chosen`"
    );
    assert_eq!(
        kinds_of(&history.lines()),
        vec![
            "session_started",
            "preamble_built",
            "model_changed",
            "model_changed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

/// A `preamble_built` with `credential` and `lifetime`: `preamble` builds
/// the one-hour one.
fn preamble_with(credential: Option<&str>, lifetime: contract::events::CacheLifetime) -> Event {
    Event::PreambleBuilt(contract::events::PreambleBuilt {
        reason: contract::events::PreambleReason::Start,
        model: MODEL.into(),
        context_window: 0,
        trigger_at: None,
        budget: None,
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: lifetime,
        credential: credential.map(str::to_owned),
        system_prompt: String::new(),
        tools: Vec::new(),
        replaced: Vec::new(),
    })
}

#[test]
fn a_resume_switching_the_label_records_model_changed_before_the_build() {
    use contract::events::CacheLifetime;
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(
        preamble_with(Some("work"), CacheLifetime::FiveMinutes),
        None,
    );
    history.freeze();

    let looped = history.resume_on(Vec::new(), Some("home"), CacheLifetime::OneHour);
    let outcome = history.run(looped, "hi");
    assert_eq!(outcome, contract::events::TurnOutcome::Completed);

    // The recorded settings are `before`, exactly as the log last had
    // them; the session's current ones are `after`
    // (`docs/events.md`, "`model_changed`").
    let new = history.new_lines();
    assert_eq!(new[0].kind, "model_changed");
    assert_eq!(
        new[0].payload["before"],
        serde_json::json!({
            "model": MODEL,
            "cache_lifetime": "5m",
            "credential": "work",
        })
    );
    assert_eq!(
        new[0].payload["after"],
        serde_json::json!({
            "model": MODEL,
            "cache_lifetime": "1h",
            "credential": "home",
        })
    );
    assert_eq!(new[0].payload["source"], "driver");
    assert_eq!(new[1].kind, "preamble_built");
    assert_eq!(new[1].payload["reason"], "resume");
    assert_eq!(new[1].payload["credential"], "home");
    assert_eq!(
        history.new_kinds(),
        [
            "model_changed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_resume_after_a_switch_records_the_switch_after_as_before() {
    use contract::events::CacheLifetime;
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(preamble(Some("work")), None);
    history.write(switched("fake/second", None, Some("personal")), None);
    history.freeze();

    let looped = history.resume_on(Vec::new(), Some("home"), CacheLifetime::OneHour);
    history.run(looped, "hi");

    let new = history.new_lines();
    assert_eq!(new[0].kind, "model_changed");
    assert_eq!(new[0].payload["before"]["model"], "fake/second");
    assert_eq!(new[0].payload["before"]["credential"], "personal");
    assert_eq!(new[0].payload["after"]["credential"], "home");
    assert_eq!(
        history.new_kinds(),
        [
            "model_changed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_resume_with_the_recorded_label_records_nothing() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(preamble(Some("work")), None);
    history.freeze();

    let looped = history.resume_on(
        Vec::new(),
        Some("work"),
        contract::events::CacheLifetime::OneHour,
    );
    history.run(looped, "hi");

    let new = history.new_lines();
    assert_eq!(new[0].kind, "preamble_built");
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_resume_with_a_label_but_no_recorded_one_records_nothing() {
    // A valid `preamble_built` without the `credential` field folds to
    // settings with `credential: None`: no label to switch from.
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(preamble(None), None);
    history.freeze();

    let looped = history.resume_on(
        Vec::new(),
        Some("home"),
        contract::events::CacheLifetime::OneHour,
    );
    history.run(looped, "hi");

    let new = history.new_lines();
    assert_eq!(new[0].kind, "preamble_built");
    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_resume_with_a_label_but_no_recorded_settings_records_nothing() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.freeze();

    let looped = history.resume_on(
        Vec::new(),
        Some("home"),
        contract::events::CacheLifetime::OneHour,
    );
    history.run(looped, "hi");

    assert_eq!(
        history.new_kinds(),
        [
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_resume_switching_the_label_marks_orphans_first() {
    let mut history = History::new(vec![Scripted::text("Hello.")]);
    history.write(preamble(Some("work")), None);
    history.write(job_started("j_a"), Some("a_1"));
    history.freeze();

    let looped = history.resume_on(
        Vec::new(),
        Some("home"),
        contract::events::CacheLifetime::OneHour,
    );
    history.run(looped, "hi");

    let new = history.new_lines();
    assert_eq!(new[0].kind, "job_completed");
    assert_orphaned(&new[0], "j_a");
    assert_eq!(new[1].kind, "model_changed");
    assert_eq!(
        history.new_kinds(),
        [
            "job_completed",
            "model_changed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}
