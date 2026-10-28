//! `fiber setup`: one guided run through the install questions
//! (`docs/user/install.md`, "Setup"): each answered step runs its command
//! as the flags do, and each unanswered step names its command. On a
//! terminal the five questions are asked in the documented order with the
//! current answers preselected; without one only the flags apply.

use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::{Config, ImportSource};
use contract::clock::Clock;
use contract::files::PathLock;
use extensions::{Providers, SessionExtensions};
use serde_json::Value;

use crate::model_lists::model_lists_apart;
use crate::{CliError, ModelLists, ModelListsError, login_targets};

/// `fiber setup [provider...] [--model <model>] [--thinking <level>]
/// [--import <source>...] [--completions <shell> | --no-completions]
/// [--hub | --no-hub]`: the install questions in one command.
#[derive(Debug, Default)]
pub struct SetupFlags {
    /// Providers to log in to, each as `fiber login <name>` does.
    pub provider: Vec<String>,
    /// The default model, written with
    /// `fiber setup --model <model> --thinking <level>`.
    pub model: Option<String>,
    /// Its thinking level, written with the default model.
    pub thinking: Option<String>,
    /// Sources whose configuration to import, each as `fiber import`
    /// does.
    pub import: Vec<ImportSource>,
    /// Whether to install shell completions, and for which shell.
    pub completions: Completions,
    /// Whether to register the hub as a login service.
    pub hub: Hub,
}

/// The `--completions <shell> | --no-completions` answer.
#[derive(Debug, Default)]
pub enum Completions {
    /// No `--completions` flag: ask on a terminal, skip without one.
    #[default]
    Unset,
    /// Install completions for the shell, as `fiber completion` prints.
    Yes(String),
    /// Leave the startup files alone.
    No,
}

/// The `--hub | --no-hub` answer.
#[derive(Debug, Default)]
pub enum Hub {
    /// No `--hub` flag: ask on a terminal, skip without one.
    #[default]
    Unset,
    /// Register the hub as a login service.
    Yes,
    /// Leave the hub unregistered.
    No,
}

/// One installed provider row the first question offers, with its live
/// signed-in mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderOffer {
    /// The provider's name.
    pub(crate) name: String,
    /// Whether any label is stored for it.
    pub(crate) signed_in: bool,
}

/// One model reference the third question offers, with its thinking
/// levels by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelOffer {
    /// The `provider/model` reference.
    pub(crate) reference: String,
    /// Its thinking levels, in the provider data's order.
    pub(crate) levels: Vec<String>,
}

/// The installed providers as rows, in login-menu order with each live
/// signed-in mark; declared secrets are never offered.
pub(crate) fn provider_offers(
    home: &Path,
    providers: &Providers,
) -> Result<Vec<ProviderOffer>, CliError> {
    let mut offers = Vec::new();
    for target in login_targets(home)? {
        let name = match target {
            crate::login::LoginName::Provider(name) | crate::login::LoginName::Browser(name) => {
                name
            }
            crate::login::LoginName::Secret(_) => continue,
        };
        let Some(data) = providers.get(&name) else {
            continue;
        };
        offers.push(ProviderOffer {
            name,
            signed_in: crate::login::signed_in(home, data).map_err(CliError::Config)?,
        });
    }
    Ok(offers)
}

/// The models of the signed-in providers only, as references, in the same
/// row order with each provider's model order.
pub(crate) fn model_offers(
    home: &Path,
    providers: &Providers,
) -> Result<Vec<ModelOffer>, CliError> {
    let mut offers = Vec::new();
    for offer in provider_offers(home, providers)? {
        if !offer.signed_in {
            continue;
        }
        let Some(data) = providers.get(&offer.name) else {
            continue;
        };
        for model in &data.models {
            offers.push(ModelOffer {
                reference: format!("{}/{}", offer.name, model.id),
                levels: model
                    .thinking_levels
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            });
        }
    }
    Ok(offers)
}

/// Whether the terminal import question offers `source`: its directory
/// exists and no completed receipt names it. A receipt that fails to
/// parse as JSON is no receipt: the offer stands.
pub(crate) fn import_offered(home_env: &Path, home: &Path, source: ImportSource) -> bool {
    config::source_home(home_env, source).is_dir() && !receipt_completed(home, source)
}

