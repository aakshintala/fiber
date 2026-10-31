//! Binary-level tests of the first-party `codemode` extension (#3752,
//! `docs/extensions.md`, "Running a tool"): the built `fiber` runs in its
//! own process group with its own `FIBER_HOME`, the scripted provider as
//! the model and reviewer, and the real `extensions/codemode` package
//! installed from the repository.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::path::PathBuf;
use std::process::Output;

use serde_json::{Value, json};
use support::{Setup, run_to_exit, write_json};

/// The first-party package under test.
const CODEMODE_NAME: &str = "github.com/aakshintala/fiber/extensions/codemode";

/// One finished `fiber ask`: its exit code, its stdout lines parsed, and
/// its stderr.
struct Run {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

impl Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        Self {
            code: output.status.code(),
            lines: stdout
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    /// Every line of `kind`, in order.
    fn of(&self, kind: &str) -> Vec<&Value> {
        self.lines
            .iter()
            .filter(|line| line["kind"] == kind)
            .collect()
    }

    /// The session's directory, from its id.
    fn session_dir(&self, setup: &Setup) -> PathBuf {
        let workspace = std::fs::canonicalize(setup.workspace()).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        setup
            .home()
            .join("projects")
            .join(key)
            .join("sessions")
            .join(self.lines[0]["session_id"].as_str().unwrap())
    }
}

/// Writes `script` as `w/<name>`.
fn script(setup: &Setup, name: &str, script: &Value) {
    write_json(&setup.workspace().join(name), script);
}

/// Installs the real `codemode` package from the repository.
fn install_codemode(setup: &Setup) {
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extensions/codemode"),
        ),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// Configures the scripted reviewer; the model arrives on the command line.
fn configure(setup: &Setup) {
    write_json(
        &setup.home().join("config.json"),
        &json!({"reviewer": {"model": "scripted/r.json"}}),
    );
}

/// `fiber ask --model scripted/s.json <prompt>`, run from the workspace to
/// its exit.
fn ask(setup: &Setup, prompt: &str) -> Run {
    let mut command = setup.fiber(&["ask", "--model", "scripted/s.json", prompt]);
    command.current_dir(setup.workspace());
    Run::from(run_to_exit(setup.deadline, "fiber ask", command))
}

/// A setup whose scripted model and reviewer run `session` and `reviewer`.
fn reviewed_setup(session: &Value, reviewer: &Value) -> Setup {
    let setup = Setup::new();
    script(&setup, "s.json", session);
    script(&setup, "r.json", reviewer);
    configure(&setup);
    install_codemode(&setup);
    setup
}

/// The reviewer script answering the first stage with `allow`.
fn allow() -> Value {
    json!({"steps": [{"text": "allow"}]})
}

/// A session script making one `codemode` call with `source`, then done.
fn calling(source: &str) -> Value {
    json!({"steps": [
        {"tool_calls": [{"name": "codemode", "arguments": {"source": source}}]},
        {"text": "Done."},
    ]})
}

/// The `tool_call_completed` whose text holds `needle`: the outer call's
/// completion, since inner completions carry only one tool's output.
#[track_caller]
fn completed_holding<'a>(run: &'a Run, needle: &str) -> &'a Value {
    run.of("tool_call_completed")
        .into_iter()
        .find(|line| {
            line["payload"]["content"]
                .as_array()
                .is_some_and(|content| {
                    content.iter().any(|part| {
                        part["text"]
                            .as_str()
                            .is_some_and(|text| text.contains(needle))
                    })
                })
        })
        .unwrap_or_else(|| panic!("no tool_call_completed holding {needle:?}"))
}

/// The single text part of a `tool_call_completed`'s `content`.
#[track_caller]
fn completed_text(completed: &Value) -> &str {
    let content = completed["payload"]["content"]
        .as_array()
        .expect("content is a list of one text part");
    assert_eq!(content.len(), 1, "content holds one part");
    assert_eq!(content[0]["type"], "text");
    content[0]["text"].as_str().expect("the part holds text")
}

