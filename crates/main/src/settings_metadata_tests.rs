//! The schema and the `/settings` screen rows agree: a key with no `*` has
//! a tab unless a screen row covers it, and a row covers only real schema
//! entries (`docs/tui.md`, "Swapped views", `/settings`, "Tabs").

use config::SETTINGS_SCHEMA;
use tui::SCREEN_ROWS;

fn covered() -> Vec<&'static str> {
    SCREEN_ROWS
        .iter()
        .flat_map(|row| row.covers)
        .copied()
        .collect()
}

fn entry(path: &str) -> Option<&'static config::SettingsKey> {
    SETTINGS_SCHEMA.iter().find(|key| key.path == path)
}

#[test]
fn every_covered_path_is_a_real_schema_entry() {
    for path in covered() {
        let key = entry(path).unwrap_or_else(|| panic!("`{path}` is not a schema entry"));
        assert_eq!(key.metadata.tab, None, "`{path}` is covered, so has no tab");
    }
}

#[test]
fn plain_keys_have_a_tab_unless_a_screen_covers_them() {
    let covered = covered();
    for key in SETTINGS_SCHEMA {
        let path = key.path;
        let want_tab = path != "hub.port" && !path.contains('*') && !covered.contains(&path);
        assert_eq!(
            key.metadata.tab.is_some(),
            want_tab,
            "`{path}`: a tab exactly when it has no `*` and no screen row covers it"
        );
        assert!(
            !key.metadata.label.trim().is_empty(),
            "`{path}` has no label"
        );
        assert!(
            !key.metadata.description.trim().is_empty(),
            "`{path}` has no description"
        );
    }
    for row in SCREEN_ROWS {
        let label = row.metadata.label;
        assert!(row.metadata.tab.is_some(), "`{label}` has no tab");
        assert!(!label.trim().is_empty(), "a screen row has no label");
        assert!(
            !row.metadata.description.trim().is_empty(),
            "`{label}` has no description"
        );
    }
}

#[test]
fn screen_coverage_is_exact_and_disjoint() {
    // The families the screens stand for, read from the schema itself, so
    // a new tool, MCP server or extension tool-switch key that no row
    // covers fails here.
    let mut want: Vec<&str> = SETTINGS_SCHEMA
        .iter()
        .map(|key| key.path)
        .filter(|path| {
            path.starts_with("tools.*.")
                || path.starts_with("mcp.servers.*.")
                || path.starts_with("extensions.*.tools.")
                || ["keys.*", "providers.*.credential", "skills.disabled"].contains(path)
        })
        .collect();
    let mut got = covered();
    want.sort_unstable();
    got.sort_unstable();
    assert_eq!(got, want, "the schema entries the screen rows cover");

    let mut once = got.clone();
    once.dedup();
    assert_eq!(once, got, "two rows cover one entry");

    let rules = SCREEN_ROWS
        .iter()
        .find(|row| row.command == "/rules")
        .expect("a Rules row");
    assert!(rules.covers.is_empty(), "Rules covers no key");
    assert!(
        !got.contains(&"providers.*.credentials.*"),
        "a credential source is not the Credentials screen's key"
    );
}
