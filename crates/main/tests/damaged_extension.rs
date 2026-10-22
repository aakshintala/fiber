//! Binary-level tests of a damaged extension (`docs/extensions.md`,
//! "Installing"): with one healthy extension and one directory whose
//! install record is missing or unreadable, `list` shows both, `install`
//! and `update` skip it with one line, and `remove` deletes it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use serde_json::{Value, json};
use support::{Deadline, Output, Setup, spawn_watched};

/// The damaged extension's full name and directory.
const BROKEN: &str = "github.com/aakshintala/fiber/providers/opencode";
/// The fix sentence `list` prints first on stdout.
const FIX: &str = "`opencode` is damaged; run `fiber extension reinstall opencode`.";
/// The skip line `install` and `update` print once on stderr.
const SKIP: &str = "fiber: `opencode` is damaged, so its dependency minimums are unknown and the versions chosen did not count them; run `fiber extension reinstall opencode`.";

/// A scripted text turn's event kinds in order, with the damaged-extension
/// startup notice, as the stream produces it.
const ASK_KINDS: [&str; 15] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "notice",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

impl Setup {
    fn new_with_sources() -> Self {
        let deadline = Deadline::start();
        let setup = Self {
            deadline,
            root: fakes::TempDir::under(std::path::Path::new("/tmp"), "fiber-ext-damaged"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        setup.source("healthy", "acme");
        setup.source("broken", BROKEN);
        setup.source("fresh", "example.com/acme/fresh");
        // Each installed source serves one pure-data provider, declared
        // under its manifest's `providers`.
        setup.data_provider("healthy", "acme", "acme", "m1");
        setup.data_provider("broken", BROKEN, "ocx", "m9");
        setup
    }

    fn broken_dir(&self) -> PathBuf {
        self.home().join("extensions/opencode")
    }

    /// A source package with this manifest name and version `v1`.
    fn source(&self, dir: &str, name: &str) {
        let path = self.root.path().join("src").join(dir);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("extension.json"),
            json!({"name": name, "version": "v1", "fiber": "0.0.0", "api": 1}).to_string(),
        )
        .unwrap();
    }

    fn src(&self, dir: &str) -> String {
        self.root
            .path()
            .join("src")
            .join(dir)
            .to_str()
            .unwrap()
            .to_owned()
    }

    /// A pure-data provider `provider` serving `model` in the source
    /// package `dir` named `name`: declared under its manifest and
    /// written as data.
    fn data_provider(&self, dir: &str, name: &str, provider: &str, model: &str) {
        let path = self.root.path().join("src").join(dir);
        let mut manifest = json!({"name": name, "version": "v1", "fiber": "0.0.0", "api": 1});
        manifest["providers"] = Value::Object(
            [(provider.to_owned(), json!(["http://127.0.0.1:1/v1"]))]
                .into_iter()
                .collect(),
        );
        fs::write(path.join("extension.json"), manifest.to_string()).unwrap();
        fs::create_dir_all(path.join("providers")).unwrap();
        fs::write(
            path.join("providers").join(format!("{provider}.json")),
            json!({
                "name": provider,
                "models": [{
                    "id": model,
                    "protocol": "openai-responses",
                    "base_url": "http://127.0.0.1:1/v1",
                    "context_window": 1000,
                }],
            })
            .to_string(),
        )
        .unwrap();
    }

    /// Runs `fiber extension` with `args` and no terminal, so it does not
    /// ask.
    #[track_caller]
    fn extension(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(["extension"])
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber extension {}` to exit", args.join(" ")),
            ),
        };
        watchdog.stand_down(self.deadline.cleanup_phase());
        Output::from(output)
    }

    /// Runs `fiber ask` from the workspace: its exit code, its stdout
    /// as JSON lines, and its stderr.
    #[track_caller]
    fn ask(&self, args: &[&str]) -> (Option<i32>, Vec<Value>, String) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path().join("workspace"))
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
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
        watchdog.stand_down(self.deadline.cleanup_phase());
        let lines = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (
            output.status.code(),
            lines,
            String::from_utf8(output.stderr).unwrap(),
        )
    }

    /// Runs `fiber` with `args` and `input` on stdin, no terminal, with
    /// the same watchdog and deadline as `extension`.
    #[track_caller]
    fn run_with_input(&self, args: &[&str], input: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        // Taking the pipe closes it once written.
        let mut pipe = child.stdin.take().unwrap();
        let input = input.to_owned();
        thread::spawn(move || match pipe.write_all(input.as_bytes()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => panic!("writing to the stdin of `fiber`: {e}"),
        });
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
        watchdog.stand_down(self.deadline.cleanup_phase());
        Output::from(output)
    }
}

/// How many of stderr's lines are the skip line.
fn skips(run: &Output) -> usize {
    run.stderr.lines().filter(|line| *line == SKIP).count()
}

#[track_caller]
fn damaged_flow(invalid: bool) {
    let setup = Setup::new_with_sources();
    let installed = setup.extension(&["install", &setup.src("healthy")]);
    assert_eq!(installed.code, Some(0), "{}", installed.stderr);
    let installed = setup.extension(&["install", &setup.src("broken")]);
    assert_eq!(installed.code, Some(0), "{}", installed.stderr);
    if invalid {
        fs::write(setup.broken_dir().join(".fiber.json"), "{").unwrap();
    } else {
        fs::remove_file(setup.broken_dir().join(".fiber.json")).unwrap();
    }

    let listed = setup.extension(&["list"]);
    assert_eq!(listed.code, Some(0), "{}", listed.stderr);
    assert_eq!(
        listed.stdout.lines().collect::<Vec<_>>(),
        [FIX, "acme v1 local"],
        "stdout: {:?}",
        listed.stdout
    );

    let run = setup.extension(&["install", &setup.src("fresh")]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(skips(&run), 1, "stderr: {:?}", run.stderr);
    let skip = run.stderr.lines().position(|line| line == SKIP).unwrap();
    let installed = run
        .stderr
        .lines()
        .position(|line| line.starts_with("fiber: installed"))
        .unwrap();
    assert!(skip < installed, "stderr: {:?}", run.stderr);

    let run = setup.extension(&["update"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(skips(&run), 1, "stderr: {:?}", run.stderr);

    let run = setup.extension(&["reinstall", "acme"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(skips(&run), 1, "stderr: {:?}", run.stderr);
    let skip = run.stderr.lines().position(|line| line == SKIP).unwrap();
    let installed = run
        .stderr
        .lines()
        .position(|line| line == "fiber: installed acme")
        .unwrap();
    assert!(skip < installed, "stderr: {:?}", run.stderr);

    let run = setup.extension(&["reinstall", "nothing-here"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(!setup.home().join("extensions/nothing-here").exists());
    assert!(setup.broken_dir().is_dir());

    let run = setup.run_with_input(&["models"], "");
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(run.stdout.contains("acme/m1"), "stdout: {:?}", run.stdout);
    assert!(!run.stdout.contains("ocx/m9"), "stdout: {:?}", run.stdout);
    assert_eq!(
        run.stderr.matches(FIX).count(),
        1,
        "stderr: {:?}",
        run.stderr
    );

    let run = setup.run_with_input(&["login", "acme"], "test-key\n");
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        run.stderr.matches(FIX).count(),
        1,
        "stderr: {:?}",
        run.stderr
    );

    let workspace = setup.root.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("s.json"),
        json!({"steps": [{"text": "Hello."}]}).to_string(),
    )
    .unwrap();
    let (code, lines, stderr) = setup.ask(&["ask", "--model", "scripted/s.json", "hi"]);
    assert_eq!(code, Some(0), "{stderr}");
    // `session_status` is written by an observer thread with no position
    // relative to the session's other lines, so it is set aside from the
    // ordered list (`docs/testing.md`, "Event streams").
    let kinds: Vec<&str> = lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ASK_KINDS);
    let notice = lines.iter().find(|l| l["kind"] == "notice").unwrap();
    assert_eq!(notice["payload"]["code"], "extension_failed");
    assert_eq!(notice["payload"]["message"], FIX);
    assert_eq!(notice["payload"]["extension"], "opencode");

    let run = setup.extension(&["remove", "opencode"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(!setup.broken_dir().exists());
}

#[test]
fn a_missing_record_is_listed_skipped_and_removable() {
    damaged_flow(false);
}

#[test]
fn an_unreadable_record_is_listed_skipped_and_removable() {
    damaged_flow(true);
}
