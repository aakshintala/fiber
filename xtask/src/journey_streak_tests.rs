use serde_json::Value;

use super::{producer_job, select, streaks};

#[test]
fn selector_filters_and_orders_mixed_runs() {
    let runs: Value = serde_json::from_str(
        r#"[
      {"databaseId":3,"event":"push","headBranch":"main","conclusion":"failure"},
      {"databaseId":9,"event":"pull_request","headBranch":"main","conclusion":"failure"},
      {"databaseId":5,"event":"schedule","headBranch":"main","conclusion":"success"},
      {"databaseId":7,"event":"push","headBranch":"other","conclusion":"success"},
      {"databaseId":4,"event":"workflow_dispatch","headBranch":"main","conclusion":"failure"}
    ]"#,
    )
    .unwrap();
    assert_eq!(select(&runs), ["5", "3"]);
}

#[test]
fn current_run_counts_once_and_threshold_and_first_sha_are_exact() {
    let current_sha = "0123456789abcdef0123456789abcdef01234567";
    let prior_sha = "abcdef0123456789abcdef0123456789abcdef01";
    assert_eq!(
        streaks(current_sha, "qa\n", &[])["qa"],
        (1, current_sha.to_owned())
    );
    assert_eq!(
        streaks(
            current_sha,
            "qa\n",
            &[(prior_sha.into(), Some("qa\n".into()), true)]
        )["qa"],
        (2, prior_sha.to_owned())
    );
}

#[test]
fn pass_or_missing_artifact_resets_each_journey() {
    let current_sha = "0123456789abcdef0123456789abcdef01234567";
    let prior_sha = "abcdef0123456789abcdef0123456789abcdef01";
    let history = [
        (prior_sha.into(), Some("qa\n".into()), true),
        (prior_sha.into(), Some("qb\n".into()), true),
    ];
    let results = streaks(current_sha, "qa\nqb\n", &history);
    assert_eq!(results["qa"], (2, prior_sha.to_owned()));
    assert_eq!(results["qb"], (1, current_sha.to_owned()));
    assert_eq!(
        streaks(current_sha, "qa\n", &[(prior_sha.into(), None, true)])["qa"],
        (1, current_sha.to_owned())
    );
}

#[test]
fn producer_lookup_selects_journey_suite_or_absence() {
    let view: Value = serde_json::json!({"jobs":[{"name":"Run the journey suite against the built binary","conclusion":"failure"}]});
    assert_eq!(
        producer_job(&view)
            .and_then(|job| job.get("conclusion"))
            .and_then(Value::as_str),
        Some("failure")
    );
    assert!(producer_job(&serde_json::json!({"jobs":[]})).is_none());
}

#[test]
fn skipped_producer_is_ignored() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        streaks(sha, "qa\n", &[(sha.into(), None, false)])["qa"],
        (1, sha.to_owned())
    );
}
