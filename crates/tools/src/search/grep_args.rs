//! The `grep` command line: flags, pattern and paths
//! (`docs/tools.md`, "Search", "Flags").

use std::ffi::OsString;
use std::path::PathBuf;

/// How the pattern reads: basic, extended (`-E`) or fixed (`-F`), last wins.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Mode {
    /// A basic regular expression.
    #[default]
    Basic,
    /// An extended regular expression.
    Extended,
    /// A fixed string.
    Fixed,
}

/// What `grep` was asked to search.
#[derive(Default)]
pub(crate) struct Options {
    /// The pattern as given.
    pub pattern: Vec<u8>,
    /// The paths as given.
    pub paths: Vec<PathBuf>,
    /// Whether `-r` was given.
    pub recursive: bool,
    /// Whether `-n` was given.
    pub line_numbers: bool,
    /// Whether `-i` was given.
    pub ignore_case: bool,
    /// Whether `-v` was given.
    pub invert: bool,
    /// The last of `-E` and `-F`, or basic.
    pub mode: Mode,
    /// Whether `-l` was given.
    pub files_with_matches: bool,
    /// Whether `-c` was given.
    pub count: bool,
    /// Whether `-w` was given.
    pub word: bool,
    /// Whether `-o` was given: each non-empty match prints on its own
    /// line. With `-v`, context or an alternation the call instead runs
    /// the system grep (ruling 17 on #298).
    pub only_matching: bool,
    /// The `-B` context lines.
    pub before: usize,
    /// The `-A` context lines.
    pub after: usize,
    /// The `--include` and `--exclude` globs in the order given, matched
    /// against the base name: the last rule a file matches decides.
    pub filters: Vec<Filter>,
}

/// One `--include` or `--exclude` rule, in the order given on the command
/// line: the last rule a file matches decides whether it is searched.
pub(crate) struct Filter {
    /// Whether the rule includes rather than excludes.
    pub include: bool,
    /// The glob, matched against the base name.
    pub glob: Vec<u8>,
}

/// Why parsing declined.
pub(crate) enum Parsed {
    /// The search to run.
    Run(Options),
    /// A flag the built-in does not handle.
    Fallback,
    /// A bad invocation: the message, already a full line.
    Error(String),
}

/// Splits flags from the pattern and paths. Short flags combine, `--` ends
/// flags, and a flag-like operand after the pattern hands the call over, as
/// the system permutes what the built-in reads in order.
/// The iterator advances in this loop, never inside a flag helper, so a
/// declined helper cannot spin it.
pub(crate) fn parse(args: &[OsString]) -> Parsed {
    let mut options = Options::default();
    let mut operands: Vec<PathBuf> = Vec::new();
    let mut args = args.iter().peekable();
    let mut flags = true;
    let mut dashes = false;
    while let Some(arg) = args.next() {
        let bytes = arg.as_encoded_bytes();
        if !flags {
            // The system permutes a flag-like operand into the flags; the
            // built-in reads in order, so it hands the call over instead.
            // Past `--` all is operands, and a lone `-` is standard input.
            if !dashes && bytes.len() > 1 && bytes.first() == Some(&b'-') {
                return Parsed::Fallback;
            }
            operands.push(PathBuf::from(arg));
            continue;
        }
        if bytes == b"--" {
            flags = false;
            dashes = true;
            continue;
        }
        if bytes.len() > 1 && bytes.first() == Some(&b'-') && bytes.get(1) != Some(&b'-') {
            let cluster = bytes.to_vec();
            if let Err(decision) = short(&mut options, &cluster, &mut args) {
                return decision.parsed();
            }
            continue;
        }
        if is_long(bytes) {
            let body = bytes.get(2..).unwrap_or_default().to_vec();
            if let Err(decision) = long(&mut options, &body, &mut args) {
                return decision.parsed();
            }
            continue;
        }
        flags = false;
        operands.push(PathBuf::from(arg));
    }
    let mut paths = operands.into_iter();
    let Some(pattern) = paths.next() else {
        // No pattern: the system prints its usage.
        return Parsed::Fallback;
    };
    options.pattern = pattern.as_os_str().as_encoded_bytes().to_vec();
    options.paths = paths.collect();
    Parsed::Run(options)
}

