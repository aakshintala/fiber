//! The stale-wait ratchet (#3664): each file under `crates/main/tests/`
//! holds at most its inline maximum of stale-wait occurrences, outside
//! `crates/main/tests/support/`. A stale-wait occurrence is a source line
//! holding the identifier `recv_reply`, the text `fn subscribe(` or the
//! text `fn subscribe_full(`, other than a line whose trimmed start is a
//! line comment, or a stale wait in the admitted shape (#4102): a `full`
//! subscribe on a session that can replay, then the test's action, then
//! an unbounded wait for that action's event carrying no reopen-derived
//! bound, where a replayed earlier occurrence could still be unread at
//! the wait's position. A consuming wait positioned after the replay
//! drains it; a keeping read preserves it. Replay exposure comes from
//! executable flow, never a comment.
//!
//! The count is conservative: an occurrence counts even inside a string
//! literal or a block comment, and only a full-line `//` comment line
//! exempts it. Literal false positives are safe because the ratchet is an
//! upper bound: over-counting a use inside a string never lets a real
//! `recv_reply` or local `fn subscribe` slip past. A file missing from the
//! maxima table holds none. `scripts/check` keeps the table inline and
//! passes it here on stdin; this module owns occurrence detection and the
//! per-file comparison.

use std::collections::{BTreeMap, BTreeSet};

use crate::rules::RustFile;

// debt: the scope constants, the scope test and the sort-then-compare loop
// repeat screen_ratchet's; a third ratchet that scopes by path prefix moves
// both into one shared helper.

/// Files in scope: Rust files under `crates/main/tests/`, outside `support/`.
const SCOPE_PREFIX: &str = "crates/main/tests/";
/// The driver and its helpers live here; the rule does not apply to them.
const SUPPORT_PREFIX: &str = "crates/main/tests/support/";
/// Suffix of the files the ratchet scans.
const RUST_SUFFIX: &str = ".rs";
/// A wait that can answer from the fold's replay instead of the live
/// stream: it takes the next line whatever it is, where `recv_answer`
/// matches by command id.
const REPLY: &str = "recv_reply";
/// A subscribe that replays the log without sharing its `last_seq` bound,
/// under either name: a later wait cannot tell replay from live.
const SUBSCRIBE: &str = "fn subscribe(";
const SUBSCRIBE_FULL: &str = "fn subscribe_full(";

/// Whether `path` is a file the ratchet scans.
fn is_scoped(path: &str) -> bool {
    path.starts_with(SCOPE_PREFIX)
        && path.ends_with(RUST_SUFFIX)
        && !path.starts_with(SUPPORT_PREFIX)
}

/// Each file in scope holding more stale-wait occurrences than its maximum
/// (0 when the table names no maximum for it), in path order. Each failure
/// names the file, the count and the maximum.
pub(crate) fn check(files: &[RustFile], maxima: &BTreeMap<String, usize>) -> Vec<String> {
    let mut scoped: Vec<&RustFile> = files.iter().filter(|file| is_scoped(&file.path)).collect();
    scoped.sort_by(|a, b| a.path.cmp(&b.path));
    let mut failures = Vec::new();
    for file in scoped {
        let count = count_stale(&file.source);
        let maximum = maxima.get(&file.path).copied().unwrap_or(0);
        if count > maximum {
            failures.push(format!(
                "{} holds {count} stale-wait occurrences (maximum {maximum})",
                file.path
            ));
        }
    }
    failures
}

