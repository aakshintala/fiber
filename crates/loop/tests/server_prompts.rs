//! A `prompt` naming an MCP server's prompt (`docs/mcp.md`, "Prompts and
//! resources"): the fetch runs for a `/name` no skill has, and its text
//! becomes the person's message.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod support;

use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::clock::Wake;
use contract::events::TurnOutcome;
use contract::events::{CommandInfo, McpServerFailed, McpServerReady, ServerFailure};
use contract::inbox::{Ack, Answer, Delivery};
use contract::provider::Input;
use contract::shapes::{ContentPart, Failure};
use contract::tool::{Cancel, Output, ServerRecord};
use fakes::Scripted;

use support::{Session, accepted, kinds, message, rejected};

fn capture() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (Ack(Box::new(move |answer| tx.send(answer).unwrap())), rx)
}

fn take(rx: &mpsc::Receiver<Answer>) -> Answer {
    rx.try_recv().expect("the command was answered")
}

fn plain() -> Vec<&'static str> {
    vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]
}

fn row(name: &str, tag: &str) -> CommandInfo {
    CommandInfo {
        name: name.into(),
        description: "A server's prompt.".into(),
        argument_hint: None,
        tag: tag.into(),
    }
}

fn text_output(text: &str) -> Output {
    Output {
        content: vec![ContentPart::Text { text: text.into() }],
        ..Output::default()
    }
}

/// A fetch recording `(server, prompt, arguments)` and answering `answer`.
fn recording(answer: Output, seen: Seen) -> r#loop::FetchPrompt {
    Arc::new(
        move |server: &str, prompt: &str, args: &str, _cancel: &dyn Cancel| {
            seen.lock()
                .unwrap()
                .push((server.to_owned(), prompt.to_owned(), args.to_owned()));
            answer.clone()
        },
    )
}

/// What the fake fetch saw: `(server, prompt, arguments)` per run.
type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

fn prompted(text: &str) -> (Session, Seen) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let prompts = r#loop::ServerPrompts {
        rows: vec![row("greet", "fx")],
        fetch: recording(text_output(text), Arc::clone(&seen)),
    };
    let session = Session::new(vec![Scripted::text("Hi.")], None).server_prompts(prompts);
    (session, seen)
}

/// Writes the `review-pr` skill into `workspace`'s `.agents/skills/`.
fn review_skill(workspace: &std::path::Path) {
    let dir = workspace.join(".agents/skills/review-pr");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: review-pr\ndescription: Reviews.\n---\nReview body.\n",
    )
    .unwrap();
}

#[test]
fn a_slash_prompt_runs_a_servers_prompt() {
    let (mut session, seen) = prompted("Fetched.");
    session.inbox.send(support::delivery("/greet Ada")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [("fx".to_owned(), "greet".to_owned(), "Ada".to_owned())]
    );
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    let input = &lines[3].payload["input"];
    assert_eq!(input.as_array().unwrap().len(), 1);
    assert_eq!(input[0]["content"][0]["text"], "Fetched.");
    // The provider's last user message carries the fetched text too.
    let expanded = session
        .requests()
        .iter()
        .flat_map(|request| request.conversation.clone())
        .rev()
        .find_map(|input| match input {
            Input::User { text, .. } => Some(text),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. } => None,
        })
        .unwrap();
    assert_eq!(expanded, "Fetched.");
}

#[test]
fn a_skill_of_the_same_name_runs_instead() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let prompts = r#loop::ServerPrompts {
        rows: vec![row("review-pr", "fx")],
        fetch: recording(text_output("Fetched."), Arc::clone(&seen)),
    };
    let mut session = Session::new(vec![Scripted::text("Hi.")], None).server_prompts(prompts);
    review_skill(&session.workspace);
    session
        .inbox
        .send(support::delivery("/review-pr 42"))
        .unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert!(seen.lock().unwrap().is_empty(), "the fetch never runs");
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    assert_eq!(
        lines[3].payload["input"][0]["content"][0]["text"],
        "Review body.\n\n42"
    );
}

