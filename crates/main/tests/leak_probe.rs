//! The leak-scan probe (`docs/testing.md`, "Running tests"): `fiber ask`
//! runs a shell command that leaves a process behind on purpose, so
//! `scripts/test-leak-probe` can require `scripts/leak-scan` to name it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};

/// A finished `function_call` for `name` with `arguments`.
fn function_call(call_id: &str, name: &str, arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    }})
}

/// An `openai-responses` stream answering `text`: what a scripted
/// reviewer verdict reads as.
fn text_reply(text: &str) -> Response {
    support::stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

#[test]
#[ignore = "leaks a process on purpose; scripts/test-leak-probe runs it with --run-ignored only"]
fn the_shell_tool_leak_is_named_by_pid() {
    let dir = std::env::var("FIBER_LEAK_PROBE_DIR").unwrap_or_else(|_| {
        panic!("FIBER_LEAK_PROBE_DIR is unset; run scripts/test-leak-probe, not this test directly")
    });
    // The leaked process leaves the group on purpose, as `setsid` and
    // `nohup` do (`docs/tools.md`, "Shell"), so the shell tool's wait for
    // an empty group ends without it. It writes its own PID through a
    // temp name and a rename, then polls a stop file the script owns, the
    // directory itself, and a 120 s ceiling, so it always exits.
    // A failure, such as the directory being gone, ends it.
    // The program holds no single quote, so it embeds in one.
    let program = "import os,sys,time; os.setsid(); d=sys.argv[1]; t=os.path.join(d,\"ask.pid.tmp\"); p=os.path.join(d,\"ask.pid\"); s=time.time(); open(t,\"w\").write(str(os.getpid())); os.rename(t,p);\nwhile not os.path.exists(os.path.join(d,\"stop\")) and os.path.isdir(d) and time.time()-s<120:\n time.sleep(0.05)";
    // The shell waits for the PID file before it exits: the file lands
    // strictly after `setsid`, so the group is already empty when the
    // shell exits and the call ends as one step, not a background job.
    // Without the wait the shell can exit first and the call moves to the
    // background, adding turns.
    let command = format!(
        "python3 -c '{program}' \"{dir}\" </dev/null >/dev/null 2>&1 & leaker=$!; while kill -0 $leaker 2>/dev/null && [ ! -f \"{dir}/ask.pid\" ]; do sleep 0.05; done; exit 0"
    );
    let server = ProviderServer::start([
        support::stream(&[function_call(
            "call_leak",
            "shell",
            &json!({"command": command}),
        )]),
        text_reply("allow"),
        support::hello(),
    ])
    .unwrap();
    let setup = support::Setup::new();
    setup.provider(&server);
    std::fs::write(
        setup.home().join("config.json"),
        json!({"model": "fake/m", "reviewer": {"model": "fake/m"}}).to_string(),
    )
    .unwrap();

    let output = support::run_to_exit(
        setup.deadline,
        "`fiber ask` to exit",
        setup.fiber(&["ask", "run the command"]),
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.requests().len(), 3, "one turn, no background job");
}
