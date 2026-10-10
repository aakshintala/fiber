//! Fiber's own `web_search` over an installed search backend
//! (`docs/tools.md`, "Fiber's own, over a backend"): choosing the backend
//! the tool runs, and running it.

use std::collections::BTreeMap;
use std::sync::Arc;

use contract::ErrorCode;
use contract::events::Notice;
use contract::search::{Domains, SearchBackend, SearchResult};
use contract::shapes::Failure;
use contract::tool::Cancel;
use serde_json::{Map, Value};

use crate::Error;
use crate::lua::LuaExtension;

/// One installed search backend, running in its extension's VM.
pub(crate) struct LuaSearch {
    extension: Arc<LuaExtension>,
    name: String,
}

impl LuaSearch {
    /// The backend `name` of `extension`.
    pub(crate) fn new(extension: Arc<LuaExtension>, name: String) -> Self {
        Self { extension, name }
    }
}

impl SearchBackend for LuaSearch {
    /// Runs the backend's `run` on the query and the domain filter, in the
    /// backend's order. `Ok(None)` once `cancel` stopped the search. A
    /// backend past its timeout fails `timeout`; any other extension error
    /// fails `tool_error` with its message, so a malformed return keeps its
    /// backend, extension and position.
    fn search(
        &self,
        query: &str,
        domains: &Domains,
        cancel: &dyn Cancel,
    ) -> Result<Option<Vec<SearchResult>>, Failure> {
        let mut arg = Map::new();
        arg.insert("query".to_owned(), Value::String(query.to_owned()));
        match domains {
            Domains::Any => {}
            Domains::Allowed(domains) => {
                arg.insert(
                    "allowed_domains".to_owned(),
                    Value::Array(domains.iter().cloned().map(Value::String).collect()),
                );
            }
            Domains::Blocked(domains) => {
                arg.insert(
                    "blocked_domains".to_owned(),
                    Value::Array(domains.iter().cloned().map(Value::String).collect()),
                );
            }
        }
        let returned = self
            .extension
            .search_run(&self.name, Value::Object(arg), cancel);
        match returned {
            Ok(None) => Ok(None),
            Ok(Some(value)) => read_results(&self.name, self.extension.name(), value).map(Some),
            Err(e @ Error::Timeout { .. }) => Err(failure(ErrorCode::Timeout, e.to_string())),
            Err(e) => Err(failure(ErrorCode::ToolError, e.to_string())),
        }
    }
}

/// Why a search failed, worded for the model and the log.
fn failure(code: ErrorCode, message: String) -> Failure {
    Failure {
        code,
        message,
        retry_after_ms: None,
        provider: None,
    }
}

/// The validated list as results. The VM already checked the shape on the
/// Lua value, so this only reads the checked list into results; a row that
/// does not read is the same malformed return, worded as one.
fn read_results(
    backend: &str,
    extension: &str,
    value: Value,
) -> Result<Vec<SearchResult>, Failure> {
    let wrong = |why: String| {
        failure(
            ErrorCode::ToolError,
            format!("the search backend `{backend}` of `{extension}` returned {why}"),
        )
    };
    let Value::Array(items) = value else {
        return Err(wrong("something that is not a list of results".to_owned()));
    };
    items
        .into_iter()
        .zip(1..)
        .map(|(item, position)| {
            serde_json::from_value::<SearchResult>(item)
                .map_err(|error| wrong(format!("result {position} is not a valid result: {error}")))
        })
        .collect()
}

/// What `choose` returns: the chosen backend, if any, and the notices the
/// choice raised.
pub(crate) type Choice = (Option<Arc<dyn SearchBackend>>, Vec<Notice>);

/// What `select` returns: the chosen pair's index, if any, and the notices
/// the choice raised.
type Selection = (Option<usize>, Vec<Notice>);

