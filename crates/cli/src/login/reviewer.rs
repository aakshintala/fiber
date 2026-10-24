//! Picking the reviewer model at login (`docs/model-routing.md`,
//! "Logging in"): the chosen provider's model list with the saved choice
//! preselected, and the one-key save. Shared by `fiber login` and the
//! terminal app's `/login`: neither runs a second credential transaction,
//! and neither starts any other extension.

use std::io::{self, BufRead, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use config::{Config, Sources, get_global, replace_global};
use contract::clock::Clock;
use extensions::{Browser, Providers, login_provider};

use crate::CliError;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use serde_json::Value;

use super::installed;

/// The reviewer choice for one provider: its models as local IDs, in the
/// provider's order, never padded with a stale saved ID, with the saved or
/// package choice preselected. Empty `models` is an explicit skip, even
/// when `selected` names a saved pick. Holds only local IDs and public
/// notices, never a secret or a live VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerModels {
    /// The provider's model IDs, in its order.
    pub models: Vec<String>,
    /// The saved pick, else the package default, kept even when the list
    /// does not name it.
    pub selected: Option<String>,
    /// One line per discovery problem, already redacted.
    pub notices: Vec<String>,
}

/// `providers."<name>".reviewer_model`, with a name holding a dot quoted:
/// the one key login writes, each provider keeping its own pick.
pub(crate) fn reviewer_key(name: &str) -> String {
    if name.contains('.') {
        format!("providers.\"{name}\".reviewer_model")
    } else {
        format!("providers.{name}.reviewer_model")
    }
}

/// The key's segments, refusing before any read or write when the existing
/// display/parse syntax cannot represent `name`: a name it cannot
/// round-trip, such as one holding both a dot and a quote, never becomes a
/// dotted path. The credential and every config byte stay as they were.
fn reviewer_segments(name: &str) -> Result<Vec<String>, CliError> {
    let key = reviewer_key(name);
    let expected = vec![
        "providers".to_owned(),
        name.to_owned(),
        "reviewer_model".to_owned(),
    ];
    match config::path::parse(&key) {
        Some(segments) if segments == expected => Ok(segments),
        _ => Err(CliError::ReviewerNameUnrepresentable { key: key.clone() }),
    }
}

/// Discovery never opens a browser: the handle only satisfies the VM
/// builder, and no discovery callback may reach it.
struct NoBrowser;

impl Browser for NoBrowser {
    fn open(&self, _url: &str) {}

    fn show(&self, _url: &str, _code: &str) {}

    fn attended(&self) -> bool {
        false
    }
}

/// Runs a credential command to completion, killing it at `timeout`: the
/// runner discovery's credential check takes. Time passes on the login's
/// injected clock, never the process clock. A stored credential answers
/// that check first, so this runs only for a configured command source.
fn run_command(
    command: &mut Command,
    timeout: Duration,
    clock: &Arc<dyn Clock>,
) -> io::Result<std::process::Output> {
    let mut child = command.spawn()?;
    let start = clock.now();
    loop {
        match child.try_wait()? {
            Some(_) => return child.wait_with_output(),
            None if clock.now().saturating_duration_since(start) >= timeout => {
                if child.kill().is_err() {
                    // Already gone: the wait below still reaps it.
                }
                drop(child.wait());
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the credential command timed out",
                ));
            }
            None => clock.sleep(Duration::from_millis(10)),
        }
    }
}

/// The discovery diagnostic with its secret-bearing text replaced: the
/// typed code stays, so a caller can still tell a failed callback from a
/// rejected model, but no callback text, path or value reaches a notice,
/// an error or a log. Only the requested provider and its admitted
/// extension are named.
fn redacted(code: contract::ErrorCode, provider: &str, owner: &str) -> String {
    if code == contract::ErrorCode::ModelInvalid {
        format!(
            "`{provider}` left out a model from `{owner}` that failed validation; \
             keeping the listed ones."
        )
    } else {
        format!("`{provider}`'s model list from `{owner}` is unavailable; keeping the stored list.")
    }
}

