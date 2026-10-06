//! What a diff runs (`docs/ci.md`, "Selection"), which CI jobs that means,
//! and the verdict behind the `CI` check.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};

use crate::rules::RustFile;

/// A workspace member: its directory relative to the workspace root, its
/// version, and the members it depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) dir: String,
    pub(crate) version: String,
    pub(crate) deps: Vec<String>,
    pub(crate) library: bool,
}

pub(crate) type Members = BTreeMap<String, Member>;

/// `name` as a package ID spec that names exactly one package: cargo
/// resolves a bare name against every crate in the dependency graph, not
/// just workspace members, so a workspace crate that shares a name with one
/// of its dependencies' dependencies (`docs/ci.md`, "Selection") makes `-p
/// name` ambiguous. `name@version` always picks the workspace member.
pub(crate) fn spec(name: &str, members: &Members) -> String {
    match members.get(name) {
        Some(member) => format!("{name}@{}", member.version),
        None => name.to_owned(),
    }
}

/// What a diff runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selection {
    /// Every file is Markdown or under `docs/` or `research/`, and none is a
    /// file a crate compiles in.
    Docs,
    /// A manifest, the lock file, the toolchain pin, a workflow, anything
    /// under `scripts/`, or gate configuration changed.
    All(Vec<String>),
    /// The crates the diff touches and every crate that depends on them, plus
    /// any crate that compiles in a changed listed file, without dependents.
    Crates(Vec<String>),
}

impl Selection {
    pub(crate) fn mode(&self) -> &'static str {
        match self {
            Selection::Docs => "docs",
            Selection::All(_) => "all",
            Selection::Crates(_) => "crates",
        }
    }

    pub(crate) fn packages(&self) -> &[String] {
        match self {
            Selection::Docs => &[],
            Selection::All(packages) | Selection::Crates(packages) => packages,
        }
    }
}

const RUN_ALL_NAMES: [&str; 3] = ["Cargo.lock", "Cargo.toml", "rust-toolchain.toml"];
/// Root-level gate configuration (`docs/ci.md`, "Selection").
const RUN_ALL_ROOTS: [&str; 4] = [
    ".cargo/config.toml",
    ".config/nextest.toml",
    "clippy.toml",
    "deny.toml",
];
/// Files a crate compiles in that the selector would not otherwise route to
/// that crate: Markdown anywhere, and every file outside the crate
/// (`docs/ci.md`, "Selection").
const COMPILED_IN: &[(&str, &str)] = &[
    ("docs/errors.md", "contract"),
    ("docs/events.md", "contract"),
    ("docs/invocation.md", "contract"),
    ("crates/loop/prompt/messages.md", "loop"),
    ("crates/loop/prompt/opening.md", "loop"),
    ("crates/loop/prompt/reviewer.md", "loop"),
    ("crates/loop/prompt/system.md", "loop"),
    ("crates/loop/skills/cache-warming/SKILL.md", "loop"),
    ("crates/loop/skills/using-fiber/SKILL.md", "loop"),
    ("crates/tools/prompt/guidelines.md", "tools"),
];
/// `docs/ci.md`: mutation testing runs as 6 shards.
const MUTANT_SHARDS: u64 = 6;

fn is_docs_file(path: &str) -> bool {
    path.ends_with(".md") || path.starts_with("docs/") || path.starts_with("research/")
}

fn string_literal(text: &str) -> Option<String> {
    if let Some(inner) = text.strip_prefix('"') {
        return inner.strip_suffix('"').map(str::to_owned);
    }
    let rest = text.strip_prefix('r')?;
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    let inner = rest.get(hashes..)?.strip_prefix('"')?;
    let close = format!("\"{}", "#".repeat(hashes));
    inner.strip_suffix(&close).map(str::to_owned)
}

fn include_path(stream: TokenStream) -> Option<String> {
    let mut tokens = stream.into_iter();
    let TokenTree::Literal(lit) = tokens.next()? else {
        return None;
    };
    if let Some(tree) = tokens.next() {
        let TokenTree::Punct(p) = tree else {
            return None;
        };
        if p.as_char() != ',' || tokens.next().is_some() {
            return None;
        }
    }
    string_literal(&lit.to_string())
}

