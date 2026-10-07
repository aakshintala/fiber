//! The busy workloads (`docs/performance.md`, "Budgets"): a turn of 429
//! tool calls with the context window full to the handoff point (peak RSS
//! and log bytes), the same turn once more under strace (fsyncs), and
//! `web_fetch` converting a 10 MiB HTML page (peak RSS). Also the pieces
//! the resume workloads share: scripted replies, a session started in a
//! home of its own, and reading its log.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};

use crate::home::Home;
use crate::idle::{Ctx, Samples, Workload};
use crate::linux;
use crate::run::{self, Session};

/// The workloads every run of the harness repeats.
pub(crate) const WORKLOADS: [Workload; 2] = [
    Workload {
        name: "busy turn",
        timing: false,
        run: busy_turn,
    },
    Workload {
        name: "web_fetch",
        timing: false,
        run: web_fetch,
    },
];

/// The workloads that run once: strace slows the process it traces, so its
/// pass is never a timing or memory sample.
pub(crate) const ONCE: [Workload; 1] = [Workload {
    name: "fsyncs",
    timing: false,
    run: fsyncs,
}];

/// How long `fiber` may take to start, and to exit once idle.
pub(crate) const READY: Duration = Duration::from_secs(30);

/// How long one turn may take, the 429-call turn under strace and the
/// replay of a 2,000,000-token log included.
pub(crate) const TURN: Duration = Duration::from_secs(600);

/// The p99 of tool calls per user turn (`docs/performance.md`, "Budgets").
const TOOL_CALLS: usize = 429;

/// The handoff trigger: `handoff.tokens`' default of 400,000 comes before
/// 0.7 of the fake model's 1,000,000-token window.
const TRIGGER_TOKENS: usize = 400_000;

/// "A 300,000-token context is about 1.2 MB of text".
pub(crate) const BYTES_PER_TOKEN: usize = 4;

/// Each `read` result: together they fill the context to 5 % short of the
/// trigger, each under `read`'s 16 KiB cap.
const RESULT_BYTES: usize = TRIGGER_TOKENS * BYTES_PER_TOKEN * 95 / 100 / TOOL_CALLS;

/// The download cap, `MAX_BODY` in `crates/tools/src/web_fetch.rs`.
const PAGE_BYTES: usize = 10 * 1024 * 1024;

const BUSY_PROMPT: &str = "read each file";
const FINAL_TEXT: &str = "every file is read";

/// An `openai-responses` stream of `events`, then its completion reporting
/// `input` and `output` tokens.
fn stream(events: &[Value], input: usize, output: usize) -> Response {
    let mut body = String::new();
    let done = json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed",
        "usage": {"input_tokens": input, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": output}
    }});
    for event in events.iter().chain([&done]) {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        body.push_str(&format!("event: {kind}\ndata: {event}\n\n"));
    }
    Response::stream(body)
}

/// A reply of `text`, with the context it was asked over, in bytes.
pub(crate) fn text_reply(text: &str, context: usize) -> Response {
    stream(
        &[
            json!({"type": "response.output_text.delta", "delta": text}),
            json!({"type": "response.output_item.done", "item": {
                "type": "message", "content": [{"type": "output_text", "text": text}]
            }}),
        ],
        context / BYTES_PER_TOKEN,
        text.len().div_ceil(BYTES_PER_TOKEN),
    )
}

/// A reply calling `name` with `arguments`, a JSON object's text.
fn call_reply(id: &str, name: &str, arguments: &str, context: usize) -> Response {
    stream(
        &[json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": format!("fc_{id}"), "call_id": id,
            "name": name, "arguments": arguments
        }})],
        context / BYTES_PER_TOKEN,
        arguments.len().div_ceil(BYTES_PER_TOKEN),
    )
}

/// `len` bytes of lower-case words and single spaces, the same for the
/// same `seed`: nothing JSON escapes, so its logged length is its length.
pub(crate) fn filler(len: usize, seed: u64) -> String {
    const WORDS: [&str; 8] = [
        "turn", "tool", "call", "file", "session", "context", "model", "log",
    ];
    let mut state = seed;
    let mut text = String::with_capacity(len + 8);
    while text.len() < len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let pick = usize::try_from(state >> 61).unwrap_or_default();
        text.push_str(WORDS.get(pick).copied().unwrap_or("x"));
        text.push(' ');
    }
    text.truncate(len);
    text
}