/// The run's one `permission_resolved`.
#[track_caller]
fn resolved(run: &Run) -> &Value {
    let resolved = run.of("permission_resolved");
    assert_eq!(resolved.len(), 1, "{resolved:?}");
    resolved[0]
}

/// Asserts two strings are byte-identical without printing either whole on
/// failure: only the two lengths and the first differing offset.
#[track_caller]
fn assert_large_eq(actual: &str, expected: &str) {
    if actual.len() != expected.len() {
        let offset = actual
            .bytes()
            .zip(expected.bytes())
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| actual.len().min(expected.len()));
        panic!(
            "large strings differ: actual {} bytes, expected {} bytes, first differing offset {offset}",
            actual.len(),
            expected.len()
        );
    }
    if let Some(offset) = actual
        .bytes()
        .zip(expected.bytes())
        .position(|(left, right)| left != right)
    {
        panic!(
            "large strings differ: actual {} bytes, expected {} bytes, first differing offset {offset}",
            actual.len(),
            expected.len()
        );
    }
}

#[test]
fn a_cut_row_names_its_artifact_and_an_uncut_row_names_none() {
    let big = "B".repeat(20 * 1024);
    let source = "local small = host.tool(\"read\", { path = \"small.txt\" })\n\
        local big = host.tool(\"shell\", { command = \"cat big.bin\" })\n\
        print(\"small:\" .. small.content[1].text)\n\
        print(\"big:\" .. big.content[1].text:sub(1, 4))\n\
        print(\"multi\", 1, true)\n\
        return \"ignored\"";
    let setup = reviewed_setup(&calling(source), &allow());
    std::fs::write(setup.workspace().join("small.txt"), "hello").unwrap();
    std::fs::write(setup.workspace().join("big.bin"), &big).unwrap();
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // Admission reviewed the outer `executes` call: the scripted reviewer
    // allowed it, and nothing else was reviewed.
    let verdict = resolved(&run);
    assert_eq!(verdict["payload"]["decided_by"], "reviewer");
    assert_eq!(verdict["payload"]["decision"], "allow");
    // The printed lines, then the status line, then one ledger row per
    // inner call that finished, in call order. The chunk's return value is
    // ignored and appended nowhere.
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    let text = completed_text(completed);
    let mut lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        6,
        "four printed and status lines plus two ledger rows: {text:?}"
    );
    let shell_row = lines.pop().unwrap();
    let read_row = lines.pop().unwrap();
    assert_eq!(
        lines,
        [
            "small:hello",
            "big:BBBB",
            "multi\t1\ttrue",
            "Script finished"
        ]
    );
    assert_eq!(read_row, "- read completed");
    let path = shell_row
        .strip_prefix("- shell completed ")
        .unwrap_or_else(|| panic!("the cut row names its artifact: {shell_row:?}"));
    assert!(!path.is_empty());
    // The ledger row's path holds that call's full output: the file bytes
    // plus the shell tool's closing line (`text_of`/`exit_line` in
    // `crates/tools/src/shell.rs`).
    let stored = std::fs::read(run.session_dir(&setup).join(path)).unwrap();
    let stored = String::from_utf8(stored).unwrap();
    assert_large_eq(&stored, &format!("{big}\nExit code 0.\n"));
    // The turn finished on the scripted follow-up.
    assert_eq!(
        run.of("text_completed").last().unwrap()["payload"]["text"],
        "Done."
    );
    assert_eq!(run.of("turn_completed").len(), 1);
    // The real first-party package is what loaded and registered the tool.
    let loaded = &run.of("extensions_loaded")[0]["payload"]["extensions"];
    assert!(
        loaded
            .as_array()
            .unwrap()
            .iter()
            .any(|ext| ext["name"] == CODEMODE_NAME),
        "{loaded}"
    );
}