/// Chooses the backend Fiber's own `web_search` runs: `backends` holds the
/// extension, the backend name and the handle in load order, and `setting`
/// is `web_search.backend`. With one backend installed, it is used. With
/// more than one, the setting names the one used. When two extensions
/// register one backend name, neither registers, as two tools of one name
/// neither gets (`docs/extensions.md`, "Registering").
pub(crate) fn choose(
    backends: Vec<(String, String, Arc<LuaExtension>)>,
    setting: Option<&str>,
) -> Choice {
    let pairs: Vec<(String, String)> = backends
        .iter()
        .map(|(extension, name, _)| (extension.clone(), name.clone()))
        .collect();
    let (chosen, notices) = select(&pairs, setting);
    let backend = chosen.and_then(|index| {
        let (_, name, handle) = backends.get(index)?;
        Some(Arc::new(LuaSearch::new(Arc::clone(handle), name.clone())) as Arc<dyn SearchBackend>)
    });
    (backend, notices)
}

/// The choice over `(extension, backend name)` pairs, in load order, so the
/// table test needs no Lua: the chosen pair's index and the notices.
fn select(backends: &[(String, String)], setting: Option<&str>) -> Selection {
    let mut notices = Vec::new();
    let mut by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (extension, name) in backends {
        by_name
            .entry(name.as_str())
            .or_default()
            .push(extension.as_str());
    }
    let mut duplicated: Vec<(&str, Vec<&str>)> = Vec::new();
    for (name, extensions) in &by_name {
        if extensions.len() > 1 {
            let mut extensions = extensions.clone();
            extensions.sort();
            duplicated.push((name, extensions));
        }
    }
    for (name, extensions) in &duplicated {
        notices.push(Notice {
            code: ErrorCode::ExtensionFailed,
            message: format!(
                "`{name}` search backend not registered: {} both register it.",
                extensions
                    .iter()
                    .map(|extension| format!("`{extension}`"))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
            extension: None,
        });
    }
    // A duplicated name counts as none installed.
    let mut candidates: Vec<(usize, &str)> = Vec::new();
    for (index, (_, name)) in backends.iter().enumerate() {
        if by_name
            .get(name.as_str())
            .is_some_and(|extensions| extensions.len() == 1)
        {
            candidates.push((index, name.as_str()));
        }
    }
    candidates.sort_by_key(|(_, name)| (*name).to_owned());
    match candidates.as_slice() {
        [] => {
            if let Some(setting) = setting {
                notices.push(missing(setting, &[]));
            }
            (None, notices)
        }
        [(index, name)] => {
            if setting.is_none_or(|setting| setting == *name) {
                (Some(*index), notices)
            } else {
                let setting = setting.unwrap_or_default();
                notices.push(missing(setting, &[*name]));
                (None, notices)
            }
        }
        _ => {
            let names: Vec<&str> = candidates.iter().map(|(_, name)| *name).collect();
            match setting {
                None => {
                    notices.push(Notice {
                        code: ErrorCode::WebSearchUnavailable,
                        message: format!(
                            "Several search backends are installed ({}): set `web_search.backend` to the one `web_search` uses.",
                            names
                                .iter()
                                .map(|name| format!("`{name}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        extension: None,
                    });
                    (None, notices)
                }
                Some(setting) => match candidates.iter().find(|(_, name)| *name == setting) {
                    Some((index, _)) => (Some(*index), notices),
                    None => {
                        notices.push(missing(setting, &names));
                        (None, notices)
                    }
                },
            }
        }
    }
}

/// The notice for a `web_search.backend` that names a backend that is not
/// installed: it names the setting and what is installed.
fn missing(setting: &str, installed: &[&str]) -> Notice {
    let installed = if installed.is_empty() {
        "none".to_owned()
    } else {
        installed
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Notice {
        code: ErrorCode::WebSearchUnavailable,
        message: format!(
            "`web_search.backend` names `{setting}`, which is not installed. Installed: {installed}."
        ),
        extension: None,
    }
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
