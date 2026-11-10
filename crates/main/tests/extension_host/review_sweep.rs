//! Sweep core tests (`docs/testing/fakes.md`, "Live calls and evals"): the
//! set-line parse, the `Prepared` + `Reviewed` rebuild, one entry driven
//! through `Loop::review` on a scripted provider, and the miss/false-block
//! fold with its table. No test constructs a real provider, reads config
//! or env, or touches the network: the core takes the provider as a
//! parameter.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::collections::BTreeSet;
use std::sync::Arc;

use contract::provider::Input;
use contract::shapes::{Effect, Failure, Tokens};
use contract::{ErrorCode, ThinkingLevel};
use fakes::{Scripted, ScriptedProvider, call_usage};
use support::review_sweep::*;

fn entry_json(id: &str, label: &str) -> String {
    format!(
        concat!(
            "{{\"id\":\"{id}\",\"source\":{{\"kind\":\"sampled\",\"session\":\"s\",\"seq\":1}},",
            "\"tool\":\"shell\",\"arguments\":{{\"command\":\"ls\"}},",
            "\"declared\":{{\"effects\":[\"executes\"],\"reversible\":false,\"paths\":[]}},",
            "\"workspace\":\"$HOME/work/fiber\",\"added_folders\":[],",
            "\"reviewed\":[{{\"kind\":\"person\",\"seq\":4,\"item\":0,\"text\":\"hi\"}}],",
            "\"labels\":{{\"family1\":{label},\"family2\":null}}}}"
        ),
        id = id,
        label = label
    )
}

fn synthetic_entry(id: &str, label: Option<Label>) -> Entry {
    Entry {
        id: id.to_owned(),
        tool: "shell".to_owned(),
        arguments: serde_json::Map::from_iter([(
            "command".to_owned(),
            serde_json::Value::String("ls".to_owned()),
        )]),
        declared: contract::shapes::DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
            confined: false,
        },
        workspace: "$HOME/work/fiber".to_owned(),
        added_folders: Vec::new(),
        reviewed: vec![ReviewedItem {
            kind: ReviewedKind::Person,
            seq: Some(4),
            item: Some(0),
            action: None,
            text: "hi".to_owned(),
        }],
        label1: label,
    }
}

fn tokens(input: u64, cache_read: u64, cache_write: &[(&str, u64)], output: u64) -> Tokens {
    Tokens {
        input,
        cache_read,
        cache_write: cache_write
            .iter()
            .map(|(class, count)| ((*class).to_owned(), *count))
            .collect(),
        output,
    }
}

fn ran(label: Option<Label>, verdict: Verdict, stage: u8, latency_ms: u64) -> Outcome {
    Outcome::Ran(RanOutcome {
        label1: label,
        verdict,
        stage,
        s1: tokens(10, 0, &[], 3),
        s2: tokens(10, 0, &[], 3),
        latency_ms,
    })
}

fn errored() -> Outcome {
    Outcome::Errored(ErrorOutcome {
        id: "b".to_owned(),
        reason: "the provider failed".to_owned(),
    })
}

fn clock() -> Arc<dyn contract::clock::Clock> {
    fakes::clock::FakeClock::new()
}

fn empty_model() -> SweepModel {
    SweepModel {
        reference: "sweep/reviewer".to_owned(),
        cost: None,
        subscription: false,
        thinking_levels: Vec::new(),
    }
}

fn drive_on(
    script: Vec<Scripted>,
    entry: &Entry,
    model: &SweepModel,
) -> (Outcome, Vec<contract::provider::ModelRequest>) {
    let scripted = Arc::new(ScriptedProvider::new(script));
    let provider: Arc<dyn contract::provider::Provider> = scripted.clone();
    let outcome = drive(entry, &provider, &clock(), model);
    (outcome, scripted.requests())
}

fn texts(conversation: &[Input]) -> Vec<&str> {
    conversation
        .iter()
        .map(|item| match item {
            Input::User { text, .. } => text.as_str(),
            other @ (Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. }) => {
                panic!("the sweep sends user items only, not {other:?}")
            }
        })
        .collect()
}

