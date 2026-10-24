use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

#[cfg(target_os = "macos")]
use super::brand_string;
#[cfg(target_os = "linux")]
use super::host_cpu;
use super::{
    Args, Base, Only, Pass, Side, attach, bench, busy, document, idle, listing, measure_all,
    model_name, paging, parse, resume, selected, session_list, sides, timing_names, wheel,
};

fn args(line: &str) -> Result<Args, String> {
    parse(line.split_whitespace().map(str::to_owned))
}

/// `parse` from whole arguments: `args` splits on spaces, which workload
/// names contain, so `--skip-rows` values travel as one argument here.
fn argv(words: &[&str]) -> Result<Args, String> {
    parse(words.iter().map(|word| (*word).to_owned()))
}

#[test]
fn the_defaults_are_five_runs_a_ten_second_window_and_every_workload() {
    assert_eq!(
        args("--fiber /b/fiber --out /o.json"),
        Ok(Args {
            fiber: PathBuf::from("/b/fiber"),
            out: PathBuf::from("/o.json"),
            runs: 5,
            timing_runs: 5,
            idle_secs: 10,
            only: Only::All,
            paging: None,
            skip_ten_turns: false,
            skip_rows: Vec::new(),
            base: None,
        })
    );
}

#[test]
fn every_flag_sets_its_value() {
    assert_eq!(
        args("--only timing --idle-secs 60 --runs 3 --out o --fiber f"),
        Ok(Args {
            fiber: PathBuf::from("f"),
            out: PathBuf::from("o"),
            runs: 3,
            timing_runs: 3,
            idle_secs: 60,
            only: Only::Timing,
            paging: None,
            skip_ten_turns: false,
            skip_rows: Vec::new(),
            base: None,
        })
    );
}

#[test]
fn a_usage_error_names_what_is_wrong() {
    for (line, said) in [
        ("--out o", "--fiber is required"),
        ("--fiber f", "--out is required"),
        ("--fiber f --out o --runs", "--runs needs a value"),
        ("--fiber f --out o --runs 0", "positive whole number"),
        ("--fiber f --out o --idle-secs ten", "positive whole number"),
        ("--fiber f --out o --only some", "all, timing or wheel"),
        ("--fiber f --out o --fast 1", "unknown argument"),
        ("--fiber f --out o --paging", "--paging needs a value"),
        (
            "--fiber f --out o --timing-runs",
            "--timing-runs needs a value",
        ),
        ("--fiber f --out o --timing-runs 0", "positive whole number"),
        ("--fiber f --out o --timing-runs x", "positive whole number"),
        ("--fiber f --out o --base-fiber b", "--base-fiber"),
        ("--fiber f --out o --base-out ob", "--base-out"),
        ("--fiber f --out o --base-paging j", "--base-paging"),
    ] {
        let err = args(line).unwrap_err();
        assert!(err.contains(said), "{line}: {err}");
    }
}

#[test]
fn paging_names_the_jig() {
    let parsed = args("--fiber f --out o --paging /j/paging").unwrap();
    assert_eq!(parsed.paging, Some(PathBuf::from("/j/paging")));
}

#[test]
fn base_flags_name_the_base_binary_its_file_and_its_jig() {
    let parsed = argv(&[
        "--fiber",
        "f",
        "--out",
        "o",
        "--skip-rows",
        "session start",
        "--base-fiber",
        "b",
        "--base-out",
        "ob",
        "--base-paging",
        "j",
    ])
    .unwrap();
    assert_eq!(parsed.skip_rows, vec!["session start".to_owned()]);
    assert_eq!(
        parsed.base,
        Some(Base {
            fiber: PathBuf::from("b"),
            out: PathBuf::from("ob"),
            paging: Some(PathBuf::from("j")),
        })
    );
    let parsed = args("--fiber f --out o --base-fiber b --base-out ob").unwrap();
    assert_eq!(
        parsed.base,
        Some(Base {
            fiber: PathBuf::from("b"),
            out: PathBuf::from("ob"),
            paging: None,
        })
    );
}

fn names(line: &str) -> Vec<&'static str> {
    selected(&args(line).unwrap())
        .iter()
        .map(|(workload, _, _)| workload.name)
        .collect()
}

fn counts(line: &str) -> BTreeMap<&'static str, (u32, u32)> {
    selected(&args(line).unwrap())
        .into_iter()
        .map(|(workload, repeats, runs)| (workload.name, (repeats, runs)))
        .collect()
}

