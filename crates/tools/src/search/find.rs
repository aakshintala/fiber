//! The `find` search: list what the walk finds (`docs/tools.md`, "Search").

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::fallback;
use super::notice;
use super::walk::{self, Root};
use super::{Out, Outcome, flush_exit};

/// Runs `find` against the process's working directory and standard streams,
/// returning the exit code: 0 when the walk finished without error, 2 on an
/// error, 1 never. A call the built-in does not handle replaces the process
/// with the system `find` and never returns on success.
pub fn find_main(args: Vec<OsString>) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let stdout = std::io::stdout();
    let mut buffered = std::io::BufWriter::new(stdout.lock());
    let mut stderr = std::io::stderr().lock();
    let outcome = run(&cwd, &args, &mut buffered, &mut stderr);
    let flushed = buffered.flush();
    match outcome {
        Outcome::Done(code) => flush_exit("find", code, flushed, &mut stderr),
        Outcome::Fallback => fallback::exec("find", &args, &mut stderr),
    }
}

/// Runs `find` against `cwd`, printing paths to `stdout` and complaints to
/// `stderr`, without touching the process's own streams.
/// `Outcome::Fallback` is returned before anything is read or written.
pub(crate) fn run(
    cwd: &Path,
    args: &[OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Outcome {
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(Decision::Fallback) => return Outcome::Fallback,
        Err(Decision::Error(message)) => {
            writeln!(stderr, "{message}").unwrap_or(());
            return Outcome::Done(2);
        }
    };
    let newers = match resolve_newers(cwd, &parsed.expr.newers) {
        Ok(newers) => newers,
        Err(message) => {
            writeln!(stderr, "{message}").unwrap_or(());
            return Outcome::Done(2);
        }
    };
    let mut out = Out::new(stdout);
    let mut failed = false;
    let mut printed = false;
    let mut walked = Vec::new();
    for root in &parsed.roots {
        if out.broken() {
            break;
        }
        let visit = visit(cwd, root, &parsed.expr, &newers, &mut out, stderr);
        failed |= visit.failed;
        printed |= visit.printed;
        walked.extend(visit.walked);
        out.flush();
    }
    if let Some(error) = out.take_error() {
        writeln!(stderr, "find: writing output: {error}").unwrap_or(());
        failed = true;
    }
    if !printed && !failed {
        // Computed only when nothing printed: the second walk costs
        // nothing otherwise.
        let mut skipped = Vec::new();
        for dir in &walked {
            skipped = notice::union(skipped, notice::skipped(cwd, dir));
        }
        if let Some(line) = notice::find_line(&skipped) {
            writeln!(stderr, "{line}").unwrap_or(());
        }
    }
    Outcome::Done(if failed { 2 } else { 0 })
}

/// What `find` was asked to list.
struct Parsed {
    /// The search roots as given; no path means `.`.
    roots: Vec<PathBuf>,
    /// The tests every entry passes to print; adjacent tests are ANDed.
    expr: Expr,
}

/// The expression: every test an entry passes to print.
#[derive(Default)]
struct Expr {
    /// `-name` and `-iname`, matched against the base name.
    names: Vec<Glob>,
    /// `-path`, matched against the printed path.
    paths: Vec<Glob>,
    /// `-type`, every letter required.
    kinds: Vec<Kind>,
    /// `-newer` references, as given; the entry is newer than each.
    newers: Vec<PathBuf>,
    /// `-maxdepth`, last wins.
    maxdepth: Option<usize>,
    /// `-mindepth`, last wins.
    mindepth: Option<usize>,
}

/// A `-name`, `-iname` or `-path` glob.
struct Glob {
    /// The glob as given.
    pattern: Vec<u8>,
    /// ASCII case-insensitive for `-iname`.
    ignore_case: bool,
}

/// A `-type` bucket: links stay links, directories stay directories.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A link, wherever it points.
    Link,
}

/// Why `parse` declined: hand the call over, or fail it.
enum Decision {
    /// A flag or primary the built-in does not handle.
    Fallback,
    /// A bad invocation: the message, already a full line.
    Error(String),
}