/// Whether Fiber home holds a completed receipt for `source`: the
/// documented file parses as JSON and its `source` names the source
/// (`docs/configuration.md`, "Importing another agent's setup").
pub(crate) fn receipt_completed(home: &Path, source: ImportSource) -> bool {
    let file = home.join(format!("import-{}.json", source.name()));
    let Ok(text) = std::fs::read_to_string(&file) else {
        return false;
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|receipt| {
            receipt
                .get("source")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|name| name == source.name())
}

/// The shell `$SHELL` names: zsh, bash or fish by its basename. Anything
/// else, and a missing `$SHELL`, is no shell: the completions question
/// has nothing to install into.
pub(crate) fn shell_from_env(var: &dyn Fn(&str) -> Option<OsString>) -> Option<String> {
    let shell = var("SHELL")?;
    let base = Path::new(&shell).file_name()?.to_str()?;
    let base = base.strip_prefix('-').unwrap_or(base);
    match base {
        "zsh" | "bash" | "fish" => Some(base.to_owned()),
        _ => None,
    }
}

/// Whether the startup file for `shell` already holds its completion
/// line: a missing or unreadable file holds nothing, so the question
/// asks again and the write reports an unreadable file.
pub(crate) fn completions_installed(home_env: &Path, shell: &str) -> bool {
    let Ok((file, line)) = startup_line(home_env, shell) else {
        return false;
    };
    std::fs::read_to_string(&file)
        .map(|previous| previous.lines().any(|held| held == line))
        .unwrap_or(false)
}

/// Runs the five setup steps in order against the process terminal. Each
/// answered step runs its command; each unanswered step prints its skipped
/// line on stderr. A failed step keeps the finished steps, runs no later
/// step, and exits non-zero naming the failed step.
pub fn setup(
    flags: &SetupFlags,
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
    run: config::Runner<'_>,
) -> i32 {
    crate::with_dirs_code(|home, workspace| self::run(home, workspace, flags, &clock, &locks, run))
}

/// The steps in their documented order, so a failure stops the later ones.
/// Without a terminal each step applies its flag, and each step no flag
/// answers prints its skip line: the same flag handling the terminal
/// questions share.
fn run(
    home: &Path,
    workspace: &Path,
    flags: &SetupFlags,
    clock: &Arc<dyn Clock>,
    locks: &Arc<dyn PathLock>,
    run: config::Runner<'_>,
) -> Result<i32, CliError> {
    if io::stdin().is_terminal() {
        let mut stdin = io::stdin().lock();
        let mut err = io::stderr();
        run_terminal(
            home, workspace, flags, clock, locks, run, &mut stdin, &mut err,
        )
    } else {
        let mut err = io::stderr();
        let code = step_providers(home, None, flags, clock, None, &mut err)?;
        if code != 0 {
            return Ok(code);
        }
        let code = step_import(home, flags, clock, None, &mut err)?;
        if code != 0 {
            return Ok(code);
        }
        let code = step_model(home, None, flags, None, &mut err)?;
        if code != 0 {
            return Ok(code);
        }
        let code = step_completions(flags, None, &mut err)?;
        if code != 0 {
            return Ok(code);
        }
        step_hub(home, flags, clock, None, &mut err)
    }
}

/// One cached model-list read serves the provider and model questions.
/// The notices of models left out for a missing per-account host
/// (`model_unconfigured`) are not printed: those models are out of the
/// model question, and a provider's login asks for the missing value.
/// Every other notice prints once on standard error, as `fiber models`
/// prints it (one line each), before the first question.
#[allow(
    clippy::too_many_arguments,
    reason = "one call of the terminal run: inputs, writers and the effects' context"
)]
fn run_terminal(
    home: &Path,
    workspace: &Path,
    flags: &SetupFlags,
    clock: &Arc<dyn Clock>,
    locks: &Arc<dyn PathLock>,
    run: config::Runner<'_>,
    stdin: &mut dyn BufRead,
    err: &mut dyn Write,
) -> Result<i32, CliError> {
    let load = |config: &Config| {
        SessionExtensions::load(home, config, Arc::clone(clock), Arc::clone(locks), None)
    };
    let (providers, _config, notices, _unconfigured) =
        model_lists_apart(home, workspace, ModelLists::Cached, &load, run, &**clock).map_err(
            |error| match error {
                ModelListsError::Load(error) => error,
                ModelListsError::Config(error) => CliError::Config(error),
                ModelListsError::Placeholders(error) => CliError::Extensions(error),
            },
        )?;
    for notice in &notices {
        writeln!(err, "{}", notice.message).map_err(CliError::StandardError)?;
    }
    let code = step_providers(home, Some(&providers), flags, clock, Some(&mut *stdin), err)?;
    if code != 0 {
        return Ok(code);
    }
    let code = step_import(home, flags, clock, Some(&mut *stdin), err)?;
    if code != 0 {
        return Ok(code);
    }
    let code = step_model(home, Some(&providers), flags, Some(&mut *stdin), err)?;
    if code != 0 {
        return Ok(code);
    }
    let code = step_completions(flags, Some(&mut *stdin), err)?;
    if code != 0 {
        return Ok(code);
    }
    step_hub(home, flags, clock, Some(&mut *stdin), err)
}

