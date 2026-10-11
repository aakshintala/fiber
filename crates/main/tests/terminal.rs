//! Binary-level tests of the terminal door (`docs/testing.md`, "Screens"):
//! the real binary in a pseudo-terminal: the first frame, a journey that
//! types a prompt, sees the answer and cancels a turn, an approval
//! answered from the panel, a repository's offer answered from its view,
//! resize, and no tty.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::pty::{KITTY_PUSH, TITLE, WAITING_TITLE};
use support::{Deadline, PIXEL, hello, write_json};

/// Installs a provider `fake` with model `m` on `openai-responses` at the
/// fake server, makes `fake/m` the configured model, and idles the hub
/// out a second after its last client leaves, so no hub lingers.
fn provider(setup: &support::Setup, server: &ProviderServer) {
    provider_with(setup, &json!({}), server);
}

/// [`provider`], with the fake model declaring `input`
/// `["text", "image"]`, so a pasted image is sent as an image part.
fn provider_with_images(setup: &support::Setup, server: &ProviderServer) {
    provider_with(setup, &json!({"input": ["text", "image"]}), server);
}

fn provider_with(setup: &support::Setup, model_extra: &Value, server: &ProviderServer) {
    provider_full(setup, model_extra, server, None);
}

/// [`provider_with`], with the fake model declaring thinking levels and
/// the panel pinned to `panel_width` percent of the screen
/// (`docs/tui.md`, "Layout").
fn provider_with_panel(setup: &support::Setup, server: &ProviderServer, panel_width: f64) {
    provider_full(
        setup,
        &json!({"thinking_levels": ["low", "high"], "thinking_default": "high"}),
        server,
        Some(panel_width),
    );
}

fn provider_full(
    setup: &support::Setup,
    model_extra: &Value,
    server: &ProviderServer,
    panel_width: Option<f64>,
) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
    );
    let mut model = json!({"id": "m", "protocol": "openai-responses",
        "base_url": format!("{}/v1", server.url()), "context_window": 100000});
    for (key, value) in model_extra.as_object().unwrap() {
        model[key] = value.clone();
    }
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [model]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    let mut config = json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}});
    if let Some(width) = panel_width {
        config["tui"] = json!({"panel": {"width": width}});
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// The project's sessions directory, through the canonical workspace,
/// as the project key names it.
fn sessions(setup: &support::Setup) -> PathBuf {
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    setup.home().join("projects").join(key).join("sessions")
}

/// The one session in [`sessions`].
fn only_session(setup: &support::Setup) -> String {
    let mut ids: Vec<String> = fs::read_dir(sessions(setup))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "one session");
    ids.pop().unwrap()
}

/// Runs `fiber` with `args` headless in its own process group, waiting
/// under the test's [`Deadline`]. A watchdog beside it kills that group
/// if this process dies first.
#[track_caller]
fn ask(setup: &support::Setup, args: &[&str]) -> std::process::Output {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    support::run_to_exit(
        setup.deadline,
        &format!("`fiber {}`", args.join(" ")),
        command,
    )
}

/// Spawns `fiber` with `args` on a `cols` by `rows` terminal through the
/// shared driver, with `env` after the fake key in the child's
/// environment, so every run keeps today's key.
fn terminal(
    setup: &support::Setup,
    cols: u16,
    rows: u16,
    args: &[&str],
    env: &[(&str, &str)],
) -> support::pty::Run {
    let mut full = vec![("FIBER_TEST_FAKE_KEY", "sk-test")];
    full.extend(env.iter().copied());
    support::pty::Run::spawn(setup, cols, rows, args, &full)
}

/// A setup on `deadline`, with `h` and `w` under its root, as
/// [`support::Setup::new`] makes them: the no-tty runs name their own
/// deadline.
fn setup_within(deadline: Deadline) -> support::Setup {
    let root = fakes::TempDir::new("ft");
    fs::create_dir_all(root.path().join("h")).unwrap();
    fs::create_dir_all(root.path().join("w")).unwrap();
    support::Setup { root, deadline }
}

/// Whether the grid shows a working line with its elapsed count:
/// "Working" plus a digit right after its space. The working line draws
/// only once `turn_started` has opened the turn, so an Esc sent after it
/// is never dropped as not-busy, provided the count belongs to this turn:
/// a grid caught between two reads of one frame can still hold the
/// finished turn's line beside its `completed` close.
fn working_elapsed(contents: &str) -> bool {
    contents.match_indices("Working ").any(|(at, _)| {
        contents[at + "Working ".len()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_digit())
    })
}

#[test]
fn working_elapsed_needs_the_count() {
    assert!(working_elapsed("Working 0s · esc to interrupt"));
    assert!(working_elapsed("Working 39s · esc to interrupt"));
    assert!(working_elapsed("Working 1m 2s · esc to interrupt"));
    assert!(!working_elapsed("Working · esc to interrupt"));
    assert!(!working_elapsed("Working"));
    assert!(!working_elapsed("idle"));
}

