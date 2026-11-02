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

/// Effects of `command` run in `workdir` under the session `workspace`. A
/// command the tree-sitter parse rejects, or any element not on the
/// read-only list, is `executes` with no paths. The single simple stage's
/// raw tokens are the subject. `workspace` is the trust anchor for the
/// workspace's GitHub remote owners: the call's `workdir` only resolves
/// paths, never owners. `configured` holds the person's `shell.read_only`
/// entries beside the built-in list: an element no built-in path owns
/// falls back to them, and a built-in path answers with its own flags plus
/// a same-named configured entry's. An exact form still answers first, and
/// a path-qualified or otherwise owned name still takes nothing.
pub(super) fn classify(
    command: &str,
    workspace: &Path,
    workdir: &Path,
    configured: &[(String, Vec<String>)],
) -> Effects {
    let Some(script) = super::syntax::parse(command) else {
        return executes(None, None);
    };
    let verdict = outcome(&script, workspace, workdir, configured);
    // A substituted call that executes names no subject: no allow rule can
    // match a reviewed call whose words arrive only when it runs. A read
    // keeps its raw-token subject, substitution spellings included.
    let (subject, prefix) = match &verdict {
        Outcome::Executes if script_has_substituted(&script) => (None, None),
        Outcome::Executes | Outcome::Reads(_) | Outcome::Network(_) => subject_and_prefix(&script),
    };
    match verdict {
        Outcome::Reads(paths) => reads(paths, subject, prefix),
        Outcome::Network(paths) => network(paths, subject, prefix),
        Outcome::Executes => executes(subject, prefix),
    }
}

