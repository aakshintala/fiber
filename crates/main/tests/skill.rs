//! Binary-level tests of the `skill` tool (`docs/tools.md`, "Skills";
//! `docs/testing.md`, "Levels"): the built `fiber` runs `fiber ask` in its
//! own process group with its own `FIBER_HOME`, holding fixture skills and
//! an ordinary provider whose base URL is the fake server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::fs::symlink;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::{Setup, hello, run_to_exit, stream};

/// A finished `function_call` for `skill` with `arguments`.
fn skill_call(call_id: &str, arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": "skill",
        "arguments": arguments.to_string()
    }})
}

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

/// Writes `<workspace>/.agents/skills/<name>/SKILL.md` holding `text`.
fn install_skill(setup: &Setup, name: &str, text: &str) {
    let dir = setup.root.path().join(".agents/skills").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), text).unwrap();
}

/// Runs `fiber ask` with a model that makes the calls of `replies` in
/// order, then answers `Hello.`, and returns stdout's lines with the
/// server, which stays alive for its requests.
fn ask(setup: &Setup, prompt: &str, replies: Vec<Response>) -> (Vec<Value>, ProviderServer) {
    let server = ProviderServer::start(replies).unwrap();
    setup.provider(&server);
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", prompt]));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect();
    (lines, server)
}

/// The event kinds of every durable line `fiber ask` writes, in order.
/// `session_status` lines are ephemeral, so they are filtered out.
fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// The event kinds of a turn whose first reply calls one reads-only tool
/// and whose second is `Hello.`: no `permission_` line is written, as for
/// a reads-only workspace call (`docs/permissions.md`, "Fast paths").
fn skill_kinds() -> Vec<&'static str> {
    let mut kinds = vec![
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
        "tool_call_started",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
    ];
    kinds.extend(["assistant_message_delta"; 2]);
    kinds.extend([
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    kinds
}

/// The event kinds of a turn whose first reply calls one skill that the
/// credential deny refuses and whose second is `Hello.`: the denied call
/// never starts, and its denial is decided before completion.
fn denied_skill_kinds() -> Vec<&'static str> {
    vec![
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
        "permission_resolved",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]
}

/// The event kinds of a run whose model loads a skill, reads its file,
/// writes it, loads it again, then answers `Hello.`: four fast-path
/// calls, each in its own step, with no `permission_` line.
fn edit_kinds() -> Vec<&'static str> {
    let mut kinds = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    for _ in 0..4 {
        kinds.extend([
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
        ]);
    }
    kinds.extend([
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    kinds
}

/// The one line of `kind`.
fn line<'a>(lines: &'a [Value], kind: &str) -> &'a Value {
    let mut found = lines.iter().filter(|line| line["kind"] == kind);
    let first = found.next().unwrap_or_else(|| panic!("no {kind} line"));
    assert!(found.next().is_none(), "more than one {kind} line");
    first
}

/// The listing path of the skill `name` in the run's `opening_message`.
fn listing_path(lines: &[Value], name: &str) -> String {
    let opening = line(lines, "opening_message");
    let skills = opening["payload"]["skills"].as_array().unwrap();
    let entry = skills
        .iter()
        .find(|skill| skill["name"] == name)
        .unwrap_or_else(|| panic!("no {name} skill in the listing"));
    entry["path"].as_str().unwrap().to_owned()
}

#[test]
fn a_listed_skill_returns_its_body_under_its_path() {
    let setup = Setup::new();
    install_skill(
        &setup,
        "tdd",
        "---\nname: tdd\ndescription: Tests first.\n---\nWrite the failing test first.\n",
    );
    let (lines, _) = ask(
        &setup,
        "load the skill",
        vec![
            stream(&[skill_call("call_skill", &json!({"name": "tdd"}))]),
            hello(),
        ],
    );

    assert_eq!(kinds(&lines), skill_kinds());
    let path = listing_path(&lines, "tdd");
    let started = line(&lines, "tool_call_started");
    assert_eq!(started["payload"]["effects"], json!(["reads"]));
    assert_eq!(started["payload"]["paths"], json!([path]));
    let completed = line(&lines, "tool_call_completed");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed["payload"]["content"],
        json!([{"type": "text", "text": format!("{path}\n\nWrite the failing test first.")}])
    );
    assert_eq!(
        completed["payload"]["control"],
        json!({"skill": {"name": "tdd", "path": path}})
    );
}

/// One `fiber ask` run whose model loads `name`, and its completion.
fn refused(setup: &Setup, name: &str) -> Value {
    let (lines, _) = ask(
        setup,
        "load the skill",
        vec![
            stream(&[skill_call("call_skill", &json!({"name": name}))]),
            hello(),
        ],
    );
    assert_eq!(kinds(&lines), skill_kinds());
    line(&lines, "tool_call_completed").clone()
}

