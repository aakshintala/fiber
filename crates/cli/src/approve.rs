//! `fiber approve` (`docs/extensions.md`, "Approving outside a session"):
//! shows what the repository declares and has no approval for, as one offer
//! would, and records an approval for each item once the person says so.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;

use contract::clock::Clock;
use extensions::{Fetch, Index, Origin, Store};

use crate::{CliError, project_of, with_dirs, write_line};

/// What the prompt asks.
const PROMPT: &str = "approve all? [y/N]";

/// The usage failure when nobody can answer.
const NOBODY: &str = "`fiber approve` has no terminal to ask on and no input. Pass `--yes` to approve without asking.";

/// Where `run` asks and reports: stdin, and stdout and stderr.
struct Streams<'a> {
    input: &'a mut dyn BufRead,
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
}

/// `fiber approve [--yes]` in the current directory. Without `--yes` it asks
/// once, and refuses when nobody can answer. Dependencies fetch from github
/// as the running Fiber version, on the clock that also bounds an
/// extension's install step when its copy is built.
pub fn approve(yes: bool, fiber_version: &str, clock: Arc<dyn Clock>) -> i32 {
    with_dirs(|home, workspace| {
        let stdin = io::stdin();
        let terminal = stdin.is_terminal();
        let fetch = Fetch::new(Origin::github(), fiber_version, Arc::clone(&clock));
        run(
            home,
            workspace,
            yes,
            terminal,
            Streams {
                input: &mut stdin.lock(),
                out: &mut io::stdout(),
                err: &mut io::stderr(),
            },
            clock,
            fetch,
        )
    })
}

/// Shows the pending items on `out`, asks on `err` unless `yes`, then
/// approves each in offer order. A failure on one leaves the earlier ones
/// recorded and the later ones not.
fn run(
    home: &Path,
    workspace: &Path,
    yes: bool,
    terminal: bool,
    streams: Streams<'_>,
    clock: Arc<dyn Clock>,
    fetch: Fetch,
) -> Result<(), CliError> {
    let Streams { input, out, err } = streams;
    let (_, project) = project_of(home, workspace)?;
    let store = Store::new(home, &project, Arc::clone(&clock)).fetching(fetch);
    let mut index = Index::load(home);
    let items = extensions::declared_items(workspace, clock.as_ref())?;
    let pending = extensions::pending(&store, &mut index, items)?;
    // The index only saves a re-hash; failing to write it costs nothing else.
    index.save().unwrap_or(());
    if pending.is_empty() {
        say(err, "nothing to approve\n");
        return Ok(());
    }
    for p in &pending {
        write_line(
            out,
            &format!(
                "{} {} ({})",
                extensions::kind_name(p.item.kind),
                p.item.name,
                p.item.path
            ),
        )
        .unwrap_or(());
        for line in p.offered.summary.lines() {
            write_line(out, &format!("  {line}")).unwrap_or(());
        }
        if let Some(diff) = &p.offered.diff {
            say(out, diff);
        }
    }
    if !yes && !confirmed(PROMPT, NOBODY, terminal, input, err)? {
        say(err, "nothing approved\n");
        return Ok(());
    }
    for p in &pending {
        store
            .approve(&p.item, &p.offered.hash)
            .map_err(|source| CliError::ApproveFailed {
                kind: extensions::kind_name(p.item.kind),
                name: p.item.name.clone(),
                source,
            })?;
        say(
            err,
            &format!(
                "approved {} {} {}\n",
                extensions::kind_name(p.item.kind),
                p.item.name,
                p.offered.hash.get(..12).unwrap_or(&p.offered.hash)
            ),
        );
    }
    Ok(())
}

/// Asks `prompt` once on `err`. With nothing to read and no terminal there
/// is nobody to ask, which is the usage failure `nobody`, naming `--yes`.
pub(crate) fn confirmed(
    prompt: &str,
    nobody: &str,
    terminal: bool,
    input: &mut dyn BufRead,
    err: &mut dyn Write,
) -> Result<bool, CliError> {
    say(err, &format!("{prompt} "));
    err.flush().unwrap_or(());
    let mut line = String::new();
    let read = input
        .read_line(&mut line)
        .map_err(CliError::StandardInput)?;
    // A person's Enter ends the prompt's line; a pipe's answer does not.
    if !terminal {
        say(err, "\n");
    }
    if read == 0 && !terminal {
        return Err(CliError::NobodyToAsk {
            message: nobody.to_owned(),
        });
    }
    Ok(says_yes(&line))
}

/// `y` or `yes`, in any case.
fn says_yes(line: &str) -> bool {
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// A closed stream leaves nobody to tell.
pub(crate) fn say(to: &mut dyn Write, text: &str) {
    write!(to, "{text}").unwrap_or(());
}

#[cfg(test)]
#[path = "approve_tests.rs"]
mod tests;
