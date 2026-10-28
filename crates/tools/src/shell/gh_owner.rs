//! Trusted GitHub owners for `gh` reads (`docs/tools.md`, "Shell", "Effects").
//!
//! A `gh` read is own when every repository it names belongs to an owner,
//! meaning an account or organisation, of one of the session workspace
//! repository's `github.com` remotes. The workspace root is the trust
//! anchor: the call's `workdir` never decides the owner set, so a
//! model-chosen directory cannot widen it by pointing at a clone whose
//! remotes the person never saw.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use super::entry::{WordAt, word_at};
use super::read_only::Command;
use super::syntax::Word;

/// The most bytes read from a git `config`: a longer file has no owners, so
/// every `gh` read stays reviewed.
pub(super) const MAX_CONFIG_BYTES: usize = 64 * 1024;

/// The most bytes read from a `.git` or `commondir` pointer file: the same
/// bound the git repository guards hold (`super::git::MAX_GITFILE_BYTES`).
const MAX_POINTER_BYTES: usize = super::git::MAX_GITFILE_BYTES;

/// Whether a built-in `gh` stage is own: every repository it names belongs
/// to a workspace remote owner. Anything else stays reviewed.
pub(super) enum Standing {
    Own,
    Foreign,
}

/// Whether a built-in `gh` stage names only workspace remote owners: named
/// repositories are judged by name alone, whatever `workdir` or `cd` says,
/// while an unnamed read is own only when the call's `workdir` is the
/// workspace root and the command holds no `cd`.
pub(super) fn standing(
    words: &[Word],
    entry: &Command,
    workspace: &Path,
    workdir: &Path,
    has_cd: bool,
) -> Standing {
    let refs = named_refs(words, entry);
    if refs.consumed_repo_flag || refs.search || refs.slashed_operand {
        return Standing::Foreign;
    }
    if refs.malformed {
        return Standing::Foreign;
    }
    let owners = workspace_owners(workspace);
    if refs.named.is_empty() {
        if !has_cd && same_dir(workdir, workspace) && !owners.is_empty() {
            return Standing::Own;
        }
        return Standing::Foreign;
    }
    if !owners.is_empty() && refs.named.iter().all(|owner| owners.contains(owner)) {
        Standing::Own
    } else {
        Standing::Foreign
    }
}

/// The repositories a built-in `gh` stage's words name: every `--repo`/`-R`
/// value and `https://github.com` URL operand owner, lowercased. A flag word
/// consumed as another flag's value keeps its own meaning there, so a
/// consumed `--repo`/`-R` forces the reviewed path instead of naming one.
/// A `--search`/`-S` flag, a slash-bearing operand that is no GitHub URL,
/// and a named value with no owner all fail closed the same way.
struct Refs {
    named: Vec<String>,
    malformed: bool,
    search: bool,
    slashed_operand: bool,
    consumed_repo_flag: bool,
}

/// The words of one built-in `gh` stage as its flag walk sees them: the
/// entry already accepted them, so this walk only records which words name
/// repositories. `--name=value` takes the entry's value-flag spelling
/// before the `=`, exactly as the flag walk does.
fn named_refs(words: &[Word], entry: &Command) -> Refs {
    let mut refs = Refs {
        named: Vec::new(),
        malformed: false,
        search: false,
        slashed_operand: false,
        consumed_repo_flag: false,
    };
    let mut rest = words.iter().skip(entry.name.split(' ').count());
    let mut ended = false;
    while let Some(word) = rest.next() {
        match word_at(word, ended, entry) {
            WordAt::EndFlags => ended = true,
            WordAt::Flag => observe_flag(entry, &mut rest, &word.cooked, &mut refs),
            WordAt::Operand(cooked) => observe_operand(&cooked, &mut refs),
        }
    }
    refs
}