/// Splits the leading paths from the expression. A token outside the
/// handled flags and primaries hands the call to the system `find`.
/// The iterator advances in this loop, never inside a helper, so a
/// declined helper cannot spin it.
fn parse(args: &[OsString]) -> Result<Parsed, Decision> {
    let mut args = args.iter().peekable();
    let mut roots = Vec::new();
    loop {
        // `!`, `(` and `)` never start a path: they open the expression,
        // where the unknown-primary arm hands them over.
        let is_path = match args.peek() {
            Some(arg) => {
                let bytes = arg.as_encoded_bytes();
                !(bytes.first().is_some_and(|byte| *byte == b'-')
                    || matches!(bytes, b"!" | b"(" | b")"))
            }
            None => false,
        };
        if !is_path {
            break;
        }
        if let Some(arg) = args.next() {
            roots.push(PathBuf::from(arg));
        }
    }
    if roots.is_empty() {
        roots.push(PathBuf::from("."));
    }
    let mut expr = Expr::default();
    while let Some(token) = args.next() {
        match token.as_encoded_bytes() {
            b"-name" => expr.names.push(Glob {
                pattern: take(&mut args, "-name")?,
                ignore_case: false,
            }),
            b"-iname" => expr.names.push(Glob {
                pattern: take(&mut args, "-iname")?,
                ignore_case: true,
            }),
            b"-path" => expr.paths.push(Glob {
                pattern: take(&mut args, "-path")?,
                ignore_case: false,
            }),
            b"-type" => {
                let value = take(&mut args, "-type")?;
                match value.as_slice() {
                    [b'f'] => expr.kinds.push(Kind::File),
                    [b'd'] => expr.kinds.push(Kind::Dir),
                    [b'l'] => expr.kinds.push(Kind::Link),
                    _ => return Err(Decision::Fallback),
                }
            }
            b"-maxdepth" => {
                let value = take(&mut args, "-maxdepth")?;
                expr.maxdepth = Some(depth(&value, "-maxdepth")?);
            }
            b"-mindepth" => {
                let value = take(&mut args, "-mindepth")?;
                expr.mindepth = Some(depth(&value, "-mindepth")?);
            }
            b"-newer" => {
                let value = take(&mut args, "-newer")?;
                expr.newers.push(PathBuf::from(OsString::from_vec(value)));
            }
            _ => return Err(Decision::Fallback),
        }
    }
    Ok(Parsed { roots, expr })
}

/// Takes the value after a flag, or the missing-argument error.
fn take(
    args: &mut std::iter::Peekable<std::slice::Iter<'_, OsString>>,
    flag: &str,
) -> Result<Vec<u8>, Decision> {
    match args.next() {
        Some(value) => Ok(value.as_encoded_bytes().to_vec()),
        None => Err(Decision::Error(format!(
            "find: missing argument to '{flag}'"
        ))),
    }
}

/// Parses a depth limit: ASCII digits only.
fn depth(value: &[u8], flag: &str) -> Result<usize, Decision> {
    super::decimal(value).ok_or_else(|| Decision::Error(invalid(flag, value)))
}

/// The bad-depth message.
fn invalid(flag: &str, value: &[u8]) -> String {
    format!(
        "find: invalid argument '{}' for '{flag}'",
        String::from_utf8_lossy(value)
    )
}

/// Reads every `-newer` reference before anything prints, as GNU resolves
/// them up front: a bad reference fails the run with no listing.
fn resolve_newers(cwd: &Path, refs: &[PathBuf]) -> Result<Vec<SystemTime>, String> {
    refs.iter()
        .map(|name| {
            let complaint = |error: io::Error| {
                format!("find: '{}': {}", name.display(), walk::io_message(&error))
            };
            fs::metadata(cwd.join(name))
                .map_err(complaint)?
                .modified()
                .map_err(complaint)
        })
        .collect()
}

/// What visiting one search root found.
struct Visit {
    /// Whether anything failed.
    failed: bool,
    /// Whether any path printed.
    printed: bool,
    /// The directories walked, for the no-match notice.
    walked: Vec<PathBuf>,
}

