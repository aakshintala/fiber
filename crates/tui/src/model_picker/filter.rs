//! The model picker's filter (`docs/tui.md`, "Swapped views"): each
//! space-separated word of the query must match, in order but not
//! necessarily next to each other and ignoring case, the model's
//! provider, id or display name. The filter keeps the list's order and
//! only drops rows.

use crate::catalogue::ModelEntry;

/// Whether two characters match ignoring case, compared one char at a
/// time, so hit indices always match the id's chars.
fn folds(left: char, right: char) -> bool {
    left.to_lowercase().eq(right.to_lowercase())
}

/// Whether `word` is an in-order subsequence of `field`, ignoring case.
fn subseq(field: &str, word: &str) -> bool {
    let mut chars = field.chars();
    word.chars().all(|wanted| chars.any(|got| folds(got, wanted)))
}

/// Whether `entry` matches `query`: every space-separated word is an
/// in-order, case-insensitive subsequence of the provider, the id, or
/// the display name when one is named. A word never spans two fields.
/// An empty or whitespace-only query matches everything.
pub(crate) fn matches(entry: &ModelEntry, query: &str) -> bool {
    query.split_whitespace().all(|word| {
        subseq(&entry.provider, word)
            || subseq(&entry.id, word)
            || entry
                .name
                .as_ref()
                .is_some_and(|name| subseq(name, word))
    })
}

/// One flag per char of `id`: each word that is an in-order subsequence
/// of the id, matched greedily from the left, marks the chars it took.
/// A word that matches nowhere contributes no partial hits; the flags
/// union across words.
pub(crate) fn id_hits(id: &str, query: &str) -> Vec<bool> {
    let chars: Vec<char> = id.chars().collect();
    let mut hits = vec![false; chars.len()];
    for word in query.split_whitespace() {
        let mut take: Vec<usize> = Vec::new();
        let mut at = 0;
        let mut matched = true;
        for wanted in word.chars() {
            let mut found = false;
            while let Some(got) = chars.get(at) {
                at += 1;
                if folds(*got, wanted) {
                    take.push(at - 1);
                    found = true;
                    break;
                }
            }
            if !found {
                matched = false;
                break;
            }
        }
        if matched {
            for index in take {
                if let Some(hit) = hits.get_mut(index) {
                    *hit = true;
                }
            }
        }
    }
    hits
}

#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;