/// Whether a byte can sit beside an identifier: letters, digits and `_`.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether the byte at `at` in `bytes` is an identifier character: past
/// either end never is, so an edge match keeps its outer side. A
/// non-ASCII byte belongs to a multibyte character, so decode it and ask
/// whether it is a letter, a digit or `_` in any script.
fn is_word_at(bytes: &[u8], at: usize) -> bool {
    let Some(&byte) = bytes.get(at) else {
        return false;
    };
    if byte < 0x80 {
        return is_word_byte(byte);
    }
    // The lead byte of the character covering `at`: the first byte at or
    // below `at` that is not a multibyte continuation byte. A hand-stepped
    // walk here spins forever under a stepping mutant, so this walks with
    // an iterator instead (`docs/testing/mutants.md`, "Proving a test
    // bites").
    let start = (0..=at)
        .rev()
        .find(|&i| bytes.get(i).is_none_or(|byte| byte & 0xC0 != 0x80))
        .unwrap_or(0);
    bytes
        .get(start..)
        .and_then(|suffix| std::str::from_utf8(suffix).ok())
        .and_then(|text| text.chars().next())
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// The occurrences of the identifier `needle` in `line`: a match counts
/// only when the characters either side are not identifier characters, so
/// a longer name never counts.
fn count_ident(line: &str, needle: &str) -> usize {
    let bytes = line.as_bytes();
    let mut count = 0;
    let mut offset = 0;
    while let Some(at) = line[offset..].find(needle) {
        let start = offset + at;
        let end = start + needle.len();
        if (start == 0 || !is_word_at(bytes, start - 1)) && !is_word_at(bytes, end) {
            count += 1;
        }
        offset = end;
    }
    count
}

/// The stale-wait occurrences in `source`: per line, the whole-identifier
/// `recv_reply` uses plus the `fn subscribe(` and `fn subscribe_full(`
/// definitions, plus one per stale wait in the admitted shape. A line
/// whose trimmed start is a line comment never counts; a use inside a
/// string literal or a block comment still does, keeping the count a
/// conservative upper bound.
pub(crate) fn count_stale(source: &str) -> usize {
    let kept: usize = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(|line| {
            count_ident(line, REPLY)
                + line.matches(SUBSCRIBE).count()
                + line.matches(SUBSCRIBE_FULL).count()
        })
        .sum();
    kept + count_shape(source)
}

/// Subscribe spellings that send: the wire command travels inside a
/// `support::subscribe*` call, a hand-sent line, or a constructor call,
/// never a helper-name list, so a rename cannot walk past the check.
const SUB_CALLS: [&str; 5] = [
    "subscribe",
    "subscribe_as",
    "subscribe_direct",
    "subscribe_client",
    "subscribe_line",
];

/// Event kinds the fold replays: durable lines carry `seq`.
const DURABLE_KINDS: [&str; 52] = [
    "archived",
    "assistant_message_completed",
    "assistant_message_started",
    "context_added",
    "context_nudged",
    "delegate_finished",
    "delegate_started",
    "directory_added",
    "extensions_loaded",
    "extension_exec",
    "extension_state_set",
    "extension_state_unset",
    "fiber_exited",
    "fiber_started",
    "compaction_completed",
    "compaction_started",
    "instruction_file",
    "interaction_requested",
    "interaction_resolved",
    "job_completed",
    "job_line",
    "job_started",
    "jobs_pending_notified",
    "mcp_server_failed",
    "mcp_server_ready",
    "model_changed",
    "moved",
    "opening_message",
    "permission_requested",
    "permission_resolved",
    "preamble_built",
    "quota_noticed",
    "quota_rearmed",
    "reasoning_completed",
    "reasoning_started",
    "reloaded",
    "repository_code_offered",
    "repository_code_resolved",
    "reviewer_kept",
    "rewound",
    "session_named",
    "session_started",
    "shell_command",
    "skills_changed",
    "skills_resent",
    "step_started",
    "steering_applied",
    "text_completed",
    "tool_call_completed",
    "tool_call_requested",
    "tool_call_started",
    "turn_completed",
];

/// Event kinds no fold replays: ephemeral lines carry no `seq`, and the
/// hub's own lines name no session.
const EPHEMERAL_KINDS: [&str; 22] = [
    "assistant_message_delta",
    "attention",
    "clients",
    "command_accepted",
    "command_rejected",
    "device_changed",
    "extension_hello",
    "extension_log",
    "extension_message",
    "extension_request",
    "extension_ui",
    "hub_hello",
    "job_delta",
    "notice",
    "reasoning_delta",
    "retry_scheduled",
    "session_left",
    "session_status",
    "steering_queue",
    "stream_reopened",
    "tool_call_arguments_delta",
    "tool_call_delta",
];

/// Named consuming waits whose match region decides whether they drain
/// the replay: a resume marker, an ephemeral kind, or a live-only
/// acknowledgement match.
const DRAIN_WAITS: [&str; 11] = [
    "until_a",
    "until_ack",
    "until_after",
    "until_after_reopen",
    "until_child_started",
    "until_close",
    "until_hub",
    "until_resumed",
    "until_replayed",
    "until_session",
    "wait_for_effect_after_accept",
];

/// Calls that pass the waited line to another function for matching are
/// opaque filters, except the option constructors, which store it.
const OPAQUE_EXCEPTIONS: [&str; 10] = [
    "Err", "Ok", "Some", "else", "for", "if", "loop", "match", "return", "while",
];

/// Whether `byte` can sit beside an identifier.
fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The starts of whole-identifier `needle` matches in `line`.
fn ident_ats(line: &str, needle: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    let mut ats = Vec::new();
    let mut offset = 0;
    while let Some(at) = line[offset..].find(needle) {
        let start = offset + at;
        let end = start + needle.len();
        let before_ok = start == 0
            || !bytes
                .get(start - 1)
                .is_some_and(|byte| is_ident_byte(*byte));
        let after_ok = !bytes.get(end).is_some_and(|byte| is_ident_byte(*byte));
        if before_ok && after_ok {
            ats.push(start);
        }
        offset = end;
    }
    ats
}

/// Whether the identifier at `at` opens a call: an optional run of
/// spaces or tabs, then `(`.
fn is_call_at(line: &str, at: usize, needle_len: usize) -> bool {
    let mut rest = line.get(at + needle_len..).unwrap_or("");
    rest = rest.trim_start_matches([' ', '\t']);
    rest.starts_with('(')
}

/// Whether `line` defines a function: an optional `pub(...)`, `pub` or
/// `async` prefix, then `fn` and a name, at any indent.
fn fn_name(line: &str) -> Option<&str> {
    fn strip_keyword<'a>(mut rest: &'a str, keyword: &str) -> Option<&'a str> {
        rest = rest.strip_prefix(keyword)?;
        match rest.as_bytes().first() {
            None => None,
            Some(byte) if is_ident_byte(*byte) => None,
            Some(_) => Some(rest),
        }
    }
    let mut rest = line.trim_start();
    if rest.starts_with("pub") {
        rest = strip_keyword(rest, "pub")?.trim_start();
        if let Some(after_paren) = rest.strip_prefix('(') {
            rest = after_paren.split(')').nth(1)?.trim_start();
        }
    }
    if rest.starts_with("async") {
        rest = strip_keyword(rest, "async")?.trim_start();
    }
    rest = strip_keyword(rest, "fn")?.trim_start();
    let end = rest
        .as_bytes()
        .iter()
        .position(|byte| !is_ident_byte(*byte))
        .unwrap_or(rest.len());
    let name = rest.get(..end).unwrap_or("");
    if name.is_empty() { None } else { Some(name) }
}

