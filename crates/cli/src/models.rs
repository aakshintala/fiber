//! `fiber models` (`docs/invocation.md`, "Commands and flags"): lists the
//! models the installed providers serve merged with the workspace's
//! approved repository set, one row each, with the configured
//! default marked. It reads each provider's cached list through
//! [`Providers`], which serves `cache/models/<name>.json` when there is one,
//! and runs each Lua provider's `models()` when there is none, as at startup
//! but without the background refresh: a one-shot command has nothing to
//! serve after.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use config::{Config, Sources};
use contract::clock::Clock;
use contract::events::Notice;
use contract::files::PathLock;
use extensions::{Providers, RepositorySet, SessionExtensions, StateSeed};
use serde::Serialize;

use crate::table::pad;
use crate::{CliError, project_of, with_dirs, write_line};

/// Starts the detached refresh of stale model lists: the running binary
/// re-run as its hidden refresh child: its own process group, nothing on
/// any pipe, never waited on. `exe` is the binary to re-run: the path the
/// caller recorded once at startup (`docs/releasing.md`), a test passes
/// its stub. The caller drops the returned child: dropping it neither
/// waits on nor kills it.
fn spawn_refresh(exe: &Path, providers: Vec<String>) -> io::Result<std::process::Child> {
    let mut command = std::process::Command::new(exe);
    command.arg("refresh-model-lists").args(&providers);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn()
}

/// The providers due a background refresh: a credential and a cached list
/// older than `max_age` (`docs/model-routing.md`, "Model discovery"). A
/// list exactly `max_age` old is not stale, and one with no cached copy is
/// not due: it already ran when the lists were read.
fn stale_lists(
    home: &Path,
    providers: &Providers,
    config: &Config,
    max_age: Duration,
    now: SystemTime,
    run: config::Runner<'_>,
) -> Vec<String> {
    let mut stale = Vec::new();
    for name in providers.names() {
        let Some(lua) = providers.lua(name) else {
            continue;
        };
        if let Ok(Some(age)) = config::model_cache_age(home, name, now)
            && age > max_age
            && lua.has_credential(config, &providers.data(name), run)
        {
            stale.push(name.to_owned());
        }
    }
    stale
}

/// What `fiber models` says when no provider is installed.
const NO_PROVIDER: &str =
    "No provider is installed. Run `fiber extension install <name>` to install one.";

/// One printed row: the `provider/model` reference and what its columns
/// show. `--json` prints it as is, its fields in this order.
#[derive(Serialize)]
struct Row {
    /// The model as a session names it, `provider/model`.
    #[serde(rename = "model")]
    reference: String,
    /// Its context window, in tokens.
    #[serde(rename = "context_window")]
    context: Option<u64>,
    /// Its base input price, in US dollars per million tokens.
    input: Option<f64>,
    /// Its base output price, in US dollars per million tokens.
    output: Option<f64>,
    /// Whether it is the configured default.
    default: bool,
}

/// The missing-cell mark.
fn missing() -> String {
    "-".to_owned()
}

/// The text table: a header row, then one row per model. Every line starts
/// with its two-character mark, the columns are left-aligned and padded to
/// their widest cell, and trailing spaces are trimmed.
fn text_lines(rows: &[Row]) -> Vec<String> {
    let mut table: Vec<Vec<String>> = Vec::with_capacity(rows.len() + 1);
    table.push(vec![
        "model".to_owned(),
        "context".to_owned(),
        "in $/M".to_owned(),
        "out $/M".to_owned(),
    ]);
    for row in rows {
        table.push(vec![
            row.reference.clone(),
            row.context
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(missing),
            row.input
                .map(|price| format!("{price}"))
                .unwrap_or_else(missing),
            row.output
                .map(|price| format!("{price}"))
                .unwrap_or_else(missing),
        ]);
    }
    pad(&table)
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let mark = if index > 0 && rows.get(index - 1).is_some_and(|row| row.default) {
                "* "
            } else {
                "  "
            };
            format!("{mark}{line}")
        })
        .collect()
}

/// The installed providers, the configuration for `home` and `workspace`,
/// and the notices loading the providers gave: the first half of [`run`],
/// shared with the terminal's model list, which reads the same providers
/// (`docs/model-routing.md`, "Model discovery").
pub(crate) fn providers_and_config(
    home: &Path,
    workspace: &Path,
) -> Result<(Providers, Config, Vec<Notice>), CliError> {
    let (_, project) = project_of(home, workspace)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: Vec::new(),
    })?;
    let (providers, notices) = Providers::load_enabled(home, &config)?;
    Ok((providers, config, notices))
}