#[test]
fn a_pull_request_runs_timing_workloads_three_times_and_the_rest_five() {
    let got = counts("--fiber f --out o --runs 5 --timing-runs 3 --paging j");
    assert_eq!(
        got,
        BTreeMap::from([
            ("session start", (3, 3)),
            ("session_list", (3, 3)),
            ("terminal first frame", (3, 3)),
            ("session idle", (5, 5)),
            ("terminal idle", (5, 5)),
            ("busy turn", (5, 5)),
            ("web_fetch", (5, 5)),
            ("resume 20,000 tokens", (5, 5)),
            ("resume 2,000,000 tokens", (5, 5)),
            ("paging", (3, 3)),
            ("fsyncs", (1, 1)),
            ("10 busy turns", (5, 5)),
            ("sessions list", (1, 3)),
            ("terminal attach", (1, 5)),
            ("terminal wheel", (1, 3)),
        ])
    );
    let timing = counts("--fiber f --out o --runs 5 --timing-runs 3 --paging j --only timing");
    assert_eq!(
        timing.keys().collect::<Vec<_>>(),
        [
            &"paging",
            &"session start",
            &"session_list",
            &"sessions list",
            &"terminal attach",
            &"terminal first frame",
            &"terminal wheel",
        ]
    );
    assert_eq!(timing["paging"], (3, 3));
    assert_eq!(timing["sessions list"], (1, 3));
}

#[test]
fn paging_follows_the_timing_count_at_two_three_and_four() {
    // Each new comparison gets a test that fails when it flips: a mutant
    // swapping `timing_runs` for `runs` fails every count here.
    for n in [2, 3, 4] {
        let got = counts(&format!(
            "--fiber f --out o --runs 5 --timing-runs {n} --paging j"
        ));
        assert_eq!(got["paging"], (n, n), "timing-runs {n}");
        assert_eq!(got["session start"], (n, n), "timing-runs {n}");
        assert_eq!(got["session idle"], (5, 5), "timing-runs {n}");
        assert_eq!(got["terminal attach"], (1, 5), "timing-runs {n}");
    }
    let got = counts("--fiber f --out o --runs 4 --paging j");
    assert_eq!(got["paging"], (4, 4));
}

#[test]
fn timing_runs_defaults_to_runs() {
    let parsed = args("--fiber f --out o --runs 4").unwrap();
    assert_eq!(parsed.timing_runs, 4);
    let got = counts("--fiber f --out o --runs 4");
    assert_eq!(got["session start"], (4, 4));
    assert_eq!(got["sessions list"], (1, 4));
    // The default resolves after every flag parses, whatever order they come in.
    let parsed = args("--fiber f --out o --timing-runs 3 --runs 4").unwrap();
    assert_eq!(parsed.timing_runs, 3);
}

#[test]
fn the_paging_workload_runs_only_with_its_jig_and_in_a_timing_run() {
    assert!(!names("--fiber f --out o").contains(&"paging"));
    assert!(!names("--fiber f --out o --only timing").contains(&"paging"));
    assert!(names("--fiber f --out o --paging j").contains(&"paging"));
    let timing = names("--fiber f --out o --only timing --paging j");
    assert!(timing.contains(&"paging"), "{timing:?}");
    assert!(!timing.contains(&"session idle"), "{timing:?}");
    let runs: Vec<(u32, u32)> = selected(&args("--fiber f --out o --runs 3 --paging j").unwrap())
        .iter()
        .filter(|(workload, _, _)| workload.name == "paging")
        .map(|(_, repeats, runs)| (*repeats, *runs))
        .collect();
    assert_eq!(runs, [(3, 3)]);
}

#[test]
fn the_sampled_workloads_run_once_and_in_a_timing_run() {
    let timing = names("--fiber f --out o --only timing");
    assert!(timing.contains(&"sessions list"), "{timing:?}");
    assert!(timing.contains(&"terminal attach"), "{timing:?}");
    assert!(!timing.contains(&"session idle"), "{timing:?}");
    let runs: Vec<(u32, u32)> = selected(&args("--fiber f --out o --runs 3").unwrap())
        .iter()
        .filter(|(workload, _, _)| {
            workload.name == "sessions list" || workload.name == "terminal attach"
        })
        .map(|(_, repeats, runs)| (*repeats, *runs))
        .collect();
    assert_eq!(runs, [(1, 3), (1, 3)]);
}

#[test]
fn only_wheel_parses_and_runs_just_the_wheel_workload() {
    assert_eq!(
        args("--fiber f --out o --only wheel").unwrap().only,
        Only::Wheel
    );
    assert_eq!(names("--fiber f --out o --only wheel"), ["terminal wheel"]);
    let timing = names("--fiber f --out o --only timing");
    assert!(timing.contains(&"terminal wheel"), "{timing:?}");
    let all = names("--fiber f --out o");
    assert!(all.contains(&"terminal wheel"), "{all:?}");
    let runs: Vec<(u32, u32)> = selected(&args("--fiber f --out o --runs 3").unwrap())
        .iter()
        .filter(|(workload, _, _)| workload.name == "terminal wheel")
        .map(|(_, repeats, runs)| (*repeats, *runs))
        .collect();
    assert_eq!(runs, [(1, 3)]);
}

