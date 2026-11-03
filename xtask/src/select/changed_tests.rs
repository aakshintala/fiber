//! The test functions a pull request adds or changes (`docs/ci.md`, "On
//! every pull request that changes code").
//!
//! The input is the output of `git diff -U0 --no-renames`: every hunk is
//! read for its new side only. A function is touched when a new-side line
//! falls inside its span, or when a zero-count gap sits where the rule in
//! `touches` says it changes the function. Anything unreadable is an error
//! naming the path, never an empty selection.

use proc_macro2::{Delimiter, Group, LineColumn, TokenStream, TokenTree};

use super::{Members, owner};
use crate::rules::RustFile;
use crate::test_index::TestIndex;

/// A test function the diff touches: the package and the nextest binary id
/// and test name that select it. `binary` is the nextest binary id for
/// integration, example and shared test-module sources; it is `None` for
/// unit tests. `unit` records whether nextest lists the suite as a `lib` or
/// `bin`: those tests use the package term, so a unit test whose module
/// path holds no `tests::` segment still matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestRef {
    pub(crate) package: String,
    pub(crate) binary: Option<String>,
    pub(crate) name: String,
    pub(crate) unit: bool,
}

/// A test selection is either the changed functions in a source file or,
/// for a shared helper with no test functions, every test binary that
/// compiles that source.
enum FileTests {
    Functions(Vec<(TestFn, TestRef)>),
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

pub(crate) fn has_test_functions(source: &str) -> Result<bool, String> {
    test_fns(source)
        .map(|functions| !functions.is_empty())
        .map_err(|()| "cannot parse".to_owned())
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

/// The tests compiled from `path`, selected by nextest's names rather than
/// a module path inferred from the file's location.
fn resolve(
    path: &str,
    members: &Members,
    sources: &[RustFile],
    index: &TestIndex,
) -> Result<Option<FileTests>, String> {
    if !path.ends_with(".rs") {
        return Ok(None);
    }
    let Some(package) = owner(path, members) else {
        return Ok(None);
    };
    let member = members
        .get(package)
        .ok_or_else(|| format!("{path}: no workspace member"))?;
    if member.unrun_tests.contains(&path.to_owned()) {
        return Ok(None);
    }
    let Some(source) = sources.iter().find(|source| source.path == path) else {
        return Err(format!("{path}: no source in the working tree"));
    };
    let binaries = index.binaries_for(path);
    if binaries.is_empty() {
        // This runner did not compile the file, as with a test gated for
        // another target OS. It contributes no tests to this runner's plan.
        return Ok(None);
    }
    let functions = test_fns(&source.source).map_err(|()| format!("{path}: cannot parse"))?;
    if functions.is_empty() {
        return Ok(None);
    }
    let mut tests = Vec::new();
    for function in functions {
        for binary in binaries {
            for test in &binary.tests {
                if test_name_matches(&test.name, &function.name) {
                    tests.push((
                        TestFn {
                            name: function.name.clone(),
                            start: function.start,
                            end: function.end,
                        },
                        test.clone(),
                    ));
                }
            }
        }
    }
    Ok(Some(FileTests::Functions(tests)))
}

pub(crate) fn test_name_matches(test: &str, function: &str) -> bool {
    test == function || test.ends_with(&format!("::{function}"))
}

/// The test ids the linked flake issue names, one per marker line outside
/// any fenced block, normalised past the binary prefix and de-duplicated
/// in first-sighting order. A marker line starts with "The test `" and
/// names the span to the next backtick on that line; every other line,
/// and every line inside a fence, names nothing.
pub(crate) fn issue_test_names(bodies: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut fenced = false;
    for line in bodies.lines() {
        if line.contains("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let Some(rest) = line.strip_prefix("The test `") else {
            continue;
        };
        let Some(end) = rest.find('`') else {
            continue;
        };
        let id = rest[..end].trim();
        if id.is_empty() {
            continue;
        }
        let compared = match id.find(' ') {
            Some(space) => &id[space + 1..],
            None => id,
        };
        if !names.iter().any(|name| name == compared) {
            names.push(compared.to_owned());
        }
    }
    names
}

/// The touched tests split into the named ones and the rest, each in diff
/// order. The names step matches by exact equality between the marker
/// id's compared name and the test's nextest name.
pub(crate) fn split_named_tests(
    touched: &[TestRef],
    names: &[String],
) -> (Vec<TestRef>, Vec<TestRef>) {
    let mut named = Vec::new();
    let mut rest = Vec::new();
    for test in touched {
        if names.iter().any(|name| name == &test.name) {
            named.push(test.clone());
        } else {
            rest.push(test.clone());
        }
    }
    (named, rest)
}

/// What the open file's hunks have resolved to: no test file, or its
/// functions with their selections.
struct OpenFile {
    path: Option<String>,
    raw: String,
    deleted: bool,
    tests: Option<FileTests>,
    selected: Vec<TestRef>,
}

/// The test functions the diff adds or changes, in diff order and
/// de-duplicated. The compiler index already excludes tests the runner did
/// not build.
#[cfg(test)]
pub(crate) fn changed_tests(
    diff: &str,
    members: &Members,
    sources: &[RustFile],
    index: &TestIndex,
) -> Result<Vec<TestRef>, String> {
    let mut out = Vec::new();
    for (_, tests) in changed_tests_by_file(diff, members, sources, index)? {
        for test in tests {
            if !out.contains(&test) {
                out.push(test);
            }
        }
    }
    Ok(out)
}

/// The tests selected from each changed file, in diff order.
pub(crate) fn changed_tests_by_file(
    diff: &str,
    members: &Members,
    sources: &[RustFile],
    index: &TestIndex,
) -> Result<Vec<(String, Vec<TestRef>)>, String> {
    let mut out: Vec<(String, Vec<TestRef>)> = Vec::new();
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
                    if let Some(current) = file.as_mut()
                        && let Some(tests) = current.tests.as_ref()
                    {
                        select(
                            &Hunk::Lines(next_new - 1, next_new - 1),
                            tests,
                            &mut current.selected,
                        );
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
                    if let Some(current) = file.as_mut()
                        && let Some(tests) = current.tests.as_ref()
                    {
                        select(
                            &Hunk::Lines(next_new - 1, next_new - 1),
                            tests,
                            &mut current.selected,
                        );
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
            if let Some(previous) = file.take()
                && let Some(path) = previous.path
            {
                out.push((path, previous.selected));
            }
            file = Some(OpenFile {
                path: None,
                raw: rest.to_owned(),
                deleted: false,
                tests: None,
                selected: Vec::new(),
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
                    current.tests = resolve(&path, members, sources, index)?;
                }
            }
        } else if line.starts_with("@@ ") {
            let Some(current) = file.as_ref() else {
                return Err(format!("{line}: hunk outside any file"));
            };
            let name = current
                .path
                .as_deref()
                .unwrap_or(current.raw.as_str())
                .to_owned();
            let deleted = current.deleted;
            let (_, old_count, new_start, new_count) = parse_hunk(line, &name)?;
            if !deleted && new_count == 0 {
                let after = new_start
                    .checked_add(1)
                    .ok_or_else(|| format!("{name}: malformed hunk header"))?;
                if let Some(current) = file.as_mut()
                    && let Some(tests) = current.tests.as_ref()
                {
                    select(&Hunk::Gap(new_start, after), tests, &mut current.selected);
                }
            }
            hunk = Some((old_count, new_count, new_start));
        }
    }
    if let Some(current) = file
        && let Some(path) = current.path
    {
        out.push((path, current.selected));
    }
    Ok(out)
}

/// One test `cargo nextest list --message-format json` reported as
/// matching the filter. A test the binary did not compile is absent.
struct ListedTest {
    package: String,
    binary_id: String,
    kind: String,
    name: String,
}

/// The plan after intersecting `planned` with the runner's list: whether
/// any test remains, the filter to run, and the label of each dropped test.
pub(crate) struct RunnerPlan {
    pub(crate) run: bool,
    pub(crate) filter: String,
    pub(crate) dropped: Vec<String>,
}

/// `cargo nextest list` JSON: the testcases whose `filter-match` status is
/// `matches`. Anything else, including `mismatch`, is not in the plan.
fn matching_tests(json: &str) -> Result<Vec<ListedTest>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("nextest list: {error}"))?;
    let suites = value
        .get("rust-suites")
        .and_then(serde_json::Value::as_object)
        .ok_or("nextest list: no rust-suites")?;
    let mut listed = Vec::new();
    for suite in suites.values() {
        let package = text_field(suite, "package-name");
        let binary_id = text_field(suite, "binary-id");
        let kind = text_field(suite, "kind");
        let Some(cases) = suite
            .get("testcases")
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for (name, case) in cases {
            let status = case
                .get("filter-match")
                .and_then(|matched| matched.get("status"))
                .and_then(serde_json::Value::as_str);
            if status != Some("matches") {
                continue;
            }
            listed.push(ListedTest {
                package: package.to_owned(),
                binary_id: binary_id.to_owned(),
                kind: kind.to_owned(),
                name: name.clone(),
            });
        }
    }
    Ok(listed)
}

