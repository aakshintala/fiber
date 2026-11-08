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

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Request, Watchdog};
use serde_json::{Value, json};
use support::{
    KillGroup, SessionGuard, Setup, function_call, hello, run_to_exit, stream, text_reply,
    write_json,
};

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

/// The reviewer script answering the first two stages with `allow`.
fn allow_twice() -> Value {
    json!({"steps": [{"text": "allow"}, {"text": "allow"}]})
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
fn invalid_utf8_prints_replacement_and_the_run_succeeds() {
    let source = "print(\"a\" .. string.char(255) .. \"b\")\n\
        print(\"mid\" .. string.char(255) .. string.char(254) .. \"end\")\n\
        print(\"h\" .. string.char(195, 169) .. \"llo\")\n\
        print(\"tail\" .. string.char(226, 130))";
    let setup = reviewed_setup(&calling(source), &allow());
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed_text(completed),
        "a�b\nmid��end\nhéllo\ntail�\nScript finished"
    );
}

#[test]
fn utf8_sub_never_splits_a_multibyte_character() {
    let source = "local s = \"h\" .. string.char(195, 169) .. \"llo\"\n\
        print(utf8.sub(s, 2, 2))\n\
        print(utf8.sub(s, 2, 3))\n\
        print(utf8.sub(\"hello\", -2))\n\
        print(utf8.sub(\"hi\", 1, 100))\n\
        print(utf8.sub(string.char(240, 159, 152, 128) .. \"ab\", 1, 1))\n\
        print(utf8.sub(s, -4, -4))\n\
        print(\"[\" .. utf8.sub(\"hello\", 3, 2) .. \"]\")\n\
        local ok = pcall(utf8.sub, \"a\" .. string.char(255))\n\
        print(ok)";
    let setup = reviewed_setup(&calling(source), &allow());
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed_text(completed),
        "é\nél\nlo\nhi\n😀\né\n[]\nfalse\nScript finished"
    );
}

#[test]
fn utf8_sub_stays_out_of_the_shared_table() {
    // One VM serves the whole session, with a fresh env per call: a field
    // the first call sets on its `utf8` table is gone in the second call
    // only when each env carries a copy, never the shared table.
    let session = json!({"steps": [
        {"tool_calls": [{"name": "codemode", "arguments": {"source": "utf8.marker = 42\nprint(utf8.sub(\"hi\", 2))"}}]},
        {"tool_calls": [{"name": "codemode", "arguments": {"source": "print(utf8.marker == nil)\nprint(utf8.sub(\"hi\", 2))"}}]},
        {"text": "Done."},
    ]});
    let setup = reviewed_setup(&session, &allow_twice());
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let first = completed_holding(&run, "i\nScript finished");
    assert_eq!(completed_text(first), "i\nScript finished");
    let second = completed_holding(&run, "true\ni\nScript finished");
    assert_eq!(completed_text(second), "true\ni\nScript finished");
    assert_eq!(run.of("turn_completed").len(), 1);
}

#[test]
fn null_tests_by_equality_and_indexing_null_errors() {
    let source = "local n = json.decode('null')\n\
        print(n == json.decode('null'))\n\
        local ok = pcall(function() return n.field end)\n\
        print(ok)";
    let setup = reviewed_setup(&calling(source), &allow());
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed_text(completed), "true\nfalse\nScript finished");
}

