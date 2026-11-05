//! Binary proof for #4186: a finished `fiber` delegate with a real
//! two-turn log on disk, opened from `/delegates`, renders both turns'
//! cards (`docs/tui.md`, "A delegate's or job's view": the transcript is
//! the replay of its log).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::fs;

use fakes::{ProviderServer, Request, Response};
use serde_json::{Value, json};
use support::function_call;
use support::pty::Run;

/// Installs provider `name` with model `m` on `protocol` at `url`.
fn install_url(
    setup: &support::Setup,
    name: &str,
    url: &str,
    protocol: &str,
    extra: &serde_json::Value,
) {
    let source = setup.root.path().join(format!("src-{name}"));
    let mut manifest = json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = Value::Object(
        [(name.to_owned(), json!([format!("{url}/v1")]))]
            .into_iter()
            .collect(),
    );
    support::write_json(&source.join("extension.json"), &manifest);
    let mut model = json!({"id": "m", "protocol": protocol, "base_url": format!("{url}/v1"), "context_window": 100000});
    for (key, value) in extra.as_object().unwrap() {
        model[key] = value.clone();
    }
    support::write_json(
        &source.join(format!("providers/{name}.json")),
        &json!({
            "name": name,
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
}

/// Installs the parent's `pa/m` and the child's `pb/m` on `openai-responses`.
fn providers(setup: &support::Setup, parent: &ProviderServer, child: &ProviderServer) {
    install_url(setup, "pa", &parent.url(), "openai-responses", &json!({}));
    install_url(setup, "pb", &child.url(), "openai-responses", &json!({}));
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"}}),
    );
}

/// A standing allow for `delegate_spawn`, `delegate_fork` and
/// `delegate_message`.
fn standing_allow(setup: &support::Setup) {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
            json!({"decision": "allow", "tool": "delegate_fork", "prefix": ""}),
            json!({"decision": "allow", "tool": "delegate_message", "prefix": ""})
        ),
    )
    .unwrap();
}

/// Arms the watchdog for `setup`. First, before any fallible step.
fn arm(setup: &support::Setup) -> support::SessionGuard {
    support::SessionGuard::arm(setup.deadline, &setup.root.path().to_string_lossy())
}

/// An `openai-responses` stream answering `text`.
fn text_reply(text: &str) -> Response {
    support::stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

/// A `delegate_spawn` call for `model`: the parent's first reply.
fn spawn_call(model: &str) -> Value {
    function_call(
        "call_delegate",
        "delegate_spawn",
        &json!({"description": "scan", "prompt": "say hello", "model": model}),
    )
}

/// The job id the spawn receipt names: `Started delegate <id>.`.
fn job_id(text: &str) -> String {
    let marker = "Started delegate ";
    let start = text.find(marker).unwrap() + marker.len();
    text[start..]
        .split([' ', '.', '"', '\\'])
        .next()
        .unwrap()
        .to_owned()
}

/// Quits the terminal: two interrupts and Enter, then the exit.
fn quit(run: Run) {
    let mut run = run;
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// The resume message the scripted parent sends: the child's second run.
const RESUME_MARK: &str = "resume-marker-9d2b4e77";

/// The finished Delegates-card row: the `delegate_spawn` description the
/// scripted parent sends, with the finished glyph.
const FINISHED_ROW: &str = "✓ scan";

/// The delegate child's first run: what the first turn's card shows.
const RUN_ONE: &str = "run-one-done";

/// The delegate child's resumed run: what the second turn's card shows.
const RUN_TWO: &str = "run-two-done";

#[test]
fn a_finished_delegate_opens_two_turns_through_delegates() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    // The scripted parent spawns one delegate, then resumes it as the
    // second run of the same job: a blank `delegate_message` first (the
    // refusal teaches the shape), then the resume carrying the mark.
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if text.contains(RUN_TWO) {
            return support::hello();
        }
        if text.contains("Resumed delegate") {
            return support::hello();
        }
        if text.contains("must not be empty") {
            let job = job_id(&text);
            return support::stream(&[function_call(
                "call_resume",
                "delegate_message",
                &json!({"id": job, "message": RESUME_MARK}),
            )]);
        }
        if text.contains(RUN_ONE) {
            let job = job_id(&text);
            return support::stream(&[function_call(
                "call_blank",
                "delegate_message",
                &json!({"id": job, "message": " \n\t"}),
            )]);
        }
        if text.contains("Started delegate") {
            return support::hello();
        }
        support::stream(&[spawn_call("fiber:pb/m")])
    })
    .unwrap();
    // The held child answers each run once released: its first run, then
    // the resumed run carrying the mark.
    let child = ProviderServer::start_responding(|request: &Request| {
        let resumed = String::from_utf8_lossy(&request.body).contains(RESUME_MARK);
        text_reply(if resumed { RUN_TWO } else { RUN_ONE })
    })
    .unwrap();
    child.hold();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    // The hub exits a second after its last client leaves, so the run's
    // wait sees it go; the session itself keeps its 30-minute default.
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "hub": {"idle_exit_ms": 1000}}),
    );

    let env: Vec<(&str, &str)> = [("FIBER_TEST_FAKE_KEY", "sk-test")]
        .into_iter()
        .chain(support::TRUECOLOUR)
        .collect();
    let mut run = Run::spawn(&setup, 160, 48, &[], &env);
    run.ready();
    run.write(b"scan the tree\r");
    // The hold keeps the delegate's first model call waiting, so the
    // release below stages both runs after the prompt lands.
    child.release();
    // The scripted resume plays out on its own: the card keeps its
    // finished row once both runs end.
    run.wait_screen("the finished delegate row", |grid| {
        grid.contents.contains(FINISHED_ROW)
    });
    // Open through `/delegates`: the list, the finished row, Enter.
    run.write(b"/delegates\r");
    run.wait_screen("the delegates list", |grid| {
        grid.contents.contains("Delegates") && grid.contents.contains("/delegates")
    });
    run.write(b"\x1b[B\r");
    run.wait_screen("the breadcrumb", |grid| {
        grid.contents.contains("› ◆ scan")
    });
    // Acceptance (#4186): both turns' cards render from the log replay.
    run.wait_screen("both turns' cards", |grid| {
        grid.contents.contains(RUN_ONE) && grid.contents.contains(RUN_TWO)
    });
    quit(run);
}