/// Whether `line` opens a top-level function: a definition with no
/// leading indent.
fn is_top_fn_def(line: &str) -> bool {
    !line.starts_with([' ', '\t']) && fn_name(line).is_some()
}

/// Whether `line` is a `use` declaration.
fn is_use_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("use ") || trimmed.starts_with("pub use ")
}

/// The end of the balanced group opened at `lines[start_line][start_col]`:
/// the line and column of its closing bracket. Comments, string and
/// character literals never contribute depth; the scan stops 200 lines
/// past its start. In body mode the stop check runs at each line's end,
/// so a definition line's own balanced parentheses never end the body.
/// Whether a closing bracket ends the scan: outside a body, with an
/// opener seen and every depth back at zero. Dropping the mode gate
/// only differs for a zero-depth closer in body mode, where it returns
/// the closer's column instead of the line's end; no caller reads that
/// column (`fn_bodies` takes the line, and `call_region` never sets
/// body mode), so no test can tell them apart and it takes this
/// exemption instead (`docs/testing/mutants.md`, "Proving a test
/// bites"). The other forms break balanced scans, and the group tests
/// pin those.
#[cfg_attr(false, mutants::skip)]
fn group_closed(body_mode: bool, started: bool, depth: [i32; 3]) -> bool {
    !body_mode && started && depth == [0, 0, 0]
}

/// Whether `bytes` opens a byte-string literal at `col` (`b"`): the
/// `b` prefix only skips the quote's opener. Missing the opener still
/// reaches the identical scanner state, since the stray `b` steps past
/// as an ordinary byte and the `"` then opens the string, so the `+`
/// to `*` mutant on `col + 1` is equivalent: no return-value test can
/// distinguish the live `b"` path from the dead-window path, and only
/// crash mutants (`-`, overflow) are observable there
/// (`docs/testing/mutants.md`, "Proving a test bites").
#[cfg_attr(false, mutants::skip)]
fn is_byte_string_opener(bytes: &[u8], col: usize) -> bool {
    bytes.get(col) == Some(&b'b') && bytes.get(col + 1) == Some(&b'"')
}

fn scan_group_end(
    lines: &[&str],
    start_line: usize,
    start_col: usize,
    body_mode: bool,
) -> (usize, usize) {
    let mut depth: [i32; 3] = [0, 0, 0];
    let mut started = false;
    // At most 201 lines past the start: a hand-stepped line counter spins
    // forever under a stepping mutant, so this walks with an iterator
    // instead (`docs/testing/mutants.md`, "Proving a test bites").
    for (offset, line) in lines.iter().skip(start_line).take(201).enumerate() {
        let line_no = start_line + offset;
        let bytes = line.as_bytes();
        let mut col = if line_no == start_line { start_col } else { 0 };
        let mut instr: Option<u8> = None;
        let mut escaped = false;
        let mut raw_hashes: Option<usize> = None;
        let mut in_block = false;
        // At most one step per byte, plus the closing bracket: a
        // hand-stepped byte counter spins forever under a stepping
        // mutant, so the loop is fuel-bounded instead
        // (`docs/testing/mutants.md`, "Proving a test bites").
        for _ in 0..=bytes.len() {
            let Some(&byte) = bytes.get(col) else {
                break;
            };
            if in_block {
                if byte == b'*' && bytes.get(col + 1) == Some(&b'/') {
                    in_block = false;
                    col += 1;
                }
            } else if instr.is_some() {
                if raw_hashes.is_some() {
                    let hashes = raw_hashes.unwrap_or(0);
                    if byte == b'"' && (0..hashes).all(|h| bytes.get(col + 1 + h) == Some(&b'#')) {
                        instr = None;
                        raw_hashes = None;
                        col += hashes;
                    }
                } else if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    instr = None;
                }
            } else if byte == b'/' && bytes.get(col + 1) == Some(&b'/') {
                break;
            } else if byte == b'/' && bytes.get(col + 1) == Some(&b'*') {
                in_block = true;
                col += 1;
            } else if byte == b'"' {
                instr = Some(b'"');
            } else if is_byte_string_opener(bytes, col) {
                instr = Some(b'"');
                col += 1;
            } else if byte == b'r' && matches!(bytes.get(col + 1), Some(b'"') | Some(b'#')) {
                let hashes = bytes.get(col + 1..).map_or(0, |tail| {
                    tail.iter().take_while(|&&byte| byte == b'#').count()
                });
                if bytes.get(col + 1 + hashes) == Some(&b'"') {
                    instr = Some(b'r');
                    raw_hashes = Some(hashes);
                    col += hashes + 1;
                }
            } else if byte == b'\'' {
                // A character hides its bracket; stepping through an
                // escaped character reaches the same state as skipping
                // it, so there is no branch to mutate here.
                let rest = bytes.get(col..).unwrap_or(&[]);
                let plain = rest.len() >= 3 && rest.get(2) == Some(&b'\'');
                if plain {
                    col += 2;
                }
            } else if byte == b'(' {
                depth[0] += 1;
                started = true;
            } else if byte == b'[' {
                depth[1] += 1;
                started = true;
            } else if byte == b'{' {
                depth[2] += 1;
                started = true;
            } else if byte == b')' {
                depth[0] -= 1;
                if group_closed(body_mode, started, depth) {
                    return (line_no, col);
                }
            } else if byte == b']' {
                depth[1] -= 1;
                if group_closed(body_mode, started, depth) {
                    return (line_no, col);
                }
            } else if byte == b'}' {
                depth[2] -= 1;
                if group_closed(body_mode, started, depth) {
                    return (line_no, col);
                }
            }
            col += 1;
        }
        if body_mode && started && depth == [0, 0, 0] {
            return (line_no, bytes.len());
        }
    }
    (lines.len().saturating_sub(1), 0)
}

