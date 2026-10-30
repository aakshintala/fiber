//! The fixture-start ratchet (`docs/testing/fakes.md`, "Jigs"): every
//! first exec of the checked-in MCP fixture
//! goes through `fakes::warm_up`, or through a start helper that calls
//! it, because the first exec of a freshly linked binary can stall for
//! seconds on macOS. Every Rust `mcp_fixture()` use and jig executable
//! spawn must match a named path/function row in the `sites` table; an
//! unlisted occurrence fails, so a new fixture start cannot enter
//! without an explicit site row.
//!
//! A warmed row names the helper call that must still be present before
//! the fixture execution; removing it fails. Direct `Server::start` rows
//! require the preceding `Setup::warm_fixture()` call, and indirect
//! Bash/flaky wrapper rows require the named helper or test to warm
//! before starting. Path-reader rows never exec the fixture: they pin
//! the reading call and fail when a start enters the same function.
//! Later-part rows name the binaries Parts 2-4 warm; they stay explicit
//! until those parts warm their starts. The shared-start guard itself is
//! proven by `every_start_helper_warms_the_fixture_before_its_deadline`
//! in `crates/mcp/src/start_tests.rs`, which this check does not
//! re-prove: these rows pin the sites that guard covers.
//!
//! The checker parses each file with `proc_macro2` and counts direct
//! `mcp_fixture()` calls by path and enclosing function: comments and
//! string literals never count, and neither do calls to same-named local
//! helpers, which take arguments. Each row states its exact expected
//! call count, so adding a call inside a listed function fails until the
//! row's count changes deliberately. A bare reference to the accessor,
//! such as a function pointer, and a `use ... as ...` rename of it fail
//! outright instead of escaping the count.

mod sites;

use proc_macro2::{Delimiter, TokenStream, TokenTree};

use crate::rules::RustFile;

use sites::{Kind, SITES, Scope, Site};

/// Starts a test binary may not make: a reader that grew one fails.
const STARTS: &[&str] = &[
    "Server::start",
    ".start(",
    "start_expect(",
    "relaunch(",
    "prepare(",
    "Command::new(",
];

/// One failure per unlisted use, missing marker, count mismatch, or
/// reader that grew a start, in path then line order.
pub(crate) fn fixture_starts(files: &[RustFile]) -> Vec<String> {
    fixture_starts_with(files, SITES)
}

