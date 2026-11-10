//! Person-run sweep runner (`docs/testing/fakes.md`, "Live calls and
//! evals"): runs the committed labelled set and the committed session
//! directories `--runs` times per `--models` reviewer model (3 by default,
//! back to back for cache reuse) through the public `routing::reviewer`
//! seam and prints the per-entry, per-run and per-model tables. A sweep
//! planning more than the 170-request budget is rejected before it drives
//! anything. `--check` only parses and counts a file. Live calls are opt-in
//! by running this example with `--live`; no test can enable one.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the runner's table and counts are its interface"
)]

#[path = "../tests/support/review_sweep.rs"]
mod sweep;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use contract::clock::Clock;

fn main() -> ExitCode {
    match run(&std::env::args().collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("review_sweep: {error}");
            ExitCode::from(2)
        }
    }
}

/// The committed fixture roots `--dirs` folds against: the loop crate's
/// fixture directories beside this package.
fn fixture_roots() -> (PathBuf, PathBuf) {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../loop/tests/fixtures");
    (
        base.join("reviewer_consent"),
        base.join(sweep::PERSON_INPUT_DIR),
    )
}

fn run(argv: &[String]) -> Result<(), String> {
    let mode = sweep::parse_args(argv)?;
    sweep::check_live(&mode)?;
    match mode {
        sweep::Mode::Check { file } => {
            let entries = sweep::read_set_file(&file)?;
            println!("{}: {} lines", file.display(), entries.len());
            Ok(())
        }
        sweep::Mode::Sweep {
            models,
            set,
            dirs,
            runs,
            live,
        } => {
            let mut entries = Vec::new();
            let mut srcs = Vec::new();
            if let Some(set) = set {
                for entry in sweep::read_set_file(&set)? {
                    srcs.push("set");
                    entries.push(entry);
                }
            }
            let (consent, person_input) = fixture_roots();
            let mut dropped = 0usize;
            for dir in &dirs {
                let folded = sweep::fold_session_dir(dir, &consent, &person_input)?;
                dropped = dropped.saturating_add(folded.unreplayable);
                for entry in folded.entries {
                    srcs.push("dir");
                    entries.push(entry);
                }
            }
            if dropped > 0 {
                eprintln!("review_sweep: dropped {dropped} unreplayable calls");
            }
            sweep::check_call_cap(models.len(), entries.len(), runs)?;
            let home = config::fiber_home_path_from_env().map_err(|error| error.to_string())?;
            let workspace = std::env::current_dir()
                .map_err(|error| format!("the current directory: {error}"))?;
            let clock: Arc<dyn Clock> = Arc::new(fakes::clock::SystemClock);
            let mut reports = Vec::with_capacity(models.len());
            let budget = Arc::new(sweep::RequestBudget::new());
            for model in &models {
                let (provider, sweep_model) =
                    sweep::reviewer_provider(&home, &workspace, model, &clock)?;
                let (tallies, outcomes, truncated) =
                    sweep::sweep(&entries, runs, live, &budget, |entry| {
                        sweep::drive(entry, &provider, &clock, &sweep_model, &budget)
                    })?;
                for outcome in &outcomes {
                    if let sweep::Outcome::Errored(errored) = outcome {
                        eprintln!(
                            "review_sweep: error: {model} {}: {}",
                            errored.id, errored.reason
                        );
                    }
                }
                // Each row carries the src column beside its entry, so set
                // rows and dir rows stay apart under twin ids.
                let rows: Vec<sweep::Sourced> = entries
                    .iter()
                    .zip(srcs.iter())
                    .map(|(entry, src)| sweep::Sourced { src, entry })
                    .collect();
                print!("{}", sweep::render_entry_table(&tallies, runs));
                print!("{}", sweep::render_run_table(&rows, &outcomes, runs));
                print!("{}", sweep::render_run_detail(&rows, &outcomes, runs));
                if truncated {
                    eprintln!(
                        "review_sweep: truncated at the {}-request budget: later runs stay unrun",
                        budget.limit
                    );
                }
                reports.push(sweep::score(model, &outcomes));
            }
            print!("{}", sweep::render_table(&reports));
            Ok(())
        }
    }
}