#[test]
fn measure_all_hands_each_workload_its_counts_and_records_them() {
    let parsed = args("--fiber f --out o --runs 5 --timing-runs 3 --paging j").unwrap();
    // The per-call sequence the single-side run makes: every workload's
    // calls in selection order, each with its run count, except a sampled
    // row's one call per sample; the failing `busy turn` ends its row
    // after one call.
    let expected: Vec<(Side, &str, u32, u32)> = selected(&parsed)
        .iter()
        .flat_map(|(workload, repeats, runs)| {
            let (calls, each) = if workload.name == "sessions list" {
                (*runs, 1)
            } else if workload.name == "terminal attach" {
                (2 * *runs, 1)
            } else if workload.name == "busy turn" {
                (1, *runs)
            } else {
                (*repeats, *runs)
            };
            (1..=calls).map(move |n| (Side::Head, workload.name, n, each))
        })
        .collect();
    let mut head = Pass::new(Side::Head, parsed);
    let mut log = Vec::new();
    measure_all(&mut head, None, |side, workload, runs, run, _| {
        log.push((side, workload.name, run, runs));
        if workload.name == "busy turn" {
            return Err("boom".to_owned());
        }
        let id: &'static str = Box::leak(format!("{}_m", workload.name).into_boxed_str());
        Ok(vec![(id, json!(1.0))])
    });
    assert_eq!(log, expected);
    assert_eq!(head.metric_runs["session start_m"], 3);
    assert_eq!(head.metric_runs["sessions list_m"], 3);
    assert_eq!(head.metric_runs["terminal attach_m"], 5);
    assert_eq!(head.metric_runs["fsyncs_m"], 1);
    assert_eq!(head.metrics["session start_m"].len(), 3);
    assert!(!head.metrics.contains_key("busy turn_m"));
    assert!(!head.metric_runs.contains_key("busy turn_m"));
    assert_eq!(head.failures, ["busy turn, run 1: boom"]);
}

/// The per-call log an order test asserts: every entry is one timed
/// call, in the order it ran.
fn order(head_args: Args, base_args: Option<Args>) -> Vec<(Side, &'static str, u32)> {
    let mut head = Pass::new(Side::Head, head_args);
    let mut base = base_args.map(|args| Pass::new(Side::Base, args));
    let mut log = Vec::new();
    measure_all(&mut head, base.as_mut(), |side, workload, _, run, _| {
        log.push((side, workload.name, run));
        Ok(Vec::new())
    });
    log
}

#[test]
fn a_repeated_row_alternates_head_and_base_by_sample() {
    // A mutant running the base side first, or two calls of one side in
    // a row, fails the literal log below.
    let head = argv(&[
        "--fiber",
        "f",
        "--out",
        "o",
        "--runs",
        "3",
        "--timing-runs",
        "3",
        "--only",
        "timing",
        "--skip-rows",
        "terminal first frame,session_list,paging,sessions list,terminal attach,terminal wheel",
    ])
    .unwrap();
    let base = args("--fiber b --out ob --runs 3 --timing-runs 3 --only timing").unwrap();
    assert_eq!(
        order(head, Some(base)),
        [
            (Side::Head, "session start", 1),
            (Side::Base, "session start", 1),
            (Side::Head, "session start", 2),
            (Side::Base, "session start", 2),
            (Side::Head, "session start", 3),
            (Side::Base, "session start", 3),
        ]
    );
}

#[test]
fn sampled_rows_alternate_head_and_base_by_sample() {
    // A repeated row alternates per run, and a sampled row per sample, so
    // listing's 3 samples and attach's 2 fixtures of 5 interleave. A
    // mutant running a sampled row's head block then its base block, or
    // interleaving another row, fails the log below.
    let head = args("--fiber f --out o --runs 5 --timing-runs 3 --only timing").unwrap();
    let base = args("--fiber b --out ob --runs 5 --timing-runs 3 --only timing").unwrap();
    let (forward, back) = (Side::Head, Side::Base);
    let mut expected = vec![
        (forward, "session start", 1),
        (back, "session start", 1),
        (forward, "session start", 2),
        (back, "session start", 2),
        (forward, "session start", 3),
        (back, "session start", 3),
        (forward, "terminal first frame", 1),
        (back, "terminal first frame", 1),
        (forward, "terminal first frame", 2),
        (back, "terminal first frame", 2),
        (forward, "terminal first frame", 3),
        (back, "terminal first frame", 3),
        (forward, "session_list", 1),
        (back, "session_list", 1),
        (forward, "session_list", 2),
        (back, "session_list", 2),
        (forward, "session_list", 3),
        (back, "session_list", 3),
    ];
    // Each sampled row is one call per sample: listing's 3 and attach's
    // 2 fixtures of 5. Wheel keeps its head block then its base block
    // until its sessions prepare before timing.
    for (name, total) in [("sessions list", 3), ("terminal attach", 10)] {
        for s in 1..=total {
            expected.push((forward, name, s));
            expected.push((back, name, s));
        }
    }
    expected.push((forward, "terminal wheel", 1));
    expected.push((back, "terminal wheel", 1));
    assert_eq!(order(head, Some(base)), expected);
}