/// The source text of the call whose `(` stands at or after column
/// `at` on `lines[line_no]`, through its closing bracket.
fn call_region(lines: &[&str], line_no: usize, at: usize) -> String {
    let Some(line) = lines.get(line_no) else {
        return String::new();
    };
    // The opening paren at or after the call: a hand-stepped scan spins
    // forever under a stepping mutant, so this searches with an iterator
    // instead (`docs/testing/mutants.md`, "Proving a test bites").
    let open = line
        .as_bytes()
        .iter()
        .enumerate()
        .skip(at)
        .find(|(_, byte)| **byte == b'(')
        .map_or(line.len(), |(open, _)| open);
    if line.as_bytes().get(open).is_none() {
        return String::new();
    }
    let (end_line, end_col) = scan_group_end(lines, line_no, open, false);
    let mut region = String::new();
    // Down to the closing line: a hand-stepped line counter spins
    // forever under a stepping mutant, so the loop walks with an
    // iterator instead (`docs/testing/mutants.md`, "Proving a test
    // bites").
    for (no, text) in lines.iter().enumerate().skip(line_no) {
        if no > end_line {
            break;
        }
        if no == line_no && no == end_line {
            region.push_str(text.get(open..=end_col).unwrap_or(""));
        } else if no == line_no {
            region.push_str(text.get(open..).unwrap_or(""));
        } else if no == end_line {
            region.push_str(text.get(..=end_col).unwrap_or(""));
        } else {
            region.push_str(text);
        }
        if no != end_line {
            region.push('\n');
        }
    }
    region
}

/// The bodies of every function in `lines`, by name: same-name bodies
/// concatenate, so a helper's sends count under any one of them.
fn fn_bodies(lines: &[&str]) -> BTreeMap<String, String> {
    let mut bodies = BTreeMap::new();
    // Skipping past each body with an index walks one past the end
    // forever under a stepping mutant, so the loop walks with an
    // iterator instead (`docs/testing/mutants.md`, "Proving a test
    // bites").
    let mut skip_until = 0;
    for (line_no, line) in lines.iter().enumerate() {
        if line_no < skip_until {
            continue;
        }
        let Some(name) = fn_name(line) else {
            continue;
        };
        let (end_line, _) = scan_group_end(lines, line_no, 0, true);
        let mut body = String::new();
        for text in lines.iter().take(end_line + 1).skip(line_no) {
            body.push_str(text);
            body.push('\n');
        }
        bodies
            .entry(name.to_owned())
            .and_modify(|old: &mut String| old.push_str(&body))
            .or_insert(body);
        skip_until = end_line + 1;
    }
    bodies
}

/// Lints `until`, subscribe-helper and `send` renames out of the `use`
/// declarations: `use support::{until as wait_for};` waits through
/// `wait_for`.
#[derive(Default)]
struct Aliases {
    until: BTreeSet<String>,
    subscribe: BTreeSet<String>,
    send: BTreeSet<String>,
}

/// The `ORIG as NAME` pairs inside one `use` statement.
fn use_aliases(lines: &[&str]) -> Aliases {
    let mut aliases = Aliases::default();
    let mut statement = String::new();
    let mut depth = 0;
    let mut in_use = false;
    for line in lines {
        let trimmed = line.trim_start();
        if !in_use {
            if !(trimmed.starts_with("use ") || trimmed.starts_with("pub use ")) {
                continue;
            }
            in_use = true;
        }
        statement.push_str(trimmed);
        statement.push(' ');
        depth += trimmed
            .as_bytes()
            .iter()
            .filter(|byte| **byte == b'{')
            .count();
        depth -= trimmed
            .as_bytes()
            .iter()
            .filter(|byte| **byte == b'}')
            .count();
        if depth > 0 || !trimmed.ends_with(';') {
            continue;
        }
        in_use = false;
        let bytes = statement.as_bytes();
        // Each `ORIG as NAME` pair, left to right: the scan walks with
        // iterators, since a hand-stepped cursor spins forever under a
        // stepping mutant (`docs/testing/mutants.md`, "Proving a test
        // bites").
        for (cursor, _) in statement.match_indices(" as ") {
            let start = bytes
                .get(..cursor)
                .and_then(|head| head.iter().rposition(|byte| !is_ident_byte(*byte)))
                .map_or(0, |i| i + 1);
            let end = cursor
                + 4
                + bytes.get(cursor + 4..).map_or(0, |tail| {
                    tail.iter().take_while(|b| is_ident_byte(**b)).count()
                });
            let original = statement.get(start..cursor).unwrap_or("");
            let renamed = statement.get(cursor + 4..end).unwrap_or("");
            if original == "until" {
                aliases.until.insert(renamed.to_owned());
            }
            if SUB_CALLS.contains(&original) {
                aliases.subscribe.insert(renamed.to_owned());
            }
            if original == "send" {
                aliases.send.insert(renamed.to_owned());
            }
        }
        statement.clear();
    }
    aliases
}

