//! Code citations (docs/code-quality.md, "What a reviewer checks"): line-number
//! cites and dangling heading cites in code.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub(crate) const ALLOW: &str = "xtask/doc-citations.allow";

const ROOTS: [&str; 3] = ["crates", "xtask", "scripts"];

fn is_path_char(b: u8) -> bool {
    matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'/' | b'-')
}

fn is_digit(b: u8) -> bool {
    matches!(b, b'0'..=b'9')
}

/// Blank comment markers so a cite split across lines reads as prose.
/// On every line whose first non-blank text is `///`, `//!`, `//` or `#`,
/// the marker becomes spaces of the same length, keeping every offset.
fn blank_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let had_newline = line.len() != body.len();
        let trimmed = body.trim_start();
        let indent = body.len() - trimmed.len();
        let marker_len = if trimmed.starts_with("///") {
            3
        } else if trimmed.starts_with("//!") {
            3
        } else if trimmed.starts_with("//") {
            2
        } else if trimmed.starts_with('#') {
            1
        } else {
            0
        };
        if marker_len == 0 {
            out.push_str(line);
        } else {
            out.push_str(&body[..indent]);
            for _ in 0..marker_len {
                out.push(' ');
            }
            out.push_str(&body[indent + marker_len..]);
            if had_newline {
                out.push('\n');
            }
        }
    }
    out
}

fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map_or(0, |before| before.matches('\n').count())
        + 1
}

fn escape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for c in field.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape(field: &str) -> Option<String> {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

struct TableEntry {
    line_no: usize,
    path: String,
    citation: String,
    count: usize,
}

fn read_table(root: &Path) -> Result<(Vec<TableEntry>, Vec<String>), String> {
    let text = match fs::read_to_string(root.join(ALLOW)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()));
        }
        Err(e) => return Err(format!("{}: {e}", root.join(ALLOW).display())),
    };
    let mut entries = Vec::new();
    let mut bad = Vec::new();
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        let (raw_path, raw_count, raw_cite) = match parts.as_slice() {
            [a, b, c] => (*a, *b, *c),
            _ => {
                bad.push(format!(
                    "{ALLOW}:{line_no}: expected <path>, a tab, a count, a tab, then the citation"
                ));
                continue;
            }
        };
        let (path, citation) = match (unescape(raw_path), unescape(raw_cite)) {
            (Some(p), Some(c)) => (p, c),
            _ => {
                bad.push(format!(
                    "{ALLOW}:{line_no}: expected <path>, a tab, a count, a tab, then the citation"
                ));
                continue;
            }
        };
        let count: usize = match raw_count.parse() {
            Ok(n) if n >= 1 => n,
            _ => {
                bad.push(format!(
                    "{ALLOW}:{line_no}: expected <path>, a tab, a count, a tab, then the citation"
                ));
                continue;
            }
        };
        if seen.contains_key(&(path.clone(), citation.clone())) {
            bad.push(format!(
                "{ALLOW}:{line_no}: {}: \"{}\" listed twice",
                escape(&path),
                escape(&citation)
            ));
            continue;
        }
        seen.insert((path.clone(), citation.clone()), line_no);
        entries.push(TableEntry {
            line_no,
            path,
            citation,
            count,
        });
    }
    Ok((entries, bad))
}

fn parse_digits(bytes: &[u8], mut pos: usize) -> Option<(usize, usize)> {
    let start = pos;
    let mut value: usize = 0;
    let mut any = false;
    while pos < bytes.len() && is_digit(bytes[pos]) {
        any = true;
        value = value
            .saturating_mul(10)
            .saturating_add((bytes[pos] - b'0') as usize);
        pos += 1;
    }
    if any { Some((pos, value)) } else { None }
}

fn parse_part(bytes: &[u8], pos: usize) -> Option<(usize, usize)> {
    let (mut pos, first) = parse_digits(bytes, pos)?;
    if pos < bytes.len() && bytes[pos] == b'-' {
        let after_dash = pos + 1;
        if after_dash < bytes.len() && is_digit(bytes[after_dash]) {
            let (end, _) = parse_digits(bytes, after_dash)?;
            pos = end;
        }
    }
    Some((pos, first))
}

