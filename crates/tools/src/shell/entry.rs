//! The read-only lookup: one lookup over the built-in list and the
//! person's `shell.read_only` entries, by command path.
//!
//! The built-in list always wins: a call matching a built-in entry never
//! consults configuration, and a configured entry under a built-in path (or
//! a basename the built-ins own) is never consulted. Anything neither owns
//! is not read-only.

use super::classify::Outcome;
use super::read_only::{COMMANDS, Command, EXACT, Flag};
use super::syntax::Word;

/// What the lookup found for a stage's words.
pub(super) enum Found<'a> {
    /// A built-in entry, matched by its longest command path.
    Builtin(&'static Command),
    /// The person's first matching entry: its flag spellings, and how
    /// many leading words it consumes.
    Configured(&'a [String], usize),
    /// No entry owns the words.
    None,
}

/// Find the entry for a stage's words: the longest matching built-in path
/// first, then the person's first matching entry unless a built-in owns its
/// name. An empty word list matches nothing.
pub(super) fn find<'a>(words: &[Word], configured: &'a [(String, Vec<String>)]) -> Found<'a> {
    if words.is_empty() {
        return Found::None;
    }
    if let Some(entry) = locate_builtin(words) {
        return Found::Builtin(entry);
    }
    match locate_configured(words, configured) {
        Some((consumed, flags)) => Found::Configured(flags, consumed),
        None => Found::None,
    }
}

/// Whether a configured entry name is never consulted: `sed`, which has its
/// own grammar, a wrapper or `cd` step, which the classifier unwraps itself,
/// a path-qualified basename the built-ins own, or a name a built-in or
/// exact-match path covers.
pub(super) fn owned(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if name
        .rsplit('/')
        .next()
        .is_some_and(|base| matches!(base, "cd" | "env" | "timeout" | "xargs" | "sed"))
    {
        return true;
    }
    if name.contains('/') {
        let base = name.rsplit('/').next().unwrap_or(name);
        if COMMANDS.iter().any(|entry| {
            entry
                .name
                .split(' ')
                .next()
                .is_some_and(|first| first == base)
        }) {
            return true;
        }
    }
    COMMANDS.iter().any(|entry| starts_with(name, entry.name))
        || EXACT.iter().any(|row| starts_with(name, &row.join(" ")))
}

/// Whether `path` starts with `prefix`, word for word. A longer built-in
/// path never covers a shorter configured name: only a built-in path at or
/// above the name owns it.
fn starts_with(path: &str, prefix: &str) -> bool {
    let mut path = path.split(' ');
    prefix.split(' ').all(|part| path.next() == Some(part))
}

/// The longest built-in entry whose command path matches the words.
/// Configuration never reaches this walk.
fn locate_builtin(words: &[Word]) -> Option<&'static Command> {
    COMMANDS
        .iter()
        .filter(|entry| path_matches(entry.name, words))
        .max_by_key(|entry| entry.name.split(' ').count())
}

/// Whether a command path matches the words: every path word equals the
/// word at its position. The words may hold more; a shorter word list
/// never matches.
fn path_matches(name: &str, words: &[Word]) -> bool {
    let mut rest = words.iter();
    for part in name.split(' ') {
        match rest.next() {
            Some(word) if word.cooked == part => {}
            _ => return false,
        }
    }
    true
}

/// The person's first matching entry and how many words it consumes: the
/// command word matches exactly, every subcommand word matches in order,
/// and no built-in owns the name. An empty name matches only an empty first
/// word.
fn locate_configured<'a>(
    words: &[Word],
    configured: &'a [(String, Vec<String>)],
) -> Option<(usize, &'a [String])> {
    let first = words.first()?;
    for (name, flags) in configured {
        // A single word equal to the whole name, such as `'my tool'`: the
        // person named one command holding a space.
        if first.cooked == *name && !owned(name) {
            return Some((1, flags));
        }
        // A name holding `/` is a path: unquoted, the shell splits it, so
        // it matches only whole. A slash-free name is a subcommand path
        // and matches word for word.
        if name.contains('/') {
            continue;
        }
        let mut parts = name.split(' ');
        if parts.next() != Some(first.cooked.as_str()) {
            continue;
        }
        if owned(name) {
            continue;
        }
        let mut len = 1;
        let mut rest = words.iter().skip(1);
        let mut matches = true;
        for part in parts {
            match rest.next() {
                Some(word) if word.cooked == part => len += 1,
                _ => {
                    matches = false;
                    break;
                }
            }
        }
        if matches {
            return Some((len, flags));
        }
    }
    None
}

/// Where one word falls in a built-in entry's grammar: an operand, the end
/// of flags, or a flag the entry must then accept.
pub(super) enum WordAt {
    Operand(String),
    EndFlags,
    Flag,
}

pub(super) fn word_at(word: &Word, ended: bool, entry: &Command) -> WordAt {
    if ended || !word.cooked.starts_with('-') {
        WordAt::Operand(word.cooked.clone())
    } else if entry.ends_flags && word.cooked == "--" {
        WordAt::EndFlags
    } else if entry.prints && !is_option_word(&word.cooked) {
        // bash's `echo` reads any other word as text.
        WordAt::Operand(word.cooked.clone())
    } else {
        WordAt::Flag
    }
}

/// `-` followed by one or more of `n`, `e`, `E`: the only words bash's
/// `echo` reads as options.
fn is_option_word(cooked: &str) -> bool {
    let Some(rest) = cooked.strip_prefix('-') else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|ch| matches!(ch, 'n' | 'e' | 'E'))
}