/// The 429-call turn: the workspace files the model reads, the script the
/// provider serves, and the turn's content in bytes as the log counts it.
pub(crate) struct Turn {
    pub(crate) files: Vec<(String, String)>,
    pub(crate) script: Vec<Response>,
    pub(crate) content: usize,
}

impl Turn {
    /// Each reply but the last calls `read` once on its own file; each
    /// reports the context so far as its input tokens. The content is the
    /// prompt, every call's arguments as sent, every result's file bytes
    /// and the final text.
    pub(crate) fn new() -> Self {
        let mut files = Vec::with_capacity(TOOL_CALLS);
        let mut script = Vec::with_capacity(TOOL_CALLS + 1);
        let mut context = BUSY_PROMPT.len();
        for n in 0..TOOL_CALLS {
            let name = format!("f{n:03}.txt");
            let arguments = json!({"path": name}).to_string();
            script.push(call_reply(
                &format!("call_{n}"),
                "read",
                &arguments,
                context,
            ));
            let body = filler(RESULT_BYTES, u64::try_from(n).unwrap_or_default());
            context += arguments.len() + body.len();
            files.push((name, body));
        }
        script.push(text_reply(FINAL_TEXT, context));
        Self {
            files,
            script,
            content: context + FINAL_TEXT.len(),
        }
    }
}

/// A page of exactly `len` bytes, the same every time: nested `div`s,
/// paragraphs with entities, lists, links, tables, long attributes, a
/// `script` and a `style`.
pub(crate) fn page(len: usize) -> Vec<u8> {
    let head = format!(
        "<!doctype html><html><head><title>bench</title><script>var words = \"{}\";</script><style>p {{ margin: 0 }}</style></head><body>",
        filler(400, 1)
    );
    let tail = "</body></html>";
    let mut out = head;
    let mut n: u64 = 0;
    loop {
        let w = |k: u64| filler(40, n * 8 + k);
        let block = match n % 4 {
            0 => format!(
                "<div class=\"c{n}\" data-note=\"{}\"><p>{} &amp; {} &lt;b&gt; &#8212; {}</p></div>",
                filler(200, n),
                w(1),
                w(2),
                w(3)
            ),
            1 => format!(
                "<ul><li>{}</li><li>{}</li><li><a href=\"http://127.0.0.1/{n}\" title=\"{}\">{}</a></li></ul>",
                w(1),
                w(2),
                w(3),
                w(4)
            ),
            2 => format!(
                "<table><tr><th>{}</th><th>{}</th></tr><tr><td>{}</td><td>{} &quot;q&quot;</td></tr></table>",
                w(1),
                w(2),
                w(3),
                w(4)
            ),
            _ => format!(
                "<div><div><div><p><em>{}</em> <code>{}</code></p></div></div></div>",
                w(1),
                w(2)
            ),
        };
        if out.len() + block.len() + "<p></p>".len() + tail.len() > len {
            break;
        }
        out.push_str(&block);
        n += 1;
    }
    let pad = len.saturating_sub(out.len() + "<p></p>".len() + tail.len());
    out.push_str("<p>");
    out.push_str(&filler(pad, n));
    out.push_str("</p>");
    out.push_str(tail);
    out.into_bytes()
}

/// The number of `fdatasync` calls in `strace -f -o` output: lines whose
/// call, after the thread id, is `fdatasync(`. A call another thread
/// interrupted is counted at its `<unfinished ...>` line, not again at its
/// `resumed` line. None at all is an error: a pass with no log write
/// measured nothing.
pub(crate) fn fdatasync_calls(strace: &str) -> Result<u64, String> {
    let calls = strace
        .lines()
        .filter(|line| {
            line.trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start()
                .starts_with("fdatasync(")
        })
        .count();
    match calls {
        0 => Err("strace saw no fdatasync call".to_owned()),
        n => u64::try_from(n).map_err(|err| format!("counting fdatasync calls: {err}")),
    }
}

/// A log's lines, each as written with its newline's byte, and parsed.
pub(crate) fn log_lines(log: &str) -> Result<Vec<(usize, Value)>, String> {
    log.lines()
        .map(|line| {
            serde_json::from_str(line)
                .map(|value| (line.len() + 1, value))
                .map_err(|err| format!("a log line is not JSON ({err})"))
        })
        .collect()
}

fn kind(line: &Value) -> Option<&str> {
    line.get("kind").and_then(Value::as_str)
}