#[test]
fn skip_rows_filters_the_base_list_only() {
    // A mutant applying `--skip-rows` to the head list with a base, or
    // dropping it from the base list, fails the logs below.
    let (head_args, base_args) = sides(
        &argv(&[
            "--fiber",
            "f",
            "--out",
            "o",
            "--runs",
            "5",
            "--timing-runs",
            "3",
            "--base-fiber",
            "b",
            "--base-out",
            "ob",
            "--skip-rows",
            "session start",
        ])
        .unwrap(),
    );
    assert!(head_args.skip_rows.is_empty());
    let base_args = base_args.unwrap();
    assert_eq!(base_args.skip_rows, ["session start".to_owned()]);
    let mut head = Pass::new(Side::Head, head_args);
    let mut base = Pass::new(Side::Base, base_args);
    let mut log = Vec::new();
    measure_all(&mut head, Some(&mut base), |side, workload, _, run, _| {
        log.push((side, workload.name, run));
        Ok(Vec::new())
    });
    let starts: Vec<(Side, &str, u32)> = log
        .iter()
        .filter(|(_, name, _)| *name == "session start")
        .cloned()
        .collect();
    assert_eq!(
        starts,
        [
            (Side::Head, "session start", 1),
            (Side::Head, "session start", 2),
            (Side::Head, "session start", 3),
        ]
    );
    let firsts: Vec<(Side, &str, u32)> = log
        .iter()
        .filter(|(_, name, _)| *name == "terminal first frame")
        .cloned()
        .collect();
    assert_eq!(
        firsts,
        [
            (Side::Head, "terminal first frame", 1),
            (Side::Base, "terminal first frame", 1),
            (Side::Head, "terminal first frame", 2),
            (Side::Base, "terminal first frame", 2),
            (Side::Head, "terminal first frame", 3),
            (Side::Base, "terminal first frame", 3),
        ]
    );
    // The head's file skips nothing; the base's reports the skipped row,
    // so the judge reports it instead of failing its missing metric.
    let skipped = |pass: &Pass| {
        document(
            &pass.args,
            "CPU-X",
            None,
            BTreeMap::new(),
            BTreeMap::new(),
            Vec::new(),
        )["skipped"]
            .clone()
    };
    assert_eq!(skipped(&head), json!([]));
    assert_eq!(skipped(&base), json!(["session start"]));
}

