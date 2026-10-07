//! [`super::read_selection`], [`super::dropped_oldest`] and
//! [`super::fallback_notice`], table-driven.

use super::{dropped_oldest, fallback_notice, read_selection};

#[test]
fn read_selection_reads_the_listed_numbers() {
    for (text, listed, expected) in [
        ("1, 3", 3, Some(vec![1, 3])),
        ("3 1 3", 3, Some(vec![1, 3])),
        ("0, 1", 3, Some(vec![1])),
        ("4", 3, None),
        ("3", 3, Some(vec![3])),
        ("none", 2, Some(vec![])),
        ("None.", 2, Some(vec![])),
        ("`none`", 2, Some(vec![])),
        ("none of them, though 2 matters", 2, Some(vec![])),
        ("Keep 2 because the person said so", 2, Some(vec![2])),
        ("keep all", 2, None),
        ("", 2, None),
        ("99999999999999999999999, 2", 2, Some(vec![2])),
    ] {
        assert_eq!(read_selection(text, listed).ok(), expected, "{text:?}");
    }
}

#[test]
fn read_selection_errors_never_quote_the_reply() {
    for text in ["keep all", "4", "0", "maybe ZQX-UNREAD"] {
        let Err(error) = read_selection(text, 3) else {
            panic!("{text:?} read");
        };
        assert!(!error.contains(text), "{error:?} quotes {text:?}");
        assert!(!error.contains("ZQX-UNREAD"), "{error:?} quotes {text:?}");
    }
}

#[test]
fn fallback_notice_says_what_was_kept_and_why() {
    let kept = fallback_notice("rate limited", 0);
    assert!(kept.contains("every one"), "{kept}");
    assert!(!kept.contains("oldest"), "{kept}");
    assert!(kept.contains("rate limited"), "{kept}");

    let capped = fallback_notice("rate limited", 2);
    assert!(capped.contains("the 2 oldest"), "{capped}");
    assert!(capped.contains("context window"), "{capped}");
    assert!(!capped.contains("every one"), "{capped}");
    assert!(capped.contains("rate limited"), "{capped}");

    let one = fallback_notice("the request was cancelled", 1);
    assert!(one.contains("oldest message"), "{one}");
    assert!(!one.contains("every one"), "{one}");
}

#[test]
fn dropped_oldest_drops_only_past_the_window() {
    for (sizes, window, expected) in [
        (vec![5, 1, 1], None, 0),
        (vec![5, 1, 1], Some(0), 0),
        (vec![1, 1], Some(2), 0),
        (vec![1, 1, 1], Some(2), 1),
        (vec![5], Some(2), 1),
        (vec![5, 1, 1], Some(2), 1),
    ] {
        assert_eq!(dropped_oldest(&sizes, window), expected, "{sizes:?}");
    }
}
