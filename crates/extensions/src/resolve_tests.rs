//! The case matrix of `docs/extensions.md`, "Versions".

use std::collections::BTreeMap;

use super::{newest, pick};
use crate::Error;

fn tags(list: &[&str]) -> Vec<String> {
    list.iter().map(|t| (*t).to_owned()).collect()
}

fn wants(list: &[(&str, &str)]) -> BTreeMap<String, String> {
    list.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

const ALL: &[&str] = &["v1.0.0", "v1.2.0", "v1.4.0", "v1.9.0", "v2.0.0", "v2.3.0"];

#[test]
fn the_lowest_version_meeting_every_minimum_wins() {
    let w = wants(&[("openrouter", "1.2"), ("databricks", "1.4")]);
    assert_eq!(pick("h", &w, &tags(ALL)).unwrap(), "v1.4.0");
}

#[test]
fn one_minimum_gives_the_lowest_tag_at_or_above_it() {
    for (min, got) in [
        ("1.2", "v1.2.0"),
        ("v1.2.0", "v1.2.0"),
        ("1.3", "v1.4.0"),
        ("1.9", "v1.9.0"),
    ] {
        let w = wants(&[("a", min)]);
        assert_eq!(pick("h", &w, &tags(ALL)).unwrap(), got, "{min}");
    }
    let w = wants(&[("a", "1.9.1")]);
    assert!(matches!(
        pick("h", &w, &tags(ALL)),
        Err(Error::NoVersion { .. })
    ));
}

#[test]
fn the_newest_tag_is_ignored_unless_a_minimum_asks_for_it() {
    let w = wants(&[("a", "1.0")]);
    assert_eq!(pick("h", &w, &tags(ALL)).unwrap(), "v1.0.0");
    let w = wants(&[("a", "1.9")]);
    assert_eq!(pick("h", &w, &tags(ALL)).unwrap(), "v1.9.0");
    let w = wants(&[("a", "2.3")]);
    assert_eq!(pick("h", &w, &tags(ALL)).unwrap(), "v2.3.0");
}

#[test]
fn the_same_inputs_give_the_same_result_in_any_order() {
    let w = wants(&[("a", "1.2"), ("b", "1.4")]);
    let mut shuffled = tags(ALL);
    shuffled.reverse();
    assert_eq!(
        pick("h", &w, &shuffled).unwrap(),
        pick("h", &w, &tags(ALL)).unwrap()
    );
    // `1.4.0` and `v1.4.0` are one version: the tag text breaks the tie.
    let both = tags(&["v1.4.0", "1.4.0"]);
    let w = wants(&[("a", "1.4")]);
    assert_eq!(pick("h", &w, &both).unwrap(), "1.4.0");
}

#[test]
fn two_majors_stop_naming_both() {
    let w = wants(&[("openrouter", "1.2"), ("databricks", "2.0")]);
    let err = pick("oauth-helper", &w, &tags(ALL)).unwrap_err();
    let text = err.to_string();
    for part in ["oauth-helper", "openrouter", "databricks", "1.2", "2.0"] {
        assert!(text.contains(part), "{text}");
    }
    assert!(matches!(err, Error::MajorConflict { .. }));
}

#[test]
fn a_minimum_no_tag_meets_names_the_dependency_and_the_highest_minimum() {
    let w = wants(&[("a", "1.2"), ("b", "1.10")]);
    let err = pick("h", &w, &tags(ALL)).unwrap_err();
    assert!(
        matches!(&err, Error::NoVersion { needs, .. } if needs == "1.10"),
        "{err}"
    );
    let err = pick("h", &w, &[]).unwrap_err();
    assert!(matches!(err, Error::NoVersion { .. }));
}

#[test]
fn tags_that_are_not_versions_are_ignored() {
    let w = wants(&[("a", "1.0")]);
    let t = tags(&["latest", "v1.0", "v1.0.0-rc1", "v1.1.0"]);
    assert_eq!(pick("h", &w, &t).unwrap(), "v1.1.0");
}

#[test]
fn a_minimum_that_is_not_a_version_is_refused() {
    for bad in ["", "x", "1.2.3.4", "v", "1..2"] {
        let w = wants(&[("a", bad)]);
        assert!(
            matches!(pick("h", &w, &tags(ALL)), Err(Error::BadVersion { .. })),
            "{bad}"
        );
    }
}

#[test]
fn newest_is_the_highest_version_tag() {
    assert_eq!(newest(&tags(ALL)).unwrap(), "v2.3.0");
    assert_eq!(newest(&tags(&["v1.10.0", "v1.9.0"])).unwrap(), "v1.10.0");
    assert_eq!(newest(&tags(&["latest"])), None);
}
