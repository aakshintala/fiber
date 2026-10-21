//! Binary-level tests of `fiber mcp add|remove|list`
//! (`docs/testing.md`, "Levels"; `docs/invocation.md`, "Commands and
//! flags"): the built `fiber` writes and reads `mcp.servers` in the three
//! layers, stores `--secret` values beside the configuration, and records
//! and shows a repository declaration's approval. Every run carries a
//! wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::ProviderServer;
use serde_json::json;
use support::{KillGroup, Setup, StdoutRun, files_holding, group_alive, hello, spawn_watched};

impl Setup {
    /// Runs `fiber` with `args` in `dir` under `home`, waits for it under
    /// the test's [`Deadline`], and asserts nothing it started is left
    /// behind. `stdin_text` is written to its standard input, if any.
    #[track_caller]
    fn run_as(
        &self,
        home: &Path,
        dir: &Path,
        args: &[&str],
        stdin_text: Option<&str>,
    ) -> StdoutRun {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(dir)
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", home)
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(if stdin_text.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, watchdog) = spawn_watched(&mut command);
        if let Some(text) = stdin_text {
            let written = child.stdin.take().unwrap().write_all(text.as_bytes());
            // A usage error exits before reading stdin, so the pipe may be closed.
            if let Err(e) = written {
                assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
            }
        }
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
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
        watchdog.stand_down(self.deadline.cleanup_phase());
        StdoutRun::from(output)
    }

    /// Runs `fiber` with `args` in the workspace under this home.
    #[track_caller]
    fn run(&self, args: &[&str], stdin_text: Option<&str>) -> StdoutRun {
        let (home, workspace) = (self.home(), self.workspace());
        self.run_as(&home, &workspace, args, stdin_text)
    }
}

/// Installs the fake provider with model `fake/m` in `home`.
fn install_provider(home: &Path, root: &Path, server: &ProviderServer) {
    let source = root.join("src");
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    support::write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
        }),
    );
    extensions::plan(
        home,
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    support::write_json(&home.join("config.json"), &json!({"model": "fake/m"}));
}

#[test]
fn add_then_list_then_remove_round_trips_a_remote_server() {
    let setup = Setup::new();
    let add = setup.run(&["mcp", "add", "x", "https://example.test/mcp"], None);
    assert_eq!(add.code, Some(0), "{add:?}");
    assert_eq!(add.stdout, "");
    let list = setup.run(&["mcp", "list"], None);
    assert_eq!(list.code, Some(0), "{list:?}");
    assert_eq!(list.stdout, "x  global\n", "{list:?}");
    let remove = setup.run(&["mcp", "remove", "x"], None);
    assert_eq!(remove.code, Some(0), "{remove:?}");
    let list = setup.run(&["mcp", "list"], None);
    assert_eq!(list.code, Some(0), "{list:?}");
    assert_eq!(list.stdout, "", "{list:?}");
}

#[test]
fn a_secret_piped_on_stdin_is_stored_beside_the_configuration() {
    let setup = Setup::new();
    let add = setup.run(
        &[
            "mcp", "add", "x", "-e", "A=1", "--secret", "TOKEN", "--", "npx", "-y",
        ],
        Some("tok-1\n"),
    );
    assert_eq!(add.code, Some(0), "{add:?}");
    // A `config get`-style read shows the secret's reference, never its value.
    let get = setup.run(&["config", "get", "mcp.servers.x.env.TOKEN"], None);
    assert_eq!(get.code, Some(0), "{get:?}");
    assert!(get.stdout.contains("mcp.x.TOKEN"), "{get:?}");
    assert!(!get.stdout.contains("tok-1"), "{get:?}");
    assert!(!add.stderr.contains("tok-1"), "{add:?}");
    // The value occurs in no file under the home or the repository
    // except its own credentials file.
    assert_eq!(
        files_holding(&[setup.home(), setup.workspace()], b"tok-1"),
        [setup.home().join("credentials/mcp.x.TOKEN")]
    );
}

