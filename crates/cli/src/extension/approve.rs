//! The extension install and remove prompts (`docs/extensions.md`, "What an
//! install shows" and "Installing"): what an install shows before it goes
//! ahead, and what a remove lists before it deletes. In a terminal each
//! writes to `out` and asks; without a terminal each goes ahead without
//! asking.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::shapes::Failure;
use doors::failure;

/// What `fiber extension install` shows before it installs (`docs/extensions.md`,
/// "What an install shows"): what the manifest and the files tell. Tools,
/// hooks, watchers and commands are not shown until `docs/extensions.md`
/// settles how Fiber learns them before the extension runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallSummary {
    /// The extension's name.
    pub name: String,
    /// Where it is installed from.
    pub source: String,
    /// Its version.
    pub version: String,
    /// For an update, what changed since the installed commit.
    pub changes: Option<String>,
    /// The built-in tools and commands it replaces.
    pub replaces: Vec<String>,
    /// Each provider it registers, with its models' base URLs.
    pub providers: Vec<(String, Vec<String>)>,
    /// The program a process extension runs, with its arguments.
    pub process: Option<String>,
    /// Its install step.
    pub install_step: Option<String>,
    /// What it carries, one line each: skills, prompt templates, themes,
    /// binaries and the TUI extension.
    pub carries: Vec<String>,
    /// Its files as staged, which are what will be installed.
    pub staged: PathBuf,
}

/// Whether `fiber extension install` goes ahead (`docs/extensions.md`, "Installing"):
/// in a terminal it writes each of `summaries` to `out` and asks once, and
/// only `y` or `yes` goes ahead; `s` shows every file and asks again.
/// Without a terminal it goes ahead without asking.
pub(crate) fn install_approved(
    summaries: &[InstallSummary],
    terminal: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<bool, Failure> {
    if !terminal {
        return Ok(true);
    }
    let io = |e: io::Error| failure(ErrorCode::IoFailed, format!("the terminal: {e}"));
    let mut text = String::new();
    for summary in summaries {
        let verb = if summary.changes.is_some() {
            "Update"
        } else {
            "Install"
        };
        text.push_str(&format!(
            "{verb} {} from {}\nVersion {}\n",
            summary.name, summary.source, summary.version
        ));
        if let Some(changes) = &summary.changes {
            text.push_str(&format!("Changes since the installed commit:\n{changes}"));
            if !changes.ends_with('\n') {
                text.push('\n');
            }
        }
        for built_in in &summary.replaces {
            text.push_str(&format!("Replaces `{built_in}`\n"));
        }
        if summary.providers.is_empty() {
            text.push_str("It registers no provider.\n");
        }
        for (provider, urls) in &summary.providers {
            text.push_str(&format!("Provider {provider}: {}\n", urls.join(", ")));
        }
        if let Some(process) = &summary.process {
            text.push_str(&format!("Runs the program: {process}\n"));
        }
        if let Some(step) = &summary.install_step {
            text.push_str(&format!(
                "Install step, run now and at every update: {step}\n\
                 Its dependencies' own install scripts run too.\n"
            ));
        }
        for line in &summary.carries {
            text.push_str(&format!("Carries {line}\n"));
        }
    }
    out.write_all(text.as_bytes()).map_err(io)?;
    loop {
        out.write_all(b"Go ahead? [y/N/s to show the full source] ")
            .and_then(|()| out.flush())
            .map_err(io)?;
        let mut answer = String::new();
        input.read_line(&mut answer).map_err(io)?;
        match answer.trim() {
            "s" | "S" => {
                for summary in summaries {
                    show_source(&summary.name, &summary.staged, out).map_err(io)?;
                }
            }
            "y" | "Y" | "yes" => return Ok(true),
            _ => return Ok(false),
        }
    }
}

/// Writes every file under `dir`, in path order, each after its relative
/// path. A file that is not text is named with its size.
pub(crate) fn show_source(name: &str, dir: &Path, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "=== {name}")?;
    let mut files = Vec::new();
    collect_files(dir, &mut files)?;
    files.sort();
    for file in files {
        let shown = file
            .strip_prefix(dir)
            .unwrap_or(&file)
            .display()
            .to_string();
        match std::fs::read(&file).map(String::from_utf8) {
            Ok(Ok(text)) => {
                writeln!(out, "--- {shown}")?;
                out.write_all(text.as_bytes())?;
                if !text.ends_with('\n') {
                    writeln!(out)?;
                }
            }
            Ok(Err(e)) => writeln!(out, "--- {shown} ({} bytes, not text)", e.as_bytes().len())?,
            Err(_) => writeln!(out, "--- {shown} (a link, or unreadable)")?,
        }
    }
    Ok(())
}

/// Every file under `dir`, descending into each subdirectory. A link is a
/// file, not a directory: `file_type` does not follow it, so it is listed
/// and `show_source` names it unreadable when reading through it fails.
fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), files)?;
        } else {
            files.push(entry.path());
        }
    }
    Ok(())
}

/// Whether `fiber extension remove` goes ahead: in a terminal it lists what it will
/// delete, the extensions and their data and settings, and asks; without a
/// terminal it goes ahead (`docs/state.md`, "Extension data").
pub(crate) fn remove_approved(
    names: &[String],
    data: &[PathBuf],
    terminal: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<bool, Failure> {
    if !terminal {
        return Ok(true);
    }
    let io = |e: io::Error| failure(ErrorCode::IoFailed, format!("the terminal: {e}"));
    let mut text = String::new();
    for name in names {
        text.push_str(&format!("Remove {name}\n"));
    }
    for path in data {
        text.push_str(&format!("Delete {}\n", path.display()));
    }
    text.push_str("Go ahead? [y/N] ");
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(io)?;
    let mut answer = String::new();
    input.read_line(&mut answer).map_err(io)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

#[cfg(test)]
#[path = "approve_tests.rs"]
mod tests;
