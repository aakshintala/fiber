//! The text the setup writes: the command owns the bullets under its topic
//! headings, and every other byte of a layer's notes is the person's
//! (`docs/permissions.md`, "Setting up the notes").

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use contract::notes::Topic;
use similar::{Algorithm, DiffTag, capture_diff_slices};

/// The notes layer a line goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotesLayer {
    /// The project's own `config.json`.
    PerProject,
    /// Fiber home's `config.json`.
    Global,
}

impl NotesLayer {
    /// The tag a proposed line carries.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::PerProject => "[per-project]",
            Self::Global => "[global]",
        }
    }
}

/// Where a proposed line's evidence comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The project's own files.
    Files,
    /// The person's answer to the topic's question.
    Answer,
    /// Escalations a person answered allow, from this project only when
    /// `all_here`.
    Session {
        /// Whether every escalation behind the line came from this project.
        all_here: bool,
    },
}

/// The layer a line goes to before the person flips it: a line from the
/// project's files goes to the per-project notes, an answer to the global
/// notes, and a session line to the per-project notes only when every
/// escalation behind it came from this project.
pub(crate) fn default_layer(source: Source) -> NotesLayer {
    match source {
        Source::Files | Source::Session { all_here: true } => NotesLayer::PerProject,
        Source::Answer | Source::Session { all_here: false } => NotesLayer::Global,
    }
}

/// Unicode 15's format characters (category Cf). `std` has no category
/// query and no crate for one is a dependency this file's job needs.
const FORMAT: &[(u32, u32)] = &[
    (0x00AD, 0x00AD),
    (0x0600, 0x0605),
    (0x061C, 0x061C),
    (0x06DD, 0x06DD),
    (0x070F, 0x070F),
    (0x0890, 0x0891),
    (0x08E2, 0x08E2),
    (0x180E, 0x180E),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x2064),
    (0x2066, 0x206F),
    (0xFEFF, 0xFEFF),
    (0xFFF9, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0001, 0xE0001),
    (0xE0020, 0xE007F),
];

fn is_format(c: char) -> bool {
    let code = u32::from(c);
    FORMAT.iter().any(|(lo, hi)| (*lo..=*hi).contains(&code))
}

/// A line as it is shown and saved: newlines and tabs become spaces, every
/// other control and format character goes, and the ends are trimmed, so a
/// zero-width or bidirectional character cannot make the screen say one
/// thing and the file another.
pub(crate) fn clean_line(line: &str) -> String {
    let cleaned: String = line
        .chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() || is_format(c) => None,
            c => Some(c),
        })
        .collect();
    cleaned.trim().to_string()
}

/// A line split into its text and its ending (`\n`, `\r\n` or none).
fn split_ending(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

/// A bullet's text: the line less its ending and `- `, trimmed. `None` for
/// a line that is not a bullet.
fn bullet_text(line: &str) -> Option<&str> {
    split_ending(line).0.strip_prefix("- ").map(str::trim)
}

fn heading_topic(line: &str) -> Option<Topic> {
    let body = split_ending(line).0;
    Topic::ALL.into_iter().find(|topic| topic.heading() == body)
}

/// A heading and the run of bullet lines directly after it: the lines the
/// command owns. `heading` is the heading line's index; the bullets are the
/// lines after it up to `end`.
struct Block {
    topic: Topic,
    heading: usize,
    end: usize,
}

fn blocks(lines: &[&str]) -> Vec<Block> {
    let mut found: Vec<Block> = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if let Some(topic) = heading_topic(line) {
            found.push(Block {
                topic,
                heading: at,
                end: at + 1,
            });
        } else if bullet_text(line).is_some()
            && let Some(block) = found.last_mut().filter(|block| block.end == at)
        {
            block.end = at + 1;
        }
    }
    found
}

/// Each topic's bullets in the text, in topic order, for the topics that
/// have a block: bullet texts trimmed, empty ones and exact repeats left
/// out.
pub(crate) fn owned(text: &str) -> Vec<(Topic, Vec<String>)> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let blocks = blocks(&lines);
    let mut found = Vec::new();
    for topic in Topic::ALL {
        let mut own = blocks.iter().filter(|b| b.topic == topic).peekable();
        if own.peek().is_none() {
            continue;
        }
        let mut bullets: Vec<String> = Vec::new();
        for block in own {
            // The heading line is no bullet, so it drops out here.
            for text in lines
                .iter()
                .take(block.end)
                .skip(block.heading)
                .filter_map(|line| bullet_text(line))
            {
                if !text.is_empty() && !bullets.iter().any(|b| b == text) {
                    bullets.push(text.to_string());
                }
            }
        }
        found.push((topic, bullets));
    }
    found
}

