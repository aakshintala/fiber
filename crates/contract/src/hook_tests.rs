use std::sync::Weak;

use crate::SessionId;
use crate::clock::Wake;
use crate::hook::{BeforeModelCall, SessionStart, StartReason, TurnEnd, TurnStart};
use crate::shapes::{Tokens, Usage};
use crate::tool::Cancel;

/// A cancel that never fires.
struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 1,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 1,
        },
        cost: None,
        subscription_cost: 0.0,
    }
}

#[test]
fn session_start_debug_names_the_type() {
    let id = SessionId("s_1".into());
    let start = SessionStart {
        reason: StartReason::Start,
        session_id: &id,
        workspace: "/work",
        cancel: &Never,
    };
    let shown = format!("{start:?}");
    assert!(shown.contains("SessionStart"), "{shown}");
    assert!(shown.contains("s_1"), "{shown}");
}

#[test]
fn turn_start_debug_names_the_type() {
    let turn = TurnStart {
        input: &[],
        cancel: &Never,
    };
    let shown = format!("{turn:?}");
    assert!(shown.contains("TurnStart"), "{shown}");
}

#[test]
fn before_model_call_debug_names_the_type() {
    let used = usage();
    let call = BeforeModelCall {
        model: "anthropic/claude-x",
        usage: &used,
        cancel: &Never,
    };
    let shown = format!("{call:?}");
    assert!(shown.contains("BeforeModelCall"), "{shown}");
    assert!(shown.contains("anthropic/claude-x"), "{shown}");
}

#[test]
fn turn_end_debug_names_the_type() {
    let end = TurnEnd {
        reply: "Done.",
        cancel: &Never,
    };
    let shown = format!("{end:?}");
    assert!(shown.contains("TurnEnd"), "{shown}");
    assert!(shown.contains("Done."), "{shown}");
}