/// Reads the reviewer choice for the installed provider `name`: its models
/// through its own admitted extension only, preselecting the saved global
/// value, else the package default. The caller has stored the credential:
/// discovery runs only for a provider with one, serving its data or cache
/// otherwise, and every notice crossing the discovery seam is redacted. A
/// malformed or unreadable configuration is an error, never permission to
/// guess. Writes nothing except the provider's own model cache, which a
/// successful uncached discovery updates as it returns.
pub fn reviewer_models(
    home: &Path,
    providers: &Providers,
    name: &str,
    clock: Arc<dyn Clock>,
) -> Result<ReviewerModels, CliError> {
    let data = installed(providers, name)?.clone();
    let key = reviewer_key(name);
    let selected = match get_global(home, &key)? {
        Some(value) => value.as_str().map(str::to_owned),
        None => None,
    }
    .or_else(|| data.reviewer_model.clone());
    let fallback: Vec<String> = data.models.iter().map(|model| model.id.clone()).collect();
    let Some(owner) = providers.extension_of(name).map(str::to_owned) else {
        // No recorded owner: no hook to run, so the data file's list
        // stands, with no notice.
        return Ok(ReviewerModels {
            models: fallback,
            selected,
            notices: Vec::new(),
        });
    };
    // The home layers are discovery's configuration: the stored
    // credential it checks was just written to the global file, so the
    // check does not depend on the launch directory.
    let (_, project) = crate::project_of(home, home)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: home.to_path_buf(),
        project,
        overrides: Vec::new(),
    })?;
    let lua = match login_provider(
        home,
        providers,
        name,
        Arc::new(NoBrowser),
        Arc::clone(&clock),
    ) {
        Ok(lua) => lua,
        Err(e) => {
            return Ok(ReviewerModels {
                models: fallback,
                selected,
                notices: vec![redacted(e.code(), name, &owner)],
            });
        }
    };
    // A temporary registry takes the admission: the installed providers
    // stay as loaded, and the VM drops with this return.
    let mut temp = providers.clone();
    let run = |command: &mut Command, timeout: Duration| run_command(command, timeout, &clock);
    let mut notices: Vec<String> = temp
        .add_lua(&owner, &lua, &config, &run, &|name| {
            std::env::var(name).ok()
        })
        .into_iter()
        .map(|notice| redacted(notice.code, name, &owner))
        .collect();
    let models: Vec<String> = temp
        .get(name)
        .map(|data| data.models.iter().map(|model| model.id.clone()).collect())
        .unwrap_or(fallback);
    if models.is_empty() {
        notices.push(format!(
            "`{name}` lists no models; the credential stays, with no reviewer set."
        ));
    }
    Ok(ReviewerModels {
        models,
        selected,
        notices,
    })
}

/// Saves `id` as provider `name`'s reviewer, exactly as listed: no
/// discovery rerun, no second list check, so a stale Enter still lands.
/// Names the key on failure; the stored credential stays either way.
pub fn set_reviewer(home: &Path, name: &str, id: &str) -> Result<(), CliError> {
    let key = reviewer_key(name);
    reviewer_segments(name)?;
    replace_global(home, &key, Some(Value::String(id.to_owned())))
        .map(|_| ())
        .map_err(|source| CliError::ReviewerNotSaved {
            key: key.clone(),
            source,
        })
}

/// How long one terminal prompt wait blocks before rechecking the
/// cancellation latch: a SIGINT during blocked input lands at most this
/// late, without a helper thread left blocked on stdin.
const PROMPT_POLL: Duration = Duration::from_millis(50);

/// Standard input nonblocking until it drops, so the reviewer prompt can
/// wait for a line and still answer cancellation: each read either takes
/// a buffered or freshly typed line at once, reports EOF, or says it
/// would block, and the prompt polls the terminal between tries. The
/// saved flags come back on every way out.
struct NonblockingStdin {
    fd: OwnedFd,
    saved: OFlags,
}

impl NonblockingStdin {
    /// `fd` nonblocking until the guard drops: the terminal the prompt
    /// reads, or any fd a test drives, such as a PTY slave.
    fn open(fd: BorrowedFd<'_>) -> io::Result<Self> {
        let fd = fd.try_clone_to_owned()?;
        let saved = fcntl_getfl(&fd).map_err(io::Error::from)?;
        fcntl_setfl(&fd, saved.union(OFlags::NONBLOCK)).map_err(io::Error::from)?;
        Ok(Self { fd, saved })
    }
}

impl Drop for NonblockingStdin {
    fn drop(&mut self) {
        fcntl_setfl(&self.fd, self.saved).unwrap_or(());
    }
}

/// One poll slice inside [`wait_ready`]: short enough that the latch
/// recheck stays prompt on the process clock, long enough that a fake
/// clock reaches the budget in a few advances.
const POLL_SLICE: Duration = Duration::from_millis(5);

