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

mod extension_harness;
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
    let source = "local small = _G.host.tool(\"read\", { path = \"small.txt\" })\n\
        local nested = load('local o = host.tool(\"read\", { path = \"nested.txt\" }); print(\"nested:\" .. o.content[1].text)')\n\
        nested()\n\
        local big = host.tool(\"shell\", { command = \"cat big.bin\" })\n\
        print(\"small:\" .. small.content[1].text)\n\
        print(\"big:\" .. big.content[1].text:sub(1, 4))\n\
        print(\"multi\", 1, true)\n\
        return \"ignored\"";
    let setup = reviewed_setup(&calling(source), &allow());
    std::fs::write(setup.workspace().join("small.txt"), "hello").unwrap();
    std::fs::write(setup.workspace().join("nested.txt"), "world").unwrap();
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
        8,
        "four printed and status lines plus three ledger rows: {text:?}"
    );
    let shell_row = lines.pop().unwrap();
    let nested_read_row = lines.pop().unwrap();
    let read_row = lines.pop().unwrap();
    assert_eq!(
        lines,
        [
            "nested:world",
            "small:hello",
            "big:BBBB",
            "multi\t1\ttrue",
            "Script finished"
        ]
    );
    assert_eq!(read_row, "- read completed");
    assert_eq!(nested_read_row, "- read completed");
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

#[test]
fn a_long_script_keeps_its_start_and_its_finished_ledger_suffix() {
    let source = "local small = host.tool(\"read\", { path = \"small.txt\" })\n\
        print(\"MARKER\")\n\
        print(string.rep(\"Q\", 20000))";
    let setup = reviewed_setup(&calling(source), &allow());
    std::fs::write(setup.workspace().join("small.txt"), "hello").unwrap();
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(resolved(&run)["payload"]["decision"], "allow");
    // The ordinary 16 KiB cap cuts the outer result: the model-facing text
    // begins with the printed start and names the cut.
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    assert!(completed["payload"]["artifact"].is_string());
    let text = completed_text(completed);
    assert!(
        text.starts_with(&format!("MARKER\n{}", "Q".repeat(100))),
        "the kept head holds the printed start: {text:?}"
    );
    assert!(
        text.contains("bytes cut"),
        "the middle names the cut: {text:?}"
    );
    // Head/tail keep preserves the status line and the ledger suffix for a
    // normal ledger size: the uncut call's row carries no path.
    assert!(
        text.ends_with("Script finished\n- read completed"),
        "the kept tail holds the status and ledger: {text:?}"
    );
    assert_eq!(run.of("turn_completed").len(), 1);
}

#[test]
fn a_failed_script_returns_its_failure_line_and_a_one_row_ledger() {
    let source = "local small = host.tool(\"read\", { path = \"small.txt\" })\n\
        print(\"saw:\" .. small.content[1].text)\n\
        error(\"boom\")";
    let setup = reviewed_setup(&calling(source), &allow());
    std::fs::write(setup.workspace().join("small.txt"), "hello").unwrap();
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(resolved(&run)["payload"]["decision"], "allow");
    // The printed output survives the failure, then the failure line, then
    // one ledger row for the inner call that finished before the error.
    let completed = completed_holding(&run, "Script failed:");
    assert_eq!(completed["payload"]["status"], "completed");
    let text = completed_text(completed);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "one printed line, the failure, one row: {text:?}"
    );
    assert_eq!(lines[0], "saw:hello");
    assert!(
        lines[1].starts_with("Script failed: "),
        "the failure line: {:?}",
        lines[1]
    );
    assert!(lines[1].contains("boom"), "the message: {:?}", lines[1]);
    assert_eq!(lines[2], "- read completed");
    assert!(!text.contains("Script finished"), "{text:?}");
    assert_eq!(run.of("turn_completed").len(), 1);
}

#[test]
fn a_denied_inner_call_returns_denied_inside_the_script() {
    let source = "local o = host.tool(\"read\", { path = \"small.txt\" })\n\
        print(\"status:\" .. o.status)\n\
        print(\"reason:\" .. tostring(o.reason))";
    let setup = reviewed_setup(&calling(source), &allow());
    setup.lua(
        "deny",
        "fiber.hook(\"before_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) if call.tool == \"read\" then return { refuse = \"nope\" } end end })\n",
    );
    std::fs::write(setup.workspace().join("small.txt"), "hello").unwrap();
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(resolved(&run)["payload"]["decision"], "allow");
    // A refused inner call is an ordinary return: the script sees `denied`
    // (`crates/loop/src/before_tool.rs` records a hook refusal under reason
    // `hook`; the hook's text stays in the denial message), finishes, and
    // the ledger rows it pathless.
    let completed = completed_holding(&run, "status:denied");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed_text(completed),
        "status:denied\nreason:hook\nScript finished\n- read denied"
    );
    assert_eq!(run.of("turn_completed").len(), 1);
}

#[test]
fn a_disabled_extension_leaves_its_tool_out_of_the_session() {
    let setup = Setup::new();
    // The full-name spelling: `docs/configuration.md`, "Keys", reads both
    // spellings as the same key, and this one needs no short name.
    script(&setup, "s.json", &json!({"steps": [{"text": "Hi."}]}));
    install_codemode(&setup);
    let mut extension = serde_json::Map::new();
    extension.insert("enabled".to_owned(), json!(false));
    let mut extensions = serde_json::Map::new();
    extensions.insert(CODEMODE_NAME.to_owned(), Value::Object(extension));
    let mut config = serde_json::Map::new();
    config.insert("extensions".to_owned(), Value::Object(extensions));
    write_json(&setup.home().join("config.json"), &Value::Object(config));
    let run = ask(&setup, "hi");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // The definitions sent at session start carry no `codemode` tool.
    let tools = &run.of("preamble_built")[0]["payload"]["tools"];
    assert!(
        !tools
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "codemode"),
        "{tools}"
    );
    assert_eq!(run.of("turn_completed").len(), 1);
}

#[test]
fn an_uncut_inner_call_keeps_an_after_tool_artifact_in_the_ledger() {
    let source = r#"local o = host.tool("read", { path = "small.txt" })
print(o.content[1].text)"#;
    let setup = reviewed_setup(&calling(source), &allow());
    setup.lua(
        "artifact",
        r#"fiber.hook("after_tool", { timeout = 30000, on_failure = "blocking",
           run = function(call) if call.tool == "read" then return { artifact = "full hook output" } end end })"#,
    );
    std::fs::write(setup.workspace().join("small.txt"), "hello").unwrap();
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let completed = completed_holding(&run, "Script finished");
    let text = completed_text(completed);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "printed line, status and ledger: {text:?}");
    assert_eq!(lines[0], "hello");
    assert_eq!(lines[1], "Script finished");
    let path = lines[2]
        .strip_prefix("- read completed ")
        .unwrap_or_else(|| panic!("the hook artifact path is in the row: {text:?}"));
    let stored = std::fs::read(run.session_dir(&setup).join(path)).unwrap();
    assert_eq!(stored, b"full hook output");
    assert_eq!(run.of("turn_completed").len(), 1);
}