/// The checker over `sites`: the production entry passes the checked-in
/// table, and unit tests pass a one-row table so the same input proves
/// both a rejected and an accepted expected count.
fn fixture_starts_with(files: &[RustFile], sites: &[Site]) -> Vec<String> {
    let mut failures = Vec::new();
    let mut by_path: std::collections::BTreeMap<&str, &RustFile> =
        std::collections::BTreeMap::new();
    for file in files {
        by_path.insert(file.path.as_str(), file);
    }
    let mut found: std::collections::BTreeMap<&str, Vec<Occurrence>> =
        std::collections::BTreeMap::new();
    for file in files {
        // Only files naming the accessor or a jig marker can hold an
        // occurrence: a use needs its name in the raw text, even inside
        // a comment or a string the walk then ignores. Skipping the rest
        // keeps the check fast without missing any use.
        if !may_use(&file.source) {
            found.insert(file.path.as_str(), Vec::new());
            continue;
        }
        // A file that does not tokenise could hide a start, so it fails
        // the check rather than being skipped.
        match file.source.parse::<TokenStream>() {
            Ok(stream) => {
                let trees: Vec<TokenTree> = stream.into_iter().collect();
                let mut occurrences = Vec::new();
                walk(&trees, None, &mut occurrences);
                found.insert(file.path.as_str(), occurrences);
            }
            Err(error) => {
                failures.push(format!("{}: does not tokenise as Rust: {error}", file.path));
                found.insert(file.path.as_str(), Vec::new());
            }
        }
    }
    for file in files {
        let lines: Vec<&str> = file.source.lines().collect();
        let empty: Vec<Occurrence> = Vec::new();
        let seen = found.get(file.path.as_str()).unwrap_or(&empty);
        let mut check = Check {
            path: &file.path,
            lines: &lines,
            seen,
            sites,
            failures: &mut failures,
        };
        // One pass per (function, line) holding a call or a jig use, so
        // a line mixing both checks exactly as one old line-scan row did.
        let mut checked: std::collections::BTreeSet<(&str, usize)> =
            std::collections::BTreeSet::new();
        for occurrence in seen {
            let line = occurrence.line;
            match (&occurrence.site, occurrence.what) {
                (None, Use::Alias) | (Some(_), Use::Alias) => {
                    check.failures.push(format!(
                        "{}:{line}: imported alias for `mcp_fixture`; call it directly",
                        check.path,
                    ));
                }
                (None, _) => check.failures.push(format!(
                    "{}:{line}: fixture use outside any function",
                    check.path,
                )),
                (Some((item, _)), Use::Ref) => check.failures.push(format!(
                    "{}:{line}: `{item}` references `mcp_fixture` without calling it; use a direct `mcp_fixture()` call",
                    check.path,
                )),
                (Some((item, def)), _) => {
                    if checked.insert((item.as_str(), line)) {
                        check.site(item, *def, line);
                    }
                }
            }
        }
    }
    // Every row names a function that still exists and holds exactly its
    // expected direct-call count; a renamed function or an added call
    // fails here instead of silently dropping its pin.
    for row in sites {
        let Some(file) = by_path.get(row.path) else {
            failures.push(format!("{}: row names a missing file", row.path));
            continue;
        };
        let lines: Vec<&str> = file.source.lines().collect();
        let Some(def) = fn_def_line(&lines, row.item) else {
            failures.push(format!(
                "{}: row names a missing function `{}`",
                row.path, row.item
            ));
            continue;
        };
        let empty: Vec<Occurrence> = Vec::new();
        let seen = found.get(row.path).unwrap_or(&empty);
        let calls: Vec<&Occurrence> = seen
            .iter()
            .filter(|occurrence| {
                occurrence.what == Use::Call
                    && occurrence
                        .site
                        .as_ref()
                        .is_some_and(|site| site.0 == row.item)
            })
            .collect();
        if calls.len() != row.expected {
            let line = calls.first().map_or(def + 1, |call| call.line);
            failures.push(format!(
                "{}:{line}: `{}` makes {} direct `mcp_fixture()` call(s), expected {}; update the row's expected count",
                row.path,
                row.item,
                calls.len(),
                row.expected,
            ));
        }
        // Row-level invariants need no call to trigger: the warmer still
        // stands guard and the accessor still stays pure when their
        // functions hold no direct call.
        match row.kind {
            Kind::Warmer => {
                if !file_contains_code_line(&lines, row.marker) {
                    failures.push(format!(
                        "{}:{}: `{}` lost its warmer `{}`",
                        row.path,
                        def + 1,
                        row.item,
                        row.marker,
                    ));
                }
            }
            Kind::Def => {
                if body(def, &lines)
                    .iter()
                    .any(|body_line| body_line.contains(row.marker))
                {
                    failures.push(format!(
                        "{}:{}: the pure accessor `{}` grew a `{}` call",
                        row.path,
                        def + 1,
                        row.item,
                        row.marker,
                    ));
                }
            }
            Kind::Later
            | Kind::Warmed
            | Kind::Spec
            | Kind::Program
            | Kind::Reader
            | Kind::Jig
            | Kind::FixtureUser
            | Kind::JigWarmer => {}
        }
    }
    failures.sort();
    failures.dedup();
    failures
}

/// What the walk found at one spot: a direct `mcp_fixture()` call, any
/// other mention of the accessor, a `use ... as ...` rename of it, or a
/// jig binary use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    Call,
    Ref,
    Alias,
    Jig,
}

/// One accessor or jig occurrence: the enclosing function and its
/// 0-based definition line, or neither outside any function; and the
/// 1-based line of the occurrence itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Occurrence {
    site: Option<(String, usize)>,
    line: usize,
    what: Use,
}

/// Every accessor and jig occurrence in `trees`, attributing each to the
/// innermost enclosing `fn`: a nested function owns its own calls, while
/// calls in closures and macro expansions stay with their function.
/// Comments never reach the token stream, and string contents arrive as
/// one opaque literal, so neither can count as a use.
fn walk(trees: &[TokenTree], enclosing: Option<(String, usize)>, out: &mut Vec<Occurrence>) {
    let mut index = 0;
    while let Some(tree) = trees.get(index) {
        if let TokenTree::Ident(name) = tree {
            if name == "fn"
                && let Some(TokenTree::Ident(item)) = trees.get(index + 1)
                && let Some(open) = body_at(trees, index + 2)
                && let Some(TokenTree::Group(group)) = trees.get(open)
            {
                let def = item.span().start().line - 1;
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                walk(&inner, Some((item.to_string(), def)), out);
                index = open + 1;
                continue;
            } else if name == "use" {
                // A `use` item through its `;`: plain imports never count,
                // and an `as` rename of the accessor fails rather than
                // escaping the count.
                let mut end = index;
                while let Some(cell) = trees.get(end) {
                    if matches!(cell, TokenTree::Punct(mark) if mark.as_char() == ';') {
                        break;
                    }
                    end += 1;
                }
                if let Some(line) = trees.get(index..end).and_then(alias_in) {
                    out.push(Occurrence {
                        site: enclosing.clone(),
                        line,
                        what: Use::Alias,
                    });
                }
                index = end + 1;
                continue;
            } else if name == "mcp_fixture" {
                let line = name.span().start().line;
                let site = enclosing.clone();
                if is_definition_name(trees, index) {
                    // `fn mcp_fixture` defines it: the pure accessor or a
                    // local helper. The definition is neither a call nor a use.
                } else if is_empty_call(trees, index) {
                    out.push(Occurrence {
                        site,
                        line,
                        what: Use::Call,
                    });
                } else if is_call(trees, index) {
                    // A call with arguments names a same-named local
                    // helper: the accessor takes none.
                } else {
                    out.push(Occurrence {
                        site,
                        line,
                        what: Use::Ref,
                    });
                }
            } else if name == "example_path" {
                if !is_definition_name(trees, index) && is_empty_call(trees, index) {
                    out.push(Occurrence {
                        site: enclosing.clone(),
                        line: name.span().start().line,
                        what: Use::Jig,
                    });
                }
            } else if name == "ProcessFixture" && !is_definition_name(trees, index) {
                out.push(Occurrence {
                    site: enclosing.clone(),
                    line: name.span().start().line,
                    what: Use::Jig,
                });
            }
        }
        if let TokenTree::Group(group) = tree {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            walk(&inner, enclosing.clone(), out);
        }
        index += 1;
    }
}