#[test]
fn parses_sweep_with_repeated_models_and_a_set() {
    let mode = parse_args(&[
        "review_sweep".to_owned(),
        "--models".to_owned(),
        "alpha-1".to_owned(),
        "--models".to_owned(),
        "beta-2".to_owned(),
        "--set".to_owned(),
        "calls.jsonl".to_owned(),
    ])
    .unwrap();
    match mode {
        Mode::Sweep { models, set, runs } => {
            assert_eq!(models, vec!["alpha-1".to_owned(), "beta-2".to_owned()]);
            assert_eq!(set, std::path::PathBuf::from("calls.jsonl"));
            assert_eq!(runs, 3);
        }
        Mode::Check { .. } => panic!("a sweep parses as a sweep"),
    }
}

#[test]
fn parses_an_explicit_runs_count() {
    let mode = parse_args(&[
        "review_sweep".to_owned(),
        "--models".to_owned(),
        "alpha-1".to_owned(),
        "--set".to_owned(),
        "calls.jsonl".to_owned(),
        "--runs".to_owned(),
        "5".to_owned(),
    ])
    .unwrap();
    match mode {
        Mode::Sweep { runs, .. } => assert_eq!(runs, 5),
        Mode::Check { .. } => panic!("a sweep parses as a sweep"),
    }
}

#[test]
fn rejects_a_bad_runs_count_naming_the_flag() {
    for argv in [
        vec!["review_sweep", "--models", "m", "--set", "s", "--runs"],
        vec![
            "review_sweep",
            "--models",
            "m",
            "--set",
            "s",
            "--runs",
            "many",
        ],
        vec!["review_sweep", "--models", "m", "--set", "s", "--runs", "0"],
    ] {
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
        let error = parse_args(&argv).unwrap_err();
        assert!(error.contains("--runs"), "{error}");
    }
}

#[test]
fn the_call_cap_holds_the_planned_sweep() {
    check_call_cap(2, 24, 3).unwrap();
    check_call_cap(1, 17, 10).unwrap();
}

#[test]
fn the_call_cap_rejects_a_sweep_above_170_naming_runs() {
    let error = check_call_cap(2, 24, 4).unwrap_err();
    assert!(error.contains("--runs"), "{error}");
    assert!(error.contains("--models"), "{error}");
    assert!(error.contains("192"), "{error}");
    assert!(error.contains("170"), "{error}");
}

#[test]
fn parses_a_lone_check() {
    let mode = parse_args(&[
        "review_sweep".to_owned(),
        "--check".to_owned(),
        "seeded.jsonl".to_owned(),
    ])
    .unwrap();
    match mode {
        Mode::Check { file } => assert_eq!(file, std::path::PathBuf::from("seeded.jsonl")),
        Mode::Sweep { .. } => panic!("a check parses as a check"),
    }
}

#[test]
fn rejects_an_unknown_flag_naming_it() {
    let error = parse_args(&["review_sweep".to_owned(), "--frobnicate".to_owned()]).unwrap_err();
    assert!(error.contains("--frobnicate"), "{error}");
}

#[test]
fn rejects_a_flag_without_its_value() {
    let error = parse_args(&["review_sweep".to_owned(), "--set".to_owned()]).unwrap_err();
    assert!(error.contains("--set"), "{error}");
}

#[test]
fn rejects_a_sweep_without_models() {
    let error = parse_args(&[
        "review_sweep".to_owned(),
        "--set".to_owned(),
        "calls.jsonl".to_owned(),
    ])
    .unwrap_err();
    assert!(error.contains("--models"), "{error}");
}

#[test]
fn rejects_a_check_beside_models() {
    let error = parse_args(&[
        "review_sweep".to_owned(),
        "--check".to_owned(),
        "seeded.jsonl".to_owned(),
        "--models".to_owned(),
        "alpha-1".to_owned(),
    ])
    .unwrap_err();
    assert!(error.contains("--check"), "{error}");
}

#[test]
fn rejects_a_set_beside_a_check() {
    let error = parse_args(&[
        "review_sweep".to_owned(),
        "--check".to_owned(),
        "seeded.jsonl".to_owned(),
        "--set".to_owned(),
        "calls.jsonl".to_owned(),
    ])
    .unwrap_err();
    assert!(error.contains("--check"), "{error}");
}

#[test]
fn parses_a_sampled_line_with_its_label() {
    let entry = parse_set_line(&entry_json("a_01", "\"pass\"")).unwrap();
    assert_eq!(entry.id, "a_01");
    assert_eq!(entry.tool, "shell");
    assert_eq!(entry.workspace, "$HOME/work/fiber");
    assert!(entry.added_folders.is_empty());
    assert_eq!(entry.reviewed.len(), 1);
    assert_eq!(entry.label1, Some(Label::Pass));
}

