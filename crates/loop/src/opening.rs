//! The opening message (`docs/system-prompt.md`, "The opening message"):
//! the environment and instruction files, read once at the first turn,
//! and the rendering of the logged fields back into the conversation.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use contract::ErrorCode;
use contract::events::{Environment, Git, InstructionFileSent, Notice, OpeningMessage};

use crate::prompt::{PromptInputs, fill};

const OPENING_MD: &str = include_str!("../prompt/opening.md");
const MESSAGES_MD: &str = include_str!("../prompt/messages.md");

/// A `CLAUDE.md` holding only this is never read: it points at `AGENTS.md`
/// (`docs/system-prompt.md`, "Instruction files"; the measured case is a
/// file whose whole content is `@AGENTS.md`).
const AGENTS_POINTER: &str = "@AGENTS.md";

/// What the first turn writes after `preamble_built`: the opening message
/// and the notices its build called for.
pub(crate) struct Collected {
    /// The `opening_message` payload.
    pub(crate) message: OpeningMessage,
    /// One `io_failed` per unreadable instruction file, then
    /// `instructions_large` when the instruction text passes 10% of the
    /// context window.
    pub(crate) notices: Vec<Notice>,
}

/// Reads the environment and the instruction files once
/// (`docs/system-prompt.md`, "Environment" and "Instruction files").
/// `workspace` is the loop's canonical workspace.
pub(crate) fn collect(inputs: &PromptInputs, workspace: &Path) -> Collected {
    let workspace = canonical(workspace);
    let home = canonical(&inputs.home);
    let (chain, git) = repo_chain(&workspace);
    let mut files = Vec::new();
    let mut notices = Vec::new();
    // The global file first, then the chain from the repository's top
    // level down to the workspace.
    read_candidate(&home.join("AGENTS.md"), &mut files, &mut notices);
    for dir in &chain {
        // The global file already covered the home directory: reading it
        // again would send `<home>/AGENTS.md` twice.
        if dir == &home {
            continue;
        }
        read_dir_file(dir, &mut files, &mut notices);
    }
    let message = OpeningMessage {
        environment: Environment {
            date: date_of(inputs.clock.wall()),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            shell: inputs.shell.clone(),
            workspace: workspace.display().to_string(),
            git,
            session_log: canonical(Path::new(&inputs.session_log))
                .display()
                .to_string(),
        },
        instruction_files: files,
        // debt: no skill runtime lists skills yet; fixed by #511.
        skills: Vec::new(),
    };
    let large = size_notice(inputs, &message.instruction_files);
    notices.extend(large);
    Collected { message, notices }
}

