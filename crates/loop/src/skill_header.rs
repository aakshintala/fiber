//! The header of a `SKILL.md` (`docs/system-prompt.md`, "Skills"): the
//! YAML subset Fiber reads between the opening and closing `---` lines.
//! Only `name`, `description` and `disable-model-invocation` are kept;
//! every other key is skipped by its indentation.

/// What a skill's header gives.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Header {
    /// The skill's name, trimmed.
    pub(crate) name: String,
    /// Its description, trimmed.
    pub(crate) description: String,
    /// `false` when `disable-model-invocation` is `true`.
    pub(crate) model_invocable: bool,
}

/// Why a skill is left out.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Invalid {
    /// No opening or closing `---`, a malformed line, a repeated key, an
    /// unclosed quote or a `name` or `description` that is a map or list.
    DoesNotParse,
    /// `name` is missing or empty.
    NoName,
    /// `description` is missing or empty.
    NoDescription,
}

/// One top-level entry: its key, the text after `key:` and the lines
/// below it (indented or blank).
struct Entry<'a> {
    key: &'a str,
    inline: &'a str,
    rest: Vec<&'a str>,
}

/// Parses the header at the start of `text`.
pub(crate) fn parse(text: &str) -> Result<Header, Invalid> {
    let mut lines = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line));
    if lines.next() != Some("---") {
        return Err(Invalid::DoesNotParse);
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut closed = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() {
            if let Some(entry) = entries.last_mut() {
                entry.rest.push(line);
            }
        } else if line.starts_with([' ', '\t']) {
            entries
                .last_mut()
                .ok_or(Invalid::DoesNotParse)?
                .rest
                .push(line);
        } else if line.starts_with('#') {
            // A comment at column 0.
        } else {
            let (key, inline) = split_key(line).ok_or(Invalid::DoesNotParse)?;
            if entries.iter().any(|entry| entry.key == key) {
                return Err(Invalid::DoesNotParse);
            }
            entries.push(Entry {
                key,
                inline,
                rest: Vec::new(),
            });
        }
    }
    if !closed {
        return Err(Invalid::DoesNotParse);
    }
    let value = |key: &str| -> Result<Option<String>, Invalid> {
        entries
            .iter()
            .find(|entry| entry.key == key)
            .map(string)
            .transpose()
    };
    let name = value("name")?.unwrap_or_default();
    let description = value("description")?.unwrap_or_default();
    let disabled = value("disable-model-invocation")?;
    if name.is_empty() {
        return Err(Invalid::NoName);
    }
    if description.is_empty() {
        return Err(Invalid::NoDescription);
    }
    Ok(Header {
        name,
        description,
        model_invocable: !disabled.is_some_and(|text| text.eq_ignore_ascii_case("true")),
    })
}

/// `key: value` split at its first colon, when the key is `[A-Za-z0-9_-]+`
/// and the colon ends the line or is followed by a space. The value is
/// trimmed.
fn split_key(line: &str) -> Option<(&str, &str)> {
    let colon = line.find(':')?;
    let (key, after) = (&line[..colon], &line[colon + 1..]);
    let key_ok = !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    let spaced = after.is_empty() || after.starts_with([' ', '\t']);
    (key_ok && spaced).then(|| (key, after.trim()))
}

/// The string an entry holds, trimmed at both ends.
fn string(entry: &Entry) -> Result<String, Invalid> {
    let inline = entry.inline;
    let text = if let Some(quoted) = inline.strip_prefix('\'') {
        single_quoted(quoted)?
    } else if let Some(quoted) = inline.strip_prefix('"') {
        double_quoted(quoted)?
    } else if let Some(block) = block_style(inline) {
        block_scalar(block, &entry.rest)
    } else {
        plain(inline, &entry.rest)?
    };
    Ok(text.trim().to_owned())
}

