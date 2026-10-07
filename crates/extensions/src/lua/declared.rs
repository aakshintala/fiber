//! What the entry script registered and each callback's timeout.

use std::collections::BTreeMap;
use std::time::Duration;

use mlua::Table;

use super::Target;

/// One command the entry script registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclaredCommand {
    /// Its timeout.
    pub(super) timeout: Duration,
    /// Its one-line description, `""` when undeclared.
    pub(super) description: String,
}

/// What the entry script registered: commands, provider functions and hooks,
/// in separate maps so a shared name cannot collide.
#[derive(Default)]
pub(super) struct CallbackTimeouts {
    pub(super) commands: BTreeMap<String, DeclaredCommand>,
    pub(super) providers: BTreeMap<String, BTreeMap<String, Duration>>,
    pub(super) hooks: DeclaredHooks,
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
            // A timer firing carries its own timeout from its firing's
            // start; it is never a queued call the timeout is looked up
            // for.
            Target::Timer { .. } => None,
        }
    }

    /// Every hook runs under `timeout` instead of the one it declared.
    pub(super) fn override_hooks(&mut self, timeout: Option<Duration>) {
        let Some(timeout) = timeout else {
            return;
        };
        for hook in self.hooks.by_point.values_mut().flatten() {
            hook.timeout = timeout;
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
                .map(|spec| DeclaredHook {
                    phase: match spec.get::<String>("phase").as_deref() {
                        Ok("sanitize") => HookPhase::Sanitize,
                        Ok("check") => HookPhase::Check,
                        _ => HookPhase::Transform,
                    },
                    blocking: spec
                        .get::<String>("on_failure")
                        .is_ok_and(|failure| failure == "blocking"),
                    timeout: Duration::from_millis(spec.get::<u64>("timeout").unwrap_or(0)),
                })
                .collect();
            declared.by_point.insert(point, list);
        }
        declared.problems = problems.sequence_values::<String>().flatten().collect();
        declared
    }
}
