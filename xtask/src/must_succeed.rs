//! The must-succeed bound check (`docs/testing.md`, "Waits and timeouts"): a
//! test file holds no wait bound of 1 s up to `fakes::MUST_SUCCEED_WITHIN`
//! except the kept bounds in `ALLOWANCES`. A shorter bound fires as a timing
//! assertion under load instead of the hang guard it is meant to be.

use std::time::Duration;

use proc_macro2::{Delimiter, TokenStream, TokenTree};

use crate::rules::RustFile;
use crate::select::is_test_file;

/// The shared must-succeed bound, `fakes::MUST_SUCCEED_WITHIN`. The check
/// reads no Rust source for it; an xtask test asserts the two are equal.
pub(crate) const FLOOR: Duration = Duration::from_secs(10);

/// One kept bound: a `const` by name, or every site in a file that an open
/// pull request holds.
pub(crate) struct Allowance {
    pub(crate) path: &'static str,
    /// `None` covers every site in `path`. Only a file another open pull
    /// request holds gets one, and its reason names that pull request.
    pub(crate) name: Option<&'static str>,
    pub(crate) reason: &'static str,
}

const fn kept(path: &'static str, name: &'static str, reason: &'static str) -> Allowance {
    Allowance {
        path,
        name: Some(name),
        reason,
    }
}

const fn held(path: &'static str, reason: &'static str) -> Allowance {
    Allowance {
        path,
        name: None,
        reason,
    }
}

const READS: &str = "code under test reads it";
const GRACE: &str = "fake-clock grace";
const STEP: &str = "fake-clock step";
const SLICE: &str = "poll slice";
const ABSENT: &str = "nothing arrives";

pub(crate) const ALLOWANCES: &[Allowance] = &[
    kept("crates/cli/src/hub_status_tests.rs", "ANSWER", READS),
    kept("crates/extensions/tests/lua.rs", "LOAD", READS),
    kept(
        "crates/extensions/tests/pinned_depends.rs",
        "REARM_STALL",
        READS,
    ),
    kept("crates/fakes/src/rerun_tests.rs", "REAP", READS),
    kept(
        "crates/jobs/src/delegate/message_resume_tests.rs",
        "BOUND",
        READS,
    ),
    kept("crates/jobs/src/delegate/message_tests.rs", "BOUND", READS),
    kept("crates/extensions/src/search_tests.rs", "GRACE", GRACE),
    kept(
        "crates/extensions/tests/credential_headers.rs",
        "GRACE",
        GRACE,
    ),
    kept("crates/extensions/tests/login_discovery.rs", "GRACE", GRACE),
    kept("crates/extensions/tests/lua.rs", "GRACE", GRACE),
    kept("crates/extensions/tests/lua_tools.rs", "GRACE", GRACE),
    kept("crates/extensions/tests/oauth.rs", "GRACE", GRACE),
    kept("crates/main/src/read_signals_tests.rs", "HELD_FOR", STEP),
    kept("crates/doors/tests/support/mod.rs", "SLICE", SLICE),
    kept("crates/hub/src/closed_tests.rs", "SLICE", SLICE),
    kept("crates/hub/src/rejoin_tests.rs", "SLICE", SLICE),
    kept("crates/main/tests/repository_offer.rs", "SLICE", SLICE),
    kept("crates/fakes/src/finished_tests.rs", "SHORT", ABSENT),
    kept("crates/main/src/read_signals_tests.rs", "SHORT", ABSENT),
    kept(
        "crates/fakes/src/deadline_tests.rs",
        "SPAN",
        "value under test",
    ),
    kept(
        "crates/fakes/src/provider_server_tests.rs",
        "ABSENT",
        ABSENT,
    ),
    kept(
        "crates/net/src/websocket/tls_tests.rs",
        "NO_DATA_AFTER_REJECTION",
        ABSENT,
    ),
    kept("crates/cli/src/doctor/files_tests.rs", "ABSENT", ABSENT),
    held("crates/cli/src/extension_test_tests.rs", "held by #3103"),
    held("crates/extensions/src/hooks_tests.rs", "held by #3028"),
    held("crates/extensions/src/host_tests.rs", "held by #3028"),
    held("crates/extensions/src/host/exec_tests.rs", "held by #3028"),
    held("crates/extensions/src/lua_tests.rs", "held by #3028"),
    held(
        "crates/extensions/src/lua/schedule_tests.rs",
        "held by #3028",
    ),
    held("crates/main/src/case/run_tests.rs", "held by #3028"),
    held("crates/extensions/tests/host_model.rs", "held by #3107"),
    held("crates/extensions/tests/lua_hooks.rs", "held by #3112"),
    held("crates/tools/tests/shell.rs", "held by #3112"),
    held("crates/hub/src/idle_tests.rs", "held by #3009"),
    held("crates/hub/src/port_tests.rs", "held by #3009"),
    held("crates/hub/src/rewind_tests.rs", "held by #3009"),
    held("crates/main/tests/completion.rs", "held by #3009"),
    held("crates/fakes/src/jobs_tests.rs", "held by #3001"),
    held("crates/jobs/src/delegate/run_tests.rs", "held by #3001"),
    held("crates/jobs/src/registry_tests.rs", "held by #3001"),
    held("crates/jobs/src/timing_tests.rs", "held by #3001"),
    held("crates/loop/src/inbox_tests.rs", "held by #3001"),
    held("crates/loop/tests/support/mod.rs", "held by #3001"),
    held("crates/main/tests/delegates.rs", "held by #3001"),
    held("crates/extensions/src/lua_cost_tests.rs", "held by #3116"),
    held(
        "crates/extensions/src/lua_provider/quota_tests.rs",
        "held by #3116",
    ),
    held("crates/loop/src/retry_tests.rs", "held by #3127"),
    held("crates/loop/src/timing_tests.rs", "held by #3127"),
    held("crates/provider/src/scripted_tests.rs", "held by #3127"),
    held(
        "crates/main/src/switch/read_tests.rs",
        "changed on main by #3097",
    ),
    held(
        "crates/tools/src/picture_timing_tests.rs",
        "changed on main by #3132",
    ),
];

