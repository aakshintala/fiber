//! The no-raised-budget check (#3662, rule 3): a diff that raises `WAITS`,
//! `BUDGET` or `CLEANUP`, or adds a `Deadline::after` of 60 s or more in a
//! test, fails unless the line carries `// ruling: #N`. A bound the check
//! cannot read — a shared name like `fakes::deadline::BUDGET`, or a computed
//! `Duration` — fails the same way: only the known short bounds (`WAIT`,
//! `MUST_SUCCEED_WITHIN`, `Duration::ZERO`) and direct literals below 60 s
//! pass unruled. Only lines changed against the merge base are scanned, as
//! Rust tokens like `must-succeed` does (comments never count); unchanged
//! lines never fail.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{Delimiter, TokenStream, TokenTree};

use crate::rules::RustFile;
use crate::select::is_test_file;

/// The shared per-wait guard, `support::deadline::WAITS`. A test asserts it
/// equals the real const; the check reads no Rust source for it.
pub(crate) const WAITS: std::time::Duration = std::time::Duration::from_secs(90);
/// The shared hang guard, `support::deadline::BUDGET`.
pub(crate) const BUDGET: std::time::Duration = std::time::Duration::from_secs(110);
/// The shared cleanup bound, `support::deadline::CLEANUP`.
pub(crate) const CLEANUP: std::time::Duration = std::time::Duration::from_secs(100);

/// A `Deadline::after` bound at or above this in a test file fails.
const AFTER_FLOOR_MS: u128 = 60_000;

/// Runs `budget-bounds --base REV`: the violations on lines changed against
/// the merge base with REV, as `path:line` lines.
pub(crate) fn budget_bounds(base: &str) -> Result<Vec<String>, String> {
    let members = crate::workspace_members()?;
    let changed = changed_lines(base)?;
    Ok(check_files(&crate::rust_files(&members)?, &changed))
}

/// Lines changed against the merge base with `base`, by repository path:
/// added lines are 1-based new-file lines; an untracked file counts whole.
pub(crate) fn changed_lines(base: &str) -> Result<BTreeMap<String, BTreeSet<usize>>, String> {
    let merge_base = crate::git_lines(&["merge-base", base, "HEAD"])?
        .into_iter()
        .next()
        .ok_or("no merge base")?;
    let mut changed: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    let mut path = String::new();
    for line in crate::output("git", &["diff", "-U0", &merge_base])?.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            path = rest.strip_prefix("b/").unwrap_or(rest).to_owned();
        } else if let Some(hunk) = line.strip_prefix("@@ ") {
            if path == "/dev/null" || !path.ends_with(".rs") {
                continue;
            }
            let new_range = hunk.split_whitespace().find_map(|part| {
                let rest = part.strip_prefix('+')?;
                let (start, count) = match rest.split_once(',') {
                    Some((start, count)) => (start, count),
                    None => (rest, "1"),
                };
                Some((start.parse::<usize>().ok()?, count.parse::<usize>().ok()?))
            });
            if let Some((start, count)) = new_range {
                changed
                    .entry(path.clone())
                    .or_default()
                    .extend(start..start + count);
            }
        }
    }
    for path in crate::git_lines(&["ls-files", "--others", "--exclude-standard"])? {
        if !path.ends_with(".rs") {
            continue;
        }
        let lines = std::fs::read_to_string(&path)
            .map_err(|e| format!("{path}: {e}"))?
            .lines()
            .count();
        changed.entry(path).or_default().extend(1..=lines.max(1));
    }
    Ok(changed)
}

/// Each violation in `files` on a changed line, in path then line order.
/// `changed` maps repository paths to their changed 1-based lines.
pub(crate) fn check_files(
    files: &[RustFile],
    changed: &BTreeMap<String, BTreeSet<usize>>,
) -> Vec<String> {
    let mut scanned: Vec<&RustFile> = files
        .iter()
        .filter(|f| changed.contains_key(&f.path))
        .collect();
    scanned.sort_by(|a, b| a.path.cmp(&b.path));
    let mut failures = Vec::new();
    for file in &scanned {
        let trees = match tokens(&file.source) {
            Ok(trees) => trees,
            Err(e) => {
                failures.push(format!("{}: does not tokenise as Rust: {e}", file.path));
                continue;
            }
        };
        let test = is_test_file(&file.rel);
        let mut scan = Scan {
            durations: vec!["Duration".to_string()],
        };
        scan.collect_aliases(&trees);
        let mut found = Vec::new();
        walk(&trees, &scan, test, &mut found);
        found.sort_by_key(|site| site.line);
        let Some(changed_here) = changed.get(&file.path) else {
            continue;
        };
        let source_lines: Vec<&str> = file.source.lines().collect();
        for site in found {
            if !changed_here.contains(&site.line) {
                continue;
            }
            let ruled = source_lines
                .get(site.line.wrapping_sub(1))
                .is_some_and(|line| line.contains("// ruling: #"));
            if !ruled {
                failures.push(format!("{}:{}: {}", file.path, site.line, site.what));
            }
        }
    }
    failures
}

