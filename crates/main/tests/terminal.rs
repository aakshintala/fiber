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

#[path = "support/hub_start.rs"]
mod hub_start;

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Response, fingerprint};
use serde_json::{Value, json};
use support::pty::{FINISHED_TITLE, KITTY_PUSH, TITLE, WAITING_TITLE};
use support::{Deadline, PIXEL, hello, text_reply, write_json};

/// Installs a provider `fake` with model `m` on `openai-responses` at the
/// fake server, makes `fake/m` the configured model, and idles the hub
/// out 200 ms after its last client leaves, so no hub lingers.
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
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
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
    let mut config = json!({"model": "fake/m", "hub": {"idle_exit_ms": 200}});
    if let Some(width) = panel_width {
        config["tui"] = json!({"panel": {"width": width}});
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// Hides the rail for a one-session look: the pinned grid is the
/// conversation's own, not the rail's. Merges into the config the
/// provider helpers wrote.
fn hide_rail(setup: &support::Setup) {
    let path = setup.home().join("config.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["tui"] = json!({"rail": {"shown": false}});
    write_json(&path, &config);
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
    assert!(working_elapsed("Working 0s · Esc to interrupt"));
    assert!(working_elapsed("Working 39s · Esc to interrupt"));
    assert!(working_elapsed("Working 1m 2s · Esc to interrupt"));
    assert!(!working_elapsed("Working · Esc to interrupt"));
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
        !grid.contents.contains("Esc to interrupt")
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
        grid.contents.contains("asked by the global rule echo hi")
    });
    run.wait_screen("the approval choices", |grid| {
        grid.contents.contains("Allow once")
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
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}}),
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
        grid.contents.contains("asked by the project rule echo hi")
    });
    run.wait_screen("the approval choices", |grid| {
        grid.contents.contains("Allow once")
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
    // The approval panel shows the shell's main argument for review, so
    // neither call's JSON ever draws: neither on the group line, in a
    // ledger row, nor on the panel. The grid reassembles wrapped rows,
    // so every drawn row is checked.
    assert!(!rows.iter().any(|row| row.contains("{\"")), "{rows:?}");
    assert!(grid.contents.contains("shell echo hi"), "{rows:?}");
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
fn the_offer_view_shows_an_items_file_after_enter() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    provider(&setup, &server);
    fs::create_dir_all(setup.workspace().join("tools")).unwrap();
    fs::write(
        setup.workspace().join("tools/db.sh"),
        "# OFFEREDSOURCE1237\n",
    )
    .unwrap();
    write_json(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"mcp": {"servers": {"db": {"command": "/bin/echo", "args": ["tools/db.sh"]}}}}),
    );
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"say hi\r");
    run.wait_screen("the repository offer", |grid| {
        grid.contents.contains("installed")
    });
    run.write(b"\r");
    run.wait_screen("the offered file's source", |grid| {
        grid.contents.contains("OFFEREDSOURCE1237")
    });
    run.write(b"\x1b[27u");
    run.wait_screen("the offer list again", |grid| {
        grid.contents.contains("installed")
    });
    run.write(b"\x1b[D");
    run.write(b"\x1b[B");
    run.write(b"\r");
    run.wait_screen("the first delta", |grid| grid.contents.contains("Hel"));
    run.wait_screen("the finished turn", |grid| {
        grid.contents.contains("completed")
    });
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
    // The footer's first words prove the last row drew before the resize.
    run.wait_screen("the footer", |grid| {
        grid.contents.contains("Enter opens the focused session")
    });
    run.resize(40, 10);
    // The redrawn home at 40 by 10: the box moves to the bottom, with
    // the input line on row 5 and the footer hint on row 9. Only a redraw
    // at the new size lays the frame out this way.
    let grid = run.wait_screen("the redrawn grid at the new size", |grid| {
        grid.rows.len() == 10
            && grid
                .rows
                .get(5)
                .is_some_and(|row| row.trim() == "› █? for shortcuts")
            && grid
                .rows
                .get(9)
                .is_some_and(|row| row.contains("Enter opens the focused session"))
    });
    assert_eq!(grid.cursor, (5, 4));
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

/// Two exited sessions on the hub machine in different projects, the
/// second newer: what `continue --hub` must choose between. The asks run
/// before the test-port hub is served, and the hub idles out before the
/// serve, so only one hub holds the home at a time.
fn seed_hub_sessions(a: &support::Setup) {
    let server = ProviderServer::start([reply("First."), reply("Second.")]).unwrap();
    provider(a, &server);
    assert_eq!(ask(a, &["ask", "first"]).status.code(), Some(0));
    let other = a.root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    let mut second = a.fiber(&["ask", "second"]);
    second.current_dir(&other);
    assert_eq!(
        support::run_to_exit(a.deadline, "the second ask", second)
            .status
            .code(),
        Some(0)
    );
    until_socket(a.deadline, &a.hub_socket(), false, "the hub to idle out");
}

/// Serves `a`'s hub on a test port and pairs a fresh client to it as
/// `box`: the client selects `box` by default when `default` holds.
fn paired_client(a: &support::Setup, default: bool) -> (support::Setup, hub_start::Served) {
    let b = support::Setup::new();
    let served = hub_start::serve_installed(
        a,
        hub_start::ATTEMPTS,
        &mut hub_start::free_port,
        &mut |port| {
            write_json(
                &a.home().join("config.json"),
                &json!({"hub": {"port": port}}),
            );
            a.fiber(&["hub", "serve", "--installed"])
        },
    );
    let paired = ask(a, &["hub", "pair", "laptop"]);
    assert_eq!(paired.status.code(), Some(0));
    let code = String::from_utf8(paired.stdout).unwrap();
    let address = format!("ws://127.0.0.1:{}", served.port);
    let input = b.root.path().join("pairing-input");
    fs::write(&input, format!("{}\n", code.trim())).unwrap();
    let mut args = vec!["hub", "add", "box", &address];
    if default {
        args.push("--default");
    }
    let mut command = b.fiber(&args);
    command.stdin(fs::File::open(input).unwrap());
    let added = support::run_to_exit(b.deadline, "hub add with a pairing code", command);
    assert_eq!(added.status.code(), Some(0));
    (b, served)
}

/// Reaps a served hub: the held connection drops and the hub exits on
/// `TERM`, as `hub_list.rs` reaps its served hubs.
fn reap_served(served: hub_start::Served) {
    drop(served.hub);
    served.process.kill("TERM");
    assert_eq!(served.process.wait().code(), Some(143));
}

