//! Measures what it costs to list N Fiber sessions when the listing needs
//! more than each file's first line: the first prompt (first `turn_started`)
//! and a state note from the tail of the file. See ../README.md.
//!
//! Usage: session-listing <label> <n> <preamble_kib> <total_kib> [runs]
//!
//! Generates a fresh set of N synthetic session directories under a temp
//! path, times four listing strategies over all N files in one process,
//! prints one result row, and deletes the fixtures before exiting.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// Real content, gathered as evidence (see README "Where each number came
// from"): this repo's own AGENTS.md, a skills listing observed in a real
// Claude Code session on this machine, and an environment block built from
// the fields docs/system-prompt.md says `opening_message` carries.
const AGENTS_MD: &str = include_str!("../fixtures-data/agents-md-sample.txt");
const SKILLS_LISTING: &str = include_str!("../fixtures-data/skills-listing.txt");
const ENV_BLOCK: &str = include_str!("../fixtures-data/env-block.txt");

const PREFIX_READ: usize = 256 * 1024;
const TAIL_READ: usize = 4096;

// ---------------------------------------------------------------- JSON ----

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// One envelope line, `payload_body` being the already-built contents of the
/// `payload` object (no surrounding braces).
fn envelope(
    kind: &str,
    session_id: &str,
    ts: u64,
    seq: Option<u64>,
    turn_id: Option<&str>,
    payload_body: &str,
) -> String {
    let mut s = String::with_capacity(payload_body.len() + 160);
    s.push('{');
    s.push_str(&format!("\"kind\":\"{kind}\","));
    s.push_str(&format!("\"session_id\":\"{session_id}\","));
    s.push_str(&format!("\"ts\":{ts},"));
    s.push_str("\"schema_version\":1,");
    if let Some(t) = turn_id {
        s.push_str(&format!("\"turn_id\":\"{t}\","));
    }
    if let Some(n) = seq {
        s.push_str(&format!("\"seq\":{n},"));
    }
    s.push_str(&format!("\"payload\":{{{payload_body}}}"));
    s.push('}');
    s.push('\n');
    s
}

/// Benign filler text of exactly `len` bytes (ASCII, no characters that need
/// JSON escaping), used to pad a payload up to a target size.
fn filler(len: usize) -> String {
    const WORD: &str = "the model read tool wrote a file and called another tool ";
    let mut s = String::with_capacity(len);
    while s.len() < len {
        s.push_str(WORD);
    }
    s.truncate(len);
    s
}

// ------------------------------------------------------------ fixtures ----

struct SessionPlan {
    idx: usize,
    session_id: String,
    ending: &'static str,
}

const ENDINGS: [&str; 4] = [
    "exited",
    "pending_permission",
    "pending_interaction",
    "rewound",
];

const PROMPTS: [&str; 6] = [
    "Fix the flaky test in the handoff suite",
    "Add a budget for terminal idle CPU",
    "Rewind: the command carries the jobs to adopt",
    "Events and tools: lines changed and the steering queue",
    "MCP: mcp_server_failed carries error code and message",
    "Listing reads more than first lines, measure the real cost",
];