/// One flag word in flag position: a `--repo`/`-R` value or a GitHub URL
/// operand names its owner, while `--search`/`-S` keeps the reviewed path
/// because the query can name another repository through `repo:`. A value
/// word the flag consumes is never a flag itself, so a consumed `--repo`
/// or `-R` word forces the reviewed path.
fn observe_flag<'a>(
    entry: &Command,
    rest: &mut impl Iterator<Item = &'a Word>,
    cooked: &str,
    refs: &mut Refs,
) {
    if let Some((name, value)) = split_long_value(cooked) {
        // The entry already accepted this flag when the verdict read, so a
        // `--repo=`/`--search=` in flag position always names its repository
        // or query here; a consumed value never reaches this branch.
        if name == "--repo" {
            push_repo_value(value, refs);
        } else if name == "--search" {
            refs.search = true;
        }
        return;
    }
    let Some(flag) = entry.flags.iter().find(|flag| flag.spelling == cooked) else {
        return;
    };
    if !flag.takes_value {
        return;
    }
    let Some(value) = rest.next() else {
        return;
    };
    match cooked {
        "--repo" | "-R" => push_repo_value(&value.cooked, refs),
        "--search" | "-S" => refs.search = true,
        _ if value.cooked == "--repo" || value.cooked == "-R" => {
            refs.consumed_repo_flag = true;
        }
        _ => {}
    }
}

/// One `--repo`/`-R` value: its owner when it names exactly `owner/repo`,
/// malformed otherwise, so a bare name or an empty value never reads.
fn push_repo_value(value: &str, refs: &mut Refs) {
    match repo_owner(value) {
        Some(owner) => refs.named.push(owner),
        None => refs.malformed = true,
    }
}

/// One operand word: a `https://github.com` URL names its owner, while any
/// other slash-bearing word is a positional repository reference outside
/// the ticket's named forms, so it keeps the reviewed path instead of
/// passing as an unnamed default.
fn observe_operand(cooked: &str, refs: &mut Refs) {
    if let Some(owner) = https_owner(cooked) {
        refs.named.push(owner);
    } else if cooked.contains('/') {
        refs.slashed_operand = true;
    }
}

/// Split `--name=value`, and only that form: a short flag takes no `=`,
/// exactly as the flag walk holds.
fn split_long_value(cooked: &str) -> Option<(&str, &str)> {
    let (name, value) = cooked.split_once('=')?;
    name.starts_with("--").then_some((name, value))
}

/// The owner a `--repo`/`-R` value names: exactly `owner/repo`, compared
/// case-insensitively. A host-bearing `HOST/OWNER/REPO` never reaches here:
/// it stays `executes` under the host check that runs first.
fn repo_owner(value: &str) -> Option<String> {
    let (owner, repo) = value.split_once('/')?;
    // A host-bearing value never reaches here: `repo_selects_host` stays
    // `executes` for more than `owner/repo`, so only the segments decide.
    if !clean_segment(owner) || !clean_segment(repo) {
        return None;
    }
    Some(owner.to_lowercase())
}

/// Whether a URL path segment names an owner or repository: non-empty, with
/// none of the query, fragment, credential or port marks a fail-closed
/// parse refuses.
fn clean_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '?' | '#' | '@' | ':'))
}

/// The owners of the workspace repository's `github.com` remotes,
/// lowercased. Any repository the walk cannot verify, any pointer it
/// cannot bound, and any config it cannot read, bound or parse yields no
/// owners, so every `gh` read stays reviewed.
pub(super) fn workspace_owners(workspace: &Path) -> BTreeSet<String> {
    let Some(common) = common_dir(workspace) else {
        return BTreeSet::new();
    };
    read_owners(&common.join("config"))
}