#[test]
fn continue_hub_opens_the_latest_session_across_projects() {
    let a = support::Setup::new();
    seed_hub_sessions(&a);
    let (b, served) = paired_client(&a, false);
    let mut run = terminal(&b, 120, 32, &["continue", "--hub", "box"], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_screen("the latest reply", |grid| grid.contents.contains("Second."));
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    reap_served(served);
}

#[test]
fn continue_hub_without_a_session_is_a_usage_error() {
    let a = support::Setup::new();
    let (b, served) = paired_client(&a, false);
    let mut run = terminal(&b, 120, 32, &["continue", "--hub", "box"], &[]);
    run.wait_screen("the usage error", |grid| {
        grid.contents.contains("No session on hub `box`")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(2));
    reap_served(served);
}

#[test]
fn continue_without_a_flag_uses_the_configured_hub() {
    let a = support::Setup::new();
    seed_hub_sessions(&a);
    let (b, served) = paired_client(&a, true);
    // No `--hub`: `hub.default=box` selects the listed hub.
    let mut run = terminal(&b, 120, 32, &["continue"], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_screen("the latest reply", |grid| grid.contents.contains("Second."));
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    reap_served(served);
}

#[test]
fn continue_explicit_hub_overrides_the_configured_default() {
    let a = support::Setup::new();
    seed_hub_sessions(&a);
    let (b, served) = paired_client(&a, true);
    // `hub.default=box` is configured, but the explicit flag wins: the
    // unknown hub fails instead of opening the default hub's session.
    let mut run = terminal(&b, 120, 32, &["continue", "--hub", "nope"], &[]);
    run.wait_screen("the usage error", |grid| {
        grid.contents.contains("No hub named `nope`")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(2));
    reap_served(served);
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
    /// the card text two columns past the slab, four narrower than
    /// the panel (`panel.rs`).
    fn panel_rows(&self, panel_x: usize, text: usize) -> Vec<String> {
        self.cells
            .iter()
            .map(|row| {
                row[panel_x + 3..panel_x + 3 + text]
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
        let (panel_x, text) = (cols as usize - panel as usize, panel as usize - 4);
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
        // column (text starts two columns past the slab and is four
        // narrower than the panel): no text row may hold a glyph there.
        // Edge rows span the card, so only text rows count.
        for (at, cells) in self.cells.iter().enumerate() {
            if at <= top || at >= bottom {
                continue;
            }
            let text_range: String = cells[panel_x + 3..panel_x + 3 + text].iter().collect();
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
        // pinned: at the floor the label is cut with `…` while the value
        // stays whole, so the digits stay timing-dependent; at the
        // ceiling the whole row fits, `speed, last reply  N tokens/s`.
        let speed_at = card
            .iter()
            .position(|row| row.contains("tokens/s"))
            .unwrap_or_else(|| panic!("no speed row at {cols} columns: {card:?}"));
        let speed = if panel == 30 {
            let row = card[speed_at];
            assert!(
                row.starts_with("speed, last")
                    && row.contains('\u{2026}')
                    && row.ends_with(" tokens/s"),
                "the floor speed row is not cut mid-label at {cols} columns: {row:?}"
            );
            assert_eq!(
                row.chars().count(),
                text,
                "the floor speed row is not card-wide at {cols} columns: {row:?}"
            );
            row.to_owned()
        } else {
            // The ceiling row fits whole: the label, one column of pad
            // and the value.
            let row = card[speed_at];
            let label = "speed, last reply";
            let value = row[label.len()..].trim_start();
            let digits = value.chars().take_while(|ch| ch.is_ascii_digit()).count();
            assert!(
                row.starts_with(label) && digits > 0 && value[digits..] == *" tokens/s",
                "the speed row is not whole at {cols} columns: {row:?}"
            );
            assert_eq!(
                row.chars().count(),
                text,
                "the speed row is not card-wide at {cols} columns: {row:?}"
            );
            row.to_owned()
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
/// row: the title names the working directory with `auto` right-aligned,
/// the model reads `fake/m` with thinking `high` right-aligned, the
/// context bar is 17 cells of `▆` with its marker and size, then the
/// handoff rows; the scripted turn's fixed usage reads
/// `tokens in / out  10 / 3` with 40% cache hits, the context sits at 0%
/// with the handoff marker at 70.0k, the cost is still unknown, and one
/// turn ran (`panel.rs`).
fn expected_card(workspace: &str, text: usize, _panel: u16) -> Vec<String> {
    // The title: the directory cut from the left with `auto`
    // right-aligned.
    let room = text - "auto".len() - 1;
    let name = cut_left_path(workspace, room);
    let pad = text - name.chars().count() - "auto".len();
    let mut rows = vec![
        format!("{name}{}auto", " ".repeat(pad)),
        format!(
            "{}{}high",
            "fake/m",
            " ".repeat(text - "fake/m".len() - "high".len())
        ),
    ];
    // The context rows: the 17-cell bar with its marker and size, then
    // the handoff rows. The trigger reads 70.0k.
    let bar_pad = text - 17 - 1 - "13".len();
    rows.push(format!(
        "{}│{}13",
        "\u{2586}".repeat(17),
        " ".repeat(bar_pad)
    ));
    let handoff = "handoff at 70.0k";
    let share = "0% of window";
    if handoff.len() + 1 + share.len() <= text {
        rows.push(format!(
            "{handoff}{}0% of window",
            " ".repeat(text - handoff.len() - share.len())
        ));
    } else {
        let room = text - share.len() - 1;
        let kept: String = handoff.chars().take(room - 1).collect();
        rows.push(format!("{kept}… {share}"));
    }
    rows.push(fit_row("then a summary, fresh context", text));
    // Every figure: its dim label with the value right-aligned.
    let figure = |label: &str, value: &str| {
        format!(
            "{label}{}{value}",
            " ".repeat(text - label.len() - value.len())
        )
    };
    rows.extend([
        figure("tokens in / out", "10 / 3"),
        figure("cache hits", "40%"),
        figure("cost", "unknown"),
        figure("cost on subscription", "$0.00"),
        figure("turns", "1"),
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
    run.wait_screen("the final usage rows", |grid| {
        grid.contents.contains("10 / 3") && grid.contents.contains("40%")
    });
    // The card draws top-down, its bottom edge lands last, and the `cost`
    // row lands later still (`crates/tui/src/view/panel.rs`, `session_rows`):
    // wait for the edge at the panel columns and the whole-word `cost` label
    // before snapshotting, never a torn frame. A conversation border also
    // ends in `▀`, so the check pins the edge to the panel: one blank column,
    // then the edge.
    run.wait_screen("the card drawn whole", |grid| {
        let n = usize::from(panel);
        let (width, _) = grid.size();
        let Some(left) = width.checked_sub(panel) else {
            return false;
        };
        support::session_cost_drawn(grid, left)
            && grid.rows.iter().any(|row| {
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
    hide_rail(&setup);
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
        server.await_requests(1, setup.deadline),
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
        server.await_requests(2, setup.deadline),
        "the second request to reach the server"
    );
    server.release_one();
    run.wait_screen("the approval panel", |grid| {
        grid.alternate_screen && grid.contents.contains("asked by the global rule echo hi")
    });
    run.wait_screen("the approval choices", |grid| {
        grid.contents.contains("Allow once")
    });
    // Enter on the first choice allows once; the call runs, the loop sends
    // its output, and the server holds that follow-up request too. The
    // waiting title from before the second prompt proves the turn waits
    // on a person before the choice goes out.
    run.wait_bytes(asking, WAITING_TITLE, "the waiting turn");
    let allowed = run.output().len();
    run.write(b"\r");
    assert!(
        server.await_requests(3, setup.deadline),
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
            && grid.cursor == (28, 5)
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
        server.await_requests(1, setup.deadline),
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
        server.await_requests(2, setup.deadline),
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

#[test]
fn journey_quit_just_resumed_session_prints_its_resume_line() {
    let setup = support::Setup::new();
    // One held response: the first session's answer. The resume half of
    // this journey sends no prompt, so it needs no second response.
    let server = ProviderServer::start([reply("First.")]).unwrap();
    server.hold();
    provider(&setup, &server);
    // One turn, then `/close`: the conversation grid holds the answer
    // before the terminal is restored, as in
    // `journey_quit_resume_answer_again`.
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let first_from = run.output().len();
    run.write(b"first\r");
    assert!(
        server.await_requests(1, setup.deadline),
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
    // Resume lists the exited session; Enter opens it, with the quit keys
    // in the same write so they are already queued while the hub has yet
    // to answer the open's subscribe. One write is best-effort, not a
    // forced order: the tty reader's single read can split it, and the
    // loop never blocks for the rest, so the feed's live update may still
    // land between Enter and the quit keys. Either way the line follows:
    // from the attached session while its row still reads exited, or from
    // the live row once the update lands. This journey is wiring coverage;
    // the stale exited row itself is pinned at crate level, where the
    // order is forced by construction
    // (`the_attached_session_with_an_exited_row_gets_its_line`, #2129).
    let mut run = terminal(&setup, 120, 32, &["resume"], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_screen("the session list", |grid| grid.contents.contains("first"));
    run.write(b"\r\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    // The lines print after the terminal is restored, so a grid
    // snapshot at restore can precede them: the assertion reads the
    // drained output, which holds every byte through end of file.
    let terminal = String::from_utf8_lossy(&output.terminal);
    let line = format!("fiber resume {id}");
    assert!(
        terminal.contains(&line),
        "quitting a just-resumed session prints its resume line; terminal:\n{terminal}"
    );
}

// Binary-level tests of `fiber resume --credential <label>`
// (`docs/model-routing.md`, "Which credential a session uses"): the
// real binary in a pseudo-terminal resumes a session onto another
// stored credential label, through the hub, with a fake provider.
// Every request fingerprint is asserted after its signaled
// request/turn; no test sleeps or reads the real clock.

/// Stores `stored` as `credentials/fake/<label>`, each holding
/// `<label>-key`, and names `label` as the fake provider's credential,
/// keeping the hub's test idle exit so no hub lingers. Credential and
/// config setup always lands before any config snapshot.
fn labels(setup: &support::Setup, label: &str, stored: &[&str]) {
    for name in stored {
        config::store_credential(
            &setup.home(),
            "fake",
            name,
            &config::Secret::new(format!("{name}-key\n")),
        )
        .unwrap();
    }
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "providers": {"fake": {"credential": label}}, "hub": {"idle_exit_ms": 200}}),
    );
}

/// The recorded credential header equals the fingerprint of `value`
/// (`docs/testing.md`, "Model calls"): secrets never cross the log.
fn assert_fingerprint(request: &fakes::Request, value: &str) {
    assert_eq!(
        request.header("authorization"),
        Some(fingerprint(value).as_str()),
        "authorization"
    );
}

/// One config file's bytes, or its absence when the file is not there.
#[derive(PartialEq, Eq, Debug)]
struct ConfigSnapshot {
    global: Option<Vec<u8>>,
    project: Option<Vec<u8>>,
    repository: Option<Vec<u8>>,
}

fn read_optional(path: &Path) -> Option<Vec<u8>> {
    fs::read(path).ok()
}

/// The global, project and repository config bytes: a resume switches
/// a session's credential, never any configuration file.
fn snapshot_config(setup: &support::Setup) -> ConfigSnapshot {
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    ConfigSnapshot {
        global: read_optional(&setup.home().join("config.json")),
        project: read_optional(&setup.home().join("projects").join(key).join("config.json")),
        repository: read_optional(&setup.workspace().join(".fiber/config.json")),
    }
}

#[track_caller]
fn assert_config_unchanged(setup: &support::Setup, before: &ConfigSnapshot) {
    assert_eq!(snapshot_config(setup), *before, "a config file changed");
}

/// Reads the session's `events.jsonl` until its kinds are exactly
/// `expected`, in order, or the deadline expires: the screen can show
/// a finished turn on an earlier turn's markers, so `turn_finished`
/// returning never proves the turn's durable events landed (see
/// #2211). A trailing partial line is still arriving and never
/// counts. Returns the last read; the caller asserts its kinds, which
/// fail on expiry before the log is complete.
fn wait_session_events(setup: &support::Setup, id: &str, expected: &[&str]) -> Vec<Value> {
    loop {
        let bytes = events_bytes(setup, id);
        let text = String::from_utf8(bytes).unwrap();
        let events: Vec<Value> = support::complete_lines(&text);
        if support::kinds(&events).as_slice() == expected {
            return events;
        }
        if setup.deadline.left().is_zero() {
            return events;
        }
        thread::yield_now();
    }
}

/// The raw bytes of the session's `events.jsonl`.
fn events_bytes(setup: &support::Setup, id: &str) -> Vec<u8> {
    fs::read(sessions(setup).join(id).join("events.jsonl")).unwrap()
}

/// The kinds of the complete lines in raw log `bytes`, in order: a
/// trailing partial line is still arriving and never counts, and a
/// line that is not JSON reports itself instead of panicking, so a
/// failing whole-log assertion prints kinds, not two byte arrays.
fn kinds_of_bytes(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let complete = text.rfind('\n').map_or("", |end| &text[..=end]);
    complete
        .lines()
        .flat_map(|line| match serde_json::from_str::<Value>(line) {
            Ok(event) if event.get("kind").and_then(Value::as_str).is_some() => {
                support::kinds(&[event])
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            }
            _ => vec![format!("unparsed: {line}")],
        })
        .collect()
}

#[test]
fn log_kind_projection_keeps_complete_lines_and_unparsed_diagnostics() {
    let events = support::expected(&[
        "clients",
        "fiber_started",
        "session_status",
        "attention",
        "date_changed",
        "fiber_exited",
    ]);
    // The set-aside list is fixed (`docs/testing.md`, "Event streams"):
    // `clients`, `session_status`, `attention` and `date_changed` never
    // reach the ordered projection, whatever positions they held.
    assert_eq!(support::kinds(&events), ["fiber_started", "fiber_exited"]);
    let mut bytes = events
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    bytes.push_str("not json\n{\"kind\":\"partial");
    assert_eq!(
        kinds_of_bytes(bytes.as_bytes()),
        ["fiber_started", "fiber_exited", "unparsed: not json"]
    );
}

/// The kinds of the session every credential-resume test starts from:
/// one open and one finished turn. An exited seed appends the ask
/// process's `fiber_exited`; a live terminal's session has none yet.
/// A resumed session never repeats `session_started` or
/// `opening_message`.
fn opened_kinds(exited: bool) -> Vec<&'static str> {
    let mut kinds = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ];
    if exited {
        kinds.push("fiber_exited");
    }
    kinds
}

/// The kinds of one typed turn after the resume, through this file's
/// fake provider: the `hello` reply streams whole, so there are no
/// deltas.
fn resumed_turn_kinds() -> Vec<&'static str> {
    vec![
        "turn_started",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]
}

/// Whether row `y` of `grid` carries any reversed cell: the focus look.
fn has_focus_look(grid: &support::pty::Grid, y: u16) -> bool {
    let (cols, _) = grid.size();
    (0..cols).any(|x| grid.cell(x, y).inverse)
}

/// Every reversed cell of `grid`, as (column, row): the focus look's
/// snapshot, so a step can wait for it to move.
fn reversed_cells(grid: &support::pty::Grid) -> Vec<(u16, u16)> {
    let (cols, rows) = grid.size();
    let mut cells = Vec::new();
    for y in 0..rows {
        for x in 0..cols {
            if grid.cell(x, y).inverse {
                cells.push((x, y));
            }
        }
    }
    cells
}

/// The walk's step for a notice row's reversed cells, left to right:
/// done when the box itself has focus (its run starts the row past the
/// grip), Up from the dismiss cross's focused cell, Down otherwise. The
/// cross sits at the conversation column's right edge, not the screen's,
/// so any focused cell past the box's start means the cross.
fn notice_step(row: &[bool]) -> Option<&'static [u8]> {
    if row.get(1).is_some_and(|cell| *cell) {
        None
    } else if row.iter().any(|cell| *cell) {
        Some(b"\x1b[A")
    } else {
        Some(b"\x1b[B")
    }
}

#[test]
fn notice_step_heads_back_up_from_the_dismiss_cross() {
    // The box spans its row past the grip: its run starts the row's
    // second cell.
    assert_eq!(notice_step(&[false, true, true]), None);
    // The grip's own cell never ends the walk.
    assert_eq!(
        notice_step(&[true, false, false]),
        Some(b"\x1b[A".as_slice())
    );
    // The cross sits at the conversation column's right edge, about
    // column 90 of a 120-column screen: a focused cell there steps Up,
    // where a last-column check would step Down.
    let mut row = [false; 120];
    row[90] = true;
    assert_eq!(notice_step(&row), Some(b"\x1b[A".as_slice()));
    assert_eq!(notice_step(&[false; 120]), Some(b"\x1b[B".as_slice()));
}

/// Walks keyboard focus to the notice row carrying `code`
/// (conversation door): Shift+Tab enters navigate mode, then each step
/// writes Up from the notice's own dismiss cross and Down otherwise,
/// waiting for the focus look to move each time. Returns only when the
/// row carrying `code` has its second cell reversed, so the box spans
/// the row past the grip and Enter opens it; never writes Enter, so it
/// never dismisses. Every wait goes through `wait_screen` under the
/// run's one deadline.
#[track_caller]
fn focus_notice(run: &mut support::pty::Run, code: &str) {
    run.write(b"\x1b[Z");
    let mut grid = run.wait_screen("a focused row", |grid| {
        let (_, rows) = grid.size();
        (0..rows).any(|y| has_focus_look(grid, y))
    });
    let (_, rows) = grid.size();
    for _ in 0..=rows {
        let y = u16::try_from(
            grid.rows
                .iter()
                .position(|row| row.contains(code))
                .expect("the notice row"),
        )
        .expect("the notice row fits in u16");
        // Done only on the box, so Enter never dismisses.
        let (cols, _) = grid.size();
        let row: Vec<bool> = (0..cols).map(|x| grid.cell(x, y).inverse).collect();
        let Some(key) = notice_step(&row) else {
            return;
        };
        run.write(key);
        let before = reversed_cells(&grid);
        grid = run.wait_screen("the focus moved", |next| reversed_cells(next) != before);
    }
    panic!(
        "the focus walk reached no notice row carrying {code}; screen:\n{}",
        grid.contents
    );
}

/// Spawns `fiber resume <id> --credential <label>` on a terminal.
fn resume_with<'a>(setup: &'a support::Setup, id: &'a str, label: &'a str) -> support::pty::Run {
    terminal(setup, 120, 32, &["resume", id, "--credential", label], &[])
}

/// Quits from a screen with no live session and waits for a clean
/// exit: the primary screen is back and the process ends 0.
#[track_caller]
fn quit_clean(mut run: support::pty::Run) {
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// Quits a terminal attached to a live session: the primary screen
/// carries the session's resume line. The caller reaps the run with
/// `wait` only once every terminal sharing its hub has quit, since
/// each wait holds until the hub exits after its last client leaves.
#[track_caller]
fn quit_keys(run: &mut support::pty::Run) {
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
}

#[track_caller]
fn assert_exited(run: support::pty::Run) {
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// Quits a terminal attached to a live session and waits for a clean
/// exit: the primary screen carries the session's resume line.
#[track_caller]
fn quit_live(mut run: support::pty::Run) {
    quit_keys(&mut run);
    assert_exited(run);
}

#[test]
fn resume_with_a_credential_switches_an_exited_session_to_the_asked_label() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    provider(&setup, &server);
    labels(&setup, "home", &["home", "work"]);
    let before_seed = snapshot_config(&setup);
    let asked = ask(&setup, &["ask", "first"]);
    assert_eq!(asked.status.code(), Some(0));
    assert_fingerprint(&server.requests()[0], "Bearer home-key");
    assert_config_unchanged(&setup, &before_seed);
    let id = only_session(&setup);
    let before_resume = snapshot_config(&setup);
    let mut run = resume_with(&setup, &id, "work");
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The exited turn replays, then its close.
    run.wait_screen("the restored answer", |grid| {
        grid.contents.contains("Hello.")
    });
    run.wait_screen("the closed turn", |grid| {
        grid.contents.contains("completed")
    });
    // A new prompt runs on the asked label.
    let from = run.output().len();
    run.write(b"second\r");
    assert!(
        server.await_requests(2, setup.deadline),
        "the second request to reach the server"
    );
    run.turn_finished(from);
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_fingerprint(&requests[1], "Bearer work-key");
    // One driver-source change, home before and work after, ahead of
    // the resumed startup's preamble on the new label.
    // The complete ordered log: the seeded ask, the resume's startup
    // with the asked label switched ahead of its preamble, then the
    // typed turn. A duplicated, missing or reordered event fails here.
    let mut expected = opened_kinds(true);
    expected.extend([
        "fiber_started",
        "extensions_loaded",
        "model_changed",
        "preamble_built",
    ]);
    expected.extend(resumed_turn_kinds());
    let events = wait_session_events(&setup, &id, &expected);
    assert_eq!(support::kinds(&events), expected);
    let changed: Vec<&Value> = events
        .iter()
        .filter(|line| line["kind"] == "model_changed")
        .collect();
    assert_eq!(changed.len(), 1, "{:?}", support::kinds(&events));
    assert_eq!(changed[0]["payload"]["source"], "driver");
    assert_eq!(changed[0]["payload"]["before"]["credential"], "home");
    assert_eq!(changed[0]["payload"]["after"]["credential"], "work");
    let kinds = support::kinds(&events);
    let changed_at = kinds
        .iter()
        .position(|kind| *kind == "model_changed")
        .unwrap();
    let built_after = events[changed_at..]
        .iter()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built_after["payload"]["credential"], "work");
    assert!(kinds[changed_at..].contains(&"turn_completed"), "{kinds:?}");
    assert_config_unchanged(&setup, &before_resume);
    quit_live(run);
}

#[test]
fn resume_with_a_credential_switches_a_running_session_at_the_next_turn() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello(), text_reply("The second answer.")]).unwrap();
    provider(&setup, &server);
    labels(&setup, "home", &["home", "work"]);
    let before_config = snapshot_config(&setup);
    // The first terminal owns a live session on `home`.
    let mut first = terminal(&setup, 120, 32, &[], &[]);
    first.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    first.ready();
    server.hold();
    let first_from = first.output().len();
    first.write(b"first\r");
    assert!(
        server.await_requests(1, setup.deadline),
        "the first request to reach the server"
    );
    assert_fingerprint(&server.requests()[0], "Bearer home-key");
    let id = only_session(&setup);
    // While that turn is still in flight, a second terminal resumes the
    // running session onto `work`: the open carries the switch, which
    // waits for the turn boundary.
    let mut second = resume_with(&setup, &id, "work");
    second.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    second.ready();
    // The open attaches before the held turn finishes: the attached
    // session's title proves the open, while the turn is still running
    // and its prompt has not reached history yet.
    let attached = second.output().len();
    second.wait_bytes(attached, TITLE, "the attached session's title");
    // The held turn finishes on the old key; the switch is queued ahead
    // of any later prompt on the opening connection.
    server.release_one();
    first.turn_finished(first_from);
    assert_eq!(server.requests().len(), 1, "no second request yet");
    // The first turn's title reaches the second terminal as well:
    // only past it does an offset prove the next turn finished (see
    // #2211).
    second.wait_bytes(
        attached,
        FINISHED_TITLE,
        "the first turn's title on the second terminal",
    );
    // The next prompt goes out on the new label.
    let second_from = second.output().len();
    second.write(b"second\r");
    assert!(
        server.await_requests(2, setup.deadline),
        "the second request to reach the server"
    );
    server.release_one();
    // Turn 2's own answer in the grid: the bare word `completed`
    // already shows from turn 1's card on this attached terminal, so
    // the wait names turn 2's distinct reply text.
    second.wait_screen("the second answer", |grid| {
        grid.contents.contains("The second answer.")
    });
    second.turn_finished(second_from);
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_fingerprint(&requests[0], "Bearer home-key");
    assert_fingerprint(&requests[1], "Bearer work-key");
    // Exactly one driver-source change, home before and work after; the
    // running open started no new session process.
    // The complete ordered log: the live session's first turn, the
    // switch with the cache rebuilt ahead of the next turn, then that
    // turn. A duplicated, missing or reordered event fails here.
    let mut expected = opened_kinds(false);
    expected.extend(["model_changed", "preamble_built"]);
    expected.extend(resumed_turn_kinds());
    let events = wait_session_events(&setup, &id, &expected);
    assert_eq!(support::kinds(&events), expected);
    let kinds = support::kinds(&events);
    let changed: Vec<&Value> = events
        .iter()
        .filter(|line| line["kind"] == "model_changed")
        .collect();
    assert_eq!(changed.len(), 1, "{kinds:?}");
    assert_eq!(changed[0]["payload"]["source"], "driver");
    assert_eq!(changed[0]["payload"]["before"]["credential"], "home");
    assert_eq!(changed[0]["payload"]["after"]["credential"], "work");
    assert_eq!(
        kinds
            .iter()
            .copied()
            .filter(|kind| *kind == "session_started")
            .count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds
            .iter()
            .copied()
            .filter(|kind| *kind == "fiber_started")
            .count(),
        1,
        "the running open starts no new session process: {kinds:?}"
    );
    assert_config_unchanged(&setup, &before_config);
    // Both terminals leave before either is reaped: each wait holds
    // for the hub's exit, which waits for the last client to leave.
    quit_keys(&mut second);
    quit_keys(&mut first);
    assert_exited(second);
    assert_exited(first);
}

/// Repro for #2211: the second terminal attaches while turn 1 still
/// runs, so an offset taken before turn 1's markers reach it matches
/// turn 1, not turn 2. Turn 2's reply stays held through the
/// `turn_finished` wait, which returns on turn 1's stale paint and
/// title while turn 2 still streams; the reply is released only after
/// that, and the log is read under the deadline, so the full ordered
/// log lands before the assertion.
#[test]
fn resume_with_a_credential_second_turn_outlives_a_stale_completed_paint() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    provider(&setup, &server);
    labels(&setup, "home", &["home", "work"]);
    let before_config = snapshot_config(&setup);
    let mut first = terminal(&setup, 120, 32, &[], &[]);
    first.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    first.ready();
    server.hold();
    let first_from = first.output().len();
    first.write(b"first\r");
    assert!(
        server.await_requests(1, setup.deadline),
        "the first request to reach the server"
    );
    let id = only_session(&setup);
    let mut second = resume_with(&setup, &id, "work");
    second.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    second.ready();
    let attached = second.output().len();
    second.wait_bytes(attached, TITLE, "the attached session's title");
    // Taken while turn 1 is still held, so before any completion
    // marker reaches this terminal: it matches turn 1's paint and
    // title, not turn 2's.
    let second_from = second.output().len();
    server.release_one();
    first.turn_finished(first_from);
    second.write(b"second\r");
    assert!(
        server.await_requests(2, setup.deadline),
        "the second request to reach the server"
    );
    // Turn 2's reply stays held: nothing of turn 2 can arrive, so this
    // wait can only match turn 1's stale paint and title from the
    // early offset, and returns while turn 2 still streams. The reply
    // is released only after that demonstration, and the log is read
    // under the deadline, so turn 2's tail lands before the assertion.
    second.turn_finished(second_from);
    server.release_one();
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_fingerprint(&requests[0], "Bearer home-key");
    assert_fingerprint(&requests[1], "Bearer work-key");
    // The complete ordered log, read under the deadline: `turn_finished`
    // returning above never proves turn 2's durable events landed.
    let mut expected = opened_kinds(false);
    expected.extend(["model_changed", "preamble_built"]);
    expected.extend(resumed_turn_kinds());
    let events = wait_session_events(&setup, &id, &expected);
    assert_eq!(support::kinds(&events), expected);
    assert_config_unchanged(&setup, &before_config);
    quit_keys(&mut second);
    quit_keys(&mut first);
    assert_exited(second);
    assert_exited(first);
}

#[test]
fn resume_with_a_credential_after_the_recorded_key_was_deleted() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    provider(&setup, &server);
    labels(&setup, "home", &["home", "work"]);
    let asked = ask(&setup, &["ask", "first"]);
    assert_eq!(asked.status.code(), Some(0));
    let id = only_session(&setup);
    // The recorded label's key is gone; only the asked label's remains.
    // Startup must use the asked label before the recorded one, not
    // switch late: resolving `home` first would refuse the resume.
    fs::remove_file(setup.home().join("credentials/fake/home")).unwrap();
    let before_resume = snapshot_config(&setup);
    let mut run = resume_with(&setup, &id, "work");
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_screen("the restored answer", |grid| {
        grid.contents.contains("Hello.")
    });
    let from = run.output().len();
    run.write(b"second\r");
    assert!(
        server.await_requests(2, setup.deadline),
        "the second request to reach the server"
    );
    run.turn_finished(from);
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_fingerprint(&requests[1], "Bearer work-key");
    // The complete ordered log, as for an exited switch onto the asked
    // label: the asked label wins before the recorded one is read, so
    // the log matches a switch, not a late refusal. A duplicated,
    // missing or reordered event fails here.
    let mut expected = opened_kinds(true);
    expected.extend([
        "fiber_started",
        "extensions_loaded",
        "model_changed",
        "preamble_built",
    ]);
    expected.extend(resumed_turn_kinds());
    let events = wait_session_events(&setup, &id, &expected);
    assert_eq!(support::kinds(&events), expected);
    let changed: Vec<&Value> = events
        .iter()
        .filter(|line| line["kind"] == "model_changed")
        .collect();
    assert_eq!(changed.len(), 1, "{:?}", support::kinds(&events));
    assert_eq!(changed[0]["payload"]["before"]["credential"], "home");
    assert_eq!(changed[0]["payload"]["after"]["credential"], "work");
    assert_config_unchanged(&setup, &before_resume);
    quit_live(run);
}

#[test]
fn resume_with_an_unknown_credential_on_an_exited_session_stays_home() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    provider(&setup, &server);
    labels(&setup, "home", &["home", "work"]);
    let asked = ask(&setup, &["ask", "first"]);
    assert_eq!(asked.status.code(), Some(0));
    let id = only_session(&setup);
    let before = events_bytes(&setup, &id);
    let before_config = snapshot_config(&setup);
    let mut run = resume_with(&setup, &id, "nope");
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The refused open returns home with the resolver's notice, naming
    // the labels the provider has: the row carries the code, and a
    // click opens the whole text in the overlay (`docs/tui.md`,
    // "Notices").
    let grid = run.wait_screen("the refusal notice", |grid| {
        grid.rows.iter().any(|row| {
            row.contains("credential_missing") && row.contains("has no credential label")
        })
    });
    let row = u16::try_from(
        grid.rows
            .iter()
            .position(|row| row.contains("credential_missing"))
            .expect("the notice row"),
    )
    .expect("the notice row fits in u16");
    // Home draws notice rows directly above the bottom input box. The
    // box's all-▄ top edge gives the row the click must hit.
    let box_top = grid
        .rows
        .iter()
        .position(|line| !line.is_empty() && line.chars().all(|ch| ch == '▄'))
        .expect("the input box top edge");
    assert_eq!(
        usize::from(row) + 1,
        box_top,
        "the notice stays directly above the input box"
    );
    run.write(format!("\x1b[<0;3;{}M\x1b[<0;3;{}m", row + 1, row + 1).as_bytes());
    run.wait_screen("the notice overlay", |grid| {
        // The overlay wraps at the width, so each fragment fits on
        // one of its rows: the message's start and its credential
        // path, which no wrap splits.
        grid.contents.contains("`nope`. The labels for `fake` are:")
            && grid.contents.contains("default, home, work")
    });
    assert_eq!(
        server.requests().len(),
        1,
        "the refused resume sends no new request"
    );
    let after = events_bytes(&setup, &id);
    assert!(
        after == before,
        "the refused resume appends nothing to the log: after kinds {:?}, before kinds {:?}",
        kinds_of_bytes(&after),
        kinds_of_bytes(&before),
    );
    assert_config_unchanged(&setup, &before_config);
    quit_clean(run);
}

#[test]
fn resume_with_an_unknown_credential_on_a_running_session_stays_attached() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    provider(&setup, &server);
    hide_rail(&setup);
    labels(&setup, "home", &["home", "work"]);
    let mut first = terminal(&setup, 120, 32, &[], &[]);
    first.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    first.ready();
    let first_from = first.output().len();
    first.write(b"first\r");
    assert!(
        server.await_requests(1, setup.deadline),
        "the first request to reach the server"
    );
    first.turn_finished(first_from);
    let id = only_session(&setup);
    let before = events_bytes(&setup, &id);
    let before_config = snapshot_config(&setup);
    let mut second = resume_with(&setup, &id, "nope");
    second.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    second.ready();
    // The running open is accepted, and the refused switch arrives as a
    // notice naming the labels the provider has: no rejection, no trip
    // home. The row carries the code and the message's start; a click
    // opens the whole text in the overlay (`docs/tui.md`, "Notices").
    second.wait_screen("the refusal notice", |grid| {
        grid.rows.iter().any(|row| {
            row.contains("credential_missing") && row.contains("has no credential label")
        })
    });
    // The attach replays the finished turn, redrawing the stops as
    // its lines arrive: walk only once the replayed reply and its
    // close are drawn, so the stops under the walk stay put.
    second.wait_screen("the replayed turn", |grid| {
        grid.contents.contains("Hello.") && grid.contents.contains("completed")
    });
    // The notice row moves when the layout below it changes, so a
    // read-then-click can land elsewhere: reach it by keyboard focus,
    // which follows the notice by id, then Enter opens it whole in the
    // overlay (`docs/tui.md`, "Notices").
    focus_notice(&mut second, "credential_missing");
    second.write(b"\r");
    second.wait_screen("the notice overlay", |grid| {
        grid.contents.contains("has no credential label")
            && grid.contents.contains("`nope`. The labels for `fake` are:")
            && grid.contents.contains("default, home, work")
    });
    // Back to the input box: opening the notice keeps its focus, so
    // typing would stay in navigate mode. The first Esc closes the
    // overlay with focus staying on the notice, the second leaves
    // navigate mode, which clears the focus look. Esc is kitty's
    // `CSI 27 u`: a lone ESC starts a sequence and is held.
    second.write(b"\x1b[27u");
    second.wait_screen("the closed notice overlay", |grid| {
        !grid.contents.contains("`nope`. The labels for `fake` are:")
    });
    second.write(b"\x1b[27u");
    second.wait_screen("the input box back", |grid| reversed_cells(grid).is_empty());
    // Nothing ran on its own: no new request, no turn, no change event.
    assert_eq!(
        server.requests().len(),
        1,
        "the refused switch starts no request"
    );
    assert_eq!(
        events_bytes(&setup, &id),
        before,
        "the refused switch appends nothing to the log"
    );
    // The old label still serves: an explicit prompt goes out on it.
    let second_from = second.output().len();
    second.write(b"second\r");
    assert!(
        server.await_requests(2, setup.deadline),
        "the second request to reach the server"
    );
    second.turn_finished(second_from);
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_fingerprint(&requests[1], "Bearer home-key");
    // The complete ordered log: the live session's two turns with no
    // switch between them. A duplicated, missing or reordered event
    // fails here.
    let mut expected = opened_kinds(false);
    expected.extend(resumed_turn_kinds());
    let events = wait_session_events(&setup, &id, &expected);
    assert_eq!(support::kinds(&events), expected);
    assert!(
        events.iter().all(|line| line["kind"] != "model_changed"),
        "no switch was recorded: {:?}",
        support::kinds(&events)
    );
    assert_config_unchanged(&setup, &before_config);
    // Both terminals leave before either is reaped: each wait holds
    // for the hub's exit, which waits for the last client to leave.
    quit_keys(&mut second);
    quit_keys(&mut first);
    assert_exited(second);
    assert_exited(first);
}

#[test]
fn resume_with_the_recorded_credential_records_nothing() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    provider(&setup, &server);
    labels(&setup, "home", &["home", "work"]);
    let asked = ask(&setup, &["ask", "first"]);
    assert_eq!(asked.status.code(), Some(0));
    let id = only_session(&setup);
    let before_config = snapshot_config(&setup);
    // Asking for the label the session already has is a no-op: the
    // turn still runs, on the same key, with no change recorded.
    let mut run = resume_with(&setup, &id, "home");
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_screen("the restored answer", |grid| {
        grid.contents.contains("Hello.")
    });
    let from = run.output().len();
    run.write(b"second\r");
    assert!(
        server.await_requests(2, setup.deadline),
        "the second request to reach the server"
    );
    run.turn_finished(from);
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_fingerprint(&requests[1], "Bearer home-key");
    // The complete ordered log: the seeded ask, a label-free resume
    // startup, then the typed turn. A duplicated, missing or reordered
    // event fails here.
    let mut expected = opened_kinds(true);
    expected.extend(["fiber_started", "extensions_loaded", "preamble_built"]);
    expected.extend(resumed_turn_kinds());
    let events = wait_session_events(&setup, &id, &expected);
    assert_eq!(support::kinds(&events), expected);
    assert!(
        events.iter().all(|line| line["kind"] != "model_changed"),
        "the same label records no change: {:?}",
        support::kinds(&events)
    );
    assert_config_unchanged(&setup, &before_config);
    quit_live(run);
}

#[test]
fn resume_with_the_current_credential_on_a_running_session_records_nothing() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    provider(&setup, &server);
    labels(&setup, "home", &["home", "work"]);
    let mut first = terminal(&setup, 120, 32, &[], &[]);
    first.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    first.ready();
    let first_from = first.output().len();
    first.write(b"first\r");
    assert!(
        server.await_requests(1, setup.deadline),
        "the first request to reach the server"
    );
    first.turn_finished(first_from);
    let id = only_session(&setup);
    let before_config = snapshot_config(&setup);
    // The second terminal opens the running session on the label it
    // already has: the conversation is there, and no change is queued.
    let mut second = resume_with(&setup, &id, "home");
    second.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    second.ready();
    second.wait_screen("the shared answer", |grid| grid.contents.contains("Hello."));
    let second_from = second.output().len();
    second.write(b"second\r");
    assert!(
        server.await_requests(2, setup.deadline),
        "the second request to reach the server"
    );
    second.turn_finished(second_from);
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_fingerprint(&requests[1], "Bearer home-key");
    // The complete ordered log: the live session's two turns with no
    // switch between them. A duplicated, missing or reordered event
    // fails here.
    let mut expected = opened_kinds(false);
    expected.extend(resumed_turn_kinds());
    let events = wait_session_events(&setup, &id, &expected);
    assert_eq!(support::kinds(&events), expected);
    assert!(
        events.iter().all(|line| line["kind"] != "model_changed"),
        "the same label records no change: {:?}",
        support::kinds(&events)
    );
    assert_config_unchanged(&setup, &before_config);
    // Both terminals leave before either is reaped: each wait holds
    // for the hub's exit, which waits for the last client to leave.
    quit_keys(&mut second);
    quit_keys(&mut first);
    assert_exited(second);
    assert_exited(first);
}

#[test]
fn resume_credential_without_an_id_is_a_usage_error() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    provider(&setup, &server);
    labels(&setup, "home", &["home", "work"]);
    let before = snapshot_config(&setup);
    // No tty: a bare `fiber resume --credential` fails at parse time,
    // before any terminal, hub or session exists.
    let mut command = setup.fiber(&["resume", "--credential", "work"]);
    command.current_dir(setup.workspace());
    let output = support::run_to_exit(setup.deadline, "`fiber resume --credential work`", command);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("<id>"),
        "the error names the missing id: {stderr}"
    );
    assert_config_unchanged(&setup, &before);
    let sessions = sessions(&setup);
    let live: Vec<String> = fs::read_dir(&sessions)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(live.is_empty(), "no session log was created: {live:?}");
}

#[test]
fn sigterm_takes_the_normal_exit_path_with_143() {
    signal_takes_the_normal_exit_path("TERM", 143);
}

#[test]
fn sighup_takes_the_normal_exit_path_with_129() {
    signal_takes_the_normal_exit_path("HUP", 129);
}

/// Sends `signal` to a terminal with one live session: the terminal
/// takes its normal exit path, restoring the screen and printing one
/// resume line for the live session, then exits with `code`
/// (`docs/tui.md`, "On exit"; `docs/invocation.md`, "Three signals, one
/// path"). The terminal's guard exits at once today, skipping the
/// restore and the lines, so the byte assertions fail until the fix.
#[track_caller]
fn signal_takes_the_normal_exit_path(signal: &'static str, code: i32) {
    let setup = support::Setup::new();
    let server = ProviderServer::start([reply("Hello.")]).unwrap();
    provider(&setup, &server);
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let from = run.output().len();
    run.write(b"say hi\r");
    // The reply streams in two deltas; the turn's close says it finished
    // and the session is live, so the signal below lands with one
    // session to resume.
    run.wait_screen("the answer", |grid| grid.contents.contains("Hel"));
    run.turn_finished(from);
    let id = only_session(&setup);
    fakes::kill_pid(run.id(), signal).unwrap();
    let output = run.wait();
    assert_eq!(
        output.status.code(),
        Some(code),
        "a {signal} signal exits {code}"
    );
    // The drained output holds every byte through end of file: leaving
    // the alternate screen proves the restore ran, and the resume line
    // proves the exit lines printed after it.
    let terminal = String::from_utf8_lossy(&output.terminal);
    assert!(
        terminal.contains("\x1b[?1049l"),
        "a {signal} signal restores the terminal; terminal:\n{terminal}"
    );
    let line = format!("fiber resume {id}");
    assert!(
        terminal.contains(&line),
        "a {signal} signal prints the live session's resume line; terminal:\n{terminal}"
    );
}

/// Installs a provider `fake` with the models `m` and `n` on
/// `openai-responses` at the fake server, reading its key from
/// `FIBER_TEST_FAKE_KEY`, and makes `fake/m` the configured model: `m`
/// declares the thinking levels low, medium and high, defaulting to
/// high, so the Session card opens on `fake/m` with `high`. A second
/// provider `other` carries `om`, whose key comes from a command that
/// exits at once, so choosing it rejects while the turn runs (`docs/model-routing.md`,
/// "When a credential is missing or fails"). The hub idles out a second
/// after its last client leaves, so no hub lingers.
fn provider_thinking_pair(setup: &support::Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [
                {"id": "m", "protocol": "openai-responses",
                 "base_url": format!("{}/v1", server.url()), "context_window": 100000,
                 "thinking_levels": ["low", "medium", "high"], "thinking_default": "high"},
                {"id": "n", "protocol": "openai-responses",
                 "base_url": format!("{}/v1", server.url()), "context_window": 100000},
            ],
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
    let other = setup.root.path().join("src-other");
    write_json(
        &other.join("extension.json"),
        &json!({"name": "other", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"other": [format!("{}/v1", server.url())]}}),
    );
    write_json(
        &other.join("providers/other.json"),
        &json!({
            "name": "other",
            "credential": {"command": ["sh", "-c", "exit 1"]},
            "models": [{"id": "om", "protocol": "openai-responses",
                        "base_url": format!("{}/v1", server.url()), "context_window": 100000}],
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(other),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
}

/// Whether any grid row holds `needle`.
fn shows(grid: &support::pty::Grid, needle: &str) -> bool {
    grid.rows.iter().any(|row| row.contains(needle))
}

/// The session log's `model_changed` lines for a stall report, so a
/// picker stall says whether a switch already landed. Reads under
/// construction are absent, never failures: a missing session dir or
/// log reads as empty.
fn session_model_changed(setup: &support::Setup) -> String {
    let sessions = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    let mut switches = Vec::new();
    if let Ok(dirs) = fs::read_dir(&sessions) {
        for dir in dirs.filter_map(Result::ok) {
            let log = fs::read_to_string(dir.path().join("events.jsonl")).unwrap_or_default();
            switches.extend(
                support::complete_lines(&log)
                    .into_iter()
                    .filter(|line| line["kind"] == "model_changed"),
            );
        }
    }
    format!("session model_changed: {switches:?}")
}

/// The Session card keeps the old chips through the send: some row still
/// names `fake/m` with `high`, and no row outside the pending notice
/// names `gone`. The pending notice names the new choice too, so a row
/// carrying the notice's code does not count against the card.
#[track_caller]
fn assert_card_stale(grid: &support::pty::Grid, gone: &str) {
    let rows = &grid.rows;
    assert!(
        rows.iter().any(|row| row.contains("fake/m")),
        "the Session card keeps the old model: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("high")),
        "the Session card keeps the old level: {rows:?}"
    );
    for row in rows {
        if row.contains(gone) {
            assert!(
                row.contains("switch_pending"),
                "the Session card shows the unapplied choice {gone}: {row}"
            );
        }
    }
}

/// Whether some row outside the pending notice names `reference`
/// (and, for a level choice, ends with that level): the card applies
/// the pending choice once the turn ends, while the pending notice
/// stands as its record. Rows are full-width, so the card's indent
/// rules out `starts_with`; the pending notice carries its code, which
/// rules it out instead.
fn card_applies(grid: &support::pty::Grid, reference: &str, level: Option<&str>) -> bool {
    grid.rows.iter().any(|row| {
        row.contains(reference)
            && !row.contains("switch_pending")
            && level.is_none_or(|want| row.trim_end().ends_with(want))
    })
}

/// The Session card applies the pending choice once the turn ends: a
/// non-notice row names the new reference, and ends with the new level
/// when the choice names one.
#[track_caller]
fn assert_card_applies(grid: &support::pty::Grid, reference: &str, level: Option<&str>) {
    assert!(
        card_applies(grid, reference, level),
        "the Session card applies the pending choice {reference} {level:?}: {:?}",
        grid.rows
    );
}

/// Interrupts the running turn and waits for the interrupted screen:
/// the turn never finishes on interrupt, and the loop admits the queued
/// choice only after this boundary.
#[track_caller]
fn interrupt_turn(run: &mut support::pty::Run) {
    run.write(b"\x1b[27u");
    run.wait_screen("the interrupted turn", |grid| {
        grid.alternate_screen && grid.contents.contains("interrupted")
    });
}

#[test]
fn choosing_a_model_mid_turn_notices_that_it_applies_when_the_turn_ends() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([stalled()]).unwrap();
    provider_thinking_pair(&setup, &server);
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    // The first frame draws the input line; its title proves the input
    // reader runs before the prompt goes out.
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    run.write(b"say hi\r");
    // The elapsed count proves `turn_started` folded, which opens the
    // turn: the choice below sends while it runs.
    run.wait_screen("the running turn", |grid| working_elapsed(&grid.contents));
    // Ctrl+L opens the picker: the open wait names the picker frame and
    // the count wait names the answered catalogue, so a stall says
    // whether the picker never opened or the catalogue never answered,
    // with the session log saying whether a switch already landed.
    run.write(b"\x0c");
    run.wait_screen_reporting(
        "the open picker",
        |grid| shows(grid, "Type to search"),
        || session_model_changed(&setup),
    );
    // "n" keeps only `fake/n`: the count names one row of the three
    // installed models.
    run.write(b"n");
    run.wait_screen_reporting(
        "the filtered list",
        |grid| shows(grid, "1 of 3 models"),
        || session_model_changed(&setup),
    );
    run.write(b"\r");
    let sent = run.wait_screen_reporting(
        "the pending-choice notice",
        |grid| shows(grid, "fake/n applies when this turn ends"),
        || session_model_changed(&setup),
    );
    // The chips hold the old model through the send; only the notice
    // names the new one.
    assert_card_stale(&sent, "fake/n");
    interrupt_turn(&mut run);
    // The loop admits the queued choice at the post-interrupt boundary
    // and applies it: the card names the new model, with no new
    // notice of its own.
    let applied = run.wait_screen_reporting(
        "the applied choice",
        |grid| card_applies(grid, "fake/n", None),
        || session_model_changed(&setup),
    );
    assert_card_applies(&applied, "fake/n", None);
    quit_live(run);
}

#[test]
fn choosing_a_level_mid_turn_notices_that_it_applies_when_the_turn_ends() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([stalled()]).unwrap();
    provider_thinking_pair(&setup, &server);
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    run.write(b"say hi\r");
    run.wait_screen("the running turn", |grid| working_elapsed(&grid.contents));
    run.write(b"\x0c");
    run.wait_screen_reporting(
        "the open picker",
        |grid| shows(grid, "Type to search"),
        || session_model_changed(&setup),
    );
    // "m" keeps the current model and the other provider's model: the
    // count names two rows of the three installed models.
    run.write(b"m");
    run.wait_screen_reporting(
        "the filtered list",
        |grid| shows(grid, "2 of 3 models"),
        || session_model_changed(&setup),
    );
    // Left moves the focused row's chip from high to medium; Enter
    // chooses the model with its touched level.
    run.write(b"\x1b[D");
    run.write(b"\r");
    let sent = run.wait_screen_reporting(
        "the pending-choice notice",
        |grid| shows(grid, "thinking medium applies when this turn ends"),
        || session_model_changed(&setup),
    );
    // The chips hold the old level through the send; only the notice
    // names the new one.
    assert_card_stale(&sent, "medium");
    interrupt_turn(&mut run);
    // The loop admits the queued choice at the post-interrupt boundary
    // and applies it: the card names the model with the chosen level,
    // with no new notice of its own.
    let applied = run.wait_screen_reporting(
        "the applied choice",
        |grid| card_applies(grid, "fake/m", Some("medium")),
        || session_model_changed(&setup),
    );
    assert_card_applies(&applied, "fake/m", Some("medium"));
    quit_live(run);
}

#[test]
fn choosing_a_model_while_idle_applies_at_once_with_no_pending_notice() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    provider_thinking_pair(&setup, &server);
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    // An idle warm-up turn proves the hub answers before the picker
    // opens, so the choice below sends while idle, not mid-turn.
    let from = run.output().len();
    run.write(b"say hi\r");
    run.wait_screen("the reply", |grid| grid.contents.contains("Hel"));
    run.turn_finished(from);
    run.write(b"\x0c");
    run.wait_screen_reporting(
        "the open picker",
        |grid| shows(grid, "Type to search"),
        || session_model_changed(&setup),
    );
    run.write(b"n");
    run.wait_screen_reporting(
        "the filtered list",
        |grid| shows(grid, "1 of 3 models"),
        || session_model_changed(&setup),
    );
    run.write(b"\r");
    // The idle switch applies at once: the card names the new model with
    // no pending notice on the way there.
    let switched = run.wait_screen_reporting(
        "the session on fake/n",
        |grid| shows(grid, "fake/n") && !shows(grid, "Type to search"),
        || session_model_changed(&setup),
    );
    assert!(
        !shows(&switched, "applies when this turn ends"),
        "no pending notice while idle: {:?}",
        switched.rows
    );
    quit_live(run);
}

