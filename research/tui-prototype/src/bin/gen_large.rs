//! Writes three large session fixtures for #15's memory and CPU measurement:
//! `fixtures/large-median.jsonl`, `large-p90.jsonl` and `large-heavy.jsonl`.
//! Every line is a `docs/events.md` **durable** line only (the session log
//! holds no ephemeral line; `docs/events.md`, "Durable and ephemeral"), so
//! there is no delta streaming here, unlike `gen.rs`'s fixture, which also
//! plays back live and needs deltas for that.
//!
//! Sizes come from measured evidence, not guesses; see `LARGE.md` for the
//! sources and the numbers themselves. This file only encodes them. Run with
//! `cargo run --bin gen_large`.

use serde_json::{Value, json};
use std::fmt::Write as _;

const MAIN: &str = "s_large01";

// ---- deterministic RNG: xorshift64* plus a 12-uniform sum for a normal
// draw (Irwin-Hall), both stdlib-only. Good enough to place the right mass
// at the two percentiles a distribution is fitted to; not a statistics
// library.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn normal(&mut self) -> f64 {
        let mut s = 0.0;
        for _ in 0..12 {
            s += self.f64();
        }
        s - 6.0
    }
    /// A lognormal fitted to a median and a p90 (z = 1.2816). The two points
    /// pin the curve; p99 falls out of the fit rather than being forced, and
    /// LARGE.md reports how close that lands to the measured p99.
    fn lognormal(&mut self, median: f64, p90: f64) -> f64 {
        let sigma = (p90 / median).ln() / 1.2816;
        (median.ln() + sigma * self.normal()).exp()
    }
    /// Same shape, but centred on `center` instead of the fitted median —
    /// used to draw "near p99" values for the heavy session.
    fn lognormal_centered(&mut self, center: f64, sigma: f64) -> f64 {
        (center.ln() + sigma * self.normal()).exp()
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo + 1)
    }
    fn weighted(&mut self, weights: &[u64]) -> usize {
        let total: u64 = weights.iter().sum();
        let mut x = self.next_u64() % total;
        for (i, w) in weights.iter().enumerate() {
            if x < *w {
                return i;
            }
            x -= *w;
        }
        weights.len() - 1
    }
}

// Tool call sizes: median/p90/p99 bytes, from research/tool-result-sizes
// (pi sessions, 648 sessions, last run 2026-09-22), and the call counts
// there give the tool mix. `render` is the tool name the prototype and
// docs/dependencies.md's tool surface actually use: grep, ffgrep and find
// all run through the shell tool (docs/dependencies.md, "the search behind
// the shell's grep and find").
// `flavor` picks the size distribution; `render` (used below in `gen_call`)
// is the tool name the prototype and docs/dependencies.md's tool surface
// actually use — grep, ffgrep and find all run through the shell tool.
struct ToolStat {
    flavor: &'static str,
    median: f64,
    p90: f64,
    p99: f64,
    weight: u64,
}
const TOOLS: &[ToolStat] = &[
    ToolStat { flavor: "bash", median: 799.0, p90: 4_856.0, p99: 18_916.0, weight: 26_829 },
    ToolStat { flavor: "read", median: 4_444.0, p90: 21_797.0, p99: 51_263.0, weight: 5_070 },
    ToolStat { flavor: "grep", median: 1_953.0, p90: 17_331.0, p99: 130_901.0, weight: 1_202 },
    ToolStat { flavor: "ffgrep", median: 1_769.0, p90: 12_217.0, p99: 49_494.0, weight: 1_057 },
    ToolStat { flavor: "find", median: 118.0, p90: 3_007.0, p99: 42_073.0, weight: 136 },
    ToolStat { flavor: "web_fetch", median: 4_734.0, p90: 33_729.0, p99: 51_475.0, weight: 21 },
    ToolStat { flavor: "edit", median: 110.0, p90: 149.0, p99: 203.0, weight: 3_567 },
];