#[test]
fn an_uncut_row_names_no_artifact_while_the_script_sees_whole_bytes() {
    let big = "B".repeat(20 * 1024);
    let source = "local small = _G.host.tool(\"read\", { path = \"small.txt\" })\n\
        local nested = load('local o = host.tool(\"read\", { path = \"nested.txt\" }); print(\"nested:\" .. o.content[1].text)')\n\
        nested()\n\
        local big = host.tool(\"shell\", { command = \"cat big.bin\" })\n\
        print(\"small:\" .. small.content[1].text)\n\
        print(\"bytes:\" .. #big.content[1].text)\n\
        print(\"cut:\" .. tostring(big.content[1].text:find(\"bytes cut\", 1, true) ~= nil))\n\
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
        9,
        "five printed and status lines plus three ledger rows: {text:?}"
    );
    let shell_row = lines.pop().unwrap();
    let nested_read_row = lines.pop().unwrap();
    let read_row = lines.pop().unwrap();
    // The script saw the whole 20 KiB output: the file bytes plus the
    // shell tool's closing line (`text_of`/`exit_line` in
    // `crates/tools/src/shell.rs`), with no cut marker.
    let whole = format!("{big}\nExit code 0.\n");
    let bytes_line = format!("bytes:{}", whole.len());
    assert_eq!(
        lines,
        [
            "nested:world",
            "small:hello",
            bytes_line.as_str(),
            "cut:false",
            "multi\t1\ttrue",
            "Script finished"
        ]
    );
    assert_eq!(read_row, "- read completed");
    assert_eq!(nested_read_row, "- read completed");
    assert_eq!(shell_row, "- shell completed");
    // The inner completion the script resumed with holds every byte, with
    // no artifact and no kept ranges.
    let inner = completed_holding(&run, &"B".repeat(1024));
    assert_large_eq(completed_text(inner), &whole);
    assert_eq!(inner["payload"].get("artifact"), None);
    assert_eq!(inner["payload"].get("kept"), None);
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
fn a_200kb_read_reaches_whole_without_a_cut() {
    let big = "R".repeat(200 * 1024);
    let source = "local o = host.tool(\"read\", { path = \"big.txt\" })\n\
        print(\"bytes:\" .. #o.content[1].text)\n\
        print(\"cut:\" .. tostring(o.content[1].text:find(\"bytes cut\", 1, true) ~= nil))\n\
        print(\"cutline:\" .. tostring(o.content[1].text:find(\"longer than\", 1, true) ~= nil))\n\
        print(\"artifact:\" .. tostring(o.artifact))";
    let setup = Setup::new();
    script(&setup, "s.json", &calling(source));
    script(&setup, "r.json", &allow());
    // The default cap stays: the inner path runs the read uncapped, so the
    // script sees every byte with neither the loop's cut marker nor the
    // read tool's own long-line notice.
    write_json(
        &setup.home().join("config.json"),
        &json!({"reviewer": {"model": "scripted/r.json"}}),
    );
    install_codemode(&setup);
    std::fs::write(setup.workspace().join("big.txt"), &big).unwrap();
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(resolved(&run)["payload"]["decision"], "allow");
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed_text(completed),
        "bytes:204800\ncut:false\ncutline:false\nartifact:nil\nScript finished\n- read completed"
    );
    // The inner completion the script resumed with holds all 204800 bytes,
    // with no artifact and no kept ranges.
    let inner = completed_holding(&run, &"R".repeat(1024));
    assert_large_eq(completed_text(inner), &big);
    assert_eq!(inner["payload"].get("artifact"), None);
    assert_eq!(inner["payload"].get("kept"), None);
    assert_eq!(run.of("turn_completed").len(), 1);
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
fn a_printed_outcome_shows_its_text() {
    let source = "local o = host.tool(\"read\", { path = \"small.txt\" })\n\
        print(o)";
    let setup = reviewed_setup(&calling(source), &allow());
    std::fs::write(setup.workspace().join("small.txt"), "hello").unwrap();
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(resolved(&run)["payload"]["decision"], "allow");
    // A completed outcome prints exactly its text content: the `hello`
    // line, then the status line, then the one ledger row.
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed_text(completed),
        "hello\nScript finished\n- read completed"
    );
    assert_eq!(run.of("turn_completed").len(), 1);
}