fn include_literals(source: &str) -> Result<(Vec<String>, bool), proc_macro2::LexError> {
    fn walk(stream: TokenStream, out: &mut Vec<String>, unresolvable: &mut bool) {
        let mut tokens = stream.into_iter();
        while let Some(tree) = tokens.next() {
            match tree {
                TokenTree::Ident(ident) if ident == "include_str" || ident == "include_bytes" => {
                    if let Some(TokenTree::Punct(p)) = tokens.next()
                        && p.as_char() == '!'
                        && let Some(TokenTree::Group(group)) = tokens.next()
                    {
                        match include_path(group.stream()) {
                            Some(path) => out.push(path),
                            None => *unresolvable = true,
                        }
                    }
                }
                TokenTree::Group(group) => walk(group.stream(), out, unresolvable),
                TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    let mut unresolvable = false;
    walk(source.parse()?, &mut out, &mut unresolvable);
    Ok((out, unresolvable))
}

pub(crate) fn compiled_in_mismatches(
    files: &[RustFile],
    members: &Members,
) -> Result<Vec<String>, String> {
    let mut found = BTreeSet::new();
    let mut failures = Vec::new();
    for f in files {
        let (literals, unresolvable) = include_literals(&f.source)
            .map_err(|e| format!("{}: does not tokenise as Rust: {e}", f.path))?;
        if unresolvable {
            failures.push(format!(
                "{}: include_str! argument is not a string literal; the compiled-in check cannot resolve it",
                f.path
            ));
        }
        let dir = Path::new(&f.path).parent().unwrap_or(Path::new(""));
        let Some(member) = members.get(&f.krate) else {
            continue;
        };
        for lit in literals {
            let target = crate::docs::join(dir, &lit)
                .to_string_lossy()
                .replace('\\', "/");
            let inside = target
                .strip_prefix(&member.dir)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
            if is_docs_file(&target) || !inside {
                found.insert((target, f.krate.clone()));
            }
        }
    }
    let listed: BTreeSet<(String, String)> = COMPILED_IN
        .iter()
        .map(|(path, krate)| ((*path).to_owned(), (*krate).to_owned()))
        .collect();
    failures.extend(found.difference(&listed).map(|(path, krate)| {
        format!("{path}: {krate} compiles it in, but the compiled-in list does not list it")
    }));
    failures.extend(
        listed.difference(&found).map(|(path, krate)| {
            format!("{path}: listed for {krate}, but no crate compiles it in")
        }),
    );
    Ok(failures)
}

fn runs_all(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    RUN_ALL_NAMES.contains(&name)
        || path.starts_with(".github/")
        || path.starts_with("scripts/")
        || RUN_ALL_ROOTS.contains(&path)
}

/// The workspace member whose directory holds `path`: the innermost one.
pub(crate) fn owner<'a>(path: &str, members: &'a Members) -> Option<&'a str> {
    members
        .iter()
        .filter(|(_, m)| path.starts_with(&format!("{}/", m.dir)))
        .max_by_key(|(_, m)| m.dir.len())
        .map(|(name, _)| name.as_str())
}

/// `touched` plus every member that depends on one of them, transitively.
pub(crate) fn dependents(touched: BTreeSet<String>, members: &Members) -> BTreeSet<String> {
    let mut selected = touched;
    // Each round adds one more link of the dependency chain; no chain is
    // longer than the member count.
    for _ in 0..members.len() {
        let more: Vec<String> = members
            .iter()
            .filter(|(name, m)| {
                !selected.contains(*name) && m.deps.iter().any(|d| selected.contains(d))
            })
            .map(|(name, _)| name.clone())
            .collect();
        selected.extend(more);
    }
    selected
}

pub(crate) fn classify(files: &[String], members: &Members) -> Selection {
    classify_with(files, members, COMPILED_IN)
}

fn classify_with(files: &[String], members: &Members, listed: &[(&str, &str)]) -> Selection {
    if files.iter().any(|f| runs_all(f)) {
        return Selection::All(members.keys().cloned().collect());
    }
    let is_listed = |path: &str| listed.iter().any(|(p, _)| *p == path);
    if files.iter().all(|f| is_docs_file(f) && !is_listed(f)) {
        return Selection::Docs;
    }
    let compiled: BTreeSet<String> = files
        .iter()
        .flat_map(|f| {
            listed
                .iter()
                .filter_map(move |(p, krate)| (*p == f.as_str()).then_some(*krate))
        })
        .filter(|name| members.contains_key(*name))
        .map(str::to_owned)
        .collect();
    // Docs files do not own a crate; listed files run their crate without
    // dependents (`docs/ci.md`, "Selection").
    let touched = files
        .iter()
        .filter(|f| !is_docs_file(f) && !is_listed(f))
        .filter_map(|f| owner(f, members))
        .map(str::to_owned)
        .collect();
    let mut selected = dependents(touched, members);
    selected.extend(compiled);
    Selection::Crates(selected.into_iter().collect())
}

/// Which CI jobs run, by the job ids in `.github/workflows/ci.yml`, and how
/// many mutant shards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) jobs: BTreeMap<&'static str, bool>,
    pub(crate) shards: u64,
}

