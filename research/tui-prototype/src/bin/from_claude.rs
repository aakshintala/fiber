//! Converts one Claude Code session (main thread) into a `docs/events.md`
//! durable-line fixture the prototype can play, shaped like `gen_large.rs`'s.
//!
//!   cargo run --release --bin from_claude -- <session.jsonl> <out.jsonl> [--max-bytes N]
//!
//! A real user prompt starts a turn; the turn ends at the next prompt. Each
//! assistant message id is one model call. Context is input + cache_read +
//! cache_creation; past `HANDOFF_AT` the converter writes a handoff and shifts
//! the reported context to restart near `RESTART`. Output stops after the turn
//! in which it passes `--max-bytes` (default 5 MB).

use serde_json::{Value, json};
use std::collections::HashMap;

const HANDOFF_AT: u64 = 400_000;
const RESTART: u64 = 30_000;
const CLIP_LINES: usize = 200;
const SID: &str = "s_real01";
const MODEL: &str = "anthropic/claude-opus-5-5";

struct Out {
    lines: Vec<String>,
    bytes: usize,
    ts: i64,
    seq: u64,
    n: u32,
    turn: Option<String>,
    offset: u64,
    handoffs: u32,
}

/// One assistant message (a model call), assembled from its per-block lines.
#[derive(Default)]
struct Msg {
    id: String,
    ts: i64,
    usage: Value,
    reasoning: bool,
    text: String,
    tools: Vec<(String, String, Value)>, // provider id, Claude Code name, input
}

