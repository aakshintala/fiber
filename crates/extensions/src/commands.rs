//! A session's extension commands (`docs/extensions.md`, "Commands and screens"):
//! final names after renames, conflicts, the `replaces` check, the `commands`
//! list and admission onto each extension's ordered stream.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};
use support::lock;

use config::Config;
use contract::ErrorCode;
use contract::events::{CommandInfo, Notice};
use contract::inbox::Rejection;
use contract::tool::Tool;

use crate::Error;
use crate::lua::LuaExtension;

/// One extension's registrations, before final names are settled.
pub(crate) struct CommandSource {
    /// The extension's name, as `extensions_loaded` carries it.
    pub(crate) extension: String,
    /// The manifest's `replaces`.
    pub(crate) replaces: Vec<String>,
    /// Each registered `(name, description)`.
    pub(crate) commands: Vec<(String, String)>,
    /// The started extension, for admission and failure notices.
    pub(crate) lua: Arc<LuaExtension>,
}

/// One admitted command: the final name the session lists and admits, and
/// the registered name `run` runs under.
struct Admitted {
    extension: String,
    registered: String,
    description: String,
    lua: Arc<LuaExtension>,
    /// The manifest's hint; only a lazy extension's commands have one.
    argument_hint: Option<String>,
    /// Set for a command of a lazy extension: the start its first run owes.
    lazy: Option<Arc<Lazy>>,
}

/// The session's extension commands, built once from the same final names
/// for the `commands` list and the admission table.
#[derive(Default)]
pub(crate) struct SessionCommands {
    admitted: BTreeMap<String, Admitted>,
    notices: Vec<Notice>,
    /// Extensions unloaded by the `replaces` check, by name.
    unloaded: BTreeSet<String>,
    /// The lazy extensions with an admitted command.
    lazies: Vec<Arc<Lazy>>,
}