/// The receiver of the method call at `at`, or the callee's first
/// argument: the connection a subscribe sends on or a wait reads.
fn socket_of_call(line: &str, at: usize, needle_len: usize) -> String {
    let bytes = line.as_bytes();
    // The receiver just before the call, past any blanks: hand-stepped
    // walks back spin forever under stepping mutants, so this slices
    // with iterators instead (`docs/testing/mutants.md`, "Proving a test
    // bites").
    let blanks = bytes.get(..at).map_or(0, |head| {
        head.iter()
            .rev()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count()
    });
    let head = bytes.get(..at.saturating_sub(blanks)).unwrap_or(&[]);
    if head.last() == Some(&b'.') {
        let core = head.get(..head.len().saturating_sub(1)).unwrap_or(&[]);
        let start = core
            .iter()
            .rposition(|byte| matches!(byte, b' ' | b'\t' | b';' | b'(' | b','))
            .map_or(0, |i| i + 1);
        return strip_borrow(
            core.get(start..)
                .and_then(|suffix| std::str::from_utf8(suffix).ok())
                .unwrap_or(""),
        );
    }
    strip_borrow(&first_argument(line, at, needle_len))
}

/// The callee's first argument: the text up to the top-level `,` or
/// `)`, without nesting, strings, raw strings or character literals.
fn first_argument(line: &str, at: usize, needle_len: usize) -> String {
    let rest = line.get(at + needle_len..).unwrap_or("");
    let trimmed = rest.trim_start_matches([' ', '\t']);
    if !trimmed.starts_with('(') {
        return String::new();
    }
    first_argument_from(line, line.len() - trimmed.len() + 1)
}

/// The first argument starting just past `(` at `start` in `text`: the
/// scan crosses lines, so a call region's first argument reads even when
/// the call spans lines. Each step consumes input, and balance counts
/// opens against closes, so a stepping mutant fails fast instead of
/// spinning (`docs/testing/mutants.md`, "Proving a test bites").
fn first_argument_from(text: &str, start: usize) -> String {
    let mut rest = text.get(start..).unwrap_or("");
    let initial = rest.len();
    let mut opens = 0;
    let mut closes = 0;
    let mut instr: Option<u8> = None;
    let mut raw_hashes: Option<usize> = None;
    // At most one step per byte: a stalled step spins forever under a
    // stepping mutant, so the loop is fuel-bounded instead
    // (`docs/testing/mutants.md`, "Proving a test bites").
    for _ in 0..=initial {
        let Some(&byte) = rest.as_bytes().first() else {
            break;
        };
        let mut advance = 1;
        if instr.is_some() {
            if raw_hashes.is_some() {
                let hashes = raw_hashes.unwrap_or(0);
                // The verified `#`s are inert: consuming them one by one
                // reaches the same state as skipping them, so the close
                // takes no count to mutate here.
                if byte == b'"' && (0..hashes).all(|h| rest.as_bytes().get(1 + h) == Some(&b'#')) {
                    instr = None;
                    raw_hashes = None;
                }
            } else if byte == b'\\' {
                advance = 2;
            } else if Some(byte) == instr {
                instr = None;
            }
        } else if byte == b'"' {
            instr = Some(b'"');
        } else if byte == b'\'' {
            let tail = rest.as_bytes();
            let escaped =
                tail.len() >= 4 && tail.get(1) == Some(&b'\\') && tail.get(3) == Some(&b'\'');
            let plain = tail.len() >= 3 && tail.get(2) == Some(&b'\'');
            if escaped {
                advance = 4;
            } else if plain {
                advance = 3;
            }
        } else if byte == b'r' && matches!(rest.as_bytes().get(1), Some(b'"') | Some(b'#')) {
            let hashes = rest
                .as_bytes()
                .get(1..)
                .map_or(0, |tail| tail.iter().take_while(|&&b| b == b'#').count());
            if rest.as_bytes().get(1 + hashes) == Some(&b'"') {
                instr = Some(b'r');
                raw_hashes = Some(hashes);
                advance = hashes + 2;
            }
        } else if byte == b'(' || byte == b'[' || byte == b'{' {
            opens += 1;
        } else if byte == b')' || byte == b']' || byte == b'}' {
            if opens == closes {
                break;
            }
            closes += 1;
        } else if byte == b',' && opens == closes {
            break;
        }
        rest = match rest.get(advance..) {
            Some(next) => next,
            None => return String::new(),
        };
    }
    text.get(start..start + (initial - rest.len()))
        .unwrap_or("")
        .to_owned()
}

/// The first argument of a call region, which starts at its `(`.
fn region_first_argument(region: &str) -> String {
    if region.as_bytes().first() != Some(&b'(') {
        return String::new();
    }
    first_argument_from(region, 1)
}