#[test]
fn a_name_not_in_the_listing_fails_invalid_arguments() {
    // An unknown name.
    let setup = Setup::new();
    let completed = refused(&setup, "nope");
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "invalid_arguments");
    assert!(completed["payload"].get("control").is_none());

    // A skill with `disable-model-invocation: true`.
    let setup = Setup::new();
    install_skill(
        &setup,
        "template",
        "---\nname: template\ndescription: A template.\ndisable-model-invocation: true\n---\nBody.\n",
    );
    let completed = refused(&setup, "template");
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "invalid_arguments");
    assert!(completed["payload"].get("control").is_none());

    // A skill named in `skills.disabled` in `config.json`.
    let setup = Setup::new();
    install_skill(
        &setup,
        "tdd",
        "---\nname: tdd\ndescription: Tests first.\n---\nWrite the failing test first.\n",
    );
    let server = ProviderServer::start([
        stream(&[skill_call("call_skill", &json!({"name": "tdd"}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("config.json"),
        json!({"model": "fake/m", "skills": {"disabled": ["tdd"]}}).to_string(),
    )
    .unwrap();
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", "load"]));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect();
    assert_eq!(kinds(&lines), skill_kinds());
    let completed = line(&lines, "tool_call_completed");
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "invalid_arguments");
    assert!(completed["payload"].get("control").is_none());
}

#[test]
fn an_edit_between_two_loads_is_seen_by_the_second() {
    let setup = Setup::new();
    install_skill(
        &setup,
        "tdd",
        "---\nname: tdd\ndescription: Tests first.\n---\nWrite the failing test first.\n",
    );
    let path = setup
        .root
        .path()
        .join(".agents/skills/tdd/SKILL.md")
        .display()
        .to_string();
    let server = ProviderServer::start([
        stream(&[skill_call("call_one", &json!({"name": "tdd"}))]),
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": ".agents/skills/tdd/SKILL.md"}),
        )]),
        stream(&[function_call(
            "call_write",
            "write",
            &json!({"path": path,
                "content": "---\nname: tdd\ndescription: Tests first.\n---\nRed, green, refactor.\n"}),
        )]),
        stream(&[skill_call("call_two", &json!({"name": "tdd"}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    // A write inside the workspace takes the fast path, so no reviewer
    // is needed (`docs/permissions.md`, "Fast paths").
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", "load"]));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect();
    assert_eq!(kinds(&lines), edit_kinds());

    let completed: Vec<_> = lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    // `skill`, `read`, `write`, `skill`.
    assert_eq!(
        completed
            .iter()
            .map(|done| done["payload"]["status"].clone())
            .collect::<Vec<_>>(),
        ["completed", "completed", "completed", "completed"],
        "{:?}",
        kinds(&lines)
    );
    let first = completed[0]["payload"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let second = completed[3]["payload"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        first.ends_with("\n\nWrite the failing test first."),
        "{first}"
    );
    assert!(second.ends_with("\n\nRed, green, refactor."), "{second}");
}

fn tool_names(body: &[u8]) -> Vec<String> {
    let body: Value = serde_json::from_slice(body).unwrap();
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn adding_a_skill_leaves_the_tool_set_unchanged() {
    let plain = Setup::new();
    let (_, plain_server) = ask(&plain, "hi", vec![hello()]);
    let plain_tools = tool_names(&plain_server.requests()[0].body);
    assert!(plain_tools.contains(&"skill".to_owned()));

    let setup = Setup::new();
    install_skill(
        &setup,
        "tdd",
        "---\nname: tdd\ndescription: Tests first.\n---\nWrite the failing test first.\n",
    );
    let (_, server) = ask(&setup, "hi", vec![hello()]);
    let tools = tool_names(&server.requests()[0].body);

    assert_eq!(tools, plain_tools);
}

/// Whether `bytes` holds `marker`.
fn holds_marker(bytes: &[u8], marker: &str) -> bool {
    let marker = marker.as_bytes();
    bytes.windows(marker.len()).any(|window| window == marker)
}

#[test]
fn a_skill_linked_into_credentials_is_denied() {
    const MARKER: &str = "the secret skill body that must never be read";
    let setup = Setup::new();
    let target = setup.home().join("credentials/sec");
    fs::create_dir_all(&target).unwrap();
    fs::write(
        target.join("SKILL.md"),
        format!("---\nname: sec\ndescription: A secret skill.\n---\n{MARKER}\n"),
    )
    .unwrap();
    let link = setup.root.path().join(".agents/skills/sec");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(fs::canonicalize(&target).unwrap(), &link).unwrap();

    let (lines, _) = ask(
        &setup,
        "load the secret skill",
        vec![
            stream(&[skill_call("call_skill", &json!({"name": "sec"}))]),
            hello(),
        ],
    );
    assert_eq!(kinds(&lines), denied_skill_kinds());
    let completed = line(&lines, "tool_call_completed");
    assert_eq!(completed["payload"]["status"], "denied");
    assert!(
        lines.iter().all(|line| line["kind"] != "tool_call_started"),
        "a denied call never starts"
    );
    for line in lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
    {
        for part in line["payload"]["content"].as_array().unwrap() {
            let text = part["text"].as_str().unwrap_or("");
            assert!(!text.contains(MARKER), "{text}");
        }
    }
    // The marker is in no file under the session's directory.
    let running = lines[0]["session_id"].as_str().unwrap();
    let workspace = fs::canonicalize(setup.root.path()).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    let session = setup
        .home()
        .join("projects")
        .join(key)
        .join("sessions")
        .join(running);
    assert!(session.join("events.jsonl").is_file());
    let mut pending = vec![session.join("artifacts")];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                assert!(
                    !holds_marker(&fs::read(&path).unwrap(), MARKER),
                    "{} holds the marker",
                    path.display()
                );
            }
        }
    }
}