fn text_field<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

fn listed_has(test: &TestRef, listed: &[ListedTest]) -> bool {
    listed.iter().any(|row| {
        row.name == test.name
            && row.package == test.package
            && match &test.binary {
                Some(binary) => row.binary_id == *binary,
                None if test.unit => row.kind == "lib" || row.kind == "bin",
                None => row.kind == "test",
            }
    })
}

/// The planned tests the list contains, and those it does not, each once,
/// in plan order.
fn compiled_tests(planned: &[TestRef], listed: &[ListedTest]) -> (Vec<TestRef>, Vec<TestRef>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for test in planned {
        let dest = if listed_has(test, listed) {
            &mut kept
        } else {
            &mut dropped
        };
        if !dest.contains(test) {
            dest.push(test.clone());
        }
    }
    (kept, dropped)
}

/// The same id the stress plan's `test` line uses, then the test name.
fn test_label(test: &TestRef) -> String {
    let id = test.binary.as_deref().unwrap_or(&test.package);
    format!("{id} {}", test.name)
}

/// One `planned` line `bug-filter` prints and `narrow-plan` reads back.
pub(crate) fn planned_line(test: &TestRef) -> String {
    let binary = test.binary.as_deref().unwrap_or("");
    let unit = if test.unit { "true" } else { "false" };
    format!("planned\t{}\t{binary}\t{unit}\t{}", test.package, test.name)
}