/// Calls whose top-level arguments may hold a wait bound, by name.
const WAIT_CALLS: &[&str] = &[
    "await_requests",
    "wait_line",
    "recv_timeout",
    "wait_hang_waiting",
    "wait_hang_matched",
];

/// Methods whose timeout is the second argument, after the guard.
const WAIT_METHODS: &[&str] = &["wait_timeout", "wait_timeout_while"];

/// Socket setters, whose timeout is the one argument of `Some(..)`.
const SOCKET_SETTERS: &[&str] = &["set_read_timeout", "set_write_timeout"];

/// One failure line per parse error, short bound and stale named allowance,
/// in path then line order. Only test files are scanned.
pub(crate) fn must_succeed(files: &[RustFile], allowances: &[Allowance]) -> Vec<String> {
    let floor_ms = FLOOR.as_millis();
    let mut scanned: Vec<&RustFile> = files.iter().filter(|f| is_test_file(&f.rel)).collect();
    scanned.sort_by(|a, b| a.path.cmp(&b.path));
    let mut failures = Vec::new();
    let mut hits: Vec<(&str, &str)> = Vec::new();
    for file in &scanned {
        let trees = match tokens(&file.source) {
            Ok(trees) => trees,
            Err(e) => {
                failures.push(format!("{}: does not tokenise as Rust: {e}", file.path));
                continue;
            }
        };
        let mut found = Vec::new();
        walk(&trees, floor_ms, &mut found);
        found.sort_by_key(|site| site.line);
        let whole_file = allowances
            .iter()
            .any(|a| a.path == file.path && a.name.is_none());
        for site in found {
            if whole_file {
                continue;
            }
            let allowed = allowances.iter().find(|a| {
                a.path == file.path && a.name.is_some() && a.name == site.name.as_deref()
            });
            match allowed {
                Some(allowance) => hits.push((allowance.path, allowance.name.unwrap_or_default())),
                None => failures.push(format!(
                    "{}:{}: {} MUST_SUCCEED_WITHIN ({})",
                    file.path,
                    site.line,
                    site.what,
                    show(floor_ms)
                )),
            }
        }
    }
    let mut stale: Vec<(&str, &str, String)> = allowances
        .iter()
        .filter_map(|a| {
            let name = a.name?;
            let scanned_here = scanned.iter().any(|f| f.path == a.path);
            (scanned_here && !hits.contains(&(a.path, name))).then(|| {
                (
                    a.path,
                    name,
                    format!(
                        "allowance {} {name} (reason: {}) matches no short bound",
                        a.path, a.reason
                    ),
                )
            })
        })
        .collect();
    stale.sort();
    failures.extend(stale.into_iter().map(|(_, _, line)| line));
    failures
}