// Assistant text and reasoning lengths (chars), measured for this ticket
// from the owner's Claude Code main sessions (~/.claude/projects/*/*.jsonl):
// see LARGE.md for the pi comparison and why Claude Code's numbers are used
// throughout for internal consistency.
const TEXT_MEDIAN: f64 = 193.0;
const TEXT_P90: f64 = 2_378.0;
const TEXT_MAX: f64 = 9_262.0;
const REASON_MEDIAN: f64 = 249.0;
const REASON_P90: f64 = 359.0;
const REASON_MAX: f64 = 830.0;
// 702 reasoning blocks over 6,790 text blocks in that same sample: about
// one step in ten opens with reasoning.
const REASON_PROB: u64 = 10;

// Tool calls per prompt and tool calls between two pieces of assistant
// text, from research/tui-surface/README.md's Claude Code column.
const CALLS_PER_PROMPT_MEDIAN: f64 = 5.0;
const CALLS_PER_PROMPT_P90: f64 = 28.0;
const CALLS_PER_PROMPT_P99: f64 = 101.0;
const BETWEEN_TEXT_MEDIAN: f64 = 2.0;
const BETWEEN_TEXT_P90: f64 = 6.0;
const BETWEEN_TEXT_MAX: f64 = 141.0;

fn filler(rng: &mut Rng, target: usize) -> String {
    const WORDS: &[&str] = &[
        "session", "tool", "result", "fold", "render", "event", "payload", "turn", "action",
        "call", "path", "budget", "measure", "prototype", "conversation", "panel", "ledger",
        "step", "text", "fixture",
    ];
    let mut s = String::with_capacity(target + 80);
    let mut n = 1u32;
    while s.len() < target {
        let _ = write!(s, "{n}\t");
        for _ in 0..(6 + rng.next_u64() % 6) {
            s.push_str(WORDS[(rng.next_u64() as usize) % WORDS.len()]);
            s.push(' ');
        }
        s.push('\n');
        n += 1;
    }
    s.truncate(target);
    s
}

struct G {
    out: Vec<Value>,
    ts: i64,
    seq: u64,
    turn: Option<String>,
    n: u32,
    ctx: u64,
    gen_n: u32,
    rng: Rng,
}

struct Call {
    name: &'static str,
    args: Value,
    effects: Value,
    paths: Value,
    content: Value,
    process: Option<Value>,
    changes: Option<Value>,
}

impl G {
    fn new(seed: u64) -> Self {
        G { out: vec![], ts: 1_790_604_120_000, seq: 0, turn: None, n: 0, ctx: 8_000, gen_n: 0, rng: Rng::new(seed) }
    }
    fn id(&mut self, p: &str) -> String {
        self.n += 1;
        format!("{p}_{:05x}", self.n)
    }
    fn m(&mut self, kind: &str, dt: i64, aid: Option<&str>, payload: Value) {
        self.ts += dt;
        self.seq += 1;
        let mut e = serde_json::Map::new();
        e.insert("kind".into(), json!(kind));
        e.insert("session_id".into(), json!(MAIN));
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
        self.out.push(Value::Object(e));
    }

