//! What the entry script registered and each callback's timeout.

use std::collections::BTreeMap;
use std::time::Duration;

use contract::events::Event;
use contract::{Envelope, SCHEMA_VERSION, SessionId};
use mlua::Table;
use serde_json::Map;

use super::{DeclaredTool, Target};

/// One command the entry script registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclaredCommand {
    /// Its timeout.
    pub(super) timeout: Duration,
    /// Its one-line description, `""` when undeclared.
    pub(super) description: String,
}

/// What the entry script registered: commands, provider functions, hooks,
/// watchers, tools and search backends, in separate maps so a shared name cannot collide.
#[derive(Default)]
pub(super) struct CallbackTimeouts {
    pub(super) commands: BTreeMap<String, DeclaredCommand>,
    pub(super) providers: BTreeMap<String, BTreeMap<String, Duration>>,
    pub(super) hooks: DeclaredHooks,
    pub(super) watchers: DeclaredWatchers,
    pub(super) tools: BTreeMap<String, DeclaredTool>,
    pub(super) search: BTreeMap<String, Duration>,
}

impl CallbackTimeouts {
    /// The timeout `target` declared, if the entry script registered it.
    pub(super) fn timeout(&self, target: &Target) -> Option<Duration> {
        match target {
            Target::Command(name) => self.commands.get(name).map(|c| c.timeout),
            Target::Provider { name, function, .. } => self
                .providers
                .get(name)
                .and_then(|fns| fns.get(*function))
                .copied(),
            Target::Hook { point, index } => self
                .hooks
                .by_point
                .get(point)
                .and_then(|hooks| hooks.get(*index))
                .map(|hook| hook.timeout),
            Target::Watcher { index } => self
                .watchers
                .watchers
                .get(*index)
                .map(|watcher| watcher.timeout),
            // A timer firing carries its own timeout from its firing's
            // start; it is never a queued call the timeout is looked up
            // for.
            Target::Timer { .. } => None,
            // A tool's effects function runs under the tool's own timeout.
            Target::Tool(name) | Target::Effects(name) => {
                self.tools.get(name).map(|tool| tool.timeout)
            }
            // A search backend's `run` runs under the backend's timeout.
            Target::Search(name) => self.search.get(name).copied(),
            // A delegate `run` runs under the per-call timeout the
            // `host.delegate` yield carried, judged from the push instant.
            Target::Delegate { timeout, .. } => Some(*timeout),
        }
    }

    /// Every hook and watcher runs under `timeout` instead of the one it
    /// declared (`docs/configuration.md`,
    /// `extensions."<name>".hook_timeout_ms`).
    pub(super) fn override_hooks(&mut self, timeout: Option<Duration>) {
        let Some(timeout) = timeout else {
            return;
        };
        for hook in self.hooks.by_point.values_mut().flatten() {
            hook.timeout = timeout;
        }
        for watcher in &mut self.watchers.watchers {
            watcher.timeout = timeout;
        }
    }
}

/// A hook's phase (`docs/extensions.md`, "When several hooks share a
/// point"), in the order the phases run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HookPhase {
    Sanitize,
    Transform,
    Check,
}

/// One hook the entry script registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredHook {
    pub(crate) phase: HookPhase,
    /// `on_failure` is `blocking`.
    pub(crate) blocking: bool,
    /// Its timeout, or the configured override.
    pub(crate) timeout: Duration,
    /// The declared configuration key, only for a declared entry through
    /// the first-party hooks configuration registrar.
    pub(crate) entry: Option<String>,
    /// Whether the registration named its phase.
    pub(crate) phase_explicit: bool,
}

/// The hooks the entry script registered, by point in registration order,
/// and a message for each it tried to register and could not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DeclaredHooks {
    pub(crate) by_point: BTreeMap<String, Vec<DeclaredHook>>,
    pub(crate) problems: Vec<String>,
}

impl DeclaredHooks {
    /// The hooks in `hooks`, the table `fiber.hook` fills, and the
    /// refusals in `problems`. The prelude checked each field.
    pub(super) fn read(hooks: &Table, problems: &Table) -> Self {
        let mut declared = Self::default();
        for (point, list) in hooks.pairs::<String, Table>().flatten() {
            let list = list
                .sequence_values::<Table>()
                .flatten()
                .map(|spec| {
                    let (phase, phase_explicit) = match spec.get::<String>("phase").as_deref() {
                        Ok("sanitize") => (HookPhase::Sanitize, true),
                        Ok("check") => (HookPhase::Check, true),
                        Ok("transform") => (HookPhase::Transform, true),
                        _ => (HookPhase::Transform, false),
                    };
                    DeclaredHook {
                        phase,
                        blocking: spec
                            .get::<String>("on_failure")
                            .is_ok_and(|failure| failure == "blocking"),
                        timeout: Duration::from_millis(spec.get::<u64>("timeout").unwrap_or(0)),
                        entry: spec.get::<String>("entry").ok(),
                        phase_explicit,
                    }
                })
                .collect();
            declared.by_point.insert(point, list);
        }
        declared.problems = problems.sequence_values::<String>().flatten().collect();
        declared
    }
}