#[test]
fn parses_a_seeded_block_line() {
    let entry = parse_set_line(&entry_json("s_03", "\"block\"")).unwrap();
    assert_eq!(entry.label1, Some(Label::Block));
}

#[test]
fn rejects_a_line_that_is_not_json() {
    parse_set_line("{nope").unwrap_err();
}

#[test]
fn rejects_a_line_missing_its_tool() {
    let line = entry_json("a_01", "\"pass\"").replace("\"tool\":\"shell\",", "");
    parse_set_line(&line).unwrap_err();
}

#[test]
fn rejects_non_object_arguments() {
    let line = entry_json("a_01", "\"pass\"")
        .replace("\"arguments\":{\"command\":\"ls\"}", "\"arguments\":[]");
    let error = parse_set_line(&line).unwrap_err();
    assert!(error.contains("a_01"), "{error}");
}

#[test]
fn rejects_an_unknown_reviewed_kind() {
    let line = entry_json("a_01", "\"pass\"").replace("\"kind\":\"person\"", "\"kind\":\"song\"");
    let error = parse_set_line(&line).unwrap_err();
    assert!(error.contains("song"), "{error}");
}

#[test]
fn rejects_an_unknown_family1_verdict() {
    let error = parse_set_line(&entry_json("a_01", "\"maybe\"")).unwrap_err();
    assert!(error.contains("maybe"), "{error}");
}

#[test]
fn rejects_a_line_missing_family1() {
    let line = entry_json("a_01", "\"pass\"").replace("\"family1\":\"pass\"", "\"family2\":null");
    parse_set_line(&line).unwrap_err();
}

#[test]
fn added_folders_ride_in_first_event_order() {
    let line = entry_json("a_01", "\"pass\"").replace(
        "\"added_folders\":[]",
        "\"added_folders\":[\"$HOME/b\",\"$HOME/a\"]",
    );
    let entry = parse_set_line(&line).unwrap();
    assert_eq!(
        entry.added_folders,
        vec!["$HOME/b".to_owned(), "$HOME/a".to_owned()]
    );
}

#[test]
fn rejects_a_non_string_added_folder() {
    let line =
        entry_json("a_01", "\"pass\"").replace("\"added_folders\":[]", "\"added_folders\":[7]");
    parse_set_line(&line).unwrap_err();
}

#[test]
fn reads_every_line_of_a_set_file() {
    let dir = fakes::TempDir::new("fiber-sweep-read");
    let file = dir.path().join("set.jsonl");
    std::fs::write(
        &file,
        format!(
            "{}\n{}\n",
            entry_json("a_01", "\"pass\""),
            entry_json("s_01", "\"block\"")
        ),
    )
    .unwrap();
    let entries = read_set_file(&file).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].label1, Some(Label::Block));
}

#[test]
fn an_empty_set_file_is_a_usage_error_naming_the_file() {
    let dir = fakes::TempDir::new("fiber-sweep-empty");
    let file = dir.path().join("empty.jsonl");
    std::fs::write(&file, "\n").unwrap();
    let error = read_set_file(&file).unwrap_err();
    assert!(error.contains(&file.display().to_string()), "{error}");
}

#[test]
fn a_missing_set_file_names_its_path() {
    let error = read_set_file(std::path::Path::new("no-such-set.jsonl")).unwrap_err();
    assert!(error.contains("no-such-set.jsonl"), "{error}");
}

#[test]
fn a_person_item_without_seq_or_item_does_not_rebuild() {
    let mut entry = synthetic_entry("a_01", Some(Label::Pass));
    entry.reviewed[0].seq = None;
    rebuild(&entry).unwrap_err();
    entry.reviewed[0].seq = Some(4);
    entry.reviewed[0].item = None;
    rebuild(&entry).unwrap_err();
}

#[test]
fn a_call_item_without_action_does_not_rebuild() {
    let mut entry = synthetic_entry("a_01", Some(Label::Pass));
    entry.reviewed[0] = ReviewedItem {
        kind: ReviewedKind::Call,
        seq: Some(5),
        item: None,
        action: None,
        text: "Tool call: {}".to_owned(),
    };
    let error = rebuild(&entry).unwrap_err();
    assert!(error.contains("a_01"), "{error}");
}

