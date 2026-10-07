//! `fiber_exited` declines an extension's open asks (`docs/events.md`,
//! `interaction_resolved`): a question a callback raised belongs to this
//! process's VM, so resuming never raises it again. Approvals and questions
//! without an extension stay pending instead.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::PathBuf;

use contract::events::{
    Answer, AskStep, Event, Interaction, InteractionRequested, InteractionResolved,
    PermissionRequested, ResolvedBy, RuleScope, StandingRule,
};
use contract::shapes::{DeclaredEffects, Effect};
use contract::{ActionId, RequestId, SessionId, TurnId};
use serde_json::Value;

use super::{fiber_exited, fiber_started};

/// A session log in a temporary directory, removed on drop.
struct Session {
    _root: fakes::TempDir,
    dir: PathBuf,
    log: log::Log,
}

impl Session {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-process-asks");
        let log = log::Log::create(
            root.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        let dir = root.path().join("s_1");
        Self {
            dir,
            _root: root,
            log,
        }
    }

    fn append(&self, event: &Event) {
        self.log
            .append(event, Some(TurnId("t_1".into())), None::<ActionId>)
            .unwrap();
    }

    /// Writes `fiber_exited` under `signal` and returns its exit code and payload.
    fn exit_on(&self, signal: Option<i32>) -> (i32, Value) {
        let exited = fiber_exited(&self.log, &self.dir, Ok(()), false, signal).unwrap();
        let lines = log::read(&self.dir).unwrap();
        let last = lines.last().unwrap();
        assert_eq!(last.kind, "fiber_exited");
        (exited.code, Value::Object(last.payload.clone()))
    }

    fn kinds(&self) -> Vec<String> {
        log::read(&self.dir)
            .unwrap()
            .into_iter()
            .map(|line| line.kind)
            .collect()
    }

    fn resolved(&self) -> Vec<contract::Envelope> {
        log::read(&self.dir)
            .unwrap()
            .into_iter()
            .filter(|line| line.kind == "interaction_resolved")
            .collect()
    }
}

fn permission(id: &str) -> Event {
    Event::PermissionRequested(PermissionRequested {
        request_id: RequestId(id.into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
        },
        step: AskStep::StandingAsk {
            standing_rule: StandingRule {
                scope: RuleScope::Project,
                prefix: "npm".into(),
            },
        },
    })
}

fn asked(id: &str) -> Event {
    Event::InteractionRequested(InteractionRequested {
        request_id: RequestId(id.into()),
        interaction: Interaction::Confirm {
            prompt: "go?".into(),
        },
        action_ids: None,
        extension: Some("fiber.test/notes".into()),
    })
}

fn asked_without_extension(id: &str) -> Event {
    Event::InteractionRequested(InteractionRequested {
        request_id: RequestId(id.into()),
        interaction: Interaction::Confirm {
            prompt: "go?".into(),
        },
        action_ids: None,
        extension: None,
    })
}

fn answered_by_person(id: &str) -> Event {
    Event::InteractionResolved(InteractionResolved {
        request_id: RequestId(id.into()),
        by: ResolvedBy::Person,
        answer: Answer::Confirmed { confirmed: true },
    })
}

#[test]
fn an_open_extension_ask_is_declined_at_exit() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&asked("r_1"));
    let (code, exited) = session.exit_on(None);
    assert_eq!(code, 0);
    let resolved = session.resolved();
    assert_eq!(resolved.len(), 1, "exactly one resolution is written");
    assert_eq!(resolved[0].payload["request_id"], "r_1");
    assert_eq!(resolved[0].payload["by"], "fiber");
    assert_eq!(resolved[0].payload["declined"], true);
    assert!(
        exited.get("suspended_on").is_none(),
        "no status waits on a question no callback holds"
    );
    assert_eq!(
        session.kinds(),
        [
            "fiber_started",
            "interaction_requested",
            "interaction_resolved",
            "fiber_exited",
        ]
    );
}

#[test]
fn a_resolved_extension_ask_is_left_alone() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&asked("r_1"));
    session.append(&answered_by_person("r_1"));
    let (code, exited) = session.exit_on(None);
    assert_eq!(code, 0);
    let resolved = session.resolved();
    assert_eq!(resolved.len(), 1, "no second resolution is written");
    assert_eq!(resolved[0].payload["by"], "person");
    assert!(exited.get("suspended_on").is_none());
}

#[test]
fn a_question_without_an_extension_is_still_suspended() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&asked_without_extension("r_1"));
    let (code, exited) = session.exit_on(None);
    assert_eq!(code, 0);
    assert!(
        session.resolved().is_empty(),
        "a question without an extension is never declined"
    );
    assert_eq!(exited["suspended_on"], "r_1");
}

