//! Picking the reviewer model at login (`docs/model-routing.md`,
//! "Logging in"): the chosen provider's model list with the saved choice
//! preselected, and the one-key save. Shared by `fiber login` and the
//! terminal app's `/login`: neither runs a second credential transaction,
//! and neither starts any other extension.

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use config::{Config, Sources, get_global, replace_global};
use contract::clock::Clock;
use contract::shapes::Failure;
use doors::failure;
use extensions::{Browser, Providers, login_provider};
use serde_json::Value;

use super::{config_failure, installed};

/// The reviewer choice for one provider: its models as local IDs, in the
/// provider's order, never padded with a stale saved ID, with the saved or
/// package choice preselected. Empty `models` is an explicit skip, even
/// when `selected` names a saved pick. Holds only local IDs and public
/// notices, never a secret or a live VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerModels {
    /// The provider's model IDs, in its order.
    pub models: Vec<String>,
    /// The saved pick, else the package default, kept even when the list
    /// does not name it.
    pub selected: Option<String>,
    /// One line per discovery problem, already redacted.
    pub notices: Vec<String>,
}

/// `providers."<name>".reviewer_model`, with a name holding a dot quoted:
/// the one key login writes, each provider keeping its own pick.
pub(crate) fn reviewer_key(name: &str) -> String {
    if name.contains('.') {
        format!("providers.\"{name}\".reviewer_model")
    } else {
        format!("providers.{name}.reviewer_model")
    }
}

/// The key's segments, refusing before any read or write when the existing
/// display/parse syntax cannot represent `name`: a name it cannot
/// round-trip, such as one holding both a dot and a quote, never becomes a
/// dotted path. The credential and every config byte stay as they were.
fn reviewer_segments(name: &str) -> Result<Vec<String>, Failure> {
    let key = reviewer_key(name);
    let expected = vec![
        "providers".to_owned(),
        name.to_owned(),
        "reviewer_model".to_owned(),
    ];
    match config::path::parse(&key) {
        Some(segments) if segments == expected => Ok(segments),
        _ => Err(failure(
            contract::ErrorCode::ConfigInvalid,
            format!("`{key}` cannot name a provider reviewer: the name is not one path segment."),
        )),
    }
}

/// Discovery never opens a browser: the handle only satisfies the VM
/// builder, and no discovery callback may reach it.
struct NoBrowser;

impl Browser for NoBrowser {
    fn open(&self, _url: &str) {}

    fn show(&self, _url: &str, _code: &str) {}

    fn attended(&self) -> bool {
        false
    }
}

/// Runs a credential command to completion, killing it at `timeout`: the
/// runner discovery's credential check takes. A stored credential answers
/// that check first, so this runs only for a configured command source.
fn run_command(command: &mut Command, timeout: Duration) -> io::Result<std::process::Output> {
    let mut child = command.spawn()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(_) => return child.wait_with_output(),
            None if start.elapsed() >= timeout => {
                child.kill().ok();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the credential command timed out",
                ));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// The discovery diagnostic with its secret-bearing text replaced: the
/// typed code stays, so a caller can still tell a failed callback from a
/// rejected model, but no callback text, path or value reaches a notice,
/// an error or a log. Only the requested provider and its admitted
/// extension are named.
fn redacted(code: contract::ErrorCode, provider: &str, owner: &str) -> String {
    if code == contract::ErrorCode::ModelInvalid {
        format!(
            "`{provider}` left out a model from `{owner}` that failed validation; \
             keeping the listed ones."
        )
    } else {
        format!("`{provider}`'s model list from `{owner}` is unavailable; keeping the stored list.")
    }
}

