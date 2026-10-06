//! `fiber models`: the text table, `--json` lines, the substring search,
//! the marked default and the no-provider sentence, against a temporary
//! Fiber home.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::shapes::Failure;
use serde_json::{Value, json};

use super::run;

/// Fiber home and a workspace in a temporary directory, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("fiber-models"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        fs::create_dir_all(setup.root.path().join("workspace")).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// Installs the extension `extension` registering the provider `name`
    /// with these models.
    fn install(&self, extension: &str, name: &str, models: &Value) {
        let dir = self.home().join("extensions").join(extension);
        let data = json!({
            "name": name,
            "models": models,
        });
        fs::create_dir_all(dir.join("providers")).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({
                "name": extension,
                "version": "v0.0.0",
                "fiber": "0.0.0",
                "api": extensions::API,
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
    }

    fn write_config(&self, value: &Value) {
        fs::write(self.home().join("config.json"), value.to_string()).unwrap();
    }

    /// Runs the command with in-memory writers.
    fn run(&self, search: Option<&str>, json: bool) -> (Result<(), Failure>, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = run(
            &self.home(),
            &self.workspace(),
            search,
            json,
            &mut out,
            &mut err,
        );
        (
            result,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }
}

/// The two-provider home the text and JSON tests share: `acme/big` with a
/// context window and prices, `localhost/tiny` with neither.
fn two_provider_setup() -> Setup {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        &json!([{
            "id": "big",
            "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:1/v1",
            "context_window": 200000,
            "cost": {"input": 3.0, "output": 15.0},
        }]),
    );
    setup.install(
        "localhost",
        "localhost",
        &json!([{
            "id": "tiny",
            "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:1/v1",
        }]),
    );
    setup.write_config(&json!({"model": "acme/big"}));
    setup
}

#[test]
fn the_text_table_marks_exactly_the_default_row() {
    let setup = two_provider_setup();
    let (result, out, err) = setup.run(None, false);
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "  model           context  in $/M  out $/M\n\
         * acme/big        200000   3       15\n\
         \x20 localhost/tiny  -        -       -\n"
    );
}

#[test]
fn json_prints_the_same_rows_in_the_same_order() {
    let setup = two_provider_setup();
    let (result, out, err) = setup.run(None, true);
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "{\"model\":\"acme/big\",\"context_window\":200000,\
         \"input\":3.0,\"output\":15.0,\"default\":true}\n\
         {\"model\":\"localhost/tiny\",\"context_window\":null,\
         \"input\":null,\"output\":null,\"default\":false}\n"
    );
}

#[test]
fn the_search_matches_provider_slash_model_without_regard_to_case() {
    let setup = two_provider_setup();
    for search in ["acme", "ACME", "Big", "acme/b", "localhost/t"] {
        let (result, out, err) = setup.run(Some(search), false);
        result.unwrap();
        assert_eq!(err, "", "{search}");
        assert_eq!(out.lines().count(), 2, "{search}: {out:?}");
        assert!(
            out.lines()
                .all(|line| line.contains(&search.to_ascii_lowercase())
                    || line.starts_with("  model")),
            "{search}: {out:?}"
        );
    }
    let (result, out, err) = setup.run(Some("acme"), true);
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(out.lines().count(), 1);
    assert!(out.contains("\"acme/big\""), "{out:?}");
}

#[test]
fn a_search_with_no_match_prints_nothing() {
    let setup = two_provider_setup();
    for json in [false, true] {
        let (result, out, err) = setup.run(Some("nothing-matches-this"), json);
        result.unwrap();
        assert_eq!(out, "", "json={json}");
        assert_eq!(err, "", "json={json}");
    }
}

#[test]
fn a_bare_id_as_the_default_marks_the_providers_row() {
    let setup = two_provider_setup();
    setup.write_config(&json!({"model": "big"}));
    let (result, out, err) = setup.run(None, false);
    result.unwrap();
    assert_eq!(err, "");
    assert!(
        out.lines()
            .any(|line| line == "* acme/big        200000   3       15"),
        "{out:?}"
    );
    let (result, out, _) = setup.run(None, true);
    result.unwrap();
    assert!(out.contains("\"default\":true"), "{out:?}");
}

#[test]
fn a_default_that_does_not_resolve_marks_no_row_and_is_not_an_error() {
    for model in [json!({"model": "acme/missing"}), json!({})] {
        let setup = two_provider_setup();
        setup.write_config(&model);
        let (result, out, err) = setup.run(None, false);
        result.unwrap();
        assert_eq!(err, "");
        assert!(!out.lines().any(|line| line.starts_with('*')), "{out:?}");
        assert_eq!(out.lines().count(), 3, "{out:?}");
        let (result, out, _) = setup.run(None, true);
        result.unwrap();
        assert!(!out.contains("\"default\":true"), "{out:?}");
    }
}