/// Writes one synthetic session directory. Returns the file's actual size
/// on disk (which can fall short of `total_kib` when the header and tail
/// alone are already bigger than the target).
fn generate_session(
    root: &Path,
    plan: &SessionPlan,
    preamble_kib: usize,
    total_kib: usize,
) -> std::io::Result<u64> {
    let dir = root.join(&plan.session_id);
    fs::create_dir_all(&dir)?;
    let path = dir.join("events.jsonl");

    let ts0 = 1_790_000_000_000u64 + (plan.idx as u64) * 60_000;
    let sid = &plan.session_id;

    // --- header: fiber_started .. first turn_started, per docs/events.md's
    // emission order.
    let mut head = String::new();
    head.push_str(&envelope(
        "fiber_started",
        sid,
        ts0,
        Some(1),
        None,
        "\"fiber_version\":\"0.0.1-dev\",\"resumed\":false",
    ));
    head.push_str(&envelope(
        "session_started",
        sid,
        ts0 + 1,
        Some(2),
        None,
        &format!(
            "\"created_at\":{},\"workspace\":\"/Users/owner/work/fiber\"",
            ts0
        ),
    ));

    let preamble_target = preamble_kib * 1024;
    let preamble_pad = filler(preamble_target.saturating_sub(180));
    let preamble_payload = format!(
        "\"reason\":\"start\",\"model\":\"claude-sonnet-5\",\"effort\":\"medium\",\
         \"tool_choice\":\"auto\",\"cache_lifetime\":\"5m\",\
         \"system_prompt\":\"{}\"",
        json_escape(&preamble_pad)
    );
    head.push_str(&envelope(
        "preamble_built",
        sid,
        ts0 + 2,
        Some(3),
        None,
        &preamble_payload,
    ));

    let opening_payload = format!(
        "\"environment\":\"{}\",\"instruction_files\":[{{\"path\":\"AGENTS.md\",\"content\":\"{}\"}}],\"skills\":\"{}\"",
        json_escape(ENV_BLOCK),
        json_escape(AGENTS_MD),
        json_escape(SKILLS_LISTING),
    );
    head.push_str(&envelope(
        "opening_message",
        sid,
        ts0 + 3,
        Some(4),
        None,
        &opening_payload,
    ));

    let turn_id = format!("t-{:08x}", plan.idx);
    let prompt = PROMPTS[plan.idx % PROMPTS.len()];
    head.push_str(&envelope(
        "turn_started",
        sid,
        ts0 + 4,
        Some(5),
        Some(&turn_id),
        &format!("\"input\":{{\"text\":\"{}\"}}", json_escape(prompt)),
    ));
    head.push_str(&envelope(
        "assistant_message_completed",
        sid,
        ts0 + 5,
        Some(6),
        Some(&turn_id),
        "\"text\":\"Working on it.\"",
    ));
    head.push_str(&envelope(
        "turn_completed",
        sid,
        ts0 + 6,
        Some(7),
        Some(&turn_id),
        "\"outcome\":\"completed\"",
    ));

    let header_len = head.len();

    // --- tail: a little more activity, then the state note's last line.
    let mut tail = String::new();
    tail.push_str(&envelope(
        "tool_call_requested",
        sid,
        ts0 + 100,
        Some(8),
        Some(&turn_id),
        "\"name\":\"read\",\"arguments\":{\"path\":\"docs/events.md\"}",
    ));
    tail.push_str(&envelope(
        "tool_call_completed",
        sid,
        ts0 + 101,
        Some(9),
        Some(&turn_id),
        "\"status\":\"completed\"",
    ));
    match plan.ending {
        "exited" => {
            tail.push_str(&envelope(
                "turn_completed",
                sid,
                ts0 + 102,
                Some(10),
                Some(&turn_id),
                "\"outcome\":\"completed\"",
            ));
            tail.push_str(&envelope(
                "fiber_exited",
                sid,
                ts0 + 103,
                Some(11),
                None,
                "\"exit_code\":0,\"final_action_id\":\"a-1\",\"text\":\"Done.\"",
            ));
        }
        "pending_permission" => {
            tail.push_str(&envelope(
                "tool_call_requested",
                sid,
                ts0 + 102,
                Some(10),
                Some(&turn_id),
                "\"name\":\"shell\",\"arguments\":{\"command\":\"rm -rf build\"}",
            ));
            tail.push_str(&envelope(
                "permission_requested",
                sid,
                ts0 + 103,
                Some(11),
                Some(&turn_id),
                "\"request_id\":\"r-1\",\"action_id\":\"a-2\",\"effects\":[\"writes\"]",
            ));
        }
        "pending_interaction" => {
            tail.push_str(&envelope(
                "tool_call_requested",
                sid,
                ts0 + 102,
                Some(10),
                Some(&turn_id),
                "\"name\":\"ask_user\",\"arguments\":{}",
            ));
            tail.push_str(&envelope(
                "interaction_requested",
                sid,
                ts0 + 103,
                Some(11),
                Some(&turn_id),
                "\"request_id\":\"r-2\",\"action_id\":\"a-3\",\"kind\":\"form\"",
            ));
        }
        "rewound" => {
            tail.push_str(&envelope(
                "turn_completed",
                sid,
                ts0 + 102,
                Some(10),
                Some(&turn_id),
                "\"outcome\":\"completed\"",
            ));
            tail.push_str(&envelope(
                "rewound",
                sid,
                ts0 + 103,
                Some(11),
                None,
                "\"session_id\":\"next-session\",\"seq\":9,\"jobs\":[]",
            ));
        }
        _ => unreachable!(),
    }

    let mut f = File::create(&path)?;
    f.write_all(head.as_bytes())?;

    let target_total = total_kib * 1024;
    if target_total > header_len + tail.len() + 4096 {
        // Leave a sparse hole in the middle: strategies (a)/(b)/(c) never
        // read it, and it is what lets total file size scale to megabytes
        // without megabytes of real disk writes.
        let gap = target_total - header_len - tail.len();
        f.set_len((header_len + gap) as u64)?;
        f.seek(SeekFrom::End(0))?;
    }
    f.write_all(tail.as_bytes())?;
    f.flush()?;

    Ok(f.metadata()?.len())
}