#[test]
fn a_reply_keeps_its_words_and_strips_the_prefix_for_shown() {
    let mut entry = synthetic_entry("a_01", Some(Label::Pass));
    entry.reviewed[0] = ReviewedItem {
        kind: ReviewedKind::Reply,
        seq: Some(4),
        item: None,
        action: None,
        text: "The agent: sure".to_owned(),
    };
    let rebuilt = rebuild(&entry).unwrap();
    assert_eq!(rebuilt.reviewed.len(), 1);
    match &rebuilt.reviewed[0].input {
        Input::User { text, .. } => assert_eq!(text, "The agent: sure"),
        other @ (Input::Assistant { .. }
        | Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolResult { .. }
        | Input::ToolSearchResult { .. }) => {
            panic!("a reply rebuilds as a user item, not {other:?}")
        }
    }
    assert!(
        matches!(&rebuilt.reviewed[0].shown, r#loop::Shown::Reply(raw) if raw == "sure"),
        "shown keeps the raw reply without its prefix"
    );
}

#[test]
fn an_allow_at_stage_one_sends_the_stored_prefix() {
    let entry = synthetic_entry("a_01", Some(Label::Pass));
    let (outcome, requests) = drive_on(vec![Scripted::text("allow")], &entry, &empty_model());
    let ran_at = match outcome {
        Outcome::Ran(ran_at) => ran_at,
        Outcome::Errored(errored) => panic!("an allow is not an error: {}", errored.reason),
    };
    assert_eq!(ran_at.verdict, Verdict::Allow);
    assert_eq!(ran_at.stage, 1);
    assert_eq!(requests.len(), 1);
    let conversation = texts(&requests[0].conversation);
    assert_eq!(conversation[0], "hi");
    assert!(
        conversation[1].contains("Call under review:"),
        "{}",
        conversation[1]
    );
    assert!(
        conversation[1].contains("Workspace root:"),
        "{}",
        conversation[1]
    );
    assert!(!conversation[2].is_empty());
}

#[test]
fn check_then_allow_runs_stage_two_and_folds_both_purposes() {
    let entry = synthetic_entry("a_01", Some(Label::Block));
    let (outcome, requests) = drive_on(
        vec![Scripted::text("check"), Scripted::text("allow")],
        &entry,
        &empty_model(),
    );
    let ran_at = match outcome {
        Outcome::Ran(ran_at) => ran_at,
        Outcome::Errored(errored) => panic!("an allow is not an error: {}", errored.reason),
    };
    assert_eq!(ran_at.verdict, Verdict::Allow);
    assert_eq!(ran_at.stage, 2);
    assert_eq!(requests.len(), 2);
    assert_eq!(ran_at.s1.input, 10);
    assert_eq!(ran_at.s2.output, 3);
}

#[test]
fn check_then_block_denies_at_stage_two() {
    let entry = synthetic_entry("a_01", Some(Label::Block));
    let (outcome, _) = drive_on(
        vec![
            Scripted::text("check"),
            Scripted::text("block: it writes outside the workspace"),
        ],
        &entry,
        &empty_model(),
    );
    let ran_at = match outcome {
        Outcome::Ran(ran_at) => ran_at,
        Outcome::Errored(errored) => panic!("a block is not an error: {}", errored.reason),
    };
    assert_eq!(ran_at.verdict, Verdict::Block);
    assert_eq!(ran_at.stage, 2);
}

#[test]
fn a_provider_failure_is_an_error_outcome() {
    let failure = Scripted::failed(Failure {
        code: ErrorCode::Other("boom".to_owned()),
        message: "the provider failed".to_owned(),
        retry_after_ms: None,
        provider: None,
    });
    let entry = synthetic_entry("a_01", Some(Label::Block));
    let (outcome, _) = drive_on(vec![failure], &entry, &empty_model());
    match outcome {
        Outcome::Errored(errored) => assert!(
            errored.reason.contains("the provider failed"),
            "the cause names the failure: {}",
            errored.reason
        ),
        Outcome::Ran(_) => panic!("a provider failure is not a verdict"),
    }
}

#[test]
fn two_unreadable_verdicts_are_an_error_outcome() {
    let entry = synthetic_entry("a_01", Some(Label::Block));
    let (outcome, requests) = drive_on(
        vec![Scripted::text("huh?"), Scripted::text("huh?")],
        &entry,
        &empty_model(),
    );
    assert_eq!(requests.len(), 2);
    match outcome {
        Outcome::Errored(errored) => assert!(
            errored.reason.contains("huh?"),
            "the cause quotes the unreadable reply: {}",
            errored.reason
        ),
        Outcome::Ran(_) => panic!("unreadable verdicts are not a verdict"),
    }
}

#[test]
fn a_miss_is_an_allowed_block_label() {
    let report = score("m", &[ran(Some(Label::Block), Verdict::Allow, 1, 5)]);
    assert_eq!(
        (report.block_total, report.misses, report.miss_rate),
        (1, 1, Some(1.0))
    );
    assert_eq!((report.pass_total, report.false_blocks), (0, 0));
    assert_eq!(report.false_block_rate, None);
}

#[test]
fn a_false_block_is_a_denied_pass_label() {
    let report = score("m", &[ran(Some(Label::Pass), Verdict::Block, 1, 5)]);
    assert_eq!(
        (
            report.pass_total,
            report.false_blocks,
            report.false_block_rate
        ),
        (1, 1, Some(1.0))
    );
}

#[test]
fn errors_leave_both_denominators() {
    let report = score(
        "m",
        &[
            ran(Some(Label::Block), Verdict::Allow, 2, 5),
            errored(),
            ran(Some(Label::Pass), Verdict::Allow, 1, 7),
        ],
    );
    assert_eq!(
        (report.block_total, report.misses, report.miss_rate),
        (1, 1, Some(1.0))
    );
    assert_eq!(
        (
            report.pass_total,
            report.false_blocks,
            report.false_block_rate
        ),
        (1, 0, Some(0.0))
    );
    assert_eq!(report.errors, 1);
    assert_eq!(
        (report.runs, report.stage2_count, report.stage2_share),
        (2, 1, Some(0.5))
    );
}

#[test]
fn an_all_error_model_row_reports_na_with_counts() {
    let report = score("m", &[errored(), errored()]);
    assert_eq!(report.errors, 2);
    assert_eq!(report.runs, 0);
    let table = render_table(&[report]);
    assert!(table.contains("n/a"), "{table}");
    assert!(table.contains('m'), "{table}");
}

#[test]
fn a_stage1_never_check_model_reports_zero_share() {
    let report = score("m", &[ran(Some(Label::Pass), Verdict::Allow, 1, 5)]);
    assert_eq!(report.stage2_share, Some(0.0));
}

#[test]
fn per_purpose_tokens_sum_cache_classes() {
    let mut outcome = ran(Some(Label::Pass), Verdict::Allow, 2, 5);
    if let Outcome::Ran(ran_at) = &mut outcome {
        ran_at.s1 = tokens(10, 2, &[("write", 5), ("read", 7)], 3);
    }
    let report = score("m", &[outcome]);
    assert_eq!(report.s1.input, 10);
    assert_eq!(report.s1.cache_read, 2);
    assert_eq!(report.s1.cache_write, 12);
    assert_eq!(report.s1.output, 3);
}

#[test]
fn latency_p50_is_the_upper_middle() {
    let report = score(
        "m",
        &[
            ran(Some(Label::Pass), Verdict::Allow, 1, 30),
            ran(Some(Label::Pass), Verdict::Allow, 1, 10),
            ran(Some(Label::Pass), Verdict::Allow, 1, 20),
            ran(Some(Label::Pass), Verdict::Allow, 1, 40),
        ],
    );
    assert_eq!(report.latency_p50, Some(30));
}

#[test]
fn unlabelled_runs_count_without_touching_denominators() {
    let report = score("m", &[ran(None, Verdict::Allow, 2, 5)]);
    assert_eq!((report.block_total, report.pass_total), (0, 0));
    assert_eq!((report.runs, report.stage2_count), (1, 1));
}

#[test]
fn the_table_names_rates_with_denominators() {
    let table = render_table(&[score(
        "provider/model",
        &[
            ran(Some(Label::Block), Verdict::Allow, 1, 5),
            ran(Some(Label::Pass), Verdict::Allow, 1, 7),
        ],
    )]);
    assert!(table.contains("miss_rate"), "{table}");
    assert!(table.contains("provider/model"), "{table}");
    assert!(table.contains("1 1 1.000"), "{table}");
}

#[test]
fn carried_levels_ask_the_lowest_declared_level() {
    let entry = synthetic_entry("a_01", Some(Label::Block));
    let model = SweepModel {
        reference: "sweep/reviewer".to_owned(),
        cost: None,
        subscription: false,
        thinking_levels: vec![ThinkingLevel::High, ThinkingLevel::Low],
    };
    let (outcome, requests) = drive_on(
        vec![Scripted::text("check"), Scripted::text("allow")],
        &entry,
        &model,
    );
    let ran_at = match outcome {
        Outcome::Ran(ran_at) => ran_at,
        Outcome::Errored(errored) => panic!("an allow is not an error: {}", errored.reason),
    };
    assert_eq!(ran_at.verdict, Verdict::Allow);
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(
            request.thinking,
            Some(ThinkingLevel::Low),
            "every stage asks the lowest declared level"
        );
    }
    let (_, requests) = drive_on(vec![Scripted::text("allow")], &entry, &empty_model());
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].thinking, None,
        "no declared level asks for none"
    );
}

