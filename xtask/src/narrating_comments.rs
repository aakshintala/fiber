use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::process::Command;

use grep_matcher::Matcher as _;
use grep_regex::RegexMatcherBuilder;

const PHRASES: &str =
    r"\b(?:changed[ \t]+from|previously|now[ \t]+uses|fixed[ \t]+the[ \t]+bug[ \t]+where)\b";

#[derive(Default)]
struct LexState {
    block_depth: usize,
    raw_hashes: Option<usize>,
    string: bool,
    shell_quote: Option<u8>,
    heredocs: VecDeque<String>,
}

pub(crate) fn narrating_comments(base: &str) -> Result<Vec<String>, String> {
    let merge_base = git(&["merge-base", base, "HEAD"])?;
    let diff = git(&[
        "diff",
        "--no-ext-diff",
        "--no-color",
        "--find-renames",
        "--unified=0",
        &merge_base,
    ])?;
    let mut source_map = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for bytes in [
        git_bytes(&[
            "diff",
            "--name-only",
            "-z",
            "--find-renames",
            "--diff-filter=ACMRT",
            &merge_base,
        ])?,
        git_bytes(&["ls-files", "--others", "--exclude-standard", "-z"])?,
    ] {
        for path in bytes
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path =
                std::str::from_utf8(path).map_err(|e| format!("git path is not UTF-8: {e}"))?;
            if is_source(path) {
                paths.insert(path.to_owned());
            }
        }
    }
    for path in paths {
        if !Path::new(&path).exists() {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
        if bytes.contains(&0) {
            continue;
        }
        let source = String::from_utf8(bytes).map_err(|e| format!("{path}: {e}"))?;
        source_map.insert(path, source);
    }
    let mut failures = check_diff(&diff, &source_map)?;

    // Untracked files are absent from Git's diff, so represent each eligible
    // file as an added-file patch and send it through the same line checker.
    for path in git_bytes(&["ls-files", "--others", "--exclude-standard", "-z"])?
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
    {
        let path = std::str::from_utf8(path).map_err(|e| format!("git path is not UTF-8: {e}"))?;
        if !is_source(path) {
            continue;
        }
        let text = match source_map.get(path) {
            Some(text) => text,
            None => {
                let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
                if bytes.contains(&0) {
                    continue;
                }
                return Err(format!("{path}: source unavailable"));
            }
        };
        let mut patch = format!(
            "diff --git a/{path} b/{path}\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
            text.lines().count()
        );
        for line in text.lines() {
            patch.push('+');
            patch.push_str(line);
            patch.push('\n');
        }
        failures.extend(check_diff(&patch, &source_map)?);
    }
    failures.sort();
    failures.dedup();
    Ok(failures)
}

fn is_source(path: &str) -> bool {
    path.ends_with(".rs") || path.ends_with(".sh") || path.starts_with("scripts/")
}

fn git(args: &[&str]) -> Result<String, String> {
    let bytes = git_bytes(args)?;
    String::from_utf8(bytes)
        .map(|text| text.trim_end().to_owned())
        .map_err(|e| format!("git {}: {e}", args.join(" ")))
}

