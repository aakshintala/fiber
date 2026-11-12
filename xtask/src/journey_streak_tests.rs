use serde_json::Value;

use fakes::TempDir;

use super::{PriorRun, compute, producer_job, select, streaks};

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
    // `docs/ci.md`, "The journey suite" promises newest-first order.
    assert_eq!(select(&runs), ["5", "3"]);
}

#[test]
fn selector_caps_prior_candidates_at_twenty() {
    let runs = Value::Array(
        (1..=21)
            .rev()
            .map(|id| {
                serde_json::json!({
                    "databaseId": id,
                    "headSha": format!("{id:040x}"),
                    "event": "push",
                    "headBranch": "main",
                    "conclusion": "failure"
                })
            })
            .collect(),
    );
    // `docs/ci.md`, "The journey suite" promises a 20-run cap and newest-first order.
    let selected = select(&runs);
    assert_eq!(selected.len(), 20);
    assert_eq!(selected.first().map(String::as_str), Some("21"));
    assert_eq!(selected.last().map(String::as_str), Some("2"));
}

#[test]
fn current_run_counts_once_and_threshold_and_first_sha_are_exact() {
    let current_sha = "0123456789abcdef0123456789abcdef01234567";
    let prior_sha = "abcdef0123456789abcdef0123456789abcdef01";
    assert_eq!(
        streaks(current_sha, "qa\n", &[])["qa"],
        (1, current_sha.to_owned())
    );
    // `docs/ci.md`, "The journey suite" promises a threshold of two and the first failing SHA.
    assert_eq!(
        streaks(
            current_sha,
            "qa\n",
            &[PriorRun {
                sha: prior_sha.into(),
                failures: Some("qa\n".into())
            }]
        )["qa"],
        (2, prior_sha.to_owned())
    );
}

#[test]
fn pass_or_missing_artifact_resets_each_journey() {
    let current_sha = "0123456789abcdef0123456789abcdef01234567";
    let prior_sha = "abcdef0123456789abcdef0123456789abcdef01";
    let history = [
        PriorRun {
            sha: prior_sha.into(),
            failures: Some("qa\n".into()),
        },
        PriorRun {
            sha: prior_sha.into(),
            failures: Some("qb\n".into()),
        },
    ];
    let results = streaks(current_sha, "qa\nqb\n", &history);
    assert_eq!(results["qa"], (2, prior_sha.to_owned()));
    assert_eq!(results["qb"], (1, current_sha.to_owned()));
    assert_eq!(
        streaks(
            current_sha,
            "qa\n",
            &[PriorRun {
                sha: prior_sha.into(),
                failures: None
            }]
        )["qa"],
        (1, current_sha.to_owned())
    );
}

#[test]
fn producer_lookup_selects_journey_suite_or_absence() {
    let view: Value = serde_json::json!({"jobs":[{"name":"Journey suite on the release binary","conclusion":"failure"}]});
    assert_eq!(
        producer_job(&view)
            .and_then(|job| job.get("conclusion"))
            .and_then(Value::as_str),
        Some("failure")
    );
    let nightly: Value =
        serde_json::json!({"jobs":[{"name":"Nightly publish","conclusion":"success"}]});
    assert_eq!(
        producer_job(&nightly)
            .and_then(|job| job.get("conclusion"))
            .and_then(Value::as_str),
        Some("success")
    );
    assert!(producer_job(&serde_json::json!({"jobs":[]})).is_none());
}

#[test]
fn failed_history_view_lookup_leaves_no_companion() {
    for view in [None, Some("not json")] {
        let dir = TempDir::new("journey-streak-history-failure");
        dir.write("failures.txt", "qa\\n");
        dir.write(
            "runs.json",
            r#"[
                {"databaseId":1000,"headSha":"0123456789abcdef0123456789abcdef01234567","event":"push","headBranch":"main","conclusion":"failure"},
                {"databaseId":999,"headSha":"abcdef0123456789abcdef0123456789abcdef01","event":"push","headBranch":"main","conclusion":"failure"}
            ]"#,
        );
        dir.write(
            "history/1000/view.json",
            r#"{"jobs":[{"name":"Journey suite on the release binary","conclusion":"failure"}]}"#,
        );
        dir.write("history/1000/journey-failures.txt", "qa\\n");
        if let Some(view) = view {
            dir.write("history/999/view.json", view);
        }
        let args = [
            "--current-id",
            "1000",
            "--current-sha",
            "0123456789abcdef0123456789abcdef01234567",
            "--failures",
            dir.path().join("failures.txt").to_str().unwrap(),
            "--runs",
            dir.path().join("runs.json").to_str().unwrap(),
            "--history-dir",
            dir.path().join("history").to_str().unwrap(),
            "--output",
            dir.path().join("journey-streaks.json").to_str().unwrap(),
        ]
        .map(str::to_owned);

        assert!(compute(&args).unwrap());
        assert!(!dir.path().join("journey-streaks.json").exists());
    }
}