/// Runs the lone `session start` row on both sides, failing the named
/// runs, and hands back the call log with both sides' passes.
fn fail_row(fail_head: &[u32], fail_base: &[u32]) -> (Vec<(Side, &'static str, u32)>, Pass, Pass) {
    let head = argv(&[
        "--fiber",
        "f",
        "--out",
        "o",
        "--runs",
        "3",
        "--timing-runs",
        "3",
        "--only",
        "timing",
        "--skip-rows",
        "terminal first frame,session_list,paging,sessions list,terminal attach,terminal wheel",
    ])
    .unwrap();
    let base = args("--fiber b --out ob --runs 3 --timing-runs 3 --only timing").unwrap();
    let mut head = Pass::new(Side::Head, head);
    let mut base = Pass::new(Side::Base, base);
    let mut log = Vec::new();
    measure_all(&mut head, Some(&mut base), |side, workload, _, run, _| {
        log.push((side, workload.name, run));
        let failing = match side {
            Side::Head => fail_head,
            Side::Base => fail_base,
        };
        if failing.contains(&run) {
            return Err("boom".to_owned());
        }
        Ok(vec![("ss_ms", json!(1.0))])
    });
    (log, head, base)
}

#[test]
fn a_head_error_ends_only_its_side_of_the_row() {
    let (log, head, base) = fail_row(&[2], &[]);
    assert_eq!(
        log,
        [
            (Side::Head, "session start", 1),
            (Side::Base, "session start", 1),
            (Side::Head, "session start", 2),
            (Side::Base, "session start", 2),
            (Side::Base, "session start", 3),
        ]
    );
    assert_eq!(head.failures, ["session start, run 2: boom"]);
    assert!(base.failures.is_empty());
    assert!(!head.metrics.contains_key("ss_ms"));
    assert!(!head.metric_runs.contains_key("ss_ms"));
    assert_eq!(base.metrics["ss_ms"].len(), 3);
    assert_eq!(base.metric_runs["ss_ms"], 3);
}

#[test]
fn a_head_error_on_the_first_run_leaves_the_base_its_row() {
    let (log, head, base) = fail_row(&[1], &[]);
    assert_eq!(
        log,
        [
            (Side::Head, "session start", 1),
            (Side::Base, "session start", 1),
            (Side::Base, "session start", 2),
            (Side::Base, "session start", 3),
        ]
    );
    assert_eq!(head.failures, ["session start, run 1: boom"]);
    assert!(!head.metrics.contains_key("ss_ms"));
    assert_eq!(base.metrics["ss_ms"].len(), 3);
}

#[test]
fn errors_on_both_sides_leave_neither_side_a_metric() {
    let (log, head, base) = fail_row(&[1], &[1]);
    assert_eq!(
        log,
        [
            (Side::Head, "session start", 1),
            (Side::Base, "session start", 1),
        ]
    );
    assert_eq!(head.failures, ["session start, run 1: boom"]);
    assert_eq!(base.failures, ["session start, run 1: boom"]);
    assert!(!head.metrics.contains_key("ss_ms"));
    assert!(!base.metrics.contains_key("ss_ms"));
}

#[test]
fn a_base_error_leaves_the_head_its_metrics() {
    let (log, head, base) = fail_row(&[], &[1]);
    assert_eq!(
        log,
        [
            (Side::Head, "session start", 1),
            (Side::Base, "session start", 1),
            (Side::Head, "session start", 2),
            (Side::Head, "session start", 3),
        ]
    );
    assert!(head.failures.is_empty());
    assert_eq!(base.failures, ["session start, run 1: boom"]);
    assert_eq!(head.metrics["ss_ms"].len(), 3);
    assert_eq!(head.metric_runs["ss_ms"], 3);
    assert!(!base.metrics.contains_key("ss_ms"));
}

#[test]
fn an_error_keeps_its_notes_on_both_sides() {
    // The old `repeat` recorded notes before it inspected the result; a
    // mutant recording notes only on `Ok` drops the shutdown warning
    // below, on either side and in a head-only run.
    let lone_start = || {
        argv(&[
            "--fiber",
            "f",
            "--out",
            "o",
            "--runs",
            "3",
            "--timing-runs",
            "3",
            "--only",
            "timing",
            "--skip-rows",
            "terminal first frame,session_list,paging,sessions list,terminal attach,terminal wheel",
        ])
        .unwrap()
    };
    let base_args = || args("--fiber b --out ob --runs 3 --timing-runs 3 --only timing").unwrap();
    let mut head = Pass::new(Side::Head, lone_start());
    let mut base = Pass::new(Side::Base, base_args());
    measure_all(&mut head, Some(&mut base), |_, _, _, _, notes| {
        notes.push("shutdown warning".to_owned());
        Err("boom".to_owned())
    });
    for pass in [&head, &base] {
        assert_eq!(
            pass.failures,
            [
                "session start, run 1: shutdown warning",
                "session start, run 1: boom",
            ]
        );
    }
    let mut head = Pass::new(Side::Head, lone_start());
    measure_all(&mut head, None, |_, workload, _, run, notes| {
        notes.push("shutdown warning".to_owned());
        Err(format!("boom run {run} of {}", workload.name))
    });
    assert_eq!(
        head.failures,
        [
            "session start, run 1: shutdown warning",
            "session start, run 1: boom run 1 of session start",
        ]
    );
}

#[test]
fn untimed_rows_and_a_missing_base_jig_run_on_the_head_alone() {
    let (head_args, base_args) = sides(
        &argv(&[
            "--fiber",
            "f",
            "--out",
            "o",
            "--runs",
            "5",
            "--timing-runs",
            "3",
            "--paging",
            "j",
            "--base-fiber",
            "b",
            "--base-out",
            "ob",
        ])
        .unwrap(),
    );
    let base_args = base_args.unwrap();
    assert!(base_args.paging.is_none());
    // The base list holds no memory row: every row it runs is timing-only.
    let base_names: Vec<&str> = selected(&base_args)
        .iter()
        .map(|(workload, _, _)| workload.name)
        .collect();
    assert!(!base_names.is_empty());
    for (workload, _, _) in selected(&base_args) {
        assert!(workload.timing, "{}", workload.name);
    }
    let mut head = Pass::new(Side::Head, head_args);
    let mut base = Pass::new(Side::Base, base_args);
    let mut log = Vec::new();
    measure_all(&mut head, Some(&mut base), |side, workload, _, run, _| {
        log.push((side, workload.name, run));
        Ok(Vec::new())
    });
    for name in ["fsyncs", "busy turn", "paging"] {
        let calls: Vec<(Side, &str, u32)> = log
            .iter()
            .filter(|(_, row, _)| *row == name)
            .cloned()
            .collect();
        assert!(!calls.is_empty(), "{name}");
        assert!(
            calls.iter().all(|(side, _, _)| *side == Side::Head),
            "{name}: {calls:?}"
        );
    }
    assert_eq!(
        log.iter().filter(|(_, name, _)| *name == "fsyncs").count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|(_, name, _)| *name == "busy turn")
            .count(),
        5
    );
    assert_eq!(
        log.iter().filter(|(_, name, _)| *name == "paging").count(),
        3
    );
}