/// The workspace repository's common git directory: the nearest `.git`
/// below or at the workspace root, following a `gitdir:` pointer for a
/// worktree without following a symlink. A bare repository, a symlinked
/// `.git`, and any I/O error but a missing `.git` have no common
/// directory, so every `gh` read stays reviewed.
fn common_dir(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        match std::fs::symlink_metadata(current.join(".git")) {
            Ok(meta) => {
                if meta.is_symlink() {
                    return None;
                }
                if meta.is_dir() {
                    // The canonical path always ends in `.git` here, so it
                    // is always under a `.git` component; only the
                    // directory check decides.
                    return current
                        .join(".git")
                        .canonicalize()
                        .ok()
                        .filter(|dir| dir.is_dir());
                }
                if meta.is_file() {
                    return gitdir_common(&current);
                }
                return None;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
        if super::git::is_bare(&current) {
            return None;
        }
        if !current.pop() {
            return None;
        }
    }
}

/// The common directory for a `.git` file naming `gitdir: <path>`: the
/// `commondir` file beside a worktree gitdir, else the gitdir itself. The
/// pointer is bounded like the guards bound it, and the canonical gitdir
/// and common directory must both sit under a `.git` component, so a
/// pointer escape has no owners.
fn gitdir_common(dir: &Path) -> Option<PathBuf> {
    let pointer = read_pointer(&dir.join(".git"))?;
    let gitdir = parse_pointer(&pointer, "gitdir: ")?;
    let joined = dir.join(gitdir);
    let canonical = joined.canonicalize().ok()?;
    if !canonical.is_dir() || !under_dot_git(&canonical) {
        return None;
    }
    commondir_common(&canonical)
}

/// The common directory for a canonical gitdir: the `commondir` pointer it
/// holds for a worktree, else the gitdir itself. A symlinked `commondir`
/// file, an unbounded pointer, and a target outside a `.git` component
/// have no owners.
fn commondir_common(gitdir: &Path) -> Option<PathBuf> {
    let pointer = gitdir.join("commondir");
    match std::fs::symlink_metadata(&pointer) {
        Ok(meta) => {
            if meta.is_symlink() || !meta.is_file() {
                return None;
            }
            let bytes = read_pointer(&pointer)?;
            let target = parse_pointer(&bytes, "")?;
            let joined = gitdir.join(target);
            let canonical = joined.canonicalize().ok()?;
            if !canonical.is_dir() || !under_dot_git(&canonical) {
                return None;
            }
            Some(canonical)
        }
        // A missing file falls back to the gitdir itself; any other I/O
        // error does the same and still fails closed through the
        // unreadable config, so the distinction needs no branch.
        Err(_) => Some(gitdir.to_path_buf()),
    }
}

/// Whether every component of `dir` up to and including one sits under a
/// `.git` component, so a pointer escape outside version control has no
/// owners. The comparison is exact: a suffix host directory such as
/// `x.git` never counts.
fn under_dot_git(dir: &Path) -> bool {
    dir.components()
        .any(|component| component.as_os_str() == ".git")
}

/// One pointer file's target: one line with at most one trailing newline,
/// after the `prefix` the format requires (`gitdir: `, or none for
/// `commondir`). Anything else has no owners.
fn parse_pointer(bytes: &[u8], prefix: &str) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.contains('\n') {
        return None;
    }
    let target = line.strip_prefix(prefix)?;
    if target.is_empty() {
        return None;
    }
    Some(target.to_owned())
}

/// The whole of a pointer file when it holds at most 512 bytes: a longer
/// file has no owners, exactly as the git repository guards hold.
fn read_pointer(path: &Path) -> Option<Vec<u8>> {
    let bytes = read_bounded(path, MAX_POINTER_BYTES)?;
    (bytes.len() <= MAX_POINTER_BYTES).then_some(bytes)
}

/// At most `bound + 1` bytes of `path`: `None` when the file cannot be
/// read, so the caller fails closed. The file is opened without following
/// a trailing symlink, and the opened descriptor must be a regular file
/// before a byte is read, so a swap between check and open cannot redirect
/// the read and a FIFO cannot block the classifier.
fn read_bounded(path: &Path, bound: usize) -> Option<Vec<u8>> {
    use rustix::fs::OFlags;
    let flags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC)
        .union(OFlags::NONBLOCK);
    let fd: std::os::fd::OwnedFd = rustix::fs::open(path, flags, rustix::fs::Mode::empty()).ok()?;
    let file = std::fs::File::from(fd);
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(bound as u64 + 1).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// The `github.com` remote owners in `config`: at most 64 KiB, parsed
/// without following `include.path`, counting only `remote.<name>.url`
/// values on `github.com`. A longer file, an unreadable one, and any
/// malformed line yield no owners rather than partial ones.
fn read_owners(config: &Path) -> BTreeSet<String> {
    let Some(bytes) = read_bounded(config, MAX_CONFIG_BYTES) else {
        return BTreeSet::new();
    };
    if bytes.len() > MAX_CONFIG_BYTES {
        return BTreeSet::new();
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return BTreeSet::new();
    };
    parse_config(text).unwrap_or_default()
}

