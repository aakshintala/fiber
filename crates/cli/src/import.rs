//! `fiber import <source>` (`docs/configuration.md`, "Importing";
//! `docs/invocation.md`, "Commands and flags"): copies another harness's
//! configuration into Fiber home once, printing what landed and what was
//! left behind with reasons. Names only, never secret values.

use std::io;
use std::path::Path;

use config::ImportSource;
use contract::clock::Clock;

use crate::{CliError, finish, write_line};

/// Copies another harness's configuration into Fiber home once, printing
/// what landed and what was left behind with reasons: names only, never
/// secret values.
pub fn import(source: ImportSource, clock: &dyn Clock) -> i32 {
    finish(run(source, clock))
}

/// Resolves Fiber home and the person's home from the environment, runs
/// the import against those paths, and prints what landed and what was
/// left behind with reasons.
fn run(source: ImportSource, clock: &dyn Clock) -> Result<(), CliError> {
    let home = config::fiber_home_from_env()?;
    let Some(home_env) = std::env::var_os("HOME") else {
        return Err(CliError::Forwarded(doors::failure(
            contract::ErrorCode::IoFailed,
            "HOME is not set, so the source home cannot be found.",
        )));
    };
    let report = config::import_source(
        &home,
        &config::source_home(Path::new(&home_env), source),
        source,
        clock,
    )?;
    let mut out = io::stdout();
    for name in &report.imported.skills {
        write_line(&mut out, &format!("imported skill {name}"))?;
    }
    for name in &report.imported.commands {
        write_line(&mut out, &format!("imported command {name}"))?;
    }
    for name in &report.imported.instructions {
        write_line(&mut out, &format!("imported instruction {name}"))?;
    }
    for name in &report.imported.mcp_servers {
        write_line(&mut out, &format!("imported server {name}"))?;
    }
    for left in &report.left_behind {
        write_line(
            &mut out,
            &format!("left behind {}: {}", left.item, left.reason),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failure is the test's")]

    use std::time::Duration;

    use fakes::TempDir;
    use fakes::clock::FakeClock;

    use super::*;

    /// The child's marker: set, the test runs the entry and exits with
    /// its code, so the parent proves what `import` returns.
    const CHILD: &str = "FIBER_CLI_IMPORT_CHILD";

    /// How long the re-run child may run before the test kills it and
    /// fails: the import reads local files only.
    const CHILD_WITHIN: Duration = Duration::from_secs(10);

    fn child_name() -> String {
        module_path!().split_once("::").unwrap().1.to_owned()
    }

    /// The only child-process test here: the success path's exit code,
    /// output and landed files are pinned by `crates/main/tests/import.rs`.
    /// What only a child proves is the branch `run` takes before any
    /// import: no `HOME` in the environment.
    #[test]
    fn import_without_a_home_dir_exits_nonzero() {
        if std::env::var_os(CHILD).is_some() {
            let clock = FakeClock::new();
            std::process::exit(import(ImportSource::Codex, clock.as_ref()));
        }
        let fixture = TempDir::new("fiber-import-cli");
        let output = fakes::rerun_prepared(
            &format!("{}::import_without_a_home_dir_exits_nonzero", child_name()),
            &[(CHILD, "1")],
            CHILD_WITHIN,
            |command| {
                command
                    .env("FIBER_HOME", fixture.path().join("home"))
                    .env_remove("HOME");
            },
        );
        assert_ne!(output.status.code(), Some(0), "{output:?}");
    }
}
