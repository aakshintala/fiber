//! Skill discovery and the listing (`docs/system-prompt.md`, "Skills" and
//! "Skills listing"): the places a skill is read from, the one skill each
//! name keeps, and the entries the opening message lists.

use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::events::{Notice, SkillListed, SkillSource};

use crate::opening::canonical;
use crate::prompt::PromptInputs;
use crate::skill_header::{self, Invalid};

/// A skill found in one place.
pub(crate) struct Found {
    /// The listing entry it would have.
    pub(crate) listed: SkillListed,
    /// Whether the model may load it: its header does not switch that off
    /// and it is not in an extension's `prompts/`.
    pub(crate) model_invocable: bool,
    /// The place it was read from: the canonical directory, or
    /// `extension <name>`.
    pub(crate) place: String,
}

/// What discovery found: one skill per name, and what went wrong.
pub(crate) struct Discovered {
    /// The first skill found under each name, in discovery order.
    pub(crate) skills: Vec<Found>,
    /// `io_failed`, `skill_invalid` and `skill_shadowed`, in discovery
    /// order.
    pub(crate) notices: Vec<Notice>,
}

/// One directory holding a directory per skill.
struct Place {
    dir: PathBuf,
    source: SkillSource,
    label: Option<String>,
    /// A `prompts/` place: its skills are never listed.
    prompts: bool,
}

/// Every place in order, most specific first (`docs/system-prompt.md`,
/// "Skills"). `top` is the repository's top level, or the workspace
/// outside git.
fn places(inputs: &PromptInputs, top: &Path) -> Vec<Place> {
    let place = |dir: PathBuf, source, label: Option<String>, prompts| Place {
        dir,
        source,
        label,
        prompts,
    };
    let mut places = vec![
        place(
            top.join(".fiber/skills"),
            SkillSource::Repository,
            None,
            false,
        ),
        place(
            top.join(".agents/skills"),
            SkillSource::Repository,
            None,
            false,
        ),
        place(
            inputs.home.join("skills"),
            SkillSource::Personal,
            None,
            false,
        ),
    ];
    if let Some(home) = &inputs.agents_home {
        places.push(place(
            home.join(".agents/skills"),
            SkillSource::Personal,
            None,
            false,
        ));
    }
    let mut extensions: Vec<&(String, PathBuf)> = inputs.extension_dirs.iter().collect();
    extensions.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, dir) in extensions {
        let label = Some(format!("extension {name}"));
        places.push(place(
            dir.join("skills"),
            SkillSource::Extension,
            label.clone(),
            false,
        ));
        places.push(place(
            dir.join("prompts"),
            SkillSource::Extension,
            label,
            true,
        ));
    }
    // debt: no built-in skill is compiled in, so the built-in source adds
    // no place; fixed by #760.
    places
}

/// Reads every place once and keeps the first skill under each name. A
/// place whose canonical path an earlier place already had is skipped.
pub(crate) fn discover(inputs: &PromptInputs, top: &Path) -> Discovered {
    let mut skills: Vec<Found> = Vec::new();
    let mut notices = Vec::new();
    let mut read: Vec<PathBuf> = Vec::new();
    for place in places(inputs, top) {
        let dir = canonical(&place.dir);
        if read.contains(&dir) {
            continue;
        }
        let label = place
            .label
            .clone()
            .unwrap_or_else(|| dir.display().to_string());
        for (path, text) in read_place(&dir, &mut notices) {
            let header = match skill_header::parse(&text) {
                Ok(header) => header,
                Err(invalid) => {
                    notices.push(invalid_notice(&path, &invalid));
                    continue;
                }
            };
            let path = path.display().to_string();
            if let Some(winner) = skills.iter().find(|found| found.listed.name == header.name) {
                notices.push(Notice {
                    code: ErrorCode::SkillShadowed,
                    message: format!(
                        "Skill {} at {path} is shadowed by {}, which is used.",
                        header.name, winner.listed.path
                    ),
                    extension: None,
                });
                continue;
            }
            skills.push(Found {
                listed: SkillListed {
                    name: header.name,
                    description: header.description,
                    path,
                    source: place.source,
                },
                model_invocable: header.model_invocable && !place.prompts,
                place: label.clone(),
            });
        }
        read.push(dir);
    }
    Discovered { skills, notices }
}

