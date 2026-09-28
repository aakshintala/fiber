//! Writes the fixture sessions: `fixtures/session.jsonl` (ends mid-turn) and
//! `fixtures/idle.jsonl` (the same session cut after its last finished turn).
//! Every line is a `docs/events.md` line. Run with `cargo run --bin gen`.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Write as _;

const MAIN: &str = "s_9c41e2";
const DELEGATE: &str = "s_d417a0";
const EPHEMERAL: &[&str] = &[
    "assistant_message_delta",
    "reasoning_delta",
    "tool_call_delta",
    "job_delta",
    "notice",
    "retry_scheduled",
    "steering_queue",
    "extension_ui",
];

struct G {
    out: Vec<Value>,
    ts: i64,
    seq: HashMap<String, u64>,
    turn: HashMap<String, String>,
    n: u32,
    ctx: u64,
    gen_n: u32,
}

enum R {
    Ok { lines: usize, head: String },
    Changed { path: String, added: u32, removed: u32 },
    Proc { lines: usize, exit: i32, dur: i64 },
    Fail { code: &'static str, msg: String, exit: Option<i32> },
    Running,
}

struct C {
    name: &'static str,
    args: Value,
    r: R,
}

fn read(path: &str, lines: usize) -> C {
    C { name: "read", args: json!({ "path": path }), r: R::Ok { lines, head: format!("1\t// {path}") } }
}
fn grep(pat: &str, dir: &str, lines: usize) -> C {
    C {
        name: "shell",
        args: json!({ "command": format!("grep -rn '{pat}' {dir}") }),
        r: R::Proc { lines, exit: 0, dur: 300 },
    }
}
fn find(dir: &str) -> C {
    C { name: "shell", args: json!({ "command": format!("find {dir} -name '*.rs'") }), r: R::Proc { lines: 9, exit: 0, dur: 200 } }
}
fn edit(path: &str, added: u32, removed: u32) -> C {
    C {
        name: "edit",
        args: json!({ "path": path, "edits": [{ "old_text": "    thread::sleep(Duration::from_millis(200));", "new_text": "    wait_for_path(&ready, Duration::from_secs(5))?;" }] }),
        r: R::Changed { path: path.into(), added, removed },
    }
}
fn cargo(cmd: &str, exit: i32, lines: usize, dur: i64) -> C {
    let r = if exit == 0 {
        R::Proc { lines, exit, dur }
    } else {
        R::Fail { code: "nonzero_exit", msg: format!("exit {exit}"), exit: Some(exit) }
    };
    C { name: "shell", args: json!({ "command": cmd }), r }
}

impl G {
    fn id(&mut self, p: &str) -> String {
        self.n += 1;
        format!("{p}_{:04x}", self.n)
    }
    fn emit(&mut self, sid: &str, kind: &str, dt: i64, aid: Option<&str>, payload: Value) {
        self.ts += dt;
        let mut e = serde_json::Map::new();
        e.insert("kind".into(), json!(kind));
        e.insert("session_id".into(), json!(sid));
        e.insert("ts".into(), json!(self.ts));
        e.insert("schema_version".into(), json!(1));
        if let Some(t) = self.turn.get(sid) {
            e.insert("turn_id".into(), json!(t));
        }
        if let Some(a) = aid {
            e.insert("action_id".into(), json!(a));
        }
        if !EPHEMERAL.contains(&kind) {
            let s = self.seq.entry(sid.into()).or_default();
            *s += 1;
            e.insert("seq".into(), json!(*s));
        }
        e.insert("payload".into(), payload);
        self.out.push(Value::Object(e));
    }
    fn m(&mut self, kind: &str, dt: i64, aid: Option<&str>, payload: Value) {
        self.emit(MAIN, kind, dt, aid, payload)
    }