#[test]
fn a_printed_failed_outcome_shows_its_error_and_status() {
    // Array arguments stay an array, which the loop fails
    // `invalid_arguments` as an ordinary return (`crates/loop/src/completion.rs`,
    // `failed`: the message in the content and in the error beside it).
    let source = "local o = host.tool(\"read\", { \"small.txt\" })\n\
        print(\"outcome:\")\n\
        print(o)\n\
        print(\"message:\")\n\
        print(o.error.message)\n\
        print(\"status:\")\n\
        print(o.status)";
    let setup = reviewed_setup(&calling(source), &allow());
    std::fs::write(setup.workspace().join("small.txt"), "hello").unwrap();
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(resolved(&run)["payload"]["decision"], "allow");
    // The printed outcome ends with the error message then the status, in
    // that order; the labelled blocks that follow pin what those hold.
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    let text = completed_text(completed);
    let lines: Vec<&str> = text.lines().collect();
    let outcome_at = lines.iter().position(|line| *line == "outcome:").unwrap();
    let message_at = lines.iter().position(|line| *line == "message:").unwrap();
    let status_at = lines.iter().position(|line| *line == "status:").unwrap();
    assert!(
        outcome_at < message_at && message_at < status_at,
        "{text:?}"
    );
    let outcome_block = &lines[outcome_at + 1..message_at];
    let message_block = &lines[message_at + 1..status_at];
    assert!(
        !message_block.is_empty(),
        "the error message block: {text:?}"
    );
    assert_eq!(lines[status_at + 1], "failed");
    assert_eq!(lines[status_at + 2], "Script finished");
    assert_eq!(lines[status_at + 3], "- read failed");
    assert_eq!(lines.len(), status_at + 4, "{text:?}");
    let tail = &outcome_block[outcome_block.len() - message_block.len() - 1..];
    assert_eq!(
        tail,
        &[message_block, &["failed"]].concat(),
        "the outcome ends with its error message then its status: {text:?}"
    );
    assert_eq!(run.of("turn_completed").len(), 1);
}

#[test]
fn a_printed_denied_outcome_shows_its_reason_and_status() {
    let source = "local o = host.tool(\"read\", { path = \"small.txt\" })\n\
        print(\"outcome:\")\n\
        print(o)\n\
        print(\"reason:\")\n\
        print(o.reason)\n\
        print(\"status:\")\n\
        print(o.status)";
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
    // A refused inner call is an ordinary return with the denial text in
    // the content and the reason beside it: the printed outcome ends with
    // the reason then the status, in that order.
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    let text = completed_text(completed);
    let lines: Vec<&str> = text.lines().collect();
    let outcome_at = lines.iter().position(|line| *line == "outcome:").unwrap();
    let reason_at = lines.iter().position(|line| *line == "reason:").unwrap();
    let status_at = lines.iter().position(|line| *line == "status:").unwrap();
    assert!(outcome_at < reason_at && reason_at < status_at, "{text:?}");
    let outcome_block = &lines[outcome_at + 1..reason_at];
    let reason_block = &lines[reason_at + 1..status_at];
    assert!(!reason_block.is_empty(), "the reason block: {text:?}");
    assert_eq!(lines[status_at + 1], "denied");
    assert_eq!(lines[status_at + 2], "Script finished");
    assert_eq!(lines[status_at + 3], "- read denied");
    assert_eq!(lines.len(), status_at + 4, "{text:?}");
    let tail = &outcome_block[outcome_block.len() - reason_block.len() - 1..];
    assert_eq!(
        tail,
        &[reason_block, &["denied"]].concat(),
        "the outcome ends with its reason then its status: {text:?}"
    );
    assert_eq!(run.of("turn_completed").len(), 1);
}

/// Installs provider `name` with model `m` on `protocol` at `url`.
fn install_url(setup: &Setup, name: &str, url: &str, protocol: &str) {
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
fn providers(setup: &Setup, parent: &ProviderServer, child: &ProviderServer) {
    install_url(setup, "pa", &parent.url(), "openai-responses");
    install_url(setup, "pb", &child.url(), "openai-responses");
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"}}),
    );
}

