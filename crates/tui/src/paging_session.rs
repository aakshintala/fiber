//! The heavy session behind the `paging` jig and the wheel benchmark:
//! 10 turns, 965 steps with text, about 1,051 tool calls and 8,074 lines
//! before the trailing running turn, in Fiber's own event kinds.
//! Shaped like the prototype's heavy fixture
//! (`research/tui-prototype/LARGE.md`).

use std::fmt::Write as _;

/// Turns in the session at scale 1.
const TURNS: usize = 10;

/// Steps in a turn, each with text: 965 over the session.
const STEPS: [usize; 2] = [96, 97];

/// Assistant text lengths, cycled: median about 193 characters, a long
/// tail to 4,700 (`research/tui-prototype/LARGE.md`).
const TEXT: [usize; 10] = [120, 193, 150, 90, 193, 250, 100, 300, 180, 900];

/// Tool result lengths, cycled: about 3 KiB on average, as the heavy
/// fixture's `tool_call_completed` lines are.
const OUTPUT: [usize; 8] = [400, 1200, 2600, 3000, 9000, 600, 2000, 5200];

/// Writes a session's durable lines, numbered from 0.
struct Log {
    out: String,
    seq: u64,
    session: String,
}

impl Log {
    fn new(session: &str) -> Self {
        Self {
            out: String::new(),
            seq: 0,
            session: session.to_owned(),
        }
    }

    /// A durable line, numbered.
    fn line(&mut self, kind: &str, action: Option<&str>, payload: &serde_json::Value) {
        self.write(kind, action, payload, true);
    }

    /// A line as the log holds it, or a delta, which carries no `seq`.
    fn write(
        &mut self,
        kind: &str,
        action: Option<&str>,
        payload: &serde_json::Value,
        durable: bool,
    ) {
        let mut line = serde_json::json!({
            "kind": kind,
            "session_id": &self.session,
            "ts": 1_790_604_120_000_u64 + self.seq,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": payload,
        });
        if let Some(object) = line.as_object_mut() {
            if let Some(action) = action {
                object.insert("action_id".to_owned(), serde_json::json!(action));
            }
            if durable {
                object.insert("seq".to_owned(), serde_json::json!(self.seq));
                self.seq += 1;
            }
        }
        writeln!(self.out, "{line}").unwrap_or(());
    }

    /// A turn still running: 30 steps, each a reply streamed in eight
    /// deltas and a tool call, and no `turn_completed`.
    fn running_turn(&mut self) {
        let empty = serde_json::json!({});
        let started = serde_json::json!({"effects": [], "reversible": false});
        self.line(
            "turn_started",
            None,
            &serde_json::json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": "keep going"}]}]}),
        );
        let part = "streamed text, a few words at a time; ";
        for step in 0..30 {
            let (message, call) = (format!("a_live_m{step}"), format!("a_live_t{step}"));
            self.line("step_started", None, &empty);
            self.line("assistant_message_started", Some(&message), &empty);
            for _ in 0..8 {
                self.write(
                    "assistant_message_delta",
                    Some(&message),
                    &serde_json::json!({"text": part}),
                    false,
                );
            }
            self.line(
                "text_completed",
                Some(&message),
                &serde_json::json!({"text": part.repeat(8)}),
            );
            self.write(
                "tool_call_arguments_delta",
                Some(&message),
                &serde_json::json!({"index": 0, "name": "read", "text": "{\"path\": \"src/lib.rs\"}"}),
                false,
            );
            self.line(
                "tool_call_requested",
                Some(&call),
                &serde_json::json!({"name": "read", "arguments": {"path": "src/lib.rs"}}),
            );
            self.line(
                "assistant_message_completed",
                Some(&message),
                &serde_json::json!({"outcome": "completed"}),
            );
            self.line("tool_call_started", Some(&call), &started);
            self.line(
                "tool_call_completed",
                Some(&call),
                &serde_json::json!({"status": "completed", "content": [{"type": "text", "text": "ok"}]}),
            );
        }
    }
}

/// Words, `len` characters of them.
fn words(len: usize, seed: usize) -> String {
    let source = "the session log holds every durable line and the terminal pages it by seq ";
    source.chars().cycle().skip(seed).take(len).collect()
}

/// One long row of text, about 70 characters. The row is fixed so building
/// tens of thousands of rows stays fast: varying it by seed would rescan
/// the word source once per row.
fn long_row() -> String {
    "the session log holds every durable line and the terminal pages it; ".to_owned()
}