/// Whether the entry accepts one flag word, consuming its value when the
/// flag takes one. Clusters, `--name=value` and `-<digits>` follow the
/// built-in grammar; anything else is refused. An inline-only value flag
/// takes its value only after `=`: a bare word with its spelling refuses,
/// so it never consumes the next word.
pub(super) fn accept_flag(
    entry: &Command,
    words: &mut std::slice::Iter<'_, Word>,
    cooked: &str,
) -> Option<()> {
    if let Some(name) = long_value_name(cooked) {
        let inline = cooked.split_once('=').map_or("", |(_, value)| value);
        return entry
            .flags
            .iter()
            .any(|flag| {
                flag.takes_value()
                    && flag.spelling == name
                    && (!flag.digits_only() || digits_value(cooked))
                    && (flag.spelling != "--jq" || jq_program_ok(inline))
            })
            .then_some(());
    }
    if let Some(flag) = entry.flags.iter().find(|flag| flag.spelling == cooked) {
        if flag.inline_only() {
            return None;
        }
        if flag.takes_value() {
            // A value-taking flag never consumes a substituted word: the
            // substitution would hide the next path, as in
            // `git log -n $(…) .env`.
            if words.clone().next().is_some_and(|word| word.substituted) {
                return None;
            }
            let value = words.next()?;
            if flag.spelling == "--jq" && !jq_program_ok(&value.cooked) {
                return None;
            }
        }
        return Some(());
    }
    // `-U<digits>`, the attached short form of `--unified=<digits>`, as in
    // `git diff -U80`. Only an entry listing `--unified` takes it; a bare
    // `-U` never consumes the next word.
    if let Some(digits) = cooked.strip_prefix("-U")
        && !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && entry.flags.iter().any(|flag| flag.spelling == "--unified")
    {
        return Some(());
    }
    // `git log -<digits>`, as in `git log -8`.
    if entry.counts && is_count(cooked) {
        return Some(());
    }
    short_cluster(entry, cooked).then_some(())
}

/// Whether the text after the first `=` holds only ASCII digits, never
/// empty, as in `--unified=5`.
fn digits_value(cooked: &str) -> bool {
    cooked.split_once('=').is_some_and(|(_, value)| {
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// Whether a `--jq` program keeps the call read-only: it holds none of
/// `env`, `ENV` or `input` as a substring, matched case-sensitively, so
/// `$ENV`, `env.X` and `input_filename` all refuse. gh evaluates `--jq`
/// in-process with the process environment loaded, which would print the
/// environment into the result: the same leak the `/proc/` rule blocks.
fn jq_program_ok(program: &str) -> bool {
    !program.contains("env") && !program.contains("ENV") && !program.contains("input")
}

/// `--name=value`, and only that form. A short flag does not take `=`.
fn long_value_name(cooked: &str) -> Option<&str> {
    let (name, _) = cooked.split_once('=')?;
    name.starts_with("--").then_some(name)
}

/// `-<one or more ASCII digits>`, as in `git log -8`.
fn is_count(cooked: &str) -> bool {
    let Some(rest) = cooked.strip_prefix('-') else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit())
}

/// Whether every letter of a short cluster names a flag without a value.
fn short_cluster(entry: &Command, cooked: &str) -> bool {
    let Some(rest) = cooked.strip_prefix('-') else {
        return false;
    };
    !rest.is_empty()
        && rest.chars().all(|ch| {
            entry
                .flags
                .iter()
                .any(|flag| short_letter(flag) == Some(ch))
        })
}

/// The letter of a value-less short flag: `-n` gives `n`, `--long` and
/// `-n <value>` give none.
fn short_letter(flag: &Flag) -> Option<char> {
    if flag.takes_value() {
        return None;
    }
    let rest = flag.spelling.strip_prefix('-')?;
    if rest.starts_with('-') {
        return None;
    }
    let mut chars = rest.chars();
    let letter = chars.next()?;
    chars.next().is_none().then_some(letter)
}

/// The configured read-only rule for one part: every cooked word starting
/// `-` is a flag, any other word a path operand. A listed spelling ending
/// `=` takes a value after that exact prefix, or as the next word; any
/// other listed spelling matches exactly and takes none. `--` ends no
/// flags here: it must be listed to read. Flag values are never paths.
pub(super) fn configured_outcome(
    flags: &[String],
    words: std::slice::Iter<'_, Word>,
    dirs: &[String],
) -> Outcome {
    let mut words = words;
    let mut operands = Vec::new();
    while let Some(word) = words.next() {
        if !word.cooked.starts_with('-') {
            operands.push(word.cooked.clone());
            continue;
        }
        // An exact plain spelling wins over its `=` twin, in either list
        // order: the word takes no value and the next word stays an operand.
        if flags
            .iter()
            .any(|flag| !flag.ends_with('=') && *flag == word.cooked)
        {
            continue;
        }
        // An inline value after the exact `=` prefix, empty allowed.
        if flags
            .iter()
            .any(|flag| flag.ends_with('=') && word.cooked.starts_with(flag.as_str()))
        {
            continue;
        }
        // A separate value word, a leading `-` allowed; missing executes.
        if flags.iter().any(|flag| {
            flag.strip_suffix('=')
                .is_some_and(|base| base == word.cooked)
        }) {
            if words.next().is_none() {
                return Outcome::Executes;
            }
            continue;
        }
        return Outcome::Executes;
    }
    super::classify::path_tail(&operands, dirs)
}

#[cfg(test)]
#[path = "entry_tests.rs"]
mod tests;