/// The lines of `kind` whose payload's `key` is `value`, or every line of
/// `kind` when `key` is empty.
pub(crate) fn count(lines: &[(usize, Value)], of: &str, key: &str, value: &str) -> usize {
    lines
        .iter()
        .filter(|(_, line)| {
            kind(line) == Some(of)
                && (key.is_empty()
                    || line
                        .pointer(&format!("/payload/{key}"))
                        .and_then(Value::as_str)
                        == Some(value))
        })
        .count()
}

/// The bytes of the lines from the first `turn_started` through the first
/// `turn_completed` after it, inclusive.
pub(crate) fn turn_bytes(lines: &[(usize, Value)]) -> Result<usize, String> {
    let start = lines
        .iter()
        .position(|(_, line)| kind(line) == Some("turn_started"))
        .ok_or("the log has no turn_started")?;
    let mut bytes = 0;
    for (len, line) in lines.iter().skip(start) {
        bytes += len;
        if kind(line) == Some("turn_completed") {
            return Ok(bytes);
        }
    }
    Err("the log has no turn_completed after its turn_started".to_owned())
}

/// Runs `measure` in `home`, then removes it and every process left in it.
pub(crate) fn in_home(
    home: Home,
    measure: impl FnOnce(&Home) -> Result<Samples, String>,
) -> Result<Samples, String> {
    let measured = measure(&home);
    let finished = home.finish();
    let samples = measured?;
    finished?;
    Ok(samples)
}

/// The arguments of `fiber session` for `id` in `home`'s workspace, with
/// `extra` appended.
fn session_args(home: &Home, id: &str, extra: &[&str]) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["session", "--id", id, "--workspace"]
        .iter()
        .map(OsString::from)
        .collect();
    args.push(home.workspace().into_os_string());
    args.extend(extra.iter().map(OsString::from));
    args
}

/// `fiber session` for `id` in `home`'s workspace, with `extra` appended.
pub(crate) fn session(ctx: &Ctx<'_>, home: &Home, id: &str, extra: &[&str]) -> Command {
    let mut command = run::command(home.fiber(), home.root(), &home.home(), ctx.path.as_deref());
    command.args(session_args(home, id, extra));
    command
}

/// Reads stdout lines until one of `kind`, waiting at most `within`.
pub(crate) fn until_stdout(
    ctx: &Ctx<'_>,
    session: &Session,
    kind_of: &str,
    within: Duration,
) -> Result<(), String> {
    let until = ctx.clock.now() + within;
    while kind(&session.line(ctx.clock, until, kind_of)?) != Some(kind_of) {}
    Ok(())
}

/// The session's log, parsed.
pub(crate) fn read_log(home: &Home, id: &str) -> Result<Vec<(usize, Value)>, String> {
    let path = log::sessions_dir(&home.home(), &doors::project(&home.workspace()))
        .join(id)
        .join("events.jsonl");
    let text =
        fs::read_to_string(&path).map_err(|err| format!("reading {}: {err}", path.display()))?;
    log_lines(&text)
}

/// Notes a self-check that does not hold.
pub(crate) fn expect(notes: &mut Vec<String>, what: &str, got: usize, want: usize) {
    if got != want {
        notes.push(format!("{got} {what}, expected {want}"));
    }
}

/// The self-checks of a turn that ran `calls` tool calls, one model
/// request more than that, and no handoff.
fn check_turn(
    notes: &mut Vec<String>,
    server: &ProviderServer,
    lines: &[(usize, Value)],
    calls: usize,
) {
    expect(notes, "model requests", server.requests().len(), calls + 1);
    expect(
        notes,
        "completed tool calls",
        count(lines, "tool_call_completed", "status", "completed"),
        calls,
    );
    expect(
        notes,
        "turn_completed lines",
        count(lines, "turn_completed", "", ""),
        1,
    );
    expect(
        notes,
        "handoff_started lines",
        count(lines, "handoff_started", "", ""),
        0,
    );
}

fn write(file: &Path, text: &str) -> Result<(), String> {
    fs::write(file, text).map_err(|err| format!("writing {}: {err}", file.display()))
}

fn busy_home(ctx: &Ctx<'_>, turn: &Turn) -> Result<Home, String> {
    let home = Home::scripted(ctx.home.fiber(), turn.script.clone())?;
    for (name, body) in &turn.files {
        write(&home.workspace().join(name), body)?;
    }
    Ok(home)
}

