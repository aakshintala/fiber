//! `cache.lifetime` from configuration reaches the preamble's lifetime.

use super::cache_lifetime;
use contract::events::CacheLifetime;

const MODEL: &str = "openai/gpt-5.6";

fn config(overrides: &[&str]) -> config::Config {
    let root = fakes::TempDir::new("fiber-cache-lifetime-settings");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    config::Config::load(config::Sources {
        home,
        workspace,
        project: config::ProjectKey::new("test").unwrap(),
        overrides: overrides.iter().map(|o| (*o).to_owned()).collect(),
    })
    .unwrap()
}

#[test]
fn absent_is_the_one_hour_default() {
    assert_eq!(cache_lifetime(&config(&[]), MODEL), CacheLifetime::OneHour);
}

#[test]
fn top_level_five_minutes_is_five_minutes() {
    let config = config(&["cache.lifetime=5m"]);
    assert_eq!(cache_lifetime(&config, MODEL), CacheLifetime::FiveMinutes);
}

#[test]
fn top_level_one_hour_is_one_hour() {
    let config = config(&["cache.lifetime=1h"]);
    assert_eq!(cache_lifetime(&config, MODEL), CacheLifetime::OneHour);
}

#[test]
fn a_per_model_key_wins_for_that_model_only() {
    let overrides = [
        "cache.lifetime=1h",
        "models.\"openai/gpt-5.6\".cache.lifetime=5m",
    ];
    let config = config(&overrides);

    assert_eq!(cache_lifetime(&config, MODEL), CacheLifetime::FiveMinutes);

    let other = cache_lifetime(&config, "anthropic/claude");
    assert_eq!(other, CacheLifetime::OneHour);
}

#[test]
fn a_per_model_key_for_another_model_leaves_this_one_at_the_top_level() {
    let overrides = [
        "cache.lifetime=1h",
        "models.\"anthropic/claude\".cache.lifetime=5m",
    ];
    let config = config(&overrides);

    assert_eq!(cache_lifetime(&config, MODEL), CacheLifetime::OneHour);
}