fn generate_fixtures(root: &Path, n: usize, preamble_kib: usize, total_kib: usize) -> Vec<PathBuf> {
    if root.exists() {
        fs::remove_dir_all(root).expect("clean old fixtures");
    }
    fs::create_dir_all(root).expect("create fixture root");

    let mut paths = Vec::with_capacity(n);
    for idx in 0..n {
        let plan = SessionPlan {
            idx,
            session_id: format!("s-{idx:06x}"),
            ending: ENDINGS[idx % ENDINGS.len()],
        };
        generate_session(root, &plan, preamble_kib, total_kib).expect("generate session");
        paths.push(root.join(&plan.session_id).join("events.jsonl"));
    }
    paths
}

// ------------------------------------------------------------ strategies ----

/// (a) First line only.
fn strategy_first_line(paths: &[PathBuf]) -> (Duration, u64) {
    let mut bytes = 0u64;
    let start = Instant::now();
    for p in paths {
        let f = File::open(p).unwrap();
        let mut r = BufReader::with_capacity(4096, f);
        let mut line = String::new();
        let n = r.read_line(&mut line).unwrap();
        bytes += n as u64;
    }
    (start.elapsed(), bytes)
}

/// (b1) Line-by-line buffered read until the first `turn_started`.
fn strategy_lines_to_turn(paths: &[PathBuf]) -> (Duration, u64) {
    let mut bytes = 0u64;
    let start = Instant::now();
    for p in paths {
        let f = File::open(p).unwrap();
        let mut r = BufReader::with_capacity(64 * 1024, f);
        let mut line = String::new();
        loop {
            line.clear();
            let n = r.read_line(&mut line).unwrap();
            if n == 0 {
                break; // EOF, no turn_started found (should not happen here)
            }
            bytes += n as u64;
            // `kind` is the envelope's first field: check the line's head,
            // not all of it — a `preamble_built` line can be over 100 KiB.
            if line
                .get(..64.min(line.len()))
                .unwrap_or(&line)
                .contains("\"kind\":\"turn_started\"")
            {
                break;
            }
        }
    }
    (start.elapsed(), bytes)
}

/// Reads up to `buf.len()` bytes of `p` in one read (looping only on a short
/// read), reusing the caller's buffer so the strategy is timed on I/O, not
/// on allocating and zeroing a fresh 256 KiB buffer per file.
fn read_prefix(p: &Path, buf: &mut [u8]) -> usize {
    let mut f = File::open(p).unwrap();
    let mut got = 0usize;
    loop {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => {
                got += n;
                if got == buf.len() {
                    break;
                }
            }
            Err(e) => panic!("read prefix: {e}"),
        }
    }
    got
}

/// Bytes up to and including the end of the first `turn_started` line found
/// in `buf[..len]`, or `len` if the marker is not in the prefix (a miss).
///
/// `kind` is always the envelope's first field ("The envelope" in
/// docs/events.md), so a real reader checks the start of each line rather
/// than scanning it in full — a `preamble_built` line can be well over
/// 100 KiB, and a naive byte-by-byte substring search over the whole prefix
/// would spend most of its time re-scanning that one line for nothing.
fn bytes_to_turn_started(buf: &[u8], len: usize) -> usize {
    const MARKER: &[u8] = b"\"kind\":\"turn_started\"";
    const HEAD: usize = 64; // comfortably past `{"kind":"...",` for any kind

    let mut start = 0usize;
    while start < len {
        let line_end = match buf[start..len].iter().position(|&b| b == b'\n') {
            Some(i) => start + i + 1,
            None => len,
        };
        let probe_end = (start + HEAD).min(line_end);
        if find(&buf[start..probe_end], MARKER).is_some() {
            return line_end;
        }
        start = line_end;
    }
    len
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// (b2) Fixed prefix read, scanned in memory for the first `turn_started`.
/// One buffer is allocated once and reused across every file, as a real
/// listing loop would.
fn strategy_prefix_to_turn(paths: &[PathBuf]) -> (Duration, u64) {
    let mut bytes = 0u64;
    let mut buf = vec![0u8; PREFIX_READ];
    let start = Instant::now();
    for p in paths {
        let got = read_prefix(p, &mut buf);
        bytes += got as u64; // the syscall pulled this many bytes regardless
        let _ = bytes_to_turn_started(&buf, got);
    }
    (start.elapsed(), bytes)
}

/// (c, built on the faster head strategy) line-by-line to `turn_started`,
/// plus a seek-to-end read for the state note.
fn strategy_lines_plus_tail(paths: &[PathBuf]) -> (Duration, u64) {
    let mut bytes = 0u64;
    let mut tail = vec![0u8; TAIL_READ];
    let start = Instant::now();
    for p in paths {
        let mut f = File::open(p).unwrap();
        {
            let mut r = BufReader::with_capacity(64 * 1024, &mut f);
            let mut line = String::new();
            loop {
                line.clear();
                let n = r.read_line(&mut line).unwrap();
                if n == 0 {
                    break;
                }
                bytes += n as u64;
                if line
                    .get(..64.min(line.len()))
                    .unwrap_or(&line)
                    .contains("\"kind\":\"turn_started\"")
                {
                    break;
                }
            }
        }
        let len = f.metadata().unwrap().len();
        let tail_len = TAIL_READ.min(len as usize);
        f.seek(SeekFrom::End(-(tail_len as i64))).unwrap();
        f.read_exact(&mut tail[..tail_len]).unwrap();
        bytes += tail_len as u64;
    }
    (start.elapsed(), bytes)
}

/// (c, built on the fixed-prefix head strategy) prefix read plus a
/// seek-to-end read for the state note.
fn strategy_prefix_plus_tail(paths: &[PathBuf]) -> (Duration, u64) {
    let mut bytes = 0u64;
    let mut buf = vec![0u8; PREFIX_READ];
    let mut tail = vec![0u8; TAIL_READ];
    let start = Instant::now();
    for p in paths {
        let mut f = File::open(p).unwrap();
        let mut got = 0usize;
        loop {
            match f.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => {
                    got += n;
                    if got == buf.len() {
                        break;
                    }
                }
                Err(e) => panic!("read prefix: {e}"),
            }
        }
        bytes += got as u64;
        let _ = bytes_to_turn_started(&buf, got);

        let len = f.metadata().unwrap().len();
        let tail_len = TAIL_READ.min(len as usize);
        f.seek(SeekFrom::End(-(tail_len as i64))).unwrap();
        f.read_exact(&mut tail[..tail_len]).unwrap();
        bytes += tail_len as u64;
    }
    (start.elapsed(), bytes)
}

