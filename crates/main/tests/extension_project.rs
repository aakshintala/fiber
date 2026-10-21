//! Project-scoped personal installations at the session boundary.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use fakes::{Deadline, ProviderServer};
use serde_json::{Value, json};
use support::{HELLO_KINDS, Run, Setup, hello_inline_completed, run_to_exit, write_json};

fn command(setup: &Setup, workspace: &Path, args: &[&str]) -> Output {
    let mut command = setup.fiber(args);
    command.current_dir(workspace);
    run_to_exit(setup.deadline, "project-scoped command", command)
}

fn install(setup: &Setup, workspace: &Path, source: &Path, project: bool) {
    let args = if project {
        vec![
            "extension",
            "install",
            "--project",
            source.to_str().unwrap(),
        ]
    } else {
        vec!["extension", "install", source.to_str().unwrap()]
    };
    let output = command(setup, workspace, &args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn ask(setup: &Setup, workspace: &Path, model: &str) -> Run {
    let output = command(setup, workspace, &["ask", "--model", model, "hi"]);
    let raw = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|line: &Value| line["kind"] != "session_status")
        .collect();
    if output.status.success() {
        let completed = lines
            .iter()
            .find(|line| line["kind"] == "text_completed")
            .unwrap();
        assert_eq!(completed["payload"]["text"], "Hello.");
        let id = lines[0]["session_id"].as_str().unwrap();
        let durable: String = raw
            .lines()
            .filter(|line| {
                let value: Value = serde_json::from_str(line).unwrap();
                value.get("seq").is_some()
            })
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(
            fs::read_to_string(
                cli::project_of(&setup.home(), workspace)
                    .unwrap()
                    .0
                    .join(id)
                    .join("events.jsonl")
            )
            .unwrap(),
            durable
        );
    }
    Run {
        code: output.status.code(),
        lines,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn ordinary_source(setup: &Setup) -> PathBuf {
    let source = setup.root.path().join("acme");
    write_json(
        &source.join("extension.json"),
        &json!({"name":"acme", "version":"v1.0.0", "fiber":"0.0.0", "api":1, "prompt":"prompt.md"}),
    );
    fs::write(source.join("prompt.md"), "ACME_PROJECT_PREAMBLE").unwrap();
    source
}

fn second_workspace(setup: &Setup) -> PathBuf {
    let workspace = setup.root.path().join("b");
    fs::create_dir(&workspace).unwrap();
    workspace
}

fn names(run: &Run) -> Vec<&str> {
    run.first("extensions_loaded")["payload"]["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value["name"].as_str().unwrap())
        .collect()
}

fn keys(setup: &Setup, workspace: &Path) -> Value {
    let (_, project) = cli::project_of(&setup.home(), workspace).unwrap();
    serde_json::from_slice(
        &fs::read(
            setup
                .home()
                .join("projects")
                .join(project.as_str())
                .join("config.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn ordinary_package_loads_only_in_a_and_repository_cannot_enable_it_in_b() {
    fakes::within(
        "ordinary scoped session journey",
        Deadline::after(Duration::from_secs(40)),
        || {
            let setup = Setup::new();
            let server = ProviderServer::start([
                hello_inline_completed(),
                hello_inline_completed(),
                hello_inline_completed(),
            ])
            .unwrap();
            setup.provider(&server);
            let a = setup.workspace();
            let b = second_workspace(&setup);
            install(&setup, &a, &ordinary_source(&setup), true);
            assert_eq!(
                keys(&setup, &a),
                json!({"extensions":{"acme":{"enabled":true}}})
            );
            let global: Value =
                serde_json::from_slice(&fs::read(setup.home().join("config.json")).unwrap())
                    .unwrap();
            assert_eq!(
                global,
                json!({"model":"fake/m", "extensions":{"acme":{"enabled":false}}})
            );
            assert!(!a.join(".fiber").exists());
            assert!(!b.join(".fiber").exists());
            let run_a = ask(&setup, &a, "fake/m");
            assert_eq!(run_a.code, Some(0), "{}", run_a.stderr);
            assert_eq!(run_a.kinds(), HELLO_KINDS);
            assert_eq!(names(&run_a), ["acme", "fake"]);
            let run_b = ask(&setup, &b, "fake/m");
            assert_eq!(run_b.code, Some(0), "{}", run_b.stderr);
            assert_eq!(run_b.kinds(), HELLO_KINDS);
            assert_eq!(names(&run_b), ["fake"]);
            let requests = server.requests();
            assert_eq!(requests.len(), 2);
            assert!(String::from_utf8_lossy(&requests[0].body).contains("ACME_PROJECT_PREAMBLE"));
            assert!(!String::from_utf8_lossy(&requests[1].body).contains("ACME_PROJECT_PREAMBLE"));
            write_json(
                &b.join(".fiber/config.json"),
                &json!({"extensions":{"acme":{"enabled":true}}}),
            );
            let run_b = ask(&setup, &b, "fake/m");
            assert_eq!(run_b.code, Some(0), "{}", run_b.stderr);
            let expected: Vec<&str> = HELLO_KINDS[..3]
                .iter()
                .copied()
                .chain(["notice"])
                .chain(HELLO_KINDS[3..].iter().copied())
                .collect();
            assert_eq!(run_b.kinds(), expected);
            assert_eq!(names(&run_b), ["fake"]);
            let notice = run_b.first("notice")["payload"]["message"]
                .as_str()
                .unwrap();
            assert!(notice.contains("extensions.acme.enabled"), "{notice}");
            assert!(
                notice.contains(b.join(".fiber/config.json").to_str().unwrap()),
                "{notice}"
            );
        },
    );
}

#[test]
fn scoped_provider_package_serves_a_but_neither_provider_is_available_in_b() {
    fakes::within(
        "provider scoped session journey",
        Deadline::after(Duration::from_secs(40)),
        || {
            let setup = Setup::new();
            let provider = support::package::opencode_go_ask_server(vec![hello_inline_completed()]);
            let control = ProviderServer::start([hello_inline_completed()]).unwrap();
            setup.provider(&control);
            let a = setup.workspace();
            let b = second_workspace(&setup);
            let source = support::package::copy_package(
                "opencode",
                &setup.root.path().join("opencode"),
                "https://opencode.ai",
                &provider.url(),
            );
            install(&setup, &a, &source, true);
            assert_eq!(
                keys(&setup, &a),
                json!({"extensions":{"opencode":{"enabled":true}}})
            );
            let global: Value =
                serde_json::from_slice(&fs::read(setup.home().join("config.json")).unwrap())
                    .unwrap();
            assert_eq!(
                global,
                json!({"model":"fake/m", "extensions":{"opencode":{"enabled":false}}})
            );
            let global_key = command(
                &setup,
                &a,
                &[
                    "config",
                    "set",
                    "providers.\"opencode-go\".credentials.default.env",
                    "FIBER_TEST_FAKE_KEY",
                ],
            );
            assert!(
                global_key.status.success(),
                "{}",
                String::from_utf8_lossy(&global_key.stderr)
            );
            let run_a = ask(&setup, &a, "opencode-go/muse-spark-1.3-contributor");
            assert_eq!(run_a.code, Some(0), "{}", run_a.stderr);
            assert_eq!(run_a.kinds(), HELLO_KINDS);
            let request_count = provider.requests().len();
            assert!(request_count > 0);
            for model in [
                "opencode-go/muse-spark-1.3-contributor",
                "opencode-zen/muse-spark-1.3-contributor",
            ] {
                let run_b = ask(&setup, &b, model);
                assert_eq!(run_b.code, Some(1));
                assert_eq!(run_b.kinds(), ["fiber_exited"]);
                assert_eq!(
                    run_b.first("fiber_exited")["payload"]["error"]["code"],
                    "extension_missing"
                );
            }
            assert_eq!(provider.requests().len(), request_count);
            let catalogue = command(&setup, &b, &["models", "--json"]);
            assert!(catalogue.status.success());
            let rows: Vec<Value> = String::from_utf8(catalogue.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(
                rows.iter()
                    .map(|row| row["model"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["fake/m"]
            );
            let run_b = ask(&setup, &b, "fake/m");
            assert_eq!(run_b.code, Some(0), "{}", run_b.stderr);
            assert_eq!(run_b.kinds(), HELLO_KINDS);
            assert_eq!(names(&run_b), ["fake"]);
            assert_eq!(control.requests().len(), 1);
        },
    );
}

#[test]
fn existing_global_install_keeps_loading_in_b_and_scoped_b_can_join_a() {
    fakes::within(
        "existing and joining scoped sessions",
        Deadline::after(Duration::from_secs(40)),
        || {
            let setup = Setup::new();
            let server =
                ProviderServer::start([hello_inline_completed(), hello_inline_completed()])
                    .unwrap();
            setup.provider(&server);
            let a = setup.workspace();
            let b = second_workspace(&setup);
            let source = ordinary_source(&setup);
            install(&setup, &b, &source, false);
            let before = fs::read(setup.home().join("config.json")).unwrap();
            install(&setup, &a, &source, true);
            assert_eq!(fs::read(setup.home().join("config.json")).unwrap(), before);
            for workspace in [&a, &b] {
                let run = ask(&setup, workspace, "fake/m");
                assert_eq!(run.code, Some(0));
                assert_eq!(run.kinds(), HELLO_KINDS);
                assert_eq!(names(&run), ["acme", "fake"]);
            }
        },
    );
}

#[test]
fn previously_scoped_package_adds_b_without_changing_global_false() {
    fakes::within(
        "joining a scoped extension",
        Deadline::after(Duration::from_secs(40)),
        || {
            let setup = Setup::new();
            let server =
                ProviderServer::start([hello_inline_completed(), hello_inline_completed()])
                    .unwrap();
            setup.provider(&server);
            let a = setup.workspace();
            let b = second_workspace(&setup);
            let source = ordinary_source(&setup);
            install(&setup, &a, &source, true);
            let before = fs::read(setup.home().join("config.json")).unwrap();
            install(&setup, &b, &source, true);
            assert_eq!(fs::read(setup.home().join("config.json")).unwrap(), before);
            assert_eq!(keys(&setup, &a), keys(&setup, &b));
            for workspace in [&a, &b] {
                let run = ask(&setup, workspace, "fake/m");
                assert_eq!(run.code, Some(0));
                assert_eq!(run.kinds(), HELLO_KINDS);
                assert_eq!(names(&run), ["acme", "fake"]);
            }
        },
    );
}

#[test]
fn git_worktrees_share_project_scope() {
    fakes::within(
        "git worktree project scope",
        Deadline::after(Duration::from_secs(40)),
        || {
            let setup = Setup::new();
            let server = ProviderServer::start([hello_inline_completed()]).unwrap();
            setup.provider(&server);
            let a = setup.workspace();
            support::git(setup.deadline, &a, &["init", "--quiet"]);
            support::git(
                setup.deadline,
                &a,
                &["commit", "--quiet", "--allow-empty", "-m", "initial"],
            );
            let b = setup.root.path().join("worktree");
            support::git(
                setup.deadline,
                &a,
                &["worktree", "add", "--detach", b.to_str().unwrap()],
            );
            install(&setup, &a, &ordinary_source(&setup), true);
            assert_eq!(
                cli::project_of(&setup.home(), &a).unwrap().1,
                cli::project_of(&setup.home(), &b).unwrap().1
            );
            let run = ask(&setup, &b, "fake/m");
            assert_eq!(run.code, Some(0));
            assert_eq!(run.kinds(), HELLO_KINDS);
            assert_eq!(names(&run), ["acme", "fake"]);
            assert!(!a.join(".fiber").exists());
            assert!(!b.join(".fiber").exists());
        },
    );
}

#[test]
fn failed_fresh_install_keeps_keys_but_no_session_loads_the_absent_package() {
    fakes::within(
        "failed scoped install session journey",
        Deadline::after(Duration::from_secs(40)),
        || {
            let setup = Setup::new();
            let server =
                ProviderServer::start([hello_inline_completed(), hello_inline_completed()])
                    .unwrap();
            setup.provider(&server);
            let a = setup.workspace();
            let b = second_workspace(&setup);
            let source = setup.root.path().join("broken");
            write_json(
                &source.join("extension.json"),
                &json!({"name":"acme", "version":"v1.0.0", "fiber":"0.0.0", "api":1, "install":["sh", "-c", "exit 1"]}),
            );
            let output = command(
                &setup,
                &a,
                &[
                    "extension",
                    "install",
                    "--project",
                    source.to_str().unwrap(),
                ],
            );
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("install"));
            assert!(!setup.home().join("extensions/acme").exists());
            assert_eq!(
                keys(&setup, &a),
                json!({"extensions":{"acme":{"enabled":true}}})
            );
            let global: Value =
                serde_json::from_slice(&fs::read(setup.home().join("config.json")).unwrap())
                    .unwrap();
            assert_eq!(global["extensions"]["acme"]["enabled"], false);
            for workspace in [&a, &b] {
                let run = ask(&setup, workspace, "fake/m");
                assert_eq!(run.code, Some(0));
                assert_eq!(run.kinds(), HELLO_KINDS);
                assert_eq!(names(&run), ["fake"]);
            }
            assert_eq!(server.requests().len(), 2);
        },
    );
}