fn git_bytes(args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

pub(super) fn check_diff(
    diff: &str,
    sources: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    let mut added: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    let mut path: Option<String> = None;
    let mut expected_old = 0usize;
    let mut expected_new = 0usize;
    let mut new_line = 0usize;
    let mut old_seen = 0usize;
    let mut new_seen = 0usize;
    let mut in_hunk = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            if in_hunk && (old_seen != expected_old || new_seen != expected_new) {
                return Err(format!(
                    "{}: incomplete hunk",
                    path.as_deref().unwrap_or("diff")
                ));
            }
            in_hunk = false;
            path = None;
        } else if let Some(header) = line.strip_prefix("+++ b/") {
            path = Some(header.to_owned());
        } else if line.starts_with("+++ /dev/null")
            || line.starts_with("--- ")
            || line.starts_with("\\ No newline")
        {
            continue;
        } else if line.starts_with("@@") {
            if in_hunk && (old_seen != expected_old || new_seen != expected_new) {
                return Err(format!(
                    "{}: incomplete hunk",
                    path.as_deref().unwrap_or("diff")
                ));
            }
            let (_, old_count, new, new_count) = parse_hunk(line)
                .map_err(|e| format!("{}: {e}", path.as_deref().unwrap_or("diff")))?;
            expected_old = old_count;
            expected_new = new_count;
            new_line = new;
            old_seen = 0;
            new_seen = 0;
            in_hunk = true;
        } else if in_hunk {
            if line.starts_with('+') {
                let p = path
                    .as_deref()
                    .ok_or("diff: added line before file header")?;
                added.entry(p.to_owned()).or_default().insert(new_line);
                new_line = new_line.checked_add(1).ok_or_else(|| {
                    format!(
                        "{}: new-side line overflow",
                        path.as_deref().unwrap_or("diff")
                    )
                })?;
                new_seen += 1;
            } else if line.starts_with('-') {
                old_seen += 1;
            } else if line.starts_with(' ') {
                old_seen += 1;
                new_line = new_line.checked_add(1).ok_or_else(|| {
                    format!(
                        "{}: new-side line overflow",
                        path.as_deref().unwrap_or("diff")
                    )
                })?;
                new_seen += 1;
            } else if !line.is_empty() {
                return Err(format!(
                    "{}: malformed hunk line",
                    path.as_deref().unwrap_or("diff")
                ));
            }
        }
    }
    if in_hunk && (old_seen != expected_old || new_seen != expected_new) {
        return Err(format!(
            "{}: incomplete hunk",
            path.as_deref().unwrap_or("diff")
        ));
    }

    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(true)
        .build(PHRASES)
        .map_err(|e| format!("narrating-comments matcher: {e}"))?;
    let mut failures = Vec::new();
    for (file, lines) in added {
        if !is_source(&file) {
            continue;
        }
        let source = sources
            .get(&file)
            .ok_or_else(|| format!("{file}: source unavailable"))?;
        let comments = comment_lines(&file, source);
        for number in lines {
            let text = comments
                .get(number.saturating_sub(1))
                .ok_or_else(|| format!("{file}:{number}: added line has no source text"))?;
            if matcher
                .is_match(text.as_bytes())
                .map_err(|e| format!("{file}:{number}: {e}"))?
            {
                failures.push(format!("{file}:{number}: comment narrates history"));
            }
        }
    }
    Ok(failures)
}

fn parse_hunk(line: &str) -> Result<(usize, usize, usize, usize), String> {
    let body = line
        .strip_prefix("@@ ")
        .and_then(|s| s.split_once(" @@").map(|x| x.0))
        .ok_or("malformed hunk header")?;
    let (old, new) = body.split_once(' ').ok_or("malformed hunk ranges")?;
    fn range(value: &str) -> Result<(usize, usize), String> {
        let value = value
            .strip_prefix(['-', '+'])
            .ok_or("malformed hunk range")?;
        let (start, count) = if let Some((start, count)) = value.split_once(',') {
            (
                start.parse().map_err(|_| "malformed hunk start")?,
                count.parse().map_err(|_| "malformed hunk count")?,
            )
        } else {
            (value.parse().map_err(|_| "malformed hunk start")?, 1)
        };
        if start == 0 && count > 0 {
            return Err("zero hunk start with nonzero count".to_owned());
        }
        Ok((start, count))
    }
    let (old, old_count) = range(old)?;
    let (new, new_count) = range(new)?;
    Ok((old, old_count, new, new_count))
}

fn comment_lines(path: &str, source: &str) -> Vec<String> {
    let shell = !path.ends_with(".rs");
    let mut state = LexState::default();
    source
        .lines()
        .map(|line| {
            if let Some(end) = state.heredocs.front() {
                if line.trim() == end {
                    state.heredocs.pop_front();
                }
                return String::new();
            }
            if shell {
                shell_comment(line, &mut state)
            } else {
                rust_comment(line, &mut state)
            }
        })
        .collect()
}