    fn turn(&mut self, text: &str) {
        let t = self.id("t");
        self.turn.insert(MAIN.into(), t);
        self.m("turn_started", 1500, None, json!({ "input": [{ "type": "text", "text": text }] }));
    }
    fn end_turn(&mut self, outcome: &str) {
        self.m("turn_completed", 400, None, json!({ "outcome": outcome }));
        self.turn.remove(MAIN);
    }
    /// Streams in chunks of a few words, as deltas do.
    fn chunks(text: &str) -> Vec<String> {
        let mut out = vec![];
        let mut cur = String::new();
        for (k, w) in text.split_inclusive(' ').enumerate() {
            cur.push_str(w);
            if k % 4 == 3 {
                out.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }
    fn think(&mut self, text: &str, secs: i64, finish: bool) {
        let a = self.id("a");
        self.m("reasoning_started", 800, Some(&a), json!({}));
        let ch = Self::chunks(text);
        let dt = secs * 1000 / ch.len() as i64;
        for c in ch {
            self.m("reasoning_delta", dt, Some(&a), json!({ "text": c }));
        }
        if finish {
            self.m("reasoning_completed", 100, Some(&a), json!({ "text": text, "provider_item": { "type": "reasoning" } }));
        }
    }
    /// A model call whose reply is text. Every model call opens with
    /// `assistant_message_started`, fsynced before the request is sent.
    fn say(&mut self, reasoning: Option<(&str, i64)>, text: &str) {
        let a = self.id("a");
        self.m("assistant_message_started", 900, Some(&a), json!({}));
        if let Some((t, s)) = reasoning {
            self.think(t, s, true);
        }
        for c in Self::chunks(text) {
            self.m("assistant_message_delta", 90, Some(&a), json!({ "text": c }));
        }
        self.m("assistant_message_completed", 60, Some(&a), json!({ "text": text, "outcome": "completed" }));
        self.usage(Some(&a), text.len() as u64 / 4);
    }
    fn usage(&mut self, aid: Option<&str>, out: u64) {
        self.gen_n += 1;
        self.ctx += 3200;
        let p = json!({
            "generation_id": format!("gen_{:03}", self.gen_n),
            "model": "anthropic/claude-opus-5-5",
            "tokens": { "input": 1400, "cache_read": self.ctx, "cache_write": { "1h": 2600 }, "output": out },
            "cost": ((0.02 + self.ctx as f64 * 3e-7) * 1e4).round() / 1e4,
            "action_id": aid,
        });
        self.m("usage_recorded", 50, None, p);
    }
    /// One model round trip: optional reasoning, the calls it requested, its
    /// usage, then the calls running one after another.
    fn step(&mut self, reasoning: Option<(&str, i64)>, calls: Vec<C>) -> Vec<String> {
        let msg = self.id("a");
        self.m("assistant_message_started", 900, Some(&msg), json!({}));
        if let Some((t, s)) = reasoning {
            self.think(t, s, true);
        }
        let ids: Vec<String> = calls.iter().map(|_| self.id("a")).collect();
        for (c, a) in calls.iter().zip(&ids) {
            let pid = format!("toolu_{a}");
            self.m("tool_call_requested", 700, Some(a), json!({ "name": c.name, "arguments": c.args, "provider_id": pid }));
        }
        // a reply with only tool calls: the message completes with no text
        self.m("assistant_message_completed", 60, Some(&msg), json!({ "text": "", "outcome": "completed" }));
        self.usage(Some(&msg), 300 + 120 * calls.len() as u64);
        for (c, a) in calls.iter().zip(&ids) {
            let effects = match c.name {
                "read" => json!(["reads"]),
                "edit" | "write" => json!(["writes"]),
                _ => json!(["executes"]),
            };
            let paths = c.args.get("path").map(|p| json!([p])).unwrap_or(json!([]));
            self.m("tool_call_started", 40, Some(a), json!({ "effects": effects, "reversible": c.name == "read", "paths": paths }));
            let lines = |n: usize, head: &str| {
                let mut s = String::from(head);
                for k in 1..n {
                    let _ = write!(s, "\n{k}\t…");
                }
                json!([{ "type": "text", "text": s }])
            };
            let (dt, p) = match &c.r {
                R::Running => continue,
                R::Ok { lines: n, head } => (250, json!({ "status": "completed", "content": lines(*n, head) })),
                R::Changed { path, added, removed } => (300, json!({
                    "status": "completed",
                    "content": [{ "type": "text", "text": "block 1: replaced" }],
                    "changes": [{ "path": path, "added": added, "removed": removed }],
                })),
                R::Proc { lines: n, exit, dur } => (*dur, json!({
                    "status": "completed",
                    "content": lines(*n, "running…"),
                    "process": { "exit_code": exit, "timed_out": false },
                })),
                R::Fail { code, msg, exit } => {
                    let mut p = json!({ "status": "failed", "content": lines(6, "error"), "error": { "code": code, "message": msg } });
                    if let Some(x) = exit {
                        p["process"] = json!({ "exit_code": x, "timed_out": false });
                    }
                    (if exit.is_some() { 18000 } else { 200 }, p)
                }
            };
            self.m("tool_call_completed", dt, Some(a), p);
        }
        ids
    }
    fn queue(&mut self, msgs: &[(&str, &str)]) {
        let m: Vec<Value> = msgs.iter().map(|(id, t)| json!({ "id": id, "text": t, "source": "person" })).collect();
        self.m("steering_queue", 300, None, json!({ "messages": m }));
    }
    /// A few of the delegate's own lines, relayed onto the parent's stream.
    fn delegate_calls(&mut self, files: &[&str]) {
        for f in files {
            let a = self.id("a");
            self.emit(DELEGATE, "tool_call_requested", 400, Some(&a), json!({ "name": "read", "arguments": { "path": f }, "provider_id": format!("toolu_{a}") }));
            self.emit(DELEGATE, "tool_call_started", 30, Some(&a), json!({ "effects": ["reads"], "reversible": true, "paths": [f] }));
            self.emit(DELEGATE, "tool_call_completed", 200, Some(&a), json!({ "status": "completed", "content": [{ "type": "text", "text": "…" }] }));
        }
    }
}

const REPLY1: &str = "## Why the lock test is flaky

The test races. `second_writer_is_refused` spawns the child after a fixed `thread::sleep(200ms)` and assumes the parent's lock file exists by then.

- On a loaded runner the parent has not flushed the lock yet.
- The child takes the lock, and the assertion fails.
- The same pattern appears at **31** other sites in the workspace.

```rust
// crates/log/tests/lock.rs
let child = spawn_child(&dir)?;
thread::sleep(Duration::from_millis(200)); // the race
let err = open_session(&dir).unwrap_err();
assert!(matches!(err, Error::SessionHeld { .. }));
```

| Crate | Sleeps | Waits on a child |
|---|---|---|
| log | 6 | 4 |
| loop | 9 | 7 |
| tools | 11 | 8 |
| doors | 5 | 3 |

Nothing is changed yet. Say the word and I will replace the sleeps with a wait on the lock file.";

const REPLY2: &str = "## Done: one helper, 23 sleeps replaced

`testutil::wait_for_path` polls with backoff until the path exists or the deadline passes. Every test that slept to wait for a child now waits on a file the child writes.

- The lock test passes 200 of 200 runs under `--test-threads=16`.
- Four timing tests in `crates/loop` keep their sleeps, as you asked.
- One edit in `shell_signal.rs` missed its block at first; the re-read fixed it.

| Crate | Files | Added | Removed |
|---|---|---|---|
| testutil | 1 | 14 | 0 |
| log | 3 | 18 | 9 |
| loop | 4 | 22 | 11 |
| tools | 6 | 31 | 16 |

A background stress run is still going, and a reviewer delegate is reading the diff.";

fn main() {
    let mut g = G {
        out: vec![],
        ts: 1_790_604_120_000, // 2026-09-28 14:02:00 UTC
        seq: HashMap::new(),
        turn: HashMap::new(),
        n: 0,
        ctx: 21_000,
        gen_n: 0,
    };
    g.m("fiber_started", 0, None, json!({ "version": "0.0.1", "schema_version": 1, "resumed": false }));
    g.m("session_started", 5, None, json!({ "created_at": g.ts, "workspace": "~/work/fiber" }));
    g.m("opening_message", 5, None, json!({
        "environment": { "date": "2026-09-28", "platform": "macos", "shell": "zsh", "workspace": "~/work/fiber",
            "git": { "branch": "fix/wait-for-path", "dirty": true }, "session_log": "~/.fiber/projects/fiber/sessions/s_9c41e2/events.jsonl" },
        "instruction_files": [{ "path": "AGENTS.md", "content": "…" }],
        "skills": [],
    }));
    let tools: Vec<Value> = ["read", "write", "edit", "shell", "jobs", "delegate_spawn", "delegate_fork", "ask_user", "web_fetch"]
        .iter()
        .map(|n| json!({ "name": n, "deferred": false }))
        .chain((0..32).map(|k| json!({ "name": format!("linear_{k}"), "deferred": true })))
        .collect();
    g.m("preamble_built", 5, None, json!({
        "reason": "start", "model": "anthropic/claude-opus-5-5", "effort": "high", "thinking": "adaptive",
        "tool_choice": "auto", "cache_lifetime": "1h", "system_prompt": "…", "tools": tools,
    }));
    g.m("mode_changed", 3000, None, json!({ "before": "ask", "after": "auto", "by": "mode" }));

    // ---- turn 1: find the flake, answered in markdown
    g.turn("The lock test in crates/log is flaky on Linux CI. Find out why before changing anything.");
    g.step(
        Some(("**Where the lock is taken**\nThe test spawns a second process that tries to open the same session and expects `session_held`. If the child starts before the parent has flushed its lock file, the child can win.\n\n**What to check**\nWhether the child waits on anything but a sleep.", 9)),
        vec![read("crates/log/tests/lock.rs", 142), read("crates/log/src/lock.rs", 214)],
    );
    g.step(None, vec![grep("thread::sleep", "crates", 31), read("crates/testutil/src/child.rs", 88)]);
    g.step(None, vec![cargo("cargo test -p log --test lock", 0, 14, 21000)]);
    g.say(None, REPLY1);
    g.end_turn("completed");

    // ---- turn 2: the long one
    g.ts += 40_000;
    g.turn("Fix it properly: wait on the lock file's creation instead of sleeping, then sweep every test in the workspace that sleeps to wait for a child process. Use one helper in crates/testutil.");
    g.say(Some(("**Plan**\nAdd `wait_for_path(path, deadline)` to testutil, built on polling with backoff. Then visit each sleep site; some are not waits for a child and must stay.\n\n**Order**\nHelper first, then the lock test, then crate by crate.", 14)), "I'll add one helper, `testutil::wait_for_path`, fix the lock test with it, then go crate by crate and run each crate's tests after its edits.");
    g.step(None, vec![read("crates/testutil/src/lib.rs", 64), edit("crates/testutil/src/child.rs", 14, 0)]);
    g.step(None, vec![edit("crates/log/tests/lock.rs", 3, 1), cargo("cargo test -p log --test lock", 0, 12, 19000)]);
    let tests = [
        ("crates/log/tests/append.rs", "crates/log"),
        ("crates/log/tests/torn_tail.rs", "crates/log"),
        ("crates/loop/tests/steer.rs", "crates/loop"),
        ("crates/loop/tests/cancel.rs", "crates/loop"),
        ("crates/tools/tests/shell_timeout.rs", "crates/tools"),
        ("crates/tools/tests/shell_signal.rs", "crates/tools"),
        ("crates/tools/tests/jobs_wait.rs", "crates/tools"),
        ("crates/doors/tests/serve_attach.rs", "crates/doors"),
    ];
    for (i, (t, dir)) in tests.iter().enumerate() {
        let thought = match i {
            2 => Some(("**The loop tests**\nTwo of these sleeps measure time on purpose. Only the child waits change.", 6)),
            5 => Some(("**The failed edit**\nThe block moved when the helper import went in. Read the file again before editing.", 5)),
            _ => None,
        };
        g.step(thought, vec![grep("thread::sleep", dir, 3 + i), read(t, 90 + 20 * i)]);
        if i == 5 {
            // a failed edit, then the re-read that fixes it
            g.step(None, vec![C {
                name: "edit",
                args: json!({ "path": t, "edits": [{ "old_text": "thread::sleep", "new_text": "wait_for_path" }] }),
                r: R::Fail { code: "no_match", msg: "block 1 not found".into(), exit: None },
            }]);
            g.step(None, vec![read(t, 188), edit(t, 4, 2)]);
        } else {
            g.step(None, vec![edit(t, 3 + (i as u32 % 3), 1 + (i as u32 % 2)), find(&format!("{dir}/tests"))]);
        }
        if i == 1 {
            g.queue(&[("c_71a2", "Leave the timing tests in crates/loop alone; their sleeps are what they measure.")]);
            g.m("notice", 800, None, json!({ "code": "config_key_ignored", "message": "~/.fiber/config.json sets tools.enabeld, which is not a configuration key. It was ignored." }));
        }
        if i == 2 {
            g.m("steering_applied", 200, None, json!({ "text": "Leave the timing tests in crates/loop alone; their sleeps are what they measure.", "source": "person", "steer_id": "c_71a2" }));
            g.queue(&[]);
        }
        if i == 3 {
            g.step(None, vec![cargo("cargo test -p loop", 101, 22, 18000)]);
            g.step(None, vec![read("crates/loop/tests/cancel.rs", 160), edit("crates/loop/tests/cancel.rs", 2, 2), cargo("cargo test -p loop", 0, 18, 24000)]);
        }
        if i == 4 {
            g.m("mcp_server_failed", 300, None, json!({ "server": "linear", "reason": "died", "restart": true,
                "error": { "code": "mcp_server_unavailable", "message": "The MCP server linear died. Fiber restarts it on the next call to one of its tools." } }));
        }
    }
    // a background job and a delegate, both still running at the end
    let ids = g.step(None, vec![
        C { name: "shell", args: json!({ "command": "cargo nextest run -p log --stress-count 200", "background": true }), r: R::Ok { lines: 1, head: "started job j_5e10".into() } },
        C { name: "delegate_spawn", args: json!({ "description": "review: the wait_for_path sweep", "role": "reviewer" }), r: R::Ok { lines: 1, head: "started delegate j_77c3".into() } },
    ]);
    g.m("job_started", 20, None, json!({ "job_id": "j_5e10", "action_id": ids[0], "tool_name": "shell", "description": "stress: log lock ×200", "output_path": "artifacts/j_5e10.log" }));
    g.m("job_started", 20, None, json!({ "job_id": "j_77c3", "action_id": ids[1], "tool_name": "delegate_spawn", "description": "review: the wait_for_path sweep", "output_path": "artifacts/j_77c3.log" }));
    g.m("delegate_started", 20, None, json!({ "job_id": "j_77c3", "session_id": DELEGATE, "harness": "fiber", "model": "openai/gpt-6-sol", "workspace": "~/work/fiber" }));
    g.turn.insert(DELEGATE.into(), "t_dele".into());
    g.emit(DELEGATE, "turn_started", 100, None, json!({ "input": [{ "type": "text", "text": "Review the diff on fix/wait-for-path." }] }));
    g.delegate_calls(&["crates/testutil/src/child.rs", "crates/log/tests/lock.rs"]);
    g.step(None, vec![cargo("cargo test --workspace", 0, 40, 38000)]);
    g.say(None, REPLY2);
    g.end_turn("completed");
    let idle_len = g.out.len();

    // ---- turn 3: still running at the end of the file
    g.ts += 25_000;
    g.turn("Run the stress job again with 400 iterations and tell me if it still fails.");
    g.delegate_calls(&["crates/loop/tests/steer.rs"]);
    g.step(
        Some(("**Checking the stress job**\nThe first run is still going; a second one would fight it for the lock directory.\n\n**What would still race**\nOnly the fsync ordering, which the new wait does not touch.", 8)),
        vec![read("artifacts/j_5e10.log", 40)],
    );
    g.queue(&[("c_8b04", "If it passes, run it on the Linux box too.")]);
    g.delegate_calls(&["crates/loop/tests/cancel.rs", "crates/tools/tests/jobs_wait.rs"]);
    g.step(None, vec![C { name: "shell", args: json!({ "command": "cargo nextest run -p log --stress-count 400 -j 16" }), r: R::Running }]);

    let write = |path: &str, lines: &[Value]| {
        let s: String = lines.iter().map(|v| v.to_string() + "\n").collect();
        std::fs::write(path, s).unwrap();
        eprintln!("{path}: {} lines", lines.len());
    };
    write("fixtures/session.jsonl", &g.out);
    write("fixtures/idle.jsonl", &g.out[..idle_len]);
}
