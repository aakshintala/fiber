//! `fiber import <source>` (`docs/configuration.md`, "Importing";
//! `docs/invocation.md`, "Commands and flags"): copies another harness's
//! configuration into Fiber home once, printing what landed and what was
//! left behind with reasons. Names only, never secret values.

use std::io::{self, Write};
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

/// Resolves Fiber home and the person's home from the environment, then
/// runs the import against those paths.
fn run(source: ImportSource, clock: &dyn Clock) -> Result<(), CliError> {
    let home = config::fiber_home_from_env()?;
    let Some(home_env) = std::env::var_os("HOME") else {
        return Err(CliError::Forwarded(doors::failure(
            contract::ErrorCode::IoFailed,
            "HOME is not set, so the source home cannot be found.",
        )));
    };
    run_import(
        &home,
        Path::new(&home_env),
        source,
        clock,
        &mut io::stdout(),
    )
}

/// Runs the import against explicit paths, printing what landed and what
/// was left behind with reasons.
fn run_import(
    home: &Path,
    home_env: &Path,
    source: ImportSource,
    clock: &dyn Clock,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let report =
        config::import_source(home, &config::source_home(home_env, source), source, clock)?;
    for name in &report.imported.skills {
        write_line(out, &format!("imported skill {name}"))?;
    }
    for name in &report.imported.commands {
        write_line(out, &format!("imported command {name}"))?;
    }
    for name in &report.imported.instructions {
        write_line(out, &format!("imported instruction {name}"))?;
    }
    for name in &report.imported.mcp_servers {
        write_line(out, &format!("imported server {name}"))?;
    }
    for left in &report.left_behind {
        write_line(out, &format!("left behind {}: {}", left.item, left.reason))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failure is the test's")]

    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use fakes::TempDir;
    use fakes::clock::FakeClock;
    use serde_json::json;

    use super::*;

    /// One Fiber home beside one person's home holding the source home.
    struct Fixture {
        tmp: TempDir,
        clock: Arc<FakeClock>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                tmp: TempDir::new("fiber-import-cli"),
                clock: FakeClock::new(),
            }
        }

        fn home(&self) -> PathBuf {
            self.tmp.path().join("home")
        }

        fn source(&self, source: ImportSource) -> PathBuf {
            self.tmp.path().join(match source {
                ImportSource::ClaudeCode => ".claude",
                ImportSource::Codex => ".codex",
            })
        }

        /// Runs the import with in-memory output.
        fn run(&self, source: ImportSource) -> (Result<(), CliError>, String) {
            let mut out = Vec::new();
            let result = run_import(
                &self.home(),
                self.tmp.path(),
                source,
                self.clock.as_ref(),
                &mut out,
            );
            (result, String::from_utf8_lossy(&out).into_owned())
        }
    }

    /// Every file under `dir` holding `needle`.
    fn files_holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        collect_holding(dir, needle, &mut found);
        found.sort();
        found
    }

    fn collect_holding(dir: &Path, needle: &str, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && !path.is_symlink() {
                collect_holding(&path, needle, found);
            } else if path.is_file()
                && fs::read(&path)
                    .map(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
                    .unwrap_or(false)
            {
                found.push(path);
            }
        }
    }

    #[test]
    fn lands_items_and_prints_names_only() {
        let fixture = Fixture::new();
        let source = fixture.source(ImportSource::ClaudeCode);
        fs::create_dir_all(source.join("skills/review")).unwrap();
        fs::write(
            source.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Reviews.\n---\n\nBody.\n",
        )
        .unwrap();
        fs::create_dir_all(source.join("commands")).unwrap();
        fs::write(source.join("commands/greet.md"), "Hello.\n").unwrap();
        fs::write(source.join("CLAUDE.md"), "Be kind.\n").unwrap();
        fs::write(
            source.join("settings.json"),
            json!({"mcpServers": {"notes": {"command": "notes-server"}}}).to_string(),
        )
        .unwrap();
        let (result, out) = fixture.run(ImportSource::ClaudeCode);
        result.unwrap();
        assert_eq!(
            out,
            "imported skill review\n\
             imported command greet\n\
             imported instruction CLAUDE.md\n\
             imported server notes\n"
        );
        assert!(fixture.home().join("skills/review/SKILL.md").exists());
        assert!(fixture.home().join("skills/greet/SKILL.md").exists());
        assert!(fixture.home().join("CLAUDE.md").exists());
    }

    #[test]
    fn left_behind_lines_name_items_with_reasons() {
        let fixture = Fixture::new();
        let source = fixture.source(ImportSource::Codex);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("auth.json"), "{}\n").unwrap();
        let (result, out) = fixture.run(ImportSource::Codex);
        result.unwrap();
        assert_eq!(
            out,
            "left behind auth.json: credentials are never imported\n"
        );
    }

    #[test]
    fn secret_values_reach_neither_files_nor_stdout() {
        let fixture = Fixture::new();
        let source = fixture.source(ImportSource::ClaudeCode);
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("settings.json"),
            json!({"mcpServers": {
                "vault": {"command": "v", "env": {"VAULT_TOKEN": "sk-canary-5"}},
            }})
            .to_string(),
        )
        .unwrap();
        let (result, out) = fixture.run(ImportSource::ClaudeCode);
        result.unwrap();
        assert!(out.contains("server vault"), "{out}");
        assert!(!out.contains("sk-canary-5"), "{out}");
        assert!(files_holding(&fixture.home(), "sk-canary-5").is_empty());
    }

    #[test]
    fn a_missing_source_home_imports_nothing_and_prints_nothing() {
        let fixture = Fixture::new();
        let (result, out) = fixture.run(ImportSource::Codex);
        result.unwrap();
        assert_eq!(out, "");
        assert!(fixture.home().join("import-codex.json").exists());
    }

    #[test]
    fn creates_fiber_home() {
        let fixture = Fixture::new();
        let source = fixture.source(ImportSource::ClaudeCode);
        fs::create_dir_all(source.join("skills/review")).unwrap();
        fs::write(
            source.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Reviews.\n---\n\nBody.\n",
        )
        .unwrap();
        assert!(!fixture.home().exists());
        let (result, _) = fixture.run(ImportSource::ClaudeCode);
        result.unwrap();
        assert!(fixture.home().join("skills/review/SKILL.md").exists());
    }
}
