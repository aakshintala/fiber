use super::*;

#[test]
fn stable_parses_three_plain_components() {
    assert_eq!(parse_stable("v0.1.10"), Some([0, 1, 10]));
    assert_eq!(parse_stable("v0.1.0"), Some([0, 1, 0]));
}

#[test]
fn stable_rejects_suffixes_names_and_bad_numbers() {
    assert_eq!(parse_stable("v0.1.0-rc1"), None);
    assert_eq!(parse_stable("v0.1.0-nightly.20261023"), None);
    assert_eq!(parse_stable("docs-x"), None);
    assert_eq!(parse_stable("v01.2.3"), None);
    assert_eq!(parse_stable("v99999999999999999999.0.0"), None);
    assert_eq!(parse_stable("v0.1"), None);
    assert_eq!(parse_stable("v0.1.0.0"), None);
}

#[test]
fn nightly_parses_an_eight_digit_date() {
    assert_eq!(
        parse_nightly("v0.4.1-nightly.20261101"),
        Some(([0, 4, 1], 20261101))
    );
    assert_eq!(parse_nightly("v0.4.1-nightly.2026110"), None);
    assert_eq!(parse_nightly("v0.4.1-nightly.202611011"), None);
    assert_eq!(parse_nightly("v01.4.1-nightly.20261101"), None);
    assert_eq!(parse_nightly("v0.4.1"), None);
}

#[test]
fn a_numeric_tie_breaks_by_tag_name() {
    assert!(beats::<[u64; 3]>(Some((&[0, 1, 0], "a")), &[0, 1, 0], "b"));
    assert!(!beats::<[u64; 3]>(Some((&[0, 1, 0], "b")), &[0, 1, 0], "a"));
    assert!(!beats::<[u64; 3]>(Some((&[0, 1, 0], "a")), &[0, 1, 0], "a"));
}

#[test]
fn the_first_tag_has_nothing_to_beat() {
    assert!(beats::<[u64; 3]>(None, &[0, 1, 0], "v0.1.0"));
}

#[test]
fn select_is_empty_without_release_tags() {
    assert_eq!(select("docs-x\nv0.1.0-rc1\n"), (None, None));
}
