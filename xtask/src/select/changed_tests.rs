//! The test functions a pull request adds or changes (`docs/ci.md`, "On
//! every pull request that changes code").
//!
//! The input is the output of `git diff -U0 --no-renames`: every hunk is
//! read for its new side only. A function is touched when a new-side line
//! falls inside its span, or when a zero-count gap sits where the rule in
//! `touches` says it changes the function. Anything unreadable is an error
//! naming the path, never an empty selection.

use std::collections::BTreeMap;

use proc_macro2::{Delimiter, Group, LineColumn, TokenStream, TokenTree};

use super::test_filter::{DeclarerIdent, Scope, index_path_decls, scope_of};
use super::{Members, is_test_file, owner};
use crate::rules::RustFile;

/// A test function the diff touches: the package and the nextest binary id
/// and test name that select it. `binary` is the nextest binary id for
/// integration and example tests and for `tests/<dir>/` modules with a
/// root file; `None` for unit tests and for shared `tests/<dir>/` modules.
/// `unit` is whether the test compiles into the package's unit tests (a
/// file under `src/`): only then does the filter use the package term, so
/// a unit test whose module path holds no `tests::` segment still matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestRef {
    pub(crate) package: String,
    pub(crate) binary: Option<String>,
    pub(crate) name: String,
    pub(crate) unit: bool,
}

/// Where one test file's tests run: the package, the nextest binary id if
/// the file is an integration or example test, the prefix its tests'
/// names start with, and whether its tests compile into the package's
/// unit tests rather than an integration binary.
struct FileScope {
    package: String,
    binary: Option<String>,
    prefix: String,
    unit: bool,
}