/// A raised bound or long `after`, found in a file.
struct Site {
    line: usize,
    /// What the bound is, in the words of the failure line.
    what: String,
}

/// What a file's bounds are read against.
struct Scan {
    /// `Duration` and each name the file gives it with `use .. Duration as NAME`.
    durations: Vec<String>,
}

impl Scan {
    fn collect_aliases(&mut self, trees: &[TokenTree]) {
        for (i, tree) in trees.iter().enumerate() {
            if let TokenTree::Group(group) = tree {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                self.collect_aliases(&inner);
            } else if is_ident(Some(tree), "Duration")
                && is_ident(trees.get(i + 1), "as")
                && let Some(TokenTree::Ident(alias)) = trees.get(i + 2)
            {
                self.durations.push(alias.to_string());
            }
        }
    }

    fn is_duration(&self, tree: Option<&TokenTree>) -> bool {
        matches!(tree, Some(TokenTree::Ident(ident)) if self.durations.contains(&ident.to_string()))
    }
}

fn tokens(source: &str) -> Result<Vec<TokenTree>, String> {
    let stream: TokenStream = source
        .parse()
        .map_err(|e: proc_macro2::LexError| e.to_string())?;
    Ok(stream.into_iter().collect())
}

/// Each raised bound and long `after` in `trees`, groups included.
fn walk(trees: &[TokenTree], scan: &Scan, test: bool, found: &mut Vec<Site>) {
    for (i, tree) in trees.iter().enumerate() {
        if let Some(site) = site_at(trees, i, scan, test) {
            found.push(site);
        }
        if let TokenTree::Group(group) = tree {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            walk(&inner, scan, test, found);
        }
    }
}

fn site_at(trees: &[TokenTree], i: usize, scan: &Scan, test: bool) -> Option<Site> {
    let TokenTree::Ident(ident) = trees.get(i)? else {
        return None;
    };
    if ident == "const" {
        return const_site(trees, i, scan);
    }
    if !test {
        return None;
    }
    let TokenTree::Group(call) = trees.get(i + 1)? else {
        return None;
    };
    if call.delimiter() != Delimiter::Parenthesis {
        return None;
    }
    if ident != "after" || !deadline_path(trees, i) {
        return None;
    }
    let inner: Vec<TokenTree> = call.stream().into_iter().collect();
    let args = split_top_level(&inner);
    if args.len() != 1 {
        return None;
    }
    let line = ident.span().start().line;
    let arg = args.first()?;
    if let Some((ms, spelling)) = duration(arg, scan) {
        return (ms >= AFTER_FLOOR_MS).then(|| Site {
            line,
            what: format!("Deadline::after({spelling}) is 60 s or more"),
        });
    }
    // Resolve only a complete symbolic path; compound or unknown expressions
    // fail closed so an unshown bound needs its ruling.
    let spelling: TokenStream = arg.iter().cloned().collect();
    match simple_path_ident(arg).as_deref().and_then(known_ms) {
        Some(false) => None,
        Some(true) => Some(Site {
            line,
            what: format!("Deadline::after({spelling}) is 60 s or more"),
        }),
        None => Some(Site {
            line,
            what: format!("Deadline::after({spelling}) is not shown below 60 s"),
        }),
    }
}

/// A `const WAITS|BUDGET|CLEANUP: Duration = <duration>` at `trees[i]`,
/// when the duration is above the shared bound it names.
fn const_site(trees: &[TokenTree], i: usize, scan: &Scan) -> Option<Site> {
    let mut declaration = trees.get(i..)?.iter().skip(1);
    let TokenTree::Ident(name) = declaration.next()? else {
        return None;
    };
    let cap = match name.to_string().as_str() {
        "WAITS" => WAITS,
        "BUDGET" => BUDGET,
        "CLEANUP" => CLEANUP,
        _ => return None,
    };
    if !is_punct(declaration.next(), ':') {
        return None;
    }
    let mut declared_type = Vec::new();
    let mut found_initializer = false;
    for tree in declaration
        .by_ref()
        .take_while(|tree| !is_punct(Some(tree), ';'))
    {
        if is_punct(Some(tree), '=') {
            found_initializer = true;
            break;
        }
        declared_type.push(tree.clone());
    }
    if !found_initializer {
        return None;
    }
    if !scan.is_duration(declared_type.last()) {
        return None;
    }
    let rhs: Vec<TokenTree> = declaration
        .take_while(|tree| !is_punct(Some(tree), ';'))
        .cloned()
        .collect();
    let line = name.span().start().line;
    match duration(&rhs, scan) {
        Some((ms, _)) if ms <= cap.as_millis() => None,
        Some((ms, _)) => Some(Site {
            line,
            what: format!(
                "const {name} is {}, above {}",
                show(ms),
                show(cap.as_millis())
            ),
        }),
        // A computed or symbolic bound the check cannot read: fail closed.
        None => Some(Site {
            line,
            what: format!(
                "const {name} is not shown at or below {}",
                show(cap.as_millis())
            ),
        }),
    }
}