/// Waits under the test's [`Deadline`] until `socket` exists or not, as
/// `present` says, naming `what` on expiry.
#[track_caller]
fn until_socket(deadline: Deadline, socket: &Path, present: bool, what: &str) {
    let socket = socket.to_owned();
    let (done, reached) = mpsc::channel();
    thread::spawn(move || {
        while socket.exists() != present {
            thread::yield_now();
        }
        done.send(()).unwrap_or(());
    });
    assert!(
        deadline.recv(&reached).is_ok(),
        "waited until the deadline for {what}"
    );
}

/// A reply saying `text` in two deltas.
fn reply(text: &str) -> Response {
    let at = text.len() / 2;
    let (first, rest) = text.split_at(at);
    support::stream(&[
        json!({"type": "response.output_text.delta", "delta": first}),
        json!({"type": "response.output_text.delta", "delta": rest}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": text}]
        }}),
    ])
}

/// A reply that stalls mid-body: the turn stays running until cancelled.
fn stalled() -> Response {
    let prefix = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Working\"}\n\n";
    Response::stall(200, prefix, prefix.len() + 100000).header("content-type", "text/event-stream")
}

#[test]
fn typing_a_prompt_sees_the_answer_and_cancels_a_turn() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello(), stalled()]).unwrap();
    provider(&setup, &server);
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    // The first frame draws the input line; its title proves the input
    // reader runs before the prompt goes out.
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The loop pushes kitty's flags only after it processes the harness's
    // reply, so this also keeps the responder's pty write ahead of input.
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    // Enter goes out once the hub connects.
    let from = run.output().len();
    run.write(b"say hi\r");
    // The reply streams in two deltas; the turn's close says it finished,
    // whichever order the paint and the finished title arrive in.
    run.wait_screen("the first delta", |grid| grid.contents.contains("Hel"));
    run.turn_finished(from);
    // The finished turn's working line is cleared a moment after its
    // close is drawn; the next wait must not match that old count.
    run.wait_screen("the first turn's working line cleared", |grid| {
        !grid.contents.contains("esc to interrupt")
    });
    // The second prompt starts a stalled turn; Esc interrupts it. The
    // elapsed count proves `turn_started` folded, which opens the turn:
    // Esc goes out as `cancel` only while the turn is busy.
    // The earlier kitty-push wait ensures the harness finished its reply
    // before this key is written to the pty master.
    run.write(b"again\r");
    run.wait_screen("the running turn", |grid| working_elapsed(&grid.contents));
    run.write(b"\x1b[27u");
    run.wait_screen("the interrupted turn", |grid| {
        grid.alternate_screen && grid.contents.contains("interrupted")
    });
    run.write(b"\x03\x03\r");
    // The turn just ended, so its idle status may still be on its way: the
    // quit either exits at once or asks first (`docs/tui.md`, "Quit"). The
    // Enter goes out with the Ctrl+C bytes, so it is processed after them:
    // it leaves working sessions running, and when the terminal already
    // exited it is never read.
    // The terminal is restored: the primary screen is back, the cursor
    // shows, and one resume line per live session is on it ("On exit").
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// A finished turn sends an OSC 9 desktop notification where the
/// terminal supports one.
#[test]
fn a_finished_turn_sends_an_osc_9_notification() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    provider(&setup, &server);
    let mut run = terminal(&setup, 120, 32, &[], &[("TERM_PROGRAM", "ghostty")]);
    // The first frame's title proves the input reader runs before the
    // prompt goes out; quitting is taken in any state, so the resume
    // wait below proves the quit.
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"say hi\r");
    // The reply streams in two deltas; the turn's close says it finished.
    run.wait_screen("the first delta", |grid| grid.contents.contains("Hel"));
    // The notification never reaches the cells, so this one wait stays on
    // the raw bytes.
    run.wait_bytes(0, b"\x1b]9;Fiber: ", "the OSC 9 notification");
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// A reply that calls the shell with `echo hi`.
fn calls_echo_hi() -> Response {
    let events = [
        json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": "fc_call_1", "call_id": "call_1", "name": "shell",
            "arguments": json!({"command": "echo hi"}).to_string()
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

#[test]
fn a_standing_ask_opens_the_approval_panel_and_allow_once_runs_the_call() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([calls_echo_hi(), hello()]).unwrap();
    provider(&setup, &server);
    // A standing ask for this exact command: with the terminal connected
    // the loop asks a person (`docs/permissions.md`, "Headless").
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let asking = run.output().len();
    run.write(b"run it\r");
    run.wait_screen("the approval panel", |grid| {
        grid.contents.contains("asked by a global rule: echo hi")
    });
    run.wait_screen("the approval choices", |grid| {
        grid.contents.contains("allow once")
    });
    // Enter on the first choice allows once; the call runs and the turn
    // finishes with the answer. The waiting title from before the prompt
    // proves the turn waits on a person before the choice goes out.
    run.wait_bytes(asking, WAITING_TITLE, "the waiting turn");
    run.write(b"\r");
    // The reply streams in two deltas; the turn's close says it finished.
    run.wait_screen("the first delta", |grid| grid.contents.contains("Hel"));
    run.wait_screen("the finished turn", |grid| {
        grid.contents.contains("completed")
    });
    // As above: the turn just ended, so quitting either exits at once or
    // asks first. The Enter leaves the session running, and exiting prints
    // its resume line ("Quit", "On exit").
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    // The model got the call's output, not a denial.
    let requests = server.requests();
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let result = second["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(result["output"], "hi\nExit code 0.\n");
}

/// A script step that calls `ask_user` with four questions, then the shell.
fn ask_then_echo_hi_script() -> Value {
    let questions = json!([
        {"header": "Timeout", "question": "How long?",
         "options": [{"label": "1m"}, {"label": "5m"}]},
        {"header": "Scope", "question": "Which scope?",
         "options": [{"label": "a"}, {"label": "b"}]},
        {"header": "Name", "question": "What name?"},
        {"header": "Pick", "question": "Which one?",
         "options": [{"label": "x"}, {"label": "y"}]},
    ]);
    json!({"steps": [{"tool_calls": [
        {"name": "ask_user", "arguments": {"questions": questions}},
        {"name": "shell", "arguments": {"command": "echo hi"}},
    ]}]})
}

#[test]
fn an_ask_and_a_shell_waiting_on_approval_show_no_call_json() {
    let setup = support::Setup::new();
    // The built-in `scripted` provider answers from a script in the
    // workspace, named as an ordinary model (`docs/model-routing.md`,
    // "The scripted provider"): one step carries both tool calls.
    write_json(
        &setup.workspace().join("s.json"),
        &ask_then_echo_hi_script(),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    // A standing project ask for this exact command: with the terminal
    // connected the loop asks a person (`docs/permissions.md`, "Headless").
    // The project's rules live in Fiber home at `projects/<key>/rules`
    // (`docs/state.md`, "Projects"), never in the workspace, so the harness
    // places one through the canonical project key, as `Setup::sessions` does.
    let key = log::project_key(&doors::project(&setup.workspace()));
    let rule = setup.home().join("projects").join(key).join("rules");
    fs::create_dir_all(rule.parent().unwrap()).unwrap();
    fs::write(
        rule,
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let mut run = terminal(&setup, 160, 48, &[], &[]);
    // A 160x48 grid, as the ticket's screen: every frame draws at the
    // ticket's width. The first frame's title proves the input reader
    // runs before the prompt goes out; the approval choices prove the
    // quit below lands on the drawn view.
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"run it\r");
    // The collapsed group line counts the call's parsed form, before the
    // shell's approval panel opens below it.
    run.wait_screen("the asked questions", |grid| {
        grid.contents.contains("asked 4 questions")
    });
    run.wait_screen("the approval panel", |grid| {
        grid.contents.contains("asked by a project rule: echo hi")
    });
    run.wait_screen("the approval choices", |grid| {
        grid.contents.contains("allow once")
    });
    let grid = run.screen();
    let rows = grid.rows;
    // The approval panel shows the shell's arguments for review, so the
    // shell's JSON is expected there; the `ask_user` call's JSON must
    // never draw: neither on the group line nor in its ledger row. The
    // grid reassembles wrapped rows, so every drawn row is checked.
    assert!(
        !rows.iter().any(|row| row.contains("{\"questions\"")),
        "{rows:?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("\"header\"")),
        "{rows:?}"
    );
    assert!(grid.contents.contains("asked 4 questions"), "{rows:?}");
    assert!(
        !rows.iter().any(|row| row.contains("ask_user {")),
        "{rows:?}"
    );
    // No row of the conversation holds `{"` except the shell approval
    // panel's arguments (`{\"command\":\"echo hi\"}`): every occurrence
    // on the grid is immediately followed by `command"`.
    let mut rest = grid.contents.as_str();
    let mut calls = 0;
    while let Some(at) = rest.find("{\"") {
        calls += 1;
        let after = &rest[at + 2..];
        assert!(after.starts_with("command\""), "{rows:?}");
        rest = &rest[at + 2..];
    }
    assert!(calls > 0, "{rows:?}");
    // As above: quitting either exits at once or asks first.
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// An `ask_user` call stalled mid-arguments: the added event names the
/// call, then one arguments delta carries the first part of its JSON and
/// the body stalls, so the raw text stays on the group line.
fn streaming_ask_stalls() -> Response {
    let added = json!({"type": "response.output_item.added", "item": {
        "type": "function_call", "id": "fc_ask", "name": "ask_user"
    }});
    let delta = json!({"type": "response.function_call_arguments.delta",
        "item_id": "fc_ask", "delta": "{\"questions\""});
    let prefix: String = [added, delta]
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stall(200, prefix.clone(), prefix.len() + 100000)
        .header("content-type", "text/event-stream")
}

#[test]
fn a_streaming_ask_shows_its_raw_arguments_until_requested() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([streaming_ask_stalls()]).unwrap();
    provider(&setup, &server);
    let mut run = terminal(&setup, 160, 48, &[], &[]);
    // A 160x48 grid, as the ticket's screen: every frame draws at the
    // ticket's width.
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The loop pushes kitty's flags only after it processes the harness's
    // reply, so this also keeps the responder's pty write ahead of input.
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    run.write(b"run it\r");
    // The call is still streaming its arguments, so the group line shows
    // the raw text; the scripted test above shows it gone once requested.
    run.wait_screen("the raw arguments", |grid| {
        grid.contents.contains("{\"questions\"")
    });
    // The turn stalls mid-arguments: Esc interrupts it, as the stalled
    // turn test interrupts its stalled reply. The kitty-push wait above
    // ensures the harness finished its reply before this pty write.
    run.write(b"\x1b[27u");
    run.wait_screen("the interrupted turn", |grid| {
        grid.alternate_screen && grid.contents.contains("interrupted")
    });
    // As above: quitting either exits at once or asks first.
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn a_repository_offer_swaps_in_and_approve_lets_the_turn_run() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    provider(&setup, &server);
    // The workspace's repository declares one MCP server nobody approved.
    write_json(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"mcp": {"servers": {"db": {"command": "/bin/echo"}}}}),
    );
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"say hi\r");
    // The offer names the TUI files it installs; the grid reassembles
    // the line however the terminal wraps it. The drawn offer is the
    // terminal's own view, so its keys follow the grid.
    run.wait_screen("the repository offer", |grid| {
        grid.contents.contains("installed")
    });
    // From skip, ← chooses approve; ↓ moves to Send, and Enter sends.
    run.write(b"\x1b[D");
    run.write(b"\x1b[B");
    run.write(b"\r");
    // The turn runs only once the offer resolves, so the answer shows the
    // reply was accepted and the session counted this terminal first.
    // The reply streams in two deltas; the turn's close says it finished.
    run.wait_screen("the first delta", |grid| grid.contents.contains("Hel"));
    run.wait_screen("the finished turn", |grid| {
        grid.contents.contains("completed")
    });
    // As above: quitting either exits at once or asks first.
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn resize_redraws_the_grid_at_the_new_size() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    provider(&setup, &server);
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The footer's last word proves the last row drew before the resize.
    run.wait_screen("the footer", |grid| grid.contents.contains("quit"));
    run.resize(40, 10);
    // The redrawn home at 40 by 10: the input line sits on row 4
    // with the cursor parked on it, and the footer hint closes row 9.
    // Only a redraw at the new size lays the frame out this way.
    run.wait_screen("the redrawn grid at the new size", |grid| {
        grid.rows.len() == 10
            && grid
                .rows
                .get(4)
                .is_some_and(|row| row.trim_end() == "› █? for shortcuts")
            && grid.rows.get(9).is_some_and(|row| row.contains("key map"))
            && grid.cursor == (4, 2)
    });
    // The hub `fiber` started is up before the quit, so `wait` sees it
    // idle out rather than start after the home is gone.
    until_socket(
        setup.deadline,
        &setup.hub_socket(),
        true,
        "the hub to start",
    );
    run.write(b"\x03\x03");
    // No session runs, so no resume line follows: the restored primary
    // screen is the assertion.
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn without_a_tty_bare_fiber_names_ask() {
    let deadline = Deadline::start();
    assert_names_ask(deadline, Stdio::piped(), "standard input and output", &[]);
}

