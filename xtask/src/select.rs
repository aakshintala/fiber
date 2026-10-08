//! What a diff runs (`docs/ci.md`, "Selection"), which CI jobs that means,
//! and the verdict behind the `CI` check.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
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
/// Files a crate compiles in that the selector would not otherwise route to
/// that crate: Markdown anywhere, and every file outside the crate
/// (`docs/ci.md`, "Selection").
const COMPILED_IN: &[(&str, &str)] = &[
    ("docs/errors.md", "contract"),
    ("docs/events.md", "contract"),
    ("docs/invocation.md", "contract"),
    ("docs/tui.md", "contract"),
    ("crates/loop/prompt/messages.md", "loop"),
    ("crates/loop/prompt/opening.md", "loop"),
    ("crates/loop/prompt/reviewer.md", "loop"),
    ("crates/loop/prompt/system.md", "loop"),
    ("docs/skills/cache-warming/SKILL.md", "loop"),
    ("docs/skills/using-fiber/SKILL.md", "loop"),
    ("crates/tools/prompt/guidelines.md", "tools"),
];
/// `docs/ci.md`: mutation testing runs as 6 shards.
const MUTANT_SHARDS: u64 = 6;
/// Crates whose tests read a first-party package under `providers/` or
/// `extensions/` (`docs/ci.md`, "Selection"); sorted. Checked against the
/// sources by `package_reader_mismatches`.
const PACKAGE_READERS: &[&str] = &["config", "extensions", "main"];
/// The package whose tests are the binary-level tests (`docs/ci.md`,
/// "Selection"): the `fiber` binary.
const BINARY_TESTS: &str = "main";
/// Whether `path` is a first-party package file: under `providers/` or
/// `extensions/` at the repository root (`docs/ci.md`, "Selection").
/// `crates/extensions/...` is not one.
fn is_package_file(path: &str) -> bool {
    path.starts_with("providers/") || path.starts_with("extensions/")
}

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

/// Whether Rust `source` reads a first-party package: its tokens hold
/// `CARGO_MANIFEST_DIR` and a string literal one of whose `/`-separated
/// segments is exactly `providers` or `extensions`. Literals only, never
/// comments: proc-macro2 tokenises, so comments never appear as tokens.
/// Over-detects (for example `"crates/extensions"` counts), which only
/// adds a crate to the list, so it fails safe.
fn reads_package(source: &str) -> Result<bool, proc_macro2::LexError> {
    fn walk(stream: TokenStream, manifest: &mut bool, package: &mut bool) {
        for tree in stream {
            match tree {
                TokenTree::Literal(lit) => {
                    if let Some(text) = string_literal(&lit.to_string()) {
                        if text == "CARGO_MANIFEST_DIR" {
                            *manifest = true;
                        } else if text
                            .split('/')
                            .any(|segment| segment == "providers" || segment == "extensions")
                        {
                            *package = true;
                        }
                    }
                }
                TokenTree::Group(group) => walk(group.stream(), manifest, package),
                TokenTree::Ident(_) | TokenTree::Punct(_) => {}
            }
        }
    }
    let mut manifest = false;
    let mut package = false;
    walk(source.parse()?, &mut manifest, &mut package);
    Ok(manifest && package)
}

