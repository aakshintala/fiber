//! The typed `Config` accessors read the effective value: the set value,
//! the built-in default, or `None`/empty for another type or a missing key
//! without a default. A per-model key wins over the top level for that model.

mod common;

use common::Setup;

#[test]
fn count_reads_a_set_value_the_default_and_none_for_another_type() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.count("session.idle_exit_ms", None), Some(1_800_000));
    let set = setup.load(&["session.idle_exit_ms=5000"]).unwrap();
    assert_eq!(set.count("session.idle_exit_ms", None), Some(5000));
    // A bool is not a count.
    assert_eq!(config.count("tui.hover", None), None);
    // A fractional number is not a count.
    let fractional = setup.load(&["quota.notice_at=0.5"]).unwrap();
    assert_eq!(fractional.count("quota.notice_at", None), None);
    // `model` has no default and is unset.
    assert_eq!(config.count("model", None), None);
}

#[test]
fn flag_reads_a_set_value_and_the_default() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.flag("tui.hover", None), Some(true));
    let set = setup.load(&["tui.hover=false"]).unwrap();
    assert_eq!(set.flag("tui.hover", None), Some(false));
    // A count is not a flag.
    assert_eq!(config.flag("handoff.tokens", None), None);
}

#[test]
fn text_reads_a_string_and_none_when_unset() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.text("model", None), None);
    let set = setup.load(&["model=a/b"]).unwrap();
    assert_eq!(set.text("model", None), Some("a/b".to_owned()));
    assert_eq!(config.text("tui.logo_glyph", None), Some("⌇".to_owned()));
    // A bool is not text.
    assert_eq!(config.text("tui.hover", None), None);
}

#[test]
fn list_reads_strings_and_is_empty_when_absent() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert!(config.list("hub.allowed_origins", None).is_empty());
    let set = setup.load(&["mcp.servers.gh.args=[\"a\",\"b\"]"]).unwrap();
    assert_eq!(set.list("mcp.servers.gh.args", None), ["a", "b"]);
    // A string is not an array.
    let model = setup.load(&["model=a/b"]).unwrap();
    assert!(model.list("model", None).is_empty());
}

#[test]
fn accessors_read_the_per_model_value_over_the_top_level() {
    let setup = Setup::new();
    let config = setup
        .load(&[
            "handoff.tokens=300000",
            "models.\"a/b\".handoff.tokens=200000",
        ])
        .unwrap();
    assert_eq!(config.count("handoff.tokens", Some("a/b")), Some(200_000));
    assert_eq!(config.count("handoff.tokens", Some("c/d")), Some(300_000));
    assert_eq!(config.count("handoff.tokens", None), Some(300_000));
}