#[test]
fn a_tty_on_standard_input_alone_is_not_enough() {
    let deadline = Deadline::start();
    let terminal = support::pty::open(120, 32);
    assert_names_ask(deadline, terminal.stdio(), "standard output", &[]);
}

#[test]
fn resume_and_continue_without_a_tty_name_ask() {
    let deadline = Deadline::start();
    for args in [&["resume", "s_1"][..], &["resume"][..], &["continue"][..]] {
        assert_names_ask(deadline, Stdio::piped(), "standard input and output", args);
        let terminal = support::pty::open(120, 32);
        assert_names_ask(deadline, terminal.stdio(), "standard output", args);
    }
}

#[test]
fn resume_opens_the_session_a_prefix_names() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([reply("Hello.")]).unwrap();
    provider(&setup, &server);
    let asked = ask(&setup, &["ask", "say hi"]);
    assert_eq!(asked.status.code(), Some(0));
    let id = only_session(&setup);
    let mut run = terminal(&setup, 120, 32, &["resume", &id[..4]], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_screen("the earlier reply", |grid| grid.contents.contains("Hello."));
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn continue_opens_the_latest_session() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([reply("First."), reply("Second.")]).unwrap();
    provider(&setup, &server);
    assert_eq!(ask(&setup, &["ask", "first"]).status.code(), Some(0));
    assert_eq!(ask(&setup, &["ask", "second"]).status.code(), Some(0));
    let mut run = terminal(&setup, 120, 32, &["continue"], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_screen("the latest reply", |grid| grid.contents.contains("Second."));
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn resume_without_an_id_opens_home_at_the_session_list() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([reply("Hello.")]).unwrap();
    provider(&setup, &server);
    let asked = ask(&setup, &["ask", "say hi"]);
    assert_eq!(asked.status.code(), Some(0));
    let mut run = terminal(&setup, 120, 32, &["resume"], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The exited session is listed by its first prompt.
    run.wait_screen("the session list", |grid| grid.contents.contains("say hi"));
    // The list is focused, so Enter opens the row: the reply shows.
    run.write(b"\r");
    run.wait_screen("the earlier reply", |grid| grid.contents.contains("Hello."));
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn continue_with_no_session_is_a_usage_error() {
    let setup = support::Setup::new();
    let mut run = terminal(&setup, 120, 32, &["continue"], &[]);
    run.wait_screen("the usage error", |grid| {
        grid.contents
            .contains("No session in this project to continue")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn resume_with_an_unknown_prefix_fails_before_any_frame() {
    let setup = support::Setup::new();
    let mut run = terminal(&setup, 120, 32, &["resume", "s_zzz"], &[]);
    run.wait_screen("the usage error", |grid| {
        grid.contents.contains("no session at")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(1));
}

/// Runs `fiber` with `args`, `stdin` and standard output and error
/// piped: with no tty on `missing`, it exits 2 naming `fiber ask`.
/// Without a tty the binary never draws, so these keep their
/// byte/stderr/exit assertions: there is no screen to assert on.
#[track_caller]
fn assert_names_ask(deadline: Deadline, stdin: Stdio, missing: &str, args: &[&str]) {
    let setup = setup_within(deadline);
    let mut command = setup.fiber(args);
    command
        .current_dir(setup.workspace())
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = support::run_to_exit(
        setup.deadline,
        &format!("`fiber {}`", args.join(" ")),
        command,
    );
    assert_eq!(output.status.code(), Some(2), "no tty on {missing}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "fiber: The terminal needs a tty; run `fiber ask \"<prompt>\"`. Run `fiber --help` for usage.\n"
    );
}

/// Waits under the setup's deadline for exactly one `artifacts/i_*.png`
/// under the session directory, equal byte for byte to [`PIXEL`].
fn stored_pixel(setup: &support::Setup) {
    let sessions = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    loop {
        let mut found = Vec::new();
        if let Ok(entries) = fs::read_dir(&sessions) {
            for entry in entries.flatten() {
                if let Ok(files) = fs::read_dir(entry.path().join("artifacts")) {
                    found.extend(files.flatten().map(|file| file.path()).filter(|path| {
                        path.file_name()
                            .and_then(OsStr::to_str)
                            .is_some_and(|name| name.starts_with("i_") && name.ends_with(".png"))
                    }));
                }
            }
        }
        if found.len() == 1 && fs::read(&found[0]).unwrap_or_default() == PIXEL {
            return;
        }
        if setup.deadline.left().is_zero() {
            panic!("waited until the deadline for one stored pixel; found: {found:?}");
        }
        thread::yield_now();
    }
}

#[test]
fn ctrl_v_pastes_an_image_that_the_session_stores() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    provider_with_images(&setup, &server);
    // A fake clipboard program first on the child's PATH, printing the
    // pixel in its real program's form.
    let bin = setup.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (name, body) = if cfg!(target_os = "macos") {
        let hex: String = PIXEL.iter().map(|byte| format!("{byte:02X}")).collect();
        (
            "osascript",
            format!("printf '\\302\\253data PNGf{hex}\\302\\273\\n'"),
        )
    } else {
        let octal: String = PIXEL.iter().map(|byte| format!("\\{byte:03o}")).collect();
        ("wl-paste", format!("printf '{octal}'"))
    };
    fakes::script(&bin, name, &body);
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let mut env: Vec<(&str, &str)> = vec![("PATH", path.to_str().unwrap())];
    if !cfg!(target_os = "macos") {
        env.push(("WAYLAND_DISPLAY", "fiber-test"));
    }
    let mut run = terminal(&setup, 120, 32, &[], &env);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(&[0x16]);
    run.wait_screen("the pasted image", |grid| {
        grid.contents.contains("[Image #1]")
    });
    run.write(b"\r");
    // The reply streams in two deltas; quitting needs the turn finished,
    // which the close says.
    run.wait_screen("the finished turn", |grid| {
        grid.contents.contains("completed")
    });
    stored_pixel(&setup);
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn resume_draws_the_reply_then_its_closed_turn() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([reply("marker reply")]).unwrap();
    provider(&setup, &server);
    let asked = ask(&setup, &["ask", "say hi"]);
    assert_eq!(asked.status.code(), Some(0));
    let id = only_session(&setup);
    let mut run = terminal(&setup, 120, 32, &["resume", &id[..4]], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The turn ended before the attach: the reply draws, then the turn's
    // close, which folds only once `turn_completed` arrives.
    run.wait_screen("the reply", |grid| grid.contents.contains("marker reply"));
    run.wait_screen("the closed turn", |grid| {
        grid.contents.contains("▣ completed")
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// The conversation screen: one cell per column and row.
struct Screen {
    cells: Vec<Vec<char>>,
}

impl Screen {
    /// Pads the grid's rows to `cols` columns: the grid omits trailing
    /// blanks, while the card assertions index by column.
    #[track_caller]
    fn from_rows(rows: Vec<String>, cols: usize) -> Self {
        Self {
            cells: rows
                .iter()
                .map(|row| {
                    let mut cells: Vec<char> = row.chars().collect();
                    cells.resize(cols, ' ');
                    cells
                })
                .collect(),
        }
    }

    /// The panel's text columns, one right-trimmed row per screen row:
    /// the card text at the panel's second column, three narrower than
    /// the panel (`panel.rs`).
    fn panel_rows(&self, panel_x: usize, text: usize) -> Vec<String> {
        self.cells
            .iter()
            .map(|row| {
                row[panel_x + 2..panel_x + 2 + text]
                    .iter()
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// Asserts the Session card's exact text rows in a `panel`-column
    /// panel at the screen's right: every row is at most the card's text
    /// width, a row too long ends in `…`, and a row that fits is whole
    /// (`docs/tui.md`, "The panel").
    fn assert_session_cut(&self, cols: u16, panel: u16, workspace: &str) {
        let (panel_x, text) = (cols as usize - panel as usize, panel as usize - 3);
        let rows = self.panel_rows(panel_x, text);
        // The Session card is the panel's only card: its text rows sit
        // between its top and bottom half-block edges, with no second
        // card after.
        let is_edge = |row: &str, edge: char| !row.is_empty() && row.chars().all(|ch| ch == edge);
        let top = rows
            .iter()
            .position(|row| is_edge(row, '▄'))
            .unwrap_or_else(|| panic!("no Session card top edge at {cols} columns"));
        let bottom = rows
            .iter()
            .skip(top + 1)
            .position(|row| is_edge(row, '▀'))
            .map(|at| at + top + 1)
            .unwrap_or_else(|| panic!("no Session card bottom edge at {cols} columns"));
        assert!(
            !rows[bottom + 1..].iter().any(|row| is_edge(row, '▄')),
            "a second card follows the Session card at {cols} columns"
        );
        let card: Vec<&str> = rows[top + 1..bottom]
            .iter()
            .filter(|row| !row.is_empty())
            .map(String::as_str)
            .collect();
        // The card's one-column right padding is the panel's last
        // column (text starts at the second column and is three
        // narrower than the panel): no text row may hold a glyph there.
        // Edge rows span the card, so only text rows count.
        for (at, cells) in self.cells.iter().enumerate() {
            if at <= top || at >= bottom {
                continue;
            }
            let text_range: String = cells[panel_x + 2..panel_x + 2 + text].iter().collect();
            let trimmed = text_range.trim_end();
            if trimmed.is_empty() || is_edge(trimmed, '▄') || is_edge(trimmed, '▀') {
                continue;
            }
            assert_eq!(
                cells[cols as usize - 1],
                ' ',
                "a card text row reaches the panel's last column at {cols} columns: {trimmed:?}"
            );
        }
        // The speed value tracks elapsed time, so only its shape is
        // pinned: at the floor its digits never reach the cut, so the
        // row is one fixed string; at the ceiling the whole row is
        // `output speed, last reply  N tokens/s`.
        let speed_at = card
            .iter()
            .position(|row| row.starts_with("output speed, last reply  "))
            .unwrap_or_else(|| panic!("no speed row at {cols} columns: {card:?}"));
        let speed = if panel == 30 {
            "output speed, last reply  …".to_owned()
        } else {
            let tail = &card[speed_at]["output speed, last reply  ".len()..];
            let digits = tail.chars().take_while(|ch| ch.is_ascii_digit()).count();
            assert!(
                digits > 0 && tail[digits..] == *" tokens/s",
                "the speed row is not whole at {cols} columns: {:?}",
                card[speed_at]
            );
            format!("output speed, last reply  {} tokens/s", &tail[..digits])
        };
        let mut expected = expected_card(workspace, text, panel);
        expected.insert(speed_at, speed);
        assert_eq!(
            card,
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "Session card rows at {cols} columns"
        );
    }
}

/// The Session card's exact rows at `text` columns, without the speed
/// row: the scripted turn's fixed usage reads `tokens in / out  10 /
/// 3` with 40% cache hits, the context sits at 0% with the handoff
/// marker at 70.0k, the cost is still unknown, and one turn ran
/// (`panel.rs`).
fn expected_card(workspace: &str, text: usize, panel: u16) -> Vec<String> {
    let mut rows = vec![
        format!(
            "directory  {}",
            cut_left_path(workspace, text - "directory  ".len())
        ),
        fit_row("model  fake/m · thinking high", text),
    ];
    // The new context rows: the 17-cell bar with its marker and size,
    // then the handoff rows. The trigger reads 70.0k.
    match panel {
        30 => rows.extend([
            "░░░░░░░░░░░░░░░░░│       13".to_owned(),
            "handoff at 70… 0% of window".to_owned(),
            fit_row("then a summary, fresh context", text),
        ]),
        60 => rows.extend([
            format!("{}│{}13", "░".repeat(17), " ".repeat(37)),
            format!("handoff at 70.0k{}0% of window", " ".repeat(29)),
            "then a summary, fresh context".to_owned(),
        ]),
        _ => panic!("unexpected panel width {panel}"),
    }
    rows.extend([
        fit_row("tokens in / out  10 / 3", text),
        fit_row("cache hits  40%", text),
        fit_row("cost billed  unknown", text),
        fit_row("turns  1", text),
    ]);
    rows
}

/// `panel.rs` `cut_left` over the ASCII workspace path: at most `max`
/// columns, cut from the left with a leading `…`.
fn cut_left_path(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_owned();
    }
    let kept: String = path
        .chars()
        .rev()
        .take(max - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("…{kept}")
}

/// `panel.rs` `fit_rows` for a single-span row: whole when it fits,
/// else cut to `text` columns with `…` last. Every glyph here is one
/// column wide.
fn fit_row(row: &str, text: usize) -> String {
    if row.chars().count() <= text {
        row.to_owned()
    } else {
        row.chars().take(text - 1).collect::<String>() + "…"
    }
}

#[test]
fn session_card_rows_are_cut_with_an_ellipsis() {
    // `docs/tui.md` "Layout": the panel is `tui.panel.width` percent
    // of the screen, kept from 30 to 60 columns. The terminal stays at
    // 160 by 48; 10.0% (16 columns) lands the panel on its 30-column
    // floor and 50.0% (80 columns) on its 60-column ceiling, beside a
    // conversation of at least 84 either way.
    for (panel, share) in [(30u16, 10.0), (60u16, 50.0)] {
        session_card_cut_with_an_ellipsis(panel, share);
    }
}

/// Drives one turn with the panel pinned to `share` percent of a
/// 160-by-48 screen and asserts the Session card's exact rows: a row
/// too long for the card is cut with `…`, a row that fits is whole.
#[track_caller]
fn session_card_cut_with_an_ellipsis(panel: u16, share: f64) {
    let setup = support::Setup::new();
    let server = ProviderServer::start([reply("Hello.")]).unwrap();
    // Thinking levels declared, the default in force, so the card shows
    // the `model fake/m thinking high` row; the reply's fixed usage
    // gives the card its spend, context and speed rows.
    provider_with_panel(&setup, &server, share);
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let mut run = terminal(&setup, 160, 48, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"say hi\r");
    // The reply streams in two deltas; the turn's close says it finished.
    // The updated status (with the turn's usage) can arrive before or
    // after the close line; the card is whole once its spend row draws.
    run.wait_screen("the first delta", |grid| grid.contents.contains("Hel"));
    run.wait_screen("the finished turn", |grid| {
        grid.contents.contains("completed")
    });
    run.wait_screen("the spend row", |grid| grid.contents.contains("cache hits"));
    // The card draws top-down and its bottom edge lands last: wait for
    // the edge at the panel columns before snapshotting, never a torn
    // frame. A conversation border also ends in `▀`, so the check pins
    // the edge to the panel: one blank column, then the edge.
    run.wait_screen("the card drawn whole", |grid| {
        let n = usize::from(panel);
        grid.rows.iter().any(|row| {
            let cells: Vec<char> = row.chars().collect();
            cells.len() == 160
                && cells[160 - n] == ' '
                && cells[160 - n + 1..].iter().all(|&cell| cell == '▀')
        })
    });
    // The card below is the conversation screen before quitting: the
    // primary screen after it holds the resume lines, not the card.
    let card = Screen::from_rows(run.screen().rows, 160);
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    card.assert_session_cut(160, panel, workspace.to_str().unwrap());
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn journey_prompt_answer_approval_resize_quit() {
    let setup = support::Setup::new();
    // Three responses, each held until the test sees its request and lets
    // it go: the answer, the tool call, and the answer after the call.
    let server = ProviderServer::start([reply("Hello."), calls_echo_hi(), reply("Done.")]).unwrap();
    server.hold();
    provider(&setup, &server);
    // A standing ask for this exact command: with the terminal connected
    // the loop asks a person (`docs/permissions.md`, "Headless").
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // Prompt one: the test releases the answer once the server holds its
    // request, never on a timer.
    let first_from = run.output().len();
    run.write(b"say hi\r");
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the first request to reach the server"
    );
    server.release_one();
    run.wait_screen("the answer", |grid| {
        grid.alternate_screen && grid.contents.contains("Hello.")
    });
    // The finished first turn proves the session is idle before the
    // second prompt goes out.
    run.turn_finished(first_from);
    // Prompt two ends in a tool call the rule asks about.
    let asking = run.output().len();
    run.write(b"run it\r");
    assert!(
        server.await_requests(2, setup.deadline.left()),
        "the second request to reach the server"
    );
    server.release_one();
    run.wait_screen("the approval panel", |grid| {
        grid.alternate_screen && grid.contents.contains("asked by a global rule: echo hi")
    });
    run.wait_screen("the approval choices", |grid| {
        grid.contents.contains("allow once")
    });
    // Enter on the first choice allows once; the call runs, the loop sends
    // its output, and the server holds that follow-up request too. The
    // waiting title from before the second prompt proves the turn waits
    // on a person before the choice goes out.
    run.wait_bytes(asking, WAITING_TITLE, "the waiting turn");
    let allowed = run.output().len();
    run.write(b"\r");
    assert!(
        server.await_requests(3, setup.deadline.left()),
        "the follow-up request to reach the server"
    );
    server.release_one();
    // The conversation grid before quitting: the call's answer on the
    // alternate screen. The finished second turn proves the session is
    // idle before the resize goes out.
    run.wait_screen("the answer after the call", |grid| {
        grid.alternate_screen && grid.contents.contains("Done.")
    });
    run.turn_finished(allowed);
    // A resize mid-session: 116 columns keep the panel (its 30-column
    // floor beside the 84-column conversation minimum), so the grid
    // follows to the new size with the conversation still on it.
    run.resize(116, 30);
    // The card's handoff phrase is whole only when redrawn: retained
    // bytes truncated to 116 columns lose its tail. It reads settled
    // turn-1 usage, unlike the card's turn count, which stays stale
    // when `turn_started` lines are missed under load. The parked cursor
    // proves the frame drew to its end; tinted padding means `contains`,
    // never `ends_with`.
    run.wait_screen("the redrawn grid at the new size", |grid| {
        grid.rows.len() == 30
            && grid.rows.iter().any(|row| row.contains("0% of window"))
            && grid.cursor == (28, 4)
    });
    // Quit: the terminal is restored, with one resume line per live
    // session on the primary screen ("On exit").
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn journey_quit_resume_answer_again() {
    let setup = support::Setup::new();
    // Two responses, each held until the test sees its request and lets
    // it go: the first session's answer, then the resumed session's.
    let server = ProviderServer::start([reply("First."), reply("Second.")]).unwrap();
    server.hold();
    provider(&setup, &server);
    // One turn, then quit: the conversation grid holds the answer before
    // the terminal is restored.
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let first_from = run.output().len();
    run.write(b"first\r");
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the first request to reach the server"
    );
    server.release_one();
    run.wait_screen("the answer", |grid| {
        grid.alternate_screen && grid.contents.contains("First.")
    });
    // `/close` stops the session on screen: quitting with it live would
    // leave it running with no terminal, and no list to resume from.
    // The finished turn proves the session is idle before the close
    // goes out.
    run.turn_finished(first_from);
    let id = only_session(&setup);
    run.write(b"/close\r");
    until_socket(
        setup.deadline,
        &setup.home().join("run").join(&id),
        false,
        "the closed session to exit",
    );
    // Nothing live remains, so no resume line follows: the restored
    // primary screen is the assertion.
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    // Resume lists the exited session by its first prompt; Enter opens
    // the row, with the earlier turn on screen.
    let mut run = terminal(&setup, 120, 32, &["resume"], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_screen("the session list", |grid| grid.contents.contains("first"));
    // The drawn list is the terminal's own view, so Enter follows the
    // grid; the attached session's title proves the open before the
    // next prompt goes out.
    let attached = run.output().len();
    run.write(b"\r");
    run.wait_screen("the earlier turn", |grid| {
        grid.alternate_screen && grid.contents.contains("First.")
    });
    run.wait_bytes(attached, TITLE, "the attached session's title");
    // A new prompt on the resumed session is answered.
    run.write(b"second\r");
    assert!(
        server.await_requests(2, setup.deadline.left()),
        "the second request to reach the server"
    );
    server.release_one();
    run.wait_screen("the new answer", |grid| {
        grid.alternate_screen && grid.contents.contains("Second.")
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}