#[test]
fn choosing_an_unreadable_model_mid_turn_replaces_the_pending_notice_with_its_rejection() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([stalled()]).unwrap();
    provider_thinking_pair(&setup, &server);
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    run.write(b"say hi\r");
    run.wait_screen("the running turn", |grid| working_elapsed(&grid.contents));
    run.write(b"\x0c");
    run.wait_screen_reporting(
        "the open picker",
        |grid| shows(grid, "Type to search"),
        || session_model_changed(&setup),
    );
    // "om" keeps only `other/om`: the count names one row of the three
    // installed models.
    run.write(b"om");
    run.wait_screen_reporting(
        "the filtered list",
        |grid| shows(grid, "1 of 3 models"),
        || session_model_changed(&setup),
    );
    run.write(b"\r");
    run.wait_screen_reporting(
        "the pending-choice notice",
        |grid| shows(grid, "other/om applies when this turn ends"),
        || session_model_changed(&setup),
    );
    // Esc interrupts the running turn; the loop then admits the queued
    // choice and prepares it, and preparing it runs the key command,
    // which exits at once, so the session rejects with
    // `credential_missing`: "nothing is stored in
    // credentials/other/default, and `sh` failed (exit status: 1)"
    // (`config::Credentials`, "The one precedence"). The notice row
    // cuts the long message short, so the wait names the head that fits
    // on screen. The rejection takes the pending notice's place, and the
    // card keeps the old chips.
    run.write(b"\x1b[27u");
    run.wait_screen("the interrupted turn", |grid| {
        grid.alternate_screen && grid.contents.contains("interrupted")
    });
    let rejected = run.wait_screen_reporting(
        "the rejection notice",
        |grid| shows(grid, "nothing is stored in credentials"),
        || session_model_changed(&setup),
    );
    assert!(
        shows(&rejected, "credential_missing"),
        "the rejection notice names its code: {:?}",
        rejected.rows
    );
    assert!(
        !shows(&rejected, "applies when this turn ends"),
        "the rejection replaces the pending notice: {:?}",
        rejected.rows
    );
    assert_card_stale(&rejected, "other/om");
    quit_live(run);
}