#[test]
fn a_disabled_name_is_sent_as_written() {
    let (session, seen) = prompted("Fetched.");
    let mut session = session.disabled_skills(vec!["greet".into()]);
    session.inbox.send(support::delivery("/greet Ada")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert!(seen.lock().unwrap().is_empty(), "the fetch never runs");
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    assert_eq!(
        lines[3].payload["input"][0]["content"][0]["text"],
        "/greet Ada"
    );
}

#[test]
fn the_first_row_of_a_name_is_fetched() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let prompts = r#loop::ServerPrompts {
        rows: vec![row("greet", "mm"), row("greet", "aa")],
        fetch: recording(text_output("Fetched."), Arc::clone(&seen)),
    };
    let mut session = Session::new(vec![Scripted::text("Hi.")], None).server_prompts(prompts);
    session.inbox.send(support::delivery("/greet Ada")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [("mm".to_owned(), "greet".to_owned(), "Ada".to_owned())]
    );
}

#[test]
fn a_name_no_skill_or_prompt_has_is_sent_as_written() {
    let (mut session, seen) = prompted("Fetched.");
    session.inbox.send(support::delivery("/nope x")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert!(seen.lock().unwrap().is_empty(), "the fetch never runs");
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    assert_eq!(
        lines[3].payload["input"][0]["content"][0]["text"],
        "/nope x"
    );
}

#[test]
fn a_steer_naming_a_prompt_is_not_fetched() {
    let (mut session, seen) = prompted("Fetched.");
    let (ack, answers) = capture();
    session
        .inbox
        .send(Delivery::Steer(message("/greet Ada"), ack))
        .unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(take(&answers), accepted());
    assert!(seen.lock().unwrap().is_empty(), "the fetch never runs");
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    assert_eq!(
        lines[3].payload["input"][0]["content"][0]["text"],
        "/greet Ada"
    );
}

/// `run` until it returns. The session keeps its inbox sender, so the loop
/// ends because `close` was taken. The log is read directly: no turn runs,
/// so `Session::lines` would wait for a `turn_completed` that never comes.
fn run_until_close(session: &mut Session) -> Vec<contract::Envelope> {
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run()).unwrap());
    finished
        .recv_timeout(support::DEADLINE)
        .expect("close ended the loop")
        .unwrap();
    log::read(&session.dir).unwrap()
}

fn failed_output() -> Output {
    Output {
        error: Some(Failure {
            code: ErrorCode::McpPromptFailed,
            message: "The MCP server `fx` refused the prompt `/greet`: gone.".into(),
            retry_after_ms: None,
            provider: None,
        }),
        ..Output::default()
    }
}

#[test]
fn a_failed_prompt_is_rejected_and_starts_no_turn() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let prompts = r#loop::ServerPrompts {
        rows: vec![row("greet", "fx")],
        fetch: recording(failed_output(), Arc::clone(&seen)),
    };
    let mut session = Session::new(vec![Scripted::text("Hi.")], None).server_prompts(prompts);
    let (ack, answers) = capture();
    session
        .inbox
        .send(Delivery::Prompt(message("/greet Ada"), ack))
        .unwrap();
    let (close_ack, close_answers) = capture();
    session.inbox.send(Delivery::Close(close_ack)).unwrap();
    let lines = run_until_close(&mut session);
    assert_eq!(
        take(&answers),
        rejected(
            ErrorCode::McpPromptFailed,
            "The MCP server `fx` refused the prompt `/greet`: gone."
        )
    );
    assert_eq!(take(&close_answers), accepted());
    assert_eq!(kinds(&lines), ["session_started"]);
    assert!(
        session.provider.requests().is_empty(),
        "nothing reaches the model"
    );

    // A following plain prompt runs a turn whose input is only that prompt.
    let mut session =
        Session::new(vec![Scripted::text("Hi.")], None).server_prompts(r#loop::ServerPrompts {
            rows: vec![row("greet", "fx")],
            fetch: recording(failed_output(), Arc::new(Mutex::new(Vec::new()))),
        });
    session.inbox.send(support::delivery("/greet Ada")).unwrap();
    session.inbox.send(support::delivery("plain")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.kind == "turn_started")
            .count(),
        1
    );
    assert_eq!(lines[3].payload["input"][0]["content"][0]["text"], "plain");
}