/// Text after a `'`: up to the closing quote, `''` standing for one quote.
fn single_quoted(text: &str) -> Result<String, Invalid> {
    let mut out = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c != '\'' {
            out.push(c);
        } else if chars.next_if(|(_, next)| *next == '\'').is_some() {
            out.push('\'');
        } else {
            return closed(&text[at + 1..]).map(|()| out);
        }
    }
    Err(Invalid::DoesNotParse)
}

/// Text after a `"`: up to the closing quote, with the escapes `\\`, `\"`,
/// `\n` and `\t`. Any other backslash is kept as written.
fn double_quoted(text: &str) -> Result<String, Invalid> {
    let mut out = String::new();
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => return closed(&text[at + 1..]).map(|()| out),
            '\\' => match chars.next() {
                Some((_, '\\')) => out.push('\\'),
                Some((_, '"')) => out.push('"'),
                Some((_, 'n')) => out.push('\n'),
                Some((_, 't')) => out.push('\t'),
                Some((_, other)) => {
                    out.push('\\');
                    out.push(other);
                }
                None => return Err(Invalid::DoesNotParse),
            },
            other => out.push(other),
        }
    }
    Err(Invalid::DoesNotParse)
}

/// What may follow a closing quote: nothing, or a comment.
fn closed(after: &str) -> Result<(), Invalid> {
    let after = after.trim();
    if after.is_empty() || after.starts_with('#') {
        Ok(())
    } else {
        Err(Invalid::DoesNotParse)
    }
}

/// Whether `inline` opens a block scalar (`|` or `>`, with an optional
/// `-` or `+`, then nothing or a comment): `Some(true)` folds, `Some(false)`
/// keeps line breaks.
fn block_style(inline: &str) -> Option<bool> {
    let fold = match inline.chars().next()? {
        '>' => true,
        '|' => false,
        _ => return None,
    };
    let after = inline[1..].strip_prefix(['-', '+']).unwrap_or(&inline[1..]);
    let after = after.trim();
    (after.is_empty() || after.starts_with('#')).then_some(fold)
}

/// The indented lines of a block scalar with their common indent removed,
/// folded with spaces or kept as lines.
fn block_scalar(fold: bool, rest: &[&str]) -> String {
    let indent = rest
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let lines: Vec<&str> = rest
        .iter()
        .map(|line| line.get(indent..).unwrap_or("").trim_end())
        .collect();
    if fold {
        fold_lines(&lines)
    } else {
        lines.join("\n")
    }
}

/// A plain value: the text after the key with a ` #` comment cut, then
/// each line below it. An empty value followed by a nested `key:` or
/// `- ` entry is a map or list.
fn plain(inline: &str, rest: &[&str]) -> Result<String, Invalid> {
    let cut = inline
        .char_indices()
        .find(|&(at, c)| c == '#' && (at == 0 || inline[..at].ends_with([' ', '\t'])))
        .map_or(inline, |(at, _)| &inline[..at]);
    let first = cut.trim();
    let nested = rest
        .iter()
        .map(|line| line.trim())
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .is_some_and(|line| line == "-" || line.starts_with("- ") || split_key(line).is_some());
    if first.is_empty() && nested {
        return Err(Invalid::DoesNotParse);
    }
    let mut lines = vec![first];
    lines.extend(
        rest.iter()
            .map(|line| line.trim())
            .filter(|line| !line.starts_with('#')),
    );
    Ok(fold_lines(&lines))
}

/// `lines` joined by one space; a blank line becomes `\n`.
fn fold_lines(lines: &[&str]) -> String {
    let mut out = String::new();
    let mut after_blank = false;
    for line in lines {
        if line.is_empty() {
            out.push('\n');
            after_blank = true;
        } else {
            if !out.is_empty() && !after_blank {
                out.push(' ');
            }
            out.push_str(line);
            after_blank = false;
        }
    }
    out
}

#[cfg(test)]
#[path = "skill_header_tests.rs"]
mod tests;