/// Waits for terminal input until [`PROMPT_POLL`] passes on `clock`: the
/// prompt's recheck point for the cancellation latch. Polls `fd` in short
/// slices without blocking, sleeping each slice on the login's injected
/// clock, so a test drives the wait with its fake clock instead of
/// reading the process one. Any event ends the wait early: input, a
/// hangup or an error all return to the latch check, and only a wait
/// failure of its own surfaces.
fn wait_ready(fd: &impl AsFd, clock: &Arc<dyn Clock>) -> io::Result<()> {
    let start = clock.now();
    loop {
        let mut waiting = [PollFd::new(fd, PollFlags::IN)];
        match poll(
            &mut waiting,
            Some(&Timespec::try_from(Duration::ZERO).unwrap_or_default()),
        ) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(io::Error::from(e)),
        }
        let ready = waiting
            .first()
            .is_some_and(|polled| !polled.revents().is_empty());
        if ready || clock.now().saturating_duration_since(start) >= PROMPT_POLL {
            return Ok(());
        }
        clock.sleep(POLL_SLICE);
    }
}

/// One cancellable wait slice for a blocked terminal write: parks `clock`
/// until `until`, waiting on `term` for real while parked, so a test
/// drives the wait with its fake clock instead of reading the process one.
/// An `OUT` event ends the slice early; only a wait failure of its own
/// surfaces.
fn wait_writable(term: BorrowedFd<'_>, until: Instant, clock: &Arc<dyn Clock>) -> io::Result<()> {
    let mut outcome = Ok(());
    clock.wait_until(Some(until), &mut |bound| {
        let wait = bound.or(Some(POLL_SLICE));
        let spec = wait.map(|wait| Timespec {
            tv_sec: wait.as_secs().min(i64::MAX as u64).cast_signed(),
            tv_nsec: wait.subsec_nanos() as _,
        });
        let mut waiting = [PollFd::new(&term, PollFlags::OUT)];
        match poll(&mut waiting, spec.as_ref()) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => outcome = Err(io::Error::from(e)),
        }
    });
    outcome
}