/// provider id -> (action id, effects, paths, changes)
type Calls = HashMap<String, (String, &'static str, Value, Option<Value>)>;

impl Out {
    fn ev(&mut self, kind: &str, ts: i64, aid: Option<&str>, payload: Value) {
        self.ts = self.ts.max(ts);
        self.seq += 1;
        let mut e = serde_json::Map::new();
        e.insert("kind".into(), json!(kind));
        e.insert("session_id".into(), json!(SID));
        e.insert("ts".into(), json!(self.ts));
        e.insert("schema_version".into(), json!(1));
        if let Some(t) = &self.turn {
            e.insert("turn_id".into(), json!(t));
        }
        if let Some(a) = aid {
            e.insert("action_id".into(), json!(a));
        }
        e.insert("seq".into(), json!(self.seq));
        e.insert("payload".into(), payload);
        let s = Value::Object(e).to_string();
        self.bytes += s.len() + 1;
        self.lines.push(s);
    }
    fn id(&mut self, p: &str) -> String {
        self.n += 1;
        format!("{p}_{:05x}", self.n)
    }
    fn end_turn(&mut self, ts: i64) {
        if self.turn.is_some() {
            self.ev("turn_completed", ts, None, json!({ "outcome": "completed" }));
            self.turn = None;
        }
    }
}

/// "2026-09-28T05:19:13.465Z" to epoch ms (civil-from-days, stdlib only).
fn ms(v: &Value) -> i64 {
    let s = v.as_str().unwrap_or("");
    let n = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    let (y, m, d) = (n(0, 4) - (n(5, 7) <= 2) as i64, n(5, 7), n(8, 10));
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    ((days * 24 + n(11, 13)) * 60 + n(14, 16)) * 60_000 + n(17, 19) * 1000 + n(20, 23)
}

fn lines(s: &str) -> usize {
    s.lines().count()
}

fn clip(s: &str) -> String {
    let n = lines(s);
    if n <= CLIP_LINES {
        return s.to_string();
    }
    let head: Vec<&str> = s.lines().take(CLIP_LINES).collect();
    format!("{}\n… {} more lines", head.join("\n"), n - CLIP_LINES)
}

fn result_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Claude Code tool to (Fiber name, effects, arguments, changes, paths).
fn map_tool(name: &str, input: &Value) -> (String, &'static str, Value, Option<Value>, Vec<String>) {
    let path = input["file_path"].as_str().or(input["path"].as_str()).map(str::to_string);
    let mut args = input.clone();
    if let (Some(p), Some(o)) = (&path, args.as_object_mut()) {
        o.remove("file_path");
        o.insert("path".into(), json!(p));
    }
    let paths: Vec<String> = path.iter().cloned().collect();
    let ch = |added: usize, removed: usize| Some(json!([{ "path": path.clone().unwrap_or_default(), "added": added, "removed": removed }]));
    let s = |k: &str| lines(input[k].as_str().unwrap_or(""));
    match name {
        "Read" => ("read".into(), "reads", args, None, paths),
        "Edit" => ("edit".into(), "writes", args, ch(s("new_string"), s("old_string")), paths),
        "MultiEdit" => {
            let es = input["edits"].as_array().cloned().unwrap_or_default();
            let n = |k: &str| es.iter().map(|e| lines(e[k].as_str().unwrap_or(""))).sum();
            ("edit".into(), "writes", args, ch(n("new_string"), n("old_string")), paths)
        }
        "Write" => ("edit".into(), "writes", args, ch(s("content"), 0), paths),
        "Bash" => ("shell".into(), "executes", args, None, vec![]),
        "Grep" | "Glob" => ("search".into(), "reads", args, None, vec![]),
        "Agent" | "Task" => ("delegate".into(), "executes", args, None, vec![]),
        n => (n.to_lowercase(), "reads", args, None, vec![]),
    }
}

fn flush(o: &mut Out, m: &mut Option<Msg>, calls: &mut Calls) {
    let Some(m) = m.take() else { return };
    let g = |k: &str| m.usage[k].as_u64().unwrap_or(0);
    let (input, cr, cc, out) = (g("input_tokens"), g("cache_read_input_tokens"), g("cache_creation_input_tokens"), g("output_tokens"));
    let ctx = input + cr + cc - o.offset.min(cr);
    if ctx > HANDOFF_AT {
        o.ev("handoff_started", m.ts, None, json!({ "trigger": "auto" }));
        o.ev("handoff_completed", m.ts, None, json!({ "outcome": "completed", "tokens_before": ctx, "note": [] }));
        o.handoffs += 1;
        o.offset = (input + cr + cc).saturating_sub(RESTART);
    }
    let cr = cr - o.offset.min(cr);
    let msg = o.id("a");
    o.ev("assistant_message_started", m.ts, Some(&msg), json!({}));
    if m.reasoning {
        let ra = o.id("a");
        o.ev("reasoning_started", m.ts, Some(&ra), json!({}));
        o.ev("reasoning_completed", m.ts, Some(&ra), json!({ "text": "(thinking is not stored in the source session)", "provider_item": { "type": "reasoning" } }));
    }
    for (pid, name, input) in &m.tools {
        let a = o.id("a");
        let (fname, eff, args, changes, paths) = map_tool(name, input);
        o.ev("tool_call_requested", m.ts, Some(&a), json!({ "name": fname, "arguments": args, "provider_id": pid }));
        calls.insert(pid.clone(), (a, eff, json!(paths), changes));
    }
    o.ev("assistant_message_completed", m.ts, Some(&msg), json!({ "text": m.text, "outcome": "completed" }));
    o.ev("usage_recorded", m.ts, None, json!({
        "generation_id": format!("gen_{:04}", o.n),
        "model": MODEL,
        "tokens": { "input": input, "cache_read": cr, "cache_write": { "1h": cc }, "output": out },
        "cost": 0.0,
        "action_id": msg,
    }));
}

fn header(o: &mut Out, t0: i64) {
    o.ts = t0;
    o.ev("fiber_started", t0, None, json!({ "version": "0.0.1", "schema_version": 1, "resumed": false }));
    o.ev("session_started", t0, None, json!({ "created_at": t0, "workspace": "~/work/fiber" }));
    o.ev("opening_message", t0, None, json!({
        "environment": { "date": "2026-09-28", "platform": "macos", "shell": "zsh", "workspace": "~/work/fiber",
            "git": { "branch": "main", "dirty": true }, "session_log": format!("~/.fiber/projects/fiber/sessions/{SID}/events.jsonl") },
        "instruction_files": [{ "path": "AGENTS.md", "content": "…" }], "skills": [] }));
    let tools: Vec<Value> = ["read", "write", "edit", "shell", "search", "delegate", "ask_user", "web_fetch"].iter().map(|n| json!({ "name": n, "deferred": false })).collect();
    o.ev("preamble_built", t0, None, json!({ "reason": "start", "model": MODEL, "effort": "high", "thinking": "adaptive",
        "tool_choice": "auto", "cache_lifetime": "1h", "system_prompt": "…", "tools": tools }));
    o.ev("mode_changed", t0, None, json!({ "before": "ask", "after": "auto", "by": "mode" }));
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: from_claude <session.jsonl> <out.jsonl> [--max-bytes N]");
        std::process::exit(2);
    }
    let max: usize = a.iter().position(|x| x == "--max-bytes").and_then(|i| a.get(i + 1)).and_then(|x| x.parse().ok()).unwrap_or(5_000_000);
    let src = std::fs::read_to_string(&a[1]).expect("read session");
    let mut o = Out { lines: vec![], bytes: 0, ts: 0, seq: 0, n: 0, turn: None, offset: 0, handoffs: 0 };
    let mut cur: Option<Msg> = None;
    let mut calls = Calls::new();
    let mut started = false;

    for l in src.lines() {
        let Ok(d) = serde_json::from_str::<Value>(l) else { continue };
        let ty = d["type"].as_str().unwrap_or("");
        if (ty != "user" && ty != "assistant") || d["isSidechain"] == true || d["isMeta"] == true {
            continue;
        }
        let ts = ms(&d["timestamp"]);
        let content = &d["message"]["content"];
        if !started {
            started = true;
            header(&mut o, ts - 100);
        }
        if ty == "assistant" {
            let id = d["message"]["id"].as_str().unwrap_or("").to_string();
            if cur.as_ref().is_some_and(|m| m.id != id) {
                flush(&mut o, &mut cur, &mut calls);
            }
            let m = cur.get_or_insert_with(|| Msg { id, ..Default::default() });
            m.ts = ts;
            m.usage = d["message"]["usage"].clone();
            for b in content.as_array().into_iter().flatten() {
                match b["type"].as_str() {
                    Some("thinking") => m.reasoning = true,
                    Some("text") => m.text.push_str(b["text"].as_str().unwrap_or("")),
                    Some("tool_use") => m.tools.push((b["id"].as_str().unwrap_or("").into(), b["name"].as_str().unwrap_or("").into(), b["input"].clone())),
                    _ => {}
                }
            }
            continue;
        }
        flush(&mut o, &mut cur, &mut calls);
        let mut prompt = String::new();
        match content {
            Value::String(s) => prompt = s.clone(),
            Value::Array(bs) => {
                for b in bs {
                    match b["type"].as_str() {
                        Some("tool_result") => {
                            let Some((aid, eff, paths, changes)) = calls.remove(b["tool_use_id"].as_str().unwrap_or("")) else { continue };
                            o.ev("tool_call_started", ts, Some(&aid), json!({ "effects": [eff], "reversible": eff == "reads", "paths": paths }));
                            let err = b["is_error"] == true;
                            let mut p = serde_json::Map::new();
                            p.insert("status".into(), json!(if err { "failed" } else { "completed" }));
                            p.insert("content".into(), json!([{ "type": "text", "text": clip(&result_text(&b["content"])) }]));
                            if eff == "executes" {
                                p.insert("process".into(), json!({ "exit_code": err as i32, "timed_out": false }));
                            }
                            if let Some(c) = changes.filter(|_| !err) {
                                p.insert("changes".into(), c);
                            }
                            o.ev("tool_call_completed", ts, Some(&aid), Value::Object(p));
                        }
                        Some("text") => prompt.push_str(b["text"].as_str().unwrap_or("")),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        let pt = prompt.trim();
        if pt.is_empty() || pt.starts_with("<local-command") || pt.starts_with("<command-") || pt.starts_with("<system-reminder") {
            continue;
        }
        o.end_turn(ts);
        if o.bytes > max {
            break;
        }
        let t = o.id("t");
        o.turn = Some(t);
        o.ev("turn_started", ts, None, json!({ "input": [{ "type": "text", "text": pt }] }));
    }
    flush(&mut o, &mut cur, &mut calls);
    let end = o.ts;
    o.end_turn(end);
    std::fs::write(&a[2], o.lines.join("\n") + "\n").expect("write out");
    eprintln!("{}: {} lines, {} bytes, {} handoffs", a[2], o.lines.len(), o.bytes, o.handoffs);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamp() {
        assert_eq!(ms(&json!("1970-01-01T00:00:01.002Z")), 1002);
        assert_eq!(ms(&json!("2000-03-01T00:00:00.000Z")), 951_868_800_000);
    }
    #[test]
    fn clips() {
        assert_eq!(lines(&clip(&"x\n".repeat(300))), CLIP_LINES + 1);
    }
}
