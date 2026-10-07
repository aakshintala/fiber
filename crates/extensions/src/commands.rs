//! A session's extension commands (`docs/extensions.md`, "Commands and screens"):
//! final names after renames, conflicts, the `replaces` check, the `commands`
//! list and admission onto each extension's ordered stream.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use config::Config;
use contract::ErrorCode;
use contract::events::{CommandInfo, Notice};
use contract::inbox::Rejection;

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
}

/// The session's extension commands, built once from the same final names
/// for the `commands` list and the admission table.
#[derive(Default)]
pub(crate) struct SessionCommands {
    admitted: BTreeMap<String, Admitted>,
    notices: Vec<Notice>,
    /// Extensions unloaded by the `replaces` check, by name.
    unloaded: BTreeSet<String>,
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
                },
            );
        }
        Self {
            admitted,
            notices,
            unloaded,
        }
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
    /// final name, the description, no `argument_hint`, and the extension's
    /// name as the tag.
    pub(crate) fn list(&self) -> Vec<CommandInfo> {
        // `admitted` is a `BTreeMap`, so iteration is already by name.
        self.admitted
            .iter()
            .map(|(name, cmd)| CommandInfo {
                name: name.clone(),
                description: cmd.description.clone(),
                argument_hint: None,
                tag: cmd.extension.clone(),
            })
            .collect()
    }

    /// Admits `name` with `text`. The queued call's text is `text`.
    pub(crate) fn admit_with(&self, name: &str, text: &str) -> Result<AdmittedCall, Rejection> {
        let cmd = self.admitted.get(name).ok_or_else(|| Rejection {
            code: ErrorCode::UnknownCommand,
            message: format!("`{name}` names no extension command."),
        })?;
        let queued = cmd
            .lua
            .queue_command(&cmd.registered, text)
            .map_err(|_| Rejection {
                code: ErrorCode::UnknownCommand,
                message: format!("`{name}` names no extension command."),
            })?;
        Ok(AdmittedCall {
            extension: cmd.extension.clone(),
            lua: Arc::clone(&cmd.lua),
            queued: std::sync::Arc::new(queued),
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
