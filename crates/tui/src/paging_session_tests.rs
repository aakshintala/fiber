//! Tests for the heavy session behind the `paging` jig: its shape is the
//! benchmark's input, so the counts and byte length are pinned. A change
//! to the generator moves these numbers on purpose, and the test names
//! the new shape.

use super::paging_session;

/// The session at scale 1, as lines.
fn lines() -> Vec<String> {
    paging_session(1).lines().map(str::to_owned).collect()
}

/// How many lines of `kind` the session holds.
fn count(lines: &[String], kind: &str) -> usize {
    let needle = format!("\"{kind}\"");
    lines.iter().filter(|line| line.contains(&needle)).count()
}

#[test]
fn words_wraps_a_seed_past_the_source_length() {
    assert_eq!(super::words(20, 80), "ssion log holds ever");
}

#[test]
fn scale_1_holds_8555_lines() {
    assert_eq!(lines().len(), 8555);
}

#[test]
fn scale_1_holds_8285_durable_lines() {
    let durable = lines()
        .iter()
        .filter(|line| line.contains("\"seq\""))
        .count();
    assert_eq!(durable, 8285);
}

#[test]
fn scale_1_holds_11_turns_with_the_running_turn() {
    assert_eq!(count(&lines(), "turn_started"), 11);
}

#[test]
fn scale_1_holds_1081_tool_calls() {
    assert_eq!(count(&lines(), "tool_call_requested"), 1081);
}

#[test]
fn scale_1_is_5071459_bytes() {
    assert_eq!(paging_session(1).len(), 5_071_459);
}

#[test]
fn scale_1_lines_decode_against_the_contract() {
    for (index, line) in paging_session(1).lines().enumerate() {
        let line_number = index + 1;
        let envelope: contract::Envelope = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("line {line_number} does not parse: {err}"));
        let decoded = contract::events::Event::from_envelope(&envelope).unwrap_or_else(|err| {
            panic!(
                "line {line_number} ({}) does not decode: {err}",
                envelope.kind
            )
        });
        assert!(
            decoded.is_some(),
            "line {line_number} ({}) is unknown to the contract",
            envelope.kind
        );
    }
}
