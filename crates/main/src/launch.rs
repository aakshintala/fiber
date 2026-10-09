//! Builds the terminal's launch description: the launch directory, its
//! project key, whether it is inside a git repository, and the terminal's
//! own settings (`docs/tui.md`, "Home"). Pure: the identity path, which
//! took the process, is computed once in `main` and passed in, so no test
//! runs a child process.

use std::path::{Path, PathBuf};

use config::Config;

/// The person's `keys` (`docs/configuration.md`, "Keys"): the merged
/// `keys` object as written, empty when no file sets it.
fn user_keys(config: &Config) -> serde_json::Map<String, serde_json::Value> {
    config
        .get("keys", None)
        .and_then(|(value, _)| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// Builds the terminal's launch description from the launch directory,
/// its identity path, the loaded configuration and the theme `tui.theme`
/// names.
pub(crate) fn launch(
    workspace: PathBuf,
    identity: &Path,
    config: &Config,
    theme: tui::ThemeSetting,
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
    // (`docs/configuration.md`).
    let model = config
        .get("model", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
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
        save: None,
        // `main` gives the seam once it holds Fiber home.
        configure: None,
    }
}

/// Saves a dragged share to the global configuration file.
pub(crate) fn save(home: PathBuf) -> tui::Save {
    Box::new(move |key, share| {
        config::set_global(&home, key, serde_json::json!(share)).map_err(|err| err.to_string())
    })
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