#[test]
fn an_ask_an_earlier_process_left_open_is_declined() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&asked("r_old"));
    // No `fiber_exited`: the earlier process crashed.
    fiber_started(&session.log, "1.2.3", true).unwrap();
    let (code, _) = session.exit_on(None);
    assert_eq!(code, 0);
    let resolved = session.resolved();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["request_id"], "r_old");
    assert_eq!(resolved[0].payload["by"], "fiber");
}

#[test]
fn a_resolved_ask_from_an_earlier_process_is_not_declined_again() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&asked("r_old"));
    session.append(&answered_by_person("r_old"));
    fiber_started(&session.log, "1.2.3", true).unwrap();
    let (code, _) = session.exit_on(None);
    assert_eq!(code, 0);
    assert_eq!(session.resolved().len(), 1, "the earlier resolution stands");
}

#[test]
fn a_signal_still_declines_an_open_extension_ask() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&asked("r_1"));
    let (code, _) = session.exit_on(Some(130));
    assert_eq!(code, 130);
    let resolved = session.resolved();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["by"], "fiber");
    assert_eq!(resolved[0].payload["declined"], true);
}

#[test]
fn two_open_extension_asks_are_declined_in_log_order() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&asked("r_1"));
    session.append(&asked("r_2"));
    let (code, exited) = session.exit_on(None);
    assert_eq!(code, 0);
    let resolved = session.resolved();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].payload["request_id"], "r_1");
    assert_eq!(resolved[1].payload["request_id"], "r_2");
    assert!(exited.get("suspended_on").is_none());
    assert_eq!(
        session.kinds(),
        [
            "fiber_started",
            "interaction_requested",
            "interaction_requested",
            "interaction_resolved",
            "interaction_resolved",
            "fiber_exited",
        ]
    );
}

#[test]
fn an_open_extension_ask_beside_an_open_approval_suspends_on_the_approval() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&asked("r_ask"));
    session.append(&permission("r_approval"));
    let (code, exited) = session.exit_on(None);
    assert_eq!(code, 0);
    assert_eq!(session.resolved().len(), 1, "the ask is declined");
    assert_eq!(exited["suspended_on"], "r_approval");
}

fn offered(id: &str) -> Event {
    Event::RepositoryCodeOffered(contract::events::RepositoryCodeOffered {
        request_id: RequestId(id.into()),
        items: Vec::new(),
    })
}

fn offer_resolved(id: &str) -> Event {
    Event::RepositoryCodeResolved(contract::events::RepositoryCodeResolved {
        request_id: RequestId(id.into()),
        decisions: Vec::new(),
    })
}

fn preamble() -> Event {
    Event::PreambleBuilt(
        serde_json::from_value(serde_json::json!({
            "reason": "start", "model": "fake/m", "context_window": 1000,
            "tool_choice": "auto", "cache_lifetime": "5m", "system_prompt": "", "tools": []
        }))
        .unwrap(),
    )
}

#[test]
fn a_process_that_exits_on_its_offer_names_it() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&offered("r_o"));
    let (_, exited) = session.exit_on(None);
    assert_eq!(exited["suspended_on"], "r_o");
}

#[test]
fn a_resolved_offer_is_not_suspended_on() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&offered("r_o"));
    session.append(&offer_resolved("r_o"));
    let (_, exited) = session.exit_on(None);
    assert!(exited.get("suspended_on").is_none());
}

#[test]
fn an_offer_left_behind_at_the_preamble_is_not_suspended_on() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&offered("r_o"));
    session.append(&preamble());
    let (_, exited) = session.exit_on(None);
    assert!(exited.get("suspended_on").is_none());
}

#[test]
fn an_approval_beside_an_offer_is_suspended_on() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&offered("r_o"));
    session.append(&permission("r_a"));
    let (_, exited) = session.exit_on(None);
    assert_eq!(exited["suspended_on"], "r_a");
}

#[test]
fn under_a_signal_an_earlier_approval_beats_this_process_offer() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&permission("r_a"));
    fiber_started(&session.log, "1.2.3", true).unwrap();
    session.append(&offered("r_o"));
    let (_, exited) = session.exit_on(Some(143));
    assert_eq!(exited["suspended_on"], "r_a");
}

#[test]
fn an_offer_an_earlier_process_left_is_not_suspended_on() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&offered("r_o"));
    fiber_started(&session.log, "1.2.3", true).unwrap();
    let (_, exited) = session.exit_on(None);
    assert!(exited.get("suspended_on").is_none());
}