/// The first step: the installed providers in login-menu order with each
/// live signed-in mark. Its flag logs in as `fiber login <name>` does; a
/// terminal is asked, and anything else prints the step's skip line. A
/// blank line signs in to none. A failed login keeps the finished steps
/// and ends the setup with its code.
fn step_providers(
    home: &Path,
    providers: Option<&Providers>,
    flags: &SetupFlags,
    clock: &Arc<dyn Clock>,
    mut stdin: Option<&mut dyn BufRead>,
    err: &mut dyn Write,
) -> Result<i32, CliError> {
    // Each login reads setup's reader: on a terminal setup holds the
    // standard input lock, and a second lock never returns.
    if !flags.provider.is_empty() {
        for name in &flags.provider {
            let reader = stdin
                .as_mut()
                .map(|reader| &mut **reader as &mut dyn BufRead);
            let code =
                crate::login::run_login_with(Some(name), None, false, Arc::clone(clock), reader);
            if code != 0 {
                writeln!(err, "fiber: setup providers failed").map_err(CliError::StandardError)?;
                return Ok(code);
            }
        }
        return Ok(0);
    }
    let (Some(stdin), Some(providers)) = (stdin, providers) else {
        skipped_to(err, "providers", "run fiber login");
        return Ok(0);
    };
    let offers = provider_offers(home, providers)?;
    if offers.is_empty() {
        skipped_to(err, "providers", "run fiber login");
        return Ok(0);
    }
    writeln!(err, "Providers:").map_err(CliError::StandardError)?;
    for (index, offer) in offers.iter().enumerate() {
        writeln!(
            err,
            "  {}) {}{}",
            index + 1,
            offer.name,
            if offer.signed_in { " (signed in)" } else { "" }
        )
        .map_err(CliError::StandardError)?;
    }
    let names: Vec<String> = offers.iter().map(|offer| offer.name.clone()).collect();
    for name in pick_many(
        stdin,
        err,
        "Sign in to providers by number or name, separated by commas (blank skips): ",
        &names,
    )? {
        let code = crate::login::run_login_with(
            Some(&name),
            None,
            false,
            Arc::clone(clock),
            Some(&mut *stdin),
        );
        if code != 0 {
            writeln!(err, "fiber: setup providers failed").map_err(CliError::StandardError)?;
            return Ok(code);
        }
    }
    Ok(0)
}

/// The second step: each source whose directory exists and has no
/// completed receipt is offered as `fiber import` runs it. Its flag
/// imports as the flag names; a terminal is asked, and anything else
/// prints the step's skip line. Completed receipts are shown as done,
/// never asked.
fn step_import(
    home: &Path,
    flags: &SetupFlags,
    clock: &Arc<dyn Clock>,
    stdin: Option<&mut dyn BufRead>,
    err: &mut dyn Write,
) -> Result<i32, CliError> {
    if !flags.import.is_empty() {
        for source in &flags.import {
            let code = crate::import(*source, clock.as_ref());
            if code != 0 {
                writeln!(err, "fiber: setup import failed").map_err(CliError::StandardError)?;
                return Ok(code);
            }
        }
        return Ok(0);
    }
    let Some(stdin) = stdin else {
        skipped_to(err, "import", "run fiber import");
        return Ok(0);
    };
    let home_env = std::env::var_os("HOME").map(PathBuf::from);
    let pending: Vec<ImportSource> = [ImportSource::ClaudeCode, ImportSource::Codex]
        .into_iter()
        .filter(|source| {
            home_env
                .as_ref()
                .is_some_and(|dir| import_offered(dir, home, *source))
        })
        .collect();
    let mut completed = false;
    for source in [ImportSource::ClaudeCode, ImportSource::Codex] {
        if receipt_completed(home, source) {
            completed = true;
            writeln!(err, "{}: already imported (done)", display_source(source))
                .map_err(CliError::StandardError)?;
        }
    }
    if pending.is_empty() {
        if !completed {
            skipped_to(err, "import", "run fiber import");
        }
        return Ok(0);
    }
    for source in pending {
        let Some(dir) = home_env
            .as_ref()
            .map(|root| config::source_home(root, source))
        else {
            continue;
        };
        let prompt = format!(
            "Import {}'s configuration ({})? [y/N]: ",
            display_source(source),
            display_home(&dir)
        );
        if ask_yes(stdin, err, &prompt)? {
            let code = crate::import(source, clock.as_ref());
            if code != 0 {
                writeln!(err, "fiber: setup import failed").map_err(CliError::StandardError)?;
                return Ok(code);
            }
        }
    }
    Ok(0)
}

