//! Answering-session discovery cost (`docs/performance.md`): two idle
//! sessions and the prompting session, with no artificial deadline wait.

use crate::busy::{
    self, READY, TURN, call_reply, in_home, read_log, session, text_reply, until_stdout,
};
use crate::idle::{Ctx, Samples, Workload};
use crate::run::Session;
use serde_json::{Value, json};

pub(crate) const WORKLOADS: [Workload; 1] = [Workload {
    name: "session_list",
    timing: true,
    run: measure,
}];

fn measure(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let prompt = "list the sessions";
    let script = vec![
        call_reply("call_list", "session_list", "{}", prompt.len()),
        text_reply("listed", prompt.len()),
    ];
    in_home(ctx.home.child(script)?, |home| {
        let mut idle = Vec::new();
        for _ in 0..2 {
            let id = doors::mint("s_");
            let running = Session::spawn(&mut session(ctx, home, &id, &[]), ctx.clock)?;
            until_stdout(ctx, &running, "session_status", READY)?;
            idle.push(running);
        }
        let id = doors::mint("s_");
        let running = Session::spawn(
            &mut session(ctx, home, &id, &["--prompt", prompt]),
            ctx.clock,
        )?;
        let measured = until_stdout(ctx, &running, "turn_completed", TURN).and_then(|()| {
            let lines = read_log(home, &id)?;
            let entries = listing(&lines)?;
            check(&entries, notes);
            span(&lines)
        });
        let stopped = running.proc.stop(ctx.clock);
        // Home owns cleanup even on an error; explicitly stop every idle
        // session before returning the measured result.
        let idle_stopped: Vec<_> = idle.into_iter().map(|s| s.proc.stop(ctx.clock)).collect();
        let ms = measured?;
        stopped?;
        for result in idle_stopped {
            result?;
        }
        Ok(vec![("session_list_ms", json!(ms))])
    })
}

fn action(lines: &[(usize, Value)]) -> Result<&Value, String> {
    lines
        .iter()
        .map(|(_, line)| line)
        .find(|line| {
            line.get("kind") == Some(&json!("tool_call_requested"))
                && line.pointer("/payload/name") == Some(&json!("session_list"))
        })
        .and_then(|line| line.get("action_id"))
        .ok_or_else(|| "session_list: missing tool_call_requested".into())
}

fn event<'a>(lines: &'a [(usize, Value)], action: &Value, kind: &str) -> Result<&'a Value, String> {
    lines
        .iter()
        .map(|(_, line)| line)
        .find(|line| {
            line.get("kind").and_then(Value::as_str) == Some(kind)
                && line.get("action_id") == Some(action)
        })
        .ok_or_else(|| format!("session_list: missing {kind}"))
}

fn span(lines: &[(usize, Value)]) -> Result<u64, String> {
    let action = action(lines)?;
    let start = event(lines, action, "tool_call_started")?
        .get("ts")
        .and_then(Value::as_u64)
        .ok_or("session_list: missing tool_call_started.ts")?;
    let end = event(lines, action, "tool_call_completed")?
        .get("ts")
        .and_then(Value::as_u64)
        .ok_or("session_list: missing tool_call_completed.ts")?;
    Ok(end.saturating_sub(start))
}

fn listing(lines: &[(usize, Value)]) -> Result<Vec<Value>, String> {
    let completed = event(lines, action(lines)?, "tool_call_completed")?;
    let text = completed
        .pointer("/payload/content/0/text")
        .and_then(Value::as_str)
        .ok_or("session_list: missing result text")?;
    serde_json::from_str(text).map_err(|e| format!("session_list: invalid result: {e}"))
}

fn check(entries: &[Value], notes: &mut Vec<String>) {
    busy::expect(notes, "session_list entries", entries.len(), 3);
    busy::expect(
        notes,
        "session_list self markers",
        entries
            .iter()
            .filter(|e| e.get("self").and_then(Value::as_bool) == Some(true))
            .count(),
        1,
    );
    busy::expect(
        notes,
        "session_list answering entries",
        entries
            .iter()
            .filter(|e| e.get("answering").and_then(Value::as_bool) == Some(true))
            .count(),
        3,
    );
}

#[cfg(test)]
#[path = "session_list_tests.rs"]
mod tests;
