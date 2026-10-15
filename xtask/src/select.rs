//! What a diff runs (`docs/ci.md`, "Selection"), which CI jobs that means,
//! and the verdict behind the `CI` check.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::iter::Peekable;
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};

use crate::rules::RustFile;

mod compiled_in;
mod plan;
mod test_filter;

use compiled_in::{COMPILED_IN, PACKAGE_READERS};
pub(crate) use compiled_in::{compiled_in_mismatches, package_reader_mismatches};
use plan::BINARY_TESTS;
pub(crate) use plan::{base_rows_skipped, plan, shard_timeout_minutes, ten_turns_runs, verdict};
pub(crate) use test_filter::{is_test_file, test_filter};

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

/// Whether `text`, the inside of a string literal, names a repository
/// path: one of its `/`-separated segments is exactly `docs` or `prompt`.
/// A partial segment never counts: `"mydocs/x"` and `"docs.md"` are not
/// repository paths, while `"../docs/ci.md"` is.
fn is_repo_path(text: &str) -> bool {
    text.split('/')
        .any(|segment| segment == "docs" || segment == "prompt")
}

/// Whether `text`, the inside of a string literal joined onto a path,
/// names a file: any non-empty literal counts but the junk `.`, `..`
/// and `/`, which name no file on their own.
fn is_joined_name(text: &str) -> bool {
    !text.is_empty() && text != "." && text != ".." && text != "/"
}

/// Whether `tokens` root a path at the manifest directory: they hold
/// the literal `CARGO_MANIFEST_DIR`, such as through `env!` inside
/// `Path::new`. A join onto any other base, such as a temporary
/// directory, is not a read of the repository.
fn is_manifest_rooted(tokens: &[TokenTree]) -> bool {
    tokens.iter().any(|tree| match tree {
        TokenTree::Literal(lit) => {
            string_literal(&lit.to_string()).as_deref() == Some("CARGO_MANIFEST_DIR")
        }
        TokenTree::Group(group) => {
            is_manifest_rooted(&group.stream().into_iter().collect::<Vec<_>>())
        }
        TokenTree::Ident(_) | TokenTree::Punct(_) => false,
    })
}

/// Whether `ch` ends the path expression a `join` builds on: the
/// receiver runs back to the nearest one of these, so a manifest
/// literal past it belongs to another expression. Bare brackets never
/// appear (every bracketed span is a Group), so only these count; `=>`
/// ends at its `=`.
fn is_receiver_boundary(ch: char) -> bool {
    matches!(ch, ';' | '=' | ',')
}