/// The third step: the default model and its thinking level, from the
/// signed-in providers' models, with the current global values
/// preselected. Its flags write as the flags name; a terminal is asked,
/// and anything else prints the step's skip line. A blank model keeps the
/// current one, or leaves it unset when there is none; a failed write
/// keeps the finished steps.
fn step_model(
    home: &Path,
    providers: Option<&Providers>,
    flags: &SetupFlags,
    stdin: Option<&mut dyn BufRead>,
    err: &mut dyn Write,
) -> Result<i32, CliError> {
    if flags.model.is_some() || flags.thinking.is_some() {
        if let Err(message) = write_model(home, flags.model.as_deref(), flags.thinking.as_deref()) {
            writeln!(err, "fiber: setup model failed: {message}")
                .map_err(CliError::StandardError)?;
            return Ok(1);
        }
        return Ok(0);
    }
    let (Some(stdin), Some(providers)) = (stdin, providers) else {
        skipped_to(err, "model", "run fiber setup --model <model>");
        return Ok(0);
    };
    let models = model_offers(home, providers)?;
    if models.is_empty() {
        skipped_to(err, "model", "run fiber setup --model <model>");
        return Ok(0);
    }
    let current = config::get_global(home, "model")
        .map_err(CliError::Config)?
        .and_then(|value| value.as_str().map(str::to_owned));
    match &current {
        Some(model) => {
            writeln!(err, "Default model (current: {model}):").map_err(CliError::StandardError)?
        }
        None => writeln!(err, "Default model:").map_err(CliError::StandardError)?,
    }
    for (index, offer) in models.iter().enumerate() {
        writeln!(err, "  {}) {}", index + 1, offer.reference).map_err(CliError::StandardError)?;
    }
    let names: Vec<String> = models.iter().map(|offer| offer.reference.clone()).collect();
    let picked = pick_one(
        stdin,
        err,
        "Pick the default model by number or name (blank keeps current): ",
        &names,
    )?;
    let Some(reference) = picked.clone().or(current) else {
        return Ok(0);
    };
    let levels: Vec<String> = models
        .iter()
        .find(|offer| offer.reference == reference)
        .map(|offer| offer.levels.clone())
        .unwrap_or_default();
    let thinking = if levels.is_empty() {
        None
    } else {
        let current = config::get_global(home, &format!("models.\"{reference}\".thinking"))
            .map_err(CliError::Config)?
            .and_then(|value| value.as_str().map(str::to_owned));
        match &current {
            Some(level) => writeln!(
                err,
                "Thinking for {reference} (current: {level}): {}",
                levels.join(", ")
            )
            .map_err(CliError::StandardError)?,
            None => writeln!(
                err,
                "Thinking for {reference} (current: unset): {}",
                levels.join(", ")
            )
            .map_err(CliError::StandardError)?,
        }
        pick_one(
            stdin,
            err,
            "Pick a thinking level by number or name (blank keeps current): ",
            &levels,
        )?
    };
    if picked.is_none() && thinking.is_none() {
        return Ok(0);
    }
    if let Err(message) = write_model(
        home,
        picked.as_deref().or(Some(&reference)),
        thinking.as_deref(),
    ) {
        writeln!(err, "fiber: setup model failed: {message}").map_err(CliError::StandardError)?;
        return Ok(1);
    }
    Ok(0)
}