/// The expression without its borrows: `&client` reads `client`'s
/// stream, and so does `client`.
fn strip_borrow(expression: &str) -> String {
    let mut rest = expression.trim();
    loop {
        if let Some(after) = rest.strip_prefix('&') {
            rest = after.trim_start();
            continue;
        }
        match rest.split_once([' ', '\t']) {
            Some(("mut", tail)) => rest = tail.trim_start(),
            _ => break,
        }
    }
    rest.to_owned()
}

/// The connection a wire `"subscribe"` line subscribes: the sender on
/// its own line, else the sender whose region covers the line, else
/// none. The search stops at the enclosing top-level function.
fn wire_socket(lines: &[&str], line_no: usize) -> Option<String> {
    let line = lines.get(line_no)?;
    for name in SUB_CALLS.iter().chain(["send"].iter()) {
        for at in ident_ats(line, name) {
            if is_call_at(line, at, name.len()) {
                return Some(socket_of_call(line, at, name.len()));
            }
        }
    }
    // The enclosing top-level definition: the last one at or above the
    // line. A hand-stepped search spins forever under a stepping mutant,
    // so this walks with iterators instead (`docs/testing/mutants.md`,
    // "Proving a test bites").
    let floor = lines
        .get(..line_no)
        .and_then(|head| head.iter().rposition(|line| is_top_fn_def(line)))
        .unwrap_or(0);
    for above in (floor..line_no).rev().take(16) {
        let Some(candidate) = lines.get(above) else {
            break;
        };
        if candidate.trim_start().starts_with("//") || is_use_line(candidate) {
            continue;
        }
        if fn_name(candidate).is_some() {
            continue;
        }
        if let Some(at) = ident_ats(candidate, "send")
            .into_iter()
            .find(|at| is_call_at(candidate, *at, "send".len()))
        {
            let region = call_region(lines, above, at);
            if region.matches('\n').count() + above >= line_no {
                return Some(socket_of_call(candidate, at, "send".len()));
            }
            break;
        }
    }
    None
}

/// Fresh session starts: a `start_session` helper call without
/// `--resume`, or the wire `start` command. A wait past one is on a
/// session with no pre-subscribe history.
fn reset_lines(lines: &[&str]) -> Vec<usize> {
    let mut resets = Vec::new();
    for (line_no, line) in lines.iter().enumerate() {
        if !is_executable_line(line) {
            continue;
        }
        for at in ident_ats(line, "start_session") {
            if is_call_at(line, at, "start_session".len()) {
                if !call_region(lines, line_no, at).contains("--resume") {
                    resets.push(line_no);
                }
                break;
            }
        }
        let flat: String = line.chars().filter(|c| *c != '\\').collect();
        if let Some(command) = flat.find("\"command\"") {
            let mut rest = flat.get(command + "\"command\"".len()..).unwrap_or("");
            rest = rest.trim_start_matches([' ', '\t']);
            if let Some(after_colon) = rest.strip_prefix(':') {
                rest = after_colon.trim_start_matches([' ', '\t']);
                if rest.starts_with("\"start\"") {
                    resets.push(line_no);
                }
            }
        }
    }
    resets
}

/// The event kinds a `line["kind"] == "..."` predicate names.
fn wait_kinds(region: &str) -> Vec<String> {
    let mut kinds = Vec::new();
    // Every `"kind"]` match, left to right: the scan walks with an
    // iterator, since a hand-stepped cursor spins forever under a
    // stepping mutant (`docs/testing/mutants.md`, "Proving a test
    // bites").
    for (found, _) in region.match_indices("\"kind\"]") {
        let mut rest = region.get(found + "\"kind\"]".len()..).unwrap_or("");
        rest = rest.trim_start_matches([' ', '\t', '\n']);
        if let Some(after_eq) = rest.strip_prefix("==") {
            rest = after_eq.trim_start_matches([' ', '\t', '\n']);
            if let Some(quoted) = rest.strip_prefix('"') {
                let end = quoted
                    .as_bytes()
                    .iter()
                    .position(|byte| !is_ident_byte(*byte))
                    .unwrap_or(quoted.len());
                let kind = quoted.get(..end).unwrap_or("");
                if quoted.as_bytes().get(end) == Some(&b'"') && !kind.is_empty() {
                    kinds.push(kind.to_owned());
                }
            }
        }
    }
    kinds
}

/// Whether the wait's match region delegates the line to another
/// function: an opaque session, command or acknowledgement match hides
/// inside that helper.
fn delegates_match(region: &str) -> bool {
    let bytes = region.as_bytes();
    // Every call, left to right: the scan walks with iterators, since a
    // hand-stepped cursor spins forever under a stepping mutant
    // (`docs/testing/mutants.md`, "Proving a test bites").
    for (open, _) in region.match_indices('(') {
        let start = bytes
            .get(..open)
            .and_then(|head| head.iter().rposition(|byte| !is_ident_byte(*byte)))
            .map_or(0, |i| i + 1);
        let name = region.get(start..open).unwrap_or("");
        if name.is_empty() || OPAQUE_EXCEPTIONS.contains(&name) {
            continue;
        }
        if matches!(bytes.get(start.wrapping_sub(1)), Some(b'.') | Some(b':')) {
            continue;
        }
        let after = open
            + 1
            + bytes.get(open + 1..).map_or(0, |tail| {
                tail.iter()
                    .take_while(|b| matches!(**b, b' ' | b'\t' | b'\n'))
                    .count()
            });
        if region.get(after..).is_some_and(|rest| {
            rest.starts_with("line")
                && rest
                    .as_bytes()
                    .get(4)
                    .is_none_or(|byte| !is_ident_byte(*byte))
        }) {
            return true;
        }
    }
    false
}

