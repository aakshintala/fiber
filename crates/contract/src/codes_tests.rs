use std::collections::BTreeSet;

use super::*;

const ERRORS: &str = include_str!("../../../docs/errors.md");
const INVOCATION: &str = include_str!("../../../docs/invocation.md");

/// The backticked word opening each table row under a heading of
/// `docs/errors.md`, header rows aside.
fn table_codes(heading: &str) -> Vec<&'static str> {
    let section = ERRORS.split(heading).nth(1).unwrap();
    let section = section.split("\n#").next().unwrap();
    section
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .map(|rest| rest.split('`').next().unwrap())
        .collect()
}

/// The backticked words of a comma-separated list that starts after `lead`.
fn listed_codes(doc: &'static str, lead: &str, end: char) -> Vec<&'static str> {
    let list = doc.split(lead).nth(1).unwrap().split(end).next().unwrap();
    list.split('`').skip(1).step_by(2).collect()
}

/// Every code `docs/errors.md` and `docs/invocation.md` name.
fn doc_codes() -> BTreeSet<&'static str> {
    let mut codes = BTreeSet::new();
    for heading in [
        "### Before a session exists",
        "## A failed model call",
        "## Registry",
        "Notices, for a failure outside any action or a login a person must finish:",
    ] {
        let found = table_codes(heading);
        assert!(!found.is_empty(), "no codes under {heading}");
        codes.extend(found);
    }
    codes.extend(listed_codes(ERRORS, "Driver command rejections (", ')'));
    codes.extend(listed_codes(INVOCATION, "Rejection codes: ", '.'));
    codes
}

#[test]
fn the_known_codes_are_the_codes_in_the_docs() {
    let docs = doc_codes();
    for code in KNOWN {
        let json = serde_json::to_string(code).unwrap();
        assert!(docs.contains(json.trim_matches('"')), "{json} is in no doc");
        assert_eq!(&serde_json::from_str::<ErrorCode>(&json).unwrap(), code);
    }
    assert_eq!(KNOWN.len(), docs.len());
}

#[test]
fn a_rejection_list_in_one_doc_matches_the_other() {
    let errors: BTreeSet<_> = listed_codes(ERRORS, "Driver command rejections (", ')')
        .into_iter()
        .collect();
    let invocation: BTreeSet<_> = listed_codes(INVOCATION, "Rejection codes: ", '.')
        .into_iter()
        .collect();
    assert_eq!(errors, invocation);
}

#[test]
fn from_session_is_no_registry_row() {
    let registry = table_codes("## Registry");
    assert!(
        !registry.contains(&"from_session"),
        "from_session is a driver rejection only, not a Registry row"
    );
}

#[test]
fn an_unknown_code_is_kept_as_it_was_written() {
    let parsed: ErrorCode = serde_json::from_str("\"disk_on_fire\"").unwrap();
    assert_eq!(parsed, ErrorCode::Other("disk_on_fire".into()));
    assert_eq!(serde_json::to_string(&parsed).unwrap(), "\"disk_on_fire\"");
}

#[test]
fn names_match_their_serde_spellings() {
    for code in KNOWN {
        let json = serde_json::to_string(code).unwrap();
        assert_eq!(code.name(), json.trim_matches('"'), "{code:?}");
    }
}

#[test]
fn from_name_maps_every_declared_label_to_its_variant() {
    for code in KNOWN {
        let parsed = ErrorCode::from_name(code.name());
        assert_eq!(&parsed, code, "{:?}", code.name());
        assert!(
            !matches!(parsed, ErrorCode::Other(_)),
            "{:?} read as Other",
            code.name()
        );
    }
    assert_eq!(ErrorCode::from_name("usage"), ErrorCode::Usage);
}

#[test]
fn from_name_keeps_unknown_empty_and_non_ascii_text() {
    for label in [
        "disk_on_fire",
        "",
        "échec_inconnu",
        "CONNECTION_FAILED",
        "connection failed",
    ] {
        assert_eq!(
            ErrorCode::from_name(label),
            ErrorCode::Other(label.to_owned()),
            "{label:?}"
        );
        assert_eq!(ErrorCode::from_name(label).name(), label, "{label:?}");
    }
}

#[test]
fn names_round_trip_through_from_name() {
    for code in KNOWN {
        assert_eq!(ErrorCode::from_name(code.name()), code.clone());
    }
    let other = ErrorCode::Other("disk_on_fire".to_owned());
    assert_eq!(other.name(), "disk_on_fire");
    assert_eq!(ErrorCode::from_name(other.name()), other);
}

#[test]
fn every_known_name_matches_its_doc_table_label() {
    let docs = doc_codes();
    for code in KNOWN {
        assert!(docs.contains(code.name()), "{code:?} is in no doc");
    }
}