/// Text holding `count` rows: one durable line that draws as `count`
/// paragraphs. Double newlines in the payload render as separate rows,
/// so a single message or tool output can fill thousands of rows while
/// staying one line in the log. Single newlines would fold as soft breaks.
fn long_text(count: usize) -> String {
    (0..count)
        .map(|_| long_row())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Rows in one long message or tool output of the ticket shape.
pub(crate) const TICKET_LONG_ROWS: usize = 20_000;

/// Steps in a large ticket log (thousands of durable entries).
#[cfg(test)]
pub(crate) const TICKET_LARGE_STEPS: usize = 400;

const TICKET_TURNS: usize = 10;
const TICKET_LARGE_STEPS_PER_TURN: usize = 40;
#[cfg(test)]
const TICKET_SMALL_STEPS_PER_TURN: usize = 10;

/// One ticket session's log: `steps_per_turn` steps with text and tool calls
/// in each turn, plus one long message and one long tool output when
/// `with_long` holds. The first line forks from `parent` when one is given.
fn ticket_log(
    session: &str,
    steps_per_turn: usize,
    parent: Option<(&str, u64)>,
    with_long: bool,
    step_base: usize,
) -> (String, u64) {
    let mut log = Log::new(session);
    let empty = serde_json::json!({});
    let started = serde_json::json!({"effects": [], "reversible": false});
    let mut payload = serde_json::json!({"workspace": "~/work/fiber", "variables": {"path": "/usr/bin:/bin", "names": [], "source": "inherited"}});
    if let Some((parent_id, at)) = parent {
        let _ = payload.as_object_mut().map(|object| {
            object.insert(
                "forked_from".to_owned(),
                serde_json::json!({"session_id": parent_id, "seq": at}),
            )
        });
    }
    log.line("session_started", None, &payload);
    log.line(
        "fiber_started",
        None,
        &serde_json::json!({"version": "0.0.0", "resumed": false}),
    );
    let mut step = step_base;
    for turn in 0..TICKET_TURNS {
        let prompt = words(200, turn);
        log.line(
            "turn_started",
            None,
            &serde_json::json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": prompt}]}]}),
        );
        for _ in 0..steps_per_turn {
            step += 1;
            let message = format!("t_m{step}");
            log.line("step_started", None, &empty);
            log.line("assistant_message_started", Some(&message), &empty);
            let text = if with_long && step == step_base + 5 {
                long_text(TICKET_LONG_ROWS)
            } else {
                words(193, step)
            };
            log.line(
                "text_completed",
                Some(&message),
                &serde_json::json!({"text": text}),
            );
            let call = format!("t_t{step}");
            log.line(
                "tool_call_requested",
                Some(&call),
                &serde_json::json!({"name": "shell", "arguments": {"command": "rg -n paging crates"}}),
            );
            log.line(
                "assistant_message_completed",
                Some(&message),
                &serde_json::json!({"outcome": "completed"}),
            );
            log.line(
                "usage_recorded",
                Some(&message),
                &serde_json::json!({"generation_id": "fiber-paging-0001", "model": "test/paging-model", "tokens": {"input": 1200, "cache_read": 0, "cache_write": {}, "output": 300}, "input_bytes": 4096, "cost": null}),
            );
            log.line("tool_call_started", Some(&call), &started);
            let output = if with_long && step == step_base + 6 {
                long_text(TICKET_LONG_ROWS)
            } else {
                words(3000, step)
            };
            log.line(
                "tool_call_completed",
                Some(&call),
                &serde_json::json!({"status": "completed", "content": [{"type": "text", "text": output}]}),
            );
        }
        log.line(
            "turn_completed",
            None,
            &serde_json::json!({"outcome": "completed"}),
        );
    }
    let count = log.seq;
    (log.out, count)
}

/// The ticket's single large session: thousands of entries with one long
/// message and one long tool output of tens of thousands of rows.
pub fn ticket_session(scale: usize) -> String {
    let steps_per_turn = TICKET_LARGE_STEPS_PER_TURN.saturating_mul(scale);
    let (log, _) = ticket_log("s_dddddddddddddddd", steps_per_turn, None, true, 0);
    log
}

/// The ticket's history chain, root first: four sessions with three
/// `forked_from` links. The root and the child hold long messages; the
/// middle links stay small. Each child's fork point is its parent's last
/// line, so every ancestor scan covers almost the whole ancestor log.
#[cfg(test)]
pub(crate) fn ticket_chain() -> Vec<(String, String)> {
    let (root, root_count) = ticket_log(
        "s_aaaaaaaaaaaaaaaa",
        TICKET_LARGE_STEPS_PER_TURN,
        None,
        true,
        0,
    );
    let (second, second_count) = ticket_log(
        "s_bbbbbbbbbbbbbbbb",
        TICKET_SMALL_STEPS_PER_TURN,
        Some(("s_aaaaaaaaaaaaaaaa", root_count.saturating_sub(1))),
        false,
        100_000,
    );
    let (third, third_count) = ticket_log(
        "s_cccccccccccccccc",
        TICKET_SMALL_STEPS_PER_TURN,
        Some(("s_bbbbbbbbbbbbbbbb", second_count.saturating_sub(1))),
        false,
        200_000,
    );
    let (child, _) = ticket_log(
        "s_dddddddddddddddd",
        TICKET_LARGE_STEPS_PER_TURN,
        Some(("s_cccccccccccccccc", third_count.saturating_sub(1))),
        true,
        300_000,
    );
    vec![
        ("s_aaaaaaaaaaaaaaaa".to_owned(), root),
        ("s_bbbbbbbbbbbbbbbb".to_owned(), second),
        ("s_cccccccccccccccc".to_owned(), third),
        ("s_dddddddddddddddd".to_owned(), child),
    ]
}

/// The heavy session, repeated `scale` times, then a turn still running.
pub fn paging_session(scale: usize) -> String {
    let mut log = Log::new("s_paging0000000000");
    let empty = serde_json::json!({});
    let started = serde_json::json!({"effects": [], "reversible": false});
    log.line(
        "session_started",
        None,
        &serde_json::json!({"workspace": "~/work/fiber", "variables": {"path": "/usr/bin:/bin", "names": [], "source": "inherited"}}),
    );
    log.line(
        "fiber_started",
        None,
        &serde_json::json!({"version": "0.0.0", "resumed": false}),
    );
    let mut step = 0usize;
    for turn in 0..TURNS.saturating_mul(scale) {
        let prompt = words(200, turn);
        log.line(
            "turn_started",
            None,
            &serde_json::json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": prompt}]}]}),
        );
        let steps = STEPS.get(turn % 2).copied().unwrap_or(96);
        for at in 0..steps {
            step += 1;
            let message = format!("a_m{step}");
            log.line("step_started", None, &empty);
            log.line("assistant_message_started", Some(&message), &empty);
            if step.is_multiple_of(26) {
                log.line("reasoning_started", Some(&message), &empty);
                log.line(
                    "reasoning_completed",
                    Some(&message),
                    &serde_json::json!({"text": words(250, step)}),
                );
            }
            let text = words(TEXT.get(step % TEXT.len()).copied().unwrap_or(193), step);
            log.line(
                "text_completed",
                Some(&message),
                &serde_json::json!({"text": text}),
            );
            // The turn's last step answers; the others call one tool, and
            // one step in ten calls two.
            let calls = if at + 1 == steps {
                0
            } else if step % 10 == 3 {
                2
            } else {
                1
            };
            let ids: Vec<String> = (0..calls).map(|call| format!("a_t{step}_{call}")).collect();
            for id in &ids {
                log.line(
                    "tool_call_requested",
                    Some(id),
                    &serde_json::json!({"name": "shell", "arguments": {"command": "rg -n paging crates"}}),
                );
            }
            log.line(
                "assistant_message_completed",
                Some(&message),
                &serde_json::json!({"outcome": "completed"}),
            );
            log.line(
                "usage_recorded",
                Some(&message),
                &serde_json::json!({"generation_id": "fiber-paging-0001", "model": "test/paging-model", "tokens": {"input": 1200, "cache_read": 0, "cache_write": {}, "output": 300}, "input_bytes": 4096, "cost": null}),
            );
            for (call, id) in ids.iter().enumerate() {
                log.line("tool_call_started", Some(id), &started);
                let output = words(
                    OUTPUT
                        .get((step + call) % OUTPUT.len())
                        .copied()
                        .unwrap_or(3000),
                    step,
                );
                log.line(
                    "tool_call_completed",
                    Some(id),
                    &serde_json::json!({"status": "completed", "content": [{"type": "text", "text": output}]}),
                );
            }
        }
        log.line(
            "turn_completed",
            None,
            &serde_json::json!({"outcome": "completed"}),
        );
    }
    log.running_turn();
    log.out
}

#[cfg(test)]
mod ticket_log_tests {
    use super::ticket_log;
    use serde_json::Value;

    /// `docs/events.md`, "The envelope": durable sequence values are
    /// contiguous per session, never reset or reused.
    #[test]
    fn small_ticket_log_sequences_advance_consecutively() {
        let (events, _) = ticket_log("s_aaaaaaaaaaaaaaaa", 2, None, false, 0);
        let sequences: Vec<u64> = events
            .lines()
            .map(|line| {
                let value: Value = serde_json::from_str(line).expect("ticket line parses");
                value["seq"].as_u64().expect("ticket line has a sequence")
            })
            .collect();
        let expected: Vec<u64> = (0..sequences.len() as u64).collect();
        assert_eq!(
            sequences, expected,
            "durable sequence values are consecutive"
        );
    }

    #[test]
    fn small_ticket_log_writes_requested_steps_per_turn() {
        let (events, _) = ticket_log("s_aaaaaaaaaaaaaaaa", 2, None, false, 0);
        let steps = events
            .lines()
            .filter(|line| line.contains("\"step_started\""))
            .count();
        assert_eq!(steps, 20, "two steps in each of ten turns");
    }
}

#[cfg(test)]
#[path = "paging_session_tests.rs"]
mod tests;
