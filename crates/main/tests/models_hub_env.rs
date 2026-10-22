//! `fiber models` and a session agree through the hub's captured
//! environment (`docs/invocation.md`, "A session's environment"): with a
//! hub whose captured environment lacks `DATABRICKS_HOST` and a client
//! process that has it, `fiber models` leaves the Databricks models out
//! naming `model_unconfigured`, and a session choosing one exits with the
//! hub-environment message. The reverse child, whose hub has the variable
//! and whose client lacks it, keeps the models.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::process::Command;

use serde_json::Value;
use support::*;

/// Installs the first-party `databricks` package, copied with its
/// thinking table the way `databricks.rs` installs it.
fn install(setup: &Setup) {
    let to = setup.root.path().join("pkg-databricks");
    support::package::copy_package("databricks", &to, "https://", "http://");
    std::fs::write(
        to.join("thinking.lua"),
        include_str!("../../../providers/databricks/thinking.lua"),
    )
    .unwrap();
    let output = run_to_exit(
        setup.deadline,
        "fiber extension install",
        setup.fiber(&["extension", "install", to.to_str().unwrap()]),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Starts `fiber hub serve` with `env` added to its environment and waits
/// for it to bind `run/hub`: without `SHELL` in its environment the hub
/// falls back to its inherited environment, so the hub's captured
/// environment is exactly `env` plus the test's fixed variables.
fn serve(setup: &Setup, env: &[(&str, &str)]) -> HubProc {
    let mut command = setup.fiber(&["hub", "serve"]);
    command.envs(env.iter().copied());
    let hub = HubProc::spawn_command(setup.deadline, &mut command);
    connect_when_bound(setup);
    hub
}

/// Connects to `run/hub` once the hub has bound it: the retry sleeps in
/// product code, the test only receives it with the one deadline.
fn connect_when_bound(setup: &Setup) {
    let deadline = setup.deadline;
    loop {
        let home = setup.home();
        match bounded(deadline, "the hub to bind run/hub", move || {
            let mut start = || -> std::io::Result<()> { Ok(()) };
            doors::hub::connect(&home, &mut start, &SystemClock)
        }) {
            Ok(_) => return,
            Err(error) => {
                assert!(
                    !deadline.left().is_zero(),
                    "waited until the deadline for the hub to bind run/hub: {error}"
                );
            }
        }
    }
}

/// Runs `fiber` with `args` and `env` added to the child's environment to
/// its exit under the test's deadline.
fn run(setup: &Setup, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut command: Command = setup.fiber(args);
    command.envs(env.iter().copied());
    run_to_exit(
        setup.deadline,
        &format!("fiber {}", args.join(" ")),
        command,
    )
}

/// The hub-environment message Part 1 prints for an unset placeholder.
fn expected_message(model: &str) -> String {
    format!(
        "The model `{model}` needs the setting `workspace` for its base URL, \
         which has no value, and `DATABRICKS_HOST` is not set in the hub's \
         environment (your login shell). Run `fiber login databricks`, \
         or set `workspace` in `config/databricks.json` in Fiber home, \
         or export it in your shell profile and run `fiber hub refresh`."
    )
}

/// The one notice `fiber models` prints for the Databricks models the
/// hub's environment leaves out: the package's model count, with the
/// per-model message's clauses and remedies.
fn expected_list_notice() -> String {
    let data: Value = serde_json::from_str(include_str!(
        "../../../providers/databricks/providers/databricks.json"
    ))
    .unwrap();
    let count = data["models"].as_array().unwrap().len();
    let subject = if count == 1 {
        "1 model needs the setting `workspace` for its base URL".to_owned()
    } else {
        format!("{count} models need the setting `workspace` for their base URL")
    };
    format!(
        "`databricks`: {subject}, \
         which has no value, and `DATABRICKS_HOST` is not set in the hub's \
         environment (your login shell). Run `fiber login databricks`, \
         or set `workspace` in `config/databricks.json` in Fiber home, \
         or export it in your shell profile and run `fiber hub refresh`.\n"
    )
}

#[test]
fn models_leaves_out_what_the_hub_lacks_and_the_session_names_it() {
    let setup = Setup::new();
    install(&setup);
    // The hub's captured environment holds the token but not the host;
    // the client holds both.
    let hub = serve(&setup, &[("DATABRICKS_TOKEN", "dapi-test")]);
    let client = [
        ("DATABRICKS_TOKEN", "dapi-test"),
        ("DATABRICKS_HOST", "adb-1.example.test"),
    ];

    let models = run(&setup, &["models"], &client);
    assert_eq!(
        models.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&models.stderr)
    );
    let stdout = String::from_utf8(models.stdout).unwrap();
    assert!(
        !stdout.contains("databricks/"),
        "the hub lacks the host, so no Databricks model lists: {stdout:?}"
    );
    let stderr = String::from_utf8(models.stderr).unwrap();
    assert_eq!(
        stderr,
        expected_list_notice(),
        "the list counts the unconfigured models once"
    );

    let ask = run(
        &setup,
        &[
            "ask",
            "--model",
            "databricks/databricks-claude-opus-5",
            "hi",
        ],
        &client,
    );
    assert_eq!(ask.status.code(), Some(1), "ask output: {ask:?}");
    let stdout = String::from_utf8(ask.stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{ask:?}");
    let line: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(line["kind"], "fiber_exited");
    assert_eq!(line["payload"]["error"]["code"], "model_unconfigured");
    assert_eq!(
        line["payload"]["error"]["message"].as_str().unwrap(),
        expected_message("databricks/databricks-claude-opus-5"),
        "{ask:?}"
    );

    hub.kill("TERM");
    hub.wait();
}

#[test]
fn models_keeps_what_the_hub_holds_though_the_client_lacks_it() {
    let setup = Setup::new();
    install(&setup);
    // The hub's captured environment holds the host; the client holds
    // only the token.
    let hub = serve(
        &setup,
        &[
            ("DATABRICKS_TOKEN", "dapi-test"),
            ("DATABRICKS_HOST", "adb-1.example.test"),
        ],
    );

    let models = run(&setup, &["models"], &[("DATABRICKS_TOKEN", "dapi-test")]);
    assert_eq!(
        models.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&models.stderr)
    );
    let stdout = String::from_utf8(models.stdout).unwrap();
    assert!(
        stdout.contains("databricks/databricks-claude-opus-5"),
        "the hub holds the host, so the model lists: {stdout:?}"
    );

    hub.kill("TERM");
    hub.wait();
}