    fn turn(&mut self, text: &str) {
        let t = self.id("t");
        let cid = format!("c_{}", &t[2..]);
        self.turn = Some(t);
        self.m("turn_started", 1_500, None, json!({ "input": [{ "type": "message", "content": [{ "type": "text", "text": text }], "source": "driver", "command_id": cid }] }));
    }
    fn end_turn(&mut self) {
        self.m("turn_completed", 400, None, json!({ "outcome": "completed" }));
        self.turn = None;
    }
    fn usage(&mut self, aid: Option<&str>, out: u64) {
        self.gen_n += 1;
        self.ctx += 3_200;
        let p = json!({
            "generation_id": format!("gen_{:04}", self.gen_n),
            "model": "anthropic/claude-opus-5-5",
            "tokens": { "input": 1_400, "cache_read": self.ctx, "cache_write": { "1h": 2_600 }, "output": out },
            "cost": ((0.02 + self.ctx as f64 * 3e-7) * 1e4).round() / 1e4,
        });
        self.m("usage_recorded", 50, aid, p);
    }
    fn say(&mut self, with_reasoning: bool, chars: usize) {
        let a = self.id("a");
        self.m("assistant_message_started", 900, Some(&a), json!({}));
        if with_reasoning {
            let rchars = (self.rng.lognormal(REASON_MEDIAN, REASON_P90) as usize).clamp(20, REASON_MAX as usize * 2);
            let ra = self.id("a");
            self.m("reasoning_started", 800, Some(&ra), json!({}));
            let text = filler(&mut self.rng, rchars);
            self.m("reasoning_completed", 4_000, Some(&ra), json!({ "text": text, "provider_item": { "type": "reasoning" } }));
        }
        let text = filler(&mut self.rng, chars);
        self.m("assistant_message_completed", 3_000, Some(&a), json!({ "text": text, "outcome": "completed" }));
        self.usage(Some(&a), chars as u64 / 4);
    }
    fn gen_call(&mut self) -> Call {
        let weights: Vec<u64> = TOOLS.iter().map(|t| t.weight).collect();
        let t = &TOOLS[self.rng.weighted(&weights)];
        let cap = t.p99 * 3.0;
        let size = (self.rng.lognormal(t.median, t.p90)).min(cap).max(1.0) as usize;
        let content = json!([{ "type": "text", "text": filler(&mut self.rng, size) }]);
        let n = self.n;
        match t.flavor {
            "read" => {
                let path = format!("crates/mod{}/src/file{}.rs", n % 12, n % 47);
                Call { name: "read", args: json!({ "path": path.clone() }), effects: json!(["reads"]), paths: json!([path]), content, process: None, changes: None }
            }
            "edit" => {
                let path = format!("crates/mod{}/src/file{}.rs", n % 12, n % 47);
                let added = self.rng.range(1, 25);
                let removed = self.rng.range(0, 15);
                Call {
                    name: "edit",
                    args: json!({ "path": path.clone(), "edits": [{ "old_text": "old", "new_text": "new" }] }),
                    effects: json!(["writes"]),
                    paths: json!([path.clone()]),
                    content,
                    process: None,
                    changes: Some(json!([{ "path": path, "added": added, "removed": removed }])),
                }
            }
            "web_fetch" => Call {
                name: "web_fetch",
                args: json!({ "url": format!("https://example.com/doc{}", n % 30) }),
                effects: json!(["reads"]),
                paths: json!([]),
                content,
                process: None,
                changes: None,
            },
            _ => {
                let cmd = match t.flavor {
                    "bash" => format!("cargo test -p mod{} --test t{}", n % 12, n % 9),
                    "grep" => format!("grep -rn 'pattern{}' crates", n % 40),
                    "ffgrep" => format!("ffgrep 'symbol{}' crates", n % 40),
                    _ => format!("find crates/mod{} -name '*.rs'", n % 12),
                };
                Call {
                    name: "shell",
                    args: json!({ "command": cmd }),
                    effects: json!(["executes"]),
                    paths: json!([]),
                    content,
                    process: Some(json!({ "exit_code": 0, "timed_out": false })),
                    changes: None,
                }
            }
        }
    }
    /// One model round trip: optional reasoning, then `n` tool calls
    /// requested together and run one after another, mirroring gen.rs's
    /// `step`.
    fn step(&mut self, n: usize) {
        let with_reasoning = self.rng.range(1, 100) <= REASON_PROB;
        let msg = self.id("a");
        self.m("assistant_message_started", 900, Some(&msg), json!({}));
        if with_reasoning {
            let rchars = (self.rng.lognormal(REASON_MEDIAN, REASON_P90) as usize).clamp(20, REASON_MAX as usize * 2);
            let ra = self.id("a");
            self.m("reasoning_started", 800, Some(&ra), json!({}));
            let text = filler(&mut self.rng, rchars);
            self.m("reasoning_completed", 3_000, Some(&ra), json!({ "text": text, "provider_item": { "type": "reasoning" } }));
        }
        let calls: Vec<(String, Call)> = (0..n).map(|_| (self.id("a"), self.gen_call())).collect();
        for (a, c) in &calls {
            let pid = format!("toolu_{a}");
            self.m("tool_call_requested", 400, Some(a), json!({ "name": c.name, "arguments": c.args, "provider_id": pid }));
        }
        self.m("assistant_message_completed", 60, Some(&msg), json!({ "text": "", "outcome": "completed" }));
        self.usage(Some(&msg), 300 + 120 * n as u64);
        for (a, c) in &calls {
            self.m("tool_call_started", 30, Some(a), json!({ "effects": c.effects, "reversible": c.name == "read", "paths": c.paths }));
            let mut p = serde_json::Map::new();
            p.insert("status".into(), json!("completed"));
            p.insert("content".into(), c.content.clone());
            if let Some(proc) = &c.process {
                p.insert("process".into(), proc.clone());
            }
            if let Some(ch) = &c.changes {
                p.insert("changes".into(), ch.clone());
            }
            self.m("tool_call_completed", 150, Some(a), Value::Object(p));
        }
    }