#[test]
fn a_finished_delegate_opens_two_turns_through_delegates_card() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    // The scripted parent spawns one delegate, then resumes it as the
    // second run of the same job: a blank `delegate_message` first (the
    // refusal teaches the shape), then the resume carrying the mark.
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if text.contains(RUN_TWO) {
            return support::hello();
        }
        if text.contains("Resumed delegate") {
            return support::hello();
        }
        if text.contains("must not be empty") {
            let job = job_id(&text);
            return support::stream(&[function_call(
                "call_resume",
                "delegate_message",
                &json!({"id": job, "message": RESUME_MARK}),
            )]);
        }
        if text.contains(RUN_ONE) {
            let job = job_id(&text);
            return support::stream(&[function_call(
                "call_blank",
                "delegate_message",
                &json!({"id": job, "message": " \n\t"}),
            )]);
        }
        if text.contains("Started delegate") {
            return support::hello();
        }
        support::stream(&[spawn_call("fiber:pb/m")])
    })
    .unwrap();
    // The held child answers each run once released: its first run, then
    // the resumed run carrying the mark.
    let child = ProviderServer::start_responding(|request: &Request| {
        let resumed = String::from_utf8_lossy(&request.body).contains(RESUME_MARK);
        text_reply(if resumed { RUN_TWO } else { RUN_ONE })
    })
    .unwrap();
    child.hold();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    // The hub exits a second after its last client leaves, so the run's
    // wait sees it go; the session itself keeps its 30-minute default.
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "hub": {"idle_exit_ms": 1000}}),
    );

    let env: Vec<(&str, &str)> = [("FIBER_TEST_FAKE_KEY", "sk-test")]
        .into_iter()
        .chain(support::TRUECOLOUR)
        .collect();
    let mut run = Run::spawn(&setup, 160, 48, &[], &env);
    run.ready();
    run.write(b"scan the tree\r");
    // The hold keeps the delegate's first model call waiting, so the
    // release below stages both runs after the prompt lands.
    child.release();
    // Open through the Delegates card's finished row: press and release
    // on the row, the card's serial into `open_serial` to `open_item`.
    run.click_where("the finished delegate row", |grid| {
        grid.text_at(FINISHED_ROW)
            .map(|(x, y)| (x.saturating_add(2), y))
    });
    run.wait_screen("the breadcrumb", |grid| {
        grid.contents.contains("› ◆ scan")
    });
    // Acceptance (#4186): both turns' cards render from the log replay.
    run.wait_screen("both turns' cards", |grid| {
        grid.contents.contains(RUN_ONE) && grid.contents.contains(RUN_TWO)
    });
    quit(run);
}