/// Each skill's `SKILL.md` path and text in `dir`, in byte order of the
/// directory names. Only `<entry>/SKILL.md` is opened: nothing deeper.
fn read_place(dir: &Path, notices: &mut Vec<Notice>) -> Vec<(PathBuf, String)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            notices.push(io_failed(dir, &e));
            return Vec::new();
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => names.push(entry.file_name()),
            Err(e) => notices.push(io_failed(dir, &e)),
        }
    }
    names.sort();
    let mut found = Vec::new();
    for name in names {
        let entry = dir.join(name);
        // A symlink is followed; a plain file, or a link to nothing, is no
        // skill.
        if !std::fs::metadata(&entry).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        let path = entry.join("SKILL.md");
        match std::fs::read(&path) {
            Ok(bytes) => found.push((path, String::from_utf8_lossy(&bytes).into_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => notices.push(io_failed(&path, &e)),
        }
    }
    found
}

fn io_failed(path: &Path, error: &std::io::Error) -> Notice {
    Notice {
        code: ErrorCode::IoFailed,
        message: format!("Could not read skill {}: {error}.", path.display()),
        extension: None,
    }
}

fn invalid_notice(path: &Path, invalid: &Invalid) -> Notice {
    let reason = match invalid {
        Invalid::DoesNotParse => "its header does not parse",
        Invalid::NoName => "it has no name",
        Invalid::NoDescription => "it has no description",
    };
    Notice {
        code: ErrorCode::SkillInvalid,
        message: format!("Skill {} was left out: {reason}.", path.display()),
        extension: None,
    }
}

/// The skills the model may load, by name: those whose header allows it
/// and `disabled` does not name.
pub(crate) fn listing<'a>(found: &'a [Found], disabled: &[String]) -> Vec<&'a Found> {
    let mut listed: Vec<&Found> = found
        .iter()
        .filter(|skill| skill.model_invocable && !disabled.contains(&skill.listed.name))
        .collect();
    listed.sort_by(|a, b| a.listed.name.cmp(&b.listed.name));
    listed
}

/// One listing line.
pub(crate) fn entry(skill: &SkillListed) -> String {
    format!("- {}: {} ({})", skill.name, skill.description, skill.path)
}

/// The `skills_large` notice, when the rendered listing of `listed`
/// passes 10% of the context window (`docs/system-prompt.md`, "Size"),
/// estimated at four bytes a token. Exactly 10% is not over, so `>`
/// compares the unrounded cross products. `None` when the window is
/// unknown.
pub(crate) fn size_notice(listed: &[&Found], window: Option<u64>) -> Option<Notice> {
    let window = window.unwrap_or(0);
    // Unknown context window: the 10% check has nothing to compare with.
    if window == 0 {
        return None;
    }
    let mut places: Vec<(&str, u128)> = Vec::new();
    let mut total: u128 = 0;
    for skill in listed {
        let bytes = entry(&skill.listed).len() as u128;
        // One line break joins this entry to the one before.
        total += bytes + u128::from(total > 0);
        match places.iter_mut().find(|(place, _)| *place == skill.place) {
            Some((_, sum)) => *sum += bytes,
            None => places.push((&skill.place, bytes)),
        }
    }
    if total * 10 <= u128::from(window) * 4 {
        return None;
    }
    places.sort_by_key(|place| std::cmp::Reverse(place.1));
    let largest: Vec<String> = places
        .into_iter()
        .take(3)
        .map(|(place, bytes)| format!("{place} ({bytes} bytes)"))
        .collect();
    Some(Notice {
        code: ErrorCode::SkillsLarge,
        message: format!(
            "The skills listing is about {} tokens, over 10% of the {window}-token context window. Largest: {}.",
            total / 4,
            largest.join(", ")
        ),
        extension: None,
    })
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;
