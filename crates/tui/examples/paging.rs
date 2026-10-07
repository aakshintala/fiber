//! The `paging` jig (`docs/testing.md`, "Jigs"): generates a large session
//! and measures the terminal paging it (`docs/tui.md`, "History and
//! paging").
//!
//! `cargo run --release -p tui --example paging -- [scale] [width] [height]`
//!
//! The session is shaped like the prototype's heavy fixture
//! (`research/tui-prototype/LARGE.md`): 10 turns, 965 steps with text,
//! about 1,047 tool calls with long results, in Fiber's own event kinds.
//! `scale` repeats it, 1 by default. A turn still running follows it, which
//! the jig appends one line a frame, deltas included. The screen is 160x48
//! by default. Peak memory is the process's, read from outside, such as
//! with `/usr/bin/time -l`.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};

const USAGE: &str =
    "usage: cargo run --release -p tui --example paging -- [scale] [width] [height]";

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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |at: usize, default: &str| args.get(at).map_or(default, String::as_str).to_owned();
    let (Ok(scale), Ok(width), Ok(height)) = (
        arg(0, "1").parse::<usize>(),
        arg(1, "160").parse::<u16>(),
        arg(2, "48").parse::<u16>(),
    ) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let events = session(scale);
    println!(
        "session: scale {scale}, {:.2} MiB, at {width}x{height}",
        mib(events.len())
    );
    match tui::measure_paging(&events, width, height, Arc::new(ProcessClock)) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("paging: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Bytes as MiB, for the report.
fn mib(bytes: usize) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a size in a report, read to two places"
    )]
    let bytes = bytes as f64;
    bytes / 1024.0 / 1024.0
}

/// Writes a session's durable lines, numbered from 0.
struct Log {
    out: String,
    seq: u64,
}

impl Log {
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
            "session_id": "s_paging0000000000",
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
            self.line("tool_call_started", Some(&call), &empty);
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
    let mut text = String::with_capacity(len);
    let mut at = seed % source.len();
    while text.len() < len {
        let rest = source.get(at..).unwrap_or_default();
        text.push_str(
            rest.get(..len.saturating_sub(text.len()).min(rest.len()))
                .unwrap_or(rest),
        );
        at = 0;
    }
    text
}

/// The heavy session, repeated `scale` times, then a turn still running.
fn session(scale: usize) -> String {
    let mut log = Log {
        out: String::new(),
        seq: 0,
    };
    let empty = serde_json::json!({});
    log.line(
        "fiber_started",
        None,
        &serde_json::json!({"version": "0.0.0", "resumed": false}),
    );
    log.line(
        "session_started",
        None,
        &serde_json::json!({"workspace": "~/work/fiber"}),
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
                &serde_json::json!({"input_tokens": 1200, "output_tokens": 300}),
            );
            for (call, id) in ids.iter().enumerate() {
                log.line("tool_call_started", Some(id), &empty);
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

/// The process clock. `tui` cannot depend on `main`, which owns the one
/// the binary uses, so the jig keeps a small one.
struct ProcessClock;

impl Clock for ProcessClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the paging jig times frames on the process clock; tui cannot depend on main"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the paging jig reads the process clock; tui cannot depend on main"
    )]
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep(&self, _duration: Duration) {}

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        wait(until.map(|until| until.saturating_duration_since(self.now())));
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}
