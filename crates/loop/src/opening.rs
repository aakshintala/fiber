//! The opening message (`docs/system-prompt.md`, "The opening message"):
//! the environment and instruction files, read once at the first turn,
//! and the rendering of the logged fields back into the conversation.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use contract::ErrorCode;
use contract::events::{
    Environment, ExtensionSectionSent, Git, InstructionFileSent, Notice, OpeningMessage,
};

use crate::prompt::{PromptInputs, fill};
use crate::skills;

const OPENING_MD: &str = include_str!("../prompt/opening.md");

/// A `CLAUDE.md` holding only this is never read: it points at `AGENTS.md`
/// (`docs/system-prompt.md`, "Instruction files"; the measured case is a
/// file whose whole content is `@AGENTS.md`).
const AGENTS_POINTER: &str = "@AGENTS.md";

/// What the first turn writes after `preamble_built`: the opening message
/// and the notices its build called for.
pub(crate) struct Collected {
    /// The `opening_message` payload.
    pub(crate) message: OpeningMessage,
    /// One `io_failed` per unreadable instruction file, then one per
    /// unreadable section file, then `instructions_large` when the
    /// instruction text passes 10% of the context window, then the skill
    /// notices in discovery order, then `skills_large` when the listing
    /// does.
    pub(crate) notices: Vec<Notice>,
    /// The discovery's winners, in discovery order: what the listing was
    /// built from, and what the maintained set is refreshed with.
    pub(crate) found: Vec<skills::Found>,
}

/// Reads the environment, the instruction files and the skills once
/// (`docs/system-prompt.md`, "Environment", "Instruction files" and
/// "Skills").
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
    let found = skills::discover(inputs, chain.first().unwrap_or(&workspace));
    let listed = skills::listing(&found.skills, &inputs.skills_disabled);
    let mut sections = Vec::new();
    for (name, paths, budget) in &inputs.extension_sections {
        let mut files = Vec::new();
        for path in paths {
            match std::fs::read(path) {
                Ok(bytes) => files.push(sent(path, &bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => notices.push(section_io_failed(path, &e, name)),
            }
        }
        // An extension none of whose files exist has no section.
        if !files.is_empty() {
            sections.push(ExtensionSectionSent {
                extension: name.clone(),
                files,
                budget_bytes: *budget,
            });
        }
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
        extension_sections: sections,
        skills: listed.iter().map(|skill| skill.listed.clone()).collect(),
    };
    let large = size_notice(inputs, &message);
    notices.extend(large);
    notices.extend(found.notices);
    notices.extend(skills::size_notice(&listed, inputs.context_window));
    Collected {
        message,
        notices,
        found: found.skills,
    }
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
        crate::prompt::message("no-instruction-files").to_owned()
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
                    crate::prompt::message("instruction-file"),
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
    // The sections are appended to the instruction-files text, so a
    // message with no sections renders byte-identical to today's.
    let sections = message
        .extension_sections
        .iter()
        .map(render_section)
        .collect::<Vec<_>>()
        .join("\n\n");
    let instruction_files = if sections.is_empty() {
        files
    } else if files.is_empty() {
        sections
    } else {
        format!("{files}\n\n{sections}")
    };
    // With an empty listing the `# Skills` heading and `{skills}` are left
    // out: the template is cut at its last `# Skills` line. Cutting before
    // the one `fill` keeps inserted text (a path or a file's content
    // holding `{date}`) unre-scanned.
    let template = if message.skills.is_empty() {
        cut_skills(OPENING_MD)
    } else {
        OPENING_MD.to_owned()
    };
    let skills = message
        .skills
        .iter()
        .map(skills::entry)
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
            ("instruction_files", instruction_files.as_str()),
            ("skills", skills.as_str()),
        ],
    )
}

/// The prune line ending an over-budget section, and the line a
/// `write` or `edit` result carries while its section is over budget
/// (`docs/system-prompt.md`, "Extension sections"): one helper, so the
/// two never drift.
pub(crate) fn budget_line(size: u64, budget: u64) -> String {
    fill(
        crate::prompt::message("budget-line"),
        &[("size", &size.to_string()), ("budget", &budget.to_string())],
    )
}