/// Whether `tokens` hold an identifier bound to the manifest directory
/// itself by an earlier `let`, never one bound to a join on it.
fn holds_root(tokens: &[TokenTree], root: &BTreeSet<String>) -> bool {
    tokens.iter().any(|tree| match tree {
        TokenTree::Ident(ident) => root.contains(ident.to_string().as_str()),
        TokenTree::Group(group) => {
            holds_root(&group.stream().into_iter().collect::<Vec<_>>(), root)
        }
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

/// Walk a group's tokens with the bindings scoped to it: bindings made
/// inside do not leak out, and an inner `let` never changes the outer
/// set. The one place groups enter a fresh scope.
fn all_literals(stream: TokenStream, out: &mut Vec<String>) {
    for tree in stream {
        match tree {
            TokenTree::Literal(lit) => {
                if let Some(text) = string_literal(&lit.to_string()) {
                    out.push(text);
                }
            }
            TokenTree::Group(group) => all_literals(group.stream(), out),
            TokenTree::Ident(_) | TokenTree::Punct(_) => {}
        }
    }
}

/// Failures where a crate's Rust source reads a repository file at run
/// time instead of compiling it in, one line each; empty when every read
/// is compiled in (`docs/ci.md`, "Selection"). A file has a run-time
/// read when its tokens hold, literals only, never comments:
///
/// - `read_to_string`, `read`, `read_dir` or `open` directly given a string
///   literal naming a repository path;
/// - `concat!` holding the literal `CARGO_MANIFEST_DIR` and a literal
///   naming a repository path;
/// - `join` given a literal naming a joined file, on a base rooted at
///   the manifest directory, inside the argument of `read_to_string`,
///   `read`, `read_dir` or `open`: the receiver holds the manifest
///   literal, or holds an identifier an earlier `let` bound to the
///   manifest directory itself. A `let` holding such a join reports
///   its literals when a read takes the name. A join onto any other
///   base, a join outside every read, and a second join onto a joined
///   name never report.
///
/// A literal nested inside another call is a computed path, not a named
/// one: `read_to_string(home.join("docs/README.md"))` reads a temporary
/// directory, as does a `join` onto any base but a manifest-rooted one.
/// `providers/` and `extensions/` reads are detected
/// by `package_reader_mismatches` with `PACKAGE_READERS`, so this check
/// looks only at `docs` and `prompt` segments, agreeing with that list
/// without duplicating it. For crate `xtask` only test files count (their
/// `rel` ends `_tests.rs` or starts `tests/`), because its commands read
/// docs as tools. Err on a file that does not tokenise.
pub(crate) fn runtime_read_mismatches(
    files: &[RustFile],
    members: &Members,
) -> Result<Vec<String>, String> {
    /// The literals a top-level `join` call in `rhs` joins onto the
    /// manifest directory or onto a name bound to it: the value a `let`
    /// holding such a join reports when a read takes its name. A join
    /// nested inside another call's argument, such as a read's own
    /// argument, binds nothing: the read reports it inline instead.
    fn join_on_root(rhs: &[TokenTree], root: &BTreeSet<String>) -> Option<Vec<String>> {
        let mut literals = Vec::new();
        // The tokens before the current one: the receiver a `join` builds
        // on, as the walk's `preceding` holds it, groups included.
        let mut receiver: Vec<TokenTree> = Vec::new();
        let mut stream = rhs.iter().peekable();
        while let Some(tree) = stream.next() {
            if let TokenTree::Ident(name) = tree
                && name == "join"
                && let Some(TokenTree::Group(group)) = stream.peek()
            {
                if is_manifest_rooted(&receiver) || holds_root(&receiver, root) {
                    for tree in group.stream() {
                        if let TokenTree::Literal(lit) = tree
                            && let Some(text) = string_literal(&lit.to_string())
                            && is_joined_name(&text)
                        {
                            literals.push(text);
                        }
                    }
                }
                // The group belongs to this `join`: skipping it keeps the
                // next receiver aligned with the token stream.
                stream.next();
                receiver.push(tree.clone());
                receiver.push(TokenTree::Group((*group).clone()));
            } else {
                receiver.push(tree.clone());
            }
        }
        (!literals.is_empty()).then_some(literals)
    }
    /// The literals a read reports through a bound name: when the
    /// argument of a read is one name, directly or behind `&`, holding
    /// a one-level join, that join's literals.
    fn read_name(
        group: &proc_macro2::Group,
        joined: &BTreeMap<String, Vec<String>>,
    ) -> Option<Vec<String>> {
        let mut tokens = group.stream().into_iter().peekable();
        while matches!(tokens.peek(), Some(TokenTree::Punct(mark)) if mark.as_char() == '&') {
            tokens.next();
        }
        match (tokens.next(), tokens.next()) {
            (Some(TokenTree::Ident(name)), None) => joined.get(&name.to_string()).cloned(),
            _ => None,
        }
    }
    /// If `stream` holds a plain binding next (`mut`, a name, an
    /// optional `: <type>`, `=`), rebind the name to its value through
    /// the terminating `;`: a value joining onto the manifest directory
    /// binds the name to the joined literals; a value holding the
    /// manifest literal and no `join` binds the name to the manifest
    /// directory itself; any other value unbinds the name. Walk the
    /// value either way. Anything else stays in the stream for the
    /// caller.
    fn bind(
        stream: &mut Peekable<std::vec::IntoIter<TokenTree>>,
        root: &mut BTreeSet<String>,
        joined: &mut BTreeMap<String, Vec<String>>,
        found: &mut BTreeSet<String>,
        in_read: bool,
    ) {
        if matches!(stream.peek(), Some(TokenTree::Ident(name)) if name == "mut")
            && stream.next().is_none()
        {
            return;
        }
        let name = match stream.peek() {
            Some(TokenTree::Ident(name)) => name.to_string(),
            Some(TokenTree::Group(_) | TokenTree::Punct(_) | TokenTree::Literal(_)) | None => {
                return;
            }
        };
        if stream.next().is_none() {
            return;
        }
        if matches!(stream.peek(), Some(TokenTree::Punct(p)) if p.as_char() == ':') {
            if stream.next().is_none() {
                return;
            }
            loop {
                match stream.peek() {
                    None => return,
                    Some(TokenTree::Punct(p)) if p.as_char() == '=' => break,
                    Some(TokenTree::Punct(p)) if p.as_char() == ';' => return,
                    Some(
                        TokenTree::Group(_)
                        | TokenTree::Ident(_)
                        | TokenTree::Punct(_)
                        | TokenTree::Literal(_),
                    ) => {}
                }
                if stream.next().is_none() {
                    return;
                }
            }
        }
        if !matches!(stream.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '=') {
            return;
        }
        if stream.next().is_none() {
            return;
        }
        let mut rhs = Vec::new();
        for tree in stream.by_ref() {
            if let TokenTree::Punct(punct) = &tree
                && punct.as_char() == ';'
            {
                break;
            }
            rhs.push(tree);
        }
        if let Some(literals) = join_on_root(&rhs, root) {
            root.remove(name.as_str());
            joined.insert(name, literals);
        } else if is_manifest_rooted(&rhs)
            && !rhs
                .iter()
                .any(|tree| matches!(tree, TokenTree::Ident(join) if join == "join"))
        {
            joined.remove(name.as_str());
            root.insert(name);
        } else {
            root.remove(name.as_str());
            joined.remove(name.as_str());
        }
        walk(rhs, root, joined, found, in_read);
    }
    /// Walk a group's tokens with the bindings scoped to it: bindings
    /// made inside do not leak out, and an inner `let` never changes
    /// the outer set. The one place groups enter a fresh scope.
    fn walk_group(
        group: &proc_macro2::Group,
        root: &mut BTreeSet<String>,
        joined: &mut BTreeMap<String, Vec<String>>,
        found: &mut BTreeSet<String>,
        in_read: bool,
    ) {
        let tokens: Vec<TokenTree> = group.stream().into_iter().collect();
        walk(
            tokens,
            &mut root.clone(),
            &mut joined.clone(),
            found,
            in_read,
        );
    }
    fn walk(
        tokens: Vec<TokenTree>,
        root: &mut BTreeSet<String>,
        joined: &mut BTreeMap<String, Vec<String>>,
        found: &mut BTreeSet<String>,
        in_read: bool,
    ) {
        let mut stream = tokens.into_iter().peekable();
        // The current expression back to its nearest boundary: the
        // receiver a `join` builds on.
        let mut preceding: Vec<TokenTree> = Vec::new();
        loop {
            let Some(tree) = stream.next() else {
                break;
            };
            match tree {
                TokenTree::Punct(punct) if is_receiver_boundary(punct.as_char()) => {
                    preceding.clear();
                }
                TokenTree::Ident(ident) if ident == "let" => {
                    bind(&mut stream, root, joined, found, in_read);
                    preceding.clear();
                }
                TokenTree::Ident(ident)
                    if ident == "read_to_string"
                        || ident == "read"
                        || ident == "read_dir"
                        || ident == "open" =>
                {
                    if matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        for tree in group.stream() {
                            if let TokenTree::Literal(lit) = tree
                                && let Some(text) = string_literal(&lit.to_string())
                                && is_repo_path(&text)
                            {
                                found.insert(text);
                            }
                        }
                        if let Some(literals) = read_name(&group, joined) {
                            found.extend(literals);
                        }
                        walk_group(&group, &mut *root, &mut *joined, found, true);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Ident(ident) if ident == "concat" => {
                    if matches!(stream.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '!')
                        && stream.next().is_some()
                        && matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        let mut literals = Vec::new();
                        all_literals(group.stream(), &mut literals);
                        if literals
                            .iter()
                            .any(|literal| literal == "CARGO_MANIFEST_DIR")
                        {
                            found.extend(
                                literals.into_iter().filter(|literal| is_repo_path(literal)),
                            );
                        }
                        walk_group(&group, &mut *root, &mut *joined, found, in_read);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Ident(ident) if ident == "join" => {
                    if matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        if in_read
                            && (is_manifest_rooted(&preceding) || holds_root(&preceding, root))
                        {
                            for tree in group.stream() {
                                if let TokenTree::Literal(lit) = tree
                                    && let Some(text) = string_literal(&lit.to_string())
                                    && is_joined_name(&text)
                                {
                                    found.insert(text);
                                }
                            }
                        }
                        walk_group(&group, &mut *root, &mut *joined, found, in_read);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Group(group) => {
                    walk_group(&group, &mut *root, &mut *joined, found, in_read);
                    preceding.push(TokenTree::Group(group));
                }
                TokenTree::Ident(ident) => preceding.push(TokenTree::Ident(ident)),
                TokenTree::Punct(punct) => preceding.push(TokenTree::Punct(punct)),
                TokenTree::Literal(lit) => preceding.push(TokenTree::Literal(lit)),
            }
        }
    }
    let mut failures = Vec::new();
    for f in files {
        if !members.contains_key(&f.krate) {
            continue;
        }
        if f.krate == "xtask" && !(f.rel.ends_with("_tests.rs") || f.rel.starts_with("tests/")) {
            continue;
        }
        let source: TokenStream = f
            .source
            .parse()
            .map_err(|e| format!("{}: does not tokenise as Rust: {e}", f.path))?;
        let mut root = BTreeSet::new();
        let mut joined = BTreeMap::new();
        let mut found = BTreeSet::new();
        walk(
            source.into_iter().collect(),
            &mut root,
            &mut joined,
            &mut found,
            false,
        );
        failures.extend(found.into_iter().map(|literal| {
            format!(
                "{}: reads {literal} at run time; compile it in with include_str!",
                f.path
            )
        }));
    }
    Ok(failures)
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