// -------------------------------------------------------------- runner ----

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    }
}

fn time_strategy<F: Fn(&[PathBuf]) -> (Duration, u64)>(
    name: &str,
    paths: &[PathBuf],
    runs: usize,
    f: F,
) -> (f64, u64) {
    // Warm-up pass: brings the fixture tree into the page cache so every
    // timed run measures a warm-cache listing, the realistic case for a
    // person reopening Fiber.
    let (_, bytes) = f(paths);
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let (d, _) = f(paths);
        samples.push(d.as_secs_f64() * 1000.0);
    }
    let med = median(samples);
    eprintln!(
        "    {name}: median {med:.2} ms over {runs} runs, {} bytes/session",
        bytes / paths.len() as u64
    );
    (med, bytes / paths.len() as u64)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: session-listing <label> <n> <preamble_kib> <total_kib> [runs=5]");
        std::process::exit(2);
    }
    let label = &args[1];
    let n: usize = args[2].parse().expect("n");
    let preamble_kib: usize = args[3].parse().expect("preamble_kib");
    let total_kib: usize = args[4].parse().expect("total_kib");
    let runs: usize = args.get(5).map(|s| s.parse().expect("runs")).unwrap_or(5);

    let root = std::env::temp_dir().join(format!("fiber-session-listing-{label}"));
    eprintln!("[{label}] generating {n} sessions, preamble target {preamble_kib} KiB, total target {total_kib} KiB, under {}",
        root.display());
    let paths = generate_fixtures(&root, n, preamble_kib, total_kib);

    let sizes: Vec<u64> = paths
        .iter()
        .map(|p| fs::metadata(p).unwrap().len())
        .collect();
    let actual_median_kib = median(sizes.iter().map(|&s| s as f64 / 1024.0).collect()) as u64;

    eprintln!("[{label}] actual median file size {actual_median_kib} KiB");

    let (a_ms, a_bytes) = time_strategy("first line only", &paths, runs, strategy_first_line);
    let (b1_ms, b1_bytes) = time_strategy(
        "line-by-line to turn_started",
        &paths,
        runs,
        strategy_lines_to_turn,
    );
    let (b2_ms, b2_bytes) = time_strategy(
        "256 KiB prefix, scanned",
        &paths,
        runs,
        strategy_prefix_to_turn,
    );
    let (c1_ms, c1_bytes) = time_strategy(
        "line-by-line + 4 KiB tail",
        &paths,
        runs,
        strategy_lines_plus_tail,
    );
    let (c2_ms, c2_bytes) = time_strategy(
        "prefix + 4 KiB tail",
        &paths,
        runs,
        strategy_prefix_plus_tail,
    );

    fs::remove_dir_all(&root).ok();

    println!(
        "{label}\t{n}\t{preamble_kib}\t{total_kib}\t{actual_median_kib}\t\
         {a_ms:.2}\t{a_bytes}\t{b1_ms:.2}\t{b1_bytes}\t{b2_ms:.2}\t{b2_bytes}\t\
         {c1_ms:.2}\t{c1_bytes}\t{c2_ms:.2}\t{c2_bytes}"
    );
}