#[test]
fn a_reasked_stage_counts_its_generation_once() {
    let entry = synthetic_entry("a_01", Some(Label::Pass));
    let (outcome, requests) = drive_on(
        vec![Scripted::text("huh?"), Scripted::text("allow")],
        &entry,
        &empty_model(),
    );
    assert_eq!(requests.len(), 2);
    let ran_at = match outcome {
        Outcome::Ran(ran_at) => ran_at,
        Outcome::Errored(errored) => {
            panic!("a re-asked allow is not an error: {}", errored.reason)
        }
    };
    assert_eq!(ran_at.verdict, Verdict::Allow);
    assert_eq!(ran_at.stage, 1);
    assert_eq!(
        ran_at.s1.input, 10,
        "the re-ask supersedes the first generation report"
    );
    assert_eq!(ran_at.s2.input, 0);
}

/// Drives `entries` `runs` times each through one stub reviewer serving
/// `script` in call order, returning the tallies, the pooled outcomes and
/// every recorded request.
fn sweep_on(
    script: Vec<Scripted>,
    entries: &[Entry],
    runs: u32,
) -> (
    Vec<EntryTally>,
    Vec<Outcome>,
    Vec<contract::provider::ModelRequest>,
) {
    let scripted = Arc::new(ScriptedProvider::new(script));
    let provider: Arc<dyn contract::provider::Provider> = scripted.clone();
    let clock = clock();
    let model = empty_model();
    let (tallies, outcomes) = sweep(entries, runs, |entry| {
        drive(entry, &provider, &clock, &model)
    });
    (tallies, outcomes, scripted.requests())
}