/// Drives the same switch through the picker or its label shortcut.
fn credential_journey(pick: Option<&[u8]>) {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello(), text_reply("The credential answer.")]).unwrap();
    provider(&setup, &server);
    labels(&setup, "home", &["home", "work"]);
    let before = snapshot_config(&setup);
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let first = run.output().len();
    run.write(b"first\r");
    run.turn_finished(first);
    assert_eq!(server.requests().len(), 1);
    assert_fingerprint(&server.requests()[0], "Bearer home-key");
    let id = only_session(&setup);
    match pick {
        Some(key) => {
            run.write(b"/credential\r");
            let grid = run.wait_screen("the credential picker on home", |grid| {
                grid.contents.contains("Credentials")
                    && grid.rows.iter().any(|row| row.contains("home  ● current"))
            });
            assert!(grid.rows.iter().any(|row| row.contains("work")));
            run.write(b"\x1b[B");
            run.write(key);
        }
        None => run.write(b"/credential work\r"),
    }
    run.wait_screen("the cache rebuild notice", |grid| {
        grid.contents.contains("cache_rebuild")
    });
    let second = run.output().len();
    run.write(b"second\r");
    assert!(
        server.await_requests(2, setup.deadline),
        "the second request"
    );
    run.wait_screen("the second answer", |grid| {
        grid.contents.contains("The credential answer.")
    });
    run.turn_finished(second);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_fingerprint(&requests[1], "Bearer work-key");
    let mut expected = opened_kinds(false);
    expected.extend(["model_changed", "preamble_built"]);
    expected.extend(resumed_turn_kinds());
    let events = wait_session_events(&setup, &id, &expected);
    assert_eq!(support::kinds(&events), expected);
    let changed: Vec<_> = events
        .iter()
        .filter(|line| line["kind"] == "model_changed")
        .collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0]["payload"]["before"]["credential"], "home");
    assert_eq!(changed[0]["payload"]["after"]["credential"], "work");
    assert_eq!(changed[0]["payload"]["source"], "driver");
    if pick == Some(b"\x13".as_slice()) {
        assert_config_unchanged(&setup, &before);
    } else {
        let config: Value =
            serde_json::from_slice(&fs::read(setup.home().join("config.json")).unwrap()).unwrap();
        assert_eq!(config["providers"]["fake"]["credential"], "work");
    }
    quit_live(run);
}

