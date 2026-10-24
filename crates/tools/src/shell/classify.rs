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
        Outcome::Network(paths) => network(paths, subject, prefix),
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

/// A call that reached the network: a `gh` stage read a remote, so the
/// call declares `Network` with the default host beside the paths it
/// read, in the shape `web_fetch` declares it.
fn network(paths: Vec<String>, subject: Option<String>, prefix: Option<String>) -> Effects {
    Effects {
        hosts: Some(vec!["https://api.github.com/".to_owned()]),
        ..declared(
            Effect::Network,
            true,
            if paths.is_empty() { None } else { Some(paths) },
            subject,
            prefix,
        )
    }
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

/// The stages the reviewer was not shown read-only, for the reviewer's
/// prompt (`docs/tools.md`, "Shell", "Effects"): the whole line, so the
/// reviewer names the call's other commands while still seeing all of it.
/// `None` when every stage reads, when none does, or when the parse leaves
/// no trustworthy parts.
/// The stages the reviewer was not shown read-only, for the reviewer's
/// prompt (`docs/tools.md`, "Shell", "Effects"): the whole line, so the
/// reviewer names the call's other commands while still seeing all of it.
/// `None` when every stage reads, when none does, or when the parse leaves
/// no trustworthy parts.
pub(super) fn not_shown_read_only(
    command: &str,
    workdir: &Path,
    configured: &[(String, Vec<String>)],
) -> Option<String> {
    let script = super::syntax::parse(command)?;
    let walked = walk(&script, workdir, configured);
    let shown = walked.parts.iter().any(|part| part.shown);
    let unshown: Vec<&str> = walked
        .parts
        .iter()
        .filter(|part| !part.shown)
        .map(|part| part.text.as_str())
        .collect();
    if !shown || unshown.is_empty() {
        return None;
    }
    let mut line = String::from("Not shown read-only: ");
    for (index, text) in unshown.iter().enumerate() {
        if index > 0 {
            line.push_str(", ");
        }
        line.push('`');
        line.push_str(text);
        line.push('`');
    }
    Some(line)
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
    Network(Vec<String>),
}

/// One stage's verdict beside its source slice: whether the reviewer was
/// shown it read-only.
struct Part {
    text: String,
    shown: bool,
}

/// One walk over the script: the call's verdict inputs and every stage's
/// own verdict. `not_shown_read_only` reads the parts; `outcome` folds
/// the verdict.
struct Walked {
    saw_executes: bool,
    saw_network: bool,
    paths: Vec<String>,
    parts: Vec<Part>,
}

/// The verbatim source slice of a stage: a simple command as written, or
/// an opaque statement's own slice.
fn stage_text(stage: &Stage) -> &str {
    match stage {
        Stage::Simple(simple) => simple.text.as_str(),
        Stage::Opaque(text) => text.as_str(),
    }
}

fn outcome(script: &Script, workdir: &Path, configured: &[(String, Vec<String>)]) -> Outcome {
    let walked = walk(script, workdir, configured);
    if walked.saw_executes {
        Outcome::Executes
    } else if walked.saw_network {
        Outcome::Network(walked.paths)
    } else {
        Outcome::Reads(walked.paths)
    }
}

fn walk(script: &Script, workdir: &Path, configured: &[(String, Vec<String>)]) -> Walked {
    let mut walked = Walked {
        saw_executes: false,
        saw_network: false,
        paths: Vec::new(),
        parts: Vec::new(),
    };
    let Some(start) = workdir.to_str() else {
        walked.saw_executes = true;
        return walked;
    };
    let mut cwd = Cwd::new(start);
    let mut saw_cd = false;
    let mut lost = false;
    for element in &script.elements {
        // A lost path leaves no trustworthy part behind: every later
        // stage is unshown, so the note can still name the call's tail.
        if lost {
            walked.saw_executes = true;
            for stage in &element.stages {
                walked.parts.push(Part {
                    text: stage_text(stage).to_owned(),
                    shown: false,
                });
            }
            continue;
        }
        let Some(dirs) = cwd.enter(element.join) else {
            lost = true;
            walked.saw_executes = true;
            for stage in &element.stages {
                walked.parts.push(Part {
                    text: stage_text(stage).to_owned(),
                    shown: false,
                });
            }
            continue;
        };
        let mut cd_target = None;
        if let [Stage::Simple(simple)] = element.stages.as_slice()
            && is_cd(simple)
        {
            cd_target = cd_operand(simple);
        }
        let mut stdin_words: Vec<String> = Vec::new();
        let mut stdin_known = false;
        let mut stdin_static = true;
        for stage in &element.stages {
            if let Stage::Simple(simple) = stage
                && is_cd(simple)
            {
                saw_cd = true;
            }
            // Filenames earlier stages of this element fed the pipeline:
            // the only stdin-fed paths an `xargs` stage may read. They are
            // proven only when at least one earlier stage ran and every
            // one printed statically-known words; any other producer
            // leaves stdin unproven and its `xargs` reader reviewed.
            let verdict = stage_outcome(
                stage,
                &dirs,
                configured,
                saw_cd,
                &stdin_words,
                stdin_known && stdin_static,
            );
            let network = matches!(verdict, Outcome::Network(_));
            let shown = !matches!(verdict, Outcome::Executes);
            walked.parts.push(Part {
                text: stage_text(stage).to_owned(),
                shown,
            });
            match verdict {
                Outcome::Executes => walked.saw_executes = true,
                Outcome::Reads(more) | Outcome::Network(more) => {
                    walked.saw_network = walked.saw_network || network;
                    stdin_known = true;
                    stdin_static = stdin_static && static_words(stage, &mut stdin_words);
                    walked.paths.extend(more);
                }
            }
        }
        cwd.leave(element.join, cd_target);
    }
    walked
}

fn stage_outcome(
    stage: &Stage,
    dirs: &[String],
    configured: &[(String, Vec<String>)],
    saw_cd: bool,
    stdin_words: &[String],
    stdin_proven: bool,
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
    let mut stepped = false;
    if !simple.assignments.is_empty() {
        // A leading `NAME=value` prefix reads only for names on the fixed
        // list; any other name makes the call `executes`.
        if !wrap::assignments_allowed(&simple.assignments) {
            return Outcome::Executes;
        }
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
                stepped = true;
            }
            Unwrapped::Xargs(inner) => {
                depth += 1;
                if depth > wrap::MAX_WRAPPER_DEPTH {
                    return Outcome::Executes;
                }
                return xargs_outcome(inner, simple, dirs, stdin_words, stdin_proven);
            }
        }
    }
    // A bare `cd`, or one with an allow-listed assignment prefix, is a
    // read-only step through the existing `cd` path: the prefix is not a
    // subprocess, so the join still tracks the directory for later
    // commands. An `env` or `timeout` wrapper around `cd` moves nothing
    // the directory states track, so the call is reviewed.
    if words.first().is_some_and(|word| word.cooked == "cd") {
        if stepped {
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
    // or `git branch --show-current`, whatever wrapper leads it, where
    // the repository guards pass.
    let verdict = if EXACT.iter().any(|row| cooked_equals(words, row)) {
        Outcome::Reads(Vec::new())
    } else if words.first().is_some_and(|word| word.cooked == "sed") {
        // `sed` has its own print-only grammar, not a table row.
        let files = words.iter().skip(1).map(|word| word.cooked.as_str());
        let Some(files) = super::sed::files(files) else {
            return Outcome::Executes;
        };
        path_tail(&files, dirs)
    } else {
        match entry::find(words, configured) {
            Found::Builtin(entry) => builtin_outcome(words, entry, dirs),
            Found::Configured(flags, consumed) => {
                entry::configured_outcome(flags, rest_after(words, consumed), dirs)
            }
            Found::None => Outcome::Executes,
        }
    };
    networked(
        with_inputs(git_guarded(verdict, words, dirs), simple, dirs),
        words,
    )
}

/// A `gh` read hits the API: it declares `Network` with the default host
/// beside the paths it read. A `--repo`/`-R` value naming a host reaches
/// past that endpoint, so the call is reviewed instead. Any other verdict
/// passes through.
fn networked(verdict: Outcome, words: &[Word]) -> Outcome {
    let Outcome::Reads(paths) = verdict else {
        return verdict;
    };
    if !is_gh(words) {
        return Outcome::Reads(paths);
    }
    if repo_selects_host(words) {
        return Outcome::Executes;
    }
    Outcome::Network(paths)
}

/// Whether a `--repo`/`-R` value in a `gh` call names a host:
/// `HOST/OWNER/REPO` reaches past the default endpoint, so the call is
/// reviewed. A bare `OWNER/REPO`, or a value with no slash, cannot name
/// one. A `--repo` word consumed as another flag's value keeps its
/// own meaning, but such a call is reviewed too: it stays fail-closed.
fn repo_selects_host(words: &[Word]) -> bool {
    words
        .iter()
        .any(|word| word.cooked.strip_prefix("--repo=").is_some_and(has_host))
        || words.windows(2).any(|pair| {
            let [flag, value] = pair else {
                return false;
            };
            (flag.cooked == "--repo" || flag.cooked == "-R") && has_host(&value.cooked)
        })
}

/// Whether a `--repo`/`-R` value holds a host part: more than
/// `OWNER/REPO` can name.
fn has_host(value: &str) -> bool {
    value.split('/').count() > 2
}

/// A git read runs only where the repository guards pass in every
/// directory the stage may run in: a bare repository, or a `.git` file
/// whose target Fiber cannot verify, makes the call `executes`. Any
/// other command, and a git call the lookup refused, passes through.
fn git_guarded(verdict: Outcome, words: &[Word], dirs: &[String]) -> Outcome {
    let Outcome::Reads(_) = verdict else {
        return verdict;
    };
    if !is_git(words) {
        return verdict;
    }
    if dirs
        .iter()
        .all(|dir| super::git::repository_ok(Path::new(dir)))
    {
        verdict
    } else {
        Outcome::Executes
    }
}

/// An `xargs` stage: the target must name one of the six read-only
/// commands, classified by its own flags; anything else executes. A target
/// that never opens files (`echo`, `printf`) reads with no paths. A
/// path-reading target (`wc`, `grep`, `head`, `tail`) opens filenames fed
/// on stdin, so it reads only when every one is statically known: at
/// least one earlier stage ran, every one printed its words, each word
/// holds no `xargs` metacharacter, and each word resolves, exactly as the
/// credential deny holds it, to a path inside a canonical running
/// directory. Proven paths join the declared operands; anything else is
/// reviewed, declaring nothing.
fn xargs_outcome(
    inner: &[Word],
    simple: &Simple,
    dirs: &[String],
    stdin_words: &[String],
    stdin_proven: bool,
) -> Outcome {
    let Some(entry) = inner
        .first()
        .and_then(|target| wrap::xargs_entry(&target.cooked))
    else {
        return Outcome::Executes;
    };
    if entry.paths {
        if !stdin_proven || has_input(simple) {
            return Outcome::Executes;
        }
        let Outcome::Reads(mut declared) = builtin_outcome(inner, entry, dirs) else {
            return Outcome::Executes;
        };
        let Some(canonical_dirs) = canonical_dirs(dirs) else {
            return Outcome::Executes;
        };
        for word in stdin_words {
            if !static_word(word) {
                return Outcome::Executes;
            }
            for dir in dirs {
                let Some(path) = resolve_canonical(&Path::new(dir).join(word)) else {
                    return Outcome::Executes;
                };
                if !canonical_dirs.iter().any(|base| path.starts_with(base)) {
                    return Outcome::Executes;
                }
                let Some(path) = path.to_str().map(str::to_owned) else {
                    return Outcome::Executes;
                };
                declared.push(path);
            }
        }
        return with_inputs(Outcome::Reads(declared), simple, dirs);
    }
    with_inputs(builtin_outcome(inner, entry, dirs), simple, dirs)
}

/// Whether a stage reads stdin from a redirect file of its own, rather
/// than from the pipeline.
fn has_input(simple: &Simple) -> bool {
    simple
        .redirects
        .iter()
        .any(|redirect| matches!(redirect, Redirect::Input(_)))
}

/// Whether a stage prints statically-known words: a simple `echo`, whose
/// every operand word reaches `xargs` stdin as written. Any other stage's
/// filenames are unproven, including a `printf` whose format string
/// transforms its operands and any listing whose names come from command
/// output. Appends the operand words when static.
fn static_words(stage: &Stage, words: &mut Vec<String>) -> bool {
    let Stage::Simple(simple) = stage else {
        return false;
    };
    let Some(first) = simple.words.first() else {
        return false;
    };
    if first.cooked != "echo" {
        return false;
    }
    words.extend(simple.words.iter().skip(1).map(|word| word.cooked.clone()));
    true
}

/// Whether a stdin word is statically known: it holds no `xargs`
/// metacharacter, so the pipeline feeds it whole, and it names no flag:
/// a leading `-` would read as a flag at runtime, following links the
/// call never declares. A blank or a newline would split it, a quote
/// would re-quote it, and a backslash would escape the next character,
/// so any of them leaves the word unproven.
fn static_word(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('-')
        && !word.chars().any(|ch| {
            matches!(
                ch,
                ' ' | '\t' | '\n' | '\r' | '\x0B' | '\x0C' | '\\' | '\'' | '"'
            )
        })
}

/// Each running directory as the credential deny holds it, or nothing
/// when one does not resolve: stdin proof has nothing to land in.
fn canonical_dirs(dirs: &[String]) -> Option<Vec<PathBuf>> {
    dirs.iter()
        .map(|dir| resolve_canonical(Path::new(dir)))
        .collect()
}

/// `path` with symlinks resolved and `..` removed, exactly as the
/// credential deny holds it (`docs/permissions.md`, "Credentials",
/// `loop::calls::resolve`): the longest existing prefix canonicalised,
/// the rest appended word for word. Anything but a plain name past the
/// existing prefix (`.`, `..`, `/`) resolves to nothing, and a path with
/// no existing prefix does too.
fn resolve_canonical(path: &Path) -> Option<PathBuf> {
    let parts: Vec<Component<'_>> = path.components().collect();
    (0..=parts.len()).rev().find_map(|split| {
        let (existing, rest) = parts.split_at(split);
        let real = existing.iter().collect::<PathBuf>().canonicalize().ok()?;
        Some(
            rest.iter()
                .try_fold(real, |path, component| match component {
                    Component::Normal(name) => Some(path.join(name)),
                    Component::Prefix(_)
                    | Component::RootDir
                    | Component::CurDir
                    | Component::ParentDir => None,
                }),
        )
    })?
}

/// Whether a stage runs `cd`: its first word is exactly `cd`.
fn is_cd(simple: &Simple) -> bool {
    simple.words.first().is_some_and(|word| word.cooked == "cd")
}

/// Whether a stage's words run `gh`: the first word's basename is `gh`.
fn is_gh(words: &[Word]) -> bool {
    words
        .first()
        .is_some_and(|word| word.cooked.rsplit('/').next() == Some("gh"))
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
    let mut words = rest_after(words, entry.name.split(' ').count());
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