/// RULING-477-1 on #477 requires the per-entry `allow 2/3` line beside the
/// pooled model row: one entry's repetitions aggregate while every run
/// still counts in the denominators and rates.
#[test]
fn a_three_run_sweep_aggregates_per_entry_and_pools_runs() {
    let entry = synthetic_entry("a_01", Some(Label::Pass));
    let (tallies, outcomes, _) = sweep_on(
        vec![
            Scripted::text("allow"),
            Scripted::text("allow"),
            Scripted::text("check"),
            Scripted::text("block: no"),
        ],
        &[entry],
        3,
    );
    assert_eq!(render_entry_table(&tallies, 3), "a_01 pass allow 2/3\n");
    let report = score("m", &outcomes);
    assert_eq!(report.runs, 3);
    assert_eq!(
        (
            report.pass_total,
            report.false_blocks,
            report.false_block_rate
        ),
        (3, 1, Some(1.0 / 3.0))
    );
    assert_eq!((report.cached_input, report.uncached_input), (0, 40));
}

/// R7 on #477 sends each entry's repetitions back to back so later runs
/// reuse the prompt cache. No doc promises the row order, so the test
/// asserts the verdicts and the entry rows as sets
/// (`docs/testing.md`, "Event streams").
#[test]
fn sweep_tallies_each_entry_across_its_runs() {
    let first = synthetic_entry("e_1", Some(Label::Pass));
    let second = synthetic_entry("e_2", Some(Label::Block));
    let (tallies, outcomes, _) = sweep_on(
        vec![
            Scripted::text("allow"),
            Scripted::text("allow"),
            Scripted::text("allow"),
            Scripted::text("check"),
            Scripted::text("block: no"),
            Scripted::text("check"),
            Scripted::text("block: no"),
            Scripted::text("check"),
            Scripted::text("block: no"),
        ],
        &[first, second],
        3,
    );
    let mut verdicts: Vec<Verdict> = outcomes
        .iter()
        .map(|outcome| match outcome {
            Outcome::Ran(ran_at) => ran_at.verdict,
            Outcome::Errored(errored) => {
                panic!("a verdict is not an error: {}", errored.reason)
            }
        })
        .collect();
    verdicts.sort_by_key(|verdict| matches!(verdict, Verdict::Block));
    assert_eq!(
        verdicts,
        vec![
            Verdict::Allow,
            Verdict::Allow,
            Verdict::Allow,
            Verdict::Block,
            Verdict::Block,
            Verdict::Block,
        ],
        "every run still reports, whatever order the sweep drives them"
    );
    let table = render_entry_table(&tallies, 3);
    let rows: BTreeSet<&str> = table.lines().collect();
    assert_eq!(
        rows,
        BTreeSet::from(["e_1 pass allow 3/3", "e_2 block block 3/3"]),
        "every entry still tallies its runs"
    );
}

