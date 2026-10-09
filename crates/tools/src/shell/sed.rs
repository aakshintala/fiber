//! `sed` in print-only form (`docs/tools.md`, "Shell", "Effects"): every
//! script is addresses followed by `p`, and every operand left over is a
//! file. GNU permutes options but BSD sed does not, so a flag after the
//! first operand is not read-only.

/// The file operands of `sed`, in order, when every flag is print-only and
/// every script prints only. `words` are the cooked words after `sed`,
/// redirects removed. `None` is a flag or script this classifier cannot
/// read plainly.
pub(super) fn files<'a>(words: impl IntoIterator<Item = &'a str>) -> Option<Vec<String>> {
    let mut rest = words.into_iter().peekable();
    let mut scripts: Vec<String> = Vec::new();
    let mut operands: Vec<String> = Vec::new();
    let mut seen_e = false;
    let mut ended = false;
    let mut first_seen = false;
    while let Some(word) = rest.next() {
        if !ended && !first_seen && word == "--" {
            ended = true;
            continue;
        }
        if !ended && !first_seen && word.starts_with("--") {
            return None;
        }
        if !ended && !first_seen && word.starts_with('-') && word != "-" {
            cluster(word, &mut rest, &mut scripts, &mut seen_e)?;
            continue;
        }
        if word == "-" && !ended && !first_seen {
            return None;
        }
        // After `--` every word is an operand, on GNU and BSD alike. Any
        // other `-` word after the first operand is a flag BSD would read
        // as a file.
        if !ended && first_seen && word.starts_with('-') {
            return None;
        }
        first_seen = true;
        operands.push(word.to_owned());
    }
    if seen_e {
        for script in &scripts {
            if !prints_only(script) {
                return None;
            }
        }
        return Some(operands);
    }
    // Without `-e` the first operand is the script, as for `grep`.
    if operands.is_empty() {
        return None;
    }
    let script = operands.remove(0);
    if !prints_only(&script) {
        return None;
    }
    Some(operands)
}

/// Walks one short-flag cluster, pushing each `-e` script. `None` is a
/// flag that can write or run, or an `-e` with no script.
fn cluster<'a>(
    word: &str,
    rest: &mut impl Iterator<Item = &'a str>,
    scripts: &mut Vec<String>,
    seen_e: &mut bool,
) -> Option<()> {
    let mut letters = word.strip_prefix('-')?.chars();
    for letter in letters.by_ref() {
        match letter {
            'n' | 'E' | 'r' => {}
            'e' => {
                // `e` takes the rest of its word as the script, if any.
                *seen_e = true;
                let tail: String = letters.collect();
                if !tail.is_empty() {
                    scripts.push(tail);
                } else {
                    scripts.push(rest.next()?.to_owned());
                }
                return Some(());
            }
            _ => return None,
        }
    }
    Some(())
}

/// Whether `script` prints only: one or more commands of up to two
/// addresses followed by `p`, separated by `;` or a newline. A range whose
/// first address is a number of value zero is GNU's `0,/re/` form, not a
/// print.
pub(super) fn prints_only(script: &str) -> bool {
    let mut chars = script.chars().peekable();
    skip_run(&mut chars);
    if chars.peek().is_none() {
        return false;
    }
    if !command(&mut chars) {
        return false;
    }
    loop {
        skip_blanks(&mut chars);
        match chars.peek() {
            None => return true,
            Some(&sep) if sep == ';' || sep == '\n' => {
                // Separators in a run read as empty commands.
                skip_run(&mut chars);
                if chars.peek().is_none() {
                    return true;
                }
                if !command(&mut chars) {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// Blanks and separators, in any order.
fn skip_run(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while chars
        .peek()
        .is_some_and(|&ch| ch == ' ' || ch == '\t' || ch == ';' || ch == '\n')
    {
        chars.next();
    }
}

/// Blanks only.
fn skip_blanks(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while chars.peek().is_some_and(|&ch| ch == ' ' || ch == '\t') {
        chars.next();
    }
}

/// One command: up to two addresses, blanks, then `p`.
fn command(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    skip_blanks(chars);
    let first = match address(chars) {
        Err(_) => return false,
        Ok(first) => first,
    };
    if let Some(zero) = first {
        if chars.peek() == Some(&',') {
            chars.next();
            // No blanks around `,`, and the second address is required.
            if !matches!(address(chars), Ok(Some(_))) {
                return false;
            }
            // A zero-start range is GNU's `0,/re/` form.
            if zero {
                return false;
            }
        }
    } else if chars.peek() == Some(&',') {
        return false;
    }
    skip_blanks(chars);
    if chars.next() != Some('p') {
        return false;
    }
    true
}

/// One address. The boolean is whether it is a number of value zero.
/// `Ok(None)` is no address here; `Err(())` is a malformed address, which
/// must fail the command rather than read as an absent address.
fn address(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<Option<bool>, ()> {
    match chars.peek() {
        Some(&'$') => {
            chars.next();
            Ok(Some(false))
        }
        Some(&ch) if ch.is_ascii_digit() => {
            let mut zero = true;
            while let Some(&digit) = chars.peek() {
                if !digit.is_ascii_digit() {
                    break;
                }
                zero &= digit == '0';
                chars.next();
            }
            Ok(Some(zero))
        }
        Some(&'/') => {
            if regex(chars).is_none() {
                return Err(());
            }
            Ok(Some(false))
        }
        _ => Ok(None),
    }
}

/// `/.../`, where `\` takes the next character literally. A newline inside,
/// a trailing `\`, or a missing close is not an address.
fn regex(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<()> {
    chars.next()?;
    loop {
        match chars.next()? {
            '/' => return Some(()),
            '\n' => return None,
            '\\' => match chars.next() {
                Some('\n') | None => return None,
                Some(_) => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "sed_tests.rs"]
mod tests;
