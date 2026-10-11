//! Builders and log fixtures the loop's test binaries share with the
//! crate's own tests: every helper is defined once here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code,
    missing_docs,
    reason = "test code, helpers included; each test target uses some helpers"
)]

use contract::events::{
    AskStep, CallStatus, Empty, Environment, Event, FiberStarted, InputItem, OpeningMessage,
    PermissionRequested, RuleScope, StandingRule, SteeringApplied, TextCompleted,
    ToolCallCompleted, ToolCallRequested, ToolCallStarted, TurnStarted,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender};
use contract::{ActionId, CommandId, Envelope, RequestId};
use serde_json::json;

/// The turn-starting event of a driver turn saying `text`.
pub(crate) fn user_turn(text: &str) -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text { text: text.into() }],
            sender: contract::shapes::Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
            changed_by: None,
        }],
    })
}

/// The request event of a call to `name` with `{"city": city}` arguments.
pub(crate) fn requested(name: &str, city: &str) -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: name.into(),
        arguments: json!({"city": city}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

/// The start event of a call that declares no effects.
pub(crate) fn started() -> Event {
    Event::ToolCallStarted(ToolCallStarted {
        declared: DeclaredEffects {
            effects: Vec::new(),
            reversible: true,
            paths: None,
        },
        arguments: None,
        changed_by: None,
    })
}

/// The completion event of a call that returned `text`. Named apart from
/// the `tool_call_completed` line filter because it builds an event.
pub(crate) fn completed_event(text: &str) -> Event {
    Event::ToolCallCompleted(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text { text: text.into() }],
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
    })
}

/// A standing-ask approval carrying `request_id`.
pub(crate) fn standing_request(request_id: &str) -> Event {
    Event::PermissionRequested(PermissionRequested {
        request_id: RequestId(request_id.into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: true,
            paths: None,
        },
        step: AskStep::StandingAsk {
            standing_rule: StandingRule {
                scope: RuleScope::Project,
                prefix: "run tests".into(),
            },
        },
    })
}

pub(crate) fn assistant(text: &str) -> Event {
    Event::TextCompleted(TextCompleted {
        text: text.into(),
        provider_item: None,
    })
}

pub(crate) fn message_started() -> Event {
    Event::AssistantMessageStarted(Empty {})
}

pub(crate) fn kinds_of(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|l| l.kind.as_str()).collect()
}

pub(crate) fn steering(text: &str) -> Event {
    Event::SteeringApplied(SteeringApplied {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Driver,
            command_id: Some(CommandId("c_2".into())),
        },
        changed_by: None,
    })
}

pub(crate) fn result_of(input: &Input) -> (&ActionId, &str, bool) {
    let Input::ToolResult {
        action_id,
        text,
        is_error,
        ..
    } = input
    else {
        panic!("expected a tool result, got {input:?}");
    };
    (action_id, text, *is_error)
}

pub(crate) fn fiber_started() -> Event {
    Event::FiberStarted(FiberStarted {
        version: "0.0.0".into(),
        resumed: false,
    })
}

pub(crate) fn job_started(id: &str) -> Event {
    Event::JobStarted(contract::events::JobStarted {
        job_id: contract::JobId(id.into()),
        tool: Some("shell".into()),
        extension: None,
        description: "npm test".into(),
        output_path: format!("artifacts/{id}.log"),
    })
}

pub(crate) fn job_completed(id: &str) -> Event {
    Event::JobCompleted(contract::events::JobCompleted {
        job_id: contract::JobId(id.into()),
        status: contract::events::Outcome::Completed,
        error: None,
        process: None,
        output_tail: None,
    })
}

pub(crate) fn opening_of(os: &str) -> Event {
    Event::OpeningMessage(OpeningMessage {
        environment: Environment {
            date: "2023-11-14".into(),
            os: os.into(),
            arch: "test-arch".into(),
            shell: "/bin/sh".into(),
            workspace: "/w".into(),
            git: None,
            session_log: "/log/events.jsonl".into(),
        },
        instruction_files: Vec::new(),
        extension_sections: Vec::new(),
        skills: Vec::new(),
    })
}

pub(crate) fn handoff_started() -> Event {
    Event::HandoffStarted(contract::events::HandoffStarted {
        trigger: contract::events::HandoffTrigger::Auto,
    })
}

pub(crate) fn handoff_done(outcome: contract::events::Outcome, note: &[&str]) -> Event {
    let failed = outcome == contract::events::Outcome::Failed;
    Event::HandoffCompleted(contract::events::HandoffCompleted {
        outcome,
        error: failed.then(|| contract::shapes::Failure {
            code: contract::ErrorCode::RateLimited,
            message: "slow down".into(),
            retry_after_ms: None,
            provider: None,
        }),
        note: (!note.is_empty()).then(|| contract::events::Note::Actions {
            note: note.iter().map(|id| ActionId((*id).into())).collect(),
        }),
        tokens_before: 400_120,
        instructions: None,
    })
}

pub(crate) fn text_of(input: &Input) -> &str {
    match input {
        Input::User { text, .. } | Input::Assistant { text, .. } => text,
        other @ (Input::Reasoning { .. } | Input::ToolCall { .. } | Input::ToolResult { .. }) => {
            panic!("not a message: {other:?}")
        }
    }
}

pub(crate) fn texts(conversation: &[Input]) -> Vec<&str> {
    conversation.iter().map(text_of).collect()
}

pub(crate) fn hosted_requested() -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: "web_search".into(),
        arguments: json!({"query": "rust 1.90"}),
        provider_id: Some(contract::ProviderCallId("srvtoolu_01".into())),
        repair: None,
        ran_by: None,
        provider_item: Some(json!({"type": "server_tool_use", "id": "srvtoolu_01"})),
    })
}
