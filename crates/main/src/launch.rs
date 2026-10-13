//! Builds the terminal's launch description: the launch directory, its
//! project key, whether it is inside a git repository, and the terminal's
//! own settings (`docs/tui.md`, "Home"). Pure: the identity path, which
//! took the process, is computed once in `main` and passed in, so no test
//! runs a child process.

use std::path::{Path, PathBuf};

use config::Config;
use config::path::{display, parse};

/// The person's `keys` (`docs/configuration.md`, "Keys"): the merged
/// `keys` object as written, empty when no file sets it.
fn user_keys(config: &Config) -> serde_json::Map<String, serde_json::Value> {
    config
        .get("keys", None)
        .and_then(|(value, _)| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// Builds the terminal's launch description from the launch directory,
/// its identity path, the loaded configuration, the theme `tui.theme`
/// names, and Fiber home, under which the viewer writes its copies.
/// `model` and `overrides` are bare `fiber`'s top-level flags: normalized
/// into the per-run layer, so `--model` reads as a `-c model=` before
/// every `-c` (`docs/configuration.md`, "Layers").
pub(crate) fn launch(
    workspace: PathBuf,
    identity: &Path,
    config: &Config,
    theme: tui::ThemeSetting,
    home: &Path,
    model: Option<String>,
    overrides: Vec<String>,
) -> tui::Launch {
    // The project key names the identity path: git's shared directory
    // inside a repository, else the launch directory itself.
    let project = log::project_key(identity);
    // Inside a repository the identity is git's shared directory, never
    // the launch directory itself.
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.clone());
    let git = canonical.as_path() != identity;
    // `tui.hover`, defaulting to on (`docs/configuration.md`, "Keys").
    let hover = config
        .get("tui.hover", None)
        .and_then(|(value, _)| value.as_bool())
        .unwrap_or(true);
    // `tui.reduced_motion`, defaulting to off (`docs/configuration.md`).
    // Unlike the `on` helper's keys, an absent key means stillness is
    // not asked for.
    let reduced_motion = config
        .get("tui.reduced_motion", None)
        .and_then(|(value, _)| value.as_bool())
        .unwrap_or(false);
    // `model` and `thinking`, unset for the chips' defaults
    // (`docs/configuration.md`). The run's flags name the chip's model:
    // the layer's last `model=`, so a later `-c model=` wins over
    // `--model`, as the session's own resolution reads them. The layer
    // holds canonical keys (`canonical_layer`), so the chip compares one
    // plain string; the value reads as `-c` documents it: JSON, or a bare
    // string when it does not parse as JSON.
    let layer = canonical_layer(crate::per_run(model, overrides));
    let model = layer
        .iter()
        .rev()
        .filter_map(|override_text| {
            let (key, value) = override_text.split_once('=')?;
            (key == "model").then(|| override_value(value))
        })
        .next()
        .or_else(|| {
            config
                .get("model", None)
                .and_then(|(value, _)| value.as_str().map(str::to_owned))
        });
    let thinking = config
        .get("thinking", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    // `scoped_models`, empty for every installed model
    // (`docs/configuration.md`, "Keys").
    let scoped_models = config
        .get("scoped_models", None)
        .and_then(|(value, _)| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|name| name.as_str().map(str::to_owned))
        .collect();
    tui::Launch {
        workspace,
        project,
        git,
        open_at: tui::OpenAt::Home,
        hover,
        reduced_motion,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        model,
        overrides: layer,
        thinking,
        models: None,
        scoped_models,
        // `tui.logo_glyph`, defaulting to ⌇ (`docs/configuration.md`).
        logo_glyph: config
            .get("tui.logo_glyph", None)
            .and_then(|(value, _)| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "⌇".to_owned()),
        rail_share: share(config, "tui.rail.width", 15.0),
        panel_share: share(config, "tui.panel.width", 21.0),
        // `tui.panel.cards`, defaulting to the built-in list.
        panel_cards: config
            .get("tui.panel.cards", None)
            .and_then(|(value, _)| serde_json::from_value(value).ok())
            .unwrap_or_else(|| {
                ["session", "changed_files", "delegates", "jobs", "quota"]
                    .map(str::to_owned)
                    .to_vec()
            }),
        keys: tui::KeysSetup {
            user: user_keys(config),
        },
        theme,
        attention: tui::Attention {
            notification: on(config, "tui.attention.notification"),
            bell: on(config, "tui.attention.bell"),
            title: on(config, "tui.attention.title"),
        },
        // The viewer's copies live under Fiber home's cache, which is
        // always safe to delete (`docs/state.md`, `docs/tui.md`,
        // "Images").
        images: home.join("cache").join("images"),
        // `main` gives the seam once it holds Fiber home.
        configure: None,
    }
}

/// The per-run layer in canonical spelling: each `-c` key through
/// `config`'s dotted-path parser, so `"model"` rides as `model` and the
/// terminal compares plain strings. A key the parser refuses rides
/// untouched, as does each value: `-c` documents JSON, or the bare
/// string when it does not parse as JSON, and the hub reads it the same
/// way. The chip resolution reads this same layer, so one parse names a
/// key everywhere.
fn canonical_layer(layer: Vec<String>) -> Vec<String> {
    layer
        .into_iter()
        .map(|override_text| match override_text.split_once('=') {
            Some((key, value)) => parse(key)
                .map(|segments| format!("{}={value}", display(&segments)))
                .unwrap_or(override_text),
            None => override_text,
        })
        .collect()
}

/// A `-c` value as `Config::load` reads it (`crates/config/src/lib.rs`):
/// JSON, or the bare string when it does not parse as JSON, so
/// `model="fake/m"` names the chip `fake/m` without its quotes.
fn override_value(value: &str) -> String {
    serde_json::from_str(value).unwrap_or_else(|_| value.to_owned())
}

/// The boolean at `key`, true when absent (`docs/configuration.md`,
/// "Keys").
fn on(config: &Config, key: &str) -> bool {
    config
        .get(key, None)
        .and_then(|(value, _)| value.as_bool())
        .unwrap_or(true)
}

/// A share of the screen's width, in percent, from `key`; `default` when
/// it is absent or outside 0 to 100. The terminal keeps the columns
/// between their floor and ceiling whatever the share.
fn share(config: &Config, key: &str, default: f64) -> f64 {
    config
        .get(key, None)
        .and_then(|(value, _)| value.as_f64())
        .filter(|share| (0.0..=100.0).contains(share))
        .unwrap_or(default)
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