/// The fourth step: whether to install shell completions for the shell
/// `$SHELL` names. Its flag installs as the flag names; a terminal is
/// asked, and anything else prints the step's skip line. A step already
/// done is shown as done and asked again, so answering yes reruns its
/// command.
fn step_completions(
    flags: &SetupFlags,
    stdin: Option<&mut dyn BufRead>,
    err: &mut dyn Write,
) -> Result<i32, CliError> {
    match &flags.completions {
        Completions::Unset => {}
        Completions::Yes(shell) => {
            if let Err(message) = install_completions(shell) {
                writeln!(err, "fiber: setup completions failed: {message}")
                    .map_err(CliError::StandardError)?;
                return Ok(1);
            }
            return Ok(0);
        }
        Completions::No => return Ok(0),
    }
    let Some(stdin) = stdin else {
        skipped_to(err, "completions", "run fiber setup --completions <shell>");
        return Ok(0);
    };
    let Some(shell) = shell_from_env(&|key| std::env::var_os(key)) else {
        skipped_to(err, "completions", "run fiber setup --completions <shell>");
        return Ok(0);
    };
    let home_env = std::env::var_os("HOME").map(PathBuf::from);
    let done = home_env
        .as_ref()
        .is_some_and(|dir| completions_installed(dir, &shell));
    if done {
        writeln!(err, "Completions for {shell} are installed (done).")
            .map_err(CliError::StandardError)?;
    }
    let shown = home_env.as_ref().and_then(|dir| {
        let (file, _) = startup_line(dir, &shell).ok()?;
        file.strip_prefix(dir)
            .ok()
            .map(|rest| format!("~/{}", rest.display()))
    });
    let prompt = match (done, shown) {
        (true, _) => format!("Install completions for {shell} again? [y/N]: "),
        (false, Some(file)) => format!("Install completions for {shell} ({file})? [y/N]: "),
        (false, None) => format!("Install completions for {shell}? [y/N]: "),
    };
    if !ask_yes(stdin, err, &prompt)? {
        return Ok(0);
    }
    if let Err(message) = install_completions(&shell) {
        writeln!(err, "fiber: setup completions failed: {message}")
            .map_err(CliError::StandardError)?;
        return Ok(1);
    }
    Ok(0)
}

/// The fifth step: whether to register the hub as a login service. Its
/// flag registers as the flag names; a terminal is asked, and anything
/// else prints the step's skip line. A hub already registered is named,
/// not asked.
fn step_hub(
    home: &Path,
    flags: &SetupFlags,
    clock: &Arc<dyn Clock>,
    stdin: Option<&mut dyn BufRead>,
    err: &mut dyn Write,
) -> Result<i32, CliError> {
    match &flags.hub {
        Hub::Unset => {}
        Hub::Yes => {
            let code = crate::hub_install(None, clock.as_ref());
            if code != 0 {
                writeln!(err, "fiber: setup hub failed").map_err(CliError::StandardError)?;
                return Ok(code);
            }
            return Ok(0);
        }
        Hub::No => return Ok(0),
    }
    let Some(stdin) = stdin else {
        skipped_to(err, "hub", "run fiber setup --hub");
        return Ok(0);
    };
    let manager = crate::hub_service::manager();
    match crate::hub_status::installed(manager, home, &|key| std::env::var_os(key)) {
        Ok(true) => {
            let name = crate::hub_unit::name(home)?;
            writeln!(err, "The hub login service {name} is installed.")
                .map_err(CliError::StandardError)?;
        }
        Ok(false) => {
            if ask_yes(stdin, err, "Register the hub as a login service? [y/N]: ")? {
                let code = crate::hub_install(None, clock.as_ref());
                if code != 0 {
                    writeln!(err, "fiber: setup hub failed").map_err(CliError::StandardError)?;
                    return Ok(code);
                }
            }
        }
        Err(error) => return Err(error),
    }
    Ok(0)
}

/// Prints `prompt` on standard error and reads one trimmed line; end of
/// input reads as a blank line, which every question takes as skip.
fn ask_line(
    stdin: &mut dyn BufRead,
    err: &mut dyn Write,
    prompt: &str,
) -> Result<String, CliError> {
    err.write_all(prompt.as_bytes())
        .and_then(|()| err.flush())
        .map_err(CliError::Terminal)?;
    let mut line = String::new();
    stdin
        .read_line(&mut line)
        .map_err(CliError::StandardInput)?;
    Ok(line.trim().to_owned())
}