fn failed(server: &str) -> McpServerFailed {
    McpServerFailed {
        server: server.into(),
        reason: ServerFailure::Died,
        will_restart: true,
        error: Failure {
            code: ErrorCode::McpServerUnavailable,
            message: format!(
                "The MCP server `{server}` exited; Fiber restarts it on the next call."
            ),
            retry_after_ms: None,
            provider: None,
        },
    }
}

#[test]
fn server_lines_from_a_retrieval_are_written_outside_any_turn() {
    let servers = vec![
        ServerRecord::Failed(failed("fx")),
        ServerRecord::Ready(McpServerReady {
            server: "fx".into(),
        }),
    ];
    let answer = Output {
        content: vec![ContentPart::Text {
            text: "Fetched.".into(),
        }],
        servers,
        ..Output::default()
    };
    let prompts = r#loop::ServerPrompts {
        rows: vec![row("greet", "fx")],
        fetch: recording(answer, Arc::new(Mutex::new(Vec::new()))),
    };
    let mut session = Session::new(vec![Scripted::text("Hi.")], None).server_prompts(prompts);
    session.inbox.send(support::delivery("/greet Ada")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let mut expected = vec![
        "session_started",
        "mcp_server_failed",
        "mcp_server_ready",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(plain()[4..].iter().copied());
    assert_eq!(kinds(&lines), expected);
    assert!(lines[1].turn_id.is_none());
    assert!(lines[2].turn_id.is_none());

    // On a failed fetch the server lines land with no turn after them.
    let answer = Output {
        servers: vec![ServerRecord::Failed(failed("fx"))],
        ..failed_output()
    };
    let prompts = r#loop::ServerPrompts {
        rows: vec![row("greet", "fx")],
        fetch: recording(answer, Arc::new(Mutex::new(Vec::new()))),
    };
    let mut session = Session::new(vec![Scripted::text("Hi.")], None).server_prompts(prompts);
    let (ack, answers) = capture();
    session
        .inbox
        .send(Delivery::Prompt(message("/greet Ada"), ack))
        .unwrap();
    let (close_ack, close_answers) = capture();
    session.inbox.send(Delivery::Close(close_ack)).unwrap();
    let lines = run_until_close(&mut session);
    assert_eq!(
        take(&answers),
        rejected(
            ErrorCode::McpPromptFailed,
            "The MCP server `fx` refused the prompt `/greet`: gone."
        )
    );
    assert_eq!(take(&close_answers), accepted());
    assert_eq!(kinds(&lines), ["session_started", "mcp_server_failed"]);
}

#[test]
fn the_rest_of_the_message_follows_the_prompts_text() {
    let (mut session, _) = prompted("Fetched.");
    let image = ContentPart::Image {
        path: "artifacts/i.png".into(),
        mime_type: "image/png".into(),
        width: 1,
        height: 1,
    };
    let (ack, answers) = capture();
    session
        .inbox
        .send(Delivery::Prompt(
            contract::inbox::Message {
                content: vec![
                    ContentPart::Text {
                        text: "/greet Ada".into(),
                    },
                    image.clone(),
                ],
                sender: message("ignored").sender,
            },
            ack,
        ))
        .unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(take(&answers), accepted());
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    let content = lines[3].payload["input"][0]["content"].clone();
    assert_eq!(content.as_array().unwrap().len(), 2);
    assert_eq!(content[0]["text"], "Fetched.");
    assert_eq!(serde_json::to_value(&image).unwrap(), content[1].clone());
}

/// A latch the fetch waits on: set when the session's cancel fires. The
/// wait carries [`support::DEADLINE`], so a missed cancel ends the fetch
/// instead of hanging the turn.
#[derive(Default)]
struct Latch {
    fired: Mutex<bool>,
    changed: Condvar,
}