/// The index of the `{...}` body the `fn` item opens: the first brace
/// group at or after `from`, or nothing when a `;` ends a declaration
/// without a body. Parameter lists, return types and `where` clauses
/// hold no braces, so the first brace group is the body.
fn body_at(trees: &[TokenTree], from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(tree) = trees.get(at) {
        match tree {
            TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => return Some(at),
            TokenTree::Punct(mark) if mark.as_char() == ';' => return None,
            TokenTree::Group(_)
            | TokenTree::Ident(_)
            | TokenTree::Punct(_)
            | TokenTree::Literal(_) => {}
        }
        at += 1;
    }
    None
}

/// The line of an `mcp_fixture as ...` rename inside the `use` item, if
/// any: the alias hides the accessor from the call count, so the rename
/// itself fails rather than escaping it. Plain imports never count.
fn alias_in(trees: &[TokenTree]) -> Option<usize> {
    let mut index = 0;
    while let Some(tree) = trees.get(index) {
        match tree {
            TokenTree::Ident(name) if name == "mcp_fixture" => {
                if let Some(TokenTree::Ident(next)) = trees.get(index + 1)
                    && next == "as"
                {
                    return Some(name.span().start().line);
                }
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                if let Some(line) = alias_in(&inner) {
                    return Some(line);
                }
            }
            TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
        index += 1;
    }
    None
}

/// Whether `source` can hold a fixture or jig occurrence: its raw text
/// names the accessor or a jig marker.
fn may_use(source: &str) -> bool {
    source.contains("mcp_fixture")
        || source.contains("example_path")
        || source.contains("ProcessFixture")
}

/// Whether the name at `trees[index]` is defined there: the token before
/// it opens the definition, as in `fn mcp_fixture` or
/// `impl ProcessFixture`. A definition names without using.
fn is_definition_name(trees: &[TokenTree], index: usize) -> bool {
    if index == 0 {
        return false;
    }
    matches!(
        trees.get(index - 1),
        Some(TokenTree::Ident(before))
            if before == "fn"
                || before == "struct"
                || before == "enum"
                || before == "union"
                || before == "trait"
                || before == "impl"
                || before == "mod"
                || before == "const"
                || before == "static"
                || before == "type"
    )
}

/// Whether the name at `trees[index]` is called with no arguments: only
/// an empty `()` is the fixture accessor.
fn is_empty_call(trees: &[TokenTree], index: usize) -> bool {
    let Some(TokenTree::Group(group)) = trees.get(index + 1) else {
        return false;
    };
    group.delimiter() == Delimiter::Parenthesis && group.stream().is_empty()
}

/// Whether the name at `trees[index]` is called with arguments: a call
/// with arguments names a same-named local helper, never the
/// zero-argument fixture accessor.
fn is_call(trees: &[TokenTree], index: usize) -> bool {
    let Some(TokenTree::Group(group)) = trees.get(index + 1) else {
        return false;
    };
    group.delimiter() == Delimiter::Parenthesis && !group.stream().is_empty()
}

/// The per-file state one fixture-start check reads and extends.
struct Check<'a> {
    path: &'a str,
    lines: &'a [&'a str],
    seen: &'a [Occurrence],
    sites: &'a [Site],
    failures: &'a mut Vec<String>,
}

