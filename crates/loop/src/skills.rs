//! Skill discovery and the listing (`docs/system-prompt.md`, "Skills" and
//! "Skills listing"): the places a skill is read from, the one skill each
//! name keeps, and the entries the opening message lists.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::events::{CommandInfo, Notice, SkillInfo, SkillListed, SkillSource};
use contract::shapes::ContentPart;

use crate::changes::{Stat, stat_of};
use crate::opening::canonical;
use crate::prompt::PromptInputs;
use crate::skill_header::{self, Header, Invalid};

/// A skill found in one place.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    /// The `SKILL.md` discovery opened, lossless.
    pub(crate) file: PathBuf,
    /// The listing entry it would have.
    pub(crate) listed: SkillListed,
    /// Whether the model may load it: its header does not switch that off
    /// and it is not in an extension's `prompts/`.
    pub(crate) model_invocable: bool,
    /// Its header's `argument-hint`, when it has one.
    pub(crate) argument_hint: Option<String>,
    /// The place it was read from: the canonical directory, or
    /// `extension <name>`.
    pub(crate) place: String,
    /// The extension's name, exactly for a skill from an extension's
    /// `skills/` or `prompts/`.
    pub(crate) extension: Option<String>,
}

/// What discovery found: one skill per name, and what went wrong.
pub(crate) struct Discovered {
    /// The first skill found under each name, in discovery order.
    pub(crate) skills: Vec<Found>,
    /// Every later skill found under a name the winners already hold, in
    /// discovery order: each is answered beside its winner, never listed
    /// or expanded (`docs/invocation.md`, "What each command does",
    /// `skills`).
    pub(crate) shadowed: Vec<Shadowed>,
    /// `io_failed`, `skill_invalid` and `skill_shadowed`, in discovery
    /// order.
    pub(crate) notices: Vec<Notice>,
    /// Each place directory, skill entry or `SKILL.md` whose read failed
    /// with an error other than not-found, as discovery logs it: the
    /// turn-start check keeps those skills from their last-known entries
    /// instead of reading them as removed (`docs/system-prompt.md`,
    /// "Added and removed skills").
    pub(crate) unread: Vec<PathBuf>,
}

/// A skill another skill shadows: the loser, and its winner's `SKILL.md`
/// path.
pub(crate) struct Shadowed {
    /// The skill left out of the listing.
    pub(crate) found: Found,
    /// The winner's `SKILL.md` path, as discovery logs it.
    pub(crate) by: String,
}

/// One directory holding a directory per skill.
struct Place {
    dir: PathBuf,
    source: SkillSource,
    label: Option<String>,
    /// The extension's name, exactly for an extension's `skills/` or
    /// `prompts/`.
    extension: Option<String>,
    /// A `prompts/` place: its skills are never listed.
    prompts: bool,
}

/// Every place in order, most specific first (`docs/system-prompt.md`,
/// "Skills"). The last place is the built-in one, `docs/skills/` in
/// Fiber home, so any other source wins a name. `top` is the repository's
/// top level, or the workspace outside git.
fn places(inputs: &PromptInputs, top: &Path) -> Vec<Place> {
    let place =
        |dir: PathBuf, source, label: Option<String>, extension: Option<String>, prompts| Place {
            dir,
            source,
            label,
            extension,
            prompts,
        };
    let mut places = vec![
        place(
            top.join(".fiber/skills"),
            SkillSource::Repository,
            None,
            None,
            false,
        ),
        place(
            top.join(".agents/skills"),
            SkillSource::Repository,
            None,
            None,
            false,
        ),
        place(
            inputs.home.join("skills"),
            SkillSource::Personal,
            None,
            None,
            false,
        ),
    ];
    if let Some(home) = &inputs.agents_home {
        places.push(place(
            home.join(".agents/skills"),
            SkillSource::Personal,
            None,
            None,
            false,
        ));
    }
    let mut extensions: Vec<&(String, PathBuf)> = inputs.extension_dirs.iter().collect();
    extensions.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, dir) in extensions {
        let label = Some(format!("extension {name}"));
        let extension = Some(name.clone());
        places.push(place(
            dir.join("skills"),
            SkillSource::Extension,
            label.clone(),
            extension.clone(),
            false,
        ));
        places.push(place(
            dir.join("prompts"),
            SkillSource::Extension,
            label,
            extension,
            true,
        ));
    }
    places.push(place(
        inputs.home.join("docs/skills"),
        SkillSource::Builtin,
        None,
        None,
        false,
    ));
    places
}

/// What the turn-start check remembers per `SKILL.md`: its size and
/// modification time when last read, and the header that read parsed.
/// A file whose size and time match is not read again, mirroring the
/// instruction-file check (`docs/system-prompt.md`, "When something
/// changes").
#[derive(Clone, Default)]
pub(crate) struct Cache {
    /// Each `SKILL.md` the last check read, by path as discovery logs it.
    entries: BTreeMap<PathBuf, (Stat, Result<Header, Invalid>)>,
}