/// The remote owners in git config text: `None` when any line is
/// malformed, so a config the classifier cannot prove safe yields no
/// owners at all.
fn parse_config(text: &str) -> Option<BTreeSet<String>> {
    if text.contains('\0') {
        return None;
    }
    let mut owners = BTreeSet::new();
    let mut section: Option<(String, Option<String>)> = None;
    for line in logical_lines(text) {
        let line = strip_comment(&line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            section = Some(parse_section(line)?);
            continue;
        }
        let Some((name, _)) = &section else {
            return None;
        };
        let (key, value) = split_key(line)?;
        let value = parse_value(value)?;
        if name == "remote"
            && section
                .as_ref()
                .is_some_and(|(_, sub)| sub.as_ref().is_some_and(|sub| !sub.is_empty()))
            && key.eq_ignore_ascii_case("url")
            && let Some(owner) = remote_owner(&value)
        {
            owners.insert(owner);
        }
    }
    Some(owners)
}

/// Physical lines joined over backslash-newline continuations, exactly as
/// git config continues a value onto the next line. A trailing backslash
/// escaped by another backslash ends the line instead.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for physical in text.split('\n') {
        let physical = physical.strip_suffix('\r').unwrap_or(physical);
        current.push_str(physical);
        if trailing_backslashes(&current) % 2 == 1 {
            current.pop();
            continue;
        }
        lines.push(std::mem::take(&mut current));
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// The number of backslashes ending `line`.
fn trailing_backslashes(line: &str) -> usize {
    line.chars().rev().take_while(|ch| *ch == '\\').count()
}

/// The line before a `#` or `;` comment: a comment starts the line or
/// follows whitespace, never inside quotes, so a `#` inside a URL value
/// without preceding whitespace stays part of the value.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut previous_whitespace = true;
    for (index, ch) in line.char_indices() {
        if quoted {
            if ch == '"' {
                quoted = false;
            }
        } else if ch == '"' {
            quoted = true;
            previous_whitespace = false;
            continue;
        } else if (ch == '#' || ch == ';') && previous_whitespace {
            return &line[..index];
        }
        previous_whitespace = ch == ' ' || ch == '\t';
    }
    line
}

/// One section header: `[name]` or `[name subsection]`, where the
/// subsection is quoted or bare. Anything else is malformed.
fn parse_section(line: &str) -> Option<(String, Option<String>)> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let inner = inner.trim();
    let (name, rest) = match inner.find(char::is_whitespace) {
        Some(index) => (inner[..index].to_lowercase(), inner[index..].trim()),
        None => (inner.to_lowercase(), ""),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
    {
        return None;
    }
    if rest.is_empty() {
        return Some((name, None));
    }
    if let Some(quoted) = rest.strip_prefix('"') {
        let body = quoted.strip_suffix('"')?;
        return Some((name, Some(unescape(body)?)));
    }
    if rest.contains(['"', '\'']) || rest.chars().any(char::is_whitespace) {
        return None;
    }
    Some((name, Some(rest.to_owned())))
}

/// One key line: `name = value`, `name=value`, or a bare boolean `name`.
/// The name holds only word characters, so a stray sentence is malformed
/// rather than a key.
fn split_key(line: &str) -> Option<(&str, &str)> {
    match split_unquoted(line) {
        Some(index) => {
            let key = line[..index].trim_end();
            if !valid_key(key) {
                return None;
            }
            Some((key, line[index + 1..].trim_start()))
        }
        None => {
            if !valid_key(line.trim()) {
                return None;
            }
            Some((line.trim(), "true"))
        }
    }
}

/// The index of the first `=`, if any. Valid keys never hold quotes, so the
/// separator always sits outside them; an `=` inside quotes only appears in
/// a line whose key is invalid either way.
fn split_unquoted(line: &str) -> Option<usize> {
    line.find('=')
}

/// Whether `key` is a config variable name: compared case-insensitively by
/// the caller, it holds only word characters.
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_')
}