/// Failures where the package-reader list and the sources disagree, one
/// line each; empty when they agree. A Rust file in a workspace member
/// reads a first-party package when `reads_package` says so; crate `xtask`
/// never counts (its own tests hold such literals as data). Err on a file
/// that does not tokenise.
pub(crate) fn package_reader_mismatches(
    files: &[RustFile],
    members: &Members,
) -> Result<Vec<String>, String> {
    let mut found = BTreeSet::new();
    for f in files {
        if f.krate == "xtask" || !members.contains_key(&f.krate) {
            continue;
        }
        if reads_package(&f.source)
            .map_err(|e| format!("{}: does not tokenise as Rust: {e}", f.path))?
        {
            found.insert(f.krate.clone());
        }
    }
    let listed: BTreeSet<String> = PACKAGE_READERS
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut failures: Vec<String> = found
        .difference(&listed)
        .map(|krate| {
            format!(
                "{krate}: its sources read a first-party package, but the package-reader list does not list it"
            )
        })
        .collect();
    failures.extend(listed.difference(&found).map(|krate| {
        format!("{krate}: listed as reading a first-party package, but no source reads one")
    }));
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
) -> Result<Vec<String>, String> {
    if !selection
        .packages()
        .iter()
        .any(|package| package == BINARY_TESTS)
    {
        return Ok(Vec::new());
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
    Ok(packages.into_iter().collect())
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
    if !has_package && files.iter().all(|f| is_docs_file(f) && !is_listed(f)) {
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

/// Which CI jobs run, by the job ids in `.github/workflows/ci.yml`, and how
/// many mutant shards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) jobs: BTreeMap<&'static str, bool>,
    pub(crate) shards: u64,
}

pub(crate) fn plan(mode: &str, packages: &[String], event: &str, bug: bool, mutants: bool) -> Plan {
    let pr = event == "pull_request";
    let code = mode != "docs";
    let shards = if code && mutants { MUTANT_SHARDS } else { 0 };
    let jobs = BTreeMap::from([
        ("lint", !pr || code),
        // The backstop on `main` compiles the whole workspace on every push.
        ("test", !pr || !packages.is_empty()),
        ("mutants", shards > 0),
        ("bug_red", pr && code && bug),
        // Runs with the binary-level tests, and on every push (`docs/ci.md`, "Selection").
        ("release", !pr || packages.iter().any(|p| p == BINARY_TESTS)),
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

/// Every issue a pull request body resolves, in order of appearance, from
/// "Resolves #N" lines and GitHub's other closing keywords.
pub(crate) fn ticket(body: &str) -> Vec<String> {
    let mut numbers = Vec::new();
    let mut prev = ' ';
    for (i, c) in body.char_indices() {
        if c.is_alphabetic()
            && !is_word(prev)
            && let Some(number) = ticket_at(body.get(i..).unwrap_or_default())
        {
            numbers.push(number);
        }
        prev = c;
    }
    numbers
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

/// The extra module segment a unit-test file's tests sit in, when that is
/// not its directory's own module (`docs/code-quality.md`, "Size"):
/// `foo_tests.rs` holds `foo::tests`, while `tests.rs` and, at the crate
/// root, `lib_tests.rs` and `main_tests.rs` hold `tests`.
fn test_module(root: bool, stem: &str) -> Option<&str> {
    match stem.strip_suffix("_tests") {
        None => None,
        Some("lib" | "main") if root => None,
        Some(module) => Some(module),
    }
}

/// A top-level `#[path = "<lit>"] mod <ident>;` declaration: the file the
/// literal resolves to (relative to the repository, `/` separators), with
/// the file that declares it and the declared module name.
type DeclarerIdent = (String, String);
type PathDecl = (String, DeclarerIdent);

/// The `<lit>` of a `path = <lit>` item in an attribute's tokens, if it
/// holds one. Other attributes, and items without a literal, give nothing.
fn path_attr(stream: TokenStream) -> Option<String> {
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    for window in tokens.windows(3) {
        if let [
            TokenTree::Ident(name),
            TokenTree::Punct(eq),
            TokenTree::Literal(lit),
        ] = window
            && name == "path"
            && eq.as_char() == '='
        {
            return string_literal(&lit.to_string());
        }
    }
    None
}

/// Every top-level `#[path = "<lit>"] mod <ident>;` declaration in the
/// Rust file at `path`, with `<lit>` resolved relative to its directory
/// (the same join `compiled_in_mismatches` uses). Attributes may be
/// interleaved with `#[cfg(test)]` and the like; a declaration inside an
/// inline `mod x { ... }` block is ignored, so brace groups are never
/// scanned. A file that does not tokenise declares nothing: its test
/// files keep the conventional filter.
fn path_decls(path: &str, source: &str) -> Vec<PathDecl> {
    let stream: TokenStream = match source.parse() {
        Ok(stream) => stream,
        Err(_) => return Vec::new(),
    };
    let dir = Path::new(path).parent().unwrap_or(Path::new(""));
    let mut tokens = stream.into_iter().peekable();
    let mut decls = Vec::new();
    let mut pending: Option<String> = None;
    // A token the `mod` lookahead consumed but did not use: the loop
    // reprocesses it next, so `mod mod x;` still sees its second `mod`.
    let mut pushback: Option<TokenTree> = None;
    loop {
        let tree = pushback.take().or_else(|| tokens.next());
        let Some(tree) = tree else {
            break;
        };
        match tree {
            TokenTree::Punct(hash) if hash.as_char() == '#' => match tokens.peek() {
                Some(TokenTree::Group(group))
                    if group.delimiter() == proc_macro2::Delimiter::Bracket =>
                {
                    if let Some(TokenTree::Group(group)) = tokens.next()
                        && let Some(lit) = path_attr(group.stream())
                    {
                        pending = Some(lit);
                    }
                }
                None | Some(_) => {}
            },
            TokenTree::Ident(keyword) if keyword == "mod" => match tokens.next() {
                Some(TokenTree::Ident(ident)) => match tokens.peek() {
                    Some(TokenTree::Punct(semi)) if semi.as_char() == ';' => {
                        tokens.next();
                        if let Some(lit) = pending.take() {
                            decls.push((
                                crate::docs::join(dir, &lit)
                                    .to_string_lossy()
                                    .replace('\\', "/"),
                                (path.to_owned(), ident.to_string()),
                            ));
                        }
                    }
                    None | Some(_) => {
                        pending = None;
                        pushback = Some(TokenTree::Ident(ident));
                    }
                },
                other => {
                    pending = None;
                    pushback = other;
                }
            },
            // Any other item ends the attribute run a pending `#[path]`
            // belonged to, so a `#[path]` on a non-`mod` item registers
            // nothing. Any punctuation but `#` and any brace group clear
            // it: neither can sit between an attribute and its `mod`, while
            // a paren group is the `(crate)` of `pub(crate) mod`.
            TokenTree::Punct(_) => pending = None,
            TokenTree::Group(body) if body.delimiter() == proc_macro2::Delimiter::Brace => {
                pending = None;
            }
            TokenTree::Group(_) | TokenTree::Ident(_) | TokenTree::Literal(_) => {}
        }
    }
    decls
}

/// The module path a file's location under `src/` gives it: `lib.rs` and
/// `main.rs` at the root are the crate root, `a/mod.rs` is `a`, `a/b.rs`
/// is `a::b`. A file under `examples/<name>/` is placed the same way within
/// that example. `None` when the file sits outside both.
fn conventional_module(path: &str, members: &Members) -> Option<String> {
    let name = owner(path, members)?;
    let member = members.get(name)?;
    let rel = path.get(member.dir.len() + 1..)?;
    // An example directory is its own crate root, `main.rs` its root file.
    let modules = match rel.strip_prefix("examples/") {
        Some(example) => example.split_once('/')?.1,
        None => rel.strip_prefix("src/")?,
    };
    let parts: Vec<&str> = modules.split('/').collect();
    match parts.as_slice() {
        [file] if *file == "lib.rs" || *file == "main.rs" => Some(String::new()),
        [file] => Some(file.strip_suffix(".rs").unwrap_or(file).to_owned()),
        [dirs @ .., file] if *file == "mod.rs" => Some(dirs.join("::")),
        [dirs @ .., file] => {
            let mut module = dirs.join("::");
            if !module.is_empty() {
                module.push_str("::");
            }
            module.push_str(file.strip_suffix(".rs").unwrap_or(file));
            Some(module)
        }
        // `split` never yields no parts; the arm is only for the compiler.
        [] => None,
    }
}

/// The module path the file at `path` declares its items in: through the
/// `#[path]` declaration naming it when exactly one does (resolved
/// recursively, so a declared file declaring further files chains), else
/// from its location. `None` on a declaration cycle or outside `src/`.
fn module_path(
    path: &str,
    members: &Members,
    by_target: &BTreeMap<String, Vec<DeclarerIdent>>,
    stack: &mut Vec<String>,
) -> Option<String> {
    if stack.iter().any(|seen| seen == path) {
        return None;
    }
    stack.push(path.to_owned());
    let result = match by_target.get(path).map(Vec::as_slice) {
        Some([(declarer, ident)]) => {
            let (declarer, ident) = (declarer.clone(), ident.clone());
            module_path(declarer.as_str(), members, by_target, stack).map(|parent| {
                if parent.is_empty() {
                    ident
                } else {
                    format!("{parent}::{ident}")
                }
            })
        }
        // No declaration, or two naming one file: the file's location.
        _ => conventional_module(path, members),
    };
    stack.pop();
    result
}

/// The test-id prefix of the tests in the test file at `path`, whose
/// directories under its binary's root are `modules` and whose name is
/// `file`: the module a top-level `#[path]` declaration gives it, else the
/// module its location gives it.
fn test_prefix(
    path: &str,
    modules: &[&str],
    file: &str,
    members: &Members,
    by_target: &BTreeMap<String, Vec<DeclarerIdent>>,
) -> String {
    let declared = match by_target.get(path).map(Vec::as_slice) {
        Some([_]) => {
            let mut stack = Vec::new();
            module_path(path, members, by_target, &mut stack).map(|module| format!("{module}::"))
        }
        _ => None,
    };
    declared.unwrap_or_else(|| {
        let stem = file.strip_suffix(".rs").unwrap_or(file);
        let module = test_module(modules.is_empty(), stem);
        modules
            .iter()
            .copied()
            .chain(module)
            .chain(["tests"])
            .map(|m| format!("{m}::"))
            .collect()
    })
}

/// The nextest filter selecting every test in the test files among
/// `files`, and the packages that own them. `sources` is every Rust file
/// in the workspace: a `src/` test file a top-level `#[path]`
/// declaration names maps to the module path that declares it.
pub(crate) fn test_filter(
    files: &[String],
    members: &Members,
    sources: &[RustFile],
) -> (String, Vec<String>) {
    let mut by_target: BTreeMap<String, Vec<DeclarerIdent>> = BTreeMap::new();
    for f in sources {
        for (target, declarer) in path_decls(&f.path, &f.source) {
            by_target.entry(target).or_default().push(declarer);
        }
    }
    let mut terms = Vec::new();
    let mut packages = BTreeSet::new();
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
        let term = match parts.as_slice() {
            // Only a file directly in `tests/` is a binary; a file in a
            // subdirectory of `tests/` is a module the binaries include,
            // so it adds no term of its own.
            ["tests", binary] => {
                let binary = binary.strip_suffix(".rs").unwrap_or(binary);
                format!("binary_id({name}::{binary})")
            }
            ["src", modules @ .., file] => {
                let prefix = test_prefix(path, modules, file, members, &by_target);
                format!("(package({name}) & test(/^{prefix}/))")
            }
            // An example directory is a binary of its own, named
            // `<package>::example/<name>`.
            ["examples", example, modules @ .., file] => {
                let prefix = test_prefix(path, modules, file, members, &by_target);
                format!("(binary_id({name}::example/{example}) & test(/^{prefix}/))")
            }
            _ => continue,
        };
        terms.push(term);
        packages.insert(name.to_owned());
    }
    (terms.join(" | "), packages.into_iter().collect())
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