/// Whether `bytes` opens a long flag: `--` plus a name. `--` alone is
/// claimed by the exact arm above, so any `--` prefix here is longer.
fn is_long(bytes: &[u8]) -> bool {
    bytes.starts_with(b"--")
}

/// Why a flag parser declined.
enum Decision {
    /// A flag the built-in does not handle.
    Fallback,
    /// A bad invocation: the message, already a full line.
    Error(String),
}

impl Decision {
    /// Lifts the decision into what parsing reports.
    fn parsed(self) -> Parsed {
        match self {
            Decision::Fallback => Parsed::Fallback,
            Decision::Error(message) => Parsed::Error(message),
        }
    }
}

/// Parses one short-flag cluster, taking any value it needs from the
/// arguments after it.
fn short(
    options: &mut Options,
    cluster: &[u8],
    args: &mut std::iter::Peekable<std::slice::Iter<'_, OsString>>,
) -> Result<(), Decision> {
    let mut rest = cluster.get(1..).unwrap_or_default();
    while let Some((flag, tail)) = rest.split_first() {
        rest = tail;
        match flag {
            b'n' => options.line_numbers = true,
            b'r' => options.recursive = true,
            b'i' => options.ignore_case = true,
            b'v' => options.invert = true,
            b'E' => options.mode = Mode::Extended,
            b'F' => options.mode = Mode::Fixed,
            b'l' => options.files_with_matches = true,
            b'c' => options.count = true,
            b'w' => options.word = true,
            b'o' => options.only_matching = true,
            b'A' | b'B' | b'C' => {
                let value = attached(rest, args, *flag)?;
                rest = &[];
                let count = super::decimal(&value).ok_or_else(|| {
                    Decision::Error(format!(
                        "grep: '{}': invalid context length argument",
                        String::from_utf8_lossy(&value)
                    ))
                })?;
                // `-C` sets both; a later `-A` or `-B` wins its own side.
                if *flag == b'A' || *flag == b'C' {
                    options.after = count;
                }
                if *flag == b'B' || *flag == b'C' {
                    options.before = count;
                }
            }
            _ => return Err(Decision::Fallback),
        }
    }
    Ok(())
}

/// The value of a context flag: what the cluster holds after it, or the
/// next argument.
fn attached(
    rest: &[u8],
    args: &mut std::iter::Peekable<std::slice::Iter<'_, OsString>>,
    flag: u8,
) -> Result<Vec<u8>, Decision> {
    if !rest.is_empty() {
        return Ok(rest.to_vec());
    }
    match args.next() {
        Some(value) => Ok(value.as_encoded_bytes().to_vec()),
        None => Err(Decision::Error(format!(
            "grep: option '-{}' requires an argument",
            flag as char
        ))),
    }
}

/// Parses one long flag, `body` past the `--`, taking any value it needs
/// from the arguments after it.
fn long(
    options: &mut Options,
    body: &[u8],
    args: &mut std::iter::Peekable<std::slice::Iter<'_, OsString>>,
) -> Result<(), Decision> {
    let (name, inline) = match body.iter().position(|byte| *byte == b'=') {
        Some(found) => {
            let (name, rest) = body.split_at(found);
            (name, Some(rest.get(1..).unwrap_or_default().to_vec()))
        }
        None => (body, None),
    };
    let name = String::from_utf8_lossy(name).into_owned();
    if name != "include" && name != "exclude" {
        return Err(Decision::Fallback);
    }
    let value = match inline {
        Some(value) => value,
        None => match args.next() {
            Some(value) => value.as_encoded_bytes().to_vec(),
            None => {
                return Err(Decision::Error(format!(
                    "grep: option '--{name}' requires an argument"
                )));
            }
        },
    };
    if name == "include" {
        options.filters.push(Filter {
            include: true,
            glob: value,
        });
    } else {
        options.filters.push(Filter {
            include: false,
            glob: value,
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "grep_args_tests.rs"]
mod tests;