/// The installed providers merged with the workspace's approved repository
/// set, the configuration for `home` and `workspace`, the retained set,
/// and the notices loading the providers gave: the first half of [`run`].
/// Outside a project, or where the declarations do not read, the set is
/// empty and the providers are the enabled installed ones, as
/// [`providers_and_config`] reads.
pub(crate) fn providers_config_and_set(
    home: &Path,
    workspace: &Path,
    clock: Arc<dyn Clock>,
) -> Result<(Providers, Config, RepositorySet, Vec<Notice>), CliError> {
    let (_, project) = project_of(home, workspace)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: Vec::new(),
    })?;
    let set = RepositorySet::load_pinned(home, &project, workspace, clock);
    let (providers, notices) = Providers::load_repository(home, &config, &set)?;
    Ok((providers, config, set, notices))
}

/// Lists the models of the installed providers merged with the workspace's
/// approved repository set in `home`, marking the configured
/// default, filtered by `search` and printed as text or JSON Lines.
#[allow(
    clippy::too_many_arguments,
    reason = "one call of the one-shot command: inputs, writers, loader, spawner and clock"
)]
fn run(
    home: &Path,
    workspace: &Path,
    search: Option<&str>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
    load: &dyn Fn(&Config, RepositorySet) -> SessionExtensions,
    spawn: &dyn Fn(Vec<String>) -> io::Result<()>,
    clock: Arc<dyn Clock>,
    run: config::Runner<'_>,
) -> Result<(), CliError> {
    // Each notice as one line on stderr: this load's, the session
    // extensions', then discovery and placeholders'. The session load
    // merges the same repository set, so it repeats the providers load's
    // shadow notices; the batch groups by code, so one pass prints one
    // notice per code (`docs/errors.md`, "Notices").
    let (mut providers, config, set, loading) =
        providers_config_and_set(home, workspace, Arc::clone(&clock))?;
    let mut notices: Vec<Notice> = config.notices().to_vec();
    notices.extend(loading);
    let extensions = load(&config, set);
    notices.extend(extensions.notices());
    // Each placeholder value from the hub's captured environment, so the
    // list agrees with the session the hub would start; no hub answers
    // and the process environment serves, as before.
    let lookup = crate::hub_env::placeholder_lookup(home, &providers, clock.as_ref());
    notices.extend(providers.add_session(&extensions, &config, run, &lookup));
    // A search selects rows and counts the models a placeholder with no
    // value leaves out by the one rule: an ASCII-case-insensitive
    // substring of `provider/model`.
    let needle = search.map(|search| search.to_ascii_lowercase());
    let listed = |reference: &str| {
        needle
            .as_ref()
            .is_none_or(|needle| reference.to_ascii_lowercase().contains(needle))
    };
    let (not_hosts, unconfigured) = providers.fill_placeholders_grouped(&config, &lookup)?;
    // The `model_unconfigured` lead counts models, not notice rows: a
    // group notice already covers several models, so two notices can
    // cover three models (`docs/model-routing.md`, "A per-account host":
    // the count is the models left out).
    let earlier_unconfigured = notices
        .iter()
        .filter(|notice| notice.code == contract::ErrorCode::ModelUnconfigured)
        .count();
    let not_host_unconfigured = not_hosts
        .iter()
        .filter(|notice| notice.code == contract::ErrorCode::ModelUnconfigured)
        .count();
    let mut grouped_models = 0;
    let mut grouped_notices = Vec::new();
    for group in &unconfigured {
        if let Some(notice) = group.notice(&listed) {
            grouped_models += group
                .models()
                .iter()
                .filter(|model| listed(&format!("{}/{}", group.provider(), model.id)))
                .count();
            grouped_notices.push(notice);
        }
    }
    let total_unconfigured = earlier_unconfigured + not_host_unconfigured + grouped_models;
    notices.extend(not_hosts);
    notices.extend(grouped_notices);
    let notices = contract::events::group_notices(notices, |first, count| {
        if first.code == contract::ErrorCode::ModelUnconfigured {
            return format!(
                "{total_unconfigured} models are unconfigured: {}",
                first.message
            );
        }
        contract::events::grouped_lead(first, count)
    });
    crate::config::print_notices(err, &notices);
    // A stale list refreshes in the background for the next run: the
    // detached child, never waited on. A spawn that fails is ignored:
    // `fiber models` still prints from the cache and exits 0.
    let stale = stale_lists(
        home,
        &providers,
        &config,
        config::refresh_after(&config),
        clock.wall(),
        run,
    );
    if !stale.is_empty() {
        let _ignored = spawn(stale);
    }
    if providers.names().next().is_none() {
        writeln!(err, "{NO_PROVIDER}").map_err(CliError::StandardError)?;
        return Ok(());
    }
    let default = config
        .get("model", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
        .and_then(|typed| providers.resolve(&typed).ok())
        .map(|model| model.reference());
    let mut rows = Vec::new();
    let mut marked = false;
    for name in providers.names() {
        let Some(provider) = providers.get(name) else {
            continue;
        };
        for model in &provider.models {
            let reference = format!("{name}/{}", model.id);
            if !listed(&reference) {
                continue;
            }
            let is_default = !marked && default.as_ref().is_some_and(|d| d == &reference);
            if is_default {
                marked = true;
            }
            rows.push(Row {
                reference,
                context: model.context_window,
                input: model.cost.as_ref().map(|cost| cost.input),
                output: model.cost.as_ref().map(|cost| cost.output),
                default: is_default,
            });
        }
    }
    if json {
        for row in &rows {
            let line = serde_json::to_string(row).map_err(CliError::ModelRow)?;
            write_line(out, &line).map_err(CliError::from)?;
        }
        return Ok(());
    }
    if rows.is_empty() {
        // A search with no match prints no rows, and no header either.
        return Ok(());
    }
    for line in &text_lines(&rows) {
        write_line(out, line).map_err(CliError::from)?;
    }
    Ok(())
}

/// `fiber models [--json] [<search>]` in the current directory: lists the
/// models the installed providers serve merged with the current workspace's
/// approved repository set.
pub fn models(
    search: Option<&str>,
    json: bool,
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
    exe: Result<PathBuf, String>,
    run: config::Runner<'_>,
) -> i32 {
    with_dirs(|home, workspace| {
        self::run(
            home,
            workspace,
            search,
            json,
            &mut io::stdout(),
            &mut io::stderr(),
            &|config: &Config, repository: RepositorySet| {
                SessionExtensions::load_repository(
                    home,
                    config,
                    Arc::clone(&clock),
                    Arc::clone(&locks),
                    None,
                    StateSeed::new(),
                    repository,
                    None,
                )
            },
            &|providers| match &exe {
                Ok(path) => spawn_refresh(path, providers).map(drop),
                Err(message) => Err(io::Error::other(message.clone())),
            },
            Arc::clone(&clock),
            run,
        )
    })
}

/// Refreshes the named providers' cached model lists with the age check.
/// Only the named providers load: nothing else is discovered, and every
/// refresh runs through the locked [`extensions::refresh_lists`] API, so a
/// refresh beside another process refreshes once.
fn refresh_named(
    home: &Path,
    names: &[String],
    providers: &Providers,
    loaded: &SessionExtensions,
    config: &Config,
    run: config::Runner<'_>,
    clock: &dyn Clock,
) {
    let wanted: Vec<Arc<extensions::LuaProvider>> = loaded
        .lua_providers()
        .iter()
        .filter(|(_, provider)| names.iter().any(|name| name == provider.name()))
        .map(|(_, provider)| Arc::clone(provider))
        .collect();
    let lookup = crate::hub_env::placeholder_lookup(home, providers, clock);
    for (_, handle) in extensions::refresh_lists(
        &wanted,
        providers,
        config,
        Some(config::refresh_after(config)),
        run,
        &lookup,
    ) {
        let _ignored = handle.join();
    }
}

/// Refreshes the named providers' cached lists in `home`, with the age
/// check: the testable body behind [`refresh_model_lists`], as `run` is
/// behind `models`.
fn refresh_run(
    home: &Path,
    workspace: &Path,
    names: &[String],
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
    run: config::Runner<'_>,
) {
    if let Ok((providers, config, set, _)) =
        providers_config_and_set(home, workspace, Arc::clone(&clock))
    {
        let loaded = SessionExtensions::load_repository(
            home,
            &config,
            Arc::clone(&clock),
            locks,
            None,
            StateSeed::new(),
            set,
            None,
        );
        refresh_named(
            home,
            names,
            &providers,
            &loaded,
            &config,
            run,
            clock.as_ref(),
        );
    }
}

/// `fiber refresh-model-lists <provider>...`: refreshes the named providers'
/// cached model lists with the age check, for the next run. The hidden
/// child `fiber models` spawns: hidden and free to change, like the other
/// hidden subcommands. It joins every refresh it starts, and exits 0
/// whatever the result: a background refresh never fails a command.
pub fn refresh_model_lists(
    names: &[String],
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
    run: config::Runner<'_>,
) {
    if let (Ok(home), Ok(workspace)) = (config::fiber_home_from_env(), std::env::current_dir()) {
        refresh_run(&home, &workspace, names, clock, locks, run);
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
