//! `Usage`'s default.

use super::*;

#[test]
fn protected_paths_are_empty_only_without_restored_or_failed_paths() {
    let mut report = ProtectedPaths {
        restored: Vec::new(),
        failed: Vec::new(),
    };
    assert!(report.is_empty(), "no changed paths means no report");

    report.restored.push(".fiber".to_owned());
    assert!(!report.is_empty(), "a restored path must be reported");

    report.failed.push(ProtectedPathFailure {
        path: ".git/config".to_owned(),
        message: "denied".to_owned(),
    });
    assert!(
        !report.is_empty(),
        "restored and failed paths must be reported"
    );

    report.restored.clear();
    assert!(!report.is_empty(), "a failed-only restore must be reported");
}

#[test]
fn protected_warning_lines_name_each_path_once() {
    let report = ProtectedPaths {
        restored: vec![".fiber".to_owned()],
        failed: vec![ProtectedPathFailure {
            path: ".git/config".to_owned(),
            message: "denied".to_owned(),
        }],
    };
    assert_eq!(
        report.warning_lines(),
        vec![
            "Protected path .fiber changed while a confined call ran; Fiber restored it."
                .to_owned(),
            "Protected path .git/config changed while a confined call ran; Fiber could not restore it: denied."
                .to_owned(),
        ]
    );
    assert!(
        ProtectedPaths {
            restored: Vec::new(),
            failed: Vec::new(),
        }
        .warning_lines()
        .is_empty(),
        "no change warns nothing"
    );
}

#[test]
fn default_usage_is_zero_counts_with_a_known_zero_cost() {
    // Field by field: a derived `Default` would leave `cost` as
    // `None`, an unknown cost, instead of a known `0`.
    let usage = Usage::default();
    assert_eq!(usage.tokens.input, 0);
    assert_eq!(usage.tokens.cache_read, 0);
    assert_eq!(usage.tokens.cache_write, BTreeMap::new());
    assert_eq!(usage.tokens.output, 0);
    assert_eq!(usage.cost, Some(0.0));
    assert_eq!(usage.subscription_cost, 0.0);
}