/// Whether the wait carries no reopen-derived bound and awaits a live
/// durable event: no acknowledgement, command, turn or session filter,
/// no resumed filter, no delegated match, and at least one durable
/// kind with no ephemeral one.
fn wait_is_open(region: &str) -> bool {
    if region.contains("command_accepted") {
        return false;
    }
    if delegates_match(region) {
        return false;
    }
    if region.contains("command_id") || region.contains("session_id") || region.contains("turn_id")
    {
        return false;
    }
    if region.contains("\"resumed\"") || region.contains(".resumed") {
        return false;
    }
    let kinds = wait_kinds(region);
    if kinds.is_empty() {
        return false;
    }
    if kinds
        .iter()
        .any(|kind| EPHEMERAL_KINDS.contains(&kind.as_str()))
    {
        return false;
    }
    kinds
        .iter()
        .any(|kind| DURABLE_KINDS.contains(&kind.as_str()))
}

/// Whether the line drains the replay for a later wait on
/// `expected_socket`: a bound wait ending at a live line, or a consuming
/// wait for a resume marker, an ephemeral kind, or a live-only
/// acknowledgement match, read on the same connection. A wait on another
/// connection reads that connection's stream, never this replay.
fn is_drain(lines: &[&str], line_no: usize, aliases: &Aliases, expected_socket: &str) -> bool {
    let Some(line) = lines.get(line_no) else {
        return false;
    };
    if !is_executable_line(line) {
        return false;
    }
    let mut waited: Option<(usize, &str, usize)> = None;
    for at in ident_ats(line, "until") {
        if is_call_at(line, at, "until".len()) {
            waited = Some((line_no, "until", at));
            break;
        }
    }
    if waited.is_none() {
        for name in DRAIN_WAITS
            .iter()
            .copied()
            .chain(aliases.until.iter().map(|name| name.as_str()))
        {
            let found = ident_ats(line, name)
                .into_iter()
                .find(|at| is_call_at(line, *at, name.len()));
            if let Some(at) = found {
                waited = Some((line_no, name, at));
                break;
            }
        }
    }
    let Some((found_line, name, at)) = waited else {
        return false;
    };
    let region = call_region(lines, found_line, at);
    if drain_socket(line, &region, at, name.len()) != expected_socket {
        return false;
    }
    if ["until_after", "until_after_reopen", "until_close"].contains(&name) {
        return true;
    }
    if region.contains("\"resumed\"")
        || region.contains(".resumed")
        || region.contains("stream_reopened")
        || region.contains("session_resumed")
        || ident_ats(line, "until_resumed")
            .into_iter()
            .any(|at| is_call_at(line, at, "until_resumed".len()))
    {
        return true;
    }
    let kinds = wait_kinds(&region);
    if !kinds.is_empty()
        && kinds
            .iter()
            .all(|kind| EPHEMERAL_KINDS.contains(&kind.as_str()))
    {
        return true;
    }
    region.contains("command_id") || region.contains("command_accepted")
}

/// The connection a drain reads: the call's receiver on its own line,
/// else the call's first argument, which may stand on a following line
/// when the call spans lines.
fn drain_socket(line: &str, region: &str, at: usize, needle_len: usize) -> String {
    let bytes = line.as_bytes();
    // The blanks just before the call: a hand-stepped cursor stalls
    // forever under a stepping mutant, so this walks with an iterator
    // as socket_of_call does (`docs/testing/mutants.md`, "Proving a
    // test bites").
    let blanks = bytes.get(..at).map_or(0, |head| {
        head.iter()
            .rev()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count()
    });
    let cursor = at.saturating_sub(blanks);
    if cursor > 0 && bytes.get(cursor - 1) == Some(&b'.') {
        return socket_of_call(line, at, needle_len);
    }
    strip_borrow(&region_first_argument(region))
}

/// Strict line order: a subscribe never shares its wait's line, since
/// the replay point and the action must both sit strictly between the
/// two, and it never shares its action's line either, since each line
/// holds one shape point at most (a subscribe line returns before the
/// action scan). The `<=` variant therefore counts exactly the same
/// pairs, and no test can tell them apart; it takes this exemption
/// instead (`docs/testing/mutants.md`, "Proving a test bites"). The
/// `==` and `>` variants break valid pairs, and the shape tests pin
/// those.
#[cfg_attr(false, mutants::skip)]
fn is_before_strict(before: usize, after: usize) -> bool {
    before < after
}

/// Executable flow, not scaffolding: a comment, a `use` line or a
/// function definition never replays, resets or drains anything. No
/// line is all three at once, so the `&&` forms below accept nothing
/// and no test can tell them apart; they take this exemption instead
/// (`docs/testing/mutants.md`, "Proving a test bites"). The `||`
/// forms accept scaffolding as flow, and the shape tests pin those.
#[cfg_attr(false, mutants::skip)]
fn is_executable_line(line: &str) -> bool {
    !(line.trim_start().starts_with("//") || is_use_line(line) || fn_name(line).is_some())
}

