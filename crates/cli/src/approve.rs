//! `fiber approve` (`docs/extensions.md`, "Approving outside a session"):
//! shows what the repository declares and has no approval for, as one offer
//! would, and records an approval for each item once the person says so.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use contract::ErrorCode;
use contract::shapes::Failure;
use extensions::{Index, Store};

use crate::{fail, failed, project_of, usage};

/// What the prompt asks.
const PROMPT: &str = "approve all? [y/N]";

/// `fiber approve [--yes]` in the current directory. Without `--yes` it asks
/// once, and refuses when nobody can answer.
pub fn approve(yes: bool) -> i32 {
    let prepared = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            Ok((home, workspace))
        });
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = prepared.and_then(|(home, workspace)| {
        run(
            &home,
            &workspace,
            yes,
            terminal,
            &mut stdin.lock(),
            &mut io::stdout(),
            &mut io::stderr(),
        )
    });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// Shows the pending items on `out`, asks on `err` unless `yes`, then
/// approves each in offer order. A failure on one leaves the earlier ones
/// recorded and the later ones not.
fn run(
    home: &Path,
    workspace: &Path,
    yes: bool,
    terminal: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let (_, project) = project_of(home, workspace)?;
    let store = Store::new(home, &project);
    let mut index = Index::load(home);
    let items = extensions::declared_items(workspace).map_err(|e| failed(e.code(), e))?;
    let pending =
        extensions::pending(&store, &mut index, items).map_err(|e| failed(e.code(), e))?;
    // The index only saves a re-hash; failing to write it costs nothing else.
    index.save().unwrap_or(());
    if pending.is_empty() {
        say(err, "nothing to approve\n");
        return Ok(());
    }
    for p in &pending {
        say(
            out,
            &format!(
                "{} {} ({})\n",
                extensions::kind_name(p.item.kind),
                p.item.name,
                p.item.path
            ),
        );
        for line in p.offered.summary.lines() {
            say(out, &format!("  {line}\n"));
        }
        if let Some(diff) = &p.offered.diff {
            say(out, diff);
        }
    }
    if !yes && !confirmed(terminal, input, err)? {
        say(err, "nothing approved\n");
        return Ok(());
    }
    for p in &pending {
        store.approve(&p.item, &p.offered.hash).map_err(|e| {
            failed(
                e.code(),
                format!(
                    "could not approve {} {}: {e}",
                    extensions::kind_name(p.item.kind),
                    p.item.name
                ),
            )
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

/// Asks once. With nothing to read and no terminal there is nobody to ask,
/// which is a usage failure that names `--yes`.
fn confirmed(
    terminal: bool,
    input: &mut dyn BufRead,
    err: &mut dyn Write,
) -> Result<bool, Failure> {
    say(err, &format!("{PROMPT} "));
    err.flush().unwrap_or(());
    let mut line = String::new();
    let read = input
        .read_line(&mut line)
        .map_err(|e| failed(ErrorCode::IoFailed, format!("standard input: {e}")))?;
    // A person's Enter ends the prompt's line; a pipe's answer does not.
    if !terminal {
        say(err, "\n");
    }
    if read == 0 && !terminal {
        return Err(usage(
            "`fiber approve` has no terminal to ask on and no input. Pass `--yes` to approve without asking.",
        ));
    }
    Ok(says_yes(&line))
}

/// `y` or `yes`, in any case.
fn says_yes(line: &str) -> bool {
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// A closed stream leaves nobody to tell.
fn say(to: &mut dyn Write, text: &str) {
    write!(to, "{text}").unwrap_or(());
}

#[cfg(test)]
#[path = "approve_tests.rs"]
mod tests;