#[test]
fn credential_picker_switches_and_saves_the_label() {
    credential_journey(Some(b"\r"));
}

#[test]
fn credential_picker_ctrl_s_switches_this_session_only() {
    credential_journey(Some(b"\x13"));
}

#[test]
fn credential_with_a_label_switches_and_saves() {
    credential_journey(None);
}

/// Installs both credential-taking providers in one extension.
fn credential_provider_pair(setup: &support::Setup, server: &ProviderServer) {
    let source = setup.root.path().join("credential-pair");
    let url = format!("{}/v1", server.url());
    write_json(
        &source.join("extension.json"),
        &json!({"name":"credential-pair", "version":"v0.0.0", "fiber":"0.0.0", "api":1,
        "providers":{"fake":[url], "other":[url]}}),
    );
    for (name, model) in [("fake", "m"), ("other", "n")] {
        write_json(
            &source.join(format!("providers/{name}.json")),
            &json!({"name":name, "credential":{"env":"FIBER_TEST_FAKE_KEY"},
            "models":[{"id":model,"protocol":"openai-responses","base_url":url,"context_window":100000}]}),
        );
    }
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
    for (provider, names) in [("fake", ["home", "work"]), ("other", ["day", "night"])] {
        for name in names {
            config::store_credential(
                &setup.home(),
                provider,
                name,
                &config::Secret::new(format!("{name}-key\n")),
            )
            .unwrap();
        }
    }
    write_json(
        &setup.home().join("config.json"),
        &json!({"model":"fake/m", "providers":{"fake":{"credential":"home"},"other":{"credential":"day"}},"hub":{"idle_exit_ms":1000}}),
    );
}