#[test]
fn with_no_provider_it_names_extension_install_on_stderr() {
    let setup = Setup::new();
    for json in [false, true] {
        let (result, out, err) = setup.run(None, json);
        result.unwrap();
        assert_eq!(out, "", "json={json}");
        assert_eq!(
            err, "No provider is installed. Run `fiber extension install <name>` to install one.\n",
            "json={json}"
        );
    }
}

#[test]
fn a_tiered_model_shows_its_base_prices() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        &json!([{
            "id": "big",
            "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:1/v1",
            "context_window": 1000,
            "cost": {
                "input": 1.25,
                "output": 0.3,
                "tiers": [{
                    "input_tokens_above": 100000,
                    "input": 50.0,
                    "output": 60.0,
                    "cache_read": 5.0,
                    "cache_write": 6.0,
                }],
            },
        }]),
    );
    setup.write_config(&json!({"model": "acme/big"}));
    let (result, out, err) = setup.run(None, false);
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "  model     context  in $/M  out $/M\n\
         * acme/big  1000     1.25    0.3\n"
    );
    let (result, out, _) = setup.run(None, true);
    result.unwrap();
    assert_eq!(
        out,
        "{\"model\":\"acme/big\",\"context_window\":1000,\
         \"input\":1.25,\"output\":0.3,\"default\":true}\n"
    );
}

#[test]
fn a_model_listed_twice_prints_two_rows_and_marks_only_the_first() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        &json!([
            {
                "id": "big",
                "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1",
                "context_window": 7,
                "cost": {"input": 1.0, "output": 2.0},
            },
            {
                "id": "big",
                "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1",
                "context_window": 7,
                "cost": {"input": 1.0, "output": 2.0},
            },
        ]),
    );
    setup.write_config(&json!({"model": "acme/big"}));
    let (result, out, err) = setup.run(None, false);
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "  model     context  in $/M  out $/M\n\
         * acme/big  7        1       2\n\
         \x20 acme/big  7        1       2\n"
    );
    let (result, out, _) = setup.run(None, true);
    result.unwrap();
    assert_eq!(
        out,
        "{\"model\":\"acme/big\",\"context_window\":7,\
         \"input\":1.0,\"output\":2.0,\"default\":true}\n\
         {\"model\":\"acme/big\",\"context_window\":7,\
         \"input\":1.0,\"output\":2.0,\"default\":false}\n"
    );
}

/// The child's marker: set, the test runs `models` and exits with its code.
const CHILD: &str = "FIBER_CLI_TEST_CHILD";

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn models_exits_zero_and_prints_the_row() {
    // Runs in a child with a Fiber home holding one provider, so the exit
    // code is `models`' own. The parent checks the code and the row: a code
    // alone would not catch a `models` that returns 0 without printing.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(super::models(None, false));
    }
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        &json!([{
            "id": "big",
            "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:1/v1",
        }]),
    );
    let name = module_path!().split_once("::").unwrap().1;
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::models_exits_zero_and_prints_the_row"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", setup.home())
        .env(CHILD, "1")
        .current_dir(setup.workspace())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait_with_output().unwrap()));
    let Ok(output) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for `fiber models` to exit");
    };
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("acme/big"), "{stdout:?}");
}

#[test]
fn models_with_a_relative_fiber_home_is_a_usage_failure() {
    // `FIBER_HOME` must be an absolute path, so a relative one fails before
    // anything is read. It runs in a child so the process's own `FIBER_HOME`
    // cannot change the outcome.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(super::models(None, false));
    }
    let setup = Setup::new();
    let name = module_path!().split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::models_with_a_relative_fiber_home_is_a_usage_failure"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", "relative")
        .env(CHILD, "1")
        .current_dir(setup.workspace())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait().unwrap()));
    let Ok(status) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for `fiber models` to exit");
    };
    // `FIBER_HOME must be an absolute path` is a usage error.
    assert_eq!(status.code(), Some(2), "{status:?}");
}

#[test]
fn padding_counts_characters_not_bytes() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        &json!([
            {
                "id": "café",
                "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1",
            },
            {
                "id": "big",
                "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1",
            },
        ]),
    );
    setup.write_config(&json!({"model": "acme/big"}));
    let (result, out, err) = setup.run(None, false);
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "  model      context  in $/M  out $/M\n\
         \x20 acme/café  -        -       -\n\
         * acme/big   -        -       -\n"
    );
}
