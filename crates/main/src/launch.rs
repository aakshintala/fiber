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
    // `tui.hover` (`docs/configuration.md`, "Keys"): the default, on,
    // comes from the config schema.
    let hover = config.flag("tui.hover", None).unwrap_or_default();
    // `tui.reduced_motion`, defaulting to off (`docs/configuration.md`).
    // Unlike the `on` helper's keys, an absent key means stillness is
    // not asked for.
    let reduced_motion = config.flag("tui.reduced_motion", None).unwrap_or_default();
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
        .or_else(|| config.text("model", None));
    let thinking = config.text("thinking", None);
    // `scoped_models`, empty for every installed model
    // (`docs/configuration.md`, "Keys").
    let scoped_models = config.list("scoped_models", None);
    tui::Launch {
        location: tui::LaunchLocation::Local {
            workspace,
            project,
            git,
        },
        open_at: tui::OpenAt::Home,
        hover,
        reduced_motion,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        commit: option_env!("FIBER_COMMIT").map(str::to_owned),
        model,
        overrides: layer,
        thinking,
        models: None,
        scoped_models,
        // `tui.logo_glyph`, defaulting to ⌇ (`docs/configuration.md`).
        logo_glyph: config
            .text("tui.logo_glyph", None)
            .unwrap_or_else(|| "⌇".to_owned()),
        rail_share: share(config, "tui.rail.width", tui::DEFAULT_RAIL_SHARE),
        rail_hidden: !on(config, "tui.rail.shown"),
        panel_share: share(config, "tui.panel.width", tui::DEFAULT_PANEL_SHARE),
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
        images: config::images_cache_dir(home),
        // `tui.inline_images`, defaulting to on in the config schema
        // (`docs/configuration.md`, "Keys").
        inline_images: on(config, "tui.inline_images"),
        // `terminal` supplies the report writer once it holds Fiber home.
        feedback: None,
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

/// The boolean at `key` (`docs/configuration.md`, "Keys"). Every key
/// read here defaults to true in the config schema, so the fallback is
/// unreachable.
fn on(config: &Config, key: &str) -> bool {
    config.flag(key, None).unwrap_or_default()
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

/// A terminal on a listed hub: the location names the hub and holds no
/// launch workspace, project or git row to empty, and no value from the
/// client's file reaches the chips: `model` stands only when the run's
/// own layer names it, while `thinking` and `scoped_models` are always
/// cleared. Every other field rides untouched.
pub(crate) fn remote(launch: tui::Launch, hub: &str) -> tui::Launch {
    let flag_names_model = launch
        .overrides
        .iter()
        .filter_map(|entry| entry.split_once('='))
        .any(|(key, _)| key == "model");
    tui::Launch {
        location: tui::LaunchLocation::Remote {
            hub: hub.to_owned(),
        },
        model: flag_names_model.then_some(()).and(launch.model),
        thinking: None,
        scoped_models: Vec::new(),
        ..launch
    }
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
