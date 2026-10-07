//! Replaces credential and signed-header values in logged provider text
//! (`docs/errors.md`, "The shape").

use contract::Secret;

/// The placeholder a secret value is replaced with.
pub const REDACTED: &str = "[redacted]";

/// The values to replace in one logged message: the request's credential
/// value and every header value the signer supplied for the attempt.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    values: Vec<Secret>,
}

impl Secrets {
    /// Adds one secret value. An empty value is ignored, so the scan below
    /// always advances.
    pub fn add(&mut self, value: Secret) {
        if value.expose().is_empty() {
            return;
        }
        self.values.push(value);
    }

    /// Adds a signed header's full value and, when its name is
    /// `authorization` (any ASCII case) and its value is
    /// `<scheme> <credentials>`, the trimmed credentials part too.
    pub fn add_header(&mut self, name: &str, value: &str) {
        self.add(Secret::new(value.to_owned()));
        if name.eq_ignore_ascii_case("authorization")
            && let Some((_, credentials)) = value.split_once(' ')
        {
            self.add(Secret::new(credentials.trim().to_owned()));
        }
    }

    /// Replaces every occurrence of a secret value with `[redacted]`. One
    /// left-to-right scan of the original text; at each position the longest
    /// matching secret wins, and the scan resumes after the match, so an
    /// inserted placeholder is never scanned.
    pub fn redact(&self, text: &str) -> String {
        let mut patterns: Vec<&str> = self
            .values
            .iter()
            // A key leaves `Secret` only on the line that sends it
            // (`docs/code-quality.md`, "Errors"): `expose` here borrows each
            // value only to compare it, copying no key bytes into the output.
            .map(Secret::expose)
            .collect();
        patterns.sort_by_key(|pattern| std::cmp::Reverse(pattern.len()));
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while !rest.is_empty() {
            if let Some(secret) = patterns.iter().find(|pattern| rest.starts_with(**pattern)) {
                out.push_str(REDACTED);
                rest = rest.strip_prefix(*secret).unwrap_or("");
            } else if let Some(next) = rest.chars().next() {
                out.push(next);
                rest = rest.get(next.len_utf8()..).unwrap_or("");
            } else {
                break;
            }
        }
        out
    }
}
