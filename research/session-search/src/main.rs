//! Times `session_search`'s scan, `log::SessionScan::scan`, over a synthetic
//! Fiber home. See ../README.md.
//!
//! Usage:
//!   session-search gen <home> <total_mib> <artifact_every>
//!   session-search scan <home> <query>
//!
//! `gen` writes one project of session logs and text artifacts under
//! `<home>/projects/`, until the logs total `<total_mib>`; one tool output
//! in `<artifact_every>` is also saved whole as a text artifact (0: none). `scan` runs one
//! scan of that project, the tool's default search, and prints one row:
//! milliseconds, hits found, problems.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use contract::session_search::{Query, Scan};
use contract::tool::Cancel;
use contract::{Envelope, Seq, SessionId};
use serde_json::{Value, json};

/// The workspace every synthetic session records.
const WORKSPACE: &str = "/bench/workspace";

/// Session log sizes, from the owner's pooled Claude Code and pi session
/// files (research/session-listing, "Where each number came from"):
/// median 326 KiB, 90th percentile 1,749 KiB, maximum 15,313 KiB. A
/// log-normal through the median and the 90th percentile, cut at the
/// maximum.
const MEDIAN_KIB: f64 = 326.0;
const P90_KIB: f64 = 1_749.0;
const MAX_KIB: f64 = 15_313.0;

/// One line in this many tool outputs holds the many-hits query.
const NEEDLE_EVERY: u64 = 200;

/// A saved artifact's size: the whole output of a cut tool result.
const ARTIFACT_KIB: usize = 64;

/// A small deterministic generator, so every run sees the same corpus.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in (0, 1].
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }

    /// A standard normal (Box-Muller).
    fn normal(&mut self) -> f64 {
        let (a, b) = (self.unit(), self.unit());
        (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()
    }
}

/// Words a tool output is made of: code and prose of the kind a session
/// reads, never the queries.
const WORDS: &[&str] = &[
    "fn", "let", "match", "self", "return", "impl", "struct", "pub", "use", "crate", "error",
    "result", "path", "file", "line", "the", "a", "of", "to", "in", "is", "and", "session",
    "tool", "call", "output", "test", "cargo", "build", "check", "string", "value", "map",
    "config", "provider", "event", "log", "seq", "offset", "\"quoted\"", "{", "}", "(", ")",
    "=>", "::", "//", "0", "1", "42", "Ok(())", "Err(e)", "\\path\\to", "tab\there",
];

fn words(rng: &mut Rng, bytes: usize, needle: bool) -> String {
    let mut out = String::with_capacity(bytes + 16);
    let mut since_newline = 0;
    while out.len() < bytes {
        let word = WORDS[(rng.next() % WORDS.len() as u64) as usize];
        out.push_str(word);
        since_newline += word.len() + 1;
        if since_newline > 80 {
            out.push('\n');
            since_newline = 0;
        } else {
            out.push(' ');
        }
    }
    if needle {
        out.push_str("\nthe Retry Budget was exhausted\n");
    }
    out
}

/// Writes one log line; returns its bytes.
fn line(out: &mut impl Write, id: &str, seq: &mut u64, kind: &str, payload: Value) -> u64 {
    let Value::Object(payload) = payload else {
        unreachable!("payloads are objects");
    };
    let envelope = Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(id.to_owned()),
        ts: 1_700_000_000_000 + *seq * 1_000,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(*seq)),
        payload,
    };
    let mut bytes = serde_json::to_vec(&envelope).unwrap();
    bytes.push(b'\n');
    out.write_all(&bytes).unwrap();
    *seq += 1;
    bytes.len() as u64
}

/// Writes one session of about `kib` KiB of log; returns its log's bytes.
fn session(sessions: &Path, id: &str, kib: f64, artifact_every: u64, rng: &mut Rng) -> u64 {
    let dir = sessions.join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let path = dir.join("events.jsonl");
    let mut out = BufWriter::new(File::create(&path).unwrap());
    let mut seq = 0;
    let target = (kib * 1024.0) as u64;
    let mut written = line(
        &mut out,
        id,
        &mut seq,
        "session_started",
        json!({"workspace": WORKSPACE, "variables": {"path": "/bin", "names": [], "source": "inherited"}}),
    );
    written += line(
        &mut out,
        id,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "content": [{"type": "text", "text": words(rng, 300, false)}], "source": "driver"}]}),
    );
    let mut calls = 0u64;
    while written < target {
        calls += 1;
        written += line(
            &mut out,
            id,
            &mut seq,
            "tool_call_requested",
            json!({"name": "shell", "arguments": {"command": format!("rg -n {} src/", words(rng, 20, false))}}),
        );
        let size = 512 + (rng.next() % 6_144) as usize;
        let needle = rng.next() % NEEDLE_EVERY == 0;
        let mut completed = json!({"status": "completed", "content": [{"type": "text", "text": words(rng, size, needle)}]});
        if artifact_every > 0 && calls % artifact_every == 0 {
            let name = format!("call_{calls}.txt");
            let needle = rng.next() % 4 == 0;
            let full = words(rng, ARTIFACT_KIB * 1024, needle);
            fs::write(dir.join("artifacts").join(&name), full).unwrap();
            completed["artifact"] = json!(format!("artifacts/{name}"));
        }
        written += line(&mut out, id, &mut seq, "tool_call_completed", completed);
        if calls % 5 == 0 {
            written += line(
                &mut out,
                id,
                &mut seq,
                "text_completed",
                json!({"text": words(rng, 400, false)}),
            );
        }
    }
    out.flush().unwrap();
    written
}

fn generate(home: &Path, total_mib: u64, artifact_every: u64) {
    let sessions = log::sessions_dir(home, Path::new(WORKSPACE));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let sigma = (P90_KIB / MEDIAN_KIB).ln() / 1.281_551_6;
    let target = total_mib * 1024 * 1024;
    let (mut written, mut count) = (0u64, 0u64);
    while written < target {
        let kib = (MEDIAN_KIB.ln() + sigma * rng.normal()).exp().min(MAX_KIB);
        written += session(&sessions, &format!("s_{count:06}"), kib, artifact_every, &mut rng);
        count += 1;
    }
    println!(
        "generated\t{count} sessions\t{} MiB of logs",
        written / (1024 * 1024)
    );
}

struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

fn scan(home: &Path, text: &str) {
    let identity: log::Identity = Arc::new(|path: &Path| path.to_path_buf());
    let scan = log::SessionScan::new(home, Path::new(WORKSPACE), identity);
    let query = Query {
        text: text.to_owned(),
        all_projects: false,
        limit: 20,
    };
    let started = Instant::now();
    let found = scan.scan(&query, &Never);
    let elapsed = started.elapsed();
    println!(
        "{:.1}\t{}\t{}",
        elapsed.as_secs_f64() * 1_000.0,
        found.total,
        found.problems.len() as u64 + found.more_problems
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen") if args.len() == 5 => {
            generate(
                &PathBuf::from(&args[2]),
                args[3].parse().unwrap(),
                args[4].parse().unwrap(),
            );
        }
        Some("scan") if args.len() == 4 => scan(&PathBuf::from(&args[2]), &args[3]),
        _ => {
            eprintln!("usage: session-search gen <home> <total_mib> <artifact_every> | scan <home> <query>");
            std::process::exit(2);
        }
    }
}
