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
        "Notices, for a failure outside any action:",
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
fn every_code_in_the_docs_is_known_and_keeps_its_string() {
    for code in doc_codes() {
        let json = format!("\"{code}\"");
        let parsed: ErrorCode = serde_json::from_str(&json).unwrap();
        assert!(KNOWN.contains(&parsed), "{code} is not a known code");
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
    }
}

#[test]
fn every_known_code_is_in_the_docs() {
    let docs = doc_codes();
    for code in KNOWN {
        let name = serde_json::to_value(code).unwrap();
        assert!(docs.contains(name.as_str().unwrap()), "{name} is in no doc");
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
fn an_unknown_code_is_kept_as_it_was_written() {
    let parsed: ErrorCode = serde_json::from_str("\"disk_on_fire\"").unwrap();
    assert_eq!(parsed, ErrorCode::Other("disk_on_fire".into()));
    assert_eq!(serde_json::to_string(&parsed).unwrap(), "\"disk_on_fire\"");
}