/// A standing allow for `delegate_spawn`.
fn standing_allow(setup: &Setup) {
    std::fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
        ),
    )
    .unwrap();
}

/// The session keeps serving instead of idling out: an exit proves the run
/// ended on its own.
fn slow_idle(setup: &Setup) {
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000}}),
    );
}

/// Arms the watchdog for `setup`: every child holds the test root on its
/// command line, so a failing test leaves no child behind. First, before
/// any fallible step.
fn arm(setup: &Setup) -> SessionGuard {
    SessionGuard::arm(setup.deadline, &setup.root.path().to_string_lossy())
}

/// A `codemode` call running `source`: the parent's first reply.
fn codemode_call(source: &str) -> Value {
    function_call("call_codemode", "codemode", &json!({"source": source}))
}

/// The finished `fiber ask` under the delegate drive: its exit code, stdout
/// lines and stderr.
struct Finished {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

/// Runs `fiber ask` to its exit under the test's deadline, in its own
/// process group with a watchdog beside it: expiry kills the group and
/// reaps it, and the watchdog kills the group when the test dies first
/// (`docs/testing.md`, "Running tests").
fn run_ask(setup: &Setup, prompt: &str) -> Finished {
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

/// The `tool_call_completed` in `lines` whose text holds `needle`: the
/// outer call's completion, since inner completions carry only one tool's
/// output.
#[track_caller]
fn finished_holding<'a>(lines: &'a [Value], needle: &str) -> &'a Value {
    lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
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

/// The single text of a `tool_call_completed`'s one text part.
#[track_caller]
fn finished_text(completed: &Value) -> &str {
    let content = completed["payload"]["content"]
        .as_array()
        .expect("content is a list of one text part");
    assert_eq!(content.len(), 1, "content holds one part");
    assert_eq!(content[0]["type"], "text");
    content[0]["text"].as_str().expect("the part holds text")
}

#[test]
fn a_spawn_row_names_its_job_id() {
    const SOURCE: &str = "local o = host.tool(\"delegate_spawn\", { description = \"scan\", prompt = \"say hello\", model = \"fiber:pb/m\" })\nprint(o)";
    let setup = Setup::new();
    let _guard = arm(&setup);
    let child = ProviderServer::start_responding(|_: &Request| hello()).unwrap();
    // Reviewer requests carry no tools, so they answer allow; the first
    // model request streams the codemode call; once its result is back the
    // turn ends on hello.
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if text.contains("Script finished") || text.contains("Script failed:") {
            return hello();
        }
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        stream(&[codemode_call(SOURCE)])
    })
    .unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);
    install_codemode(&setup);

    let finished = run_ask(&setup, "run it");
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    // The outer completion holding Script finished prints the receipt and
    // carries the spawn row with the new job's id.
    let completed = finished_holding(&finished.lines, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    let text = finished_text(completed);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "printed receipt, status, one row: {text:?}");
    assert_eq!(lines[1], "Script finished");
    let id = lines[2]
        .strip_prefix("- delegate_spawn completed job ")
        .expect("the spawn row names its job id");
    assert!(
        !id.is_empty() && !id.contains(' '),
        "the id is one token: {id:?}"
    );
    // The printed receipt names the same id: the outcome no longer hides
    // the job the script started.
    assert!(
        lines[0].contains(id),
        "the printed receipt names the job id: {text:?}"
    );
    // The id is the job the spawn started in this run.
    let started = finished
        .lines
        .iter()
        .find(|line| line["kind"] == "job_started")
        .expect("a job_started in the run");
    assert_eq!(started["payload"]["job_id"].as_str(), Some(id));
    assert_eq!(
        child.requests().len(),
        1,
        "the child called its provider once"
    );
}