#[test]
fn a_repo_add_is_approved_here_and_skipped_in_a_fresh_home() {
    let setup = Setup::new();
    let provider = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&provider);
    // The fixture server: `tools.json` and the script the declaration
    // pins, with the fixture directory outside the repository.
    let dir = setup.root.path().join("fx");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("tools.json"),
        json!([{
            "name": "echo",
            "description": "Echoes.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true, "openWorldHint": false},
        }])
        .to_string(),
    )
    .unwrap();
    let workspace = setup.workspace();
    let srv = workspace.join("srv");
    fs::create_dir_all(&srv).unwrap();
    fs::write(
        srv.join("run.sh"),
        format!(
            "#!/bin/bash\nexec /bin/bash \"{}\" \"$1\"\n",
            fakes::mcp_fixture().display(),
        ),
    )
    .unwrap();
    let add = setup.run(
        &[
            "mcp",
            "add",
            "--repo",
            "db",
            "--",
            "/bin/bash",
            "srv/run.sh",
            &dir.display().to_string(),
        ],
        None,
    );
    assert_eq!(add.code, Some(0), "{add:?}");
    let list = setup.run(&["mcp", "list"], None);
    assert_eq!(list.code, Some(0), "{list:?}");
    assert_eq!(list.stdout, "db  repository  approved\n", "{list:?}");
    // The declaration is approved: the session starts it with no question.
    let ask = setup.run(&["ask", "hi"], None);
    assert_eq!(ask.code, Some(0), "stderr: {}", ask.stderr);
    assert!(
        !skipped(&ask, "db"),
        "the approved server was skipped: {}",
        ask.stderr
    );
    assert!(declares(&ask, "mcp__db__echo"), "no server tools: {ask:?}");
    // The same declaration in a fresh home has no approval on its
    // machine, and is skipped.
    let fresh = setup.root.path().join("h2");
    fs::create_dir_all(&fresh).unwrap();
    let second_source = setup.root.path().join("src2");
    install_provider(&fresh, &second_source, &provider);
    let list = setup.run_as(&fresh, &workspace, &["mcp", "list"], None);
    assert_eq!(list.code, Some(0), "{list:?}");
    assert_eq!(list.stdout, "db  repository  not approved\n", "{list:?}");
    let ask = setup.run_as(&fresh, &workspace, &["ask", "hi"], None);
    assert_eq!(ask.code, Some(0), "stderr: {}", ask.stderr);
    assert!(skipped(&ask, "db"), "the unapproved server ran: {ask:?}");
    assert!(!declares(&ask, "mcp__db__echo"), "{ask:?}");
}

/// Whether the ask run skipped the repository server `name` with a notice.
fn skipped(run: &StdoutRun, name: &str) -> bool {
    run.lines.iter().any(|line| {
        line["kind"] == "notice"
            && line["payload"]["code"] == "repository_code_skipped"
            && line["payload"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains(name)
    })
}

/// Whether the ask run declares the tool `name`.
fn declares(run: &StdoutRun, name: &str) -> bool {
    run.lines
        .iter()
        .filter(|line| line["kind"] == "preamble_built")
        .flat_map(|line| line["payload"]["tools"].as_array().unwrap().to_vec())
        .any(|tool| tool["name"] == name)
}

#[test]
fn a_url_with_userinfo_fails_naming_the_server_and_no_code() {
    let setup = Setup::new();
    let run = setup.run(&["mcp", "add", "db", "https://user@example.test/mcp"], None);
    assert_eq!(run.code, Some(1), "{run:?}");
    assert!(run.stderr.contains("db"), "{run:?}");
    assert!(!run.stderr.contains("config_invalid"), "{run:?}");
    assert!(
        !setup.home().join("config.json").exists(),
        "a file was written"
    );
}

#[test]
fn remove_of_an_absent_name_names_the_layers_holding_it() {
    let setup = Setup::new();
    let add = setup.run(
        &["mcp", "add", "--project", "b", "https://example.test/mcp"],
        None,
    );
    assert_eq!(add.code, Some(0), "{add:?}");
    let run = setup.run(&["mcp", "remove", "b"], None);
    assert_eq!(run.code, Some(2), "{run:?}");
    assert!(run.stderr.contains("project"), "{run:?}");
    let run = setup.run(&["mcp", "remove", "zzz"], None);
    assert_eq!(run.code, Some(2), "{run:?}");
    assert!(run.stderr.contains("no layer declares"), "{run:?}");
}

#[test]
fn a_shape_the_parser_cannot_say_is_a_usage_error() {
    let setup = Setup::new();
    for args in [
        // Both a url and a command.
        vec!["mcp", "add", "x", "https://example.test/mcp", "--", "npx"],
        // Neither.
        vec!["mcp", "add", "x"],
        // `-e` and `--secret` with a url.
        vec!["mcp", "add", "x", "-e", "A=1", "https://example.test/mcp"],
        vec![
            "mcp",
            "add",
            "x",
            "--secret",
            "A",
            "https://example.test/mcp",
        ],
        // No `=`.
        vec!["mcp", "add", "x", "-e", "A", "--", "npx"],
    ] {
        let run = setup.run(&args, Some("v\n"));
        assert_eq!(run.code, Some(2), "{args:?}: {run:?}");
        assert!(
            !setup.home().join("config.json").exists(),
            "{args:?}: a file was written"
        );
        assert!(
            !setup.home().join("credentials").exists(),
            "{args:?}: a secret was stored"
        );
    }
}