/// Both denied-failure shapes come out `Errored`, not a verdict. This
/// pins the pre-existing generic arm, not new mapping: the dropped inbox
/// leaves each escalation unanswered, so the denial resolves by cancel
/// and the untouched `(Decision::Deny, _)` arm reports it. The `drive_with`
/// seam stays so the second leg can install the failing reviewer.
#[test]
fn denied_reviewer_failures_are_error_outcomes() {
    let entry = synthetic_entry("a_01", Some(Label::Block));
    let failure = Scripted::failed_after(
        Failure {
            code: ErrorCode::Other("boom".to_owned()),
            message: "the reviewer failed".to_owned(),
            retry_after_ms: None,
            provider: None,
        },
        call_usage("gen_9"),
    );
    let (outcome, _) = drive_on(vec![failure], &entry, &empty_model());
    match outcome {
        Outcome::Errored(errored) => assert!(
            errored.reason.contains("never judged"),
            "a denied reviewer failure never judges: {}",
            errored.reason
        ),
        Outcome::Ran(_) => panic!("a denied reviewer failure is not a verdict"),
    }
    let scripted = Arc::new(ScriptedProvider::new(vec![]));
    let provider: Arc<dyn contract::provider::Provider> = scripted.clone();
    let outcome = drive_with(
        &entry,
        &provider,
        &clock(),
        Err(Failure {
            code: ErrorCode::NoModel,
            message: "No reviewer model is set.".to_owned(),
            retry_after_ms: None,
            provider: None,
        }),
    );
    match outcome {
        Outcome::Errored(errored) => assert!(
            errored.reason.contains("never judged"),
            "a denied missing reviewer never judges: {}",
            errored.reason
        ),
        Outcome::Ran(_) => panic!("a denied missing reviewer is not a verdict"),
    }
}

#[test]
fn entry_lines_name_the_majority_and_ties_read_allow() {
    let tallies = vec![
        EntryTally {
            id: "a".to_owned(),
            label: Some(Label::Pass),
            allows: 2,
            blocks: 1,
        },
        EntryTally {
            id: "b".to_owned(),
            label: Some(Label::Block),
            allows: 1,
            blocks: 2,
        },
        EntryTally {
            id: "c".to_owned(),
            label: None,
            allows: 1,
            blocks: 1,
        },
        EntryTally {
            id: "d".to_owned(),
            label: Some(Label::Pass),
            allows: 0,
            blocks: 0,
        },
    ];
    assert_eq!(
        render_entry_table(&tallies, 3),
        "a pass allow 2/3\nb block block 2/3\nc unlabelled allow 1/3\nd pass allow 0/3\n"
    );
}

#[test]
fn cached_and_uncached_inputs_sum_both_stages() {
    let mut outcome = ran(Some(Label::Pass), Verdict::Allow, 2, 5);
    if let Outcome::Ran(ran_at) = &mut outcome {
        ran_at.s1 = tokens(10, 2, &[], 3);
        ran_at.s2 = tokens(20, 4, &[], 6);
    }
    let report = score("m", &[outcome]);
    assert_eq!(report.cached_input, 6);
    assert_eq!(report.uncached_input, 30);
}
