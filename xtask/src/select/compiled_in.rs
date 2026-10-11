//! The compiled-in list and the package-reader list (`docs/ci.md`,
//! "Selection"), and the checks that keep each against the sources.

use std::collections::BTreeSet;
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};

use super::{Members, is_docs_file, string_literal};
use crate::rules::RustFile;

/// Files a crate compiles in that the selector would not otherwise route to
/// that crate: Markdown anywhere, and every file outside the crate
/// (`docs/ci.md`, "Selection").
pub(super) const COMPILED_IN: &[(&str, &str)] = &[
    ("docs/ci.md", "xtask"),
    ("docs/errors.md", "contract"),
    ("docs/events.md", "contract"),
    ("docs/invocation.md", "contract"),
    ("docs/tui.md", "contract"),
    ("docs/tui.md", "tui"),
    ("crates/loop/prompt/messages.md", "loop"),
    ("crates/loop/prompt/opening.md", "loop"),
    ("crates/loop/prompt/reviewer.md", "loop"),
    ("crates/loop/prompt/system.md", "loop"),
    ("docs/skills/cache-warming/SKILL.md", "loop"),
    ("docs/skills/using-fiber/SKILL.md", "loop"),
    ("docs/skills/cache-warming/SKILL.md", "main"),
    ("docs/skills/using-fiber/SKILL.md", "main"),
    ("crates/tools/prompt/guidelines.md", "tools"),
];
/// Crates whose tests read a first-party package under `providers/` or
/// `extensions/` (`docs/ci.md`, "Selection"); sorted. Checked against the
/// sources by `package_reader_mismatches`.
pub(super) const PACKAGE_READERS: &[&str] = &["cli", "config", "extensions", "main", "xtask"];

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
/// reads a first-party package when `reads_package` says so, `xtask`
/// included: its drift test reads the committed packages. Err on a file
/// that does not tokenise.
pub(crate) fn package_reader_mismatches(
    files: &[RustFile],
    members: &Members,
) -> Result<Vec<String>, String> {
    let mut found = BTreeSet::new();
    for f in files {
        if !members.contains_key(&f.krate) {
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

#[cfg(test)]
#[path = "compiled_in_tests.rs"]
mod tests;
