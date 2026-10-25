//! `fiber sessions prune` (`docs/invocation.md`, "Deleting and pruning"):
//! deletes old exited sessions through the hub, removes kept worktrees
//! that hold nothing to lose, removes pinned copies no worktree matches,
//! and deletes old diagnostic logs and crash files, with `--dry-run`,
//! `--yes` and the space freed.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufReader, IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use contract::ErrorCode;
use contract::SessionId;
use contract::clock::Clock;
use contract::shapes::Failure;
use log::diag::{OldFile, old_diagnostics};

mod ignored;
mod pinned;
mod sessions;
mod worktrees;

use crate::approve::{confirmed, say};
use crate::sessions::Ask;
use crate::{CliError, with_dirs, write_line};

/// What the prompt asks.
const PRUNE_PROMPT: &str = "prune? [y/N]";

/// The usage failure when there is nobody to ask.
const PRUNE_NOBODY: &str =
    "`fiber sessions prune` has no terminal to ask on. Pass `--yes` to prune without asking.";

/// The projects a prune selection covers: one repository's project, or
/// every project on the machine.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Scope<'a> {
    /// The repository's project at this workspace, as today.
    Project(&'a Path),
    /// Every project under `home`.
    HubWide,
}

/// The session-delete callback the hub path invokes once per selected
/// session, and the local path once per delete with no `expect`. The
/// lifetime is the callback's borrows, which end when the seam call
/// returns; a bare alias would demand `'static` and refuse either caller.
pub type HubDelete<'a> =
    dyn FnMut(&SessionId, bool, Option<&BTreeSet<SessionId>>) -> Result<(), Failure> + Send + 'a;

/// What a hub-wide selection answers: the printable lines, the per-session
/// sets, the file keys and the bytes the preview would free. File keys are
/// the dry-run line strings: worktree directory paths, pinned hashes and
/// diagnostic paths.
pub struct HubPruneSelection {
    /// The preview lines, in print order.
    pub lines: Vec<String>,
    /// The sessions to delete, in send order.
    pub sessions: Vec<HubPruneSession>,
    /// The worktree directory paths to remove.
    pub worktrees: Vec<String>,
    /// The pinned-copy hashes to remove.
    pub pinned: Vec<String>,
    /// The diagnostic paths to remove.
    pub diagnostics: Vec<String>,
    /// The bytes the preview would free.
    pub would_free: u64,
}

/// One session a hub-wide selection deletes.
pub struct HubPruneSession {
    /// The session to delete.
    pub id: SessionId,
    /// Whether the preview deletes with `cascade`.
    pub cascade: bool,
    /// The listed bytes the delete would free.
    pub bytes: u64,
    /// The confirmed set the delete removes; empty is an unconstrained
    /// preview, deleted without `expect`.
    pub expect: BTreeSet<SessionId>,
}

/// The previewed items a confirmed hub prune removes: each named as the
/// dry run names it.
pub struct HubPruneWant {
    /// The sessions to delete, each with its previewed cascade set; an
    /// empty set is an unconstrained preview.
    pub sessions: BTreeMap<SessionId, BTreeSet<SessionId>>,
    /// The worktree directory paths to remove.
    pub worktrees: BTreeSet<String>,
    /// The pinned-copy hashes to remove.
    pub pinned: BTreeSet<String>,
    /// The diagnostic paths to remove.
    pub diagnostics: BTreeSet<String>,
}

/// What a confirmed hub prune removed: the space freed, each item that
/// failed or was skipped, and how many items it tried to remove.
pub struct HubPruneRemoved {
    /// The bytes removed.
    pub freed: u64,
    /// Each item that failed or was skipped: what stayed, its code and why.
    pub failures: Vec<(String, ErrorCode, String)>,
    /// How many items the confirm tried to remove.
    pub total: usize,
}

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
    /// Removes a worktree even when removing it would lose something.
    /// A worktree a running session works in is never removed.
    pub force: bool,
}

