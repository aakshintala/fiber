//! Classifies a shell command (`docs/tools.md`, "Shell", "Effects").

use std::path::Path;

use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::Effects;

use super::read_only::{COMMANDS, Command};

/// Effects of `command` run in `workdir`. A command this classifier cannot
/// read plainly, or any part not on the read-only list, is `executes` with
/// no paths. One plain part is the subject.
pub(super) fn classify(command: &str, workdir: &Path) -> Effects {
    let Some(parts) = Lexer::new(command).run() else {
        return executes(None, None);
    };
    let (subject, prefix) = subject_and_prefix(&parts);
    match outcome(&parts, workdir) {
        Outcome::Reads(paths) => reads(paths, subject, prefix),
        Outcome::Executes => executes(subject, prefix),
    }
}

fn reads(paths: Vec<String>, subject: Option<String>, prefix: Option<String>) -> Effects {
    declared(
        Effect::Reads,
        true,
        if paths.is_empty() { None } else { Some(paths) },
        subject,
        prefix,
    )
}

/// Declares `executes`, not reversible, with no paths.
pub(super) fn executes(subject: Option<String>, prefix: Option<String>) -> Effects {
    declared(Effect::Executes, false, None, subject, prefix)
}

fn declared(
    effect: Effect,
    reversible: bool,
    paths: Option<Vec<String>>,
    subject: Option<String>,
    prefix: Option<String>,
) -> Effects {
    Effects {
        declared: DeclaredEffects {
            effects: vec![effect],
            reversible,
            paths,
        },
        subject,
        prefix,
    }
}