impl SessionCommands {
    /// Settles final names, conflicts and the `replaces` check. `sources` is
    /// in load order; the list and admission table are built once here.
    pub(crate) fn build(sources: &[CommandSource], config: &Config) -> Self {
        // Proposed final names, after renames.
        struct Proposed {
            source: usize,
            registered: String,
            description: String,
            final_name: String,
            renamed: bool,
        }
        let mut proposed: Vec<Proposed> = Vec::new();
        let mut notices: Vec<Notice> = Vec::new();
        for (source, item) in sources.iter().enumerate() {
            for (registered, description) in &item.commands {
                let key = format!(
                    "extensions.\"{}\".commands.\"{}\"",
                    item.extension, registered
                );
                let rename = config
                    .get(&key, None)
                    .and_then(|(value, _)| value.as_str().map(str::to_owned));
                let (final_name, renamed) = match rename {
                    Some(next) => {
                        if is_plain(&next) {
                            (next, true)
                        } else {
                            notices.push(Notice {
                                code: ErrorCode::ExtensionFailed,
                                message: format!(
                                    "Command rename `{next}` for `{}` is not a plain name; it is ignored.",
                                    item.extension
                                ),
                                extension: Some(item.extension.clone()),
                            });
                            (registered.clone(), false)
                        }
                    }
                    None => (registered.clone(), false),
                };
                proposed.push(Proposed {
                    source,
                    registered: registered.clone(),
                    description: description.clone(),
                    final_name,
                    renamed,
                });
            }
        }
        // A rename that lands on another final name of the same extension is
        // ignored, and both keep their registered names. Reverting one
        // rename can create another collision (`a`->`b` and `b`->`c`
        // reverts `b` onto `a`'s new name), so repeat until final names are
        // unique per source.
        loop {
            let mut by_source_final: BTreeMap<(usize, String), Vec<usize>> = BTreeMap::new();
            for (idx, p) in proposed.iter().enumerate() {
                by_source_final
                    .entry((p.source, p.final_name.clone()))
                    .or_default()
                    .push(idx);
            }
            let mut revert: BTreeSet<usize> = BTreeSet::new();
            for ((source, _), idxs) in &by_source_final {
                if idxs.len() > 1 {
                    let renamed: Vec<usize> = idxs
                        .iter()
                        .copied()
                        .filter(|i| proposed.get(*i).is_some_and(|p| p.renamed))
                        .collect();
                    if !renamed.is_empty() {
                        let names: Vec<String> = idxs
                            .iter()
                            .filter_map(|i| proposed.get(*i))
                            .map(|p| format!("`{}`", p.registered))
                            .collect();
                        if let Some(item) = sources.get(*source) {
                            notices.push(Notice {
                                code: ErrorCode::ExtensionFailed,
                                message: format!(
                                    "Commands {} of `{}` rename onto one name; both keep their registered names.",
                                    names.join(" and "),
                                    item.extension
                                ),
                                extension: Some(item.extension.clone()),
                            });
                        }
                        revert.extend(renamed);
                    }
                }
            }
            if revert.is_empty() {
                break;
            }
            for idx in revert {
                if let Some(p) = proposed.get_mut(idx) {
                    p.final_name.clone_from(&p.registered);
                    p.renamed = false;
                }
            }
        }
        // The `replaces` check: a command named like a built-in without naming
        // it in its manifest's `replaces` unloads the extension.
        let mut unloaded: BTreeSet<String> = BTreeSet::new();
        for (source, item) in sources.iter().enumerate() {
            for p in proposed.iter().filter(|p| p.source == source) {
                if contract::commands::BUILT_IN_COMMANDS.contains(&p.final_name.as_str())
                    && !item.replaces.iter().any(|r| r == &p.final_name)
                {
                    unloaded.insert(item.extension.clone());
                    notices.push(Notice {
                        code: ErrorCode::ExtensionFailed,
                        message: format!(
                            "Command `{}` replaces a built-in command its manifest does not list in `replaces`; {} is not loaded.",
                            p.final_name, item.extension
                        ),
                        extension: Some(item.extension.clone()),
                    });
                    break;
                }
            }
        }
        let ext_name = |idx: usize| sources.get(idx).map(|s| s.extension.as_str());
        // Cross-extension conflicts: none of them gets the name.
        let mut by_final: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for p in &proposed {
            let Some(ext) = ext_name(p.source) else {
                continue;
            };
            if unloaded.contains(ext) {
                continue;
            }
            by_final
                .entry(p.final_name.clone())
                .or_default()
                .insert(ext.to_owned());
        }
        for (final_name, owners) in &by_final {
            if owners.len() > 1 {
                // `BTreeSet` iterates in order, so no sort.
                let named: Vec<&str> = owners.iter().map(String::as_str).collect();
                notices.push(Notice {
                    code: ErrorCode::CommandConflict,
                    message: format!(
                        "Two extensions register the command `{final_name}`: {}. Neither gets it; rename one under extensions.\"<name>\".commands.",
                        named.join(" and ")
                    ),
                    extension: None,
                });
            }
        }
        let conflicted: BTreeSet<String> = by_final
            .into_iter()
            .filter(|(_, owners)| owners.len() > 1)
            .map(|(name, _)| name)
            .collect();
        let mut admitted: BTreeMap<String, Admitted> = BTreeMap::new();
        for p in proposed {
            let Some(src) = sources.get(p.source) else {
                continue;
            };
            if unloaded.contains(&src.extension) {
                continue;
            }
            if conflicted.contains(&p.final_name) {
                continue;
            }
            // Same-extension duplicates after revert cannot happen: the revert
            // above restored registered names, which are distinct per source.
            if admitted.contains_key(&p.final_name) {
                continue;
            }
            admitted.insert(
                p.final_name,
                Admitted {
                    extension: src.extension.clone(),
                    registered: p.registered,
                    description: p.description,
                    lua: Arc::clone(&src.lua),
                    argument_hint: None,
                    lazy: None,
                },
            );
        }
        Self {
            admitted,
            notices,
            unloaded,
            lazies: Vec::new(),
        }
    }

    /// Gives each admitted command of an extension in `lazies` its start and
    /// its manifest's argument hint, and keeps the extensions that have one.
    /// Run once, before the table is shared.
    pub(crate) fn attach_lazy(&mut self, lazies: Vec<Arc<Lazy>>) {
        for cmd in self.admitted.values_mut() {
            if let Some(lazy) = lazies.iter().find(|lazy| lazy.extension == cmd.extension) {
                cmd.argument_hint = lazy.declared.get(&cmd.registered).cloned().flatten();
                cmd.lazy = Some(Arc::clone(lazy));
            }
        }
        self.lazies = lazies
            .into_iter()
            .filter(|lazy| {
                self.admitted
                    .values()
                    .any(|cmd| cmd.extension == lazy.extension)
            })
            .collect();
    }