#[test]
fn a_spawn_row_names_its_job_id_when_the_log_path_has_a_space() {
    const SOURCE: &str = "local o = host.tool(\"delegate_spawn\", { description = \"scan\", prompt = \"say hello\", model = \"fiber:pb/m\" })\nprint(o)";
    // A space in the root reaches the receipt's log path through the home
    // directory and the project key, which keeps spaces.
    let outer = fakes::TempDir::new("fh sp");
    let setup = Setup::new_under(outer.path());
    assert!(
        setup.root.path().to_string_lossy().contains(' '),
        "the spaced root carries the space the receipt path needs"
    );
    let _guard = arm(&setup);
    let child = ProviderServer::start_responding(|_: &Request| hello()).unwrap();
    // Reviewer requests carry no tools, so they answer allow; the first
    // model request streams the codemode call; once its result is back the
    // turn ends on hello.
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if text.contains("Script finished") || text.contains("Script failed:") {
            return hello();
        }
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        stream(&[codemode_call(SOURCE)])
    })
    .unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);
    install_codemode(&setup);

    let finished = run_ask(&setup, "run it");
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    // The receipt's log path holds the space, yet the row still names the
    // new job's id: the matcher reads only the receipt's id prefix.
    let completed = finished_holding(&finished.lines, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    let text = finished_text(completed);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "printed receipt, status, one row: {text:?}");
    let path = lines[0]
        .strip_prefix("Started delegate ")
        .and_then(|rest| rest.split_once(". Its log is "))
        .map(|(_, path)| path.strip_suffix('.').unwrap_or(path))
        .expect("the receipt names a log path");
    assert!(
        path.contains(' '),
        "the receipt's log path carries the space: {text:?}"
    );
    assert_eq!(lines[1], "Script finished");
    let id = lines[2]
        .strip_prefix("- delegate_spawn completed job ")
        .expect("the spawn row names its job id");
    assert!(
        !id.is_empty() && !id.contains(' '),
        "the id is one token: {id:?}"
    );
    assert!(
        lines[0].contains(id),
        "the printed receipt names the job id: {text:?}"
    );
    let started = finished
        .lines
        .iter()
        .find(|line| line["kind"] == "job_started")
        .expect("a job_started in the run");
    assert_eq!(started["payload"]["job_id"].as_str(), Some(id));
    assert_eq!(
        child.requests().len(),
        1,
        "the child called its provider once"
    );
}

#[test]
fn a_background_shell_row_names_its_job_id() {
    let source = r#"local o = host.tool("shell", { command = "printf hello", run_in_background = true })
print(o)"#;
    let setup = reviewed_setup(&calling(source), &allow());
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let completed = completed_holding(&run, "Script finished");
    let text = completed_text(completed);
    let receipt_id = text
        .lines()
        .find_map(|line| {
            line.strip_prefix("Job ")?
                .split_once(". Output:")
                .map(|(id, _)| id)
        })
        .expect("background receipt names its job id");
    assert!(
        text.lines()
            .any(|line| line == format!("- shell completed job {receipt_id}")),
        "ledger row names receipt id {receipt_id}: {text:?}"
    );
}

#[test]
fn a_fabricated_receipt_in_a_non_job_row_stays_plain() {
    let source = "local o = host.tool(\"read\", { path = \"trap.txt\" })\nprint(o.content[1].text)";
    let setup = reviewed_setup(&calling(source), &allow());
    std::fs::write(
        setup.workspace().join("trap.txt"),
        "Started delegate j_fake. Its log is /tmp/fake.log.",
    )
    .unwrap();
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(resolved(&run)["payload"]["decision"], "allow");
    // Receipt-shaped file content through another tool leaves the row
    // byte-identical: no job id.
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed_text(completed),
        "Started delegate j_fake. Its log is /tmp/fake.log.\nScript finished\n- read completed"
    );
    assert_eq!(run.of("turn_completed").len(), 1);
}

