//! The case matrix of `docs/extensions.md`, "Versions".

use std::collections::BTreeMap;

use super::{meets, newest, newest_within, pick, root_meets};
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

#[test]
fn newest_within_accepted_major_skips_a_newer_major() {
    let all = tags(&["v1.2.0", "v1.5.0", "v2.0.0"]);
    // At, just below and just above the edge of major 1's newest tag.
    for (min, got) in [
        ("1.2", "v1.5.0"),
        ("1.5", "v1.5.0"),
        ("1.4", "v1.5.0"),
        ("2.0", "v2.0.0"),
    ] {
        let w = wants(&[("a", min)]);
        assert_eq!(newest_within("d", &w, &all).unwrap(), got, "{min}");
    }
    // Above the newest tag of the accepted major: no version qualifies.
    for min in ["1.6", "1.5.1"] {
        let w = wants(&[("a", min)]);
        assert!(
            matches!(newest_within("d", &w, &all), Err(Error::NoVersion { .. })),
            "{min}"
        );
    }
}

#[test]
fn newest_within_with_two_dependents_takes_the_newest_of_their_major() {
    let all = tags(&["v1.2.0", "v1.5.0", "v2.0.0"]);
    let w = wants(&[("a", "1.2"), ("b", "1.4")]);
    assert_eq!(newest_within("d", &w, &all).unwrap(), "v1.5.0");
}

#[test]
fn newest_within_with_two_majors_stops_naming_both() {
    let all = tags(&["v1.2.0", "v1.5.0", "v2.0.0"]);
    let w = wants(&[("a", "1.2"), ("b", "2.0")]);
    let err = newest_within("d", &w, &all).unwrap_err();
    let text = err.to_string();
    for part in ["d", "a", "b", "1.2", "2.0"] {
        assert!(text.contains(part), "{text}");
    }
    assert!(matches!(err, Error::MajorConflict { .. }));
}

#[test]
fn newest_within_without_constraints_is_the_newest_tag() {
    let all = tags(&["v1.2.0", "v1.5.0", "v2.0.0"]);
    assert_eq!(newest_within("d", &wants(&[]), &all).unwrap(), "v2.0.0");
    assert!(matches!(
        newest_within("d", &wants(&[]), &tags(&["latest"])),
        Err(Error::NoTag { .. })
    ));
}

#[test]
fn newest_within_is_deterministic_in_any_order() {
    let w = wants(&[("a", "1.2")]);
    let mut shuffled = tags(&["v1.2.0", "v1.5.0", "v2.0.0"]);
    shuffled.reverse();
    assert_eq!(
        newest_within("d", &w, &shuffled).unwrap(),
        newest_within("d", &w, &tags(&["v1.2.0", "v1.5.0", "v2.0.0"])).unwrap()
    );
    // `1.4.0` and `v1.4.0` are one version: either order picks the same tag.
    let both = tags(&["v1.4.0", "1.4.0"]);
    let mut swapped = both.clone();
    swapped.reverse();
    let w = wants(&[("a", "1.4")]);
    assert_eq!(
        newest_within("d", &w, &both).unwrap(),
        newest_within("d", &w, &swapped).unwrap()
    );
}

#[test]
fn nightly_parts_splits_the_base_and_the_date() {
    use super::nightly_parts;
    assert_eq!(
        nightly_parts("v0.3.0-nightly.20240101").unwrap(),
        ([0, 3, 0], "20240101".to_owned())
    );
    assert_eq!(
        nightly_parts("1.2.3-nightly.19991231").unwrap(),
        ([1, 2, 3], "19991231".to_owned())
    );
}

#[test]
fn nightly_parts_rejects_anything_not_shaped_like_a_nightly() {
    use super::nightly_parts;
    for bad in [
        "v0.3.0",
        "v0.3.0-nightly.2024010",
        "v0.3.0-nightly.202401011",
        "v0.3.0-nightly.2024abcd",
        "v0.3.0-nightly.",
        "v0.3-nightly.20240101",
        "v0.3.0.1-nightly.20240101",
        "x.y.z-nightly.20240101",
        "",
    ] {
        assert!(
            matches!(nightly_parts(bad), Err(Error::BadVersion { .. })),
            "{bad}"
        );
    }
}

#[test]
fn version_rejects_what_is_not_three_numbers() {
    use super::version;
    for bad in ["", "1.2", "1.2.3.4", "a.b.c", "1.2.x", "v1.2.3 "] {
        assert!(
            matches!(version(bad), Err(Error::BadVersion { .. })),
            "{bad}"
        );
    }
    assert_eq!(version("v1.2.3").unwrap(), [1, 2, 3]);
}

#[test]
fn meets_orders_stable_and_nightly_forms() {
    // docs/releasing.md, "A nightly pre-release": SemVer sorts a nightly
    // below its base stable, and the fixed-width YYYYMMDD dates order two
    // nightlies on one base chronologically.
    use super::at_least;
    let cases: [(&str, &str, bool); 8] = [
        ("0.3.0", "0.3.0", true),
        ("0.2.0", "0.3.0", true),
        ("0.3.0", "0.2.0", false),
        ("0.3.0-nightly.20240101", "0.3.0", true),
        ("0.3.0", "0.3.0-nightly.20240101", false),
        ("0.3.0-nightly.20240101", "0.3.0-nightly.20240201", true),
        ("0.3.0-nightly.20240201", "0.3.0-nightly.20240101", false),
        ("0.3.0-nightly.20240101", "0.3.0-nightly.20240101", true),
    ];
    for (need, have, want) in cases {
        assert_eq!(at_least(need, have).unwrap(), want, "{need} in {have}");
    }
    assert!(matches!(
        at_least("bogus", "0.3.0"),
        Err(Error::BadVersion { .. })
    ));
    assert!(matches!(
        at_least("0.3.0", "bogus"),
        Err(Error::BadVersion { .. })
    ));
}

