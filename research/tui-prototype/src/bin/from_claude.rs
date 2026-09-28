//! Converts one Claude Code session (main thread) into a `docs/events.md`
//! durable-line fixture the prototype can play, shaped like `gen_large.rs`'s.
//!
//!   cargo run --release --bin from_claude -- <session.jsonl> <out.jsonl> [--max-bytes N]
//!
//! A real user prompt starts a turn; the turn ends at the next prompt. Each
//! assistant message id is one model call. Context is input + cache_read +
//! cache_creation; past `HANDOFF_AT` the converter writes a handoff and shifts
//! the reported context to restart near `RESTART`. Output stops after the turn
//! in which it passes `--max-bytes` (default 12 MB).

use serde_json::{Value, json};
use std::collections::HashMap;

const HANDOFF_AT: u64 = 400_000;
const RESTART: u64 = 30_000;
const DEFAULT_MAX: usize = 12_000_000;
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
    last_ctx: u64,
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
    fn handoff(&mut self, ts: i64, tokens_before: u64, trigger: &str) {
        self.ev("handoff_started", ts, None, json!({ "trigger": trigger }));
        self.ev("handoff_completed", ts, None, json!({ "outcome": "completed", "tokens_before": tokens_before, "note": [] }));
        self.handoffs += 1;
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

/// Drops every `<system-reminder>…</system-reminder>` span.
fn strip_reminders(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("<system-reminder>") {
        out.push_str(&rest[..i]);
        rest = match rest[i..].find("</system-reminder>") {
            Some(j) => &rest[i + j + "</system-reminder>".len()..],
            None => "",
        };
    }
    out + rest
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
        o.handoff(m.ts, ctx, "auto");
        o.offset = (input + cr + cc).saturating_sub(RESTART);
    }
    let cr = cr - o.offset.min(cr);
    o.last_ctx = input + cr + cc;
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
    let max: usize = a.iter().position(|x| x == "--max-bytes").and_then(|i| a.get(i + 1)).and_then(|x| x.parse().ok()).unwrap_or(DEFAULT_MAX);
    let o = convert(&std::fs::read_to_string(&a[1]).expect("read session"), max);
    std::fs::write(&a[2], o.lines.join("\n") + "\n").expect("write out");
    eprintln!("{}: {} lines, {} bytes, {} handoffs", a[2], o.lines.len(), o.bytes, o.handoffs);
}