#[test]
fn a_denied_spawn_row_names_no_job() {
    let source = "local o = host.tool(\"delegate_spawn\", { description = \"scan\", prompt = \"say hello\", model = \"fiber:pb/m\" })\nprint(o.status)";
    let setup = reviewed_setup(&calling(source), &allow());
    setup.lua(
        "deny",
        "fiber.hook(\"before_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) if call.tool == \"delegate_spawn\" then return { refuse = \"nope\" } end end })\n",
    );
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(resolved(&run)["payload"]["decision"], "allow");
    // A refused spawn carries denial text, not the receipt sentence: the
    // row stays plain.
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed_text(completed),
        "denied\nScript finished\n- delegate_spawn denied"
    );
    assert_eq!(run.of("turn_completed").len(), 1);
}

#[test]
fn the_tool_description_lists_the_outcome_fields() {
    let source = "print(\"hi\")";
    let setup = reviewed_setup(&calling(source), &allow());
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // The session-start preamble carries every tool's description; the
    // codemode one states the outcome fields with a three-line example.
    let tools = &run.of("preamble_built")[0]["payload"]["tools"];
    let codemode = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "codemode")
        .expect("the preamble lists the codemode tool");
    let description = codemode["definition"]["description"]
        .as_str()
        .expect("the description is text");
    for field in ["status", "content", "artifact", "reason", "error"] {
        assert!(
            description.contains(field),
            "the description names {field}: {description:?}"
        );
    }
    for line in [
        "local o = host.tool(\"read\", { path = \"x\" })",
        "print(o)",
        "print(o.content[1].text)",
    ] {
        assert!(
            description.contains(line),
            "the description holds the example line {line:?}: {description:?}"
        );
    }
    assert_eq!(run.of("turn_completed").len(), 1);
}

#[test]
fn a_printed_failed_outcome_with_redacted_content_shows_its_error_and_status() {
    // Reading a missing file runs the tool and fails as an ordinary
    // return; an `after_tool` hook replaces the content, so the printed
    // error line is the only place the message survives: dropping the
    // error line changes the output.
    let source = "local o = host.tool(\"read\", { path = \"missing.txt\" })\n\
        print(\"outcome:\")\n\
        print(o)\n\
        print(\"message:\")\n\
        print(o.error.message)\n\
        print(\"status:\")\n\
        print(o.status)";
    let setup = reviewed_setup(&calling(source), &allow());
    setup.lua(
        "redact",
        r#"fiber.hook("after_tool", { timeout = 30000, on_failure = "blocking",
           run = function(call) if call.tool == "read" then return { content = "redacted" } end end })"#,
    );
    let run = ask(&setup, "run it");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(resolved(&run)["payload"]["decision"], "allow");
    let completed = completed_holding(&run, "Script finished");
    assert_eq!(completed["payload"]["status"], "completed");
    let text = completed_text(completed);
    let lines: Vec<&str> = text.lines().collect();
    let outcome_at = lines.iter().position(|line| *line == "outcome:").unwrap();
    let message_at = lines.iter().position(|line| *line == "message:").unwrap();
    let status_at = lines.iter().position(|line| *line == "status:").unwrap();
    assert!(
        outcome_at < message_at && message_at < status_at,
        "{text:?}"
    );
    let outcome_block = &lines[outcome_at + 1..message_at];
    let message_block = &lines[message_at + 1..status_at];
    assert!(
        !message_block.is_empty(),
        "the error message block: {text:?}"
    );
    assert_eq!(lines[status_at + 1], "failed");
    assert_eq!(lines[status_at + 2], "Script finished");
    assert_eq!(lines[status_at + 3], "- read failed");
    assert_eq!(lines.len(), status_at + 4, "{text:?}");
    // The redacted content, then the full error message, then the status.
    assert_eq!(
        outcome_block,
        &[&["redacted"], message_block, &["failed"]].concat(),
        "{text:?}"
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
