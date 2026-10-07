use std::path::PathBuf;

use super::{Args, Only, parse};

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
            idle_secs: 10,
            only: Only::All,
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
            idle_secs: 60,
            only: Only::Timing,
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
        ("--fiber f --out o --only some", "all or timing"),
        ("--fiber f --out o --fast 1", "unknown argument"),
    ] {
        let err = args(line).unwrap_err();
        assert!(err.contains(said), "{line}: {err}");
    }
}