fn shell_comment(line: &str, state: &mut LexState) -> String {
    let bytes = line.as_bytes();
    let mut quote = state.shell_quote;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes.get(i).copied().unwrap_or_default();
        if let Some(q) = quote {
            if b == q {
                quote = None;
            } else if b == b'\\' {
                i += 1;
            }
        } else if b == b'\'' || b == b'"' {
            quote = Some(b);
        } else if b == b'#' && (i == 0 || bytes.get(i - 1).is_some_and(u8::is_ascii_whitespace)) {
            state.shell_quote = quote;
            return line[i + 1..].to_owned();
        } else if line[i..].starts_with("<<") && !line[i..].starts_with("<<<") {
            let mut start = i + 2;
            if bytes.get(start).copied() == Some(b'-') {
                start += 1;
            }
            let delim = line.get(start..).unwrap_or_default().trim();
            let delim = delim
                .trim_matches(['\'', '"'])
                .split_whitespace()
                .next()
                .unwrap_or("");
            if !delim.is_empty() {
                state.heredocs.push_back(delim.to_owned());
            }
        }
        i += 1;
    }
    state.shell_quote = quote;
    String::new()
}

fn rust_comment(line: &str, state: &mut LexState) -> String {
    let bytes = line.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if state.block_depth > 0 {
            if line[i..].starts_with("/*") {
                state.block_depth += 1;
                out.push(' ');
                i += 2;
            } else if line[i..].starts_with("*/") {
                state.block_depth -= 1;
                out.push('\0');
                i += 2;
            } else {
                let ch = line[i..].chars().next().unwrap_or('\0');
                out.push(ch);
                i += ch.len_utf8();
            }
        } else if let Some(hashes) = state.raw_hashes {
            let close = format!("\"{}", "#".repeat(hashes));
            if line[i..].starts_with(&close) {
                state.raw_hashes = None;
                i += close.len();
            } else {
                i += line[i..].chars().next().unwrap_or('\0').len_utf8();
            }
        } else if state.string {
            if bytes.get(i).copied() == Some(b'\\') {
                i += 2;
            } else if bytes.get(i).copied() == Some(b'"') {
                state.string = false;
                i += 1;
            } else {
                i += line[i..].chars().next().unwrap_or('\0').len_utf8();
            }
        } else if line[i..].starts_with("//") {
            out.push_str(&line[i + 2..]);
            break;
        } else if line[i..].starts_with("/*") {
            state.block_depth = 1;
            out.push(' ');
            i += 2;
        } else if line[i..].starts_with('r') || line[i..].starts_with("br") {
            let quote = if line[i..].starts_with("br") {
                i + 2
            } else {
                i + 1
            };
            let hashes = bytes
                .get(quote..)
                .unwrap_or_default()
                .iter()
                .take_while(|b| **b == b'#')
                .count();
            let opening = quote + hashes;
            if bytes.get(opening).copied() == Some(b'\"') {
                state.raw_hashes = Some(hashes);
                i = opening + 1;
            } else {
                i += 1;
            }
        } else if bytes.get(i).copied() == Some(b'"') {
            state.string = true;
            i += 1;
        } else if bytes.get(i).copied() == Some(b'\'') {
            if let Some(end) = char_literal_end(bytes, i) {
                i = end;
            } else {
                i += 1;
            }
        } else {
            i += line[i..].chars().next().unwrap_or('\0').len_utf8();
        }
    }
    out
}

fn char_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start.checked_add(1)?;
    if bytes.get(i).copied() == Some(b'\\') {
        i = i.checked_add(1)?;
        match bytes.get(i).copied()? {
            b'u' if bytes.get(i + 1).copied() == Some(b'{') => {
                i = i.checked_add(2)?;
                while bytes.get(i).copied()? != b'}' {
                    i = i.checked_add(1)?;
                }
                i = i.checked_add(1)?;
            }
            b'x' => i = i.checked_add(3)?,
            _ => i = i.checked_add(1)?,
        }
    } else {
        let first = bytes.get(i)?;
        i = i.checked_add(if first.is_ascii() {
            1
        } else if first & 0b1110_0000 == 0b1100_0000 {
            2
        } else if first & 0b1111_0000 == 0b1110_0000 {
            3
        } else {
            4
        })?;
    }
    (bytes.get(i).copied() == Some(b'\'')).then_some(i + 1)
}

#[cfg(test)]
#[path = "narrating_comments_tests.rs"]
mod tests;
