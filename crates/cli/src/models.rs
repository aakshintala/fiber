//! `fiber models` (`docs/invocation.md`, "Commands and flags"): lists the
//! models the installed providers serve, one row each, with the configured
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
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::Notice;
use contract::files::PathLock;
use contract::shapes::Failure;
use extensions::{Providers, SessionExtensions};
use serde::Serialize;

use crate::table::pad;
use crate::{fail, failed, project_of};

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
    #[cfg(unix)]
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
) -> Vec<String> {
    let mut stale = Vec::new();
    for name in providers.names() {
        let Some(lua) = providers.lua(name) else {
            continue;
        };
        if !lua.has_credential(config, &providers.data(name)) {
            continue;
        }
        if let Ok(Some(age)) = config::model_cache_age(home, name, now)
            && age > max_age
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
pub fn providers_and_config(
    home: &Path,
    workspace: &Path,
) -> Result<(Providers, Config, Vec<Notice>), Failure> {
    let (providers, notices) = Providers::load(home).map_err(|e| failed(e.code(), e))?;
    let (_, project) = project_of(home, workspace)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .map_err(|e| failed(e.code(), e))?;
    Ok((providers, config, notices))
}

/// Lists the installed providers' models in `home`, marking the configured
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
    load: &dyn Fn(&Config) -> SessionExtensions,
    spawn: &dyn Fn(Vec<String>) -> io::Result<()>,
    clock: &dyn Clock,
) -> Result<(), Failure> {
    // debt: notices from loading are dropped, as `parts_with` drops them;
    // surfaced when #382 lands.
    let (mut providers, config, _notices) = providers_and_config(home, workspace)?;
    let extensions = load(&config);
    for (extension, provider) in extensions.lua_providers() {
        let _notices = providers.add_lua(extension, provider, &config);
    }
    // debt: notices from placeholders are dropped, as above; surfaced
    // when #382 lands.
    providers
        .fill_placeholders(&config, &|name| std::env::var(name).ok())
        .map_err(|e| failed(e.code(), e))?;
    // A stale list refreshes in the background for the next run: the
    // detached child, never waited on. A spawn that fails is ignored:
    // `fiber models` still prints from the cache and exits 0.
    let stale = stale_lists(
        home,
        &providers,
        &config,
        config::refresh_after(&config),
        clock.wall(),
    );
    if !stale.is_empty() {
        let _ignored = spawn(stale);
    }
    if providers.names().next().is_none() {
        writeln!(err, "{NO_PROVIDER}")
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard error: {e}")))?;
        return Ok(());
    }
    let needle = search.map(|search| search.to_ascii_lowercase());
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
            if let Some(needle) = &needle
                && !reference.to_ascii_lowercase().contains(needle)
            {
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
            let line = serde_json::to_string(row)
                .map_err(|e| failed(ErrorCode::IoFailed, format!("a model row: {e}")))?;
            writeln!(out, "{line}")
                .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
        }
        return Ok(());
    }
    if rows.is_empty() {
        // A search with no match prints no rows, and no header either.
        return Ok(());
    }
    for line in &text_lines(&rows) {
        writeln!(out, "{line}")
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
    }
    Ok(())
}

/// `fiber models [--json] [<search>]` in the current directory: lists the
/// models the installed providers serve.
pub fn models(
    search: Option<&str>,
    json: bool,
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
    exe: Result<PathBuf, String>,
) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            run(
                &home,
                &workspace,
                search,
                json,
                &mut io::stdout(),
                &mut io::stderr(),
                &|config: &Config| {
                    SessionExtensions::load(
                        &home,
                        config,
                        Arc::clone(&clock),
                        Arc::clone(&locks),
                        None,
                    )
                },
                &|providers| match &exe {
                    Ok(path) => spawn_refresh(path, providers).map(drop),
                    Err(message) => Err(io::Error::other(message.clone())),
                },
                clock.as_ref(),
            )
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// Refreshes the named providers' cached model lists with the age check.
/// Only the named providers load: nothing else is discovered, and every
/// refresh runs through the locked [`extensions::refresh_lists`] API, so a
/// refresh beside another process refreshes once.
fn refresh_named(
    names: &[String],
    providers: &Providers,
    loaded: &SessionExtensions,
    config: &Config,
) {
    let wanted: Vec<Arc<extensions::LuaProvider>> = loaded
        .lua_providers()
        .iter()
        .filter(|(_, provider)| names.iter().any(|name| name == provider.name()))
        .map(|(_, provider)| Arc::clone(provider))
        .collect();
    for (_, handle) in extensions::refresh_lists(
        &wanted,
        providers,
        config,
        Some(config::refresh_after(config)),
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
) {
    if let Ok((_, project)) = project_of(home, workspace)
        && let Ok(config) = Config::load(Sources {
            home: home.to_path_buf(),
            workspace: workspace.to_path_buf(),
            project,
            overrides: Vec::new(),
        })
        && let Ok((providers, _)) = Providers::load(home)
    {
        let loaded = SessionExtensions::load(home, &config, clock, locks, None);
        refresh_named(names, &providers, &loaded, &config);
    }
}

/// `fiber refresh-model-lists <provider>...`: refreshes the named providers'
/// cached model lists with the age check, for the next run. The hidden
/// child `fiber models` spawns: hidden and free to change, like the other
/// hidden subcommands. It joins every refresh it starts, and exits 0
/// whatever the result: a background refresh never fails a command.
pub fn refresh_model_lists(names: &[String], clock: Arc<dyn Clock>, locks: Arc<dyn PathLock>) {
    if let (Ok(home), Ok(workspace)) = (config::fiber_home_from_env(), std::env::current_dir()) {
        refresh_run(&home, &workspace, names, clock, locks);
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