/// One stale wait per unbounded `until` in the admitted shape: a
/// subscribe on the same connection before it, a replay point and the
/// test's action between them, and no same-connection drain from the
/// replay point on.
fn count_shape(source: &str) -> usize {
    let lines: Vec<&str> = source.lines().collect();
    let bodies = fn_bodies(&lines);
    let senders: BTreeSet<&str> = bodies
        .iter()
        .filter(|(_, body)| body.contains(".send("))
        .map(|(name, _)| name.as_str())
        .collect();
    let aliases = use_aliases(&lines);
    let mut subscribes: Vec<usize> = Vec::new();
    let mut sockets: BTreeMap<usize, String> = BTreeMap::new();
    let mut actions: Vec<usize> = Vec::new();
    let mut waits: Vec<(usize, usize, String)> = Vec::new();
    let mut replay_points: Vec<usize> = Vec::new();
    for (line_no, line) in lines.iter().enumerate() {
        // Replay exposure comes from executable flow: a resumed, reopened
        // or rewound session the test runs. A comment naming one, a `use`
        // line or a function definition never replays anything.
        if !is_executable_line(line) {
            continue;
        }
        if line.contains("resum") || line.contains("reopen") || line.contains("rewind") {
            replay_points.push(line_no);
        }
        let flat: String = line.chars().filter(|c| *c != '\\').collect();
        if flat.contains("\"subscribe\"") {
            if !flat.contains("\"summary\"") {
                subscribes.push(line_no);
                sockets.insert(line_no, wire_socket(&lines, line_no).unwrap_or_default());
            }
            continue;
        }
        let mut subscribed = false;
        for name in SUB_CALLS
            .iter()
            .copied()
            .chain(aliases.subscribe.iter().map(|name| name.as_str()))
        {
            if ident_ats(line, name)
                .into_iter()
                .any(|at| is_call_at(line, at, name.len()))
            {
                if !(line.contains("summary") && line.contains("level")) {
                    subscribes.push(line_no);
                    if let Some(at) = ident_ats(line, name)
                        .into_iter()
                        .find(|at| is_call_at(line, *at, name.len()))
                    {
                        sockets.insert(line_no, socket_of_call(line, at, name.len()));
                    }
                }
                subscribed = true;
                break;
            }
        }
        if subscribed {
            continue;
        }
        if line.contains(".send(") {
            actions.push(line_no);
            continue;
        }
        let mut acted = false;
        for name in ["send"]
            .iter()
            .copied()
            .chain(aliases.send.iter().map(|name| name.as_str()))
        {
            if ident_ats(line, name)
                .into_iter()
                .any(|at| is_call_at(line, at, name.len()))
            {
                actions.push(line_no);
                acted = true;
                break;
            }
        }
        if acted {
            continue;
        }
        for name in senders.iter() {
            if ident_ats(line, name)
                .into_iter()
                .any(|at| is_call_at(line, at, name.len()))
            {
                actions.push(line_no);
                acted = true;
                break;
            }
        }
        if acted {
            continue;
        }
        for name in ["until"]
            .iter()
            .copied()
            .chain(aliases.until.iter().map(|name| name.as_str()))
        {
            if let Some(at) = ident_ats(line, name)
                .into_iter()
                .find(|at| is_call_at(line, *at, name.len()))
            {
                waits.push((line_no, at, (*name).to_owned()));
                break;
            }
        }
    }
    let resets = reset_lines(&lines);
    let mut stale = 0;
    for (wait_line, at, name) in waits {
        let Some(wait_text) = lines.get(wait_line) else {
            continue;
        };
        let socket = socket_of_call(wait_text, at, name.len());
        let region = call_region(&lines, wait_line, at);
        if !wait_is_open(&region) {
            continue;
        }
        if wait_kinds(&region)
            .iter()
            .any(|kind| kind == "fiber_exited")
        {
            continue;
        }
        let mut counted = false;
        for subscribe in subscribes.iter().copied() {
            if !is_before_strict(subscribe, wait_line) {
                continue;
            }
            if sockets.get(&subscribe).is_none_or(|known| *known != socket) {
                continue;
            }
            if resets
                .iter()
                .any(|reset| subscribe < *reset && *reset < wait_line)
            {
                continue;
            }
            let Some(replay) = replay_points
                .iter()
                .copied()
                .filter(|point| subscribe < *point && *point < wait_line)
                .min()
            else {
                continue;
            };
            if !actions.iter().any(|action| {
                is_before_strict(subscribe, *action) && is_before_strict(*action, wait_line)
            }) {
                continue;
            }
            // From the replay point to the wait: a hand-stepped line
            // counter stalls forever under a stepping mutant, so this
            // walks the range with an iterator instead
            // (`docs/testing/mutants.md`, "Proving a test bites").
            let drained = (replay..wait_line)
                .any(|drained_line| is_drain(&lines, drained_line, &aliases, &socket));
            if drained {
                continue;
            }
            counted = true;
            break;
        }
        if counted {
            stale += 1;
        }
    }
    stale
}

#[cfg(test)]
#[path = "stale_wait_ratchet_tests.rs"]
mod tests;