/// One extension section, rendered from its logged fields only: each file
/// under its path, then the budget line when the files together are over
/// the budget (`docs/system-prompt.md`, "Extension sections").
fn render_section(section: &ExtensionSectionSent) -> String {
    let files = section
        .files
        .iter()
        .map(|file| {
            fill(
                crate::prompt::message("extension-file"),
                &[
                    ("path", file.path.as_str()),
                    ("content", file.content.as_str()),
                ],
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut text = fill(
        crate::prompt::message("extension-section"),
        &[
            ("extension", section.extension.as_str()),
            ("files", files.as_str()),
        ],
    );
    // Over means strictly greater: exactly at budget has no line. The
    // size is the sent contents' length, so a resume renders the same.
    let size: u64 = section
        .files
        .iter()
        .map(|file| file.content.len() as u64)
        .sum();
    if let Some(budget) = section.budget_bytes
        && size > budget
    {
        text.push_str("\n\n");
        text.push_str(&budget_line(size, budget));
    }
    text
}

/// `YYYY-MM-DD` of `wall`, in UTC, computed without a date crate: days
/// since the epoch to civil date. A time before the epoch reads as the
/// epoch's date.
pub(crate) fn date_of(wall: SystemTime) -> String {
    let (year, month, day) = contract::clock::utc_date(wall);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The chain of directories holding instruction files, and the git state:
/// in a repository, each directory from its top level down to `workspace`;
/// outside one, `workspace` alone. The top level is the directory holding
/// `.git`. All paths are canonical, as `workspace` is.
pub(crate) fn repo_chain(workspace: &Path) -> (Vec<PathBuf>, Option<Git>) {
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

/// The instruction file `dir` contributes, if any: `AGENTS.md`, or
/// `CLAUDE.md` when there is no `AGENTS.md`. A `CLAUDE.md` whose only
/// content points at `AGENTS.md` counts as no file. A path that is
/// present but cannot be read still counts: reading it names the
/// failure. An absent `CLAUDE.md` counts too, so the read below tells a
/// file deleted after the check from an unreadable one: gone is silence,
/// unreadable is a notice. The one precedence the opening message and
/// the change check share.
pub(crate) fn candidate(dir: &Path) -> Option<PathBuf> {
    let agents = dir.join("AGENTS.md");
    match std::fs::metadata(&agents) {
        Ok(_) => Some(agents),
        // Absent: `CLAUDE.md` may stand in. Unreadable: the file is
        // there, so it stays the candidate and the read names it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let claude = dir.join("CLAUDE.md");
            match std::fs::read(&claude) {
                // A `CLAUDE.md` whose only content points at
                // `AGENTS.md` is never read.
                Ok(bytes) if String::from_utf8_lossy(&bytes).trim() != AGENTS_POINTER => {
                    Some(claude)
                }
                // Holding only the pointer: no file. Absent, or present
                // but unreadable: the read below names the failure or
                // stays silent.
                Ok(_) => None,
                Err(_) => Some(claude),
            }
        }
        Err(_) => Some(agents),
    }
}

/// The instruction file `dir` holds: [`candidate`], read in full. Gone
/// after the check is left out silently; present but unreadable is left
/// out and a notice names it; an empty file is sent as it is.
fn read_dir_file(dir: &Path, files: &mut Vec<InstructionFileSent>, notices: &mut Vec<Notice>) {
    let Some(path) = candidate(dir) else {
        return;
    };
    match std::fs::read(&path) {
        Ok(bytes) => files.push(sent(&path, &bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => notices.push(io_failed(&path, &e)),
    }
}

/// One `AGENTS.md` candidate: the global `<home>/AGENTS.md` or a chain
/// directory's. Absent is no file; unreadable is a notice. Returns whether `path` was present or unreadable: `false`
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

/// An unreadable section file's notice: it names the path and the section's
/// extension.
pub(crate) fn section_io_failed(path: &Path, error: &std::io::Error, extension: &str) -> Notice {
    Notice {
        code: ErrorCode::IoFailed,
        message: format!(
            "Could not read section file {} from the {extension} extension: {error}.",
            path.display()
        ),
        extension: Some(extension.to_owned()),
    }
}

/// An unreadable instruction file's notice: it names the path.
pub(crate) fn io_failed(path: &Path, error: &std::io::Error) -> Notice {
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
/// text is the instruction files, extension sections, extension texts,
/// `SYSTEM.md` and `APPEND_SYSTEM.md`, estimated at four bytes a token.
/// Exactly 10% is not over, so `>` compares the unrounded cross products.
fn size_notice(inputs: &PromptInputs, message: &OpeningMessage) -> Option<Notice> {
    let window = inputs.context_window;
    let mut sources: Vec<(String, u128)> = Vec::new();
    for file in message.instruction_files.iter().chain(
        message
            .extension_sections
            .iter()
            .flat_map(|section| &section.files),
    ) {
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
    let largest = crate::prompt::oversize(total, window, sources)?;
    Some(Notice {
        code: ErrorCode::InstructionsLarge,
        message: format!(
            "Instruction text is about {} tokens, over 10% of the {window}-token context window. Largest: {largest}.",
            total / 4,
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
pub(crate) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[path = "opening_tests.rs"]
mod tests;
