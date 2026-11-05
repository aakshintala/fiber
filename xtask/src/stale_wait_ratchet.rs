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
//! drains it; a keeping read preserves it.
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
    let mut start = at;
    while start > 0 && (bytes[start] & 0xC0) == 0x80 {
        start -= 1;
    }
    std::str::from_utf8(&bytes[start..])
        .ok()
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
    "handoff_completed",
    "handoff_started",
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
fn scan_group_end(
    lines: &[&str],
    start_line: usize,
    start_col: usize,
    body_mode: bool,
) -> (usize, usize) {
    let mut depth: [i32; 3] = [0, 0, 0];
    let mut started = false;
    let mut line_no = start_line;
    while let Some(line) = lines.get(line_no) {
        let bytes = line.as_bytes();
        let mut col = if line_no == start_line { start_col } else { 0 };
        let mut instr: Option<u8> = None;
        let mut raw_hashes: Option<usize> = None;
        let mut in_block = false;
        while let Some(&byte) = bytes.get(col) {
            if in_block {
                if byte == b'*' && bytes.get(col + 1) == Some(&b'/') {
                    in_block = false;
                    col += 1;
                }
            } else if instr.is_some() {
                if raw_hashes.is_some() {
                    let hashes = raw_hashes.unwrap_or(0);
                    if byte == b'"' && (0..hashes).all(|h| bytes.get(col + 1 + h) == Some(&b'#')) {
                        let mut ok = true;
                        for h in 0..hashes {
                            if bytes.get(col + 1 + h) != Some(&b'#') {
                                ok = false;
                                break;
                            }
                        }
                        if ok {
                            instr = None;
                            raw_hashes = None;
                            col += hashes;
                        }
                    }
                } else if byte == b'\\' {
                    col += 1;
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
            } else if byte == b'b' && bytes.get(col + 1) == Some(&b'"') {
                instr = Some(b'"');
                col += 1;
            } else if byte == b'r' && matches!(bytes.get(col + 1), Some(b'"') | Some(b'#')) {
                let mut hashes = 0;
                while bytes.get(col + 1 + hashes) == Some(&b'#') {
                    hashes += 1;
                }
                if bytes.get(col + 1 + hashes) == Some(&b'"') {
                    instr = Some(b'r');
                    raw_hashes = Some(hashes);
                    col += hashes + 1;
                }
            } else if byte == b'\'' {
                let rest = bytes.get(col..).unwrap_or(&[]);
                let plain = rest.len() >= 3 && rest.get(2) == Some(&b'\'');
                let escaped =
                    rest.len() >= 4 && rest.get(1) == Some(&b'\\') && rest.get(3) == Some(&b'\'');
                if plain || escaped {
                    col += if escaped { 3 } else { 2 };
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
                if !body_mode && started && depth == [0, 0, 0] {
                    return (line_no, col);
                }
            } else if byte == b']' {
                depth[1] -= 1;
                if !body_mode && started && depth == [0, 0, 0] {
                    return (line_no, col);
                }
            } else if byte == b'}' {
                depth[2] -= 1;
                if !body_mode && started && depth == [0, 0, 0] {
                    return (line_no, col);
                }
            }
            col += 1;
        }
        if body_mode && started && depth == [0, 0, 0] {
            return (line_no, bytes.len());
        }
        line_no += 1;
        if line_no > start_line + 200 {
            break;
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
    let mut open = at;
    while line.as_bytes().get(open) != Some(&b'(') {
        if line.as_bytes().get(open).is_none() {
            return String::new();
        }
        open += 1;
    }
    let (end_line, end_col) = scan_group_end(lines, line_no, open, false);
    let mut region = String::new();
    let mut no = line_no;
    while no <= end_line {
        let Some(text) = lines.get(no) else { break };
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
        no += 1;
    }
    region
}

/// The bodies of every function in `lines`, by name: same-name bodies
/// concatenate, so a helper's sends count under any one of them.
fn fn_bodies(lines: &[&str]) -> BTreeMap<String, String> {
    let mut bodies = BTreeMap::new();
    let mut line_no = 0;
    while line_no < lines.len() {
        let Some(line) = lines.get(line_no) else {
            break;
        };
        let Some(name) = fn_name(line) else {
            line_no += 1;
            continue;
        };
        let (end_line, _) = scan_group_end(lines, line_no, 0, true);
        let mut body = String::new();
        let mut no = line_no;
        while no <= end_line {
            if let Some(text) = lines.get(no) {
                body.push_str(text);
                body.push('\n');
            }
            no += 1;
        }
        bodies
            .entry(name.to_owned())
            .and_modify(|old: &mut String| old.push_str(&body))
            .or_insert(body);
        line_no = end_line + 1;
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
        let mut cursor = 0;
        while cursor + 4 <= bytes.len() {
            if statement[cursor..].starts_with(" as ") {
                let mut start = cursor;
                while start > 0
                    && bytes
                        .get(start - 1)
                        .is_some_and(|byte| is_ident_byte(*byte))
                {
                    start -= 1;
                }
                let mut end = cursor + 4;
                while bytes.get(end).is_some_and(|byte| is_ident_byte(*byte)) {
                    end += 1;
                }
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
                cursor = end;
            } else {
                cursor += 1;
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
    let mut cursor = at;
    while cursor > 0 && matches!(bytes.get(cursor - 1), Some(b' ') | Some(b'\t')) {
        cursor -= 1;
    }
    if cursor > 0 && bytes.get(cursor - 1) == Some(&b'.') {
        let mut end = cursor - 1;
        while end > 0 && !matches!(bytes.get(end - 1), Some(b' ' | b'\t' | b';' | b'(' | b',')) {
            end -= 1;
        }
        return strip_borrow(line.get(end..cursor - 1).unwrap_or(""));
    }
    strip_borrow(&first_argument(line, at, needle_len))
}

/// The callee's first argument: the text up to the top-level `,` or
/// `)`, without nesting, strings, raw strings or character literals.
fn first_argument(line: &str, at: usize, needle_len: usize) -> String {
    let bytes = line.as_bytes();
    let mut cursor = at + needle_len;
    while matches!(bytes.get(cursor), Some(b' ') | Some(b'\t')) {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'(') {
        return String::new();
    }
    cursor += 1;
    let start = cursor;
    let mut depth = 0;
    let mut instr: Option<u8> = None;
    let mut raw_hashes: Option<usize> = None;
    while let Some(byte) = bytes.get(cursor) {
        if instr.is_some() {
            if raw_hashes.is_some() {
                let hashes = raw_hashes.unwrap_or(0);
                if *byte == b'"' {
                    let mut ok = true;
                    for h in 0..hashes {
                        if bytes.get(cursor + 1 + h) != Some(&b'#') {
                            ok = false;
                            break;
                        }
                    }
                    if ok {
                        instr = None;
                        raw_hashes = None;
                        cursor += hashes;
                    }
                }
            } else if *byte == b'\\' {
                cursor += 1;
            } else if Some(*byte) == instr {
                instr = None;
            }
        } else if *byte == b'"' {
            instr = Some(b'"');
        } else if *byte == b'\'' {
            let rest = bytes.get(cursor..).unwrap_or(&[]);
            let escaped =
                rest.len() >= 4 && rest.get(1) == Some(&b'\\') && rest.get(3) == Some(&b'\'');
            let plain = rest.len() >= 3 && rest.get(2) == Some(&b'\'');
            if escaped {
                cursor += 3;
            } else if plain {
                cursor += 2;
            }
        } else if *byte == b'r' && matches!(bytes.get(cursor + 1), Some(b'"') | Some(b'#')) {
            let mut hashes = 0;
            while bytes.get(cursor + 1 + hashes) == Some(&b'#') {
                hashes += 1;
            }
            if bytes.get(cursor + 1 + hashes) == Some(&b'"') {
                instr = Some(b'r');
                raw_hashes = Some(hashes);
                cursor += hashes + 1;
            }
        } else if *byte == b'(' || *byte == b'[' || *byte == b'{' {
            depth += 1;
        } else if *byte == b')' || *byte == b']' || *byte == b'}' {
            if depth == 0 {
                break;
            }
            depth -= 1;
        } else if *byte == b',' && depth == 0 {
            break;
        }
        cursor += 1;
    }
    line.get(start..cursor).unwrap_or("").to_owned()
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
    let mut floor = 0;
    for (no, text) in lines.iter().enumerate() {
        if no > line_no {
            break;
        }
        if is_top_fn_def(text) {
            floor = no;
        }
    }
    let mut above = line_no;
    while above > floor && line_no - above < 16 {
        above -= 1;
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
        if line.trim_start().starts_with("//") || is_use_line(line) || fn_name(line).is_some() {
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
    let mut cursor = 0;
    while let Some(found) = region[cursor..].find("\"kind\"]") {
        let mut rest = region
            .get(cursor + found + "\"kind\"]".len()..)
            .unwrap_or("");
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
        cursor += found + 1;
    }
    kinds
}

/// Whether the wait's match region delegates the line to another
/// function: an opaque session, command or acknowledgement match hides
/// inside that helper.
fn delegates_match(region: &str) -> bool {
    let bytes = region.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes.get(cursor) != Some(&b'(') {
            cursor += 1;
            continue;
        }
        let mut start = cursor;
        while start > 0
            && bytes
                .get(start - 1)
                .is_some_and(|byte| is_ident_byte(*byte))
        {
            start -= 1;
        }
        let name = region.get(start..cursor).unwrap_or("");
        if name.is_empty() || OPAQUE_EXCEPTIONS.contains(&name) {
            cursor += 1;
            continue;
        }
        if matches!(bytes.get(start.wrapping_sub(1)), Some(b'.') | Some(b':')) {
            cursor += 1;
            continue;
        }
        let mut after = cursor + 1;
        while matches!(bytes.get(after), Some(b' ') | Some(b'\t') | Some(b'\n')) {
            after += 1;
        }
        if region.get(after..).is_some_and(|rest| {
            rest.starts_with("line")
                && rest
                    .as_bytes()
                    .get(4)
                    .is_none_or(|byte| !is_ident_byte(*byte))
        }) {
            return true;
        }
        cursor += 1;
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

/// Whether the line drains the replay for a later wait: a bound wait
/// ending at a live line, or a consuming wait for a resume marker, an
/// ephemeral kind, or a live-only acknowledgement match.
fn is_drain(lines: &[&str], line_no: usize, aliases: &Aliases) -> bool {
    let Some(line) = lines.get(line_no) else {
        return false;
    };
    if line.trim_start().starts_with("//") || is_use_line(line) || fn_name(line).is_some() {
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
    if ["until_after", "until_after_reopen", "until_close"].contains(&name) {
        return true;
    }
    let region = call_region(lines, found_line, at);
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

/// One stale wait per unbounded `until` in the admitted shape: a
/// subscribe on the same connection before it, a replay point and the
/// test's action between them, and no drain from the replay point on.
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
        if line.trim_start().starts_with("//") || is_use_line(line) {
            if line.contains("resum") || line.contains("reopen") || line.contains("rewind") {
                replay_points.push(line_no);
            }
            continue;
        }
        if line.contains("resum") || line.contains("reopen") || line.contains("rewind") {
            replay_points.push(line_no);
        }
        if fn_name(line).is_some() {
            continue;
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
            if !(subscribe < wait_line) {
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
            if !actions
                .iter()
                .any(|action| subscribe < *action && *action < wait_line)
            {
                continue;
            }
            let mut drained = false;
            let mut drained_line = replay;
            while drained_line < wait_line {
                if is_drain(&lines, drained_line, &aliases) {
                    drained = true;
                    break;
                }
                drained_line += 1;
            }
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
