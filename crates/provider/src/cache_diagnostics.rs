//! Redacted digests of a request's cacheable prefix segments
//! (`docs/prompt-cache.md`, "Cache diagnostics"). Each digest is the
//! SHA-256 of the encoded segment as it went on the wire, so equal bytes
//! hash equal and the bytes themselves never reach the line.

use serde_json::Value;

/// The digest of one segment value as the body sends it: objects
/// serialize with their keys sorted, because serde_json's
/// `preserve_order` is never on (`docs/prompt-cache.md`, "Bytes").
pub(crate) fn segment(value: &Value) -> String {
    support::sha256_hex(value.to_string().as_bytes())
}

/// Each cumulative message prefix's digest, in order: the digest of the
/// first `i` items serialized as sent, so an appended message moves no
/// earlier prefix and every change propagates to the new end.
pub(crate) fn cumulative(items: &[Value]) -> Vec<String> {
    (1..=items.len())
        .map(|end| segment(&Value::Array(items.iter().take(end).cloned().collect())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_nist_vectors() {
        assert_eq!(
            support::sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            support::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_digest_is_deterministic_lowercase_hex() {
        let first = support::sha256_hex(b"system prompt bytes");
        assert_eq!(first, support::sha256_hex(b"system prompt bytes"));
        assert_eq!(first.len(), 64);
        assert!(
            first
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "lowercase hex only: {first}"
        );
    }

    #[test]
    fn a_digest_holds_no_byte_of_its_input() {
        let prompt = "You are terse.";
        let hashed = support::sha256_hex(prompt.as_bytes());
        assert!(!hashed.contains(prompt), "no prompt text in {hashed}");
        assert_ne!(hashed, prompt);
        assert_ne!(support::sha256_hex(b"You are verbose."), hashed);
    }

    #[test]
    fn a_segment_hashes_the_wire_serialization() {
        let value = serde_json::json!({"b": 1, "a": [true, "x"]});
        assert_eq!(
            segment(&value),
            support::sha256_hex(br#"{"a":[true,"x"],"b":1}"#),
            "keys sort as the body sends them"
        );
    }

    #[test]
    fn cumulative_prefixes_extend_without_moving() {
        let (a, b, c) = (
            serde_json::json!({"role": "user", "content": "hi"}),
            serde_json::json!({"role": "assistant", "content": "hello"}),
            serde_json::json!({"role": "user", "content": "again"}),
        );
        let two = cumulative(&[a.clone(), b.clone()]);
        let three = cumulative(&[a.clone(), b.clone(), c.clone()]);
        assert_eq!(two.len(), 2);
        assert_eq!(three.len(), 3);
        assert_eq!(
            two[0], three[0],
            "an appended message moves no earlier prefix"
        );
        assert_eq!(two[0], segment(&serde_json::Value::Array(vec![a.clone()])));
        assert_ne!(two[1], two[0], "a new boundary hashes differently");
        assert_ne!(three[2], two[1], "the change propagates to the new end");
        assert!(cumulative(&[]).is_empty());
    }
}