#[test]
fn a_base_row_the_head_did_not_select_does_not_run() {
    let parsed = args("--fiber f --out o --only wheel --base-fiber b --base-out ob").unwrap();
    let (head_args, base_args) = sides(&parsed);
    assert!(base_args.is_some());
    assert_eq!(
        order(head_args, base_args),
        [
            (Side::Head, "terminal wheel", 1),
            (Side::Base, "terminal wheel", 1),
        ]
    );
}

#[test]
fn attach_keeps_its_full_count_on_both_sides() {
    let (head_args, base_args) = sides(
        &args(
            "--fiber f --out o --runs 5 --timing-runs 3 --only timing --base-fiber b --base-out ob",
        )
        .unwrap(),
    );
    let mut head = Pass::new(Side::Head, head_args);
    let mut base = Pass::new(Side::Base, base_args.unwrap());
    let mut log = Vec::new();
    measure_all(
        &mut head,
        Some(&mut base),
        |side, workload, runs, run, _| {
            log.push((side, workload.name, run, runs));
            if workload.name == "terminal attach" {
                return Ok(vec![("attach_ms", json!(1.0))]);
            }
            Ok(Vec::new())
        },
    );
    // Each side times one attach per call: 2 fixtures of 5, alternating
    // head and base, each call carrying a single sample.
    let attach: Vec<(Side, &str, u32, u32)> = log
        .iter()
        .filter(|(_, name, _, _)| *name == "terminal attach")
        .cloned()
        .collect();
    let mut expected = Vec::new();
    for s in 1..=10 {
        expected.push((Side::Head, "terminal attach", s, 1));
        expected.push((Side::Base, "terminal attach", s, 1));
    }
    assert_eq!(attach, expected);
    assert_eq!(head.metrics["attach_ms"].len(), 10);
    assert_eq!(base.metrics["attach_ms"].len(), 10);
    assert_eq!(head.metric_runs["attach_ms"], 5);
    assert_eq!(base.metric_runs["attach_ms"], 5);
}

#[test]
fn skip_rows_leaves_exactly_the_named_timing_workloads_out() {
    let parsed = argv(&[
        "--fiber",
        "f",
        "--out",
        "o",
        "--paging",
        "j",
        "--skip-rows",
        "session start,sessions list",
    ])
    .unwrap();
    assert_eq!(
        parsed.skip_rows,
        vec!["session start".to_owned(), "sessions list".to_owned()]
    );
    let all = names("--fiber f --out o --paging j");
    let kept: Vec<&str> = selected(&parsed)
        .iter()
        .map(|(workload, _, _)| workload.name)
        .collect();
    assert!(!kept.contains(&"session start"), "{kept:?}");
    assert!(!kept.contains(&"sessions list"), "{kept:?}");
    assert_eq!(kept.len(), 13, "{kept:?}");
    assert_eq!(kept.len() + 2, all.len());
}

#[test]
fn skip_rows_rejects_anything_but_a_timing_workload() {
    // Each rejected value names the value, so a typo points at itself.
    for (value, said) in [
        ("no such workload", "no such workload"),
        ("10 busy turns", "10 busy turns"),
        ("session idle", "session idle"),
        ("session start,", "session start,"),
        ("session start,", "is not a timing workload"),
    ] {
        let err = argv(&["--fiber", "f", "--out", "o", "--skip-rows", value]).unwrap_err();
        assert!(err.contains(said), "{value}: {err}");
    }
    // An empty value skips nothing; a missing value needs one.
    assert_eq!(
        argv(&["--fiber", "f", "--out", "o", "--skip-rows", ""])
            .unwrap()
            .skip_rows,
        Vec::<String>::new()
    );
    let err = parse(
        [
            "--fiber".to_owned(),
            "f".to_owned(),
            "--out".to_owned(),
            "o".to_owned(),
            "--skip-rows".to_owned(),
        ]
        .into_iter(),
    )
    .unwrap_err();
    assert!(err.contains("needs a value"), "{err}");
}