/// Reads every place once and keeps the first skill under each name. A
/// place whose canonical path an earlier place already had is skipped.
pub(crate) fn discover(inputs: &PromptInputs, top: &Path) -> Discovered {
    discover_cached(inputs, top, &mut Cache::default())
}

/// Reads every place once and keeps the first skill under each name, as
/// [`discover`] does, but answers a `SKILL.md` from the cache when its
/// size and modification time match the last read. Paths the check no
/// longer sees leave the cache, so it never grows past the skills on
/// disk. A place whose canonical path an earlier place already had is
/// skipped.
pub(crate) fn discover_cached(inputs: &PromptInputs, top: &Path, cache: &mut Cache) -> Discovered {
    let mut skills: Vec<Found> = Vec::new();
    let mut shadowed: Vec<Shadowed> = Vec::new();
    let mut notices = Vec::new();
    let mut unread = Vec::new();
    let mut read: Vec<PathBuf> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for place in places(inputs, top) {
        let dir = canonical(&place.dir);
        if read.contains(&dir) {
            continue;
        }
        let label = place
            .label
            .clone()
            .unwrap_or_else(|| dir.display().to_string());
        for skill in read_place(&dir, &mut notices, &mut unread, cache) {
            seen.push(skill.path.clone());
            let header = match skill.header {
                Ok(header) => header,
                Err(invalid) => {
                    notices.push(invalid_notice(&skill.path, &invalid));
                    continue;
                }
            };
            let file = skill.path.clone();
            let path = skill.path.display().to_string();
            let found = Found {
                file,
                listed: SkillListed {
                    name: header.name,
                    description: header.description,
                    path,
                    source: place.source,
                },
                model_invocable: header.model_invocable && !place.prompts,
                argument_hint: header.argument_hint,
                place: label.clone(),
                extension: place.extension.clone(),
            };
            if let Some(by) = skills
                .iter()
                .find(|kept| kept.listed.name == found.listed.name)
                .map(|winner| winner.listed.path.clone())
            {
                notices.push(Notice {
                    code: ErrorCode::SkillShadowed,
                    message: format!(
                        "Skill {} at {} is shadowed by {}, which is used.",
                        found.listed.name, found.listed.path, by
                    ),
                    extension: None,
                });
                shadowed.push(Shadowed { found, by });
                continue;
            }
            skills.push(found);
        }
        read.push(dir);
    }
    cache.entries.retain(|path, _| seen.contains(path));
    Discovered {
        skills,
        shadowed,
        notices,
        unread,
    }
}

/// One `SKILL.md` read: its path as discovery logs it, and its header
/// or why it was left out. From the cache when its size and
/// modification time match the last read, read and parsed otherwise.
struct Read {
    path: PathBuf,
    header: Result<Header, Invalid>,
}

/// Each skill's `SKILL.md` in `dir`, in byte order of the directory
/// names. Only `<entry>/SKILL.md` is opened: nothing deeper. A place
/// directory, entry or file that cannot be read for an error other than
/// not-found joins `unread`, with the file and place failures also
/// keeping their `io_failed` notice; a missing place, a plain file, and
/// a link to nothing are skipped as today, with no notice.
fn read_place(
    dir: &Path,
    notices: &mut Vec<Notice>,
    unread: &mut Vec<PathBuf>,
    cache: &mut Cache,
) -> Vec<Read> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            notices.push(io_failed(dir, &e));
            unread.push(dir.to_path_buf());
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
        // A symlink is followed; a plain file, or a link to nothing, is
        // no skill. An entry that cannot be reached at all joins
        // `unread` with no notice: the turn-start check keeps its skill
        // from the last-known entry.
        match std::fs::metadata(&entry) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                unread.push(entry);
                continue;
            }
        }
        let path = entry.join("SKILL.md");
        // The size-and-time shortcut: unchanged without reading, as the
        // instruction-file check reads (`docs/system-prompt.md`, "When
        // something changes").
        if let (Some(now), Some((known, header))) = (
            stat_of(&path.display().to_string()),
            cache.entries.get(&path),
        ) && now == *known
        {
            found.push(Read {
                path,
                header: header.clone(),
            });
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) => {
                let header = skill_header::parse(&String::from_utf8_lossy(&bytes));
                if let Some(now) = stat_of(&path.display().to_string()) {
                    cache.entries.insert(path.clone(), (now, header.clone()));
                }
                found.push(Read { path, header });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                notices.push(io_failed(&path, &e));
                unread.push(path);
            }
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