/// The byte length of the span and its first line number.
fn parse_span(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let (mut pos, first) = parse_part(bytes, 0)?;
    loop {
        if pos >= bytes.len() || bytes[pos] != b',' {
            break;
        }
        let mut next = pos + 1;
        if next < bytes.len() && bytes[next] == b' ' {
            next += 1;
        }
        if next >= bytes.len() || !is_digit(bytes[next]) {
            break;
        }
        let (end, _) = parse_part(bytes, next)?;
        pos = end;
    }
    Some((pos, first))
}

enum CiteKind {
    Line { target: String, line_no: usize },
    Heading { target: String, heading: String },
}

struct Found {
    offset: usize,
    line: usize,
    key: String,
    kind: CiteKind,
}

fn find_cites(file: &str, original: &str, blanked: &str) -> Vec<Found> {
    let _ = file;
    let bytes = blanked.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let prefix_len = if blanked[i..].starts_with("docs/") {
            5
        } else if blanked[i..].starts_with("research/") {
            9
        } else {
            i += 1;
            continue;
        };
        let mut p = i + prefix_len;
        while p < bytes.len() && is_path_char(bytes[p]) {
            p += 1;
        }
        if p == i + prefix_len || !blanked[p..].starts_with(".md") {
            i += 1;
            continue;
        }
        let target = blanked[i..p + 3].to_owned();
        let path_end = p + 3;
        let mut tail = path_end;
        if tail < bytes.len() && bytes[tail] == b'`' {
            tail += 1;
        }
        if tail < blanked.len() && blanked[tail..].starts_with(':') {
            let after = tail + 1;
            if after < blanked.len()
                && is_digit(blanked.as_bytes()[after])
                && let Some((span_len, first)) = parse_span(&blanked[after..])
            {
                let end = after + span_len;
                out.push(Found {
                    offset: i,
                    line: line_of(blanked, i),
                    key: original[i..end].to_owned(),
                    kind: CiteKind::Line {
                        target,
                        line_no: first,
                    },
                });
                i = end;
                continue;
            }
            i += 1;
            continue;
        }
        if tail + 6 <= blanked.len() && blanked[tail..].starts_with(" line ") {
            let after = tail + 6;
            if after < blanked.len()
                && is_digit(blanked.as_bytes()[after])
                && let Some((span_len, first)) = parse_span(&blanked[after..])
            {
                let end = after + span_len;
                out.push(Found {
                    offset: i,
                    line: line_of(blanked, i),
                    key: original[i..end].to_owned(),
                    kind: CiteKind::Line {
                        target,
                        line_no: first,
                    },
                });
                i = end;
                continue;
            }
            i += 1;
            continue;
        }
        if tail < bytes.len() && bytes[tail] == b',' {
            if let Some((heading, len)) = crate::docs::cited_heading(&blanked[tail..]) {
                let end = tail + len;
                out.push(Found {
                    offset: i,
                    line: line_of(blanked, i),
                    key: original[i..end].to_owned(),
                    kind: CiteKind::Heading { target, heading },
                });
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn walk_files(root: &Path, out: &mut Vec<String>) -> Result<(), String> {
    for entry_root in ROOTS {
        let base = root.join(entry_root);
        if !base.exists() {
            continue;
        }
        let mut stack = vec![base];
        while let Some(dir) = stack.pop() {
            let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            for entry in entries {
                let entry = entry.map_err(|e| e.to_string())?;
                let file_type = entry.file_type().map_err(|e| e.to_string())?;
                let path = entry.path();
                if file_type.is_dir() {
                    stack.push(path);
                } else if file_type.is_file() {
                    let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;
                    rel.to_str()
                        .ok_or_else(|| format!("{}: file name is not UTF-8", path.display()))?;
                    let rel_slash = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    if rel_slash == ALLOW {
                        continue;
                    }
                    out.push(rel_slash);
                }
            }
        }
    }
    out.sort();
    Ok(())
}

fn cached_index(
    root: &Path,
    cache: &mut BTreeMap<String, Option<crate::docs::HeadingIndex>>,
    target: &str,
) -> Result<bool, String> {
    if !cache.contains_key(target) {
        let path = root.join(target);
        let index = match fs::read_to_string(&path) {
            Ok(text) => Some(crate::docs::heading_index(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        cache.insert(target.to_owned(), index);
    }
    Ok(cache.get(target).is_some_and(|v| v.is_some()))
}

fn heading_above(
    root: &Path,
    cache: &mut BTreeMap<String, Option<crate::docs::HeadingIndex>>,
    target: &str,
    line_no: usize,
) -> Result<Option<String>, String> {
    cached_index(root, cache, target)?;
    Ok(cache
        .get(target)
        .and_then(|v| v.as_ref())
        .and_then(|index| index.above(line_no))
        .map(|s| s.to_owned()))
}

fn heading_names(
    root: &Path,
    cache: &mut BTreeMap<String, Option<crate::docs::HeadingIndex>>,
    target: &str,
) -> Result<Option<Vec<String>>, String> {
    cached_index(root, cache, target)?;
    Ok(cache
        .get(target)
        .and_then(|v| v.as_ref())
        .map(|index| index.names().to_vec()))
}

/// Failures in path then source order, then stale and bad table lines in
/// table order. A missing table is the empty table.
pub(crate) fn check(root: &Path) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    walk_files(root, &mut files)?;
    let mut cached: BTreeMap<String, Option<crate::docs::HeadingIndex>> = BTreeMap::new();
    let mut found_all: Vec<(String, usize, usize, String, CiteKind)> = Vec::new();
    for file in &files {
        let text = match fs::read_to_string(root.join(file)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(e) => return Err(format!("{}: {e}", root.join(file).display())),
        };
        let blanked = blank_markers(&text);
        for found in find_cites(file, &text, &blanked) {
            found_all.push((
                file.clone(),
                found.offset,
                found.line,
                found.key,
                found.kind,
            ));
        }
    }
    found_all.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut failing: Vec<(String, usize, usize, String, String)> = Vec::new();
    for (file, offset, line, key, kind) in &found_all {
        match kind {
            CiteKind::Line { target, line_no } => {
                let reason = match heading_above(root, &mut cached, target, *line_no)? {
                    None if !cached_index(root, &mut cached, target)? => "no such file".to_owned(),
                    None => {
                        format!("no heading of {target} at or above that line: delete the citation")
                    }
                    Some(heading) => format!("use \"{target}, \"{heading}\"\""),
                };
                failing.push((file.clone(), *offset, *line, key.clone(), reason));
            }
            CiteKind::Heading { target, heading } => {
                let found = heading_names(root, &mut cached, target)?;
                if let Some(message) =
                    crate::docs::heading_failure(target, heading, found.as_deref())
                {
                    let reason = message.split(": ").last().unwrap_or(&message).to_owned();
                    failing.push((file.clone(), *offset, *line, key.clone(), reason));
                }
            }
        }
    }
    let (entries, bad) = read_table(root)?;
    let mut by_pair: BTreeMap<(String, String), Vec<(usize, usize, String)>> = BTreeMap::new();
    for (file, offset, line, key, reason) in &failing {
        by_pair
            .entry((file.clone(), key.clone()))
            .or_default()
            .push((*offset, *line, reason.clone()));
    }
    let mut allowed: BTreeMap<(String, String), usize> = BTreeMap::new();
    for entry in &entries {
        allowed.insert((entry.path.clone(), entry.citation.clone()), entry.count);
    }
    let mut sources: Vec<(String, usize, String)> = Vec::new();
    for ((file, key), occurrences) in &by_pair {
        let found = occurrences.len();
        let allowed_count = allowed
            .get(&(file.clone(), key.clone()))
            .copied()
            .unwrap_or(0);
        if found > allowed_count {
            for (offset, line, reason) in occurrences.iter().skip(allowed_count) {
                sources.push((
                    file.clone(),
                    *offset,
                    format!("{file}:{line}: unlisted cite \"{key}\": {reason}"),
                ));
            }
        }
    }
    sources.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut failures: Vec<String> = sources.into_iter().map(|(_, _, line)| line).collect();
    for entry in &entries {
        let found = by_pair
            .get(&(entry.path.clone(), entry.citation.clone()))
            .map(|v| v.len())
            .unwrap_or(0);
        if found < entry.count {
            failures.push(format!(
                "{ALLOW}:{}: stale: {}: \"{}\" allowed {}, found {found}: lower or delete the entry",
                entry.line_no,
                escape(&entry.path),
                escape(&entry.citation),
                entry.count,
            ));
        }
    }
    failures.extend(bad);
    Ok(failures)
}

#[cfg(test)]
#[path = "doc_citations_tests.rs"]
mod tests;