#[test]
fn compute_counts_two_prior_failures_and_keeps_oldest_sha() {
    let dir = TempDir::new("journey-streak-two-prior-failures");
    let oldest_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let newer_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let current_sha = "cccccccccccccccccccccccccccccccccccccccc";
    dir.write("failures.txt", "qa\n");
    dir.write(
        "runs.json",
        &format!(
            r#"[
                {{"databaseId":1000,"headSha":"{current_sha}","event":"push","headBranch":"main","conclusion":"failure"}},
                {{"databaseId":999,"headSha":"{newer_sha}","event":"push","headBranch":"main","conclusion":"failure"}},
                {{"databaseId":998,"headSha":"{oldest_sha}","event":"push","headBranch":"main","conclusion":"failure"}}
            ]"#
        ),
    );
    for id in ["999", "998"] {
        dir.write(
            &format!("history/{id}/view.json"),
            r#"{"jobs":[{"name":"Journey suite on the release binary","conclusion":"failure"}]}"#,
        );
        dir.write(&format!("history/{id}/journey-failures.txt"), "qa\n");
    }
    let args = [
        "--current-id",
        "1000",
        "--current-sha",
        current_sha,
        "--failures",
        dir.path().join("failures.txt").to_str().unwrap(),
        "--runs",
        dir.path().join("runs.json").to_str().unwrap(),
        "--history-dir",
        dir.path().join("history").to_str().unwrap(),
        "--output",
        dir.path().join("journey-streaks.json").to_str().unwrap(),
    ]
    .map(str::to_owned);

    assert!(compute(&args).unwrap());
    let output: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("journey-streaks.json")).unwrap())
            .unwrap();
    assert_eq!(output["qa"]["count"], 3);
    assert_eq!(output["qa"]["first_sha"], oldest_sha);
}

#[test]
fn skipped_producer_does_not_contribute_to_history() {
    let dir = TempDir::new("journey-streak-skipped-producer");
    dir.write("failures.txt", "qa\n");
    dir.write(
        "runs.json",
        r#"[
            {"databaseId":1000,"headSha":"0123456789abcdef0123456789abcdef01234567","event":"push","headBranch":"main","conclusion":"failure"},
            {"databaseId":999,"headSha":"abcdef0123456789abcdef0123456789abcdef01","event":"push","headBranch":"main","conclusion":"failure"}
        ]"#,
    );
    dir.write(
        "history/1000/view.json",
        r#"{"jobs":[{"name":"Journey suite on the release binary","conclusion":"failure"}]}"#,
    );
    dir.write("history/1000/journey-failures.txt", "qa\n");
    dir.write(
        "history/999/view.json",
        r#"{"jobs":[{"name":"Journey suite on the release binary","conclusion":"skipped"}]}"#,
    );
    dir.write("history/999/journey-failures.txt", "qa\n");
    let args = [
        "--current-id",
        "1000",
        "--current-sha",
        "0123456789abcdef0123456789abcdef01234567",
        "--failures",
        dir.path().join("failures.txt").to_str().unwrap(),
        "--runs",
        dir.path().join("runs.json").to_str().unwrap(),
        "--history-dir",
        dir.path().join("history").to_str().unwrap(),
        "--output",
        dir.path().join("journey-streaks.json").to_str().unwrap(),
    ]
    .map(str::to_owned);

    assert!(compute(&args).unwrap());
    assert!(!dir.path().join("journey-streaks.json").exists());
}

#[test]
fn skipped_producer_is_ignored() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        streaks(
            sha,
            "qa\n",
            &[PriorRun {
                sha: sha.into(),
                failures: None
            }]
        )["qa"],
        (1, sha.to_owned())
    );
}