#[test]
fn newest_within_accepts_a_nightly_minimum_where_its_base_passes() {
    // docs/releasing.md, "A nightly pre-release": a nightly minimum orders
    // with its base triple, below the same-base stable release.
    let all = tags(&["v1.2.0", "v1.5.0", "v2.0.0"]);
    // A nightly minimum orders with its base triple: the same-base
    // stable tag still wins, below the base nothing qualifies.
    for (min, got) in [
        ("1.4.0-nightly.20240101", "v1.5.0"),
        ("1.5.0-nightly.20240101", "v1.5.0"),
        ("1.2.0-nightly.20240101", "v1.5.0"),
    ] {
        let w = wants(&[("a", min)]);
        assert_eq!(newest_within("d", &w, &all).unwrap(), got, "{min}");
    }
    // At, just below and just above the edge of the accepted major: a
    // nightly below the newest tag still takes it, one above takes none.
    for min in ["1.6.0-nightly.20240101", "1.5.1-nightly.20240101"] {
        let w = wants(&[("a", min)]);
        assert!(
            matches!(newest_within("d", &w, &all), Err(Error::NoVersion { .. })),
            "{min}"
        );
    }
    // A malformed minimum, including a malformed nightly shape, is
    // refused rather than accepted or skipped.
    for bad in [
        "",
        "x",
        "1.2.3.4",
        "1.2.0-nightly.2024010",
        "1.2.0-nightly.202401011",
    ] {
        let w = wants(&[("a", bad)]);
        assert!(
            matches!(newest_within("d", &w, &all), Err(Error::BadVersion { .. })),
            "{bad}"
        );
    }
}

#[test]
fn meets_orders_a_nightly_have_below_its_base_stable() {
    // docs/releasing.md, "A nightly pre-release": a nightly sorts below
    // the stable release on its base, and two nightlies on one base order
    // by date; the floor's base decides.
    let cases: [(&str, &str, bool); 9] = [
        ("0.3.0", "0.3.0", true),
        ("0.3.0", "0.3.0-nightly.20240101", false),
        ("0.3.0-nightly.20240101", "0.3.0", true),
        ("0.3.0-nightly.20240101", "0.3.0-nightly.20240201", true),
        ("0.3.0-nightly.20240201", "0.3.0-nightly.20240101", false),
        ("0.3.0-nightly.20240101", "0.3.0-nightly.20240101", true),
        ("0.2.0", "0.3.0-nightly.20240101", true),
        ("0.3.0", "0.2.0-nightly.20240101", false),
        ("1.0.0", "2.0.0-nightly.20240101", false),
    ];
    for (floor, have, want) in cases {
        let w = wants(&[("a", floor)]);
        assert_eq!(meets("h", have, &w).unwrap(), want, "{floor} needs {have}");
    }
    // A malformed have, including a malformed nightly shape, stays a
    // miss rather than an error, so the update restages it.
    for bad in ["bogus", "0.3.0-nightly.2024010"] {
        let w = wants(&[("a", "0.3.0")]);
        assert!(!meets("h", bad, &w).unwrap(), "{bad}");
    }
    // A malformed minimum, including a malformed nightly shape, is
    // refused rather than accepted.
    for bad in ["bogus", "0.3.0-nightly.2024010"] {
        let w = wants(&[("a", bad)]);
        assert!(
            matches!(meets("h", "0.3.0", &w), Err(Error::BadVersion { .. })),
            "{bad}"
        );
    }
}

#[test]
fn an_empty_floor_keeps_a_zero_base_nightly_minimum() {
    // The empty floor sorts below every valid minimum, including
    // `0.0.0-nightly.YYYYMMDD` (docs/releasing.md, "A nightly
    // pre-release"): an equal nightly `have` meets its own minimum
    // instead of losing to the sentinel. Flipping the sentinel's `false`
    // back to `true` fails the first two assertions.
    let w = wants(&[("a", "0.0.0-nightly.20240101")]);
    assert!(meets("h", "0.0.0-nightly.20240101", &w).unwrap());
    root_meets("h", "0.0.0-nightly.20240101", &w).unwrap();
    // With no minima at all, a nightly `have` still meets.
    assert!(meets("h", "0.0.0-nightly.20240101", &wants(&[])).unwrap());
}

#[test]
fn root_meets_accepts_a_nightly_release_and_refuses_a_malformed_one() {
    // docs/releasing.md, "A nightly pre-release": the nightly install's
    // own version meets its dependents' minima.
    let w = wants(&[("a", "0.2.0")]);
    root_meets("h", "0.3.0-nightly.20240101", &w).unwrap();
    // A malformed root release, including a malformed nightly shape,
    // is refused through the minimum parser.
    for bad in ["bogus", "0.3.0-nightly.2024010"] {
        let w = wants(&[("a", "0.2.0")]);
        assert!(
            matches!(root_meets("h", bad, &w), Err(Error::BadVersion { .. })),
            "{bad}"
        );
    }
}