    /// The lazy extensions with an admitted command, in load order.
    pub(crate) fn lazies(&self) -> &[Arc<Lazy>] {
        &self.lazies
    }

    /// Notices from renames, conflicts and the `replaces` check.
    pub(crate) fn notices(&self) -> Vec<Notice> {
        self.notices.clone()
    }

    /// Extensions the `replaces` check unloaded, by name.
    pub(crate) fn unloaded(&self) -> BTreeSet<String> {
        self.unloaded.clone()
    }

    /// The extension entries of the `commands` answer, sorted by name: the
    /// final name, the description, the manifest's `argument_hint` for a lazy
    /// extension's command and none for any other, and the extension's name
    /// as the tag.
    pub(crate) fn list(&self) -> Vec<CommandInfo> {
        // `admitted` is a `BTreeMap`, so iteration is already by name.
        self.admitted
            .iter()
            .map(|(name, cmd)| CommandInfo {
                name: name.clone(),
                description: cmd.description.clone(),
                argument_hint: cmd.argument_hint.clone(),
                tag: cmd.extension.clone(),
                extension: true,
            })
            .collect()
    }

    /// Admits `name` with `text`. The queued call's text is `text`.
    pub(crate) fn admit_with(
        &self,
        id: &contract::CommandId,
        name: &str,
        text: &str,
    ) -> Result<AdmittedCall, Rejection> {
        let cmd = self.admitted.get(name).ok_or_else(|| Rejection {
            code: ErrorCode::UnknownCommand,
            message: format!("`{name}` names no extension command."),
        })?;
        // A lazy extension starts first, so a failed start rejects the
        // command before it is accepted.
        if let Some(lazy) = &cmd.lazy {
            lazy.ensure(&cmd.registered)?;
        }
        let queued = cmd
            .lua
            .queue_command(id.clone(), &cmd.registered, text)
            .map_err(|e| match cmd.lazy {
                // `ensure` found the command registered, so it cannot be
                // unknown here: the extension stopped or failed.
                Some(_) => Rejection {
                    code: ErrorCode::ExtensionFailed,
                    message: format!("Command `{name}` did not run: {e}"),
                },
                None => Rejection {
                    code: ErrorCode::UnknownCommand,
                    message: format!("`{name}` names no extension command."),
                },
            })?;
        Ok(AdmittedCall {
            extension: cmd.extension.clone(),
            lua: Arc::clone(&cmd.lua),
            queued: std::sync::Arc::new(queued),
        })
    }
}

/// What a lazy extension's start registered.
struct Registered {
    /// The commands it registered, by registered name.
    commands: BTreeSet<String>,
    /// Each registration the start refused, as the hooks and watchers
    /// reads report it.
    problems: Vec<String>,
}

/// One lazy extension: the commands its manifest declares and the one start
/// its first command run owes (`docs/extensions.md`, "Commands and screens").
/// The start runs once, whoever asks first; later callers wait for it and
/// share its outcome, a failure included, until a reload builds a new one.
pub(crate) struct Lazy {
    extension: String,
    lua: Arc<LuaExtension>,
    /// Each declared command's registered name and manifest hint.
    declared: BTreeMap<String, Option<String>>,
    /// The session's list of extensions whose data a misplaced
    /// `fiber.provider` call refuses.
    rejected: Arc<Mutex<Vec<String>>>,
    started: OnceLock<Result<Registered, Rejection>>,
}

