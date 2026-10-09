//! Classifies a shell command (`docs/tools.md`, "Shell", "Effects").

use std::path::{Component, Path, PathBuf};

use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::Effects;

use super::read_only::{COMMANDS, Command, EXACT};

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
        always_reviewed: false,
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

/// A second word offered in the prefix: no leading `-`, and no `/`, `.`,
/// `=` or `>` (`docs/tools.md`, "Shell", "Effects").
fn plain_word(raw: &str) -> bool {
    !raw.is_empty()
        && !raw.starts_with('-')
        && !raw.contains(['\'', '"', '/', '.', '=', '>'])
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
    let Some(entry) = lookup(first, &mut words) else {
        return Outcome::Executes;
    };
    let mut operands = Vec::new();
    let mut ended = false;
    let mut pattern_given = false;
    while let Some(word) = words.next() {
        match word_at(word, ended, entry) {
            WordAt::Operand(cooked) => operands.push(cooked),
            WordAt::EndFlags => ended = true,
            WordAt::Flag => {
                if accept_flag(entry, &mut words, &word.cooked).is_none() {
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
    let mut paths = Vec::new();
    for operand in &operands {
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

enum WordAt {
    Operand(String),
    EndFlags,
    Flag,
}

fn word_at(word: &Word, ended: bool, entry: &Command) -> WordAt {
    if ended || !word.cooked.starts_with('-') {
        WordAt::Operand(word.cooked.clone())
    } else if entry.ends_flags && word.cooked == "--" {
        WordAt::EndFlags
    } else if entry.prints && !is_option_word(&word.cooked) {
        // bash's `echo` reads any other word as text.
        WordAt::Operand(word.cooked.clone())
    } else {
        WordAt::Flag
    }
}

/// `-` followed by one or more of `n`, `e`, `E`: the only words bash's
/// `echo` reads as options.
fn is_option_word(cooked: &str) -> bool {
    let Some(rest) = cooked.strip_prefix('-') else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|ch| matches!(ch, 'n' | 'e' | 'E'))
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

fn accept_flag(
    entry: &Command,
    words: &mut std::slice::Iter<'_, Word>,
    cooked: &str,
) -> Option<()> {
    if let Some(name) = long_value_name(cooked) {
        return entry
            .flags
            .iter()
            .any(|flag| flag.takes_value && flag.spelling == name)
            .then_some(());
    }
    if let Some(flag) = entry.flags.iter().find(|flag| flag.spelling == cooked) {
        if flag.takes_value {
            words.next()?;
        }
        return Some(());
    }
    // `git log -<digits>`, as in `git log -8`.
    if entry.counts && is_count(cooked) {
        return Some(());
    }
    short_cluster(entry, cooked).then_some(())
}

/// `--name=value`, and only that form. A short flag does not take `=`.
fn long_value_name(cooked: &str) -> Option<&str> {
    let (name, _) = cooked.split_once('=')?;
    name.starts_with("--").then_some(name)
}

/// `-<one or more ASCII digits>`, as in `git log -8`.
fn is_count(cooked: &str) -> bool {
    let Some(rest) = cooked.strip_prefix('-') else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit())
}

fn cooked_equals(words: &[Word], row: &[&str]) -> bool {
    words.len() == row.len()
        && words
            .iter()
            .zip(row.iter())
            .all(|(word, want)| word.cooked == *want)
}

fn short_cluster(entry: &Command, cooked: &str) -> bool {
    let Some(rest) = cooked.strip_prefix('-') else {
        return false;
    };
    !rest.is_empty()
        && rest.chars().all(|ch| {
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