/// The package, binary and name prefix of the tests in the file at `path`:
/// `None` for any other path. A file directly in `tests/` and files under
/// `src/` or `examples/` take the `bug-filter` scope; a module under
/// `tests/<dir>/` takes the file layout, since several binaries may include
/// it under different names and no binary is known. When `tests/<dir>.rs`
/// is among `sources` the directory is that binary's module and the prefix
/// runs below it, else the prefix runs below the directory itself.
fn file_scope(
    path: &str,
    members: &Members,
    by_target: &BTreeMap<String, Vec<DeclarerIdent>>,
    sources: &[RustFile],
) -> Option<FileScope> {
    if let Some((package, scope, prefix)) = scope_of(path, members, by_target) {
        // Only a file under `src/` runs in the package's unit tests; a
        // file directly in `tests/` or under `examples/` runs in its own
        // binary instead.
        let unit = matches!(scope, Scope::Package);
        let binary = match scope {
            Scope::Binary(stem) => Some(format!("{package}::{stem}")),
            Scope::Package => None,
            Scope::Example(example) => Some(format!("{package}::example/{example}")),
        };
        return Some(FileScope {
            package,
            binary,
            prefix,
            unit,
        });
    }
    if !path.ends_with(".rs") {
        return None;
    }
    let name = owner(path, members)?;
    let member = members.get(name)?;
    let rel = path.get(member.dir.len() + 1..).unwrap_or_default();
    if !is_test_file(rel) {
        return None;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    let ["tests", dir, rest @ ..] = parts.as_slice() else {
        return None;
    };
    if rest.is_empty() {
        return None;
    }
    let mut below: Vec<&str> = rest.to_vec();
    if below.last() == Some(&"mod.rs") {
        below.pop();
    }
    let mut short = String::new();
    for segment in &below {
        short.push_str(segment.strip_suffix(".rs").unwrap_or(segment));
        short.push_str("::");
    }
    let root = format!("{}/tests/{dir}.rs", member.dir);
    if sources.iter().any(|f| f.path == root) {
        Some(FileScope {
            package: name.to_owned(),
            binary: Some(format!("{name}::{dir}")),
            prefix: short,
            unit: false,
        })
    } else {
        Some(FileScope {
            package: name.to_owned(),
            binary: None,
            prefix: format!("{dir}::{short}"),
            unit: false,
        })
    }
}

/// A test function in one file: its name with inline modules, and its
/// span in lines, from its first attribute through its closing brace.
struct TestFn {
    name: String,
    start: u32,
    end: u32,
}

/// A source line number: real files are far shorter, so a number that
/// does not fit saturates instead of panicking.
fn line(at: LineColumn) -> u32 {
    u32::try_from(at.line).unwrap_or(u32::MAX)
}

/// A raw identifier without its `r#`, as nextest prints the test name.
fn strip_raw(ident: &str) -> &str {
    ident.strip_prefix("r#").unwrap_or(ident)
}

/// Whether the `[...]` attribute makes its function a test: its path ends
/// in `test`, with at most a trailing argument list. `#[cfg(test)]`
/// names no test, and neither do `#[ignore]`, `#[should_panic]` or a test
/// macro's own attribute.
fn is_test_attr(group: &Group) -> bool {
    let tokens: Vec<TokenTree> = group.stream().into_iter().collect();
    let mut segments: Vec<String> = Vec::new();
    // The unconsumed tail of the path: each pass takes at least the
    // leading identifier, so the walk always ends. (Stepping an index
    // past each `::` instead stalls under its `*=` mutant: `0 * 3`
    // stays 0 and the walk never ends.)
    let mut rest: &[TokenTree] = &tokens;
    while let [TokenTree::Ident(segment), tail @ ..] = rest {
        segments.push(segment.to_string());
        match tail {
            [TokenTree::Punct(first), TokenTree::Punct(second), next @ ..]
                if first.as_char() == ':' && second.as_char() == ':' =>
            {
                rest = next;
            }
            _ => {
                rest = tail;
                break;
            }
        }
    }
    let (Some(first), Some(last)) = (segments.first(), segments.last()) else {
        return false;
    };
    if last != "test" || first == "cfg" {
        return false;
    }
    matches!(rest, [] | [TokenTree::Group(_)])
}

/// One attribute stack: where it starts and whether it holds a test.
struct AttrStack {
    start: u32,
    is_test: bool,
}

/// Every `#[test]` function at the file's top level and in its inline
/// modules, with the module path in its name. Only `#[` joins an
/// attribute stack; `#![..]` and `//!` never do. A function body is never
/// entered, so a macro's tests and a method (which the compiler rejects
/// as a test) select nothing.
fn walk(trees: Vec<TokenTree>, prefix: &str, out: &mut Vec<TestFn>) {
    let mut trees = trees.into_iter().peekable();
    let mut stack: Option<AttrStack> = None;
    while let Some(tree) = trees.next() {
        match tree {
            TokenTree::Punct(hash) if hash.as_char() == '#' => match trees.next() {
                Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Bracket => {
                    let test = is_test_attr(&group);
                    match stack.as_mut() {
                        Some(top) => top.is_test = top.is_test || test,
                        None => {
                            stack = Some(AttrStack {
                                start: line(hash.span().start()),
                                is_test: test,
                            });
                        }
                    }
                }
                Some(TokenTree::Punct(bang)) if bang.as_char() == '!' => {
                    if matches!(trees.peek(), Some(TokenTree::Group(_))) {
                        trees.next();
                    }
                }
                _ => {
                    stack = None;
                }
            },
            TokenTree::Ident(word) if word == "mod" => {
                stack = None;
                let name = match trees.next() {
                    Some(TokenTree::Ident(name)) => name,
                    _ => continue,
                };
                let is_inline = matches!(
                    trees.peek(),
                    Some(TokenTree::Group(next)) if next.delimiter() == Delimiter::Brace
                );
                if !is_inline {
                    continue;
                }
                let Some(TokenTree::Group(body)) = trees.next() else {
                    continue;
                };
                let nested = format!("{prefix}{}::", strip_raw(&name.to_string()));
                walk(body.stream().into_iter().collect(), &nested, out);
            }
            TokenTree::Ident(word) if word == "fn" => {
                let Some(TokenTree::Ident(name)) = trees.next() else {
                    stack = None;
                    continue;
                };
                let (start, is_test) = match stack.take() {
                    Some(top) => (top.start, top.is_test),
                    None => (line(word.span().start()), false),
                };
                let mut end = None;
                for tree in trees.by_ref() {
                    match tree {
                        TokenTree::Group(body) if body.delimiter() == Delimiter::Brace => {
                            end = Some(line(body.span().end()));
                            break;
                        }
                        TokenTree::Punct(semi) if semi.as_char() == ';' => break,
                        TokenTree::Group(_)
                        | TokenTree::Ident(_)
                        | TokenTree::Punct(_)
                        | TokenTree::Literal(_) => {}
                    }
                }
                if let (true, Some(end)) = (is_test, end) {
                    out.push(TestFn {
                        name: format!("{prefix}{}", strip_raw(&name.to_string())),
                        start,
                        end,
                    });
                }
            }
            // A qualifier between the attributes and the `fn`.
            TokenTree::Ident(word)
                if word == "pub"
                    || word == "unsafe"
                    || word == "async"
                    || word == "extern"
                    || word == "const" => {}
            // A group or literal never clears the stack: after a pending
            // stack it is the `pub(..)` restriction or the `extern`
            // string, and with no stack there is nothing to clear. (A
            // `stack.is_some()` guard here would be a tautology: dropping
            // it changes no selection, so it would leave a mutant no test
            // can catch.)
            TokenTree::Group(_) | TokenTree::Literal(_) => {}
            TokenTree::Ident(_) | TokenTree::Punct(_) => {
                stack = None;
            }
        }
    }
}

/// Every test function the source holds, in file order.
fn test_fns(source: &str) -> Result<Vec<TestFn>, ()> {
    let stream: TokenStream = source.parse().map_err(|_| ())?;
    let mut out = Vec::new();
    walk(stream.into_iter().collect(), "", &mut out);
    Ok(out)
}

/// One hunk's new side: the lines it holds, or the gap a zero-count hunk
/// leaves between new lines `before` and `before + 1`.
enum Hunk {
    Lines(u32, u32),
    Gap(u32, u32),
}

/// Whether the hunk changes a function spanning `start` to `end`: a held
/// line inside, or a gap whose far side is inside while its near side is
/// inside too or the far side is the span's first line. A gap after the
/// closing brace changes nothing, and a deleted function above a test
/// changes that test through its first line.
// Exempt: the gap arm's inner `&&` becomes `||` without changing any
// selection. `changed_tests` only builds gaps as (`new_start`,
// `new_start + 1`), so when exactly one side of the inner `&&` holds the
// far side already misses the span: `before` past `end` puts `after` past
// it too, and `before` short of `start` puts `after` on or short of
// `start` (on it, `after == start` already selects). No consumer-visible
// input distinguishes the two, so no test can catch it.
#[cfg_attr(false, mutants::skip)]
fn touches(hunk: &Hunk, start: u32, end: u32) -> bool {
    match *hunk {
        Hunk::Lines(first, last) => first <= end && last >= start,
        Hunk::Gap(before, after) => {
            start <= after && after <= end && (start <= before && before <= end || after == start)
        }
    }
}

/// One side of a hunk header: its start line and line count, the count
/// defaulting to 1 when omitted.
fn parse_side(field: &str, prefix: char, path: &str) -> Result<(u32, u32), String> {
    let err = || format!("{path}: malformed hunk header");
    let digits = field.strip_prefix(prefix).ok_or_else(err)?;
    match digits.split_once(',') {
        Some((start, count)) => Ok((
            start.parse().map_err(|_| err())?,
            count.parse().map_err(|_| err())?,
        )),
        None => Ok((digits.parse().map_err(|_| err())?, 1)),
    }
}

/// A hunk header's old and new starts and counts. Trailing section
/// headings are ignored, as `git diff -U0` still prints them.
fn parse_hunk(header: &str, path: &str) -> Result<(u32, u32, u32, u32), String> {
    let err = || format!("{path}: malformed hunk header");
    let ranges = header
        .strip_prefix("@@ ")
        .and_then(|rest| rest.split_once(" @@"))
        .map(|(ranges, _)| ranges)
        .ok_or_else(err)?;
    let mut ranges = ranges.split_whitespace();
    let (old, new) = match (ranges.next(), ranges.next(), ranges.next()) {
        (Some(old), Some(new), None) => (old, new),
        _ => return Err(err()),
    };
    let (old_start, old_count) = parse_side(old, '-', path)?;
    let (new_start, new_count) = parse_side(new, '+', path)?;
    Ok((old_start, old_count, new_start, new_count))
}

/// The tests in scope at `path`: an empty selection when the file is no
/// test file, or an error naming the path when its source is missing or
/// does not tokenise.
fn resolve(
    path: &str,
    members: &Members,
    by_target: &BTreeMap<String, Vec<DeclarerIdent>>,
    sources: &[RustFile],
) -> Result<Option<Vec<(TestFn, TestRef)>>, String> {
    let Some(scope) = file_scope(path, members, by_target, sources) else {
        return Ok(None);
    };
    let Some(source) = sources.iter().find(|f| f.path == path) else {
        return Err(format!("{path}: no source in the working tree"));
    };
    let fns = test_fns(&source.source).map_err(|()| format!("{path}: cannot parse"))?;
    Ok(Some(
        fns.into_iter()
            .map(|f| {
                let test = TestRef {
                    package: scope.package.clone(),
                    binary: scope.binary.clone(),
                    name: format!("{}{}", scope.prefix, f.name),
                    unit: scope.unit,
                };
                (f, test)
            })
            .collect(),
    ))
}

/// What the open file's hunks have resolved to: no test file, or its
/// functions with their selections.
struct OpenFile {
    path: Option<String>,
    raw: String,
    deleted: bool,
    tests: Option<Vec<(TestFn, TestRef)>>,
}

/// The test functions the diff adds or changes, in diff order,
/// de-duplicated: a pure function of its inputs, which reads the working
/// tree through `sources` and never git or the clock.
pub(crate) fn changed_tests(
    diff: &str,
    members: &Members,
    sources: &[RustFile],
) -> Result<Vec<TestRef>, String> {
    let by_target = index_path_decls(sources);
    let mut out: Vec<TestRef> = Vec::new();
    let mut file: Option<OpenFile> = None;
    // The open hunk's remaining old and new lines and its next new line.
    let mut hunk: Option<(u32, u32, u32)> = None;
    for line in diff.lines() {
        if let Some((remaining_old, remaining_new, next_new)) = hunk {
            if remaining_old > 0 || remaining_new > 0 {
                let current = file.as_ref();
                let path = current
                    .and_then(|f| f.path.as_deref())
                    .unwrap_or_else(|| current.map_or("", |f| f.raw.as_str()));
                if line.starts_with('+') {
                    let remaining_new = remaining_new
                        .checked_sub(1)
                        .ok_or_else(|| format!("{path}: hunk body does not match its header"))?;
                    let next_new = next_new
                        .checked_add(1)
                        .ok_or_else(|| format!("{path}: hunk body does not match its header"))?;
                    if let Some(tests) = current.and_then(|f| f.tests.as_ref()) {
                        select(&Hunk::Lines(next_new - 1, next_new - 1), tests, &mut out);
                    }
                    hunk = Some((remaining_old, remaining_new, next_new));
                    continue;
                }
                if line.starts_with('-') {
                    let remaining_old = remaining_old
                        .checked_sub(1)
                        .ok_or_else(|| format!("{path}: hunk body does not match its header"))?;
                    hunk = Some((remaining_old, remaining_new, next_new));
                    continue;
                }
                if line.starts_with(' ') || line.is_empty() {
                    let remaining_old = remaining_old
                        .checked_sub(1)
                        .ok_or_else(|| format!("{path}: hunk body does not match its header"))?;
                    let remaining_new = remaining_new
                        .checked_sub(1)
                        .ok_or_else(|| format!("{path}: hunk body does not match its header"))?;
                    let next_new = next_new
                        .checked_add(1)
                        .ok_or_else(|| format!("{path}: hunk body does not match its header"))?;
                    if let Some(tests) = current.and_then(|f| f.tests.as_ref()) {
                        select(&Hunk::Lines(next_new - 1, next_new - 1), tests, &mut out);
                    }
                    hunk = Some((remaining_old, remaining_new, next_new));
                    continue;
                }
                if line.starts_with('\\') {
                    continue;
                }
                return Err(format!("{path}: hunk body does not match its header"));
            }
            hunk = None;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            file = Some(OpenFile {
                path: None,
                raw: rest.to_owned(),
                deleted: false,
                tests: None,
            });
        } else if let Some(rest) = line.strip_prefix("--- ") {
            if rest.starts_with('"') {
                return Err(format!("{rest}: quoted path in diff header"));
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if rest.starts_with('"') {
                return Err(format!("{rest}: quoted path in diff header"));
            }
            if let Some(current) = file.as_mut() {
                if rest == "/dev/null" {
                    current.deleted = true;
                } else {
                    let path = rest.strip_prefix("b/").unwrap_or(rest).to_owned();
                    current.path = Some(path.clone());
                    current.tests = resolve(&path, members, &by_target, sources)?;
                }
            }
        } else if line.starts_with("@@ ") {
            let Some(current) = file.as_ref() else {
                return Err(format!("{line}: hunk outside any file"));
            };
            let name = current.path.as_deref().unwrap_or(current.raw.as_str());
            let (_, old_count, new_start, new_count) = parse_hunk(line, name)?;
            if !current.deleted
                && new_count == 0
                && let Some(tests) = current.tests.as_ref()
            {
                let after = new_start
                    .checked_add(1)
                    .ok_or_else(|| format!("{name}: malformed hunk header"))?;
                select(&Hunk::Gap(new_start, after), tests, &mut out);
            }
            hunk = Some((old_count, new_count, new_start));
        }
    }
    Ok(out)
}

/// Every touched function into the selection, once each.
fn select(hunk: &Hunk, tests: &[(TestFn, TestRef)], out: &mut Vec<TestRef>) {
    for (f, test) in tests {
        if touches(hunk, f.start, f.end) && !out.contains(test) {
            out.push(test.clone());
        }
    }
}

/// The nextest filter selecting exactly the touched tests, in diff order:
/// a binary term for integration and example tests, a package term for
/// unit tests, and a `kind(test)` package term for shared `tests/`
/// support modules. Which package term a test without a binary takes is
/// kept from where it compiles, never read from its module path: a unit
/// test in a `#[path]`-declared file (as `app::commands::aliases::<name>`)
/// holds no `tests::` segment, yet still runs in the package's unit
/// tests, where a `kind(test)` term would match nothing.
pub(crate) fn filter(tests: &[TestRef]) -> String {
    tests
        .iter()
        .map(|test| match &test.binary {
            Some(binary) => format!("(binary_id({binary}) & test(={}))", test.name),
            None if test.unit => {
                format!("(package({}) & test(={}))", test.package, test.name)
            }
            None => format!(
                "(package({}) & kind(test) & test(={}))",
                test.package, test.name
            ),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
#[path = "changed_tests_tests.rs"]
mod tests;
