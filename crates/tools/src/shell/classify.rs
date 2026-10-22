//! Classifies a shell command (`docs/tools.md`, "Shell", "Effects").

use std::path::{Component, Path, PathBuf};

use contract::shapes::Effect;
use contract::tool::Effects;

use super::entry;
use super::read_only::{COMMANDS, Command, EXACT};
use super::syntax::Word;

/// Effects of `command` run in `workdir`. A command this classifier cannot
/// read plainly, or any part not on the read-only list, is `executes` with
/// no paths. One plain part is the subject. `configured` holds the person's
/// `shell.read_only` entries beside the built-in list: a part the built-in
/// list does not answer falls back to them.
pub(super) fn classify(
    command: &str,
    workdir: &Path,
    configured: &[(String, Vec<String>)],
) -> Effects {
    let Some(parts) = Lexer::new(command).run() else {
        return executes(None, None);
    };
    let (subject, prefix) = subject_and_prefix(&parts);
    match outcome(&parts, workdir, configured) {
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
    crate::tool_util::effects(vec![effect], reversible, paths, subject, prefix)
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

/// A second word offered in the prefix: no leading `-`, and no `/`, `.`,
/// `=` or `>` (`docs/tools.md`, "Shell", "Effects").
fn plain_word(raw: &str) -> bool {
    !raw.is_empty() && !raw.starts_with('-') && !raw.contains(['\'', '"', '/', '.', '=', '>'])
}

pub(super) enum Outcome {
    Executes,
    Reads(Vec<String>),
}

fn outcome(parts: &[Part], workdir: &Path, configured: &[(String, Vec<String>)]) -> Outcome {
    let mut saw_executes = false;
    let mut paths = Vec::new();
    for part in parts {
        match part_outcome(part, workdir, configured) {
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

fn part_outcome(part: &Part, workdir: &Path, configured: &[(String, Vec<String>)]) -> Outcome {
    // A redirect applies wherever it stands, so it is dropped before the
    // read-only check and kept only in the subject.
    let kept: Vec<Word> = part
        .words
        .iter()
        .filter(|word| !is_redirect_word(word))
        .map(|word| Word {
            raw: word.raw.clone(),
            cooked: word.cooked.clone(),
        })
        .collect();
    let mut words = kept.iter();
    let Some(first) = words.next() else {
        return Outcome::Executes;
    };
    if first.cooked.contains('=') {
        return Outcome::Executes;
    }
    // An exact form reads with no paths: only `git branch --show-current`.
    if EXACT.iter().any(|row| cooked_equals(&kept, row)) {
        return Outcome::Reads(Vec::new());
    }
    // `sed` has its own print-only grammar, not a table row.
    if first.cooked == "sed" {
        let words = kept.iter().skip(1).map(|word| word.cooked.as_str());
        let Some(files) = super::sed::files(words) else {
            return Outcome::Executes;
        };
        return path_tail(&files, workdir);
    }
    let Some(entry) = lookup(first, &mut words) else {
        // A name the built-in list owns never reaches configuration: the
        // link-following and write flags the built-in rule refuses stay
        // refused, and configuration cannot widen them.
        if builtin_name(&first.cooked) {
            return Outcome::Executes;
        }
        let flags = configured
            .iter()
            .find(|(name, _)| *name == first.cooked)
            .map(|(_, flags)| flags);
        let Some(flags) = flags else {
            return Outcome::Executes;
        };
        return entry::configured_outcome(flags, words, workdir);
    };
    let mut operands = Vec::new();
    let mut ended = false;
    let mut pattern_given = false;
    while let Some(word) = words.next() {
        match entry::word_at(word, ended, entry) {
            entry::WordAt::Operand(cooked) => operands.push(cooked),
            entry::WordAt::EndFlags => ended = true,
            entry::WordAt::Flag => {
                if entry::accept_flag(entry, &mut words, &word.cooked).is_none() {
                    return Outcome::Executes;
                }
                pattern_given |= entry.pattern_flags.contains(&word.cooked.as_str());
            }
        }
    }
    if !entry.paths {
        return Outcome::Reads(Vec::new());
    }
    // `grep` and `rg` read the first operand as the pattern unless a flag
    // such as `-e` gives it, wherever that flag stands.
    if entry.pattern && !pattern_given && !operands.is_empty() {
        operands.remove(0);
    }
    path_tail(&operands, workdir)
}

/// Resolves each operand against `workdir`, declaring the workdir when
/// there are none. A path under `/proc/` is not read-only.
pub(super) fn path_tail(operands: &[String], workdir: &Path) -> Outcome {
    let mut paths = Vec::new();
    for operand in operands {
        let Some(path) = resolve(workdir, operand) else {
            return Outcome::Executes;
        };
        paths.push(path);
    }
    if paths.is_empty() {
        let Some(path) = workdir.to_str().map(str::to_owned) else {
            return Outcome::Executes;
        };
        paths.push(path);
    }
    if paths.iter().any(|path| under_proc(path)) {
        return Outcome::Executes;
    }
    Outcome::Reads(paths)
}

/// Whether `path`, with `.` and `..` taken as written, is `/proc` or under
/// it, where `/proc/<pid>/environ` holds every variable a process was given.
fn under_proc(path: &str) -> bool {
    let mut normal = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            kept @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                normal.push(kept);
            }
        }
    }
    normal.starts_with("/proc")
}

/// Whether `first`, the cooked first word, names a built-in command: the
/// text after its last `/`, against the built-in command names and `sed`,
/// which has its own grammar. The whole word is used, so a quoted path
/// holding a space still ends at its basename. A configured entry under
/// such a name is never consulted.
fn builtin_name(first: &str) -> bool {
    let base = first.rsplit('/').next().unwrap_or(first);
    base == "sed"
        || COMMANDS
            .iter()
            .any(|entry| entry.name.split(' ').next().unwrap_or(entry.name) == base)
}

fn lookup(first: &Word, words: &mut std::slice::Iter<'_, Word>) -> Option<&'static Command> {
    if first.cooked == "git" {
        let second = words.next()?;
        return COMMANDS.iter().find(|entry| {
            entry
                .name
                .strip_prefix("git ")
                .is_some_and(|sub| sub == second.cooked)
        });
    }
    COMMANDS.iter().find(|entry| entry.name == first.cooked)
}

fn cooked_equals(words: &[Word], row: &[&str]) -> bool {
    words.len() == row.len()
        && words
            .iter()
            .zip(row.iter())
            .all(|(word, want)| word.cooked == *want)
}

/// A word the lexer accepted as a stderr redirect. Raw equality is
/// enough: `>` is refused everywhere else, so only an accepted redirect
/// has this raw text, and a quoted `'2>&1'` keeps its quotes in raw.
fn is_redirect_word(word: &Word) -> bool {
    word.raw == "2>/dev/null" || word.raw == "2>&1"
}

fn resolve(workdir: &Path, operand: &str) -> Option<String> {
    if Path::new(operand).is_absolute() {
        Some(operand.to_owned())
    } else {
        workdir.join(operand).to_str().map(str::to_owned)
    }
}

/// One command between `&&`, `||`, `;` or `|`.
#[derive(Debug, PartialEq, Eq)]
struct Part {
    words: Vec<Word>,
}

pub(super) fn one_grep(command: &str) -> bool {
    let Some(parts) = Lexer::new(command).run() else {
        return false;
    };
    let [part] = parts.as_slice() else {
        return false;
    };
    part.words.first().is_some_and(|word| word.cooked == "grep")
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    parts: Vec<Part>,
    words: Vec<Word>,
    raw: String,
    cooked: String,
    in_word: bool,
    quote: Option<char>,
}

impl<'a> Lexer<'a> {
    fn new(command: &'a str) -> Self {
        Self {
            chars: command.chars().peekable(),
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
        while let Some(ch) = self.chars.next() {
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
        // Inside double quotes a backslash before `$`, a backtick, `"`,
        // `\` or a newline still expands, so the command is not plain.
        // Before any other character it is literal, as bash reads it.
        if closer == '"' && ch == '\\' {
            let special = matches!(self.chars.peek(), Some('$' | '`' | '"' | '\\' | '\n'));
            if special || self.chars.peek().is_none() {
                return None;
            }
            self.raw.push(ch);
            self.cooked.push(ch);
            return Some(());
        }
        if closer == '"' && matches!(ch, '$' | '`') {
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
        Some(())
    }

    fn unquoted(&mut self, ch: char) -> Option<()> {
        if ch == ' ' || ch == '\t' {
            self.finish_word();
            return Some(());
        }
        if ch == '\'' || ch == '"' {
            self.in_word = true;
            self.raw.push(ch);
            self.quote = Some(ch);
            return Some(());
        }
        // `>` starts only the two stderr redirects, read as one word.
        if ch == '>' {
            return self.redirect();
        }
        match self.operator(ch) {
            Operator::Split => self.finish_part().then_some(()),
            Operator::None => {
                // Globs, `~`, redirects and every other character outside
                // the plain set expand or mean something this list cannot see.
                if !plain_char(ch) {
                    return None;
                }
                self.in_word = true;
                self.raw.push(ch);
                self.cooked.push(ch);
                Some(())
            }
        }
    }

    /// Reads `>/dev/null` or `>&1` after a plain `2` as one redirect word.
    /// Anything else holding `>` is not plain, so the call is reviewed.
    fn redirect(&mut self) -> Option<()> {
        // The word so far is exactly `2`, and the part already holds a
        // word, so the redirect is never first in a part.
        if !self.in_word || self.raw != "2" || self.words.is_empty() {
            return None;
        }
        let tail = if self.ahead("/dev/null") {
            "/dev/null"
        } else if self.ahead("&1") {
            "&1"
        } else {
            return None;
        };
        for _ in 0..tail.len() {
            self.chars.next();
        }
        self.raw.push('>');
        self.cooked.push('>');
        for ch in tail.chars() {
            self.raw.push(ch);
            self.cooked.push(ch);
        }
        // The redirect is a word of its own: what follows ends the word.
        match self.chars.peek() {
            None | Some(' ') | Some('\t') | Some('|') | Some('&') | Some(';') => Some(()),
            _ => None,
        }
    }

    fn ahead(&self, want: &str) -> bool {
        let mut rest = self.chars.clone();
        for ch in want.chars() {
            if rest.next() != Some(ch) {
                return false;
            }
        }
        true
    }

    /// A lone `&` is not an operator: the plain-character check refuses it.
    /// `;;` is two splits, so the second leaves an empty part.
    fn operator(&mut self, ch: char) -> Operator {
        match ch {
            '|' => {
                if self.chars.peek() == Some(&'|') {
                    self.chars.next();
                }
                Operator::Split
            }
            '&' if self.chars.peek() == Some(&'&') => {
                self.chars.next();
                Operator::Split
            }
            ';' => Operator::Split,
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
    Split,
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