/// Whether any stage of the script holds a substituted word.
fn script_has_substituted(script: &Script) -> bool {
    script
        .elements
        .iter()
        .flat_map(|element| element.stages.iter())
        .any(|stage| {
            matches!(stage, Stage::Simple(simple) if simple.words.iter().any(|word| word.substituted))
        })
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
pub(super) fn not_shown_read_only(
    command: &str,
    workspace: &Path,
    workdir: &Path,
    configured: &[(String, Vec<String>)],
) -> Option<String> {
    let script = super::syntax::parse(command)?;
    let walked = walk(&script, workspace, workdir, configured);
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
    let unshown = unshown
        .iter()
        .map(|text| format!("`{text}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("Not shown read-only: {unshown}"))
}

/// The trust context for one classification: the session workspace root
/// that owns the GitHub remote owners, beside the call's own workdir,
/// which only resolves paths, and whether the command holds a `cd`.
struct Trust<'a> {
    workspace: &'a Path,
    workdir: &'a Path,
    has_cd: bool,
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

/// Record every stage as unshown after the path is lost: no later part
/// is trustworthy, but the note can still name the call's tail.
fn push_unshown(walked: &mut Walked, stages: &[Stage]) {
    for stage in stages {
        walked.parts.push(Part {
            text: stage_text(stage).to_owned(),
            shown: false,
        });
    }
}

fn outcome(
    script: &Script,
    workspace: &Path,
    workdir: &Path,
    configured: &[(String, Vec<String>)],
) -> Outcome {
    let walked = walk(script, workspace, workdir, configured);
    if walked.saw_executes {
        Outcome::Executes
    } else if walked.saw_network {
        Outcome::Network(walked.paths)
    } else {
        Outcome::Reads(walked.paths)
    }
}

fn walk(
    script: &Script,
    workspace: &Path,
    workdir: &Path,
    configured: &[(String, Vec<String>)],
) -> Walked {
    let trust = Trust {
        workspace,
        workdir,
        has_cd: script_has_cd(script),
    };
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
            push_unshown(&mut walked, &element.stages);
            continue;
        }
        let Some(dirs) = cwd.enter(element.join) else {
            lost = true;
            walked.saw_executes = true;
            push_unshown(&mut walked, &element.stages);
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
                &trust,
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
    trust: &Trust,
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
    // A substitution survives unwrapping only in a direct read: a wrapper
    // around one, such as `env` or `timeout`, never reads.
    if stepped && words.iter().any(|word| word.substituted) {
        return Outcome::Executes;
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
    let (verdict, gh_entry) = if EXACT.iter().any(|row| cooked_equals(words, row)) {
        (Outcome::Reads(Vec::new()), None)
    } else if words.first().is_some_and(|word| word.cooked == "sed") {
        // `sed` has its own print-only grammar, not a table row. It never
        // takes a substituted word: only a listed git read validates one.
        if words.iter().any(|word| word.substituted) {
            return Outcome::Executes;
        }
        let files = words.iter().skip(1).map(|word| word.cooked.as_str());
        let Some(files) = super::sed::files(files) else {
            return Outcome::Executes;
        };
        (path_tail(&files, dirs), None)
    } else {
        match entry::find(words, configured) {
            Found::Builtin(entry, extra) => {
                // Leading outer redirects prepend their spellings to the
                // tokens, so the words start past them: a substitution in
                // `>/dev/null git diff $(...)..HEAD` still lines up with
                // its own token. A redirect's extra word stays out of line
                // by construction, so it still executes.
                let base = simple
                    .tokens
                    .iter()
                    .position(|token| simple.words.first().is_some_and(|word| *token == word.raw))
                    .unwrap_or(0);
                (
                    builtin_outcome(words, entry, extra, dirs, Some((&simple.tokens, base))),
                    Some(entry),
                )
            }
            Found::Configured(flags, consumed) => {
                // A configured entry never takes a substituted word: its
                // operands declare paths, and a substitution is never one.
                if words.iter().any(|word| word.substituted) {
                    (Outcome::Executes, None)
                } else {
                    (
                        entry::configured_outcome(flags, rest_after(words, consumed), dirs),
                        None,
                    )
                }
            }
            Found::None => (Outcome::Executes, None),
        }
    };
    networked(
        with_inputs(git_guarded(verdict, words, dirs), simple, dirs),
        words,
        trust,
        gh_entry,
    )
}

/// A `gh` read hits the API: it declares `Network` with the default host
/// beside the paths it read, unless it is own: every repository it names
/// belongs to an owner of the session workspace's `github.com` remotes,
/// and then it declares `reads` with the paths it read. A `--repo`/`-R`
/// value naming a host reaches past that endpoint, so the call is reviewed
/// instead. A configured `gh` entry keeps `network`: only the built-in
/// reads qualify, because Fiber knows their flags and operands. Any other
/// verdict passes through.
fn networked(
    verdict: Outcome,
    words: &[Word],
    trust: &Trust,
    gh_entry: Option<&Command>,
) -> Outcome {
    let Outcome::Reads(paths) = verdict else {
        return verdict;
    };
    if !is_gh(words) {
        return Outcome::Reads(paths);
    }
    if repo_selects_host(words, gh_entry) {
        return Outcome::Executes;
    }
    let Some(entry) = gh_entry else {
        return Outcome::Network(paths);
    };
    match super::gh_owner::standing(words, entry, trust.workspace, trust.workdir, trust.has_cd) {
        super::gh_owner::Standing::Own => Outcome::Reads(paths),
        super::gh_owner::Standing::Foreign => Outcome::Network(paths),
    }
}

/// Whether a `--repo`/`-R` value in a `gh` call names a host:
/// `HOST/OWNER/REPO` reaches past the default endpoint, so the call is
/// reviewed. A bare `OWNER/REPO`, or a value with no slash, cannot name
/// one. Only a `--repo`/`-R` in flag position counts: a word another flag
/// consumed keeps its own meaning there, so `gh pr view --json --repo
/// h/o/r` stays `network` instead of tripping the host check.
fn repo_selects_host(words: &[Word], gh_entry: Option<&Command>) -> bool {
    let Some(entry) = gh_entry else {
        return words
            .iter()
            .any(|word| word.cooked.strip_prefix("--repo=").is_some_and(has_host))
            || words.windows(2).any(|pair| {
                let [flag, value] = pair else {
                    return false;
                };
                (flag.cooked == "--repo" || flag.cooked == "-R") && has_host(&value.cooked)
            });
    };
    let mut rest = words.iter().skip(entry.name.split(' ').count());
    let mut ended = false;
    while let Some(word) = rest.next() {
        match entry::word_at(word, ended, entry) {
            entry::WordAt::EndFlags => ended = true,
            entry::WordAt::Flag => {
                // The entry already accepted this `--repo=` when the verdict
                // read, so only the host decides here.
                if let Some(value) = word.cooked.strip_prefix("--repo=")
                    && has_host(value)
                {
                    return true;
                }
                if word.cooked.contains('=') {
                    continue;
                }
                let Some(flag) = entry.flags.iter().find(|flag| flag.spelling == word.cooked)
                else {
                    continue;
                };
                if !flag.takes_value() {
                    continue;
                }
                let Some(value) = rest.next() else {
                    return false;
                };
                if (word.cooked == "--repo" || word.cooked == "-R") && has_host(&value.cooked) {
                    return true;
                }
            }
            entry::WordAt::Operand(_) => {}
        }
    }
    false
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
        let Outcome::Reads(mut declared) = builtin_outcome(inner, entry, &[], dirs, None) else {
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
    with_inputs(builtin_outcome(inner, entry, &[], dirs, None), simple, dirs)
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
/// output. Appends the operand words when static. A substituted word is
/// never static: its text arrives only when it runs.
fn static_words(stage: &Stage, words: &mut Vec<String>) -> bool {
    let Stage::Simple(simple) = stage else {
        return false;
    };
    if simple.words.iter().any(|word| word.substituted) {
        return false;
    }
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

/// Whether any simple stage of the command runs `cd`: an unnamed `gh`
/// read is own only when the command holds no `cd`, wherever it stands.
fn script_has_cd(script: &Script) -> bool {
    script
        .elements
        .iter()
        .flat_map(|element| element.stages.iter())
        .any(|stage| matches!(stage, Stage::Simple(simple) if is_cd(simple)))
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
    if operand.substituted {
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

/// Whether a substituted operand word may read: the substitution sits at
/// a word edge, bare or composed with one `..`/`...` range against a plain
/// outer ref, and its inner text is exactly `git merge-base <rev> <rev>`.
/// On empty substitution output the runtime word is the bare range, which
/// git parses as revisions: never a flag and never a path.
fn substituted_operand_ok(raw: &str) -> bool {
    let Some((inner, remainder, sub_first)) = substitution_parts(raw) else {
        return false;
    };
    if !inner_ok(inner) {
        return false;
    }
    if remainder.is_empty() {
        return true;
    }
    if sub_first {
        let range = remainder
            .strip_prefix("...")
            .or_else(|| remainder.strip_prefix(".."));
        range.is_some_and(outer_ref_ok)
    } else {
        let range = remainder
            .strip_suffix("...")
            .or_else(|| remainder.strip_suffix(".."));
        range.is_some_and(outer_ref_ok)
    }
}

/// Split a substituted word into its inner command text and the outer
/// remainder, with whether the substitution stands first. The substitution
/// must start at the word's first byte or end at its last byte; any prefix
/// and suffix together refuse.
fn substitution_parts(raw: &str) -> Option<(&str, &str, bool)> {
    if let Some(rest) = raw.strip_prefix("$(") {
        let end = rest.find(')')?;
        return Some((&rest[..end], &rest[end + 1..], true));
    }
    let start = raw.rfind("$(")?;
    if !raw.ends_with(')') {
        return None;
    }
    Some((&raw[start + 2..raw.len() - 1], &raw[..start], false))
}

/// Whether the outer ref beside a substitution names a plain revision:
/// non-empty, never starting with `-` (a flag on empty output) or `.` (a
/// path), holding no `..` and no `:`, over the plain alphabet otherwise.
fn outer_ref_ok(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('-')
        && !text.starts_with('.')
        && !text.contains("..")
        && text
            .chars()
            .all(|ch| super::syntax::is_plain(ch) && ch != ':')
}

/// Whether the inner text is exactly `git merge-base <rev> <rev>` over the
/// safe byte alphabet: the plain set plus the `^~{}` suffix bytes and
/// literal spaces. Every other byte refuses before tokenizing, so a
/// newline can never separate a second command.
fn inner_ok(inner: &str) -> bool {
    if !inner
        .chars()
        .all(|ch| super::syntax::is_plain(ch) || matches!(ch, '^' | '~' | '{' | '}' | ' '))
    {
        return false;
    }
    let mut words = inner.split(' ').filter(|word| !word.is_empty());
    match (
        words.next(),
        words.next(),
        words.next(),
        words.next(),
        words.next(),
    ) {
        (Some("git"), Some("merge-base"), Some(first), Some(second), None) => {
            rev_ok(first) && rev_ok(second)
        }
        _ => false,
    }
}

/// Whether an inner revision matches the ticket grammar: a non-empty base
/// over the plain alphabet without `:`, never starting with `-` and
/// holding no `..`, with at most one `^`, `~<digits>` or `^{commit}`
/// suffix. A shape-ok revision a `merge-base` run rejects fails with no
/// output, so it is never a path.
fn rev_ok(rev: &str) -> bool {
    let base = if let Some(base) = rev.strip_suffix("^{commit}") {
        base
    } else if let Some(base) = rev.strip_suffix('^') {
        base
    } else if let Some((base, digits)) = rev.split_once('~') {
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        base
    } else {
        rev
    };
    !base.is_empty()
        && !base.starts_with('-')
        && !base.contains("..")
        && base
            .chars()
            .all(|ch| super::syntax::is_plain(ch) && ch != ':')
}

/// The built-in flag walk, kept from the hand lexer: flags by the table,
/// `--` where the entry says so, any other `-` word an operand for `echo`,
/// the first operand the pattern for `grep` and `rg`. The entry's path
/// words never reach the walk: `git status -s` walks from `-s`. `aligned`
/// holds the stage's tokens with the walked words' base index, past any
/// leading redirect spellings, so a substituted word reads only in its own
/// operand slot: a redirect's extra word never lines up with the tokens
/// there. No walk runs for it: `None` always executes on a substituted
/// word.
fn builtin_outcome(
    words: &[Word],
    entry: &Command,
    extra: &[String],
    dirs: &[String],
    aligned: Option<(&[String], usize)>,
) -> Outcome {
    let skipped = entry.name.split(' ').count();
    let mut words = rest_after(words, skipped);
    let total = words.len();
    let mut operands = Vec::new();
    let mut ended = false;
    let mut pattern_given = false;
    while let Some(word) = words.next() {
        // The word's index into the walked rest: yields so far, values a
        // flag consumed included, so it lines up with the stage's tokens.
        let at = total - words.len() - 1;
        match entry::word_at(word, ended, entry) {
            entry::WordAt::Operand(cooked) => {
                if word.substituted {
                    // A substituted operand reads only validated, in a
                    // listed git read before `--`, in its own token slot:
                    // anywhere else it executes, and it never declares a
                    // path and never reads as a flag.
                    let direct = aligned.is_some_and(|(tokens, base)| {
                        tokens
                            .get(base + skipped + at)
                            .is_some_and(|token| *token == word.raw)
                    });
                    if ended
                        || !entry.name.starts_with("git ")
                        || !direct
                        || !substituted_operand_ok(&word.raw)
                    {
                        return Outcome::Executes;
                    }
                } else {
                    operands.push(cooked);
                }
            }
            entry::WordAt::EndFlags => ended = true,
            entry::WordAt::Flag => {
                if word.substituted {
                    return Outcome::Executes;
                }
                // A recursive read that follows links is never read-only,
                // even when the person lists the flag: the credential deny
                // sees only the paths a call names, never a link below
                // them.
                if follows_links(entry.name, &word.cooked) {
                    return Outcome::Executes;
                }
                // The built-in's flags are kept and the configured ones are
                // added: a word the table refuses may still read under the
                // same-named entry's grammar below.
                if entry::accept_flag(entry, &mut words, &word.cooked).is_none()
                    && accept_configured_flag(extra, &mut words, &word.cooked).is_none()
                {
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

/// Whether `word` is a flag that makes a recursive read of `command`
/// follow symbolic links, such as `grep -R`: the credential deny sees only
/// the paths a call names, never a link below them, so the call is reviewed
/// even when the person lists the flag. The built-in list omits these
/// spellings; this guard keeps a merged entry from adding one back. A
/// bundled short-option cluster counts letter by letter, such as `grep -Rn`:
/// the system tool follows links for the `R` inside it. Only a flag-position
/// word reaches it: an operand after `--` keeps its meaning.
fn follows_links(command: &str, word: &str) -> bool {
    let spelling = word.split('=').next().unwrap_or(word);
    if matches!(
        (command, spelling),
        ("grep", "-R")
            | ("grep", "--dereference-recursive")
            | ("rg", "-L")
            | ("rg", "--follow")
            | ("find", "-L")
            | ("find", "-H")
            | ("find", "-follow")
            | ("ls", "-L")
    ) {
        return true;
    }
    let Some(cluster) = spelling.strip_prefix('-') else {
        return false;
    };
    if cluster.starts_with('-') || cluster.is_empty() {
        return false;
    }
    match command {
        "grep" => cluster.contains('R'),
        "rg" | "ls" => cluster.contains('L'),
        "find" => cluster.contains('L') || cluster.contains('H'),
        _ => false,
    }
}

/// Whether the merged entry accepts one flag word under the configured
/// grammar (`entry::configured_outcome`): an exact plain spelling takes no
/// value, a spelling ending `=` takes its value after that exact prefix or
/// as the next word, and any other word refuses. A consumed value is never
/// a path, and a substituted one never reads, as with the built-in walk.
fn accept_configured_flag(
    extra: &[String],
    words: &mut std::slice::Iter<'_, Word>,
    cooked: &str,
) -> Option<()> {
    // An exact plain spelling wins over its `=` twin, in either list
    // order: the word takes no value and the next word stays an operand.
    if extra
        .iter()
        .any(|flag| !flag.ends_with('=') && *flag == cooked)
    {
        return Some(());
    }
    // An inline value after the exact `=` prefix, empty allowed.
    if extra
        .iter()
        .any(|flag| flag.ends_with('=') && cooked.starts_with(flag.as_str()))
    {
        return Some(());
    }
    // A separate value word; missing or substituted executes.
    if extra
        .iter()
        .any(|flag| flag.strip_suffix('=').is_some_and(|base| base == cooked))
    {
        let value = words.next()?;
        if value.substituted {
            return None;
        }
        return Some(());
    }
    None
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
/// a second element, a second stage, a substituted word, or any unparsed
/// text does: the pattern must be known before the call runs.
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
    if simple.words.iter().any(|word| word.substituted) {
        return false;
    }
    simple
        .words
        .first()
        .is_some_and(|word| word.cooked == "grep")
}

/// Whether `command` holds a built-in destructive form anywhere in its
/// text (`docs/tools.md`, "What a tool declares"): `rm -r`, `rm -rf`,
/// `git reset --hard`, `git clean`, `git checkout --`, `git restore` or `git push`.
/// The scan runs before parsing and never claims a complete parse: a word
/// sequence in any `;`-, `&`-, `|`-, group- or newline-separated component
/// matches, whether the parser accepts the command or not. Quotes do not
/// hide a form and wrappers do not hide one either: the listed sequence
/// matches at any word offset within a component, so `sudo rm -rf tmp`
/// counts, with a slash-qualified name compared by its basename. The scan
/// is textual and may over-match, so a quoted or echoed form still counts;
/// over-matching only keeps a call reviewed. Each form below is its own
/// check, so removing one leaves its commands unguarded.
pub(super) fn review_when_confined(command: &str) -> bool {
    components(command).into_iter().any(|words| {
        rm_recursive(&words)
            || rm_recursive_force(&words)
            || git_reset_hard(&words)
            || git_clean(&words)
            || git_checkout_dashdash(&words)
            || git_restore(&words)
            || git_push(&words)
    })
}

/// Git global options precede the subcommand. The scan remains textual:
/// wrappers, quoting and an echoed form do not hide a push from review.
/// A long global holding `=` is one word; the long globals taking a
/// separated value consume the next word, so `git --git-dir /repo/.git`
/// does not read the directory as the subcommand.
fn git_push(words: &[&str]) -> bool {
    words.iter().enumerate().any(|(at, word)| {
        if command_name(word) != "git" {
            return false;
        }
        let mut rest = words
            .iter()
            .skip(at + 1)
            .map(|word| word.trim_matches(['\'', '"']));
        while let Some(word) = rest.next() {
            match word {
                "-C" | "-c" => {
                    let _value = rest.next();
                }
                option if option.starts_with("--") => {
                    if !option.contains('=')
                        && [
                            "--git-dir",
                            "--work-tree",
                            "--namespace",
                            "--super-prefix",
                            "--config-env",
                            "--exec-path",
                        ]
                        .contains(&option)
                    {
                        let _value = rest.next();
                    }
                }
                option if option.starts_with('-') => {}
                subcommand => return subcommand == "push",
            }
        }
        false
    })
}

/// One component's words: the command splits on `;`, `&`, `|`, grouping
/// parentheses and newlines, then on whitespace. Flags keep their dashes,
/// so `--hard` and `--` survive intact.
fn components(command: &str) -> Vec<Vec<&str>> {
    command
        .split([';', '&', '|', '(', ')', '\n'])
        .map(|part| part.split_whitespace().collect())
        .collect()
}

/// A component's command name: the first word with quotes stripped, then
/// the slash-delimited basename, so `/usr/bin/rm` names `rm` without
/// resolving `PATH`.
fn command_name(word: &str) -> &str {
    let bare = word.trim_matches(|mark| mark == '\'' || mark == '"');
    bare.rsplit('/').next().unwrap_or(bare)
}

/// Whether `word` asks `rm` to recurse: a short cluster holding `r` or
/// `R`, or the long `--recursive`. `--force` holds an `r` but recurses
/// nothing, so long options compare whole.
fn rm_recursive_flag(word: &str) -> bool {
    let Some(cluster) = word.strip_prefix('-') else {
        return false;
    };
    if cluster.starts_with('-') {
        return word == "--recursive";
    }
    cluster.contains('r') || cluster.contains('R')
}

/// Whether `word` asks `rm` to force: a short cluster holding `f`, or the
/// long `--force`.
// Exempted from mutation testing: the guard answers `recursive && !force`
// or `recursive && force`, whose disjunction is `recursive`, so no change
// to the force value alone changes `review_when_confined` and every mutant
// here is equivalent (docs/testing/mutants.md). The `rm -r`/`rm -rf` split
// stays so removing either predicate fails the guard test.
#[cfg_attr(false, mutants::skip)]
fn rm_force_flag(word: &str) -> bool {
    let Some(cluster) = word.strip_prefix('-') else {
        return false;
    };
    if cluster.starts_with('-') {
        return word == "--force";
    }
    cluster.contains('f')
}

/// `rm -r` without `-f`, at any word offset within the component so a
/// wrapper such as `sudo` does not hide it. A force flag beside it is the
/// `rm -rf` form below, so the two checks never overlap and deleting one
/// fails its own cases.
fn rm_recursive(words: &[&str]) -> bool {
    rm_starts(words).any(|rest| {
        let recursive = rest.iter().any(|word| rm_recursive_flag(word));
        let force = rest.iter().any(|word| rm_force_flag(word));
        recursive && !force
    })
}

/// `rm -rf`, at any word offset: recurse and force together, in one cluster
/// or two words.
fn rm_recursive_force(words: &[&str]) -> bool {
    rm_starts(words).any(|rest| {
        rest.iter().any(|word| rm_recursive_flag(word))
            && rest.iter().any(|word| rm_force_flag(word))
    })
}

/// The words after each `rm` word in the component, by basename, so a
/// wrapper before `rm` does not hide the form.
fn rm_starts<'a>(words: &'a [&'a str]) -> impl Iterator<Item = &'a [&'a str]> {
    words.iter().enumerate().filter_map(|(start, word)| {
        if command_name(word) == "rm" {
            words.get(start + 1..)
        } else {
            None
        }
    })
}

/// Whether `words` holds `git <name>` at any word offset: a `git` word by
/// basename, directly followed by the subcommand.
fn git_subcommand(words: &[&str], name: &str) -> bool {
    git_starts(words, name).next().is_some()
}

/// The words after each `git <name>` occurrence in the component.
fn git_starts<'a>(words: &'a [&'a str], name: &str) -> impl Iterator<Item = &'a [&'a str]> {
    words.iter().enumerate().filter_map(move |(start, word)| {
        if command_name(word) == "git" && words.get(start + 1).is_some_and(|second| *second == name)
        {
            words.get(start + 2..)
        } else {
            None
        }
    })
}

/// `git reset --hard`: the reset runs with the hard flag anywhere after
/// the subcommand.
fn git_reset_hard(words: &[&str]) -> bool {
    git_starts(words, "reset").any(|rest| rest.contains(&"--hard"))
}

/// `git clean`: the subcommand alone names it, whatever flags follow.
fn git_clean(words: &[&str]) -> bool {
    git_subcommand(words, "clean")
}

/// `git checkout --`: the checkout separates paths with a bare `--`.
fn git_checkout_dashdash(words: &[&str]) -> bool {
    git_starts(words, "checkout").any(|rest| rest.contains(&"--"))
}

/// `git restore`: the subcommand alone names it, whatever follows.
fn git_restore(words: &[&str]) -> bool {
    git_subcommand(words, "restore")
}

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