impl Wake for Latch {
    fn wake(&self) {
        *self.fired.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

impl Latch {
    fn wait(&self) {
        let fired = self.fired.lock().unwrap();
        if *fired {
            return;
        }
        let (fired, _) = self
            .changed
            .wait_timeout_while(fired, support::DEADLINE, |fired| !*fired)
            .unwrap();
        assert!(*fired, "the cancel fired while the fetch waited");
    }
}

#[test]
fn a_shutdown_during_retrieval_reaches_the_fetch() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let fetch: r#loop::FetchPrompt = Arc::new(
        move |_server: &str, _prompt: &str, _args: &str, cancel: &dyn Cancel| {
            let latch = Arc::new(Latch::default());
            cancel.subscribe(Arc::downgrade(&(Arc::clone(&latch) as Arc<dyn Wake>)));
            entered_tx.send(()).unwrap();
            if !cancel.is_cancelled() {
                latch.wait();
            }
            assert!(cancel.is_cancelled(), "a shutdown reads cancelled");
            failed_output()
        },
    );
    let prompts = r#loop::ServerPrompts {
        rows: vec![row("greet", "fx")],
        fetch,
    };
    let mut session = Session::new(vec![Scripted::text("Hi.")], None).server_prompts(prompts);
    let (ack, answers) = capture();
    session
        .inbox
        .send(Delivery::Prompt(message("/greet Ada"), ack))
        .unwrap();
    let cancel = Arc::clone(&session.cancel);
    thread::scope(|scope| {
        scope.spawn(move || {
            entered_rx
                .recv_timeout(support::DEADLINE)
                .expect("the fetch started");
            cancel.shutdown(130);
        });
        assert_eq!(session.turn(), None);
    });
    assert_eq!(
        take(&answers),
        rejected(
            ErrorCode::McpPromptFailed,
            "The MCP server `fx` refused the prompt `/greet`: gone."
        )
    );
}

#[test]
fn a_rejected_prompt_does_not_move_the_idle_deadline() {
    let prompts = r#loop::ServerPrompts {
        rows: vec![row("greet", "fx")],
        fetch: recording(failed_output(), Arc::new(Mutex::new(Vec::new()))),
    };
    let mut session = Session::new(Vec::new(), None).server_prompts(prompts);
    session.looped = Some(
        session
            .looped
            .take()
            .unwrap()
            .idle_exit(Some(Duration::from_secs(60))),
    );
    let clock = Arc::clone(&session.clock);
    let deadline = clock.now() + Duration::from_secs(60);
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run()).unwrap());
    assert!(
        clock.await_parked(deadline, support::DEADLINE),
        "idle before the rejected prompt"
    );
    clock.advance(Duration::from_secs(10));
    let (ack, answers) = capture();
    session
        .inbox
        .send(Delivery::Prompt(message("/greet Ada"), ack))
        .unwrap();
    let answer = answers
        .recv_timeout(support::DEADLINE)
        .expect("the prompt was admitted");
    assert_eq!(
        answer,
        rejected(
            ErrorCode::McpPromptFailed,
            "The MCP server `fx` refused the prompt `/greet`: gone."
        )
    );
    assert!(
        clock.await_parked(deadline, support::DEADLINE),
        "a rejected prompt does not move the deadline"
    );
    assert!(
        finished.try_recv().is_err(),
        "the rejection ends no idle wait"
    );
}

#[test]
fn an_empty_success_has_no_text_to_send() {
    let prompts = r#loop::ServerPrompts {
        rows: vec![row("greet", "fx")],
        fetch: recording(Output::default(), Arc::new(Mutex::new(Vec::new()))),
    };
    let mut session = Session::new(vec![Scripted::text("Hi.")], None).server_prompts(prompts);
    let (ack, answers) = capture();
    session
        .inbox
        .send(Delivery::Prompt(message("/greet Ada"), ack))
        .unwrap();
    let (close_ack, close_answers) = capture();
    session.inbox.send(Delivery::Close(close_ack)).unwrap();
    let lines = run_until_close(&mut session);
    assert_eq!(
        take(&answers),
        rejected(
            ErrorCode::McpPromptFailed,
            "The MCP server `fx`'s prompt `/greet` returned no text, which Fiber cannot send as a message."
        )
    );
    assert_eq!(take(&close_answers), accepted());
    assert_eq!(kinds(&lines), ["session_started"]);
}