/// A yes-or-no answer: `y` or `yes` against anything else, blank for no.
/// Anything else names the problem and asks again.
fn ask_yes(stdin: &mut dyn BufRead, err: &mut dyn Write, prompt: &str) -> Result<bool, CliError> {
    loop {
        match ask_line(stdin, err, prompt)?.to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "" | "n" | "no" => return Ok(false),
            _ => writeln!(err, "Please answer y or n.").map_err(CliError::StandardError)?,
        }
    }
}

/// One of `names` by 1-based number or exact name; a blank line picks
/// nothing. Anything else names the problem and asks again.
fn pick_one(
    stdin: &mut dyn BufRead,
    err: &mut dyn Write,
    prompt: &str,
    names: &[String],
) -> Result<Option<String>, CliError> {
    loop {
        let answer = ask_line(stdin, err, prompt)?;
        if answer.is_empty() {
            return Ok(None);
        }
        if let Some(name) = parse_pick(&answer, names) {
            return Ok(Some(name));
        }
        writeln!(err, "`{answer}` is not listed; choose a number or name.")
            .map_err(CliError::StandardError)?;
    }
}

/// Any of `names` by 1-based number or exact name, comma- or
/// space-separated; a blank line picks none. Anything else names the
/// problem and asks again.
fn pick_many(
    stdin: &mut dyn BufRead,
    err: &mut dyn Write,
    prompt: &str,
    names: &[String],
) -> Result<Vec<String>, CliError> {
    loop {
        let answer = ask_line(stdin, err, prompt)?;
        if answer.is_empty() {
            return Ok(Vec::new());
        }
        let mut picked = Vec::new();
        let mut bad = None;
        for token in answer
            .split([',', ' ', '\t'])
            .filter(|token| !token.is_empty())
        {
            match parse_pick(token, names) {
                Some(name) => {
                    if !picked.contains(&name) {
                        picked.push(name);
                    }
                }
                None => {
                    bad = Some(token.to_owned());
                    break;
                }
            }
        }
        if let Some(token) = bad {
            writeln!(err, "`{token}` is not listed; choose numbers or names.")
                .map_err(CliError::StandardError)?;
            continue;
        }
        return Ok(picked);
    }
}

/// An exact name first, then a 1-based number into `names`.
fn parse_pick(answer: &str, names: &[String]) -> Option<String> {
    if names.iter().any(|name| name == answer) {
        return Some(answer.to_owned());
    }
    answer
        .parse::<usize>()
        .ok()?
        .checked_sub(1)
        .and_then(|index| names.get(index))
        .cloned()
}

/// `skipped <step>: <cmd>` on `err`: the step with nothing to ask and
/// the command that does it later.
fn skipped_to(err: &mut dyn Write, step: &str, cmd: &str) {
    writeln!(err, "skipped {step}: {cmd}").unwrap_or(());
}

/// The source's display name in the import question.
fn display_source(source: ImportSource) -> &'static str {
    match source {
        ImportSource::ClaudeCode => "Claude Code",
        ImportSource::Codex => "Codex",
    }
}

/// The source directory as `~/.claude`: the question shows where the
/// import reads, whatever the test's home is.
fn display_home(dir: &Path) -> String {
    match dir.file_name().and_then(|name| name.to_str()) {
        Some(name) => format!("~/{name}"),
        None => dir.display().to_string(),
    }
}

/// Writes the picked model and thinking level to the global `config.json`,
/// the same two keys the model picker saves, in the same order: root
/// `model`, then `models."<reference>".thinking`. A `--thinking` flag with
/// no `--model` flag rides on the stored global model; with none stored
/// there is nothing to attach it to.
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
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| "HOME must be an absolute path to install completions".to_owned())?;
    let (file, line) = startup_line(&home, shell)?;
    // The home is absolute, so a parent is a real directory to create.
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let previous = match std::fs::read_to_string(&file) {
        Ok(previous) => previous,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("reading {}: {error}", file.display())),
    };
    if previous.lines().any(|held| held == line) {
        return Ok(());
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
fn startup_line(home: &Path, shell: &str) -> Result<(PathBuf, String), String> {
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

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
