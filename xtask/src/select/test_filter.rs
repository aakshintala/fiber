//! Which nextest filter a changed test file selects (`bug-filter`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};

use super::{Members, owner, string_literal};
use crate::rules::RustFile;

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
#[path = "test_filter_tests.rs"]
mod tests;