    fn preamble(&mut self) {
        self.m("fiber_started", 0, None, json!({ "version": "0.0.1", "resumed": false, "mode": "ask" }));
        self.m("session_started", 5, None, json!({ "workspace": "~/work/fiber" }));
        self.m("opening_message", 5, None, json!({
            "environment": { "date": "2026-09-28", "os": "macos", "arch": "aarch64", "shell": "zsh", "workspace": "~/work/fiber",
                "git": { "branch": "large-session" }, "session_log": format!("~/.fiber/projects/fiber/sessions/{MAIN}/events.jsonl") },
            "instruction_files": [{ "path": "AGENTS.md", "content": "…" }],
            "skills": [],
        }));
        let tools: Vec<Value> = ["read", "write", "edit", "shell", "jobs", "delegate_spawn", "delegate_fork", "ask_user", "web_fetch"]
            .iter()
            .map(|n| json!({ "name": n, "deferred": false, "definition": { "name": n, "input_schema": { "type": "object" } } }))
            .collect();
        self.m("preamble_built", 5, None, json!({
            "reason": "start", "model": "anthropic/claude-opus-5-5", "context_window": 1_000_000, "trigger_at": 400_000, "effort": "high", "thinking": "adaptive",
            "tool_choice": "auto", "cache_lifetime": "1h", "system_prompt": "…", "tools": tools,
        }));
        self.m("mode_changed", 3_000, None, json!({ "before": "ask", "after": "auto", "by": "command" }));
    }
}

const PROMPTS: &[&str] = &[
    "Find out why the integration suite is flaky on CI and fix the root cause.",
    "Add a helper that waits on a condition instead of sleeping, and sweep every call site.",
    "Trace the memory growth in the long-running worker and cut it.",
    "Review the diff on the feature branch and list what still needs a test.",
    "The listing command is slow on a large project; profile it and speed it up.",
    "Migrate the config loader to the new schema, crate by crate.",
    "Reproduce the panic from the bug report and add a regression test.",
    "Audit every place that touches the session lock file for a race.",
    "Cut the binary size back under budget without dropping a feature.",
    "Write the missing docs for the extension hooks and check them against the code.",
];

/// Draws a turn's tool-call target from the per-prompt distribution, then
/// alternates batches of tool calls with assistant text until that target is
/// spent, using the "calls between text" distribution to size each batch
/// run. Ends with the turn's reply.
fn turn_body(g: &mut G, target_calls: u64) {
    let mut remaining = target_calls;
    loop {
        let interval = (g.rng.lognormal(BETWEEN_TEXT_MEDIAN, BETWEEN_TEXT_P90) as u64)
            .clamp(1, BETWEEN_TEXT_MAX as u64)
            .min(remaining.max(1));
        let mut left = interval;
        while left > 0 {
            let batch = left.min(1 + g.rng.range(0, 4));
            g.step(batch as usize);
            left -= batch;
        }
        remaining = remaining.saturating_sub(interval);
        let last = remaining == 0;
        let chars = if last {
            (g.rng.lognormal(TEXT_MEDIAN, TEXT_P90) as usize).clamp(20, TEXT_MAX as usize)
        } else {
            // a short mid-turn update, not the final reply
            (g.rng.lognormal(TEXT_MEDIAN, TEXT_P90) as usize).clamp(10, 400)
        };
        g.say(false, chars);
        if last {
            break;
        }
    }
}

