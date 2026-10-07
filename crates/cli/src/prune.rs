//! `fiber sessions prune` (`docs/invocation.md`, "Deleting and pruning"):
//! deletes old exited sessions through the hub, and deletes old diagnostic
//! logs and crash files, with `--dry-run`, `--yes` and the space freed.

use std::io::{self, BufReader, IsTerminal, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::SystemTime;

use contract::clock::Clock;
use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};

mod diagnostics;
mod sessions;

use crate::approve::{confirmed, say};
use crate::sessions::Ask;
use crate::{fail, failed, usage};

pub(crate) use diagnostics::{OldFile, old_diagnostics};

/// What the prompt asks.
const PRUNE_PROMPT: &str = "prune? [y/N]";

/// The usage failure when there is nobody to ask.
const PRUNE_NOBODY: &str =
    "`fiber sessions prune` has no terminal to ask on. Pass `--yes` to prune without asking.";

const OLDER_THAN_USAGE: &str =
    "`--older-than` takes a duration such as `30d`: a whole number and `s`, `m`, `h` or `d`.";

/// `fiber sessions prune`'s arguments.
pub struct PruneArgs {
    /// Deletes every exited session whose last line is older than this,
    /// such as `30d`. Without it no session is deleted.
    pub older_than: Option<String>,
    /// Deletes with `cascade: true`, listing every dependent.
    pub cascade: bool,
    /// Prints what would be deleted and frees nothing.
    pub dry_run: bool,
    /// Prunes without asking.
    pub yes: bool,
}

/// `fiber sessions prune` in the current directory.
pub fn prune(
    args: &PruneArgs,
    clock: &dyn Clock,
    connect: &mut dyn FnMut() -> io::Result<doors::hub::Hub>,
) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            let ask = Ask {
                yes: args.yes,
                terminal,
                input: &mut stdin.lock(),
                err: &mut io::stderr(),
            };
            prune_run(
                &home,
                &workspace,
                args,
                clock.wall(),
                ask,
                &mut io::stdout(),
                connect,
            )
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// Runs prune for `home` and `workspace` at `now`: lists, asks unless
/// `yes` or `dry_run`, deletes diagnostics then sessions, and prints the
/// space freed. `--dry-run` never connects and never asks.
fn prune_run(
    home: &Path,
    workspace: &Path,
    args: &PruneArgs,
    now: SystemTime,
    ask: Ask<'_>,
    out: &mut dyn Write,
    connect: &mut dyn FnMut() -> io::Result<doors::hub::Hub>,
) -> Result<(), Failure> {
    let older_than = match &args.older_than {
        None => None,
        Some(text) => match config::parse_duration(text) {
            Some(duration) => Some(duration),
            None => return Err(usage(OLDER_THAN_USAGE)),
        },
    };
    // No terminal and no `--yes` is a usage error before anything is
    // scanned. `--dry-run` never asks and needs no terminal.
    if !args.dry_run && !ask.yes && !ask.terminal {
        return Err(usage(PRUNE_NOBODY));
    }
    let logs = old_diagnostics(&home.join("logs"), now);
    let crashes = old_diagnostics(&home.join("crashes"), now);
    let selected = sessions::select(home, workspace, older_than, args.cascade, now);
    let has_sessions = !selected.deletes.is_empty();
    let has_diagnostics = !logs.is_empty() || !crashes.is_empty();
    if !has_sessions && !has_diagnostics {
        print_rows(out, &selected.rows, &logs, &crashes, now)?;
        writeln!(out, "freed {}", format_size(0))
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
        return Ok(());
    }
    let would_free =
        session_listed_bytes(&selected.rows) + diag_bytes(&logs) + diag_bytes(&crashes);
    if args.dry_run {
        print_rows(out, &selected.rows, &logs, &crashes, now)?;
        writeln!(out, "would free {}", format_size(would_free))
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
        return Ok(());
    }
    print_rows(out, &selected.rows, &logs, &crashes, now)?;
    if !ask.yes && !confirmed(PRUNE_PROMPT, PRUNE_NOBODY, ask.terminal, ask.input, ask.err)? {
        say(ask.err, "nothing deleted\n");
        return Ok(());
    }
    let mut freed = 0_u64;
    freed += remove_diagnostics(&logs)?;
    freed += remove_diagnostics(&crashes)?;
    if !has_sessions {
        writeln!(out, "freed {}", format_size(freed))
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
        return Ok(());
    }
    let stream = match connect() {
        Ok((stream, _)) => stream,
        Err(e) => {
            let message = format!("the hub: {e}");
            return reconcile_no_connection(out, &selected, freed, ask.err, &message);
        }
    };
    let mut read = BufReader::new(stream);
    let mut results: Vec<Result<(), Failure>> = Vec::with_capacity(selected.deletes.len());
    for (n, delete) in selected.deletes.iter().enumerate() {
        let command_id = format!("c_prune_{}", n + 1);
        results.push(crate::sessions::send_delete(
            &mut read,
            &command_id,
            &delete.id,
            delete.cascade,
        ));
    }
    reconcile(out, &selected, &results, freed, ask.err)
}

