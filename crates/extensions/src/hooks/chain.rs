//! A session's hook chains, in run order (`docs/extensions.md`, "When
//! several hooks share a point").

use std::collections::BTreeMap;
use std::sync::Arc;

use config::Config;
use contract::CommandId;

use crate::lua::{DeclaredHooks, HookPhase, LuaExtension};
use crate::runtime::{FunctionRef, FunctionTarget, LuaBinding, LuaKind, Runtime};

use super::SessionExtensions;

/// The `session_start` hook point.
pub(super) const SESSION_START: &str = "session_start";

/// The `turn_start` hook point.
pub(super) const TURN_START: &str = "turn_start";

/// The `before_model_call` hook point.
pub(super) const BEFORE_MODEL_CALL: &str = "before_model_call";

/// The `turn_end` hook point.
pub(super) const TURN_END: &str = "turn_end";

/// The `before_tool` hook point.
pub(super) const BEFORE_TOOL: &str = "before_tool";

/// The `before_message` hook point.
pub(super) const BEFORE_MESSAGE: &str = "before_message";

/// The `before_handoff` hook point.
pub(super) const BEFORE_HANDOFF: &str = "before_handoff";

/// One hook in a chain: its function behind the session's runtime, so the
/// dispatch names no backend.
pub(super) struct Entry {
    /// Its function behind the session's runtime.
    pub(super) hook: FunctionRef,
    /// Whether its failure stops what it guards.
    pub(super) blocking: bool,
    /// Whether it is a `check` hook: it refuses or lets it pass, without
    /// changing it.
    pub(super) check: bool,
}

/// Each point's hooks across extensions, in load order, before they run.
#[derive(Default)]
pub(super) struct Builder {
    entries: BTreeMap<&'static str, Vec<(HookPhase, String, Entry)>>,
}

/// The `session_start`, `turn_start`, `before_model_call`, `turn_end` and
/// `before_handoff` chains, in run order.
#[derive(Default)]
pub(super) struct Lifecycle {
    /// The `session_start` chain, in run order.
    pub(super) session_start: Vec<Entry>,
    /// The `turn_start` chain, in run order.
    pub(super) turn_start: Vec<Entry>,
    /// The `before_model_call` chain, in run order.
    pub(super) before_model_call: Vec<Entry>,
    /// The `turn_end` chain, in run order.
    pub(super) turn_end: Vec<Entry>,
    /// The `before_handoff` chain, in run order.
    pub(super) before_handoff: Vec<Entry>,
}

impl Builder {
    /// Pushes `declared`'s hooks at the three points onto the builder, in
    /// registration order, registering each behind the session's runtime:
    /// the entry carries the reference, never the backend.
    pub(super) fn push(
        &mut self,
        runtime: &Runtime,
        name: &str,
        extension: &Arc<LuaExtension>,
        declared: &DeclaredHooks,
    ) {
        for point in [
            SESSION_START,
            TURN_START,
            BEFORE_MODEL_CALL,
            TURN_END,
            BEFORE_HANDOFF,
            BEFORE_TOOL,
            BEFORE_MESSAGE,
            super::AFTER_TOOL,
        ] {
            for (index, hook) in declared
                .by_point
                .get(point)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let made = runtime.register_function(
                    name,
                    CommandId(format!("lua:{name}:{point}:{index}")),
                    "run".to_owned(),
                    FunctionTarget::Lua(LuaBinding {
                        extension: Arc::downgrade(extension),
                        kind: LuaKind::Hook {
                            point: point.to_owned(),
                            index,
                        },
                    }),
                );
                self.entries.entry(point).or_default().push((
                    hook.phase,
                    name.to_owned(),
                    Entry {
                        hook: made,
                        blocking: hook.blocking,
                        check: hook.phase == HookPhase::Check,
                    },
                ));
            }
        }
    }

    /// Sorts each point's hooks by phase, `hooks.order.<point>`, then name,
    /// and fills the session's chains (`docs/extensions.md`, "When several
    /// hooks share a point").
    pub(super) fn fill(self, config: &Config, session: &mut SessionExtensions) {
        let mut entries = self.entries;
        session.lifecycle.session_start =
            sorted(config, SESSION_START, entries.remove(SESSION_START));
        session.lifecycle.turn_start = sorted(config, TURN_START, entries.remove(TURN_START));
        session.lifecycle.before_model_call =
            sorted(config, BEFORE_MODEL_CALL, entries.remove(BEFORE_MODEL_CALL));
        session.lifecycle.turn_end = sorted(config, TURN_END, entries.remove(TURN_END));
        session.lifecycle.before_handoff =
            sorted(config, BEFORE_HANDOFF, entries.remove(BEFORE_HANDOFF));
        session.before_tool = sorted(config, BEFORE_TOOL, entries.remove(BEFORE_TOOL));
        session.before_message = sorted(config, BEFORE_MESSAGE, entries.remove(BEFORE_MESSAGE));
        session.after_tool = sorted(config, super::AFTER_TOOL, entries.remove(super::AFTER_TOOL));
    }
}

/// Sorts `entries` for `point`: within a phase, the extensions
/// `hooks.order` names, in its order, then the rest by name; one
/// extension's hooks in registration order.
fn sorted(
    config: &Config,
    point: &str,
    entries: Option<Vec<(HookPhase, String, Entry)>>,
) -> Vec<Entry> {
    let order: Vec<String> = config
        .get(&format!("hooks.order.{point}"), None)
        .and_then(|(value, _)| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let rank = |name: &str| order.iter().position(|named| named == name);
    // Within a phase: the extensions `hooks.order` names, in its order,
    // then the rest by name; one extension's hooks in registration order.
    let mut entries = entries.unwrap_or_default();
    // The sort is stable, so one extension's hooks keep registration order:
    // each entry carries its own reference, and no index is compared.
    entries.sort_by_key(|(phase, name, _): &(HookPhase, String, Entry)| {
        (
            *phase,
            rank(name).map_or((1, 0), |at| (0, at)),
            name.clone(),
        )
    });
    entries.into_iter().map(|(_, _, entry)| entry).collect()
}
