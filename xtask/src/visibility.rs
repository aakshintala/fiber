//! The visibility check (`docs/code-quality.md`, "Visibility"): a
//! `pub(crate)` or `pub(super)` function, const or static no other file of
//! its crate names is wider than any use needs. Use-search is plain text
//! over every other file of the crate, so a match in a comment, a string,
//! a macro body or a `#[cfg]`-gated file still counts, and a match only ever
//! hides a narrowing. Items defined inside an inline `mod` block are
//! skipped — their same-file uses stay invisible to a file-granular search.

use std::collections::BTreeSet;
use std::path::Path;

use proc_macro2::{Delimiter, TokenStream, TokenTree};

use crate::rules::RustFile;

// RULING-4142-6: flagged sites that live in files this branch must not
// narrow stay listed in the baseline instead. The list is shrink-only:
// deleting a line is the only way it changes, and a line that matches no
// real flagged site fails the check, so a stale line is a red build, not
// silent rot.
/// The checked-in baseline of held `path:item` sites, relative to the
/// repository root. Private: only this module reads it.
const BASELINE: &str = "xtask/src/visibility_baseline.txt";

/// The baseline entries in `text`: one `path:item` per line, blank lines
/// ignored.
pub(crate) fn parse_baseline(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The baseline entries in the `BASELINE` file at or above `dir`: the
/// check runs from the repository root and from any subdirectory, so the
/// lookup walks up. A missing file means empty, no skip.
pub(crate) fn load_baseline_from(dir: &Path) -> Vec<String> {
    let mut dir = dir.to_path_buf();
    loop {
        if let Ok(text) = std::fs::read_to_string(dir.join(BASELINE)) {
            return parse_baseline(&text);
        }
        if !dir.pop() {
            return Vec::new();
        }
    }
}

/// The baseline entries for the current working directory.
pub(crate) fn load_baseline() -> Vec<String> {
    match std::env::current_dir() {
        Ok(dir) => load_baseline_from(&dir),
        Err(_) => Vec::new(),
    }
}

/// A `pub(crate)` or `pub(super)` function, const or static no other file
/// of its crate names, as `<path>:<line>: <name> is pub(crate) but no other
/// file in <crate> names it`, minus every flagged site the baseline lists
/// as `path:item`, plus one failure per baseline entry matching no flagged
/// site: a narrowed-away or newly-used item reddens the build until its
/// line is deleted. Test files count on both sides; macro bodies count as
/// use-sites. The check passes only when the flagged set minus the
/// baseline is empty and no baseline entry is stale.
pub(crate) fn check_with_baseline(files: &[RustFile], baseline: &[String]) -> Vec<String> {
    let baselined: BTreeSet<&str> = baseline.iter().map(String::as_str).collect();
    let mut live: BTreeSet<String> = BTreeSet::new();
    let mut failures = Vec::new();
    for (index, file) in files.iter().enumerate() {
        for declaration in declarations(&file.source) {
            let named_elsewhere = files.iter().enumerate().any(|(other, candidate)| {
                other != index
                    && candidate.krate == file.krate
                    && candidate.source.contains(declaration.name.as_str())
            });
            if !named_elsewhere {
                let key = format!("{}:{}", file.path, declaration.name);
                live.insert(key.clone());
                if !baselined.contains(key.as_str()) {
                    failures.push(format!(
                        "{}:{}: {} is {} but no other file in {} names it",
                        file.path,
                        declaration.line,
                        declaration.name,
                        declaration.visibility,
                        file.krate
                    ));
                }
            }
        }
    }
    for entry in baselined {
        if !live.contains(entry) {
            failures.push(format!(
                "{entry} is in the visibility baseline but matches no flagged site"
            ));
        }
    }
    failures
}

/// A checkable declaration: its name, its 1-based line, and its visibility
/// spelled as written.
struct Declaration {
    name: String,
    line: usize,
    visibility: &'static str,
}

/// Every checkable `pub(crate)`/`pub(super)` function, const or static in
/// `source`, in token order. A file that does not tokenise resolves to
/// skip: nothing in it can be named, and a miss under-flags.
fn declarations(source: &str) -> Vec<Declaration> {
    let stream: TokenStream = match source.parse() {
        Ok(stream) => stream,
        Err(_) => return Vec::new(),
    };
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    let mut found = Vec::new();
    walk_items(&trees, &mut found);
    found
}

/// The declarations `trees` holds, in order. Macro-definition bodies and
/// inline `mod` bodies are skipped whole; every other group recurses, so
/// items nested in functions, blocks, impls and attributes still count.
/// Struct, enum and union bodies need no rule: a field after a visibility
/// qualifier is never followed by a `fn`, `const` or `static` keyword.
fn walk_items(trees: &[TokenTree], found: &mut Vec<Declaration>) {
    let mut i = 0;
    while let Some(tree) = trees.get(i) {
        if let Some(declaration) = declaration_at(trees, i) {
            found.push(declaration);
        } else if let Some(after) = skip_macro_body(trees, i) {
            i = after;
            continue;
        } else if let Some(after) = skip_inline_mod(trees, i) {
            i = after;
            continue;
        } else if let TokenTree::Group(group) = tree {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            walk_items(&inner, found);
        }
        i += 1;
    }
}

/// The `pub(crate)` or `pub(super)` function, const or static starting at
/// `trees[i]`, if one starts there: the name, the visibility as written,
/// and the name's line. Only `fn`, `const` and `static` items are
/// candidates: a type's visibility is pinned by any wider-or-equal
/// interface that mentions it (E0446), and a type's use can reach another
/// module through inference without being spelled, so a syntactic check
/// cannot see it (`crates/fakes/src/http_server/server.rs:150` keeps
/// `TunnelState` visible through its `pub(crate)` field;
/// `crates/log/src/search.rs:145` binds `workers::run(...)` and reads its
/// fields without ever spelling `Searched`). Structs, enums, unions,
/// traits, type aliases, modules, imports and fields resolve to skip, which
/// under-flags.
/// The index past the `unsafe`/`async`/`default`/`extern ["lit"]`/`const
/// fn` qualifier run opening at `at`: `const fn`, `unsafe fn`, `async fn`,
/// `extern "C" fn` carry qualifiers before the item keyword.
///
/// Why an iterator: `docs/testing/mutants.md`, "Proving a test bites" — a
/// loop that steps an index by hand can spin forever when a mutant breaks
/// the arithmetic, and the hung mutant fails CI as a timeout. Counting the
/// run with an iterator over `trees[at..]` and advancing once by the count
/// turns that timeout family into plain misparses a test can catch.
fn qualifier_end(trees: &[TokenTree], at: usize) -> usize {
    let mut rest = trees.get(at..).unwrap_or(&[]).iter();
    while let Some(TokenTree::Ident(word)) = rest.as_slice().first() {
        let word = word.to_string();
        if matches!(word.as_str(), "unsafe" | "async" | "default") {
            rest.next();
        } else if word.as_str() == "extern" {
            rest.next();
            if matches!(rest.as_slice().first(), Some(TokenTree::Literal(_))) {
                rest.next();
            }
        } else if word.as_str() == "const" && is_ident(rest.clone().nth(1), "fn") {
            rest.next();
        } else {
            break;
        }
    }
    trees.len() - rest.len()
}

fn declaration_at(trees: &[TokenTree], i: usize) -> Option<Declaration> {
    let (visibility, at) = restricted_visibility(trees, i)?;
    // `const fn`, `unsafe fn`, `async fn`, `extern "C" fn`: qualifiers
    // before the item keyword (counted with an iterator, advanced once;
    // see `qualifier_end`).
    let mut at = qualifier_end(trees, at);
    if is_ident(trees.get(at), "fn") || is_ident(trees.get(at), "const") {
        at += 1;
    } else if is_ident(trees.get(at), "static") {
        at += 1;
        if is_ident(trees.get(at), "mut") {
            at += 1;
        }
    } else {
        return None;
    }
    let ident = match trees.get(at) {
        Some(TokenTree::Ident(ident)) => ident,
        _ => return None,
    };
    let spelling = ident.to_string();
    let name = spelling.strip_prefix("r#").unwrap_or(&spelling).to_owned();
    Some(Declaration {
        name,
        line: ident.span().start().line,
        visibility,
    })
}

/// The `pub(crate)` or `pub(super)` qualifier at `trees[i]`, spelled as
/// written, and the index after it.
fn restricted_visibility(trees: &[TokenTree], i: usize) -> Option<(&'static str, usize)> {
    if !is_ident(trees.get(i), "pub") {
        return None;
    }
    let group = match trees.get(i + 1) {
        Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Parenthesis => group,
        _ => return None,
    };
    let inner: Vec<TokenTree> = group.stream().into_iter().collect();
    if inner.len() != 1 {
        return None;
    }
    if is_ident(inner.first(), "crate") {
        Some(("pub(crate)", i + 2))
    } else if is_ident(inner.first(), "super") {
        Some(("pub(super)", i + 2))
    } else {
        None
    }
}

/// The index past a `macro_rules!` or `macro` definition body at `trees[i]`,
/// if one opens there: the name and its single body group are skipped
/// whole, whatever the delimiter.
fn skip_macro_body(trees: &[TokenTree], i: usize) -> Option<usize> {
    if !is_ident(trees.get(i), "macro_rules") && !is_ident(trees.get(i), "macro") {
        return None;
    }
    let mut at = i + 1;
    if is_ident(trees.get(i), "macro_rules") {
        if !is_punct(trees.get(at), '!') {
            return None;
        }
        at += 1;
    }
    if !matches!(trees.get(at), Some(TokenTree::Ident(_))) {
        return None;
    }
    at += 1;
    if matches!(trees.get(at), Some(TokenTree::Group(_))) {
        Some(at + 1)
    } else {
        None
    }
}

/// The index past an inline `mod` body at `trees[i]`, if one opens there:
/// an optional visibility prefix, `mod`, a name, and a brace group. The
/// body is skipped whole — same-file uses stay invisible to the
/// file-granular search — while the opener line itself holds no
/// declaration, since modules are out of scope.
fn skip_inline_mod(trees: &[TokenTree], i: usize) -> Option<usize> {
    // Why an iterator: the same hand-stepped `at += 1` timeout family as
    // the old qualifier loop (docs/testing/mutants.md, "Proving a test
    // bites") — the `pub`-prefix stepping is walked with an iterator over
    // `trees[i..]` and resolved once from its remainder.
    let mut rest = trees.get(i..).unwrap_or(&[]).iter();
    if is_ident(rest.as_slice().first(), "pub") {
        rest.next();
        if matches!(rest.as_slice().first(), Some(TokenTree::Group(_))) {
            rest.next();
        }
    }
    if !is_ident(rest.as_slice().first(), "mod") {
        return None;
    }
    rest.next();
    if !matches!(rest.as_slice().first(), Some(TokenTree::Ident(_))) {
        return None;
    }
    rest.next();
    let is_brace = matches!(
        rest.as_slice().first(),
        Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Brace
    );
    if !is_brace {
        return None;
    }
    rest.next();
    Some(trees.len() - rest.len())
}

fn is_ident(tree: Option<&TokenTree>, word: &str) -> bool {
    matches!(tree, Some(TokenTree::Ident(ident)) if ident == word)
}

fn is_punct(tree: Option<&TokenTree>, ch: char) -> bool {
    matches!(tree, Some(TokenTree::Punct(punct)) if punct.as_char() == ch)
}

#[cfg(test)]
#[path = "visibility_tests.rs"]
mod tests;