/// A bound below the floor, found in a file.
struct Site {
    line: usize,
    /// The const's name; `None` for an inline literal.
    name: Option<String>,
    /// What the bound is, in the words of the failure line.
    what: String,
}

fn tokens(source: &str) -> Result<Vec<TokenTree>, String> {
    let stream: TokenStream = source
        .parse()
        .map_err(|e: proc_macro2::LexError| e.to_string())?;
    Ok(stream.into_iter().collect())
}

/// Each short bound in `trees`, groups included.
fn walk(trees: &[TokenTree], floor_ms: u128, found: &mut Vec<Site>) {
    for (i, tree) in trees.iter().enumerate() {
        found.extend(site_at(trees, i, floor_ms));
        if let TokenTree::Group(group) = tree {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            walk(&inner, floor_ms, found);
        }
    }
}

fn site_at(trees: &[TokenTree], i: usize, floor_ms: u128) -> Option<Site> {
    let TokenTree::Ident(ident) = trees.get(i)? else {
        return None;
    };
    if ident == "const" {
        return const_site(trees, i, floor_ms);
    }
    let TokenTree::Group(call) = trees.get(i + 1)? else {
        return None;
    };
    if call.delimiter() != Delimiter::Parenthesis {
        return None;
    }
    let word = ident.to_string();
    let inner: Vec<TokenTree> = call.stream().into_iter().collect();
    let args = split_top_level(&inner);
    let method = i > 0 && is_punct(trees.get(i - 1), '.');
    let what = if word == "after" && deadline_path(trees, i) {
        format!("Deadline::after({})", short_in(&args, floor_ms, true)?)
    } else if WAIT_CALLS.contains(&word.as_str()) {
        format!("{word}(.., {})", short_in(&args, floor_ms, false)?)
    } else if method && WAIT_METHODS.contains(&word.as_str()) {
        let second = args.get(1..2).unwrap_or(&[]);
        format!("{word}(.., {})", short_in(second, floor_ms, false)?)
    } else if method && word == "min" {
        format!(".min({})", short_in(&args, floor_ms, true)?)
    } else if method && SOCKET_SETTERS.contains(&word.as_str()) {
        let [part] = args.as_slice() else {
            return None;
        };
        let [TokenTree::Ident(some), TokenTree::Group(wrapped)] = part else {
            return None;
        };
        if some != "Some" || wrapped.delimiter() != Delimiter::Parenthesis {
            return None;
        }
        let wrapped: Vec<TokenTree> = wrapped.stream().into_iter().collect();
        let bound = short_in(&split_top_level(&wrapped), floor_ms, true)?;
        format!("{word}(Some({bound}))")
    } else {
        return None;
    };
    Some(Site {
        line: ident.span().start().line,
        name: None,
        what: format!("{what} is below"),
    })
}