impl Check<'_> {
    /// The failures one (function, line) holding a call or jig use earns:
    /// a missing row, a kind the row disallows, a lost warm marker, or a
    /// start a path reader grew.
    fn site(&mut self, item: &str, def: usize, line: usize) {
        let index = line - 1;
        let on_line = |what: Use| {
            self.seen.iter().any(|other| {
                other.site.as_ref().is_some_and(|site| site.0 == item)
                    && other.line == line
                    && other.what == what
            })
        };
        let has_call = on_line(Use::Call);
        let has_jig = on_line(Use::Jig);
        let Some(row) = self
            .sites
            .iter()
            .find(|row| row.path == self.path && row.item == item)
        else {
            self.failures.push(format!(
                "{}:{line}: unlisted fixture start in `{item}`; add an exact-site row",
                self.path,
            ));
            return;
        };
        if has_jig
            && !has_call
            && !matches!(
                row.kind,
                Kind::Jig | Kind::FixtureUser | Kind::JigWarmer | Kind::Later
            )
        {
            self.failures.push(format!(
                "{}:{line}: `{item}` is a jig site but its row is not a jig kind",
                self.path,
            ));
            return;
        }
        if has_call && matches!(row.kind, Kind::Jig | Kind::FixtureUser | Kind::JigWarmer) {
            self.failures.push(format!(
                "{}:{line}: `{item}` uses the fixture but its row is a jig kind",
                self.path,
            ));
            return;
        }
        match row.kind {
            Kind::Later | Kind::Warmer | Kind::Def => {}
            Kind::Warmed
            | Kind::Spec
            | Kind::Program
            | Kind::Reader
            | Kind::Jig
            | Kind::FixtureUser
            | Kind::JigWarmer => {
                let body_lines;
                let window: &[&str] = match row.scope {
                    Scope::BeforeUse => self.lines.get(def..=index).unwrap_or(&[]),
                    Scope::Body => {
                        body_lines = body(def, self.lines);
                        &body_lines
                    }
                    Scope::File => self.lines,
                };
                let holds = match row.scope {
                    Scope::BeforeUse | Scope::Body => window
                        .iter()
                        .any(|window_line| window_line.contains(row.marker)),
                    Scope::File => file_contains_code_line(self.lines, row.marker),
                };
                if !holds {
                    self.failures.push(format!(
                        "{}:{line}: `{item}` lost its `{}` call before the fixture use",
                        self.path, row.marker,
                    ));
                }
                // A reader never starts: the whole body, not just the
                // lines above the use, must hold no start.
                if matches!(row.kind, Kind::Reader)
                    && body(def, self.lines)
                        .iter()
                        .any(|body_line| STARTS.iter().any(|start| body_line.contains(start)))
                {
                    self.failures.push(format!(
                        "{}:{line}: path reader `{item}` grew a start",
                        self.path,
                    ));
                }
            }
        }
    }
}

/// The index of the first line defining `item`: the row-existence pin
/// reads the table, not the walk, so a renamed function fails there
/// instead of silently dropping its pin.
fn fn_def_line(lines: &[&str], item: &str) -> Option<usize> {
    lines
        .iter()
        .position(|line| is_fn_def(line) && fn_name(line).as_deref() == Some(item))
}

/// Whether the line defines a function: `fn`, `pub fn`, `pub(crate)
/// fn` and `async fn` openers, never `use` lines or comments.
fn is_fn_def(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
        return false;
    }
    if !(trimmed.starts_with("fn ") || trimmed.starts_with("pub") || trimmed.starts_with("async "))
    {
        return false;
    }
    if !trimmed.contains("fn ") || !trimmed.contains('(') {
        return false;
    }
    // A `pub` opener names the function itself, not a later call: the
    // first `fn ` starts the name.
    !trimmed.starts_with("pub") || trimmed.find("fn ").is_some()
}

/// The function name a definition line defines.
fn fn_name(line: &str) -> Option<String> {
    let at = line.find("fn ")? + 3;
    line[at..]
        .split(|cell: char| !cell.is_alphanumeric() && cell != '_')
        .find(|part| !part.is_empty())
        .map(str::to_owned)
}

/// Whether any trimmed code line starts with `marker`, so source-text
/// self-mentions inside string literals never count as the call.
fn file_contains_code_line(lines: &[&str], marker: &str) -> bool {
    lines
        .iter()
        .any(|line| line.trim_start().starts_with(marker))
}

/// The function body from its definition line through the next
/// column-zero `}` or definition-level opener.
fn body<'a>(start: usize, lines: &'a [&'a str]) -> Vec<&'a str> {
    let mut end = lines.len();
    for (offset, line) in lines.iter().enumerate().skip(start + 1) {
        if *line == "}"
            || line.trim_start().starts_with("#[")
            || is_fn_def(line)
            || line.trim_start().starts_with("impl ")
            || line.trim_start().starts_with("mod ")
        {
            end = offset;
            break;
        }
    }
    lines.get(start..end).map_or_else(Vec::new, <[_]>::to_vec)
}

#[cfg(test)]
#[path = "fixture_starts_tests.rs"]
mod tests;