/// A `prompt` whose first word is `/name`, split from its arguments
/// (`docs/invocation.md`, "Driver commands"): the first word runs to the
/// first whitespace char, the name is the word without the `/`, and the
/// arguments are the rest, with leading whitespace trimmed and trailing
/// kept as written. The text's leading whitespace is skipped. `None`
/// when the text names no command: it does not start with `/`, or the
/// name is empty.
pub(crate) fn split_command(text: &str) -> Option<(&str, &str)> {
    let command = text
        .trim_start_matches(|c: char| c.is_whitespace())
        .strip_prefix('/')?;
    let end = command
        .find(|c: char| c.is_whitespace())
        .unwrap_or(command.len());
    let (name, after) = (&command[..end], &command[end..]);
    if name.is_empty() {
        return None;
    }
    Some((name, after.trim_start_matches(|c: char| c.is_whitespace())))
}

/// Expands a `prompt` whose first word is `/name` into the skill's text,
/// then the rest of the prompt as its arguments (`docs/invocation.md`,
/// "Driver commands"): the body, then, when the arguments are non-empty,
/// one blank line and the arguments. `file` is the winner's `SKILL.md`,
/// as the session's maintained set holds it. `None` means "send as
/// written": the first part is no text, names no command, or the file
/// cannot be read at expansion. Only the first content part changes.
pub(crate) fn expand(file: &Path, content: &[ContentPart]) -> Option<Vec<ContentPart>> {
    let Some((ContentPart::Text { text }, rest)) = content.split_first() else {
        return None;
    };
    let (_, args) = split_command(text)?;
    let raw = std::fs::read(file).ok()?;
    let read = String::from_utf8_lossy(&raw);
    let body = skill_header::body(&read)?;
    let expanded = if args.is_empty() {
        body
    } else {
        format!("{body}\n\n{args}")
    };
    let mut out = Vec::with_capacity(content.len());
    out.push(ContentPart::Text { text: expanded });
    out.extend(rest.iter().cloned());
    Some(out)
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

/// The `commands` answer's rows (`docs/invocation.md`, "What each command
/// does"): every skill `disabled` does not name, in discovery order, tagged
/// `skill` when the model may load it and `template` when only a person
/// runs it.
pub(crate) fn commands(found: &[Found], disabled: &[String]) -> Vec<CommandInfo> {
    found
        .iter()
        .filter(|skill| !disabled.contains(&skill.listed.name))
        .map(|skill| CommandInfo {
            name: skill.listed.name.clone(),
            description: skill.listed.description.clone(),
            argument_hint: skill.argument_hint.clone(),
            tag: if skill.model_invocable {
                "skill"
            } else {
                "template"
            }
            .to_owned(),
        })
        .collect()
}

/// The `skills` answer's rows (`docs/invocation.md`, "What each command
/// does"): every skill discovery read, in discovery order, each winner
/// followed at once by the skills it shadows. A switched-off name marks
/// every row under it, the winner and each shadowed skill, and never
/// promotes one (`docs/system-prompt.md`, "Skills"). Whether the model
/// may load a skill is its header and place alone, unchanged by the
/// switch or the shadowing. A skill whose header is invalid is left out,
/// as discovery leaves it out.
pub(crate) fn rows(discovered: &Discovered, disabled: &[String]) -> Vec<SkillInfo> {
    let mut out = Vec::new();
    for winner in &discovered.skills {
        let losers: Vec<&Shadowed> = discovered
            .shadowed
            .iter()
            .filter(|shadowed| shadowed.found.listed.name == winner.listed.name)
            .collect();
        out.push(info(
            winner,
            disabled.contains(&winner.listed.name),
            losers
                .iter()
                .map(|shadowed| shadowed.found.listed.path.clone())
                .collect(),
            None,
        ));
        for loser in losers {
            out.push(info(
                &loser.found,
                disabled.contains(&loser.found.listed.name),
                Vec::new(),
                Some(loser.by.clone()),
            ));
        }
    }
    out
}

/// One `skills` row: the skill's name, description, path, source and
/// header-and-place eligibility, with its switch and shadowing marks.
fn info(
    found: &Found,
    disabled: bool,
    shadows: Vec<String>,
    shadowed_by: Option<String>,
) -> SkillInfo {
    SkillInfo {
        name: found.listed.name.clone(),
        description: found.listed.description.clone(),
        path: found.listed.path.clone(),
        source: found.listed.source,
        extension: found.extension.clone(),
        model_invocable: found.model_invocable,
        disabled,
        shadows,
        shadowed_by,
    }
}

/// One listing line.
pub(crate) fn entry(skill: &SkillListed) -> String {
    format!("- {}: {} ({})", skill.name, skill.description, skill.path)
}

/// The `skills_large` notice, when the rendered listing of `listed`
/// passes 10% of the context window (`docs/system-prompt.md`, "Size"),
/// estimated at four bytes a token. Exactly 10% is not over, so `>`
/// compares the unrounded cross products.
pub(crate) fn size_notice(listed: &[&Found], window: u64) -> Option<Notice> {
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

#[cfg(test)]
#[path = "skill_rows_tests.rs"]
mod rows_tests;