/// Prints one search root and what the walk finds below it, without
/// following links.
fn visit(
    cwd: &Path,
    root: &Path,
    expr: &Expr,
    newers: &[SystemTime],
    out: &mut Out<'_>,
    stderr: &mut dyn Write,
) -> Visit {
    let mut visit = Visit {
        failed: false,
        printed: false,
        walked: Vec::new(),
    };
    let classified = match walk::root_of(cwd, root) {
        Ok(classified) => classified,
        Err(error) => {
            complaint(stderr, root, &walk::io_message(&error));
            visit.failed = true;
            return visit;
        }
    };
    match classified {
        Root::File(file) => {
            apply(
                check(
                    expr,
                    newers,
                    &file.show,
                    0,
                    kind_of(&file.file_type),
                    &file.read,
                ),
                &mut visit,
                &file.show,
                out,
                stderr,
            );
        }
        Root::Dir(dir) => {
            visit.walked.push(dir.walk.clone());
            // A named link reads as a link, as GNU tests the root itself:
            // a link to a directory matches `-type l`, not `-type d`.
            let kind = match fs::symlink_metadata(cwd.join(root)) {
                Ok(meta) if meta.file_type().is_symlink() => Some(Kind::Link),
                _ => Some(Kind::Dir),
            };
            apply(
                check(expr, newers, &dir.show, 0, kind, &dir.walk),
                &mut visit,
                &dir.show,
                out,
                stderr,
            );
            for entry in walk::walk(cwd, &dir, expr.maxdepth) {
                match entry {
                    Ok(found) => {
                        // The root already had its test.
                        if found.depth == 0 {
                            continue;
                        }
                        let fs = cwd.join(&found.display);
                        apply(
                            check(
                                expr,
                                newers,
                                &found.display,
                                found.depth,
                                kind_of(&found.file_type),
                                &fs,
                            ),
                            &mut visit,
                            &found.display,
                            out,
                            stderr,
                        );
                    }
                    Err(error) => {
                        complaint(stderr, &error.display, &error.message);
                        visit.failed = true;
                    }
                }
            }
        }
    }
    visit
}

/// What testing one entry decided.
enum Tested {
    /// Print the display path.
    Pass,
    /// Say nothing.
    Skip,
    /// Complain with the message, and fail the run.
    Fail(String),
}

/// Prints, skips or complains for what testing one entry decided.
fn apply(
    tested: Tested,
    visit: &mut Visit,
    display: &Path,
    out: &mut Out<'_>,
    stderr: &mut dyn Write,
) {
    match tested {
        Tested::Pass => {
            emit(out, display);
            visit.printed = true;
        }
        Tested::Skip => {}
        Tested::Fail(message) => {
            complaint(stderr, display, &message);
            visit.failed = true;
        }
    }
}

/// Tests one entry: its display path, depth, kind and file for `-newer`.
/// Adjacent tests are ANDed.
fn check(
    expr: &Expr,
    newers: &[SystemTime],
    display: &Path,
    depth: usize,
    kind: Option<Kind>,
    fs: &Path,
) -> Tested {
    if expr.mindepth.is_some_and(|min| depth < min) {
        return Tested::Skip;
    }
    if !expr.kinds.iter().all(|wanted| kind == Some(*wanted)) {
        return Tested::Skip;
    }
    let bytes = display.as_os_str().as_encoded_bytes();
    if !expr
        .paths
        .iter()
        .all(|glob| glob_match(&glob.pattern, bytes, false))
    {
        return Tested::Skip;
    }
    if !expr
        .names
        .iter()
        .all(|glob| glob_match(&glob.pattern, basename(display), glob.ignore_case))
    {
        return Tested::Skip;
    }
    if !newers.is_empty() {
        match modified(fs) {
            Ok(modified) => {
                if !newers.iter().all(|mark| modified > *mark) {
                    return Tested::Skip;
                }
            }
            Err(error) => return Tested::Fail(walk::io_message(&error)),
        }
    }
    Tested::Pass
}