pub(crate) fn plan(mode: &str, packages: &[String], event: &str, bug: bool) -> Plan {
    let pr = event == "pull_request";
    let code = mode != "docs";
    let shards = if pr && code { MUTANT_SHARDS } else { 0 };
    let jobs = BTreeMap::from([
        ("lint", pr && code),
        // The backstop on `main` compiles the whole workspace on every push.
        ("test", !pr || !packages.is_empty()),
        ("mutants", shards > 0),
        ("bug_base", pr && code && bug),
    ]);
    Plan { jobs, shards }
}

/// Failures, one line each; empty when `CI` passes. `results` maps each job
/// `CI` needs to its result, and `jobs` says which the selection chose.
pub(crate) fn verdict(
    results: &BTreeMap<String, String>,
    jobs: &BTreeMap<String, bool>,
) -> Vec<String> {
    let select = results.get("select").map_or("missing", String::as_str);
    if select != "success" {
        return vec![format!("select: {select}, so the selection failed")];
    }
    let mut failures = Vec::new();
    for (name, result) in results.iter().filter(|(name, _)| *name != "select") {
        match jobs.get(name) {
            None => failures.push(format!("{name}: not in the selection")),
            Some(true) if result != "success" => {
                failures.push(format!("{name}: selected, but {result}"))
            }
            Some(false) if result != "skipped" => {
                failures.push(format!("{name}: not selected, but {result}"))
            }
            Some(_) => {}
        }
    }
    failures
}

const TICKET_WORDS: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The issue a pull request body resolves, from its first "Resolves #N" or
/// GitHub's other closing keywords.
pub(crate) fn ticket(body: &str) -> Option<String> {
    let mut prev = ' ';
    for (i, c) in body.char_indices() {
        if c.is_alphabetic()
            && !is_word(prev)
            && let Some(number) = ticket_at(body.get(i..).unwrap_or_default())
        {
            return Some(number);
        }
        prev = c;
    }
    None
}

fn ticket_at(text: &str) -> Option<String> {
    let word_end = text
        .find(|c: char| !c.is_alphabetic())
        .unwrap_or(text.len());
    let (word, rest) = text.split_at(word_end);
    if !TICKET_WORDS.contains(&word.to_lowercase().as_str()) {
        return None;
    }
    let after_space = rest.trim_start();
    if after_space.len() == rest.len() {
        return None;
    }
    let digits_and_more = after_space.strip_prefix('#')?;
    let end = digits_and_more
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits_and_more.len());
    let (digits, tail) = digits_and_more.split_at(end);
    if digits.is_empty() || tail.starts_with(is_word) {
        return None;
    }
    Some(digits.to_owned())
}