/// One layer's text with the person's decisions applied. `struck` names
/// bullets to remove and `added` lines to write, each under its topic.
///
/// Only what a decision touches changes: every other byte, line endings
/// and spacing included, is copied as it is, so a run that changes nothing
/// returns `text` byte for byte. A struck text removes every bullet equal
/// to it in any block of its topic. A block that had a bullet and has none
/// left goes with its heading unless it receives a line. An added line goes
/// after the last line of the topic's first block, and a topic with no
/// block gets a new one at the end. An added line equal to an owned bullet
/// that stays is not written again.
pub(crate) fn merge(text: &str, struck: &[(Topic, String)], added: &[(Topic, String)]) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let blocks = blocks(&lines);
    let struck: Vec<(Topic, &str)> = struck
        .iter()
        .map(|(topic, line)| (*topic, line.trim()))
        .filter(|(_, line)| !line.is_empty())
        .collect();
    let is_struck = |topic: Topic, bullet: &str| {
        !bullet.is_empty() && struck.iter().any(|(t, s)| *t == topic && *s == bullet)
    };
    let bullets_of = |block: &Block| -> Vec<(usize, &str)> {
        lines
            .iter()
            .enumerate()
            .take(block.end)
            .skip(block.heading + 1)
            .map(|(at, line)| (at, bullet_text(line).unwrap_or_default()))
            .collect()
    };

    let mut fresh: Vec<(Topic, String)> = Vec::new();
    for (topic, line) in added {
        let line = clean_line(line);
        if line.is_empty() || fresh.iter().any(|(t, l)| t == topic && *l == line) {
            continue;
        }
        let covered = blocks
            .iter()
            .filter(|b| b.topic == *topic)
            .flat_map(&bullets_of)
            .any(|(_, bullet)| bullet == line && !is_struck(*topic, bullet));
        if !covered {
            fresh.push((*topic, line));
        }
    }

    let mut dropped: BTreeSet<usize> = BTreeSet::new();
    let mut inserts: BTreeMap<usize, (&str, Vec<&str>)> = BTreeMap::new();
    for topic in Topic::ALL {
        let new: Vec<&str> = fresh
            .iter()
            .filter(|(t, _)| *t == topic)
            .map(|(_, line)| line.as_str())
            .collect();
        for (n, block) in blocks.iter().filter(|b| b.topic == topic).enumerate() {
            let bullets = bullets_of(block);
            let mut last = block.heading;
            for (at, bullet) in &bullets {
                if is_struck(topic, bullet) {
                    dropped.insert(*at);
                } else {
                    last = *at;
                }
            }
            let kept = last > block.heading;
            let first = n == 0;
            if first && !new.is_empty() {
                let ending = match lines.get(block.heading) {
                    Some(heading) if split_ending(heading).1 == "\r\n" => "\r\n",
                    _ => "\n",
                };
                inserts.insert(last, (ending, new.clone()));
            } else if !bullets.is_empty() && !kept {
                dropped.insert(block.heading);
            }
        }
    }

    let mut out = String::with_capacity(text.len());
    for (at, line) in lines.iter().enumerate() {
        if dropped.contains(&at) {
            continue;
        }
        out.push_str(line);
        if let Some((ending, new)) = inserts.get(&at) {
            if !line.ends_with('\n') {
                out.push_str(ending);
            }
            for line in new {
                out.push_str("- ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    for topic in Topic::ALL {
        if blocks.iter().any(|b| b.topic == topic) {
            continue;
        }
        let new: Vec<&str> = fresh
            .iter()
            .filter(|(t, _)| *t == topic)
            .map(|(_, line)| line.as_str())
            .collect();
        if new.is_empty() {
            continue;
        }
        if !out.is_empty() {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(topic.heading());
        out.push('\n');
        for line in new {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// One row of a layer's diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiffRow {
    /// A line the save leaves as it is.
    Context(String),
    /// A line the save removes.
    Removed(String),
    /// A line the save adds.
    Added(String),
}

/// The topic each line of a text belongs to: a heading or a bullet of a
/// block. Prose belongs to none.
fn line_topics(lines: &[&str]) -> Vec<Option<Topic>> {
    let mut topics = vec![None; lines.len()];
    for block in blocks(lines) {
        for slot in topics.iter_mut().take(block.end).skip(block.heading) {
            *slot = Some(block.topic);
        }
    }
    topics
}

/// The rows `make` builds from the lines of `range`, each with its topic
/// and without its line ending.
fn lines_of<'a>(
    lines: &'a [&str],
    topics: &'a [Option<Topic>],
    range: Range<usize>,
    make: fn(String) -> DiffRow,
) -> impl Iterator<Item = (Option<Topic>, DiffRow)> + 'a {
    range.filter_map(move |at| {
        let line = lines.get(at)?;
        Some((
            topics.get(at).copied().flatten(),
            make(split_ending(line).0.to_string()),
        ))
    })
}

/// The rows the final screen draws for one layer: the line diff of the text
/// read against the text saved, limited to the changed topics. Each changed
/// topic shows its heading as saved and its kept bullets as context, struck
/// bullets removed and new ones added; a heading that is new is itself an
/// added line. A topic with no change and every line of prose is left out,
/// so an unchanged layer has no rows.
pub(crate) fn diff(before: &str, after: &str) -> Vec<DiffRow> {
    if before == after {
        return Vec::new();
    }
    let before: Vec<&str> = before.split_inclusive('\n').collect();
    let after: Vec<&str> = after.split_inclusive('\n').collect();
    let (before_topics, after_topics) = (line_topics(&before), line_topics(&after));

    let mut rows: Vec<(Option<Topic>, DiffRow)> = Vec::new();
    for op in capture_diff_slices(Algorithm::Myers, &before, &after) {
        let (tag, old, new) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            rows.extend(lines_of(&after, &after_topics, new, DiffRow::Context));
        } else {
            rows.extend(lines_of(&before, &before_topics, old, DiffRow::Removed));
            rows.extend(lines_of(&after, &after_topics, new, DiffRow::Added));
        }
    }
    let changed: Vec<Topic> = rows
        .iter()
        .filter(|(_, row)| !matches!(row, DiffRow::Context(_)))
        .filter_map(|(topic, _)| *topic)
        .collect();
    rows.into_iter()
        .filter(|(topic, _)| topic.is_some_and(|t| changed.contains(&t)))
        .map(|(_, row)| row)
        .collect()
}

#[cfg(test)]
#[path = "template_tests.rs"]
mod tests;