/// The bound in ms named by the last path segment `name`, when the lane
/// knows whether it is below the 60 s floor: the shared guards are long,
/// and the one-wait bounds are short. An xtask test pins them to their consts.
fn known_ms(name: &str) -> Option<bool> {
    match name {
        "WAITS" | "BUDGET" | "CLEANUP" | "MAX" => Some(true),
        "WAIT" | "MUST_SUCCEED_WITHIN" | "ZERO" => Some(false),
        _ => None,
    }
}

/// The final identifier of a complete Rust path, optionally absolute.
/// Compound expressions and other token sequences fail closed.
fn simple_path_ident(tokens: &[TokenTree]) -> Option<String> {
    let tokens = match tokens {
        [first, second, rest @ ..] if is_punct(Some(first), ':') && is_punct(Some(second), ':') => {
            rest
        }
        _ => tokens,
    };
    let TokenTree::Ident(first) = tokens.first()? else {
        return None;
    };
    let mut last = first.to_string();
    let mut i = 1;
    while i < tokens.len() {
        if !is_punct(tokens.get(i), ':') || !is_punct(tokens.get(i + 1), ':') {
            return None;
        }
        let TokenTree::Ident(ident) = tokens.get(i + 2)? else {
            return None;
        };
        last = ident.to_string();
        i += 3;
    }
    Some(last)
}

fn split_top_level(args: &[TokenTree]) -> Vec<&[TokenTree]> {
    args.split(|tree| is_punct(Some(tree), ','))
        .filter(|part| !part.is_empty())
        .collect()
}

/// The value in ms, and the call as written without its path, of
/// `[::][path::]Duration::from_secs(N)` or `from_millis(N)` and nothing else.
/// A path before `Duration`, such as `std::time::` or `::std::time::`, is
/// allowed; aliases and other expressions fail closed.
fn duration(tokens: &[TokenTree], scan: &Scan) -> Option<(u128, String)> {
    let [path @ .., duration, c1, c2, from, TokenTree::Group(call)] = tokens else {
        return None;
    };
    if !scan.is_duration(Some(duration))
        || !is_punct(Some(c1), ':')
        || !is_punct(Some(c2), ':')
        || call.delimiter() != Delimiter::Parenthesis
    {
        return None;
    }
    let path = match path {
        [root_a, root_b, rest @ ..]
            if is_punct(Some(root_a), ':') && is_punct(Some(root_b), ':') =>
        {
            rest
        }
        _ => path,
    };
    let path_is_segments = path.chunks(3).all(|segment| {
        matches!(segment, [TokenTree::Ident(_), a, b] if is_punct(Some(a), ':') && is_punct(Some(b), ':'))
    });
    if !path_is_segments {
        return None;
    }
    let inner: Vec<TokenTree> = call.stream().into_iter().collect();
    let [number] = inner.as_slice() else {
        return None;
    };
    let n = u128::from(single_integer(number)?);
    let ms = match from.to_string().as_str() {
        "from_secs" => n * 1000,
        "from_millis" => n,
        _ => return None,
    };
    Some((ms, format!("{duration}::{from}({})", call.stream())))
}

/// The value of an integer literal, `_` separators, radix prefixes and an
/// integer type suffix. Other literals fail closed.
fn single_integer(tree: &TokenTree) -> Option<u64> {
    let TokenTree::Literal(literal) = tree else {
        return None;
    };
    let text = literal.to_string().replace('_', "");
    let (radix, body) = match text.get(..2) {
        Some("0x") => (16, &text[2..]),
        Some("0o") => (8, &text[2..]),
        Some("0b") => (2, &text[2..]),
        _ => (10, text.as_str()),
    };
    let digits: String = body.chars().take_while(|c| c.is_digit(radix)).collect();
    let rest = &body[digits.len()..];
    if !(rest.is_empty() || rest.starts_with('u') || rest.starts_with('i')) {
        return None;
    }
    u64::from_str_radix(&digits, radix).ok()
}

/// Whether `trees[i]` is the `after` of `Deadline::after`, with any path
/// before `Deadline`.
fn deadline_path(trees: &[TokenTree], i: usize) -> bool {
    i >= 3
        && is_ident(trees.get(i - 3), "Deadline")
        && is_punct(trees.get(i - 2), ':')
        && is_punct(trees.get(i - 1), ':')
}

fn show(ms: u128) -> String {
    if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{ms} ms")
    }
}

fn is_punct(tree: Option<&TokenTree>, ch: char) -> bool {
    matches!(tree, Some(TokenTree::Punct(punct)) if punct.as_char() == ch)
}

fn is_ident(tree: Option<&TokenTree>, word: &str) -> bool {
    matches!(tree, Some(TokenTree::Ident(ident)) if ident == word)
}

#[cfg(test)]
#[path = "budget_bounds_tests.rs"]
mod tests;