/// Whether `rel`, a path relative to its crate, is a test file
/// (`docs/code-quality.md`, "Size").
pub(crate) fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs" || name.ends_with("_tests.rs") || rel.starts_with("tests/")
}

/// A test file the bug-fix check runs at the base commit, and how its
/// parent module declares it there when the file is new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestFile {
    pub(crate) path: String,
    /// The module files that may declare it, relative to the repository, in
    /// the order to try them, each with the declaration that file needs.
    /// Empty for a file under a crate's `tests/`, which Cargo finds on its
    /// own.
    pub(crate) declared_in: Vec<(String, String)>,
}

/// For a unit-test file `stem`.rs in `dir`: the module its tests sit in,
/// when that is not `dir`'s own, and the files that may declare it with the
/// declaration each needs (`docs/code-quality.md`, "Size"). A `#[path]` is
/// relative to the declaring file's directory. `root` says `dir` is the
/// crate's `src`.
fn declarations<'a>(
    dir: &str,
    root: bool,
    stem: &'a str,
) -> (Option<&'a str>, Vec<(String, String)>) {
    let plain = || "#[cfg(test)]\nmod tests;".to_owned();
    let with_path = |path: &str| format!("#[cfg(test)]\n#[path = \"{path}\"]\nmod tests;");
    match stem.strip_suffix("_tests") {
        None if root => (
            None,
            vec![
                (format!("{dir}/lib.rs"), plain()),
                (format!("{dir}/main.rs"), plain()),
            ],
        ),
        None => (
            None,
            vec![
                (format!("{dir}.rs"), plain()),
                (format!("{dir}/mod.rs"), plain()),
            ],
        ),
        Some(crate_root @ ("lib" | "main")) if root => (
            None,
            vec![(
                format!("{dir}/{crate_root}.rs"),
                with_path(&format!("{stem}.rs")),
            )],
        ),
        Some(module) => (
            Some(module),
            vec![
                (
                    format!("{dir}/{module}.rs"),
                    with_path(&format!("{stem}.rs")),
                ),
                (
                    format!("{dir}/{module}/mod.rs"),
                    with_path(&format!("../{stem}.rs")),
                ),
            ],
        ),
    }
}

/// The nextest filter selecting every test in the test files among
/// `files`, the packages that own them, and those test files.
pub(crate) fn test_filter(
    files: &[String],
    members: &Members,
) -> (String, Vec<String>, Vec<TestFile>) {
    let mut terms = Vec::new();
    let mut packages = BTreeSet::new();
    let mut tests = Vec::new();
    for path in files.iter().filter(|p| p.ends_with(".rs")) {
        let Some(name) = owner(path, members) else {
            continue;
        };
        let Some(member) = members.get(name) else {
            continue;
        };
        let rel = path.get(member.dir.len() + 1..).unwrap_or_default();
        if !is_test_file(rel) {
            continue;
        }
        let parts: Vec<&str> = rel.split('/').collect();
        let (term, declared_in) = match parts.as_slice() {
            ["tests", binary, ..] => {
                let binary = binary.strip_suffix(".rs").unwrap_or(binary);
                (format!("binary_id({name}::{binary})"), vec![])
            }
            ["src", modules @ .., file] => {
                let stem = file.strip_suffix(".rs").unwrap_or(file);
                let dir = format!(
                    "{}/src{}",
                    member.dir,
                    modules.iter().map(|m| format!("/{m}")).collect::<String>()
                );
                let (module, declared_in) = declarations(&dir, modules.is_empty(), stem);
                let prefix: String = modules
                    .iter()
                    .copied()
                    .chain(module)
                    .chain(["tests"])
                    .map(|m| format!("{m}::"))
                    .collect();
                (
                    format!("(package({name}) & test(/^{prefix}/))"),
                    declared_in,
                )
            }
            _ => continue,
        };
        terms.push(term);
        packages.insert(name.to_owned());
        tests.push(TestFile {
            path: path.clone(),
            declared_in,
        });
    }
    (terms.join(" | "), packages.into_iter().collect(), tests)
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