/// Writes `buf` to `err` and flushes, waiting through `WouldBlock` in
/// [`POLL_SLICE`] slices until the terminal takes every byte: partial
/// writes continue in byte order, and the latch is rechecked after every
/// slice, so cancellation lands within [`PROMPT_POLL`] even mid-write.
/// Returns true when cancellation interrupts the write, leaving partial
/// text behind; any other write error surfaces at once, exactly as the
/// blocking writes did.
fn write_all_wait(
    err: &mut dyn Write,
    term: BorrowedFd<'_>,
    buf: &[u8],
    stopped: &dyn Fn() -> bool,
    clock: &Arc<dyn Clock>,
) -> io::Result<bool> {
    let mut written = 0;
    while written < buf.len() {
        if stopped() {
            return Ok(true);
        }
        match err.write(buf.get(written..).unwrap_or_default()) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write the whole prompt",
                ));
            }
            Ok(n) => written += n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                wait_writable(term, clock.now() + POLL_SLICE, clock)?;
            }
            Err(e) => return Err(e),
        }
    }
    loop {
        if stopped() {
            return Ok(true);
        }
        match err.flush() {
            Ok(()) => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                wait_writable(term, clock.now() + POLL_SLICE, clock)?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// What the terminal prompt answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    /// A listed ID, or the kept preselection on Enter.
    Chosen(String),
    /// EOF, cancellation, or an empty list: save nothing.
    Cancelled,
}

/// Asks for one of `choices` on the terminal: the numbered list with the
/// saved row marked, then one line per answer. An exact name wins over a
/// number, so a numeric model ID still chooses itself; a number out of
/// range, zero, an overflow, an unknown name and (with nothing preselected)
/// a blank line each print one line naming the problem and ask again,
/// until a listed choice, cancellation or EOF. `stopped` is the login's
/// cancellation scope, checked before every read. When `terminal` holds,
/// `input` is the process stdin, and the prompt reads it stoppably:
/// `signal-hook` installs `SA_RESTART`, so a plain blocking read would
/// keep waiting through a SIGINT without ever checking the latch. The
/// prompt instead reads stdin nonblocking and polls the terminal between
/// tries, on this thread alone: no helper stays blocked on stdin, and the
/// flags (like the echo, which stays on) are restored on every return.
/// Never fabricates or saves a model: choosing is the caller's, after the
/// store. When `terminal` holds, `term` is the descriptor made nonblocking
/// and polled for reads and writes; otherwise it is ignored.
#[allow(clippy::too_many_arguments, reason = "one per input the prompt reads")]
pub(crate) fn prompt_choice(
    name: &str,
    choices: &ReviewerModels,
    input: &mut dyn BufRead,
    err: &mut dyn Write,
    stopped: &dyn Fn() -> bool,
    terminal: bool,
    clock: &Arc<dyn Clock>,
    term: BorrowedFd<'_>,
) -> io::Result<Pick> {
    if choices.models.is_empty() {
        return Ok(Pick::Cancelled);
    }
    writeln!(err, "Reviewer for {name}:")?;
    for (index, model) in choices.models.iter().enumerate() {
        let saved = choices
            .selected
            .as_deref()
            .is_some_and(|keep| keep == model);
        writeln!(
            err,
            "  {}) {model}{}",
            index + 1,
            if saved { " (saved)" } else { "" }
        )?;
    }
    if let Some(keep) = choices.selected.as_deref()
        && !choices.models.iter().any(|model| model == keep)
    {
        writeln!(
            err,
            "Saved reviewer `{keep}` is not listed; Enter keeps it."
        )?;
    }
    // Nonblocking for the stoppable terminal read below; restored when
    // the prompt returns, however it returns. A pipe keeps blocking.
    let _nonblocking = terminal.then(|| NonblockingStdin::open(term)).transpose()?;
    loop {
        if stopped() {
            return Ok(Pick::Cancelled);
        }
        let cancelled = if terminal {
            write_all_wait(err, term, b"Reviewer, by number or name: ", stopped, clock)?
        } else {
            write!(err, "Reviewer, by number or name: ")?;
            err.flush()?;
            false
        };
        if cancelled {
            return Ok(Pick::Cancelled);
        }
        let mut line = String::new();
        if terminal {
            // Stoppable: a buffered or freshly typed line lands at once
            // and EOF ends the read, while a would-block waits on the
            // terminal until the latch or more input. A partial line
            // without its newline reads on: EOF follows it. The latch
            // is checked on every pass, including the waits: a `continue`
            // below must never skip it.
            loop {
                if stopped() {
                    return Ok(Pick::Cancelled);
                }
                let at_end = match input.read_line(&mut line) {
                    Ok(0) => true,
                    Ok(_) => line.ends_with('\n'),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        wait_ready(&term, clock)?;
                        continue;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                };
                if stopped() {
                    return Ok(Pick::Cancelled);
                }
                if at_end {
                    break;
                }
            }
            if line.is_empty() {
                return Ok(Pick::Cancelled);
            }
        } else if input.read_line(&mut line)? == 0 {
            return Ok(Pick::Cancelled);
        }
        if stopped() {
            return Ok(Pick::Cancelled);
        }
        let answer = line.trim();
        if answer.is_empty() {
            if let Some(keep) = choices.selected.as_deref() {
                return Ok(Pick::Chosen(keep.to_owned()));
            }
            let cancelled = if terminal {
                write_all_wait(
                    err,
                    term,
                    b"No reviewer is selected; choose a number or name.\n",
                    stopped,
                    clock,
                )?
            } else {
                writeln!(err, "No reviewer is selected; choose a number or name.")?;
                false
            };
            if cancelled {
                return Ok(Pick::Cancelled);
            }
            continue;
        }
        if let Some(pick) = parse_pick(answer, choices) {
            return Ok(Pick::Chosen(pick));
        }
        let retry = format!("`{answer}` is not a listed reviewer; choose a number or name.\n");
        let cancelled = if terminal {
            write_all_wait(err, term, retry.as_bytes(), stopped, clock)?
        } else {
            write!(err, "{retry}")?;
            false
        };
        if cancelled {
            return Ok(Pick::Cancelled);
        }
    }
}

/// A listed answer: an exact name first, then a 1-based number. Anything
/// else is `None`: invalid input never becomes a model.
fn parse_pick(answer: &str, choices: &ReviewerModels) -> Option<String> {
    if choices.models.iter().any(|model| model == answer) {
        return Some(answer.to_owned());
    }
    let number: usize = answer.parse().ok()?;
    choices.models.get(number.checked_sub(1)?).cloned()
}

#[cfg(test)]
#[path = "reviewer_tests.rs"]
mod tests;