fn convert(src: &str, max: usize) -> Out {
    let mut o = Out { lines: vec![], bytes: 0, ts: 0, seq: 0, n: 0, turn: None, offset: 0, handoffs: 0, last_ctx: 0 };
    let mut cur: Option<Msg> = None;
    let mut calls = Calls::new();
    let mut started = false;
    let mut asked = false; // a `/compact` prompt is waiting for its compaction
    for l in src.lines() {
        let Ok(d) = serde_json::from_str::<Value>(l) else { continue };
        let ty = d["type"].as_str().unwrap_or("");
        if d["isSidechain"] == true || d["isMeta"] == true || d["isCompactSummary"] == true {
            continue;
        }
        // a Claude Code compaction is a Fiber handoff; the source context is small afterwards
        if ty == "system" && d["subtype"] == "compact_boundary" && started {
            flush(&mut o, &mut cur, &mut calls);
            let before = o.last_ctx;
            o.handoff(ms(&d["timestamp"]), before, if asked { "person" } else { "auto" });
            asked = false;
            o.offset = 0;
            continue;
        }
        if ty != "user" && ty != "assistant" {
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
        let clean = strip_reminders(&prompt);
        let pt = clean.trim();
        if pt.is_empty() || ["<task-notification", "<local-command", "<command-", "<system-reminder"].iter().any(|p| pt.starts_with(p)) {
            continue;
        }
        if pt == "/compact" || pt.starts_with("/compact ") {
            asked = true;
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
    o
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
    fn asst(id: &str, ts: &str, cr: u64) -> String {
        json!({"type":"assistant","timestamp":ts,"message":{"id":id,"content":[{"type":"text","text":"hi"}],
            "usage":{"input_tokens":1,"cache_read_input_tokens":cr,"cache_creation_input_tokens":0,"output_tokens":5}}}).to_string()
    }
    fn prompt(ts: &str) -> String {
        json!({"type":"user","timestamp":ts,"message":{"content":"go"}}).to_string()
    }
    fn ctxs(o: &Out) -> (Vec<u64>, Vec<u64>) {
        let (mut c, mut h) = (vec![], vec![]);
        for l in &o.lines {
            let v: Value = serde_json::from_str(l).unwrap();
            if v["kind"] == "usage_recorded" {
                let t = &v["payload"]["tokens"];
                c.push(t["input"].as_u64().unwrap() + t["cache_read"].as_u64().unwrap() + t["cache_write"]["1h"].as_u64().unwrap());
            }
            if v["kind"] == "handoff_completed" {
                h.push(v["payload"]["tokens_before"].as_u64().unwrap());
            }
        }
        (c, h)
    }
    #[test]
    fn compaction_becomes_a_handoff() {
        let src = [
            prompt("2026-01-01T00:00:00.000Z"),
            asst("m1", "2026-01-01T00:00:01.000Z", 300_000),
            json!({"type":"system","subtype":"compact_boundary","timestamp":"2026-01-01T00:00:02.000Z"}).to_string(),
            json!({"type":"user","isCompactSummary":true,"timestamp":"2026-01-01T00:00:01.900Z","message":{"content":"summary"}}).to_string(),
            asst("m2", "2026-01-01T00:00:03.000Z", 20_000),
        ]
        .join("\n");
        let o = convert(&src, usize::MAX);
        let (c, h) = ctxs(&o);
        assert_eq!((c, h), (vec![300_001, 20_001], vec![300_001]));
        assert_eq!(o.lines.iter().filter(|l| l.contains("\"turn_started\"")).count(), 1);
        assert!(!o.lines.iter().any(|l| l.contains("summary")));
    }
    #[test]
    fn overflow_without_compaction_restarts_near_30k() {
        let src = [
            prompt("2026-01-01T00:00:00.000Z"),
            asst("m1", "2026-01-01T00:00:01.000Z", 390_000),
            asst("m2", "2026-01-01T00:00:02.000Z", 410_000),
            asst("m3", "2026-01-01T00:00:03.000Z", 415_000),
        ]
        .join("\n");
        let (c, h) = ctxs(&convert(&src, usize::MAX));
        assert_eq!(h, vec![410_001]);
        assert_eq!(c, vec![390_001, RESTART, RESTART + 5_000]);
    }
    #[test]
    fn harness_text_is_not_a_prompt_and_compact_is_the_persons_handoff() {
        let u = |t: &str, ts: &str| json!({"type":"user","timestamp":ts,"message":{"content":t}}).to_string();
        let src = [
            u("real <system-reminder>secret</system-reminder>prompt", "2026-01-01T00:00:00.000Z"),
            asst("m1", "2026-01-01T00:00:01.000Z", 1_000),
            u("<task-notification>done</task-notification>", "2026-01-01T00:00:02.000Z"),
            asst("m2", "2026-01-01T00:00:03.000Z", 1_000),
            u("/compact keep going", "2026-01-01T00:00:04.000Z"),
            json!({"type":"system","subtype":"compact_boundary","timestamp":"2026-01-01T00:00:05.000Z"}).to_string(),
            asst("m3", "2026-01-01T00:00:06.000Z", 1_000),
            json!({"type":"system","subtype":"compact_boundary","timestamp":"2026-01-01T00:00:07.000Z"}).to_string(),
        ]
        .join("\n");
        let o = convert(&src, usize::MAX);
        let all = o.lines.join("\n");
        assert_eq!(all.matches("\"turn_started\"").count(), 1);
        assert!(all.contains("real prompt") && !all.contains("secret") && !all.contains("keep going"));
        let t: Vec<String> = o.lines.iter().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["kind"] == "handoff_started").map(|v| v["payload"]["trigger"].as_str().unwrap().to_string()).collect();
        assert_eq!(t, ["person", "auto"]);
    }
}