/// Reads the reviewer choice for the installed provider `name`: its models
/// through its own admitted extension only, preselecting the saved global
/// value, else the package default. The caller has stored the credential:
/// discovery runs only for a provider with one, serving its data or cache
/// otherwise, and every notice crossing the discovery seam is redacted. A
/// malformed or unreadable configuration is an error, never permission to
/// guess. Writes nothing except the provider's own model cache, which a
/// successful uncached discovery updates as it returns.
pub fn reviewer_models(
    home: &Path,
    providers: &Providers,
    name: &str,
    clock: Arc<dyn Clock>,
) -> Result<ReviewerModels, Failure> {
    let data = installed(providers, name)?.clone();
    let key = reviewer_key(name);
    let selected = match get_global(home, &key).map_err(config_failure)? {
        Some(value) => value.as_str().map(str::to_owned),
        None => None,
    }
    .or_else(|| data.reviewer_model.clone());
    let fallback: Vec<String> = data.models.iter().map(|model| model.id.clone()).collect();
    let Some(owner) = providers.extension_of(name).map(str::to_owned) else {
        // No recorded owner: no hook to run, so the data file's list
        // stands, with no notice.
        return Ok(ReviewerModels {
            models: fallback,
            selected,
            notices: Vec::new(),
        });
    };
    // The home layers are discovery's configuration: the stored
    // credential it checks was just written to the global file, so the
    // check does not depend on the launch directory.
    let (_, project) = crate::project_of(home, home)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: home.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .map_err(config_failure)?;
    let lua = match login_provider(home, providers, name, Arc::new(NoBrowser), clock) {
        Ok(lua) => lua,
        Err(e) => {
            return Ok(ReviewerModels {
                models: fallback,
                selected,
                notices: vec![redacted(e.code(), name, &owner)],
            });
        }
    };
    // A temporary registry takes the admission: the installed providers
    // stay as loaded, and the VM drops with this return.
    let mut temp = providers.clone();
    let mut notices: Vec<String> = temp
        .add_lua(&owner, &lua, &config, &run_command)
        .into_iter()
        .map(|notice| redacted(notice.code, name, &owner))
        .collect();
    let models: Vec<String> = temp
        .get(name)
        .map(|data| data.models.iter().map(|model| model.id.clone()).collect())
        .unwrap_or(fallback);
    if models.is_empty() {
        notices.push(format!(
            "`{name}` lists no models; the credential stays, with no reviewer set."
        ));
    }
    Ok(ReviewerModels {
        models,
        selected,
        notices,
    })
}

/// Saves `id` as provider `name`'s reviewer, exactly as listed: no
/// discovery rerun, no second list check, so a stale Enter still lands.
/// Names the key on failure; the stored credential stays either way.
pub fn set_reviewer(home: &Path, name: &str, id: &str) -> Result<(), Failure> {
    let key = reviewer_key(name);
    reviewer_segments(name)?;
    replace_global(home, &key, Some(Value::String(id.to_owned())))
        .map(|_| ())
        .map_err(|e| failure(e.code(), format!("could not save `{key}`: {e}")))
}

/// What the terminal prompt answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    /// A listed ID, or the kept preselection on Enter.
    Chosen(String),
    /// EOF, cancellation, or an empty list: save nothing.
    Cancelled,
}

/// Asks for one of `choices` on the terminal: the numbered list with the
/// saved row marked, then one line per answer. An exact name wins over a
/// number, so a numeric model ID still chooses itself; a number out of
/// range, zero, an overflow, an unknown name and (with nothing preselected)
/// a blank line each print one line naming the problem and ask again,
/// until a listed choice, cancellation or EOF. `stopped` is the login's
/// cancellation scope, checked before every read. Never fabricates or
/// saves a model: choosing is the caller's, after the store.
pub(crate) fn prompt_choice(
    name: &str,
    choices: &ReviewerModels,
    input: &mut dyn BufRead,
    err: &mut dyn Write,
    stopped: &dyn Fn() -> bool,
) -> io::Result<Pick> {
    if choices.models.is_empty() {
        return Ok(Pick::Cancelled);
    }
    writeln!(err, "Reviewer for {name}:")?;
    for (index, model) in choices.models.iter().enumerate() {
        let saved = choices
            .selected
            .as_deref()
            .is_some_and(|keep| keep == model);
        writeln!(
            err,
            "  {}) {model}{}",
            index + 1,
            if saved { " (saved)" } else { "" }
        )?;
    }
    if let Some(keep) = choices.selected.as_deref()
        && !choices.models.iter().any(|model| model == keep)
    {
        writeln!(
            err,
            "Saved reviewer `{keep}` is not listed; Enter keeps it."
        )?;
    }
    loop {
        if stopped() {
            return Ok(Pick::Cancelled);
        }
        write!(err, "Reviewer, by number or name: ")?;
        err.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(Pick::Cancelled);
        }
        if stopped() {
            return Ok(Pick::Cancelled);
        }
        let answer = line.trim();
        if answer.is_empty() {
            if let Some(keep) = choices.selected.as_deref() {
                return Ok(Pick::Chosen(keep.to_owned()));
            }
            writeln!(err, "No reviewer is selected; choose a number or name.")?;
            continue;
        }
        if let Some(pick) = parse_pick(answer, choices) {
            return Ok(Pick::Chosen(pick));
        }
        writeln!(
            err,
            "`{answer}` is not a listed reviewer; choose a number or name."
        )?;
    }
}

/// A listed answer: an exact name first, then a 1-based number. Anything
/// else is `None`: invalid input never becomes a model.
fn parse_pick(answer: &str, choices: &ReviewerModels) -> Option<String> {
    if choices.models.iter().any(|model| model == answer) {
        return Some(answer.to_owned());
    }
    let number: usize = answer.parse().ok()?;
    choices.models.get(number.checked_sub(1)?).cloned()
}

#[cfg(test)]
#[path = "reviewer_tests.rs"]
mod tests;
