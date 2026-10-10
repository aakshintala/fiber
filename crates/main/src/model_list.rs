//! The installed models for the terminal's model picker (`docs/tui.md`,
//! "Swapped views"): one entry per model of every installed provider,
//! read the way `fiber models` reads them (`docs/model-routing.md`,
//! "Model discovery"). A `Cached` read serves the cached lists and starts
//! no extension; `Stale` and `Every` load the Lua providers, refresh with
//! and without the age check, join every refresh, and drop the extensions
//! before returning, so no VM stays resident and the idle terminal starts
//! none.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use config::Config;
use contract::clock::Clock;
use contract::files::PathLock;
use extensions::{Providers, SessionExtensions};

/// How the terminal reads the installed models: [`read`] on a thread off
/// the loop, with Fiber home, the workspace, the clock and the locks.
pub(crate) fn reader(
    home: PathBuf,
    workspace: PathBuf,
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
) -> tui::ReadModels {
    Arc::new(move |refresh| {
        read(&home, &workspace, refresh, clock.wall(), &|config| {
            SessionExtensions::load(&home, config, Arc::clone(&clock), Arc::clone(&locks), None)
        })
    })
}

/// The installed models at `refresh`: the cached lists at once, refreshed
/// in the background for `Stale` and `Every`. `load` starts the Lua
/// providers; a `Cached` read never calls it. `Err` names why nothing
/// could be read; a failed refresh is a notice, and the cached list stays.
pub(crate) fn read(
    home: &Path,
    workspace: &Path,
    refresh: tui::Refresh,
    now: SystemTime,
    load: &dyn Fn(&Config) -> SessionExtensions,
) -> Result<tui::Catalogue, String> {
    let (providers, config, notices) = cli::model_lists(home, workspace, mode(refresh), load)
        .map_err(|error| error.to_string())?;
    Ok(tui::Catalogue {
        models: entries(&providers, &config),
        // The ages read after the refresh above ran, so a refreshed
        // list reads fresh.
        lists: ages(home, &providers, &config, now),
        notices,
    })
}

/// The [`cli::ModelLists`] read behind each [`tui::Refresh`]: `Cached`
/// serves the cached lists, `Stale` refreshes with the age check, `Every`
/// without.
fn mode(refresh: tui::Refresh) -> cli::ModelLists {
    match refresh {
        tui::Refresh::Cached => cli::ModelLists::Cached,
        tui::Refresh::Stale => cli::ModelLists::Refresh { check_age: true },
        tui::Refresh::Every => cli::ModelLists::Refresh { check_age: false },
    }
}

/// One list time per installed provider: when its cached copy was
/// written, and whether it is stale. A file dated after `now` reads as
/// updated `now`, age zero; a read error reads as no cached copy.
fn ages(home: &Path, providers: &Providers, config: &Config, now: SystemTime) -> Vec<tui::ListAge> {
    let after = config::refresh_after(config);
    let now_ms = contract::clock::wall_ms(now);
    providers
        .names()
        .map(|name| {
            let age = config::model_cache_age(home, name, now).ok().flatten();
            let updated_ms = age.map(|age| {
                now_ms.saturating_sub(u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
            });
            tui::ListAge {
                provider: name.to_owned(),
                updated_ms,
                stale: age.is_none_or(|age| age > after),
            }
        })
        .collect()
}

/// A model's rebuild price from its provider data: the `cache_write`
/// price, or `input` when it names none, with each tier's own the same
/// way (a zero tier `cache_write` names no price, so `input` stands in).
/// `None` when the model names no cost. Prices are integer micro-dollars
/// per million tokens.
fn price(cost: Option<&config::Cost>) -> Option<tui::Price> {
    let cost = cost?;
    let mut tiers: Vec<(u64, u64)> = cost
        .tiers
        .iter()
        .map(|tier| {
            let dollars = if tier.cache_write > 0.0 {
                tier.cache_write
            } else {
                tier.input
            };
            (tier.input_tokens_above, micros(dollars))
        })
        .collect();
    tiers.sort();
    Some(tui::Price {
        micros_per_mtok: micros(cost.cache_write.unwrap_or(cost.input)),
        tiers,
    })
}

/// US dollars per million tokens as integer micro-dollars.
fn micros(dollars: f64) -> u64 {
    // The clamp keeps the cast inside the range: a price is neither
    // negative nor near `u64::MAX` dollars per million tokens.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped into the u64 range just above"
    )]
    let micros = (dollars * 1_000_000.0).round().clamp(0.0, u64::MAX as f64) as u64;
    micros
}
/// One entry per model of every installed provider, providers sorted by
/// name, models in list order.
fn entries(providers: &Providers, config: &Config) -> Vec<tui::ModelEntry> {
    let mut entries = Vec::new();
    for name in providers.names() {
        let Some(data) = providers.get(name) else {
            continue;
        };
        for model in &data.models {
            let reference = format!("{name}/{}", model.id);
            entries.push(tui::ModelEntry {
                reference: reference.clone(),
                provider: name.to_owned(),
                id: model.id.clone(),
                // The provider data names no display name yet: the
                // picker's name match stays dormant until one exists.
                name: None,
                levels: model
                    .thinking_levels
                    .iter()
                    .map(|level| level.as_str().to_owned())
                    .collect(),
                default_level: model
                    .thinking_default
                    .map(|level| level.as_str().to_owned()),
                configured: config
                    .get("thinking", Some(&reference))
                    .and_then(|(value, _)| value.as_str().map(str::to_owned)),
                roles: cli::roles_of(config, &reference),
                price: price(model.cost.as_ref()),
            });
        }
    }
    entries
}

#[cfg(test)]
#[path = "model_list_tests.rs"]
mod tests;
