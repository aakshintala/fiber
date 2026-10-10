//! The shipped codex package in the built binary (`docs/testing.md`,
//! "Model calls"): loaded by local path, one turn against a scripted
//! Responses stream, a usage-limit reply, and no stored credential. The
//! package copy rewrites the ChatGPT origin to the fake server; the OAuth
//! origin is untouched, and no token endpoint is ever reached.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Request, Response, fingerprint};
use serde_json::{Value, json};
use support::{Deadline, group_alive};

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fx");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    /// Installs the codex package copy with the ChatGPT origin rewritten to
    /// `server`, with `fiber extension install`.
    fn install(&self, server: &ProviderServer) {
        let to = self.root.path().join("pkg-codex");
        support::package::copy_package("codex", &to, "https://chatgpt.com", &server.url());
        let run = self.fiber(&["extension", "install", to.to_str().unwrap()], "");
        assert_eq!(run.code, Some(0), "{}", run.stderr);
    }

    /// Stores the codex credential under `default`, mode 0600.
    fn store(&self, token: &str, account: &str) {
        let dir = self.home().join("credentials/codex");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("default");
        fs::write(
            &file,
            json!({
                "token": token,
                "expires_at": 4_102_444_800u64,
                "refresh_token": "rt_1",
                "account_id": account,
            })
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path().join("w"))
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home());
        command
    }

    /// Runs `fiber` with `args` to completion under the deadline, in its own
    /// process group with a watchdog beside it.
    fn fiber(&self, args: &[&str], input: &str) -> Run {
        let mut command = self.command(args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.process_group(0).spawn().unwrap();
        let group = child.id();
        let guard = KillGroup(group);
        let watchdog = fakes::Watchdog::group(group);
        feed(&mut child, input);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(
            !group_alive(self.deadline, group),
            "`fiber` left a process behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup());
        let stdout = String::from_utf8(output.stdout).unwrap();
        Run {
            code: output.status.code(),
            lines: stdout
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

struct Run {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

impl Run {
    fn last(&self) -> &Value {
        self.lines.last().unwrap()
    }
}

/// Writes `input` to the child's stdin on a thread and closes it.
fn feed(child: &mut std::process::Child, input: &str) {
    use std::io::Write;
    let mut pipe = child.stdin.take().unwrap();
    let input = input.to_owned();
    thread::spawn(move || match pipe.write_all(input.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => panic!("writing to the stdin of `fiber`: {e}"),
    });
}

/// Kills process group `group` on drop.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        support::kill_group_detached(self.0, "KILL");
    }
}

/// An `openai-responses` stream answering `Hello.` with usage.
fn hello() -> Response {
    let events = [
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
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

/// A stored codex token carrying `account`, expiring in 2100.
fn token(account: &str) -> String {
    fakes::jwt(&json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": account },
        "exp": 4_102_444_800u64,
    }))
}

#[test]
fn a_codex_turn_sends_the_codex_wire_shape() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.install(&server);
    let access = token("acct_secret");
    setup.store(&access, "acct_secret");

    let run = setup.fiber(&["ask", "--model", "codex/gpt-6-luna", "hi"], "");

    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request: &Request = &requests[0];
    assert_eq!(request.path, "/backend-api/codex/responses");
    let bearer = fingerprint(&format!("Bearer {access}"));
    assert_eq!(request.header("authorization"), Some(bearer.as_str()));
    assert_eq!(request.header("chatgpt-account-id"), Some("acct_secret"));
    assert_eq!(request.header("originator"), Some("fiber"));
    assert!(request.header("openai-beta").is_none());
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        request.header("session_id"),
        body["prompt_cache_key"].as_str()
    );
    assert_eq!(body["store"], false);
    assert!(body.get("include").is_some());
    assert!(body.get("temperature").is_none());
    assert!(body.get("service_tier").is_none());
    let recorded: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "usage_recorded")
        .collect();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["payload"]["subscription"], true);
}

#[test]
fn a_codex_usage_limit_is_quota_exceeded_with_its_wait_and_not_retried() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(
        429,
        r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":1791396000}}"#,
    )
    .header("date", "Wed, 07 Oct 2026 16:00:00 GMT")])
    .unwrap();
    setup.install(&server);
    setup.store(&token("acct_secret"), "acct_secret");

    let run = setup.fiber(&["ask", "--model", "codex/gpt-6-luna", "hi"], "");

    assert_eq!(run.code, Some(1), "{}", run.stderr);
    let error = &run.last()["payload"]["error"];
    assert_eq!(error["code"], "quota_exceeded");
    assert_eq!(error["retry_after_ms"], 7_200_000);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("2026-10-07 18:00 UTC"),
        "{error}"
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_thinking_suffix_is_sent_and_an_undeclared_level_is_invalid_arguments() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.install(&server);
    setup.store(&token("acct_secret"), "acct_secret");

    let run = setup.fiber(&["ask", "--model", "codex/gpt-6-luna:xhigh", "hi"], "");
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["reasoning"], json!({"effort": "xhigh"}));

    let bad = setup.fiber(&["ask", "--model", "codex/gpt-6-luna:minimal", "hi"], "");
    assert_eq!(bad.code, Some(1), "{}", bad.stderr);
    assert_eq!(bad.last()["payload"]["error"]["code"], "invalid_arguments");
    assert_eq!(server.requests().len(), 1, "no second request is sent");
}

#[test]
fn a_codex_ask_with_nothing_stored_is_authentication_failed_naming_login() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.install(&server);

    let run = setup.fiber(&["ask", "--model", "codex/gpt-6-luna", "hi"], "");

    assert_eq!(run.code, Some(1), "{}", run.stderr);
    let error = &run.last()["payload"]["error"];
    assert_eq!(error["code"], "authentication_failed");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("fiber login codex"),
        "{error}"
    );
    assert!(server.requests().is_empty());
}