#[test]
fn credential_after_a_queued_model_switch_saves_to_the_queued_provider() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([
        calls_echo_hi(),
        hello(),
        text_reply("The queued credential answer."),
    ])
    .unwrap();
    credential_provider_pair(&setup, &server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision":"ask", "tool":"shell", "prefix":"echo hi"})
        ),
    )
    .unwrap();
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let first = run.output().len();
    run.write(b"first\r");
    run.wait_screen("the pending approval", |grid| {
        grid.contents.contains("Allow once")
    });
    assert_eq!(server.requests().len(), 1);
    assert_fingerprint(&server.requests()[0], "Bearer home-key");
    let id = only_session(&setup);
    // Put the approval aside without answering it so commands reach the draft.
    run.write(b"\x1b[27u");
    run.wait_screen("the approval put aside", |grid| {
        grid.contents.contains("approvals waiting")
    });
    run.write(b"/model\r");
    run.wait_screen("the model picker", |grid| {
        grid.contents.contains("Type to search")
    });
    run.write(b"n");
    run.wait_screen("the other model", |grid| {
        grid.contents.contains("1 of 2 models")
    });
    run.write(b"\r");
    // The model is written only after its command is accepted. Its exact
    // saved value proves admission while the approval still holds the turn.
    loop {
        let config: Value =
            serde_json::from_slice(&fs::read(setup.home().join("config.json")).unwrap()).unwrap();
        if config["model"] == "other/n" {
            break;
        }
        assert!(
            !setup.deadline.left().is_zero(),
            "the model command's acceptance to save other/n"
        );
        thread::yield_now();
    }
    run.write(b"/credential night\r");
    // Saving this label proves the credential switch was admitted on other,
    // before the approval is released, not merely dispatched on the terminal.
    loop {
        let config: Value =
            serde_json::from_slice(&fs::read(setup.home().join("config.json")).unwrap()).unwrap();
        if config["providers"]["other"]["credential"] == "night" {
            break;
        }
        assert!(
            !setup.deadline.left().is_zero(),
            "the credential command's acceptance to save night"
        );
        thread::yield_now();
    }
    run.write(b"\x1ba");
    run.wait_screen("the approval reopened", |grid| {
        grid.contents.contains("Allow once")
    });
    run.write(b"\r");
    run.turn_finished(first);
    assert!(
        server.await_requests(2, setup.deadline),
        "the first turn's follow-up"
    );
    let second = run.output().len();
    run.write(b"second\r");
    assert!(
        server.await_requests(3, setup.deadline),
        "the second turn's request"
    );
    run.wait_screen("the queued credential answer", |grid| {
        grid.contents.contains("The queued credential answer.")
    });
    run.turn_finished(second);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_fingerprint(&requests[2], "Bearer night-key");
    let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(body["model"], "n");
    let config: Value =
        serde_json::from_slice(&fs::read(setup.home().join("config.json")).unwrap()).unwrap();
    assert_eq!(config["providers"]["fake"]["credential"], "home");
    assert_eq!(config["providers"]["other"]["credential"], "night");
    let mut expected = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "permission_requested",
        "permission_resolved",
        "tool_call_started",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "model_changed",
        "model_changed",
        "preamble_built",
    ];
    expected.extend(resumed_turn_kinds());
    let events = wait_session_events(&setup, &id, &expected);
    assert_eq!(support::kinds(&events), expected);
    let changed: Vec<_> = events
        .iter()
        .filter(|line| line["kind"] == "model_changed")
        .collect();
    assert_eq!(changed.len(), 2);
    assert_eq!(changed[0]["payload"]["before"]["model"], "fake/m");
    assert_eq!(changed[0]["payload"]["before"]["credential"], "home");
    assert_eq!(changed[0]["payload"]["after"]["model"], "other/n");
    assert_eq!(changed[0]["payload"]["after"]["credential"], "day");
    assert_eq!(changed[1]["payload"]["before"]["credential"], "day");
    assert_eq!(changed[1]["payload"]["after"]["credential"], "night");
    for change in changed {
        assert_eq!(change["payload"]["source"], "driver");
    }
    quit_live(run);
}