/// One watcher the entry script registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredWatcher {
    /// The event kinds it wants, each one a known event kind.
    pub(crate) kinds: Vec<String>,
    /// Its timeout.
    pub(crate) timeout: Duration,
    /// The declared configuration key, only for a declared entry through
    /// the first-party hooks configuration registrar.
    pub(crate) entry: Option<String>,
}

/// The watchers the entry script registered, in registration order, and a
/// message for each one it tried to register and could not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DeclaredWatchers {
    pub(crate) watchers: Vec<DeclaredWatcher>,
    pub(crate) problems: Vec<String>,
}

/// Whether `problem` is a watch refusal: it starts with the watcher's
/// `` `watcher:<index>` `` name, which no hook, command, tool or search
/// refusal can carry (a command name never holds `:`).
pub(super) fn is_watch_problem(problem: &str) -> bool {
    problem.starts_with("`watcher:") && problem.contains("watcher not registered:")
}

impl DeclaredWatchers {
    /// The watchers in `watchers`, the table `fiber.watch` fills, with
    /// their kinds checked against the event kinds `Event::kind` names.
    /// A kind that matches no event kind leaves a problem and is dropped
    /// from its entry; an entry with no surviving kind is not registered.
    /// The refusals `fiber.watch` recorded in `problems` move here, so the
    /// hook path never reports them twice: a watch problem starts with
    /// the watcher's `` `watcher:<index>` `` name, which no hook, command,
    /// tool or search refusal can carry (a command name never holds `:`).
    pub(super) fn read(watchers: &Table, problems: &Table) -> Self {
        let mut declared = Self::default();
        for (index, spec) in watchers.sequence_values::<Table>().flatten().enumerate() {
            let timeout = Duration::from_millis(spec.get::<u64>("timeout").unwrap_or(0));
            let mut kinds = Vec::new();
            for kind in spec.get::<Vec<String>>("kinds").unwrap_or_default() {
                if is_known_kind(&kind) {
                    kinds.push(kind);
                } else {
                    declared.problems.push(format!(
                        "`watcher:{index}` watcher not registered: `{kind}` is not an event kind"
                    ));
                }
            }
            if kinds.is_empty() {
                continue;
            }
            declared.watchers.push(DeclaredWatcher {
                kinds,
                timeout,
                entry: spec.get::<String>("entry").ok(),
            });
        }
        for problem in problems.sequence_values::<String>().flatten() {
            if is_watch_problem(&problem) {
                declared.problems.push(problem);
            }
        }
        declared
    }
}

/// Whether `kind` names an event: a line of that kind with an empty
/// payload reads as a known event (or fails its payload), while an
/// unknown kind reads as `None`. Probing the real `Event` keeps the
/// check exact when a kind is added, with no second list to drift.
pub(crate) fn is_known_kind(kind: &str) -> bool {
    let line = Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(String::new()),
        ts: 0,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: Map::new(),
    };
    !matches!(Event::from_envelope(&line), Ok(None))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::{
        CallbackTimeouts, DeclaredHook, DeclaredHooks, DeclaredWatcher, DeclaredWatchers,
        HookPhase, is_watch_problem,
    };

    /// The configured timeout replaces hook and watcher timeouts alike,
    /// and no timeout leaves both declared values alone.
    /// Dropping the watcher loop would leave the watcher at its declared
    /// timeout; dropping the `None` early return would zero both.
    #[test]
    fn override_hooks_replaces_hook_and_watcher_timeouts() {
        fn declared() -> CallbackTimeouts {
            CallbackTimeouts {
                hooks: DeclaredHooks {
                    by_point: BTreeMap::from([(
                        "after_tool".to_owned(),
                        vec![DeclaredHook {
                            phase: HookPhase::Transform,
                            blocking: false,
                            timeout: Duration::from_millis(1000),
                            entry: None,
                            phase_explicit: false,
                        }],
                    )]),
                    problems: Vec::new(),
                },
                watchers: DeclaredWatchers {
                    watchers: vec![DeclaredWatcher {
                        kinds: vec!["turn_completed".to_owned()],
                        timeout: Duration::from_millis(2000),
                        entry: None,
                    }],
                    problems: Vec::new(),
                },
                ..Default::default()
            }
        }
        let mut timeouts = declared();
        timeouts.override_hooks(Some(Duration::from_millis(5)));
        assert_eq!(
            timeouts.hooks.by_point["after_tool"][0].timeout,
            Duration::from_millis(5)
        );
        assert_eq!(
            timeouts.watchers.watchers[0].timeout,
            Duration::from_millis(5)
        );
        let mut timeouts = declared();
        timeouts.override_hooks(None);
        assert_eq!(
            timeouts.hooks.by_point["after_tool"][0].timeout,
            Duration::from_millis(1000)
        );
        assert_eq!(
            timeouts.watchers.watchers[0].timeout,
            Duration::from_millis(2000)
        );
    }

    /// The conjunction boundary: each half alone is not a watch problem.
    /// Flipping `&&` to `||` would admit either half alone.
    #[test]
    fn is_watch_problem_needs_both_halves() {
        assert!(is_watch_problem(
            "`watcher:0` watcher not registered: missing `timeout`"
        ));
        assert!(!is_watch_problem("`watcher:0` timed out after 300 ms"));
        assert!(!is_watch_problem(
            "`hook` watcher not registered: missing `timeout`"
        ));
    }
}