/// The subject and prefix of a one-part plain command. Anything else has
/// neither, so no allow rule can match it.
fn subject_and_prefix(parts: &[Part]) -> (Option<String>, Option<String>) {
    let Some(part) = parts.first() else {
        return (None, None);
    };
    if parts.len() != 1 || !plain_part(part) {
        return (None, None);
    }
    let subject = part
        .words
        .iter()
        .map(|word| word.raw.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    (Some(subject), prefix_of(part))
}

fn plain_part(part: &Part) -> bool {
    part.words
        .first()
        .is_some_and(|word| !word.cooked.contains('='))
}

fn prefix_of(part: &Part) -> Option<String> {
    let first = part.words.first()?;
    if first.raw.is_empty() || first.raw.contains(['\'', '"']) {
        return None;
    }
    let Some(second) = part.words.get(1) else {
        return Some(first.raw.clone());
    };
    if plain_word(&second.raw) {
        Some(format!("{} {}", first.raw, second.raw))
    } else {
        Some(first.raw.clone())
    }
}

/// A second word offered in the prefix: no leading `-`, and no `/`, `.` or
/// `=` (`docs/tools.md`, "Shell", "Effects").
fn plain_word(raw: &str) -> bool {
    !raw.is_empty() && !raw.starts_with('-') && !raw.contains(['\'', '"', '/', '.', '='])
}

enum Outcome {
    Executes,
    Reads(Vec<String>),
}

fn outcome(parts: &[Part], workdir: &Path) -> Outcome {
    let mut saw_executes = false;
    let mut paths = Vec::new();
    for part in parts {
        match part_outcome(part, workdir) {
            Outcome::Executes => saw_executes = true,
            Outcome::Reads(more) => paths.extend(more),
        }
    }
    if saw_executes {
        Outcome::Executes
    } else {
        Outcome::Reads(paths)
    }
}

fn part_outcome(part: &Part, workdir: &Path) -> Outcome {
    let Some(first) = part.words.first() else {
        return Outcome::Executes;
    };
    if first.cooked.contains('=') {
        return Outcome::Executes;
    }
    let Some((entry, start)) = lookup(&part.words) else {
        return Outcome::Executes;
    };
    let mut paths = Vec::new();
    let mut index = start;
    let mut ended = false;
    let mut saw_operand = false;
    while let Some(word) = part.words.get(index) {
        match word_at(word, ended, entry.ends_flags) {
            WordAt::Operand(cooked) => {
                if entry.paths {
                    let Some(path) = resolve(workdir, &cooked) else {
                        return Outcome::Executes;
                    };
                    paths.push(path);
                }
                saw_operand = true;
                index += 1;
            }
            WordAt::EndFlags => {
                ended = true;
                index += 1;
            }
            WordAt::Flag => {
                let Some(next) = accept_flag(entry, &part.words, index) else {
                    return Outcome::Executes;
                };
                index = next;
            }
        }
    }
    if entry.paths && !saw_operand {
        let Some(path) = workdir.to_str().map(str::to_owned) else {
            return Outcome::Executes;
        };
        paths.push(path);
    }
    Outcome::Reads(paths)
}

enum WordAt {
    Operand(String),
    EndFlags,
    Flag,
}

fn word_at(word: &Word, ended: bool, ends_flags: bool) -> WordAt {
    if ended || !word.cooked.starts_with('-') {
        WordAt::Operand(word.cooked.clone())
    } else if ends_flags && word.cooked == "--" {
        WordAt::EndFlags
    } else {
        WordAt::Flag
    }
}

fn lookup(words: &[Word]) -> Option<(&'static Command, usize)> {
    let first = words.first()?;
    if first.cooked == "git" {
        let second = words.get(1)?;
        if second.cooked.is_empty() || second.cooked.starts_with('-') {
            return None;
        }
        let entry = COMMANDS.iter().find(|entry| {
            entry
                .name
                .strip_prefix("git ")
                .is_some_and(|sub| sub == second.cooked)
        })?;
        return Some((entry, 2));
    }
    let entry = COMMANDS.iter().find(|entry| entry.name == first.cooked)?;
    Some((entry, 1))
}

fn accept_flag(entry: &Command, words: &[Word], index: usize) -> Option<usize> {
    let cooked = &words.get(index)?.cooked;
    if let Some(name) = long_value_name(cooked) {
        return entry
            .flags
            .iter()
            .any(|flag| flag.takes_value && flag.spelling == name)
            .then_some(index + 1);
    }
    if let Some(flag) = entry.flags.iter().find(|flag| flag.spelling == cooked) {
        if flag.takes_value {
            words.get(index + 1)?;
            return Some(index + 2);
        }
        return Some(index + 1);
    }
    if short_cluster(entry, cooked) {
        return Some(index + 1);
    }
    None
}

/// `--name=value`, and only that form. A short flag does not take `=`.
fn long_value_name(cooked: &str) -> Option<&str> {
    let (name, _) = cooked.split_once('=')?;
    if name.starts_with("--") && name.len() > 2 {
        Some(name)
    } else {
        None
    }
}

fn short_cluster(entry: &Command, cooked: &str) -> bool {
    let Some(rest) = cooked.strip_prefix('-') else {
        return false;
    };
    if rest.starts_with('-') || rest.chars().nth(1).is_none() {
        return false;
    }
    rest.chars().all(|ch| {
        entry
            .flags
            .iter()
            .any(|flag| short_letter(flag) == Some(ch))
    })
}

fn short_letter(flag: &super::read_only::Flag) -> Option<char> {
    if flag.takes_value {
        return None;
    }
    let rest = flag.spelling.strip_prefix('-')?;
    if rest.starts_with('-') {
        return None;
    }
    let mut chars = rest.chars();
    let letter = chars.next()?;
    chars.next().is_none().then_some(letter)
}

fn resolve(workdir: &Path, operand: &str) -> Option<String> {
    if Path::new(operand).is_absolute() {
        Some(operand.to_owned())
    } else {
        workdir.join(operand).to_str().map(str::to_owned)
    }
}

/// One word as written, and the same word with quotes removed.
#[derive(Debug, PartialEq, Eq)]
struct Word {
    raw: String,
    cooked: String,
}

/// One command between `&&`, `||`, `;` or `|`.
#[derive(Debug, PartialEq, Eq)]
struct Part {
    words: Vec<Word>,
}

struct Lexer {
    chars: Vec<char>,
    index: usize,
    parts: Vec<Part>,
    words: Vec<Word>,
    raw: String,
    cooked: String,
    in_word: bool,
    quote: Option<char>,
}

impl Lexer {
    fn new(command: &str) -> Self {
        Self {
            chars: command.chars().collect(),
            index: 0,
            parts: Vec::new(),
            words: Vec::new(),
            raw: String::new(),
            cooked: String::new(),
            in_word: false,
            quote: None,
        }
    }

    /// Splits the command into parts and words. `None` when the command is
    /// not plain: an empty part, an operator this classifier does not read
    /// (`;;`, `|&`, a lone `&`), an unterminated quote, or a character
    /// outside the plain set.
    fn run(&mut self) -> Option<Vec<Part>> {
        while let Some(ch) = self.chars.get(self.index).copied() {
            self.step(ch)?;
        }
        if self.quote.is_some() || !self.finish_part() {
            return None;
        }
        Some(std::mem::take(&mut self.parts))
    }

    /// `None` is a syntax this classifier refuses.
    fn step(&mut self, ch: char) -> Option<()> {
        let Some(closer) = self.quote else {
            return self.unquoted(ch);
        };
        // `$`, backticks and backslash expand inside double quotes, so
        // the command names something this classifier cannot see.
        if closer == '"' && matches!(ch, '$' | '`' | '\\') {
            return None;
        }
        self.quoted(ch, closer)
    }

    fn quoted(&mut self, ch: char, closer: char) -> Option<()> {
        self.raw.push(ch);
        if ch == closer {
            self.quote = None;
        } else {
            self.cooked.push(ch);
        }
        self.index += 1;
        Some(())
    }

    fn unquoted(&mut self, ch: char) -> Option<()> {
        if ch == ' ' || ch == '\t' {
            self.finish_word();
            self.index += 1;
            return Some(());
        }
        if ch == '\'' || ch == '"' {
            self.in_word = true;
            self.raw.push(ch);
            self.quote = Some(ch);
            self.index += 1;
            return Some(());
        }
        match self.operator() {
            Operator::Split(next) => {
                if !self.finish_part() {
                    return None;
                }
                self.index = next;
                Some(())
            }
            Operator::Unreadable => None,
            Operator::None => {
                // Globs, `~`, redirects and every other character outside
                // the plain set expand or mean something this list cannot see.
                if !plain_char(ch) {
                    return None;
                }
                self.in_word = true;
                self.raw.push(ch);
                self.cooked.push(ch);
                self.index += 1;
                Some(())
            }
        }
    }

    fn operator(&self) -> Operator {
        let next = self.chars.get(self.index + 1).copied();
        match self.chars.get(self.index).copied() {
            Some('|') => match next {
                Some('|') => Operator::Split(self.index + 2),
                Some('&') => Operator::Unreadable,
                _ => Operator::Split(self.index + 1),
            },
            Some('&') => match next {
                Some('&') => Operator::Split(self.index + 2),
                _ => Operator::Unreadable,
            },
            Some(';') => match next {
                Some(';') => Operator::Unreadable,
                _ => Operator::Split(self.index + 1),
            },
            _ => Operator::None,
        }
    }

    fn finish_word(&mut self) {
        if !self.in_word {
            return;
        }
        self.words.push(Word {
            raw: std::mem::take(&mut self.raw),
            cooked: std::mem::take(&mut self.cooked),
        });
        self.in_word = false;
    }

    fn finish_part(&mut self) -> bool {
        self.finish_word();
        if self.words.is_empty() {
            return false;
        }
        self.parts.push(Part {
            words: std::mem::take(&mut self.words),
        });
        true
    }
}

enum Operator {
    Split(usize),
    Unreadable,
    None,
}

fn plain_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '_' | '-' | '.' | '/' | ':' | '=' | '@' | '%' | '+' | ','
        )
}

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
