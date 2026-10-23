//! Join and format helpers for the run-time-read scanner.

use super::paths::{
    all_literals, holds_root, is_joined_name, is_manifest_rooted, is_path_like, joined_lit,
    resolve_in_root, yields_to_packages,
};
use crate::select::string_literal;
use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};

/// The literals a top-level `join` call in `rhs` joins onto the
/// manifest directory or onto a name bound to it: the value a `let`
/// holding such a join reports when a read takes its name. A join
/// nested inside another call's argument, such as a read's own
/// argument, binds nothing: the read reports it inline instead.
pub(super) fn join_on_root(rhs: &[TokenTree], root: &BTreeSet<String>) -> Option<Vec<String>> {
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
                        && (is_joined_name(&text) || text == "..")
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
pub(super) fn joined_mention(
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
pub(super) fn second_mention(
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
pub(super) fn join_on_joined(
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
                base =
                    joined_mention(&receiver, joined).or_else(|| second_mention(&receiver, second));
            }
            if base.is_some() {
                for tree in group.stream() {
                    if let TokenTree::Literal(lit) = tree
                        && let Some(text) = string_literal(&lit.to_string())
                        && joined_lit(&text)
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
pub(super) fn rooted_format(
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

/// The suffixes a `format!` outside any read builds onto the
/// manifest directory: `None` when the macro takes a runtime value,
/// misses the manifest token, or holds a template that does not
/// start with the placeholder. A template resolves with the
/// placeholders removed; a plain suffix resolves as written, and an
/// absolute plain suffix names a system path, so the whole macro
/// reports nothing, exactly like the read-argument arm.
pub(super) fn format_probes(group: &proc_macro2::Group) -> Option<Vec<String>> {
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
    let mut probes = Vec::new();
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
            probes.push(probe);
        } else {
            if piece.starts_with('/') {
                return None;
            }
            if !is_path_like(&piece) {
                continue;
            }
            probes.push(piece);
        }
    }
    (!probes.is_empty()).then_some(probes)
}