#[test]
fn the_results_document_records_skipped_rows_after_ten_turns() {
    let parsed = argv(&[
        "--fiber",
        "f",
        "--out",
        "o",
        "--skip-rows",
        "session start,paging",
        "--skip-ten-turns",
    ])
    .unwrap();
    let recorded = document(
        &parsed,
        "CPU-X",
        None,
        BTreeMap::new(),
        BTreeMap::new(),
        vec![],
    );
    assert_eq!(
        recorded["skipped"],
        json!(["10 busy turns", "session start", "paging"])
    );
    let parsed = argv(&["--fiber", "f", "--out", "o", "--skip-rows", "paging"]).unwrap();
    let recorded = document(
        &parsed,
        "CPU-X",
        None,
        BTreeMap::new(),
        BTreeMap::new(),
        vec![],
    );
    assert_eq!(recorded["skipped"], json!(["paging"]));
}

#[test]
fn skip_rows_names_exactly_the_timing_workloads() {
    // The six names mapped in `xtask/src/select/plan_tests.rs`, plus
    // session_list, which runs on every base pass.
    let literal = [
        "session start",
        "terminal first frame",
        "session_list",
        "paging",
        "sessions list",
        "terminal attach",
        "terminal wheel",
    ];
    assert_eq!(timing_names(), literal);
    let every: Vec<&str> = idle::WORKLOADS
        .iter()
        .chain(&busy::WORKLOADS)
        .chain(&busy::ONCE)
        .chain(&resume::WORKLOADS)
        .chain(&session_list::WORKLOADS)
        .chain(&paging::WORKLOADS)
        .chain(listing::SAMPLED.iter())
        .chain(attach::SAMPLED.iter())
        .chain(wheel::SAMPLED.iter())
        .filter(|workload| workload.timing)
        .map(|workload| workload.name)
        .collect();
    assert_eq!(every, literal);
}

#[test]
fn skip_ten_turns_leaves_the_workload_out_and_records_it() {
    assert!(!args("--fiber f --out o").unwrap().skip_ten_turns);
    let parsed = args("--fiber f --out o --skip-ten-turns").unwrap();
    assert!(parsed.skip_ten_turns);
    // The flag is a switch: it takes no value.
    assert!(args("--fiber f --out o --skip-ten-turns --runs 3").is_ok());
    let all = names("--fiber f --out o");
    assert!(all.contains(&"10 busy turns"), "{all:?}");
    let skipped = names("--fiber f --out o --skip-ten-turns");
    assert!(!skipped.contains(&"10 busy turns"), "{skipped:?}");
    assert_eq!(skipped.len() + 1, all.len());
    // The record names the skipped workload, so the judge reports it
    // instead of failing its missing metric; without the flag nothing
    // was skipped.
    let metrics: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    let recorded = document(
        &parsed,
        "CPU-X",
        None,
        metrics.clone(),
        BTreeMap::new(),
        vec![],
    );
    assert_eq!(recorded["skipped"], json!(["10 busy turns"]));
    let plain = document(
        &args("--fiber f --out o").unwrap(),
        "CPU-X",
        None,
        metrics,
        BTreeMap::new(),
        vec![],
    );
    assert_eq!(plain["skipped"], json!([]));
}

#[test]
fn the_results_document_carries_cpu_and_counts() {
    let parsed = args("--fiber f --out o --runs 5 --timing-runs 3").unwrap();
    let metrics: BTreeMap<&'static str, Vec<Value>> =
        BTreeMap::from([("session_start_ms", vec![json!(6.0); 3])]);
    let metric_runs: BTreeMap<&'static str, u32> = BTreeMap::from([("session_start_ms", 3)]);
    let document = document(
        &parsed,
        "CPU-X",
        None,
        metrics.clone(),
        metric_runs.clone(),
        vec![],
    );
    assert_eq!(document["schema"], json!(1));
    assert_eq!(document["runs"], json!(5));
    assert_eq!(document["timing_runs"], json!(3));
    assert_eq!(document["cpu"], json!("CPU-X"));
    assert_eq!(document["idle_secs"], json!(10));
    assert_eq!(
        document["metrics"]["session_start_ms"],
        json!([6.0, 6.0, 6.0])
    );
    assert_eq!(document["metric_runs"]["session_start_ms"], json!(3));
    assert_eq!(document["failures"], json!([]));
}