/// Renders `message` from its logged fields only: no clock, no disk, so a
/// resume renders the identical bytes (`docs/system-prompt.md`,
/// "Recording").
pub(crate) fn render(message: &OpeningMessage) -> String {
    let environment = &message.environment;
    let git = match &environment.git {
        None => "no".to_owned(),
        Some(git) => match &git.branch {
            Some(branch) => format!("yes, branch {branch}"),
            None => "yes, detached HEAD".to_owned(),
        },
    };
    let files = if message.instruction_files.is_empty() {
        crate::prompt::body(MESSAGES_MD, "no-instruction-files")
    } else {
        message
            .instruction_files
            .iter()
            .map(|file| {
                let dir = Path::new(&file.path)
                    .parent()
                    .map(|parent| parent.display().to_string())
                    .unwrap_or_default();
                fill(
                    &crate::prompt::body(MESSAGES_MD, "instruction-file"),
                    &[
                        ("path", file.path.as_str()),
                        ("dir", dir.as_str()),
                        ("content", file.content.as_str()),
                    ],
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    // With an empty listing the `# Skills` heading and `{skills}` are left
    // out: the template is cut at its last `# Skills` line. Cutting before
    // the one `fill` keeps inserted text (a path or a file's content
    // holding `{date}`) unre-scanned.
    // debt: the skills listing is always empty and its entry format is
    // provisional; fixed by #511.
    let template = if message.skills.is_empty() {
        cut_skills(OPENING_MD)
    } else {
        OPENING_MD.to_owned()
    };
    let skills = message
        .skills
        .iter()
        .map(|skill| format!("- {}: {} ({})", skill.name, skill.description, skill.path))
        .collect::<Vec<_>>()
        .join("\n");
    fill(
        &template,
        &[
            ("date", environment.date.as_str()),
            ("os", environment.os.as_str()),
            ("arch", environment.arch.as_str()),
            ("shell", environment.shell.as_str()),
            ("workspace", environment.workspace.as_str()),
            ("git", git.as_str()),
            ("session_log", environment.session_log.as_str()),
            ("instruction_files", files.as_str()),
            ("skills", skills.as_str()),
        ],
    )
}

/// `YYYY-MM-DD` of `wall`, in UTC, computed without a date crate: days
/// since the epoch to civil date. A time before the epoch reads as the
/// epoch's date.
pub(crate) fn date_of(wall: SystemTime) -> String {
    let days = wall
        .duration_since(UNIX_EPOCH)
        .map(|ago| ago.as_secs() / 86_400)
        .unwrap_or(0);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Proleptic Gregorian date of `days` since 1970-01-01.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let era = (days + 719_468) / 146_097;
    let start = days + 719_468 - era * 146_097;
    let year = (start - start / 1_460 + start / 36_524 - start / 146_096) / 365;
    let ordinal = start - (365 * year + year / 4 - year / 100);
    let month = (5 * ordinal + 2) / 153;
    let day = ordinal - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    let year = if month <= 2 {
        year + era * 400 + 1
    } else {
        year + era * 400
    };
    (year, month, day)
}

/// The chain of directories holding instruction files, and the git state:
/// in a repository, each directory from its top level down to `workspace`;
/// outside one, `workspace` alone. The top level is the directory holding
/// `.git`. All paths are canonical, as `workspace` is.
fn repo_chain(workspace: &Path) -> (Vec<PathBuf>, Option<Git>) {
    let mut up = vec![workspace.to_path_buf()];
    let mut dir = workspace;
    loop {
        let dotgit = dir.join(".git");
        match std::fs::metadata(&dotgit) {
            Ok(meta) if meta.is_dir() => {
                up.reverse();
                return (up, Some(read_head(&dotgit)));
            }
            Ok(_) => {
                // Reading a non-file fails and returns `None`, so no guard
                // is needed: only a `gitdir:` line names a repository here.
                if let Some(target) = gitdir_target(&dotgit, dir) {
                    up.reverse();
                    return (up, Some(read_head(&target)));
                }
                // A `.git` file without a `gitdir:` line names no
                // repository here; keep looking above.
            }
            // Absent or unreadable: no repository at this level.
            Err(_) => {}
        }
        match dir.parent() {
            Some(parent) => {
                dir = parent;
                up.push(dir.to_path_buf());
            }
            None => return (vec![workspace.to_path_buf()], None),
        }
    }
}

/// The branch `gitdir`'s `HEAD` names, `None` when detached or unreadable.
fn read_head(gitdir: &Path) -> Git {
    let branch = std::fs::read(gitdir.join("HEAD")).ok().and_then(|bytes| {
        let line = String::from_utf8_lossy(&bytes);
        line.trim()
            .strip_prefix("ref: ")
            .and_then(|named| named.strip_prefix("refs/heads/"))
            .map(str::to_owned)
    });
    Git { branch }
}

/// The repository a worktree `.git` file points at: its `gitdir:` line,
/// relative to `dir` when it is not absolute. `None` when the line is
/// missing. A target outside the tree still counts: the top level is the
/// directory holding the `.git` file.
fn gitdir_target(dotgit: &Path, dir: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(dotgit).ok()?;
    let line = String::from_utf8_lossy(&bytes);
    let target = line.lines().next()?.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let target = Path::new(target);
    Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        dir.join(target)
    })
}

/// The instruction file `dir` holds: `AGENTS.md`, or `CLAUDE.md` when
/// there is no `AGENTS.md`. A file that cannot be read is left out and a
/// notice names it; an empty file is sent as it is.
fn read_dir_file(dir: &Path, files: &mut Vec<InstructionFileSent>, notices: &mut Vec<Notice>) {
    if read_candidate(&dir.join("AGENTS.md"), files, notices) {
        return;
    }
    let claude = dir.join("CLAUDE.md");
    match std::fs::read(&claude) {
        Ok(bytes) => {
            // A `CLAUDE.md` whose only content points at `AGENTS.md` is
            // never read.
            if String::from_utf8_lossy(&bytes).trim() != AGENTS_POINTER {
                files.push(sent(&claude, &bytes));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => notices.push(io_failed(&claude, &e)),
    }
}

/// The global file: `<home>/AGENTS.md` only. Absent is no file; unreadable
/// is a notice. Returns whether `path` was present or unreadable: `false`
/// only when absent, so only an absent file falls through to `CLAUDE.md`.
fn read_candidate(
    path: &Path,
    files: &mut Vec<InstructionFileSent>,
    notices: &mut Vec<Notice>,
) -> bool {
    match std::fs::read(path) {
        Ok(bytes) => {
            files.push(sent(path, &bytes));
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            notices.push(io_failed(path, &e));
            true
        }
    }
}

/// `path`'s bytes, read lossily: a file that is not valid UTF-8 still
/// reaches the model.
fn sent(path: &Path, bytes: &[u8]) -> InstructionFileSent {
    InstructionFileSent {
        path: path.display().to_string(),
        content: String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// An unreadable instruction file's notice: it names the path.
fn io_failed(path: &Path, error: &std::io::Error) -> Notice {
    Notice {
        code: ErrorCode::IoFailed,
        message: format!(
            "Could not read instruction file {}: {error}.",
            path.display()
        ),
        extension: None,
    }
}

/// The `instructions_large` notice, when the instruction text passes 10%
/// of the context window (`docs/system-prompt.md`, "Size"). Instruction
/// text is the instruction files, extension texts, `SYSTEM.md` and
/// `APPEND_SYSTEM.md`, estimated at four bytes a token. `None` when the
/// window is unknown (`0`): no basis for the check. Exactly 10% is not
/// over, so `>` compares the unrounded cross products.
fn size_notice(inputs: &PromptInputs, files: &[InstructionFileSent]) -> Option<Notice> {
    let window = inputs.context_window.unwrap_or(0);
    // Unknown context window: the 10% check has nothing to compare with.
    if window == 0 {
        return None;
    }
    let mut sources: Vec<(String, u128)> = Vec::new();
    for file in files {
        sources.push((file.path.clone(), file.content.len() as u128));
    }
    for (name, text) in inputs
        .system
        .iter()
        .map(|text| ("SYSTEM.md", text))
        .chain(inputs.append.iter().map(|text| ("APPEND_SYSTEM.md", text)))
    {
        if !text.trim().is_empty() {
            sources.push((name.to_owned(), text.len() as u128));
        }
    }
    for (name, text) in &inputs.extensions {
        if !text.trim().is_empty() {
            sources.push((name.clone(), text.len() as u128));
        }
    }
    let total: u128 = sources.iter().map(|(_, bytes)| bytes).sum();
    if total * 10 <= u128::from(window) * 4 {
        return None;
    }
    sources.sort_by_key(|source| std::cmp::Reverse(source.1));
    let largest: Vec<String> = sources
        .into_iter()
        .take(3)
        .map(|(name, bytes)| format!("{name} ({bytes} bytes)"))
        .collect();
    Some(Notice {
        code: ErrorCode::InstructionsLarge,
        message: format!(
            "Instruction text is about {} tokens, over 10% of the {window}-token context window. Largest: {}.",
            total / 4,
            largest.join(", ")
        ),
        extension: None,
    })
}

/// `OPENING_MD` cut at its last `# Skills` line, trailing blank lines
/// removed, for a message with an empty skills listing.
fn cut_skills(template: &str) -> String {
    match template.rfind("# Skills") {
        Some(at) => template[..at].trim_end().to_owned(),
        None => template.to_owned(),
    }
}

/// `path` with symlinks resolved, or as is when it cannot be read.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[path = "opening_tests.rs"]
mod tests;
