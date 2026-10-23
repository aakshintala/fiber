//! Path and token helpers for the run-time-read scanner.

use crate::select::Members;
use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn string_literal(text: &str) -> Option<String> {
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
pub(super) fn is_path_like(text: &str) -> bool {
    text.contains('/') && !text.contains(':')
}

/// Resolve `pieces` against `manifest_dir`, the reading crate's manifest
/// directory, one segment at a time: the repository-relative path when the
/// result stays inside the repository root, else `None`. `..` past the
/// root escapes, so it is not a repository read. A leading slash is only
/// a separator here: each arm decides whether an absolute literal counts
/// before resolving, since string concatenation keeps the base while a
/// file read or a `join` discards it.
pub(super) fn resolve_in_root(manifest_dir: &str, pieces: &[String]) -> Option<String> {
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
pub(super) fn yields_to_packages(resolved: &str, has_manifest: bool) -> bool {
    has_manifest
        && (resolved == "providers"
            || resolved == "extensions"
            || resolved.starts_with("providers/")
            || resolved.starts_with("extensions/"))
}

/// Whether `text`, the inside of a string literal joined onto a path,
/// names a file: any non-empty literal counts but the junk `.`, `..`
/// and `/`, which name no file on their own.
pub(super) fn is_joined_name(text: &str) -> bool {
    !text.is_empty() && text != "." && text != ".." && text != "/"
}

/// Whether `text` is a literal a `join` reports: a joined file name
/// that is not the manifest literal and not an absolute path, which
/// replaces the receiver.
pub(super) fn joined_lit(text: &str) -> bool {
    is_joined_name(text) && text != "CARGO_MANIFEST_DIR" && !text.starts_with('/')
}

/// Whether `tokens` root a path at the manifest directory: they hold
/// the literal `CARGO_MANIFEST_DIR`, such as through `env!` inside
/// `Path::new`. A join onto any other base, such as a temporary
/// directory, is not a read of the repository.
pub(super) fn is_manifest_rooted(tokens: &[TokenTree]) -> bool {
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

/// Every string literal in `tokens` naming a joined file: the inline
/// segments of a chain built on a bound base.
pub(super) fn all_lits(tokens: &[TokenTree]) -> Vec<String> {
    fn visit(tokens: &[TokenTree], out: &mut Vec<String>) {
        for tree in tokens {
            match tree {
                TokenTree::Literal(lit) => {
                    if let Some(text) = string_literal(&lit.to_string())
                        && joined_lit(&text)
                    {
                        out.push(text);
                    }
                }
                TokenTree::Group(group) => {
                    visit(&group.stream().into_iter().collect::<Vec<_>>(), out)
                }
                TokenTree::Ident(_) | TokenTree::Punct(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    visit(tokens, &mut out);
    out
}
/// Whether `resolved` escapes the crate collectively: outside every
/// workspace member directory, not just outside the reading crate.
/// Cross-crate member fixtures stay unflagged; selection covers them
/// through dependents.
pub(super) fn escapes_members(resolved: &str, members: &Members) -> bool {
    members
        .values()
        .all(|member| resolved != member.dir && !resolved.starts_with(&format!("{}/", member.dir)))
}
/// Whether `resolved` names the executable fixtures `scripts/`
/// changes run with: a manifest-rooted chain there is legitimate.
pub(super) fn is_scripts_path(resolved: &str) -> bool {
    resolved == "scripts" || resolved.starts_with("scripts/")
}
/// Whether `resolved` is a run-time built path the gate reports:
/// outside every member, not a `scripts/` fixture, not a first-party
/// package, and not the repository root itself (an empty relative path
/// is a tree name, not a file, so inline uses stay unreported; checked
/// inline here, the only call site).
pub(super) fn escapes(resolved: &str, members: &Members, has_manifest: bool) -> bool {
    escapes_members(resolved, members)
        && !is_scripts_path(resolved)
        && !yields_to_packages(resolved, has_manifest)
        && !resolved.is_empty()
}
/// The first name in `tokens` bound to the manifest directory
/// itself, searching inside groups: the base a bare join builds on.
pub(super) fn root_name(tokens: &[TokenTree], root: &BTreeSet<String>) -> Option<String> {
    tokens.iter().find_map(|tree| match tree {
        TokenTree::Ident(ident) => {
            let name = ident.to_string();
            root.contains(name.as_str()).then_some(name)
        }
        TokenTree::Group(group) => root_name(&group.stream().into_iter().collect::<Vec<_>>(), root),
        TokenTree::Punct(_) | TokenTree::Literal(_) => None,
    })
}
/// The string literals after the manifest literal in `preceding`:
/// the inline chain a join builds on, without the base. Absolute
/// segments stay collected: a non-empty chain never reports through
/// a bare base, so an absolute path replacing the receiver stays
/// silent.
pub(super) fn inline_lits(preceding: &[TokenTree]) -> Vec<String> {
    fn visit(tokens: &[TokenTree], seen: &mut bool, out: &mut Vec<String>) {
        for tree in tokens {
            match tree {
                TokenTree::Literal(lit) => {
                    if let Some(text) = string_literal(&lit.to_string()) {
                        if text == "CARGO_MANIFEST_DIR" {
                            *seen = true;
                        } else if *seen && is_joined_name(&text) {
                            out.push(text);
                        }
                    }
                }
                TokenTree::Group(group) => {
                    visit(&group.stream().into_iter().collect::<Vec<_>>(), seen, out)
                }
                TokenTree::Ident(_) | TokenTree::Punct(_) => {}
            }
        }
    }
    let mut lits = Vec::new();
    let mut seen_manifest = false;
    visit(preceding, &mut seen_manifest, &mut lits);
    lits
}
/// Whether `group`'s argument takes a runtime value: any identifier
/// at all, searching inside groups. A join with one threads a value
/// into the path instead of naming a file.
pub(super) fn holds_value(group: &proc_macro2::Group) -> bool {
    fn visit(tokens: &[TokenTree]) -> bool {
        tokens.iter().any(|tree| match tree {
            TokenTree::Ident(_) => true,
            TokenTree::Group(nested) => visit(&nested.stream().into_iter().collect::<Vec<_>>()),
            TokenTree::Punct(_) | TokenTree::Literal(_) => false,
        })
    }
    visit(&group.stream().into_iter().collect::<Vec<_>>())
}
/// The identifiers a `write` or `create` call consumes as its first
/// argument, passed through references: the written path. Receivers
/// never count: only the argument group is scanned.
fn first_arg_idents(group: &proc_macro2::Group) -> Vec<String> {
    let mut first = Vec::new();
    for tree in group.stream() {
        if let TokenTree::Punct(mark) = &tree
            && mark.as_char() == ','
        {
            break;
        }
        first.push(tree);
    }
    fn visit(tokens: &[TokenTree], out: &mut Vec<String>) {
        for tree in tokens {
            match tree {
                TokenTree::Ident(ident) => out.push(ident.to_string()),
                TokenTree::Group(nested) => {
                    visit(&nested.stream().into_iter().collect::<Vec<_>>(), out)
                }
                TokenTree::Punct(_) | TokenTree::Literal(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    visit(&first, &mut out);
    out
}
/// How many times `name` is mentioned in `tokens`, searching inside
/// groups: the binding, the write positions, and every other use.
fn count_mentions(tokens: &[TokenTree], name: &str) -> usize {
    tokens
        .iter()
        .map(|tree| match tree {
            TokenTree::Ident(ident) => usize::from(ident == name),
            TokenTree::Group(group) => {
                count_mentions(&group.stream().into_iter().collect::<Vec<_>>(), name)
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => 0,
        })
        .sum()
}
/// The write-only idents of one function body: idents consumed in
/// first-argument position by a call named `write` or `create` with
/// no other mention in the body besides their binding and those
/// write positions. A written ident later passed elsewhere stays
/// reported.
pub(super) fn write_muted(body: &[TokenTree]) -> BTreeSet<String> {
    fn visit(tokens: &[TokenTree], written: &mut BTreeMap<String, usize>) {
        let owned: Vec<TokenTree> = tokens.to_vec();
        let mut stream = owned.into_iter().peekable();
        while let Some(tree) = stream.next() {
            match tree {
                TokenTree::Ident(name) if name == "write" || name == "create" => {
                    if let Some(TokenTree::Group(group)) = stream.peek() {
                        for ident in first_arg_idents(group) {
                            *written.entry(ident).or_default() += 1;
                        }
                    }
                }
                TokenTree::Group(group) => {
                    visit(&group.stream().into_iter().collect::<Vec<_>>(), written)
                }
                TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => {}
            }
        }
    }
    let mut written = BTreeMap::new();
    visit(body, &mut written);
    written
        .into_iter()
        .filter(|(name, count)| count_mentions(body, name) == 1 + count)
        .map(|(name, _)| name)
        .collect()
}
/// The literals of a bare-root join chain resolving exactly to the
/// repository root: a helper returning the tree itself. Reported at the
/// definition site so a call-site join onto the helper cannot thread the
/// root past the gate. `None` for
/// any other shape: a parameter, a non-bare base, a non-join use, or
/// a chain resolving anywhere else. Join shape only: ancestors and
/// push chains stay out of scope.
fn bare_root_lits(
    expr: &[TokenTree],
    manifest_dir: &str,
    root: &BTreeSet<String>,
) -> Option<Vec<String>> {
    let mut receiver: Vec<TokenTree> = Vec::new();
    let owned: Vec<TokenTree> = expr.to_vec();
    let mut stream = owned.into_iter().peekable();
    let mut lits = Vec::new();
    let mut based = false;
    let mut bare = false;
    while let Some(tree) = stream.next() {
        let is_join = matches!(&tree, TokenTree::Ident(name) if name == "join");
        // Clone the argument group before advancing: the peeked borrow
        // ends here, so the skip below stays legal.
        let group: Option<proc_macro2::Group> = if is_join {
            match stream.peek() {
                Some(TokenTree::Group(group)) => Some((*group).clone()),
                _ => None,
            }
        } else {
            None
        };
        if let Some(group) = group {
            if !based {
                // The base is bare when it roots at the manifest
                // directory with no literal segment yet, directly or
                // through a name bound to it.
                bare = is_manifest_rooted(&receiver) && inline_lits(&receiver).is_empty()
                    || holds_root(&receiver, root);
                based = true;
            }
            if holds_value(&group) {
                return None;
            }
            for tree in group.stream() {
                if let TokenTree::Literal(lit) = tree
                    && let Some(text) = string_literal(&lit.to_string())
                    && joined_lit(&text)
                {
                    lits.push(text);
                }
            }
            stream.next();
            receiver.push(tree);
            receiver.push(TokenTree::Group(group));
        } else {
            receiver.push(tree);
        }
    }
    if !based || !bare || lits.is_empty() {
        return None;
    }
    resolve_in_root(manifest_dir, &lits)
        .is_some_and(|resolved| resolved.is_empty())
        .then_some(lits)
}
/// The bare-root chains one function body returns: explicit `return`
/// values and the tail expression. Each resolves exactly to the
/// repository root.
pub(super) fn root_returns(
    body: &[TokenTree],
    manifest_dir: &str,
    root: &BTreeSet<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut returns = Vec::new();
    for tree in body {
        if let TokenTree::Punct(mark) = tree
            && mark.as_char() == ';'
        {
            if let [TokenTree::Ident(ret), rest @ ..] = current.as_slice()
                && ret == "return"
            {
                returns.push(rest.to_vec());
            }
            current.clear();
        } else {
            current.push(tree.clone());
        }
    }
    // The tail: a bare value, or an explicit return without its
    // semicolon. A leading `return` never joins the manifest-rooted
    // receiver, so stripping it changes no report and the tail pushes
    // whole.
    if !current.is_empty() {
        returns.push(current);
    }
    for expr in returns {
        if let Some(lits) = bare_root_lits(&expr, manifest_dir, root) {
            out.extend(lits);
        }
    }
    out
}
/// Whether `ch` ends the path expression a `join` builds on: the
/// receiver runs back to the nearest one of these, so a manifest
/// literal past it belongs to another expression. Bare brackets never
/// appear (every bracketed span is a Group), so only these count; `=>`
/// ends at its `=`.
pub(super) fn is_receiver_boundary(ch: char) -> bool {
    matches!(ch, ';' | '=' | ',')
}

/// Whether `tokens` hold an identifier bound to the manifest directory
/// itself by an earlier `let`, never one bound to a join on it.
pub(super) fn holds_root(tokens: &[TokenTree], root: &BTreeSet<String>) -> bool {
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
pub(super) fn all_literals(stream: TokenStream, out: &mut Vec<String>) {
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
