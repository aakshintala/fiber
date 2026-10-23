//! Classifies a shell command (`docs/tools.md`, "Shell", "Effects").
//!
//! Each verdict comes from one of three places. The tree-sitter parse
//! (`super::syntax`) splits the call into elements, stages, words and
//! redirects. The entry lookup (`super::entry`) names the command path and
//! reads its flags. The flag walk, the `sed` file grammar, the exact forms,
//! the path tail and the subject line below are the old hand-lexer rules,
//! kept word for word and cited where each stands.

use std::path::{Component, Path, PathBuf};

use contract::shapes::Effect;
use contract::tool::Effects;

use super::cwd::Cwd;
use super::entry::{self, Found};
use super::read_only::{Command, EXACT};
use super::syntax::{Redirect, Script, Simple, Stage, Word};
use super::wrap::{self, Unwrapped};

/// Effects of `command` run in `workdir`. A command the tree-sitter parse
/// rejects, or any element not on the read-only list, is `executes` with no
/// paths. The single simple stage's raw tokens are the subject.
/// `configured` holds the person's `shell.read_only` entries beside the
/// built-in list: an element no built-in path owns falls back to them, and
/// a built-in path answers first so configuration cannot widen it.
pub(super) fn classify(
    command: &str,
    workdir: &Path,
    configured: &[(String, Vec<String>)],
) -> Effects {
    let Some(script) = super::syntax::parse(command) else {
        return executes(None, None);
    };
    let (subject, prefix) = subject_and_prefix(&script);
    match outcome(&script, workdir, configured) {
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

/// The subject reviewers see and the prefix allow rules match: the single
/// simple stage's raw tokens joined, then its first one or two words. A
/// pipeline, an opaque stage, a leading redirect or a leading assignment
/// names nothing: the assignments never show, so no allow rule can match
/// an environment-modified call, as the hand lexer showed raw words.
fn subject_and_prefix(script: &Script) -> (Option<String>, Option<String>) {
    let [element] = script.elements.as_slice() else {
        return (None, None);
    };
    let [Stage::Simple(simple)] = element.stages.as_slice() else {
        return (None, None);
    };
    if !simple.assignments.is_empty() {
        return (None, None);
    }
    let tokens = &simple.tokens;
    let Some(first) = tokens.first() else {
        return (None, None);
    };
    if !simple.words.first().is_some_and(|word| word.raw == *first) {
        return (None, None);
    }
    let prefix = prefix_of(tokens);
    (Some(tokens.join(" ")), prefix)
}

fn prefix_of(tokens: &[String]) -> Option<String> {
    let first = tokens.first()?;
    if first.is_empty() || first.contains(['\'', '"']) {
        return None;
    }
    let Some(second) = tokens.get(1) else {
        return Some(first.clone());
    };
    if plain_word(second) {
        Some(format!("{first} {second}"))
    } else {
        Some(first.clone())
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

fn outcome(script: &Script, workdir: &Path, configured: &[(String, Vec<String>)]) -> Outcome {
    let Some(start) = workdir.to_str() else {
        return Outcome::Executes;
    };
    let mut cwd = Cwd::new(start);
    let mut saw_cd = false;
    let mut saw_executes = false;
    let mut lost = false;
    let mut paths = Vec::new();
    for element in &script.elements {
        if lost {
            saw_executes = true;
            continue;
        }
        let Some(dirs) = cwd.enter(element.join) else {
            lost = true;
            saw_executes = true;
            continue;
        };
        let mut cd_target = None;
        if let [Stage::Simple(simple)] = element.stages.as_slice()
            && is_cd(simple)
        {
            cd_target = cd_operand(simple);
        }
        for stage in &element.stages {
            if let Stage::Simple(simple) = stage
                && is_cd(simple)
            {
                saw_cd = true;
            }
            match stage_outcome(stage, &dirs, configured, saw_cd) {
                Outcome::Executes => saw_executes = true,
                Outcome::Reads(mut more) => paths.append(&mut more),
            }
        }
        cwd.leave(element.join, cd_target);
    }
    if saw_executes {
        Outcome::Executes
    } else {
        Outcome::Reads(paths)
    }
}

fn stage_outcome(
    stage: &Stage,
    dirs: &[String],
    configured: &[(String, Vec<String>)],
    saw_cd: bool,
) -> Outcome {
    let Stage::Simple(simple) = stage else {
        // An unlisted or refused stage: a loop body, a heredoc with a body,
        // a substitution, an unlisted operator, or a redirect outside the
        // allow-list.
        return Outcome::Executes;
    };
    // An assignment prefix, `env`, `timeout` and `xargs` unwrap to the
    // inner command, which the same verdict classifies below; anything
    // the wrappers refuse is `executes`.
    let mut words: &[Word] = &simple.words;
    let mut wrapped = false;
    if !simple.assignments.is_empty() {
        // A leading `NAME=value` prefix reads only for names on the fixed
        // list; any other name makes the call `executes`.
        if !wrap::assignments_allowed(&simple.assignments) {
            return Outcome::Executes;
        }
        wrapped = true;
    }
    let mut depth = 0;
    loop {
        match wrap::unwrap(words) {
            Unwrapped::Plain => break,
            Unwrapped::Refused => return Outcome::Executes,
            Unwrapped::Inner(inner) => {
                depth += 1;
                if depth > wrap::MAX_WRAPPER_DEPTH {
                    return Outcome::Executes;
                }
                words = inner;
                wrapped = true;
            }
            Unwrapped::Xargs(inner) => {
                depth += 1;
                if depth > wrap::MAX_WRAPPER_DEPTH {
                    return Outcome::Executes;
                }
                return xargs_outcome(inner, simple, dirs);
            }
        }
    }
    // A wrapper around `cd` moves nothing the directory states track:
    // the join sees only a leading `cd`, so the call is reviewed. A bare
    // `cd` is a read-only step and declares its directory.
    if words.first().is_some_and(|word| word.cooked == "cd") {
        if wrapped {
            return Outcome::Executes;
        }
        return cd_outcome(simple, dirs);
    }
    // A `git` after any earlier `cd` is reviewed, whatever wrapper leads
    // it: the new directory can hold hooks or configuration the person
    // never saw.
    if saw_cd && is_git(words) {
        return Outcome::Executes;
    }
    // An exact form reads with no paths: `git status`, `git stash list`
    // or `git branch --show-current`, whatever wrapper leads it.
    if EXACT.iter().any(|row| cooked_equals(words, row)) {
        return with_inputs(Outcome::Reads(Vec::new()), simple, dirs);
    }
    // `sed` has its own print-only grammar, not a table row.
    if words.first().is_some_and(|word| word.cooked == "sed") {
        let files = words.iter().skip(1).map(|word| word.cooked.as_str());
        let Some(files) = super::sed::files(files) else {
            return Outcome::Executes;
        };
        return with_inputs(path_tail(&files, dirs), simple, dirs);
    }
    let verdict = match entry::find(words, configured) {
        Found::Builtin(entry) => builtin_outcome(words, entry, dirs),
        Found::Configured(flags, consumed) => {
            entry::configured_outcome(flags, rest_after(words, consumed), dirs)
        }
        Found::None => Outcome::Executes,
    };
    with_inputs(verdict, simple, dirs)
}

/// An `xargs` stage: the target must name one of the six read-only
/// commands, classified by its own flags; anything else executes. Paths
/// come from the target's operands, with the stage's input redirects.
fn xargs_outcome(inner: &[Word], simple: &Simple, dirs: &[String]) -> Outcome {
    let Some(entry) = inner
        .first()
        .and_then(|target| wrap::xargs_entry(&target.cooked))
    else {
        return Outcome::Executes;
    };
    let mut rest = inner.iter();
    rest.next();
    with_inputs(builtin_tail(rest, entry, dirs), simple, dirs)
}

/// Whether a stage runs `cd`: its first word is exactly `cd`.
fn is_cd(simple: &Simple) -> bool {
    simple.words.first().is_some_and(|word| word.cooked == "cd")
}

/// Whether a stage's words run `git`: the first word's basename is `git`.
fn is_git(words: &[Word]) -> bool {
    words
        .first()
        .is_some_and(|word| word.cooked.rsplit('/').next() == Some("git"))
}

/// The `cd` target: `cd <dir>` or `cd -- <dir>` with exactly one
/// following word, or nothing when the shape differs. A flag-like operand
/// reads only after `--`: `cd -P` executes while `cd -- -P` names a
/// directory.
fn cd_operand(simple: &Simple) -> Option<&str> {
    let mut words = simple.words.iter();
    if !words.next().is_some_and(|word| word.cooked == "cd") {
        return None;
    }
    let first = words.next()?;
    let (operand, dashed) = if first.cooked == "--" {
        (words.next()?, true)
    } else {
        (first, false)
    };
    if words.next().is_some() {
        return None;
    }
    if !dashed && operand.cooked.starts_with('-') {
        return None;
    }
    Some(operand.cooked.as_str())
}

/// A `cd` stage: a plain `cd <dir>` or `cd -- <dir>` reads, declaring the
/// joined directory in every directory the element may run in. Any flag,
/// extra operand, `~` word or glob executes, as does a target under
/// `/proc`.
fn cd_outcome(simple: &Simple, dirs: &[String]) -> Outcome {
    let Some(target) = cd_operand(simple) else {
        return Outcome::Executes;
    };
    if target.is_empty()
        || target == "-"
        || target == "~"
        || target.starts_with("~/")
        || target
            .chars()
            .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
    {
        return Outcome::Executes;
    }
    let mut joined = Vec::new();
    for dir in dirs {
        joined.push(super::cwd::join_dir(dir, target));
    }
    with_inputs(Outcome::Reads(joined), simple, dirs)
}

/// The input redirect files, in order, beside a read-only verdict, in
/// every directory the element may run in: the shell opens each for the
/// command's stdin before it runs, so `cat <f` reads `f` and a `/proc`
/// file executes.
fn with_inputs(verdict: Outcome, simple: &Simple, dirs: &[String]) -> Outcome {
    let Outcome::Reads(mut paths) = verdict else {
        return Outcome::Executes;
    };
    let inputs = simple
        .redirects
        .iter()
        .filter_map(|redirect| match redirect {
            Redirect::Input(word) => Some(&word.cooked),
            Redirect::Discard | Redirect::Merge => None,
        });
    for input in inputs {
        for dir in dirs {
            let Some(path) = resolve(dir, input) else {
                return Outcome::Executes;
            };
            paths.push(path);
        }
    }
    if paths.iter().any(|path| under_proc(path)) {
        return Outcome::Executes;
    }
    Outcome::Reads(paths)
}

/// The words after the command path, for the flag walks: `git status -s`
/// walks from `-s`. The slice stays whole because the workspace forbids
/// indexing; the loop drops the path words instead.
fn rest_after(words: &[Word], skip: usize) -> std::slice::Iter<'_, Word> {
    let mut rest = words.iter();
    for _ in 0..skip {
        rest.next();
    }
    rest
}

/// The built-in flag walk, kept from the hand lexer: flags by the table,
/// `--` where the entry says so, any other `-` word an operand for `echo`,
/// the first operand the pattern for `grep` and `rg`. The entry's path
/// words never reach the walk: `git status -s` walks from `-s`.
fn builtin_outcome(words: &[Word], entry: &Command, dirs: &[String]) -> Outcome {
    builtin_tail(
        rest_after(words, entry.name.split(' ').count()),
        entry,
        dirs,
    )
}

/// The flag walk over the words past the command path.
fn builtin_tail(
    mut words: std::slice::Iter<'_, Word>,
    entry: &Command,
    dirs: &[String],
) -> Outcome {
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
    path_tail(&operands, dirs)
}

/// Resolves each operand against every directory the element may run in,
/// declaring each running directory when there are no operands. A path
/// under `/proc/` is not read-only.
pub(super) fn path_tail(operands: &[String], dirs: &[String]) -> Outcome {
    let mut paths = Vec::new();
    for dir in dirs {
        if operands.is_empty() {
            paths.push(dir.clone());
            continue;
        }
        for operand in operands {
            let Some(path) = resolve(dir, operand) else {
                return Outcome::Executes;
            };
            paths.push(path);
        }
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

fn cooked_equals(words: &[Word], row: &[&str]) -> bool {
    words.len() == row.len()
        && words
            .iter()
            .zip(row.iter())
            .all(|(word, want)| word.cooked == *want)
}

fn resolve(dir: &str, operand: &str) -> Option<String> {
    if Path::new(operand).is_absolute() {
        Some(operand.to_owned())
    } else {
        Path::new(dir).join(operand).to_str().map(str::to_owned)
    }
}

/// Whether `command` is one plain `grep` call, so the run can attach its
/// match lines. An assignment prefix or a redirect does not disqualify it;
/// a second element, a second stage, or any unparsed text does.
pub(super) fn one_grep(command: &str) -> bool {
    let Some(script) = super::syntax::parse(command) else {
        return false;
    };
    let [element] = script.elements.as_slice() else {
        return false;
    };
    let [Stage::Simple(simple)] = element.stages.as_slice() else {
        return false;
    };
    simple
        .words
        .first()
        .is_some_and(|word| word.cooked == "grep")
}

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
