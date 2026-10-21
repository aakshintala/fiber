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

/// Whether `text`, the inside of a string literal, can name a repository
/// path on its own: it holds a `/` but no `:`, so bare event names and
/// URLs never count.
fn is_path_like(text: &str) -> bool {
    text.contains('/') && !text.contains(':')
}

/// Resolve `pieces` against `manifest_dir`, the reading crate's manifest
/// directory, one segment at a time: the repository-relative path when the
/// result stays inside the repository root, else `None`. `..` past the
/// root escapes, so it is not a repository read. A leading slash is only
/// a separator here: each arm decides whether an absolute literal counts
/// before resolving, since string concatenation keeps the base while a
/// file read or a `join` discards it.
fn resolve_in_root(manifest_dir: &str, pieces: &[String]) -> Option<String> {
    let mut parts: Vec<&str> = manifest_dir
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    for piece in pieces {
        for segment in piece.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    parts.pop()?;
                }
                _ => parts.push(segment),
            }
        }
    }
    Some(parts.join("/"))
}

/// Whether a resolved repository path stays with the package-reader check:
/// it names a first-party package at the repository root and the file holds
/// the manifest token, which is exactly what `package_reader_mismatches`
/// claims, so this check leaves it alone and reports it bare. A
/// crate-internal `providers` or `extensions` segment, such as
/// `crates/main/src/providers/file.bin`, is not a root package path, so it
/// stays a run-time read here.
fn yields_to_packages(resolved: &str, has_manifest: bool) -> bool {
    has_manifest
        && (resolved == "providers"
            || resolved == "extensions"
            || resolved.starts_with("providers/")
            || resolved.starts_with("extensions/"))
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

// debt: the walker stays in select.rs near 1000 lines while
// runtime_read_mismatches is its only caller; a second caller splits it
// into select/runtime_read.rs.
/// Failures where a crate's Rust source reads a repository file at run
/// time instead of compiling it in, one line each; empty when every read
/// is compiled in (`docs/ci.md`, "Selection"). A file has a run-time
/// read when its tokens hold, literals only, never comments:
///
/// - `read_to_string`, `read` or `open` directly given a path-like string
///   literal (holding `/` but no `:`) that resolves inside the repository
///   from the reading crate's manifest directory;
/// - `concat!` holding the literal `CARGO_MANIFEST_DIR` and a suffix
///   literal resolving inside the repository from the manifest directory;
/// - `format!` with all-literal arguments rooted at the manifest
///   directory, inside the argument of such a read, every suffix
///   resolving inside the repository;
/// - `join` given a literal naming a joined file, on a base rooted at
///   the manifest directory, inside the argument of such a read: the
///   receiver holds the manifest literal, or holds an identifier an
///   earlier `let` bound to the manifest directory itself, or holds an
///   identifier an earlier `let` bound to a join chain rooted there. A `let`
///   holding such a join reports its literals when a read takes the name,
///   through any number of `let`s. A join onto any other base, a join
///   outside every read, and an absolute or escaping suffix never report.
///
/// A literal nested inside another call is a computed path, not a named
/// one: `read_to_string(home.join("docs/README.md"))` reads a temporary
/// directory. `read_dir` never reports: no include macro compiles a
/// directory in. A `providers/` or `extensions/` path reports only when
/// the file holds no manifest token: with the token the package-reader
/// check claims the read, keeping single ownership there. For crate
/// `xtask` only test files count (their `rel` ends `_tests.rs` or starts
/// `tests/`), because its commands read docs as tools. Err on a file that
/// does not tokenise.
pub(crate) fn runtime_read_mismatches(
    files: &[RustFile],
    members: &Members,
) -> Result<Vec<String>, String> {
    /// The mutable walk state: the bindings in scope, the reading crate's
    /// manifest directory, whether the file holds the manifest token, and
    /// the reported literals.
    struct Walk<'a> {
        root: &'a mut BTreeSet<String>,
        joined: &'a mut BTreeMap<String, Vec<String>>,
        second: &'a mut BTreeMap<String, (Vec<String>, Vec<String>)>,
        manifest_dir: &'a str,
        has_manifest: bool,
        found: &'a mut BTreeSet<String>,
    }
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
    /// The first name in `tokens` bound to a one-level join, searching
    /// inside groups: the base a second join builds on.
    fn joined_mention(
        tokens: &[TokenTree],
        joined: &BTreeMap<String, Vec<String>>,
    ) -> Option<String> {
        tokens.iter().find_map(|tree| match tree {
            TokenTree::Ident(ident) => {
                let name = ident.to_string();
                joined.contains_key(name.as_str()).then_some(name)
            }
            TokenTree::Group(group) => {
                joined_mention(&group.stream().into_iter().collect::<Vec<_>>(), joined)
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => None,
        })
    }
    /// The first name in `tokens` bound to a deeper join chain, searching
    /// inside groups: the base a later join builds on once the chain has
    /// already left the one-level map.
    fn second_mention(
        tokens: &[TokenTree],
        second: &BTreeMap<String, (Vec<String>, Vec<String>)>,
    ) -> Option<String> {
        tokens.iter().find_map(|tree| match tree {
            TokenTree::Ident(ident) => {
                let name = ident.to_string();
                second.contains_key(name.as_str()).then_some(name)
            }
            TokenTree::Group(group) => {
                second_mention(&group.stream().into_iter().collect::<Vec<_>>(), second)
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => None,
        })
    }
    // debt: join_on_joined repeats join_on_root's receiver walk by hand;
    // a third join walker shares the loop.
    /// The chained value of a `let` holding a single top-level `join`
    /// onto a joined name: the full suffix chain from the
    /// manifest directory for the containment check, and the join's own
    /// literals for the report. A second top-level join, a join onto any
    /// other base, and the manifest literal itself as a suffix bind
    /// nothing.
    fn join_on_joined(
        rhs: &[TokenTree],
        joined: &BTreeMap<String, Vec<String>>,
        second: &BTreeMap<String, (Vec<String>, Vec<String>)>,
    ) -> Option<(Vec<String>, Vec<String>)> {
        let mut own = Vec::new();
        let mut base: Option<String> = None;
        let mut joins = 0;
        // The tokens before the current one: the receiver a `join` builds
        // on, as the walk's `preceding` holds it, groups included.
        let mut receiver: Vec<TokenTree> = Vec::new();
        let mut stream = rhs.iter().peekable();
        while let Some(tree) = stream.next() {
            if let TokenTree::Ident(name) = tree
                && name == "join"
                && let Some(TokenTree::Group(group)) = stream.peek()
            {
                joins += 1;
                if base.is_none() {
                    base = joined_mention(&receiver, joined)
                        .or_else(|| second_mention(&receiver, second));
                }
                if base.is_some() {
                    for tree in group.stream() {
                        if let TokenTree::Literal(lit) = tree
                            && let Some(text) = string_literal(&lit.to_string())
                            && is_joined_name(&text)
                            && text != "CARGO_MANIFEST_DIR"
                            // An absolute suffix replaces the receiver.
                            && !text.starts_with('/')
                        {
                            own.push(text);
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
        if joins != 1 {
            return None;
        }
        let base = base?;
        if own.is_empty() {
            return None;
        }
        let mut chain = joined
            .get(&base)
            .cloned()
            .or_else(|| second.get(&base).map(|(full, _)| full.clone()))
            .unwrap_or_default();
        chain.extend(own.clone());
        Some((chain, own))
    }
    /// Whether every identifier in `stream` is `env`: a `format!`
    /// argument list holding any other name takes a runtime value.
    fn all_literal(stream: TokenStream) -> bool {
        stream.into_iter().all(|tree| match tree {
            TokenTree::Ident(ident) => ident == "env",
            TokenTree::Group(group) => all_literal(group.stream()),
            TokenTree::Punct(_) | TokenTree::Literal(_) => true,
        })
    }
    /// The literals a `format!` inside a file read reports: `None` when
    /// the macro takes a runtime value, the template is not rooted at the
    /// manifest directory, or a suffix is absolute or escaping; else every
    /// non-marker suffix. A template must start with the placeholder, so
    /// it resolves with the placeholders removed; a plain suffix resolves
    /// as written, and an absolute one names a system path.
    fn rooted_format(
        group: &proc_macro2::Group,
        manifest_dir: &str,
        has_manifest: bool,
    ) -> Option<Vec<String>> {
        if !all_literal(group.stream()) {
            return None;
        }
        let mut literals = Vec::new();
        all_literals(group.stream(), &mut literals);
        if !literals
            .iter()
            .any(|literal| literal == "CARGO_MANIFEST_DIR")
        {
            return None;
        }
        let mut reported = Vec::new();
        for piece in literals {
            if piece == "CARGO_MANIFEST_DIR" {
                continue;
            }
            if piece.contains("{}") {
                if !piece.starts_with("{}") {
                    return None;
                }
                let probe = piece.replace("{}", "");
                if !is_joined_name(&probe) {
                    continue;
                }
                let resolved = resolve_in_root(manifest_dir, std::slice::from_ref(&probe))?;
                if yields_to_packages(&resolved, has_manifest) {
                    return None;
                }
            } else {
                if piece.starts_with('/') {
                    return None;
                }
                if !is_path_like(&piece) {
                    continue;
                }
                let resolved = resolve_in_root(manifest_dir, std::slice::from_ref(&piece))?;
                if yields_to_packages(&resolved, has_manifest) {
                    return None;
                }
            }
            reported.push(piece);
        }
        (!reported.is_empty()).then_some(reported)
    }
    // debt: format! in a read argument keeps its own windows(3) scan;
    // a second scanned macro moves both into the walk.
    /// The literals a `format!` in a read's argument group reports: only
    /// `format!` calls directly in the group count, so the thousands of
    /// `format!` calls elsewhere never report.
    fn format_literals(group: &proc_macro2::Group, cx: &Walk) -> Vec<String> {
        let tokens: Vec<TokenTree> = group.stream().into_iter().collect();
        let mut out = Vec::new();
        for window in tokens.windows(3) {
            if let [
                TokenTree::Ident(name),
                TokenTree::Punct(mark),
                TokenTree::Group(argument),
            ] = window
                && name == "format"
                && mark.as_char() == '!'
                && let Some(literals) = rooted_format(argument, cx.manifest_dir, cx.has_manifest)
            {
                out.extend(literals);
            }
        }
        out
    }
    // debt: each read arm repeats the junk, marker, absolute, resolve and
    // yield checks by hand; a new arm centralizes them into one helper.
    /// The literals a read reports through a bound name: when the
    /// argument of a read is one name, directly or behind `&` with an
    /// optional trailing comma. A one-level join reports the literals
    /// resolving inside the repository; a deeper join reports its own
    /// literals, the full chain having been contained at binding.
    fn read_name(group: &proc_macro2::Group, cx: &Walk) -> Option<Vec<String>> {
        let mut tokens = group.stream().into_iter().peekable();
        while matches!(tokens.peek(), Some(TokenTree::Punct(mark)) if mark.as_char() == '&') {
            tokens.next();
        }
        let name = match tokens.next() {
            Some(TokenTree::Ident(name)) => name,
            _ => return None,
        };
        match tokens.next() {
            None => {}
            Some(TokenTree::Punct(mark)) if mark.as_char() == ',' && tokens.next().is_none() => {}
            _ => return None,
        }
        if let Some((_, own)) = cx.second.get(&name.to_string()) {
            return Some(own.clone());
        }
        let literals = cx.joined.get(&name.to_string())?;
        let kept: Vec<String> = literals
            .iter()
            .filter(|literal| {
                is_joined_name(literal)
                    && *literal != "CARGO_MANIFEST_DIR"
                    // An absolute suffix replaces the receiver in a join.
                    && !literal.starts_with('/')
                    && resolve_in_root(cx.manifest_dir, std::slice::from_ref(literal))
                        .is_some_and(|resolved| !yields_to_packages(&resolved, cx.has_manifest))
            })
            .cloned()
            .collect();
        (!kept.is_empty()).then_some(kept)
    }
    /// If `stream` holds a plain binding next (`mut`, a name, an
    /// optional `: <type>`, `=`), rebind the name to its value through
    /// the terminating `;`: a value joining onto the manifest directory
    /// binds the name to the joined literals; a value joining onto a
    /// joined name binds the name to the latest join's literals
    /// when the full chain stays inside the repository; a value holding
    /// the manifest literal and no `join` binds the name to the manifest
    /// directory itself; any other value unbinds the name. Walk the
    /// value either way. Anything else stays in the stream for the
    /// caller.
    fn bind(stream: &mut Peekable<std::vec::IntoIter<TokenTree>>, cx: &mut Walk, in_read: bool) {
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
        // Walk the value with the old bindings first: Rust resolves a
        // shadowing `let`'s value to the previous bindings, so the
        // value must report through them before the new one replaces
        // them.
        let shadowed = join_on_root(&rhs, &*cx.root);
        let chained = if shadowed.is_none() {
            join_on_joined(&rhs, &*cx.joined, &*cx.second)
        } else {
            None
        };
        let rooted = is_manifest_rooted(&rhs)
            && !rhs
                .iter()
                .any(|tree| matches!(tree, TokenTree::Ident(join) if join == "join"));
        walk(rhs, cx, in_read);
        if let Some(literals) = shadowed {
            cx.root.remove(name.as_str());
            cx.second.remove(name.as_str());
            cx.joined.insert(name, literals);
        } else if let Some((chain, own)) = chained {
            // The full chain must resolve inside the repository and stay
            // out of the package reader's paths: the report names only
            // the latest join's literals, which no longer carry the base.
            let contained = resolve_in_root(cx.manifest_dir, &chain)
                .is_some_and(|resolved| !yields_to_packages(&resolved, cx.has_manifest));
            cx.root.remove(name.as_str());
            cx.joined.remove(name.as_str());
            if contained {
                cx.second.insert(name, (chain, own));
            } else {
                cx.second.remove(name.as_str());
            }
        } else if rooted {
            cx.joined.remove(name.as_str());
            cx.second.remove(name.as_str());
            cx.root.insert(name);
        } else {
            cx.root.remove(name.as_str());
            cx.joined.remove(name.as_str());
            cx.second.remove(name.as_str());
        }
    }
    /// Walk a group's tokens with the bindings scoped to it: bindings
    /// made inside do not leak out, and an inner `let` never changes
    /// the outer set. The one place groups enter a fresh scope.
    fn walk_group(group: &proc_macro2::Group, cx: &mut Walk, in_read: bool) {
        let tokens: Vec<TokenTree> = group.stream().into_iter().collect();
        walk(
            tokens,
            &mut Walk {
                root: &mut cx.root.clone(),
                joined: &mut cx.joined.clone(),
                second: &mut cx.second.clone(),
                manifest_dir: cx.manifest_dir,
                has_manifest: cx.has_manifest,
                found: &mut *cx.found,
            },
            in_read,
        );
    }
    fn walk(tokens: Vec<TokenTree>, cx: &mut Walk, in_read: bool) {
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
                    bind(&mut stream, cx, in_read);
                    preceding.clear();
                }
                TokenTree::Ident(ident)
                    if ident == "read_to_string" || ident == "read" || ident == "open" =>
                {
                    if matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        for tree in group.stream() {
                            if let TokenTree::Literal(lit) = tree
                                && let Some(text) = string_literal(&lit.to_string())
                                && is_joined_name(&text)
                                && is_path_like(&text)
                                // An absolute path names no repository file:
                                // the read takes no base.
                                && !text.starts_with('/')
                                && let Some(resolved) =
                                    resolve_in_root(cx.manifest_dir, std::slice::from_ref(&text))
                                && !yields_to_packages(&resolved, cx.has_manifest)
                            {
                                cx.found.insert(text);
                            }
                        }
                        if let Some(literals) = read_name(&group, cx) {
                            cx.found.extend(literals);
                        }
                        cx.found.extend(format_literals(&group, cx));
                        walk_group(&group, cx, true);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Ident(ident) if ident == "read_dir" => {
                    if matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        // Directory reads never report: no include macro
                        // compiles a directory in.
                        walk_group(&group, cx, false);
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
                            for literal in literals {
                                if literal == "CARGO_MANIFEST_DIR"
                                    || !is_joined_name(&literal)
                                    // A bare word is a computed value, not a
                                    // suffix: the block test pins `"x"`.
                                    || !is_path_like(&literal)
                                {
                                    continue;
                                }
                                if let Some(resolved) =
                                    resolve_in_root(cx.manifest_dir, std::slice::from_ref(&literal))
                                    && !yields_to_packages(&resolved, cx.has_manifest)
                                {
                                    cx.found.insert(literal);
                                }
                            }
                        }
                        walk_group(&group, cx, in_read);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Ident(ident) if ident == "join" => {
                    if matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        if in_read {
                            let base: Option<Vec<String>> = if is_manifest_rooted(&preceding)
                                || holds_root(&preceding, &*cx.root)
                            {
                                Some(Vec::new())
                            } else if let Some(name) = joined_mention(&preceding, &*cx.joined) {
                                cx.joined.get(&name).cloned()
                            } else {
                                second_mention(&preceding, &*cx.second).and_then(|name| {
                                    cx.second.get(&name).map(|(full, _)| full.clone())
                                })
                            };
                            if let Some(chain) = base {
                                let mut own = Vec::new();
                                for tree in group.stream() {
                                    if let TokenTree::Literal(lit) = tree
                                        && let Some(text) = string_literal(&lit.to_string())
                                        && is_joined_name(&text)
                                        && text != "CARGO_MANIFEST_DIR"
                                        // A join on an absolute path replaces
                                        // the receiver.
                                        && !text.starts_with('/')
                                    {
                                        own.push(text);
                                    }
                                }
                                let mut full = chain;
                                full.extend(own.clone());
                                if !own.is_empty()
                                    && let Some(resolved) = resolve_in_root(cx.manifest_dir, &full)
                                    && !yields_to_packages(&resolved, cx.has_manifest)
                                {
                                    cx.found.extend(own);
                                }
                            }
                        }
                        walk_group(&group, cx, in_read);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Group(group) => {
                    walk_group(&group, cx, in_read);
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
        let member = &members[&f.krate];
        let mut manifest_literals = Vec::new();
        all_literals(source.clone(), &mut manifest_literals);
        let has_manifest = manifest_literals
            .iter()
            .any(|literal| literal == "CARGO_MANIFEST_DIR");
        let mut root = BTreeSet::new();
        let mut joined = BTreeMap::new();
        let mut second = BTreeMap::new();
        let mut found = BTreeSet::new();
        walk(
            source.into_iter().collect(),
            &mut Walk {
                root: &mut root,
                joined: &mut joined,
                second: &mut second,
                manifest_dir: &member.dir,
                has_manifest,
                found: &mut found,
            },
            false,
        );
        failures.extend(found.into_iter().map(|literal| {
            format!(
                "{}: reads {literal} at run time; compile it in with include_str! or include_bytes!",
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