/// The `-type` bucket: links stay links, directories stay directories,
/// regular files are files; anything else, such as a socket, is none.
fn kind_of(file_type: &std::fs::FileType) -> Option<Kind> {
    if file_type.is_symlink() {
        Some(Kind::Link)
    } else if file_type.is_dir() {
        Some(Kind::Dir)
    } else if file_type.is_file() {
        Some(Kind::File)
    } else {
        None
    }
}

/// The modification time for `-newer`: links read as themselves, as the
/// walk never follows them.
fn modified(fs: &Path) -> io::Result<SystemTime> {
    fs::symlink_metadata(fs).and_then(|meta| meta.modified())
}

/// The base name as bytes: the file name, or the whole path for `.`.
pub(crate) fn basename(path: &Path) -> &[u8] {
    path.file_name()
        .map(|name| name.as_encoded_bytes())
        .unwrap_or_else(|| path.as_os_str().as_encoded_bytes())
}

/// Whether `text` matches the glob `pattern`: `*` spans any bytes, `?` one
/// byte, `[...]` a class with ranges and `!`/`^` negation, and `\` quotes
/// the next byte. Matching is ASCII case-insensitive when `ignore_case`.
pub(crate) fn glob_match(pattern: &[u8], text: &[u8], ignore_case: bool) -> bool {
    match_here(pattern, text, ignore_case)
}

/// Whether `pattern` matches the start of `text`, and the rest of both
/// matches after.
fn match_here(pattern: &[u8], text: &[u8], ignore_case: bool) -> bool {
    let (first, rest) = match pattern.split_first() {
        Some(pair) => pair,
        None => return text.is_empty(),
    };
    match first {
        b'*' => (0..=text.len())
            .filter_map(|index| text.get(index..))
            .any(|tail| match_here(rest, tail, ignore_case)),
        b'?' => match text.split_first() {
            Some((_, tail)) => match_here(rest, tail, ignore_case),
            None => false,
        },
        b'\\' => {
            let (literal, rest) = match rest.split_first() {
                Some((byte, rest)) => (*byte, rest),
                // A trailing backslash quotes nothing: a literal backslash.
                None => (b'\\', rest),
            };
            match text.split_first() {
                Some((byte, tail)) if fold(*byte, ignore_case) == fold(literal, ignore_case) => {
                    match_here(rest, tail, ignore_case)
                }
                _ => false,
            }
        }
        b'[' => match_bracket(rest, text, ignore_case),
        _ => match text.split_first() {
            Some((byte, tail)) if fold(*byte, ignore_case) == fold(*first, ignore_case) => {
                match_here(rest, tail, ignore_case)
            }
            _ => false,
        },
    }
}

/// Matches a `[...]` class against `text`'s first byte, then the rest.
/// A `[` without a closer matches itself.
fn match_bracket(after: &[u8], text: &[u8], ignore_case: bool) -> bool {
    let Some(class) = parse_class(after) else {
        return match text.split_first() {
            Some((byte, tail)) if *byte == b'[' => match_here(after, tail, ignore_case),
            _ => false,
        };
    };
    match text.split_first() {
        Some((byte, tail)) => {
            let hit = class.members.iter().any(|(low, high)| {
                fold(*low, ignore_case) <= fold(*byte, ignore_case)
                    && fold(*byte, ignore_case) <= fold(*high, ignore_case)
            });
            if hit != class.negated {
                match_here(class.rest, tail, ignore_case)
            } else {
                false
            }
        }
        None => false,
    }
}

/// A `[...]` class: negation, member ranges and what follows the closer.
struct Class<'a> {
    /// Whether the class is negated.
    negated: bool,
    /// The member ranges, singles as equal ends.
    members: Vec<(u8, u8)>,
    /// What follows the closer.
    rest: &'a [u8],
}

