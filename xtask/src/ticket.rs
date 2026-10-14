//! The resolved issue a pull request body names, via GitHub's closing
//! keywords. It belongs to no other `select` command; `main.rs` is its only
//! caller.

const TICKET_WORDS: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Every issue a pull request body resolves, in order of appearance, from
/// "Resolves #N" lines and GitHub's other closing keywords.
pub(crate) fn ticket(body: &str) -> Vec<String> {
    let mut numbers = Vec::new();
    let mut prev = ' ';
    for (i, c) in body.char_indices() {
        if c.is_alphabetic()
            && !is_word(prev)
            && let Some(number) = ticket_at(body.get(i..).unwrap_or_default())
        {
            numbers.push(number);
        }
        prev = c;
    }
    numbers
}

fn ticket_at(text: &str) -> Option<String> {
    let word_end = text
        .find(|c: char| !c.is_alphabetic())
        .unwrap_or(text.len());
    let (word, rest) = text.split_at(word_end);
    if !TICKET_WORDS.contains(&word.to_lowercase().as_str()) {
        return None;
    }
    let after_space = rest.trim_start();
    if after_space.len() == rest.len() {
        return None;
    }
    let digits_and_more = after_space.strip_prefix('#')?;
    let end = digits_and_more
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits_and_more.len());
    let (digits, tail) = digits_and_more.split_at(end);
    if digits.is_empty() || tail.starts_with(is_word) {
        return None;
    }
    Some(digits.to_owned())
}

#[cfg(test)]
#[path = "ticket_tests.rs"]
mod tests;
