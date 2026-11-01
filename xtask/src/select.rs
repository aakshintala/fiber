//! What a diff runs (`docs/ci.md`, "Selection"), which CI jobs that means,
//! and the verdict behind the `CI` check.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

mod changed_tests;
mod compiled_in;
mod plan;
mod runtime_read;
mod test_filter;

pub(crate) use changed_tests::{TestRef, changed_tests, filter};
use compiled_in::{COMPILED_IN, PACKAGE_READERS};
pub(crate) use compiled_in::{compiled_in_mismatches, package_reader_mismatches};
use plan::BINARY_TESTS;
pub(crate) use plan::{base_rows_skipped, plan, shard_timeout_minutes, ten_turns_runs, verdict};
pub(crate) use runtime_read::runtime_read_mismatches;
pub(crate) use runtime_read::string_literal;
pub(crate) use test_filter::{is_test_file, test_filter, unrun_test_files};

/// A workspace member: its directory relative to the workspace root, its
/// version, and the members it depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) dir: String,
    pub(crate) version: String,
    pub(crate) deps: Vec<String>,
    pub(crate) library: bool,
    /// The integration-test files, relative to the workspace root, whose
    /// `[[test]]` target declares `test = false`: only an explicit
    /// `--test <name>` builds or runs them, so a plain nextest filter
    /// matches nothing in them (`docs/ci.md`, "On every pull request that
    /// changes code").
    pub(crate) unrun_tests: Vec<String>,
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
    /// any crate that compiles in a changed listed file, without dependents,
    /// plus, when the diff changes a first-party package, the crates that
    /// read packages and the binary-level tests, without dependents.
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
/// Whether `path` is a first-party package file: under `providers/` or
/// `extensions/` at the repository root (`docs/ci.md`, "Selection").
/// `crates/extensions/...` is not one.
fn is_package_file(path: &str) -> bool {
    path.starts_with("providers/") || path.starts_with("extensions/")
}

/// The prototype crate that `lint` checks with its own fmt and clippy step
/// (`docs/ci.md`, "Selection"). It is not a workspace member.
const PROTOTYPE_ROOT: &str = "research/tui-prototype/";

fn is_docs_file(path: &str) -> bool {
    path.ends_with(".md") || path.starts_with("docs/") || path.starts_with("research/")
}

/// Whether a pull request changing `files` changes only docs: non-empty
/// and every path is a docs file, never a package file.
pub(crate) fn docs_only(files: &[String]) -> bool {
    !files.is_empty() && files.iter().all(|f| is_docs_file(f) && !is_package_file(f))
}

/// Whether `path` runs every job. `research/` is outside the rule: its
/// manifests are not the workspace's (`docs/ci.md`, "Selection").
fn runs_all(path: &str) -> bool {
    if path.starts_with("research/") {
        return false;
    }
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
    classify_with(files, members, COMPILED_IN, PACKAGE_READERS)
}

/// First-party packages with direct JSON cases, when the binary-level tests
/// are selected (`docs/ci.md`, "Selection").
pub(crate) fn extension_packages(
    selection: &Selection,
    root: &Path,
) -> Result<Option<Vec<String>>, String> {
    if !selection
        .packages()
        .iter()
        .any(|package| package == BINARY_TESTS)
    {
        return Ok(None);
    }

    let mut packages = BTreeSet::new();
    for group in ["providers", "extensions"] {
        let path = root.join(group);
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|error| format!("{}: {error}", path.display()))?;
            if !entry
                .file_type()
                .map_err(|error| format!("{}: {error}", entry.path().display()))?
                .is_dir()
            {
                continue;
            }
            let package = entry.path();
            let tests = package.join("tests");
            let cases = match fs::read_dir(&tests) {
                Ok(cases) => cases,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("{}: {error}", tests.display())),
            };
            let mut has_cases = false;
            for case in cases {
                let case = case.map_err(|error| format!("{}: {error}", tests.display()))?;
                if case
                    .file_type()
                    .map_err(|error| format!("{}: {error}", case.path().display()))?
                    .is_file()
                    && case.path().extension() == Some(std::ffi::OsStr::new("json"))
                {
                    has_cases = true;
                    break;
                }
            }
            if has_cases {
                packages.insert(format!("{group}/{}", entry.file_name().to_string_lossy()));
            }
        }
    }
    Ok(Some(packages.into_iter().collect()))
}

fn classify_with(
    files: &[String],
    members: &Members,
    listed: &[(&str, &str)],
    readers: &[&str],
) -> Selection {
    if files.iter().any(|f| runs_all(f)) {
        return Selection::All(members.keys().cloned().collect());
    }
    // A package file is checked before the docs rule, so a `.md` inside a
    // package is not a docs-only diff (`docs/ci.md`, "Selection").
    let has_package = files.iter().any(|f| is_package_file(f));
    let is_listed = |path: &str| listed.iter().any(|(p, _)| *p == path);
    // A prototype file selects no crate but still runs `lint`, so the diff is
    // `Crates` with no packages, never `Docs`.
    let has_prototype = files.iter().any(|f| f.starts_with(PROTOTYPE_ROOT));
    if !has_package && !has_prototype && files.iter().all(|f| is_docs_file(f) && !is_listed(f)) {
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
    // dependents (`docs/ci.md`, "Selection"). Package files own no crate;
    // they run the package readers and the binary-level tests, below.
    let touched = files
        .iter()
        .filter(|f| !is_package_file(f) && !is_docs_file(f) && !is_listed(f))
        .filter_map(|f| owner(f, members))
        .map(str::to_owned)
        .collect();
    let mut selected = dependents(touched, members);
    selected.extend(compiled);
    if has_package {
        selected.extend(readers.iter().map(|name| (*name).to_owned()));
        selected.insert(BINARY_TESTS.to_owned());
    }
    Selection::Crates(selected.into_iter().collect())
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
