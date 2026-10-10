//! Tests for the model picker's filter: each word of the query must match,
//! in order but not necessarily next to each other and ignoring case, the
//! model's provider, id or display name.

use super::matches;
use crate::catalogue::ModelEntry;

fn entry(provider: &str, id: &str, name: Option<&str>) -> ModelEntry {
    ModelEntry {
        reference: format!("{provider}/{id}"),
        provider: provider.to_owned(),
        id: id.to_owned(),
        name: name.map(str::to_owned),
        levels: Vec::new(),
        default_level: None,
        configured: None,
        roles: Vec::new(),
    }
}

#[test]
fn an_empty_query_matches_everything() {
    let entry = entry("openai", "gpt-5", None);
    assert!(matches(&entry, ""));
}

#[test]
fn a_whitespace_only_query_matches_everything() {
    let entry = entry("openai", "gpt-5", None);
    assert!(matches(&entry, "   "));
}

#[test]
fn an_in_order_subsequence_of_the_id_matches() {
    let entry = entry("openai", "gpt-5", None);
    assert!(matches(&entry, "gp5"));
}

#[test]
fn an_out_of_order_word_does_not_match() {
    let entry = entry("openai", "gpt-5", None);
    assert!(!matches(&entry, "tg"));
}

#[test]
fn case_is_ignored_both_ways() {
    let lower = entry("openai", "gpt", None);
    assert!(matches(&lower, "GPT"));
    let upper = entry("openai", "GPT-5", None);
    assert!(matches(&upper, "gpt"));
}

#[test]
fn a_word_in_the_provider_only_matches() {
    let entry = entry("openai", "zzz", None);
    assert!(matches(&entry, "oai"));
}

#[test]
fn a_word_in_the_name_only_matches() {
    let entry = entry("qqq", "zzz", Some("Claude Opus"));
    assert!(matches(&entry, "opus"));
}

#[test]
fn a_missing_name_never_matches() {
    let entry = entry("qqq", "zzz", None);
    assert!(!matches(&entry, "opus"));
}

#[test]
fn a_name_matches_on_its_own() {
    // The provider and the id match nothing: the name alone carries it.
    let entry = entry("qqq", "zzz", Some("Claude Opus"));
    assert!(matches(&entry, "opus cl"));
}

#[test]
fn two_words_must_both_match() {
    let entry = entry("openai", "gpt-5", None);
    assert!(matches(&entry, "op g5"));
    assert!(!matches(&entry, "op zzz"));
}

#[test]
fn a_word_spanning_two_fields_does_not_match() {
    // "o" ends the provider and "g" starts the id: together they match
    // neither field alone.
    let entry = entry("openai", "gpt-5", None);
    assert!(!matches(&entry, "og"));
}

#[test]
fn repeated_letters_must_advance() {
    let one = entry("p", "ab", None);
    assert!(!matches(&one, "aa"));
    let two = entry("p", "aba", None);
    assert!(matches(&two, "aa"));
}

#[test]
fn non_ascii_case_folds() {
    let entry = entry("p", "Éclair", None);
    assert!(matches(&entry, "é"));
}

