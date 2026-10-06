//! `fiber models` (`docs/invocation.md`, "Commands and flags"): lists the
//! models the installed providers serve, one row each, with the configured
//! default marked. It reads each provider's cached list through
//! [`Providers`], which serves `cache/models/<name>.json` when there is one.

use std::io::{self, Write};
use std::path::Path;

use config::{Config, Sources};
use contract::ErrorCode;
use contract::shapes::Failure;
use extensions::Providers;
use serde::Serialize;

use crate::{fail, failed, project_of};

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
    let mut table: Vec<[String; 4]> = Vec::with_capacity(rows.len() + 1);
    table.push([
        "model".to_owned(),
        "context".to_owned(),
        "in $/M".to_owned(),
        "out $/M".to_owned(),
    ]);
    for row in rows {
        table.push([
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
    let mut widths = [0_usize; 4];
    for cells in &table {
        for (index, cell) in cells.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(cell.chars().count());
            }
        }
    }
    let mut lines = Vec::with_capacity(table.len());
    for (index, cells) in table.iter().enumerate() {
        let mark = if index == 0 {
            "  "
        } else if rows.get(index - 1).is_some_and(|row| row.default) {
            "* "
        } else {
            "  "
        };
        let mut line = String::from(mark);
        for (index, cell) in cells.iter().enumerate() {
            if index > 0 {
                line.push_str("  ");
            }
            let width = widths.get(index).copied().unwrap_or(0);
            line.push_str(&format!("{cell:<width$}"));
        }
        while line.ends_with(' ') {
            line.pop();
        }
        lines.push(line);
    }
    lines
}

/// Lists the installed providers' models in `home`, marking the configured
/// default, filtered by `search` and printed as text or JSON Lines.
fn run(
    home: &Path,
    workspace: &Path,
    search: Option<&str>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    // debt: notices from loading are dropped, as `parts_with` drops them;
    // surfaced when #382 lands.
    let (providers, _notices) = Providers::load(home).map_err(|e| failed(e.code(), e))?;
    if providers.names().next().is_none() {
        writeln!(err, "{NO_PROVIDER}")
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard error: {e}")))?;
        return Ok(());
    }
    let (_, project) = project_of(home, workspace)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .map_err(|e| failed(e.code(), e))?;
    let default = config
        .get("model", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
        .and_then(|typed| providers.resolve(&typed).ok())
        .map(|model| model.reference());
    let needle = search.map(|search| search.to_ascii_lowercase());
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
pub fn models(search: Option<&str>, json: bool) -> i32 {
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
            )
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