/// Whether `cwd` is inside a git repository: `git -C cwd rev-parse
/// --git-dir` exits 0, with stdin and stderr null. A failure to spawn git
/// counts as outside.
fn in_repository(cwd: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-dir"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Prints every row: sessions by id, then `logs/` then `crashes/` by file
/// name, fields separated by two spaces.
fn print_rows(
    out: &mut dyn Write,
    sessions: &[sessions::SessionRow],
    logs: &[OldFile],
    crashes: &[OldFile],
    now: SystemTime,
) -> Result<(), Failure> {
    let failed = |e: io::Error| failed(ErrorCode::IoFailed, format!("standard output: {e}"));
    for row in sessions {
        writeln!(out, "{}", session_line(row)).map_err(failed)?;
    }
    for file in logs {
        writeln!(out, "{}", diag_line("log", &file.path, file.bytes, now)).map_err(failed)?;
    }
    for file in crashes {
        writeln!(out, "{}", diag_line("crash", &file.path, file.bytes, now)).map_err(failed)?;
    }
    Ok(())
}

/// One session row, fields separated by two spaces.
fn session_line(row: &sessions::SessionRow) -> String {
    match row {
        sessions::SessionRow::Deletable {
            id,
            age_days,
            bytes,
            ..
        } => {
            format!(
                "session  {id}  {}  {}",
                format_age(*age_days),
                format_size(*bytes)
            )
        }
        sessions::SessionRow::Blocked {
            id,
            age_days,
            blockers,
        } => {
            let who = blockers.join(", ");
            let verb = if blockers.len() == 1 {
                "continues it"
            } else {
                "continue it"
            };
            format!(
                "session  {id}  {}  skipped: {who} {verb}",
                format_age(*age_days)
            )
        }
        sessions::SessionRow::Cycle {
            id,
            age_days,
            members,
        } => {
            if members.len() == 1 {
                format!(
                    "session  {id}  {}  skipped: {} continues itself",
                    format_age(*age_days),
                    members.join(", ")
                )
            } else {
                format!(
                    "session  {id}  {}  skipped: {} continue each other",
                    format_age(*age_days),
                    members.join(", ")
                )
            }
        }
        sessions::SessionRow::Unreadable { id } => {
            format!("session  {id}  skipped: its last line does not read")
        }
        sessions::SessionRow::Continues {
            id,
            age_days,
            bytes,
            parent,
            ..
        } => {
            format!(
                "session  {id}  {}  {}  continues {parent}",
                format_age(*age_days),
                format_size(*bytes)
            )
        }
    }
}

/// One diagnostics row: its kind, file name, age and size.
fn diag_line(kind: &str, path: &Path, bytes: u64, now: SystemTime) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "{kind}  {name}  {}  {}",
        diag_age(path, now),
        format_size(bytes)
    )
}

/// A diagnostics file's age in whole days, floored: 0 when its mtime does
/// not read or lies in the future.
fn diag_age(path: &Path, now: SystemTime) -> String {
    format_age(diag_age_days(path, now))
}

/// A diagnostics file's age in whole days, floored.
fn diag_age_days(path: &Path, now: SystemTime) -> u64 {
    let at = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    now.duration_since(at)
        .map(|d| d.as_secs() / (24 * 60 * 60))
        .unwrap_or(0)
}

/// Whole days, floored.
fn format_age(days: u64) -> String {
    format!("{days}d")
}

/// Logical bytes: `N B` below 1024, otherwise one decimal in the largest
/// of KiB, MiB or GiB that is at least 1.
fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let count = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if count >= GIB {
        format!("{:.1} GiB", count / GIB)
    } else if count >= MIB {
        format!("{:.1} MiB", count / MIB)
    } else {
        format!("{:.1} KiB", count / KIB)
    }
}

/// The listed bytes of every session row prune deletes: each deletable and
/// each cascade dependent.
fn session_listed_bytes(rows: &[sessions::SessionRow]) -> u64 {
    rows.iter()
        .map(|row| match row {
            sessions::SessionRow::Deletable { bytes, .. }
            | sessions::SessionRow::Continues { bytes, .. } => *bytes,
            sessions::SessionRow::Blocked { .. }
            | sessions::SessionRow::Cycle { .. }
            | sessions::SessionRow::Unreadable { .. } => 0,
        })
        .sum()
}

/// The listed bytes of diagnostics rows.
fn diag_bytes(files: &[OldFile]) -> u64 {
    files.iter().map(|file| file.bytes).sum()
}