/// `prompts` turns, each drawing its tool-call count from the per-prompt
/// distribution fitted to (median, p90).
fn build_by_prompts(seed: u64, prompts: u64) -> (G, u64) {
    let mut g = G::new(seed);
    g.preamble();
    let mut total = 0u64;
    for i in 0..prompts {
        g.ts += 20_000;
        g.turn(PROMPTS[(i as usize) % PROMPTS.len()]);
        let calls = (g.rng.lognormal(CALLS_PER_PROMPT_MEDIAN, CALLS_PER_PROMPT_P90) as u64).max(1);
        total += calls;
        turn_body(&mut g, calls);
        g.end_turn();
    }
    (g, total)
}

/// Turns whose tool-call counts are drawn centred on the p99 figure, until
/// the running total reaches `target_total` — modelling "p99 per prompt,
/// many prompts" against the real heaviest session's tool-call count.
fn build_heavy(seed: u64, target_total: u64) -> (G, u64, u64) {
    let mut g = G::new(seed);
    g.preamble();
    // A modest spread around p99 (not the full-population sigma, which is
    // fitted to a range starting near zero and would occasionally draw a
    // single prompt the size of the whole target): this is a judgment call
    // to get "many heavy prompts" rather than one or two freak ones, noted
    // in LARGE.md as not directly evidenced.
    let sigma = 0.3;
    let mut total = 0u64;
    let mut prompts = 0u64;
    while total < target_total && prompts < 40 {
        g.ts += 20_000;
        g.turn(PROMPTS[(prompts as usize) % PROMPTS.len()]);
        let calls = (g.rng.lognormal_centered(CALLS_PER_PROMPT_P99, sigma) as u64).clamp(40, 250);
        total += calls;
        turn_body(&mut g, calls);
        g.end_turn();
        prompts += 1;
    }
    (g, total, prompts)
}

fn write(path: &str, lines: &[Value]) {
    let s: String = lines.iter().map(|v| v.to_string() + "\n").collect();
    let bytes = s.len();
    std::fs::write(path, s).unwrap();
    eprintln!("{path}: {} lines, {} bytes ({:.2} MiB)", lines.len(), bytes, bytes as f64 / (1024.0 * 1024.0));
}

fn main() {
    std::fs::create_dir_all("fixtures").ok();

    // median: 5 prompts (Claude Code median session length)
    let (g, total) = build_by_prompts(0x0C0F_FEE1, 5);
    eprintln!("median: 5 prompts, {total} tool calls (target: per-prompt lognormal fit to median 5 / p90 28)");
    write("fixtures/large-median.jsonl", &g.out);

    // p90: 17 prompts (Claude Code p90 session length), same per-prompt distribution
    let (g, total) = build_by_prompts(0x0C0F_FEE2, 17);
    eprintln!("p90: 17 prompts, {total} tool calls (same per-prompt distribution as median, more prompts)");
    write("fixtures/large-p90.jsonl", &g.out);

    // heavy: p99-centred prompts until the total matches the longest real
    // Claude Code session by tool-call count (984, found by measuring the
    // owner's ~/.claude/projects/*/*.jsonl main sessions for this ticket).
    let (g, total, prompts) = build_heavy(0x0C0F_FEE3, 984);
    eprintln!("heavy: {prompts} prompts, {total} tool calls (target: 984, the longest real session found)");
    write("fixtures/large-heavy.jsonl", &g.out);
}
