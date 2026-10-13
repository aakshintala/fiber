use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

#[cfg(target_os = "macos")]
use super::brand_string;
#[cfg(target_os = "linux")]
use super::host_cpu;
use super::{Args, Only, bench, document, measure_all, model_name, parse, selected};

fn args(line: &str) -> Result<Args, String> {
    parse(line.split_whitespace().map(str::to_owned))
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
fn a_pull_request_runs_timing_only_workloads_three_times_and_the_rest_five() {
    let got = counts("--fiber f --out o --runs 5 --timing-runs 3 --paging j");
    assert_eq!(
        got,
        BTreeMap::from([
            ("session start", (3, 3)),
            ("terminal first frame", (3, 3)),
            ("session idle", (5, 5)),
            ("terminal idle", (5, 5)),
            ("busy turn", (5, 5)),
            ("web_fetch", (5, 5)),
            ("resume 20,000 tokens", (5, 5)),
            ("resume 2,000,000 tokens", (5, 5)),
            ("paging", (5, 5)),
            ("fsyncs", (1, 1)),
            ("10 busy turns", (1, 1)),
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
            &"sessions list",
            &"terminal attach",
            &"terminal first frame",
            &"terminal wheel",
        ]
    );
    assert_eq!(timing["paging"], (5, 5));
    assert_eq!(timing["sessions list"], (1, 3));
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
    let expected: Vec<(&str, u32, u32)> = selected(&parsed)
        .iter()
        .map(|(workload, repeats, runs)| (workload.name, *repeats, *runs))
        .collect();
    let mut log = Vec::new();
    let mut failures = Vec::new();
    let (metrics, metric_runs) =
        measure_all(&parsed, &mut failures, |workload, repeats, runs, _| {
            log.push((workload.name, repeats, runs));
            if workload.name == "busy turn" {
                return None;
            }
            let mut samples: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
            samples.insert(
                Box::leak(format!("{}_m", workload.name).into_boxed_str()),
                vec![json!(1.0); runs as usize],
            );
            Some(samples)
        });
    assert_eq!(log, expected);
    assert_eq!(metric_runs["session start_m"], 3);
    assert_eq!(metric_runs["sessions list_m"], 3);
    assert_eq!(metric_runs["terminal attach_m"], 5);
    assert_eq!(metric_runs["fsyncs_m"], 1);
    assert_eq!(metrics["session start_m"].len(), 3);
    assert!(!metrics.contains_key("busy turn_m"));
    assert!(!metric_runs.contains_key("busy turn_m"));
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
    let result = bench(&parsed, "CPU-X");
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
    let result = bench(&parsed, "CPU-X");
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
            Duration::from_secs(30),
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