/// Removes every diagnostics file: a `NotFound` is neither a failure nor
/// counted. Gives the bytes removed.
fn remove_diagnostics(files: &[OldFile]) -> Result<u64, Failure> {
    let mut freed = 0_u64;
    for file in files {
        match std::fs::remove_file(&file.path) {
            Ok(()) => freed += file.bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(failed(
                    ErrorCode::IoFailed,
                    format!("{}: {e}", file.path.display()),
                ));
            }
        }
    }
    Ok(freed)
}

/// Reconciles after a failed hub connection, when no `delete` was
/// accepted: every session row whose directory remains is a failure.
fn reconcile_no_connection(
    out: &mut dyn Write,
    selected: &sessions::Selected,
    diag_freed: u64,
    err: &mut dyn Write,
    message: &str,
) -> Result<(), Failure> {
    let mut freed = diag_freed;
    let mut failures: Vec<(String, ErrorCode, String)> = Vec::new();
    for row in &selected.rows {
        let (id, dir, bytes) = match row {
            sessions::SessionRow::Deletable { id, bytes, dir, .. }
            | sessions::SessionRow::Continues { id, bytes, dir, .. } => (id, dir, *bytes),
            sessions::SessionRow::Blocked { .. }
            | sessions::SessionRow::Cycle { .. }
            | sessions::SessionRow::Unreadable { .. } => continue,
        };
        match log::remaining(dir) {
            Ok(None) => freed += bytes,
            Ok(Some(left)) => {
                freed += bytes.saturating_sub(left);
                failures.push((id.clone(), ErrorCode::IoFailed, message.to_owned()));
            }
            Err(_) => {
                failures.push((id.clone(), ErrorCode::IoFailed, message.to_owned()));
            }
        }
    }
    finish(out, err, &selected.rows, freed, failures)
}

/// Reconciles each session row with `log::remaining` after every `delete`
/// has been answered: freed is listed minus remaining, and a row whose
/// `delete` was not accepted and whose directory remains is a failure
/// keeping the hub's code and message.
fn reconcile(
    out: &mut dyn Write,
    selected: &sessions::Selected,
    results: &[Result<(), Failure>],
    diag_freed: u64,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let mut freed = diag_freed;
    let mut failures: Vec<(String, ErrorCode, String)> = Vec::new();
    for row in &selected.rows {
        let (id, dir, bytes, delete) = match row {
            sessions::SessionRow::Deletable {
                id,
                bytes,
                dir,
                delete,
                ..
            }
            | sessions::SessionRow::Continues {
                id,
                bytes,
                dir,
                delete,
                ..
            } => (id, dir, *bytes, *delete),
            sessions::SessionRow::Blocked { .. }
            | sessions::SessionRow::Cycle { .. }
            | sessions::SessionRow::Unreadable { .. } => continue,
        };
        let accepted = results.get(delete).is_some_and(Result::is_ok);
        let hub_failure = match results.get(delete) {
            Some(Err(failure)) => Some(failure),
            Some(Ok(())) | None => None,
        };
        match log::remaining(dir) {
            Ok(None) => {
                freed += bytes;
            }
            Ok(Some(left)) => {
                freed += bytes.saturating_sub(left);
                if !accepted && let Some(failure) = hub_failure {
                    failures.push((id.clone(), failure.code.clone(), failure.message.clone()));
                }
            }
            Err(_) => {
                if let Some(failure) = hub_failure {
                    failures.push((id.clone(), failure.code.clone(), failure.message.clone()));
                } else {
                    failures.push((
                        id.clone(),
                        ErrorCode::IoFailed,
                        format!("{id} could not be read after pruning"),
                    ));
                }
            }
        }
    }
    finish(out, err, &selected.rows, freed, failures)
}

/// Prints `freed`, and when any deletion failed the per-row stderr lines
/// and the `<n>` of `<m>` failure with the first failure's code.
fn finish(
    out: &mut dyn Write,
    err: &mut dyn Write,
    rows: &[sessions::SessionRow],
    freed: u64,
    failures: Vec<(String, ErrorCode, String)>,
) -> Result<(), Failure> {
    writeln!(out, "freed {}", format_size(freed))
        .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
    if failures.is_empty() {
        return Ok(());
    }
    for (id, _, message) in &failures {
        say(
            err,
            &format!("fiber: session {id} could not be deleted: {message}\n"),
        );
    }
    let total = rows
        .iter()
        .filter(|row| {
            matches!(
                row,
                sessions::SessionRow::Deletable { .. } | sessions::SessionRow::Continues { .. }
            )
        })
        .count();
    let (_, code, _) =
        failures
            .first()
            .cloned()
            .unwrap_or((String::new(), ErrorCode::IoFailed, String::new()));
    Err(failed(
        code,
        format!("{} of {} could not be deleted", failures.len(), total),
    ))
}

/// The `SessionId` of a delete, for tests.
#[allow(dead_code, reason = "only tests read the delete's session")]
fn delete_id(id: &SessionId) -> &str {
    &id.0
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod tests;
