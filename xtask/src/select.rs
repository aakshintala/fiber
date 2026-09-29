//! What a diff runs (`docs/ci.md`, "Selection"), which CI jobs that means,
//! and the verdict behind the `CI` check.

use std::collections::{BTreeMap, BTreeSet};

/// A workspace member: its directory relative to the workspace root, and
/// the members it depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) dir: String,
    pub(crate) deps: Vec<String>,
    pub(crate) library: bool,
}

pub(crate) type Members = BTreeMap<String, Member>;

/// What a diff runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selection {
    /// Every file is Markdown or under `docs/` or `research/`.
    Docs,
    /// A manifest, the lock file, the toolchain pin or a workflow changed.
    All(Vec<String>),
    /// The crates the diff touches and every crate that depends on them.
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
/// `docs/ci.md`: one shard per 25 mutants, at most 6.
const MUTANTS_PER_SHARD: u64 = 25;
const MAX_SHARDS: u64 = 6;

fn is_docs_file(path: &str) -> bool {
    path.ends_with(".md") || path.starts_with("docs/") || path.starts_with("research/")
}

fn runs_all(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    RUN_ALL_NAMES.contains(&name) || path.starts_with(".github/")
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
    if files.iter().all(|f| is_docs_file(f)) {
        return Selection::Docs;
    }
    if files.iter().any(|f| runs_all(f)) {
        return Selection::All(members.keys().cloned().collect());
    }
    let touched = files
        .iter()
        .filter_map(|f| owner(f, members))
        .map(str::to_owned)
        .collect();
    Selection::Crates(dependents(touched, members).into_iter().collect())
}

pub(crate) fn shard_count(mutants: u64) -> u64 {
    mutants.div_ceil(MUTANTS_PER_SHARD).min(MAX_SHARDS)
}

/// Which CI jobs run, by the job ids in `.github/workflows/ci.yml`, and how
/// many mutant shards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) jobs: BTreeMap<&'static str, bool>,
    pub(crate) shards: u64,
}

pub(crate) fn plan(mode: &str, packages: &[String], event: &str, bug: bool, mutants: u64) -> Plan {
    let pr = event == "pull_request";
    let code = mode != "docs";
    let shards = if pr && code { shard_count(mutants) } else { 0 };
    let jobs = BTreeMap::from([
        ("docs", pr),
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
    /// The module file that declares it, relative to the repository, and the
    /// declaration. None for a file under a crate's `tests/`, which Cargo
    /// finds on its own.
    pub(crate) declared_in: Option<(String, String)>,
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
                (format!("binary_id({name}::{binary})"), None)
            }
            ["src", modules @ .., file] => {
                let stem = file.strip_suffix(".rs").unwrap_or(file);
                let mut path_modules: Vec<&str> = modules.to_vec();
                let dir = format!(
                    "{}/src{}",
                    member.dir,
                    modules.iter().map(|m| format!("/{m}")).collect::<String>()
                );
                let declared_in = if stem == "tests" {
                    // A crate root may be main.rs, and a module dir.rs may
                    // be dir/mod.rs; scripts/bug-base falls back to those.
                    let parent = if modules.is_empty() {
                        format!("{dir}/lib.rs")
                    } else {
                        format!("{dir}.rs")
                    };
                    (parent, "#[cfg(test)]\nmod tests;".to_owned())
                } else {
                    let module = stem.strip_suffix("_tests").unwrap_or(stem);
                    path_modules.push(module);
                    (
                        format!("{dir}/{module}.rs"),
                        format!("#[cfg(test)]\n#[path = \"{stem}.rs\"]\nmod tests;"),
                    )
                };
                path_modules.push("tests");
                let prefix: String = path_modules.iter().map(|m| format!("{m}::")).collect();
                (
                    format!("(package({name}) & test(/^{prefix}/))"),
                    Some(declared_in),
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