/// Reads a `[...]` class: negation, member ranges and what follows the
/// closer. Returns nothing when no closer follows.
fn parse_class(after: &[u8]) -> Option<Class<'_>> {
    let (negated, mut body) = match after.split_first() {
        Some((b'!' | b'^', rest)) => (true, rest),
        _ => (false, after),
    };
    let mut members = Vec::new();
    // A leading `]` is a literal member, not the closer.
    if let Some((b']', rest)) = body.split_first() {
        members.push((b']', b']'));
        body = rest;
    }
    loop {
        let (byte, rest) = body.split_first()?;
        body = rest;
        if *byte == b']' {
            return Some(Class {
                negated,
                members,
                rest: body,
            });
        }
        if *byte == b'\\' {
            let (escaped, rest) = body.split_first()?;
            members.push((*escaped, *escaped));
            body = rest;
            continue;
        }
        if *byte == b'[' {
            // A POSIX class such as `[:alpha:]` adds its ranges; any
            // other `[` is an ordinary member, as below.
            if let Some(rest) = take_posix(&mut members, body) {
                body = rest;
                continue;
            }
        }
        let low = *byte;
        match body.split_first() {
            Some((b'-', after_dash)) => match after_dash.split_first() {
                Some((high, rest)) if *high != b']' => {
                    members.push((low, *high));
                    body = rest;
                }
                _ => members.push((low, low)),
            },
            _ => members.push((low, low)),
        }
    }
}

/// Adds the POSIX class opening `body` (`:name:]...`) to `members`,
/// returning what follows its closer: nothing when no `:]` follows or the
/// name is unknown, when the `[` stays an ordinary member.
fn take_posix<'a>(members: &mut Vec<(u8, u8)>, body: &'a [u8]) -> Option<&'a [u8]> {
    let (first, body) = body.split_first()?;
    if *first != b':' {
        return None;
    }
    let end = body.windows(2).position(|pair| pair == *b":]")?;
    members.extend(posix_ranges(body.get(..end)?)?);
    body.get(end + 2..)
}

/// The member ranges of the POSIX class `name` (`alpha` in `[:alpha:]`),
/// ASCII only: nothing for an unknown name, when the class stays literal.
fn posix_ranges(name: &[u8]) -> Option<Vec<(u8, u8)>> {
    let ranges: &[(u8, u8)] = match name {
        b"alnum" => &[(b'a', b'z'), (b'A', b'Z'), (b'0', b'9')],
        b"alpha" => &[(b'a', b'z'), (b'A', b'Z')],
        b"blank" => &[(b' ', b' '), (b'\t', b'\t')],
        b"cntrl" => &[(0x00, 0x1f), (0x7f, 0x7f)],
        b"digit" => &[(b'0', b'9')],
        b"graph" => &[(0x21, 0x7e)],
        b"lower" => &[(b'a', b'z')],
        b"print" => &[(0x20, 0x7e)],
        b"punct" => &[(0x21, 0x2f), (0x3a, 0x40), (0x5b, 0x60), (0x7b, 0x7e)],
        b"space" => &[(0x09, 0x0d), (b' ', b' ')],
        b"upper" => &[(b'A', b'Z')],
        b"xdigit" => &[(b'0', b'9'), (b'a', b'f'), (b'A', b'F')],
        _ => return None,
    };
    Some(ranges.to_vec())
}

/// Folds one byte for a case-insensitive match: ASCII only, as the class
/// ranges compare folded ends.
fn fold(byte: u8, ignore_case: bool) -> u8 {
    if ignore_case {
        byte.to_ascii_lowercase()
    } else {
        byte
    }
}

/// Prints one path and its line ending as bytes, so names outside UTF-8
/// print as they are.
fn emit(out: &mut Out<'_>, path: &Path) {
    out.emit(path.as_os_str().as_encoded_bytes());
    out.emit(b"\n");
}

/// Complains to standard error, as GNU does: `find: '<path>': <reason>`.
fn complaint(stderr: &mut dyn Write, path: &Path, message: &str) {
    stderr.write_all(b"find: '").unwrap_or(());
    stderr
        .write_all(path.as_os_str().as_encoded_bytes())
        .unwrap_or(());
    writeln!(stderr, "': {message}").unwrap_or(());
}

#[cfg(test)]
#[path = "find_tests.rs"]
mod tests;
