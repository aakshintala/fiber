//! `fiber setup` (`docs/invocation.md`, "Setting up"): asks the five
//! questions a new install needs, in the documented order, and applies
//! each answer with the command that already does that job. A flag answers
//! its question; without a terminal nothing is asked: setup does what the
//! flags say, skips every question without one, and prints each skipped
//! step with the command that does it later. It never picks a model nobody
//! chose.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;

use contract::clock::Clock;
use contract::files::PathLock;
use serde_json::Value;

use crate::CliError;

/// How the completions question was answered: the flag, or nothing yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completions {
    /// No `--completions` and no `--no-completions`.
    Unset,
    /// `--completions <shell>`: the shell name as the parser validated it.
    Yes(String),
    /// `--no-completions`.
    No,
}

/// How the hub question was answered: the flag, or nothing yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hub {
    /// No `--hub` and no `--no-hub`.
    Unset,
    /// `--hub`.
    Yes,
    /// `--no-hub`.
    No,
}

/// What `fiber setup` applies, in step order. The parser-level conflicts
/// (`--completions` with `--no-completions`, `--hub` with `--no-hub`) are
/// refused before these are built.
#[derive(Debug)]
pub struct SetupFlags {
    /// One `fiber login <name>` per entry, in order.
    pub provider: Vec<String>,
    /// The default model, written to the global `config.json`.
    pub model: Option<String>,
    /// The default thinking level, written beside the model.
    pub thinking: Option<String>,
    /// One `fiber import` per entry, in order.
    pub import: Vec<config::ImportSource>,
    /// The completions answer.
    pub completions: Completions,
    /// The hub answer.
    pub hub: Hub,
}

/// Runs the five setup steps in order against the process terminal. Each
/// answered step runs its command; each unanswered step prints its skipped
/// line on stderr. A failed step keeps the finished steps, runs no later
/// step, and exits non-zero naming the failed step.
pub fn setup(
    flags: &SetupFlags,
    clock: Arc<dyn Clock>,
    _locks: Arc<dyn PathLock>,
    _run: config::Runner<'_>,
) -> i32 {
    crate::with_dirs_code(|home, _workspace| run(home, flags, &clock))
}

/// The steps in their documented order, so a failure stops the later ones.
fn run(home: &Path, flags: &SetupFlags, clock: &Arc<dyn Clock>) -> Result<i32, CliError> {
    if !flags.provider.is_empty() {
        for name in &flags.provider {
            let code = crate::run_login(Some(name), None, false, Arc::clone(clock));
            if code != 0 {
                let mut err = io::stderr();
                writeln!(err, "fiber: setup providers failed").unwrap_or(());
                return Ok(code);
            }
        }
    } else {
        skipped("providers", "run fiber login");
    }
    if !flags.import.is_empty() {
        for source in &flags.import {
            let code = crate::import(*source, clock.as_ref());
            if code != 0 {
                let mut err = io::stderr();
                writeln!(err, "fiber: setup import failed").unwrap_or(());
                return Ok(code);
            }
        }
    } else {
        skipped("import", "run fiber import");
    }
    if flags.model.is_some() || flags.thinking.is_some() {
        if let Err(message) = write_model(home, flags.model.as_deref(), flags.thinking.as_deref()) {
            let mut err = io::stderr();
            writeln!(err, "fiber: setup model failed: {message}").unwrap_or(());
            return Ok(1);
        }
    } else {
        skipped("model", "run fiber setup --model <model>");
    }
    match &flags.completions {
        Completions::Yes(shell) => {
            if let Err(message) = install_completions(shell) {
                let mut err = io::stderr();
                writeln!(err, "fiber: setup completions failed: {message}").unwrap_or(());
                return Ok(1);
            }
        }
        Completions::No => {}
        Completions::Unset => skipped("completions", "run fiber setup --completions <shell>"),
    }
    match &flags.hub {
        Hub::Yes => {
            let code = crate::hub_install(None, clock.as_ref());
            if code != 0 {
                let mut err = io::stderr();
                writeln!(err, "fiber: setup hub failed").unwrap_or(());
                return Ok(code);
            }
        }
        Hub::No => {}
        Hub::Unset => skipped("hub", "run fiber setup --hub"),
    }
    Ok(0)
}

/// One skipped step with the command that does it later, on stderr.
fn skipped(step: &str, cmd: &str) {
    let mut err = io::stderr();
    writeln!(err, "skipped {step}: {cmd}").unwrap_or(());
}

/// Whether stdin is a terminal. The question flow in the follow-up task
/// asks only then; the flag path below runs either way.
#[allow(dead_code, reason = "the terminal question flow uses it next")]
fn on_terminal() -> bool {
    io::stdin().is_terminal()
}

/// Writes the picked model and thinking level to the global `config.json`,
/// the same two keys the model picker saves, in the same order: root
/// `model`, then `models."<ref>".thinking`. A `--thinking` flag with no
/// `--model` flag rides on the stored global model; with none stored there
/// is nothing to attach it to.
fn write_model(home: &Path, model: Option<&str>, thinking: Option<&str>) -> Result<(), String> {
    let reference = match model {
        Some(reference) => {
            let value = Value::String(reference.to_owned());
            config::replace_global(home, "model", Some(value))
                .map_err(|e| format!("writing the default model: {e}"))?;
            reference.to_owned()
        }
        None => match config::get_global(home, "model")
            .map_err(|e| format!("reading the default model: {e}"))?
        {
            Some(Value::String(reference)) => reference,
            _ => {
                return Err("--thinking needs a model: pass --model <model> first".to_owned());
            }
        },
    };
    if let Some(level) = thinking {
        let key = format!("models.\"{reference}\".thinking");
        config::replace_global(home, &key, Some(Value::String(level.to_owned())))
            .map_err(|e| format!("writing the thinking level: {e}"))?;
    }
    Ok(())
}

/// Appends the one line that runs `fiber completion <shell>` to the
/// shell's startup file, unless it is already there. The startup file is
/// picked by the shell name, the same rule `install.sh` uses for `PATH`.
fn install_completions(shell: &str) -> Result<(), String> {
    let (file, line) = startup_line(shell)?;
    let previous = std::fs::read_to_string(&file).unwrap_or_default();
    if previous.lines().any(|held| held == line) {
        return Ok(());
    }
    if let Some(parent) = file.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let mut text = previous;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&line);
    text.push('\n');
    std::fs::write(&file, text).map_err(|e| format!("writing {}: {e}", file.display()))?;
    Ok(())
}

/// The startup file and the one line that runs `fiber completion` for the
/// named shell: `zsh`, `bash` or `fish`, as the parser validates them.
fn startup_line(shell: &str) -> Result<(std::path::PathBuf, String), String> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| "HOME must be an absolute path to install completions".to_owned())?;
    let line = format!("eval \"$(fiber completion {shell})\"");
    match shell {
        "zsh" => Ok((home.join(".zshrc"), line)),
        "bash" => Ok((home.join(".bashrc"), line)),
        "fish" => Ok((home.join(".config/fish/config.fish"), line)),
        _ => Err(format!(
            "--completions takes bash, zsh or fish, not {shell}"
        )),
    }
}