/// One config value: leading whitespace skipped, a quoted value unescaped
/// with nothing but the already-stripped comment after it. The caller
/// lowercases the key it compares, since variable names are
/// case-insensitive.
fn parse_value(raw: &str) -> Option<String> {
    let raw = raw.trim_start();
    if let Some(quoted) = raw.strip_prefix('"') {
        let (value, rest) = quoted_value(quoted)?;
        if !rest.trim().is_empty() {
            return None;
        }
        return Some(value);
    }
    Some(raw.trim_end().to_owned())
}

/// A quoted value body and the text after its closing quote: `\"`, `\\`,
/// `\n`, `\t` and `\b` unescape, anything else is malformed.
fn quoted_value(quoted: &str) -> Option<(String, &str)> {
    let mut value = String::new();
    let mut chars = quoted.char_indices();
    while let Some((index, ch)) = chars.next() {
        if ch == '"' {
            return Some((value, &quoted[index + 1..]));
        }
        if ch != '\\' {
            value.push(ch);
            continue;
        }
        let (_, escaped) = chars.next()?;
        match escaped {
            'n' => value.push('\n'),
            't' => value.push('\t'),
            'b' => value.push('\x08'),
            '"' => value.push('"'),
            '\\' => value.push('\\'),
            _ => return None,
        }
    }
    None
}

/// Unescape a quoted section body with the value escapes.
fn unescape(body: &str) -> Option<String> {
    let closed = format!("{body}\"");
    let (value, rest) = quoted_value(&closed)?;
    rest.is_empty().then_some(value)
}

/// The owner a `remote.<name>.url` value reaches on `github.com`: the
/// HTTPS and SSH forms the ticket counts, compared case-insensitively,
/// with the host equal to `github.com` rather than a suffix of it.
/// Credentials, ports, `http://` and any other scheme yield no owner.
fn remote_owner(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(owner) = https_owner(value) {
        return Some(owner);
    }
    if let Some(rest) = strip_scheme(value, "ssh://") {
        let (authority, path) = rest.split_once('/')?;
        let (user, host) = authority.split_once('@')?;
        if user != "git" || host.contains(':') || !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        return url_path_owner(path);
    }
    if value.contains("://") {
        return None;
    }
    let (left, path) = value.split_once(':')?;
    let (user, host) = left.split_once('@')?;
    if user != "git" || !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if parts.next().is_some() || !clean_segment(owner) || !clean_segment(repo) {
        return None;
    }
    Some(owner.to_lowercase())
}

/// The owner an `https://github.com/<owner>/<repo>/...` reference names:
/// the scheme and host case-insensitively, with no credentials or port,
/// and at least an owner and repository after it.
fn https_owner(value: &str) -> Option<String> {
    let rest = strip_scheme(value, "https://")?;
    let (authority, path) = rest.split_once('/')?;
    // An empty or credential-bearing authority also fails the host check
    // below, so only that check decides.
    if !authority.eq_ignore_ascii_case("github.com") {
        return None;
    }
    url_path_owner(path)
}

/// Strip an ASCII case-insensitive `scheme://` prefix.
fn strip_scheme<'a>(value: &'a str, scheme: &str) -> Option<&'a str> {
    if value.len() >= scheme.len() && value[..scheme.len()].eq_ignore_ascii_case(scheme) {
        Some(&value[scheme.len()..])
    } else {
        None
    }
}

/// The owner in a URL path after the host: the first two segments name
/// the owner and repository, whatever follows them. The path never holds
/// the separator the host split consumed.
fn url_path_owner(path: &str) -> Option<String> {
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if !clean_segment(owner) || !clean_segment(repo) {
        return None;
    }
    Some(owner.to_lowercase())
}

/// Whether the call's `workdir` is the workspace root: compared after
/// resolving `.` and `..` lexically, so an explicit `.` workdir counts,
/// without touching the filesystem.
fn same_dir(workdir: &Path, workspace: &Path) -> bool {
    normalized(workdir) == normalized(workspace)
}

/// The path with `.` dropped and `..` applied lexically.
fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            kept @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                out.push(kept.as_os_str())
            }
        }
    }
    out
}
