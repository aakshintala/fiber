//! A delegate ending on a failed model call keeps the provider's words on
//! the parent's surfaces: the completion notice the parent's next request
//! carries, and the logged `job_completed.error`
//! (`docs/errors.md`, "The shape").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::path::Path;
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Request, Response, Watchdog};
use serde_json::{Value, json};
use support::{KillGroup, SessionGuard, function_call, hello, stream, text_reply};

/// The provider's own message the fake child answers 400 with.
const PROVIDER_MESSAGE: &str = "content cannot be empty if is_error is true";

/// Installs provider `name` with model `m` on `protocol` at `url`.
fn install_url(setup: &support::Setup, name: &str, url: &str, protocol: &str) {
    let source = setup.root.path().join(format!("src-{name}"));
    let mut manifest = json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = Value::Object(
        [(name.to_owned(), json!([format!("{url}/v1")]))]
            .into_iter()
            .collect(),
    );
    support::write_json(&source.join("extension.json"), &manifest);
    let model = json!({"id": "m", "protocol": protocol, "base_url": format!("{url}/v1"), "context_window": 100000});
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

/// Installs the parent's `pa/m` and the child's `pb/m` on
/// `openai-responses`, and configures the parent's model and reviewer.
fn providers(setup: &support::Setup, parent: &ProviderServer, child: &ProviderServer) {
    install_url(setup, "pa", &parent.url(), "openai-responses");
    install_url(setup, "pb", &child.url(), "openai-responses");
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"}}),
    );
}

/// A standing allow for `delegate_spawn`, `delegate_fork` and
/// `delegate_message`.
fn standing_allow(setup: &support::Setup) {
    std::fs::write(
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

/// The session keeps serving instead of idling out: an exit proves the run
/// ended on its own.
fn slow_idle(setup: &support::Setup) {
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000}}),
    );
}

/// Arms the watchdog for `setup`: every child holds the test root on its
/// command line, so a failing test leaves no child behind. First, before
/// any fallible step.
fn arm(setup: &support::Setup) -> SessionGuard {
    SessionGuard::arm(setup.deadline, &setup.root.path().to_string_lossy())
}

/// A `delegate_spawn` call for `model`: the parent's first reply.
fn spawn_call(model: &str) -> Value {
    function_call(
        "call_delegate",
        "delegate_spawn",
        &json!({"description": "scan", "prompt": "say hello", "model": model}),
    )
}

/// The finished `ask` run: its exit code, stdout lines and stderr.
struct Finished {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

/// Runs `fiber ask` to its exit under the test's deadline, in its own
/// process group with a watchdog beside it: expiry kills the group and
/// reaps it, and the watchdog kills the group when the test dies first
/// (`docs/testing.md`, "Running tests").
fn run_ask(setup: &support::Setup, prompt: &str) -> Finished {
    fakes::warm_up(Path::new(env!("CARGO_BIN_EXE_fiber")), &["--bogus"], 2);
    let child = setup.fiber(&["ask", prompt]).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match setup.deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(setup.deadline, group, &finished, "`fiber ask` to exit"),
    };
    assert!(
        !fakes::group_alive(group),
        "`fiber ask` left a process in its group behind"
    );
    // The group is empty. Skip the drop, which would kill it again, and
    // tell the watchdog to exit without signalling.
    std::mem::forget(guard);
    watchdog.stand_down(setup.deadline.cleanup_phase());
    Finished {
        code: output.status.code(),
        lines: String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn a_delegate_failing_on_its_model_call_reports_the_providers_message() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let child = ProviderServer::start_responding(|_: &Request| {
        Response::status(
            400,
            json!({"error": {"message": PROVIDER_MESSAGE}}).to_string(),
        )
    })
    .unwrap();
    // Reviewer requests carry no tools, so they never match the spawn
    // check; each later request reads the notice or answers it.
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if !text.contains("Started delegate") {
            return stream(&[spawn_call("fiber:pb/m")]);
        }
        hello()
    })
    .unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);

    let finished = run_ask(&setup, "scan the tree");
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);

    // The child's 400 ends its run at once: no retry, no second call
    // (`docs/errors.md`, "A failed model call": `invalid_request` is never
    // retried).
    assert_eq!(
        child.requests().len(),
        1,
        "the child called its provider once"
    );

    // The parent's log carries the failed completion with the provider.
    let done = finished
        .lines
        .iter()
        .find(|line| line["kind"] == "job_completed")
        .expect("a job_completed in the parent log");
    let done = &done["payload"];
    assert_eq!(done["status"], "failed");
    assert_eq!(done["error"]["code"], "invalid_request");
    assert_eq!(done["error"]["provider"]["message"], PROVIDER_MESSAGE);

    // The job notice a later parent request carries names the line.
    let notices: Vec<String> = parent
        .requests()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .filter(|body| body.contains("ended: failed"))
        .collect();
    assert!(
        !notices.is_empty(),
        "a later parent request carries the job notice"
    );
    for body in &notices {
        assert!(
            body.contains(&format!("Provider said: {PROVIDER_MESSAGE}")),
            "the notice carries the provider line: {body}"
        );
    }
}