/// The `planned` lines, in order. A line that is not one is an error
/// naming it: a dropped field would plan the wrong test.
pub(crate) fn parse_planned(text: &str) -> Result<Vec<TestRef>, String> {
    let mut planned = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix("planned\t") else {
            return Err(format!("planned line {}: {line}", index + 1));
        };
        let mut fields = rest.split('\t');
        let package = fields.next().unwrap_or("");
        let binary = fields.next().unwrap_or("");
        let unit = fields.next().unwrap_or("");
        let name = fields.next().unwrap_or("");
        if package.is_empty()
            || name.is_empty()
            || fields.next().is_some()
            || (unit != "true" && unit != "false")
        {
            return Err(format!("planned line {}: {line}", index + 1));
        }
        planned.push(TestRef {
            package: package.to_owned(),
            binary: if binary.is_empty() {
                None
            } else {
                Some(binary.to_owned())
            },
            name: name.to_owned(),
            unit: unit == "true",
        });
    }
    Ok(planned)
}

fn exclusion(test: &TestRef) -> String {
    match &test.binary {
        Some(binary) => format!("(binary_id({binary}) & test(={}))", test.name),
        None if test.unit => format!("(package({}) & test(={}))", test.package, test.name),
        None => format!(
            "(package({}) & kind(test) & test(={}))",
            test.package, test.name
        ),
    }
}

/// `expression` minus each dropped test. The original filter stays, so a
/// test the parser cannot name, such as one a macro emits, still runs;
/// only the named tests the runner left out are excluded.
fn without_dropped(expression: &str, dropped: &[TestRef]) -> String {
    let mut filter = format!("({expression})");
    for test in dropped {
        filter.push_str(" & not ");
        filter.push_str(&exclusion(test));
    }
    filter
}

/// Intersect `planned` with the runner's list. An empty remainder runs
/// nothing. A partial remainder keeps `expression` and excludes each
/// dropped test by name.
pub(crate) fn narrow_plan(
    expression: &str,
    planned_text: &str,
    list_json: &str,
) -> Result<RunnerPlan, String> {
    let planned = parse_planned(planned_text)?;
    let listed = matching_tests(list_json)?;
    let (kept, dropped) = compiled_tests(&planned, &listed);
    let labels = dropped.iter().map(test_label).collect();
    if kept.is_empty() {
        return Ok(RunnerPlan {
            run: false,
            filter: String::new(),
            dropped: labels,
        });
    }
    let filter = if dropped.is_empty() {
        expression.to_owned()
    } else {
        without_dropped(expression, &dropped)
    };
    Ok(RunnerPlan {
        run: true,
        filter,
        dropped: labels,
    })
}

/// Every test in `files`, in file order, as selected by the compiler index.
pub(crate) fn tests_in_files(
    files: &[String],
    members: &Members,
    sources: &[RustFile],
    index: &TestIndex,
) -> Result<Vec<TestRef>, String> {
    let mut out = Vec::new();
    for path in files {
        let Some(package) = owner(path, members) else {
            continue;
        };
        let Some(member) = members.get(package) else {
            continue;
        };
        let rel = path.get(member.dir.len() + 1..).unwrap_or_default();
        if member.unrun_tests.iter().any(|unrun| unrun == path) {
            continue;
        }
        let Some(source) = sources.iter().find(|source| source.path == *path) else {
            continue;
        };
        let functions = test_fns(&source.source).map_err(|()| format!("{path}: cannot parse"))?;
        if functions.is_empty() && !rel.starts_with("tests/") {
            continue;
        }
        for binary in index.binaries_for(path) {
            for test in &binary.tests {
                if (functions.is_empty()
                    || functions
                        .iter()
                        .any(|function| test_name_matches(&test.name, &function.name)))
                    && !out.contains(test)
                {
                    out.push(test.clone());
                }
            }
        }
    }
    Ok(out)
}

/// Add each test touched by a hunk, once, in diff order.
fn select(hunk: &Hunk, tests: &FileTests, out: &mut Vec<TestRef>) {
    match tests {
        FileTests::Functions(functions) => {
            for (function, test) in functions {
                if touches(hunk, function.start, function.end) && !out.contains(test) {
                    out.push(test.clone());
                }
            }
        }
    }
}

/// The nextest filter selecting exactly the touched tests, in diff order:
/// a binary-id term for test binaries and a package term for unit tests.
/// Test names and binary ids come from nextest, not a file-layout guess. A
/// unit test in a `#[path]`-declared file can have any module path, yet
/// still runs in package unit tests.
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