impl Lazy {
    pub(crate) fn new(
        extension: String,
        lua: Arc<LuaExtension>,
        declared: BTreeMap<String, Option<String>>,
        rejected: Arc<Mutex<Vec<String>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            extension,
            lua,
            declared,
            rejected,
            started: OnceLock::new(),
        })
    }

    /// The extension's name.
    pub(crate) fn extension(&self) -> &str {
        &self.extension
    }

    /// Whether the extension's VM has started, whatever started it: a
    /// command, a provider's first use or a restart. A start that failed
    /// counts.
    pub(crate) fn started(&self) -> bool {
        self.lua.started()
    }

    /// Starts the extension if no one has, waits for a start in flight, and
    /// returns its outcome. Registering anything but the declared commands
    /// fails it.
    pub(crate) fn start(&self) -> Result<(), Rejection> {
        self.registered().as_ref().map(|_| ()).map_err(Clone::clone)
    }

    /// Starts the extension as [`start`](Self::start) does, then checks that
    /// it registered `command`.
    pub(crate) fn ensure(&self, command: &str) -> Result<(), Rejection> {
        let reject = |code: ErrorCode, text: &str| Rejection {
            code,
            message: format!("Command `{command}` did not run: {text}"),
        };
        let ready = self
            .registered()
            .as_ref()
            .map_err(|failed| reject(failed.code.clone(), &failed.message))?;
        if ready.commands.contains(command) {
            return Ok(());
        }
        let missing = Error::Lua {
            extension: self.extension.clone(),
            message: format!(
                "declares the command `{command}`, which its entry script did not register"
            ),
        };
        let refused = format!("`{command}` command not registered");
        let why: Vec<&str> = ready
            .problems
            .iter()
            .filter(|problem| problem.starts_with(&refused))
            .map(String::as_str)
            .collect();
        let text = if why.is_empty() {
            missing.to_string()
        } else {
            format!("{missing}; {}", why.join("; "))
        };
        Err(reject(missing.code(), &text))
    }

    fn registered(&self) -> &Result<Registered, Rejection> {
        self.started.get_or_init(|| {
            self.read().map_err(|e| {
                if self.lua.misplaced_provider() {
                    lock(&self.rejected).push(self.extension.clone());
                }
                Rejection {
                    code: e.code(),
                    message: e.to_string(),
                }
            })
        })
    }

    /// Starts the VM and reads what its entry script registered. Anything
    /// but a declared command is an error; the VM is left running, for any
    /// provider it serves.
    fn read(&self) -> Result<Registered, Error> {
        let commands = self.lua.commands()?;
        let tools = self.lua.tools()?;
        let hooks = self.lua.hooks()?;
        let watchers = self.lua.watchers()?;
        let backends = self.lua.search_backends()?;
        let undeclared = |what: String| Error::Lua {
            extension: self.extension.clone(),
            message: format!(
                "registers {what}, which an extension with a `commands` key may not: it registers only the commands its manifest declares"
            ),
        };
        if let Some((name, _)) = commands
            .iter()
            .find(|(name, _)| !self.declared.contains_key(name))
        {
            return Err(undeclared(format!("the command `{name}`")));
        }
        if let Some(tool) = tools.first() {
            return Err(undeclared(format!("the tool `{}`", tool.definition().name)));
        }
        if let Some((point, _)) = hooks.by_point.iter().find(|(_, list)| !list.is_empty()) {
            return Err(undeclared(format!("a hook at `{point}`")));
        }
        if !watchers.watchers.is_empty() {
            return Err(undeclared("a watcher".to_owned()));
        }
        if let Some(backend) = backends.first() {
            return Err(undeclared(format!("the search backend `{backend}`")));
        }
        Ok(Registered {
            commands: commands.into_iter().map(|(name, _)| name).collect(),
            problems: hooks
                .problems
                .into_iter()
                .chain(watchers.problems)
                .collect(),
        })
    }
}

/// An admitted command: the held queued call, its release, and its waiter.
pub(crate) struct AdmittedCall {
    extension: String,
    lua: Arc<LuaExtension>,
    queued: std::sync::Arc<crate::lua::Queued>,
}

impl AdmittedCall {
    /// The queued call, shared with the waiter thread.
    pub(crate) fn queued(&self) -> &std::sync::Arc<crate::lua::Queued> {
        &self.queued
    }

    /// The extension's VM, for failure notices.
    pub(crate) fn lua(&self) -> &Arc<LuaExtension> {
        &self.lua
    }

    /// The extension this call runs in.
    pub(crate) fn extension(&self) -> &str {
        &self.extension
    }
}

/// Whether `name` is plain: non-empty with no whitespace, `/` or `:`.
fn is_plain(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(char::is_whitespace)
        && !name.contains('/')
        && !name.contains(':')
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