/// The 429-call turn: peak RSS once it completes, then the turn's log bytes.
fn busy_turn(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let turn = Turn::new();
    in_home(busy_home(ctx, &turn)?, |home| {
        let id = doors::mint("s_");
        let session = Session::spawn(
            &mut session(ctx, home, &id, &["--prompt", BUSY_PROMPT]),
            ctx.clock,
        )?;
        let pid = session.proc.pid();
        let measured = until_stdout(ctx, &session, "turn_completed", TURN)
            .and_then(|()| linux::peak_rss_kib(pid));
        let stopped = session.proc.stop(ctx.clock);
        let rss = measured?;
        stopped?;
        let lines = read_log(home, &id)?;
        check_turn(notes, home.server(), &lines, TOOL_CALLS);
        Ok(vec![
            ("busy_turn_rss_kib", json!(rss)),
            (
                "turn_log_bytes",
                json!({
                    "bytes": turn_bytes(&lines)?,
                    "content": turn.content,
                    "tool_calls": count(&lines, "tool_call_started", "", ""),
                }),
            ),
        ])
    })
}

/// The same turn under `strace -f`, counting `fdatasync`, the call the log
/// makes its lines durable with; directories are made durable with `fsync`,
/// so their creation is not counted. The session exits once the turn ends
/// (`session.idle_exit_ms` 0), and strace with it.
fn fsyncs(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let turn = Turn::new();
    in_home(busy_home(ctx, &turn)?, |home| {
        write(
            &home.home().join("config.json"),
            &json!({"model": "fake/m", "session": {"idle_exit_ms": 0}}).to_string(),
        )?;
        let out = home.root().join("strace.txt");
        let id = doors::mint("s_");
        let mut command = run::command(
            Path::new("strace"),
            home.root(),
            &home.home(),
            ctx.path.as_deref(),
        );
        command
            .args(["-f", "-e", "trace=fdatasync", "-o"])
            .arg(&out)
            .arg(home.fiber())
            .args(session_args(home, &id, &["--prompt", BUSY_PROMPT]));
        let session = Session::spawn(&mut command, ctx.clock)?;
        let ran = until_stdout(ctx, &session, "turn_completed", TURN);
        let mut proc = session.proc;
        let exited = match ran {
            Ok(()) => proc.exits(ctx.clock, READY),
            Err(err) => Err(err),
        };
        let stopped = proc.stop(ctx.clock);
        if !exited? {
            notes.push("the session did not exit once idle".to_owned());
        }
        stopped?;
        let strace =
            fs::read_to_string(&out).map_err(|err| format!("reading {}: {err}", out.display()))?;
        let lines = read_log(home, &id)?;
        check_turn(notes, home.server(), &lines, TOOL_CALLS);
        Ok(vec![(
            "fsyncs",
            json!({
                "model_requests": count(&lines, "assistant_message_started", "", ""),
                "tool_calls": count(&lines, "tool_call_started", "", ""),
                "fdatasync": fdatasync_calls(&strace)?,
            }),
        )])
    })
}

/// One `web_fetch` of a 10 MiB HTML page from a second fake server,
/// allowed by a standing rule: peak RSS once the turn completes.
fn web_fetch(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let site = ProviderServer::start([Response {
        status: 200,
        headers: vec![(
            "content-type".to_owned(),
            "text/html; charset=utf-8".to_owned(),
        )],
        body: page(PAGE_BYTES),
        drop_connection: false,
        stall: false,
    }])
    .map_err(|err| format!("starting the page server: {err}"))?;
    let url = format!("{}/page.html", site.url());
    let arguments = json!({"url": url}).to_string();
    let prompt = "fetch the page";
    let script = [
        call_reply("call_fetch", "web_fetch", &arguments, prompt.len()),
        text_reply("fetched", prompt.len() + arguments.len()),
    ];
    in_home(Home::scripted(ctx.home.fiber(), script)?, |home| {
        let rule =
            json!({"decision": "allow", "tool": "web_fetch", "prefix": format!("{}/", site.url())});
        write(&home.home().join("rules"), &format!("{rule}\n"))?;
        let id = doors::mint("s_");
        let session = Session::spawn(
            &mut session(ctx, home, &id, &["--prompt", prompt]),
            ctx.clock,
        )?;
        let pid = session.proc.pid();
        let measured = until_stdout(ctx, &session, "turn_completed", TURN)
            .and_then(|()| linux::peak_rss_kib(pid));
        let stopped = session.proc.stop(ctx.clock);
        let rss = measured?;
        stopped?;
        expect(notes, "page requests", site.requests().len(), 1);
        check_turn(notes, home.server(), &read_log(home, &id)?, 1);
        Ok(vec![("web_fetch_rss_kib", json!(rss))])
    })
}

#[cfg(test)]
#[path = "busy_tests.rs"]
mod tests;
