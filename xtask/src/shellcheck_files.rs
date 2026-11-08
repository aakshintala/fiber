//! `cargo xtask shellcheck-files [DIR]`: the bash files `scripts/check`
//! shellchecks (`docs/ci.md`, "On every pull request that changes code").
//! A file under DIR counts when its first line is a shebang whose
//! interpreter is `bash`, directly or through `env`; every `lib/*.sh`
//! directly under DIR counts whatever its first line. `DIR/install.sh`
//! never counts: it keeps its own `--shell=sh` check.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// The directory listed when no argument names one.
const DEFAULT_DIR: &str = "scripts";

/// The interpreter a shebang line names, or `None` when it names none.
/// Splits on ASCII whitespace without unquoting, as the kernel does. An
/// `env` interpreter hands over to the first token that is not an option,
/// an option's argument or a `NAME=value` assignment.
fn interpreter(line: &str) -> Option<&str> {
    let mut tokens = line.strip_prefix("#!")?.split_ascii_whitespace();
    let first = tokens.next()?;
    if basename(first) != "env" {
        return Some(first);
    }
    while let Some(token) = tokens.next() {
        if let Some(long) = token.strip_prefix("--") {
            if long == "unset" || long == "chdir" {
                tokens.next();
            }
        } else if let Some(cluster) = token.strip_prefix('-') {
            // A cluster of short flags: `u` and `C` take an argument, the
            // rest of the cluster when there is one, else the next token.
            // `S` takes no argument of its own: the loop reads the tokens
            // after it as ordinary tokens.
            for (at, flag) in cluster.char_indices() {
                if flag == 'u' || flag == 'C' {
                    if cluster[at + 1..].is_empty() {
                        tokens.next();
                    }
                    break;
                }
            }
        } else if !token.contains('=') {
            return Some(token);
        }
    }
    None
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether `line` (a file's first line without its `\n`) names `bash`.
fn is_bash_shebang(line: &str) -> bool {
    interpreter(line).is_some_and(|name| basename(name) == "bash")
}

/// Whether the file at `path` starts with a bash shebang line.
fn starts_with_bash_shebang(path: &Path) -> Result<bool, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut line = Vec::new();
    BufReader::new(fs::File::open(path).map_err(fail)?)
        .read_until(b'\n', &mut line)
        .map_err(fail)?;
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    Ok(String::from_utf8(line).is_ok_and(|line| is_bash_shebang(&line)))
}

/// Adds the listed files under `path` to `out`; `rel` is `path` below the
/// directory the walk started at, `""` for that directory itself. Files are
/// reported as `shown` joined with their `rel` path.
fn walk(path: &Path, shown: &str, rel: &str, out: &mut BTreeSet<String>) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    for entry in fs::read_dir(path).map_err(fail)? {
        let entry = entry.map_err(fail)?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| format!("{}: name is not UTF-8", entry.path().display()))?;
        let child = if rel.is_empty() {
            name.to_owned()
        } else {
            format!("{rel}/{name}")
        };
        let kind = entry.file_type().map_err(fail)?;
        if kind.is_dir() {
            walk(&entry.path(), shown, &child, out)?;
        } else if kind.is_file() && child != "install.sh" {
            let in_lib = child
                .strip_prefix("lib/")
                .is_some_and(|rest| !rest.contains('/') && rest.ends_with(".sh"));
            if in_lib || starts_with_bash_shebang(&entry.path())? {
                out.insert(format!("{shown}/{child}"));
            }
        }
    }
    Ok(())
}

/// The sorted, de-duplicated bash files under `dir`.
fn files(dir: &Path) -> Result<Vec<String>, String> {
    if !dir.is_dir() {
        return Err(format!("{}: not a directory", dir.display()));
    }
    let mut out = BTreeSet::new();
    let shown = dir.to_string_lossy();
    walk(dir, shown.trim_end_matches('/'), "", &mut out)?;
    Ok(out.into_iter().collect())
}

/// `shellcheck-files [DIR]`: prints one path per line. Fails (exit 1) with
/// no path at all, since the real tree always holds `lib/release.sh` and an
/// empty list would drop the check without a sound.
pub(crate) fn run(rest: &[String]) -> Result<bool, String> {
    let dir = match rest {
        [] => DEFAULT_DIR,
        [dir] => dir,
        _ => return Err("usage: cargo xtask shellcheck-files [DIR]".to_owned()),
    };
    let list = files(Path::new(dir)).map_err(|e| format!("shellcheck-files: {e}"))?;
    if list.is_empty() {
        eprintln!("shellcheck-files: no bash files under {dir}");
        return Ok(false);
    }
    for path in list {
        println!("{path}");
    }
    Ok(true)
}

#[cfg(test)]
#[path = "shellcheck_files_tests.rs"]
mod tests;
