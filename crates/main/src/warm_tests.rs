//! `cache.warm_idle` and `cache.warm_cap` reach the loop's warming cap.

use super::warm;

fn config(overrides: &[&str]) -> config::Config {
    let root = fakes::TempDir::new("fiber-warm-settings");
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
fn warming_is_off_by_default_whatever_the_cap() {
    assert_eq!(warm(&config(&[])), None);
    assert_eq!(warm(&config(&["cache.warm_cap=5"])), None);
    assert_eq!(warm(&config(&["cache.warm_idle=false"])), None);
}

#[test]
fn warming_on_takes_the_default_cap_of_two_lifetimes() {
    assert_eq!(warm(&config(&["cache.warm_idle=true"])), Some(2));
}

#[test]
fn warming_on_takes_a_set_cap() {
    let eleven = config(&["cache.warm_idle=true", "cache.warm_cap=11"]);
    assert_eq!(warm(&eleven), Some(11));
    let zero = config(&["cache.warm_idle=true", "cache.warm_cap=0"]);
    assert_eq!(warm(&zero), Some(0));
}