#[test]
fn a_missing_fiber_records_no_size_and_both_failures() {
    let dir = fakes::TempDir::new("bench-size");
    let missing = dir.path().join("fiber");
    let parsed = parse(
        [
            "--fiber".to_owned(),
            missing.display().to_string(),
            "--out".to_owned(),
            "o".to_owned(),
        ]
        .into_iter(),
    )
    .unwrap();
    let (result, base) = bench(&parsed, "CPU-X");
    assert!(base.is_none());
    assert!(result.get("binary_size_bytes").is_none());
    let failures = result["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 2, "{failures:?}");
    let first = failures[0].as_str().unwrap();
    assert!(
        first.starts_with("binary size of ") && first.contains(&missing.display().to_string()),
        "{first}"
    );
    assert!(
        failures[1].as_str().unwrap().starts_with("setup: "),
        "{failures:?}"
    );
}

#[test]
fn a_fiber_that_cannot_be_copied_is_still_measured() {
    let dir = fakes::TempDir::new("bench-size");
    let parsed = parse(
        [
            "--fiber".to_owned(),
            dir.path().display().to_string(),
            "--out".to_owned(),
            "o".to_owned(),
        ]
        .into_iter(),
    )
    .unwrap();
    let (result, base) = bench(&parsed, "CPU-X");
    assert!(base.is_none());
    let size = std::fs::metadata(dir.path()).unwrap().len();
    assert!(size > 1, "{size}");
    assert_eq!(result["binary_size_bytes"], json!(size));
    assert!(
        result.to_string().contains("\"binary_size_bytes\":"),
        "{result}"
    );
    let failures = result["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 1, "{failures:?}");
    let only = failures[0].as_str().unwrap();
    assert!(only.starts_with("setup: "), "{only}");
    assert!(!only.contains("binary size of"), "{only}");
}

#[test]
fn a_missing_base_binary_leaves_the_head_untouched() {
    // Both binaries are missing, so neither side runs a benchmark: only
    // the setup failures are recorded.
    let dir = fakes::TempDir::new("bench-base");
    let missing_head = dir.path().join("head-fiber");
    let missing_base = dir.path().join("base-fiber");
    let plain = parse(
        [
            "--fiber".to_owned(),
            missing_head.display().to_string(),
            "--out".to_owned(),
            "o".to_owned(),
        ]
        .into_iter(),
    )
    .unwrap();
    let paired = parse(
        [
            "--fiber".to_owned(),
            missing_head.display().to_string(),
            "--out".to_owned(),
            "o".to_owned(),
            "--base-fiber".to_owned(),
            missing_base.display().to_string(),
            "--base-out".to_owned(),
            "ob".to_owned(),
        ]
        .into_iter(),
    )
    .unwrap();
    let (alone, no_base) = bench(&plain, "CPU-X");
    let (head, base) = bench(&paired, "CPU-X");
    assert!(no_base.is_none());
    assert_eq!(head, alone);
    let base = base.unwrap();
    assert!(base["metrics"].as_object().unwrap().is_empty());
    let failures = base["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(
        failures[0].as_str().unwrap().starts_with("binary size of "),
        "{failures:?}"
    );
    assert!(
        failures[1].as_str().unwrap().starts_with("setup: "),
        "{failures:?}"
    );
}

#[test]
fn model_name_reads_the_first_model_name_line() {
    let cpuinfo = "processor\t: 0\nmodel name\t: AMD EPYC 7763 64-Core Processor\n\
         processor\t: 1\nmodel name\t: AMD EPYC 7763 64-Core Processor\n";
    assert_eq!(model_name(cpuinfo), Some("AMD EPYC 7763 64-Core Processor"));
    assert_eq!(model_name("processor\t: 0\n"), None);
    assert_eq!(model_name("model name\t:   \n"), None);
    assert_eq!(model_name("model names\t: x\n"), None);
    assert_eq!(
        model_name("  model name  :   padded value  \n"),
        Some("padded value")
    );
}

#[test]
#[cfg(target_os = "linux")]
fn host_cpu_reads_the_model_name_on_linux() {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap();
    assert_eq!(
        host_cpu(),
        model_name(&cpuinfo).unwrap_or("unknown").to_owned()
    );
}

#[test]
#[cfg(target_os = "macos")]
fn brand_string_reads_sysctl_on_macos() {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;
    use std::time::Duration;

    // The live read: `sysctl` under a deadline, in its own process group.
    let live = brand_string(|command| {
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        Some(fakes::finished(
            "sysctl",
            child,
            &[],
            fakes::Deadline::after(Duration::from_secs(30)),
        ))
    });
    assert!(!live.is_empty() && live != "unknown", "{live:?}");
    assert_eq!(brand_string(|_| None), "unknown");
    assert_eq!(
        brand_string(|_| Some(std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: b"  \n".to_vec(),
            stderr: Vec::new(),
        })),
        "unknown"
    );
    assert_eq!(
        brand_string(|_| Some(std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: b"Apple M3\n".to_vec(),
            stderr: Vec::new(),
        })),
        "Apple M3"
    );
}