/// A `const NAME: Duration = Duration::from_secs(N)` (or `from_millis`) at
/// `trees[i]`, when it is in range.
fn const_site(trees: &[TokenTree], i: usize, floor_ms: u128) -> Option<Site> {
    let TokenTree::Ident(name) = trees.get(i + 1)? else {
        return None;
    };
    if !is_punct(trees.get(i + 2), ':') {
        return None;
    }
    let eq = (i + 3..trees.len().min(i + 16)).find(|&j| is_punct(trees.get(j), '='))?;
    if !is_ident(trees.get(eq - 1), "Duration") {
        return None;
    }
    let end = (eq + 1..trees.len())
        .find(|&j| is_punct(trees.get(j), ';'))
        .unwrap_or(trees.len());
    let ms = duration_ms(trees.get(eq + 1..end)?)?;
    in_range(ms, floor_ms).then(|| Site {
        line: name.span().start().line,
        name: Some(name.to_string()),
        what: format!("const {name} is {}, below", show(ms)),
    })
}

/// The spelled literal of the one argument in `parts` that is a short bound.
/// `single` requires exactly one argument; otherwise any argument may be the
/// literal.
fn short_in(parts: &[&[TokenTree]], floor_ms: u128, single: bool) -> Option<String> {
    if single && parts.len() != 1 {
        return None;
    }
    parts.iter().find_map(|part| {
        let ms = duration_ms(part)?;
        in_range(ms, floor_ms).then(|| spelled(part))
    })
}

fn in_range(ms: u128, floor_ms: u128) -> bool {
    (1000..floor_ms).contains(&ms)
}

/// Whether `trees[i]` is the `after` of `Deadline::after`, with any path
/// before `Deadline`.
fn deadline_path(trees: &[TokenTree], i: usize) -> bool {
    i >= 3
        && is_ident(trees.get(i - 3), "Deadline")
        && is_punct(trees.get(i - 2), ':')
        && is_punct(trees.get(i - 1), ':')
}

fn split_top_level(args: &[TokenTree]) -> Vec<&[TokenTree]> {
    args.split(|tree| is_punct(Some(tree), ','))
        .filter(|part| !part.is_empty())
        .collect()
}

/// The value in ms of `[path::]Duration::from_secs(N)` or `from_millis(N)`
/// and nothing else. A path before `Duration`, such as `std::time::`, is
/// allowed.
fn duration_ms(tokens: &[TokenTree]) -> Option<u128> {
    let [path @ .., duration, c1, c2, from, TokenTree::Group(call)] = tokens else {
        return None;
    };
    if !(is_ident(Some(duration), "Duration")
        && is_punct(Some(c1), ':')
        && is_punct(Some(c2), ':')
        && call.delimiter() == Delimiter::Parenthesis)
    {
        return None;
    }
    let path_is_segments = path.chunks(3).all(|segment| {
        matches!(segment, [TokenTree::Ident(_), a, b] if is_punct(Some(a), ':') && is_punct(Some(b), ':'))
    });
    if !path_is_segments || path.len() % 3 != 0 {
        return None;
    }
    let inner: Vec<TokenTree> = call.stream().into_iter().collect();
    let [number] = inner.as_slice() else {
        return None;
    };
    let n = u128::from(single_integer(number)?);
    match from.to_string().as_str() {
        "from_secs" => Some(n * 1000),
        "from_millis" => Some(n),
        _ => None,
    }
}

/// `Duration::from_secs(5)` as written, without a path.
fn spelled(tokens: &[TokenTree]) -> String {
    let [.., duration, _, _, from, TokenTree::Group(call)] = tokens else {
        return String::new();
    };
    format!("{duration}::{from}({})", call.stream())
}

/// The value of an integer literal, `_` separators and an unsigned or
/// signed type suffix allowed.
fn single_integer(tree: &TokenTree) -> Option<u64> {
    let TokenTree::Literal(literal) = tree else {
        return None;
    };
    let text = literal.to_string().replace('_', "");
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    let rest = &text[digits.len()..];
    if !(rest.is_empty() || rest.starts_with('u') || rest.starts_with('i')) {
        return None;
    }
    digits.parse().ok()
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
#[path = "must_succeed_tests.rs"]
mod tests;