/// `fiber sessions prune` in the current directory.
pub fn prune(
    args: &PruneArgs,
    clock: &dyn Clock,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    with_dirs(|home, workspace| {
        let ask = Ask {
            yes: args.yes,
            terminal,
            input: &mut stdin.lock(),
            err: &mut io::stderr(),
        };
        prune_run(
            home,
            workspace,
            args,
            clock,
            ask,
            &mut io::stdout(),
            connect,
        )
    })
}

/// Runs prune for `home` and `workspace` on `clock`: lists, asks unless
/// `yes` or `dry_run`, deletes diagnostics, then worktrees, then pinned
/// copies, then sessions, and prints the space freed. `--dry-run` never
/// connects and never asks.
fn prune_run(
    home: &Path,
    workspace: &Path,
    args: &PruneArgs,
    clock: &dyn Clock,
    ask: Ask<'_>,
    out: &mut dyn Write,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> Result<(), CliError> {
    let now = clock.wall();
    let older_than = match &args.older_than {
        None => None,
        Some(text) => match config::parse_duration(text) {
            Some(duration) => Some(duration),
            None => return Err(CliError::OlderThanUnreadable),
        },
    };
    // No terminal and no `--yes` is a usage error before anything is
    // scanned. `--dry-run` never asks and needs no terminal.
    if !args.dry_run && !ask.yes && !ask.terminal {
        return Err(CliError::NobodyToAsk {
            message: PRUNE_NOBODY.to_owned(),
        });
    }
    let logs = old_diagnostics(&home.join("logs"), now);
    let crashes = old_diagnostics(&home.join("crashes"), now);
    let started = log::started_sessions(home);
    let scope = Scope::Project(workspace);
    let selected = sessions::select_scoped(home, &started, scope, older_than, args.cascade, now);
    let mut planned =
        worktrees::select_scoped(home, &started, scope, args.force, args.dry_run, now);
    let pinned_plan = pinned::select_scoped(home, &started, scope, clock);
    let has_sessions = !selected.deletes.is_empty();
    let has_worktrees = !planned.removals.is_empty();
    let has_pinned = pinned_plan
        .rows
        .iter()
        .any(|row| matches!(row, pinned::PinnedRow::Removable { .. }));
    let has_diagnostics = !logs.is_empty() || !crashes.is_empty();
    if !has_sessions && !has_worktrees && !has_pinned && !has_diagnostics {
        print_rows(
            out,
            &selected.rows,
            &planned.rows,
            &pinned_plan.rows,
            &logs,
            &crashes,
            now,
        )?;
        write_line(out, &format!("freed {}", format_size(0))).map_err(CliError::from)?;
        return Ok(());
    }
    let would_free = session_listed_bytes(&selected.rows)
        + worktrees::listed_bytes(&planned)
        + pinned::listed_bytes(&pinned_plan)
        + diag_bytes(&logs)
        + diag_bytes(&crashes);
    if args.dry_run {
        print_rows(
            out,
            &selected.rows,
            &planned.rows,
            &pinned_plan.rows,
            &logs,
            &crashes,
            now,
        )?;
        write_line(out, &format!("would free {}", format_size(would_free)))
            .map_err(CliError::from)?;
        return Ok(());
    }
    print_rows(
        out,
        &selected.rows,
        &planned.rows,
        &pinned_plan.rows,
        &logs,
        &crashes,
        now,
    )?;
    if !ask.yes && !confirmed(PRUNE_PROMPT, PRUNE_NOBODY, ask.terminal, ask.input, ask.err)? {
        say(ask.err, "nothing deleted\n");
        return Ok(());
    }
    let removed_logs = remove_diagnostics(&logs);
    let removed_crashes = remove_diagnostics(&crashes);
    let mut failures: Vec<(String, ErrorCode, String)> = removed_logs
        .failures
        .into_iter()
        .chain(removed_crashes.failures)
        .map(|(path, message)| (path, ErrorCode::IoFailed, message))
        .collect();
    let diag_total = logs.len() + crashes.len();
    let removed_worktrees = worktrees::remove_planned(home, &mut planned, args.force, &mut |_| {});
    let removed_pinned = pinned::remove_planned(home, &pinned_plan, clock, &mut |_| {});
    let freed =
        removed_logs.freed + removed_crashes.freed + removed_worktrees.freed + removed_pinned.freed;
    failures.extend(removed_worktrees.failures);
    failures.extend(removed_pinned.failures);
    let other_total = removed_worktrees.total + removed_pinned.total;
    drop(planned);
    if !has_sessions {
        return finish(
            out,
            ask.err,
            &selected.rows,
            freed,
            failures,
            diag_total,
            other_total,
        );
    }
    let stream = match connect() {
        Ok((stream, _)) => stream,
        Err(e) => {
            // No `delete` was accepted: every remaining directory fails
            // with the connection error, as `reconcile` reports.
            let failure = crate::sessions::hub_failure(e);
            let results: Vec<Result<(), Failure>> = selected
                .deletes
                .iter()
                .map(|_| Err(failure.clone()))
                .collect();
            return reconcile(
                out,
                &selected,
                &results,
                freed,
                failures,
                diag_total,
                other_total,
                ask.err,
            );
        }
    };
    let mut read = BufReader::new(stream);
    // The local path sends deletes with no `expect`.
    let none: BTreeMap<SessionId, BTreeSet<SessionId>> = BTreeMap::new();
    let mut sent = 0_usize;
    let mut send = |id: &SessionId, cascade: bool, _expect: Option<&BTreeSet<SessionId>>| {
        sent += 1;
        let command_id = format!("c_prune_{sent}");
        crate::sessions::send_delete(&mut read, &command_id, id, cascade)
    };
    let results = delete_sessions(&selected.deletes, &none, &mut send);
    reconcile(
        out,
        &selected,
        &results,
        freed,
        failures,
        diag_total,
        other_total,
        ask.err,
    )
}

/// Every row prune prints, in print order: sessions by id, then worktrees
/// by name, then pinned copies by hash, then `logs/` then `crashes/` by
/// file name, fields separated by two spaces. The local print and the
/// hub-wide preview share it, so both list the same lines.
fn listing_lines(
    sessions: &[sessions::SessionRow],
    worktrees: &[worktrees::WorktreeRow],
    pinned: &[pinned::PinnedRow],
    logs: &[OldFile],
    crashes: &[OldFile],
    now: SystemTime,
) -> Vec<String> {
    let mut lines = Vec::new();
    for row in sessions {
        lines.push(session_line(row));
    }
    for row in worktrees {
        lines.push(worktrees::worktree_line(row));
    }
    for row in pinned {
        lines.push(pinned::pinned_line(row));
    }
    for file in logs {
        lines.push(diag_line("log", &file.path, file.bytes, now));
    }
    for file in crashes {
        lines.push(diag_line("crash", &file.path, file.bytes, now));
    }
    lines
}

/// Prints every row.
fn print_rows(
    out: &mut dyn Write,
    sessions: &[sessions::SessionRow],
    worktrees: &[worktrees::WorktreeRow],
    pinned: &[pinned::PinnedRow],
    logs: &[OldFile],
    crashes: &[OldFile],
    now: SystemTime,
) -> Result<(), CliError> {
    for line in listing_lines(sessions, worktrees, pinned, logs, crashes, now) {
        write_line(out, &line).map_err(CliError::from)?;
    }
    Ok(())
}

/// Deletes each session through `delete` in send order, one answer per
/// delete. `expects` carries the previewed cascade set per session; the
/// local path passes none, as today, and an empty set stays unconstrained.
fn delete_sessions(
    deletes: &[sessions::Delete],
    expects: &BTreeMap<SessionId, BTreeSet<SessionId>>,
    delete: &mut HubDelete<'_>,
) -> Vec<Result<(), Failure>> {
    deletes
        .iter()
        .map(|one| {
            let expect = expects.get(&one.id).filter(|set| !set.is_empty());
            delete(&one.id, one.cascade, expect)
        })
        .collect()
}

/// Selects hub-wide prune work across every project under `home`: the
/// printable lines, the per-session sets, the file keys and the bytes the
/// preview would free. Without `older_than` no session is selected.
pub fn select_for_hub(
    home: &Path,
    older_than: Option<Duration>,
    cascade: bool,
    force: bool,
    now: SystemTime,
    clock: &dyn Clock,
) -> HubPruneSelection {
    let started = log::started_sessions(home);
    let selected =
        sessions::select_scoped(home, &started, Scope::HubWide, older_than, cascade, now);
    // The scan holds no locks: the dry run drops each taken lock at once.
    let planned = worktrees::select_scoped(home, &started, Scope::HubWide, force, true, now);
    let pinned_plan = pinned::select_scoped(home, &started, Scope::HubWide, clock);
    let logs = old_diagnostics(&config::logs_dir(home), now);
    let crashes = old_diagnostics(&config::crashes_dir(home), now);
    let lines = listing_lines(
        &selected.rows,
        &planned.rows,
        &pinned_plan.rows,
        &logs,
        &crashes,
        now,
    );
    let sessions = selected
        .deletes
        .iter()
        .enumerate()
        .map(|(index, delete)| {
            let mut expect = BTreeSet::from([delete.id.clone()]);
            let mut bytes = 0_u64;
            for row in &selected.rows {
                let (id, row_bytes, at) = match row {
                    sessions::SessionRow::Deletable {
                        id,
                        bytes,
                        delete: at,
                        ..
                    }
                    | sessions::SessionRow::Continues {
                        id,
                        bytes,
                        delete: at,
                        ..
                    } => (id, bytes, at),
                    sessions::SessionRow::Blocked { .. }
                    | sessions::SessionRow::Cycle { .. }
                    | sessions::SessionRow::Unreadable { .. } => continue,
                };
                if *at == index {
                    expect.insert(SessionId(id.clone()));
                    bytes += *row_bytes;
                }
            }
            // A plain delete carries no set: unconstrained, as the local
            // path sends it.
            if !delete.cascade {
                expect.clear();
            }
            HubPruneSession {
                id: delete.id.clone(),
                cascade: delete.cascade,
                bytes,
                expect,
            }
        })
        .collect();
    let would_free = session_listed_bytes(&selected.rows)
        + worktrees::listed_bytes(&planned)
        + pinned::listed_bytes(&pinned_plan)
        + diag_bytes(&logs)
        + diag_bytes(&crashes);
    HubPruneSelection {
        lines,
        sessions,
        worktrees: planned
            .removals
            .iter()
            .map(|removal| removal.dir.display().to_string())
            .collect(),
        pinned: pinned_plan
            .removals
            .iter()
            .map(|removal| removal.hash.clone())
            .collect(),
        diagnostics: logs
            .iter()
            .chain(crashes.iter())
            .map(|file| file.path.display().to_string())
            .collect(),
        would_free,
    }
}

/// Removes the previewed `want` across every project under `home`:
/// diagnostics, then worktrees, then pinned copies, then sessions through
/// `delete`. Each side reselects first and acts only on items in both the
/// reselection and `want`: a newly selectable item is left alone, and a
/// previewed item that no longer qualifies is skipped and reported.
/// Sessions delete with their previewed cascade set as `expect`, so a set
/// that changed mid-prompt fails that one session without failing the
/// prune; an empty set deletes unconstrained.
pub fn remove_for_hub(
    home: &Path,
    want: &HubPruneWant,
    older_than: Option<Duration>,
    cascade: bool,
    force: bool,
    clock: &dyn Clock,
    delete: &mut HubDelete<'_>,
) -> HubPruneRemoved {
    let now = clock.wall();
    let started_sessions = log::started_sessions(home);
    let total =
        want.sessions.len() + want.worktrees.len() + want.pinned.len() + want.diagnostics.len();
    let mut freed = 0_u64;
    let mut failures = Vec::new();
    // Diagnostics first, as the local path removes them first.
    let (logs, crashes) = (
        old_diagnostics(&config::logs_dir(home), now),
        old_diagnostics(&config::crashes_dir(home), now),
    );
    let present: BTreeSet<String> = logs
        .iter()
        .chain(crashes.iter())
        .map(|file| file.path.display().to_string())
        .collect();
    for key in &want.diagnostics {
        if !present.contains(key) {
            failures.push((
                key.clone(),
                ErrorCode::StaleRequest,
                format!("{key} is no longer selected; nothing was removed"),
            ));
        }
    }
    let kept: Vec<OldFile> = logs
        .into_iter()
        .chain(crashes)
        .filter(|file| want.diagnostics.contains(&file.path.display().to_string()))
        .collect();
    let removed = remove_diagnostics(&kept);
    freed += removed.freed;
    failures.extend(
        removed
            .failures
            .into_iter()
            .map(|(path, message)| (path, ErrorCode::IoFailed, message)),
    );
    // Worktrees reselect with their locks held, as the local confirm does.
    let mut planned =
        worktrees::select_scoped(home, &started_sessions, Scope::HubWide, force, false, now);
    let selected: BTreeSet<String> = planned
        .removals
        .iter()
        .map(|removal| removal.dir.display().to_string())
        .collect();
    for key in &want.worktrees {
        if !selected.contains(key) {
            failures.push((
                format!("worktree {key}"),
                ErrorCode::StaleRequest,
                format!("worktree {key} is no longer selected; nothing was removed"),
            ));
        }
    }
    planned
        .removals
        .retain(|removal| want.worktrees.contains(&removal.dir.display().to_string()));
    let removed_worktrees = worktrees::remove_planned(home, &mut planned, force, &mut |_| {});
    freed += removed_worktrees.freed;
    failures.extend(removed_worktrees.failures);
    // Pinned copies reselect the same way.
    let mut pinned_plan = pinned::select_scoped(home, &started_sessions, Scope::HubWide, clock);
    let selected: BTreeSet<String> = pinned_plan
        .removals
        .iter()
        .map(|removal| removal.hash.clone())
        .collect();
    for key in &want.pinned {
        if !selected.contains(key) {
            failures.push((
                format!("pinned {key}"),
                ErrorCode::StaleRequest,
                format!("pinned {key} is no longer selected; nothing was removed"),
            ));
        }
    }
    pinned_plan
        .removals
        .retain(|removal| want.pinned.contains(&removal.hash));
    let removed_pinned = pinned::remove_planned(home, &pinned_plan, clock, &mut |_| {});
    freed += removed_pinned.freed;
    failures.extend(removed_pinned.failures);
    // Sessions reselect against `older_than` first, as the worktrees do
    // against their locks: a previewed session that no longer qualifies
    // is skipped and reported, and only the intersection reaches the
    // callback. Each delete carries its previewed cascade set as
    // `expect`, so a set that changed mid-prompt fails that one session
    // without failing the prune; an empty set deletes unconstrained.
    // Listed bytes are read before each delete, so a removed session
    // counts exactly the bytes its set held.
    let reselected = sessions::select_scoped(
        home,
        &started_sessions,
        Scope::HubWide,
        older_than,
        cascade,
        now,
    );
    let live: BTreeSet<&SessionId> = reselected.deletes.iter().map(|delete| &delete.id).collect();
    let mut deletes: Vec<sessions::Delete> = Vec::with_capacity(want.sessions.len());
    for (id, set) in &want.sessions {
        if live.contains(id) {
            deletes.push(sessions::Delete {
                id: id.clone(),
                cascade: !set.is_empty(),
            });
        } else {
            failures.push((
                id.0.clone(),
                ErrorCode::StaleRequest,
                format!("{} is no longer selected; nothing was removed", id.0),
            ));
        }
    }
    let listed: BTreeMap<&SessionId, u64> = deletes
        .iter()
        .map(|one| {
            // The set names its root too, so the root is counted once.
            let mut members: BTreeSet<&SessionId> = BTreeSet::new();
            if let Some(set) = want.sessions.get(&one.id) {
                members.extend(set.iter());
            }
            members.insert(&one.id);
            let bytes = members
                .into_iter()
                .filter_map(|member| log::find_logged_session(home, member))
                .map(|(_, dir)| log::session_bytes(&dir))
                .sum();
            (&one.id, bytes)
        })
        .collect();
    let results = delete_sessions(&deletes, &want.sessions, delete);
    for (one, result) in deletes.iter().zip(results) {
        match result {
            Ok(()) => {
                freed += listed.get(&one.id).copied().unwrap_or(0);
            }
            Err(failure) => {
                failures.push((one.id.0.clone(), failure.code, failure.message));
            }
        }
    }
    HubPruneRemoved {
        freed,
        failures,
        total,
    }
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
            let age = match age_days {
                Some(days) => format_age(*days),
                None => "unknown".to_owned(),
            };
            format!(
                "session  {id}  {age}  {}  continues {parent}",
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
/// of KiB, MiB or GiB that is at least 1. Shared with the hub client's
/// summary line, which prints the same sizes.
pub fn format_size(bytes: u64) -> String {
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

/// What removing diagnostics files freed and what it could not: a
/// `NotFound` is neither a failure nor counted, and every other error is
/// kept so the rest still runs.
struct DiagRemoved {
    freed: u64,
    /// Each file that stayed: its path and why.
    failures: Vec<(String, String)>,
}

/// Removes every diagnostics file, keeping every failure.
fn remove_diagnostics(files: &[OldFile]) -> DiagRemoved {
    let mut freed = 0_u64;
    let mut failures = Vec::new();
    for file in files {
        match std::fs::remove_file(&file.path) {
            Ok(()) => freed += file.bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => failures.push((file.path.display().to_string(), e.to_string())),
        }
    }
    DiagRemoved { freed, failures }
}

/// Reconciles each session row with `log::remaining` after every `delete`
/// has been answered: freed is listed minus remaining, and a row whose
/// `delete` was not accepted and whose directory remains is a failure
/// keeping the hub's code and message. `failures` already holds the
/// diagnostics files and worktrees that stayed. A failed hub connection
/// passes one error per delete, which reports every remaining directory
/// with the connection error.
#[allow(
    clippy::too_many_arguments,
    reason = "one hand-off of the reconcile: the rows, the hub answers and what already ran"
)]
fn reconcile(
    out: &mut dyn Write,
    selected: &sessions::Selected,
    results: &[Result<(), Failure>],
    diag_freed: u64,
    failures: Vec<(String, ErrorCode, String)>,
    diag_total: usize,
    other_total: usize,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let mut freed = diag_freed;
    let mut failures = failures;
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
                    failures.push((
                        format!("session {id}"),
                        failure.code.clone(),
                        failure.message.clone(),
                    ));
                }
            }
            Err(_) => {
                if let Some(failure) = hub_failure {
                    failures.push((
                        format!("session {id}"),
                        failure.code.clone(),
                        failure.message.clone(),
                    ));
                } else {
                    failures.push((
                        format!("session {id}"),
                        ErrorCode::IoFailed,
                        format!("{id} could not be read after pruning"),
                    ));
                }
            }
        }
    }
    finish(
        out,
        err,
        &selected.rows,
        freed,
        failures,
        diag_total,
        other_total,
    )
}

/// Prints `freed`, and when any deletion failed the per-row stderr lines
/// and the `<n>` of `<m>` failure with the first failure's code: session
/// rows first, then the diagnostics files that stayed.
fn finish(
    out: &mut dyn Write,
    err: &mut dyn Write,
    rows: &[sessions::SessionRow],
    freed: u64,
    failures: Vec<(String, ErrorCode, String)>,
    diag_total: usize,
    other_total: usize,
) -> Result<(), CliError> {
    write_line(out, &format!("freed {}", format_size(freed))).map_err(CliError::from)?;
    if failures.is_empty() {
        return Ok(());
    }
    for (what, _, message) in &failures {
        say(
            err,
            &format!("fiber: {what} could not be deleted: {message}\n"),
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
        .count()
        + diag_total
        + other_total;
    let (_, code, _) =
        failures
            .first()
            .cloned()
            .unwrap_or((String::new(), ErrorCode::IoFailed, String::new()));
    Err(CliError::PruneFailed {
        code,
        failed: failures.len(),
        total,
    })
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod tests;
