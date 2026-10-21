//! `host.tool` (`docs/extensions.md`, "Running a tool"): the Lua half and
//! the outcome table, with a fake runner standing in for the loop. Every
//! receive has one named wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use contract::events::{CallStatus, ToolCallCompleted};
use contract::shapes::{ContentPart, Failure};
use contract::tool::InnerCalls;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, Value, json};

use crate::{Error, LuaExtension};

/// Wall-clock bound on a wait for the extension's thread.
const WAIT: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `clock` as the extension's clock.
fn clocked(clock: &Arc<FakeClock>) -> Arc<dyn contract::clock::Clock> {
    Arc::clone(clock) as _
}

/// One recorded `host.tool` call: the extension, the tool and its arguments.
type Call = (String, String, Value);

/// What a `host.tool` call answers: its completion, or an error in the
/// calling code that writes no line.
type RunnerReply = Result<Box<ToolCallCompleted>, String>;

/// A fake runner: records each call, signals when one arrives, and answers
/// from a script, or parks the call when told to hold until released.
#[derive(Clone)]
pub(super) struct FakeRunner {
    inner: Arc<FakeInner>,
}

struct FakeInner {
    calls: Mutex<Vec<Call>>,
    entered: mpsc::Sender<()>,
    replies: Mutex<VecDeque<RunnerReply>>,
    hold: Mutex<Option<mpsc::Receiver<RunnerReply>>>,
}

impl FakeRunner {
    /// Answers each call from `replies` in order.
    fn scripted(
        replies: Vec<Result<Box<ToolCallCompleted>, String>>,
    ) -> (Self, mpsc::Receiver<()>) {
        let (entered_tx, entered_rx) = mpsc::channel();
        (
            Self {
                inner: Arc::new(FakeInner {
                    calls: Mutex::new(Vec::new()),
                    entered: entered_tx,
                    replies: Mutex::new(replies.into()),
                    hold: Mutex::new(None),
                }),
            },
            entered_rx,
        )
    }

    /// Parks the next call until `release` sends its answer.
    fn holding() -> (Self, mpsc::Receiver<()>, mpsc::Sender<RunnerReply>) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        (
            Self {
                inner: Arc::new(FakeInner {
                    calls: Mutex::new(Vec::new()),
                    entered: entered_tx,
                    replies: Mutex::new(VecDeque::new()),
                    hold: Mutex::new(Some(release_rx)),
                }),
            },
            entered_rx,
            release_tx,
        )
    }

    fn calls(&self) -> Vec<Call> {
        lock(&self.inner.calls).clone()
    }
}

impl InnerCalls for FakeRunner {
    fn run(
        &self,
        extension: &str,
        name: &str,
        arguments: Value,
    ) -> Result<Box<ToolCallCompleted>, String> {
        lock(&self.inner.calls).push((extension.to_owned(), name.to_owned(), arguments));
        match self.inner.entered.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        if let Some(release) = lock(&self.inner.hold).take() {
            // Held until the test releases it; a test that never releases
            // fails on its own deadline rather than hanging this thread.
            return Deadline::after(WAIT)
                .recv(&release)
                .expect("the test releases the held call");
        }
        lock(&self.inner.replies)
            .pop_front()
            .unwrap_or_else(|| Ok(completed("")))
    }
}

/// The extension `init`, whose tools run through `runner`.
fn extension(init: &str, clock: &Arc<FakeClock>) -> (fakes::TempDir, Arc<LuaExtension>) {
    let dir = fakes::TempDir::new("fiber-tool");
    std::fs::write(dir.path().join("init.lua"), init).unwrap();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(clock),
    ));
    (dir, ext)
}

/// Runs the tool `tool` on `{}` through `runner`, on a thread under `WAIT`.
#[track_caller]
fn tool_call(
    ext: &Arc<LuaExtension>,
    tool: &str,
    runner: FakeRunner,
) -> Result<Option<Value>, Error> {
    let ext = Arc::clone(ext);
    let tool = tool.to_owned();
    fakes::within(
        "ext.tool_run_asking",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let cancel = fakes::CancelToken::new();
            ext.tool_run_asking(
                &tool,
                Value::Object(Map::new()),
                &cancel,
                Some(Arc::new(runner) as Arc<dyn InnerCalls>),
            )
        },
    )
}

fn completed(text: &str) -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
        loaded: None,
    })
}

fn denied(reason: &str) -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Denied,
        reason: Some(reason.to_owned()),
        error: None,
        process: None,
        content: Vec::new(),
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
        loaded: None,
    })
}

fn failed(code: contract::ErrorCode, message: &str) -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Failed,
        reason: None,
        error: Some(Failure {
            code,
            message: message.to_owned(),
            retry_after_ms: None,
            provider: None,
        }),
        process: None,
        content: Vec::new(),
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
        loaded: None,
    })
}

/// A tool whose run is `body`, under `timeout_ms`.
fn tool_with_timeout(name: &str, timeout_ms: u64, body: &str) -> String {
    format!(
        "fiber.tool(\"{name}\", {{ description = \"d\", input_schema = {{ type = \"object\" }},\n\
         effects = {{ effects = {{ \"reads\" }}, reversible = true }}, timeout = {timeout_ms}, run = {body} }})\n"
    )
}

/// A tool whose run is `body`.
fn tool(name: &str, body: &str) -> String {
    tool_with_timeout(name, 5000, body)
}

#[test]
fn tool_returns_the_outcome_as_the_model_sees_it() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "fetch",
            "function() local o = host.tool(\"read\", { path = \"x\" })\n\
             return o.status .. \"|\" .. o.content[1].text .. \"|\" .. o.content[1].type end",
        ),
        &clock,
    );
    let (runner, _entered) = FakeRunner::scripted(vec![Ok(completed("file text"))]);
    let runner_calls = runner.clone();
    assert_eq!(
        tool_call(&ext, "fetch", runner).unwrap(),
        Some(Value::String("completed|file text|text".to_owned()))
    );
    // The runner saw the extension's name, the tool and the arguments.
    assert_eq!(
        runner_calls.calls(),
        vec![(
            "ext".to_owned(),
            "read".to_owned(),
            serde_json::json!({"path": "x"})
        )]
    );
}

#[test]
fn nil_and_empty_args_reach_the_runner_as_an_empty_object() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "fetch",
            "function() host.tool(\"read\") host.tool(\"read\", {}) host.tool(\"read\", { 1, 2 }) return \"done\" end",
        ),
        &clock,
    );
    let (runner, _entered) = FakeRunner::scripted(vec![
        Ok(completed("")),
        Ok(completed("")),
        Ok(completed("")),
    ]);
    let runner_calls = runner.clone();
    assert_eq!(
        tool_call(&ext, "fetch", runner).unwrap(),
        Some(Value::String("done".to_owned()))
    );
    let calls = runner_calls.calls();
    assert_eq!(calls.len(), 3);
    // `nil` and `{}` both read as no arguments; a non-empty array stays one.
    assert_eq!(calls[0].2, serde_json::json!({}));
    assert_eq!(calls[1].2, serde_json::json!({}));
    assert_eq!(calls[2].2, serde_json::json!([1, 2]));
}

#[test]
fn a_denied_outcome_is_an_ordinary_return_with_reason_and_no_error() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "fetch",
            "function() local o = host.tool(\"read\", { path = \"s\" })\n\
             local n = 0 for _ in pairs(o) do n = n + 1 end\n\
             return o.status .. \"|\" .. o.reason .. \"|\" .. tostring(o.error) .. \"|\" .. n .. \"|\" .. #o.content end",
        ),
        &clock,
    );
    // `details`, `artifact` and `changes` are set on the completion the
    // loop wrote, and none reaches the script: exactly `status`, `content`
    // and `reason`.
    let mut refused = denied("credentials");
    refused.details = Some(serde_json::json!({"diff": "x"}));
    refused.artifact = Some("artifacts/a.log".to_owned());
    refused.changes = Some(Vec::new());
    refused.process = Some(contract::shapes::Process {
        exit_code: None,
        signal: None,
        timed_out: false,
    });
    refused.control = Some(contract::events::Control::default());
    refused.changed_by = Some(vec!["fiber.test/tag".to_owned()]);
    refused.provider_item = Some(serde_json::json!({}));
    let (runner, _entered) = FakeRunner::scripted(vec![Ok(refused)]);
    assert_eq!(
        tool_call(&ext, "fetch", runner).unwrap(),
        Some(Value::String("denied|credentials|nil|3|0".to_owned()))
    );
}

#[test]
fn a_failed_outcome_is_an_ordinary_return_with_error_and_no_reason() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "fetch",
            "function() local o = host.tool(\"missing\", {})\n\
             return o.status .. \"|\" .. o.error.code .. \"|\" .. o.error.message .. \"|\" .. tostring(o.reason) end",
        ),
        &clock,
    );
    let (runner, _entered) =
        FakeRunner::scripted(vec![Ok(failed(contract::ErrorCode::ToolError, "boom"))]);
    assert_eq!(
        tool_call(&ext, "fetch", runner).unwrap(),
        Some(Value::String("failed|tool_error|boom|nil".to_owned()))
    );
}

#[test]
fn a_cancelled_outcome_is_an_ordinary_return() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "fetch",
            "function() local o = host.tool(\"slow\", {}) return o.status end",
        ),
        &clock,
    );
    let cancelled = Box::new(ToolCallCompleted {
        status: CallStatus::Cancelled,
        reason: None,
        error: None,
        process: None,
        content: Vec::new(),
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
        loaded: None,
    });
    let (runner, _entered) = FakeRunner::scripted(vec![Ok(cancelled)]);
    assert_eq!(
        tool_call(&ext, "fetch", runner).unwrap(),
        Some(Value::String("cancelled".to_owned()))
    );
}

#[test]
fn wrong_arguments_raise_strings_and_reach_no_runner() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "fetch",
            "function() local _, e1 = pcall(host.tool, 42, {})\n\
             local _, e2 = pcall(host.tool, \"read\", \"nope\")\n\
             local _, e3 = pcall(host.tool, \"read\", { path = function() end })\n\
             return type(e1) .. \"/\" .. type(e2) .. \"/\" .. type(e3) .. \"/\"\n\
             .. tostring(e1:find(\"host.tool\") ~= nil) .. \"/\"\n\
             .. tostring(e2:find(\"host.tool\") ~= nil) .. \"/\"\n\
             .. tostring(e3:find(\"host.tool\") ~= nil) end",
        ),
        &clock,
    );
    let (runner, _entered) = FakeRunner::scripted(vec![]);
    let runner_calls = runner.clone();
    assert_eq!(
        tool_call(&ext, "fetch", runner).unwrap(),
        Some(Value::String(
            "string/string/string/true/true/true".to_owned()
        ))
    );
    // A wrong argument never reaches the yield: the runner saw nothing.
    assert!(runner_calls.calls().is_empty());
}

#[test]
fn args_nested_past_the_cap_raise_and_reach_no_runner() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "deep",
            &format!(
                "function(args) local t = {{}} local top = t\n\
                 for _ = 2, args.n do local n = {{}} t[1] = n t = n end\n\
                 local ok, err = pcall(host.tool, \"read\", top)\n\
                 if ok then return \"reached\" end\n\
                 return tostring(err:find(\"host.tool\") ~= nil) .. \"/\" .. tostring(err:find(\"{}\") ~= nil) end",
                crate::host::MAX_DEPTH
            ),
        ),
        &clock,
    );
    // Values up to the JSON conversion limit reach the runner; one more
    // table raises naming `host.tool` and the depth, and reaches nothing.
    let (runner, _entered) = FakeRunner::scripted(vec![Ok(completed("")), Ok(completed(""))]);
    let runner_calls = runner.clone();
    let held = Arc::clone(&ext);
    let run = |n: i64| {
        let held = Arc::clone(&held);
        let runner = runner_calls.clone();
        fakes::within(
            "held.tool_run_asking",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                let cancel = fakes::CancelToken::new();
                held.tool_run_asking(
                    "deep",
                    serde_json::json!({"n": n}),
                    &cancel,
                    Some(Arc::new(runner) as Arc<dyn InnerCalls>),
                )
            },
        )
    };
    assert_eq!(
        run(i64::try_from(crate::host::MAX_DEPTH - 1).unwrap()).unwrap(),
        Some(Value::String("reached".to_owned()))
    );
    assert_eq!(
        run(i64::try_from(crate::host::MAX_DEPTH).unwrap()).unwrap(),
        Some(Value::String("reached".to_owned()))
    );
    assert_eq!(
        run(i64::try_from(crate::host::MAX_DEPTH + 1).unwrap()).unwrap(),
        Some(Value::String("true/true".to_owned()))
    );
    assert_eq!(runner_calls.calls().len(), 2);
}

#[test]
fn a_non_json_value_in_args_raises_the_generic_message() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "fetch",
            "function() local _, e = pcall(host.tool, \"read\", { path = function() end })\n\
             return tostring(e) end",
        ),
        &clock,
    );
    let (runner, _entered) = FakeRunner::scripted(vec![]);
    let runner_calls = runner.clone();
    assert_eq!(
        tool_call(&ext, "fetch", runner).unwrap(),
        Some(Value::String(
            "host.tool: `args` must be a table of JSON values".to_owned()
        ))
    );
    assert!(runner_calls.calls().is_empty());
}

#[test]
fn args_nested_past_the_to_json_cap_raise_the_depth_message() {
    let clock = FakeClock::new();
    // One table beyond the shared limit fails during `to_json`, before
    // arguments are serialized or handed to a runner.
    let (_dir, ext) = extension(
        &tool(
            "deep",
            &format!(
                "function() local top = {{}} local t = top\n\
                 for _ = 2, {} do local n = {{}} t[1] = n t = n end\n\
                 local _, err = pcall(host.tool, \"read\", top)\n\
                 return tostring(err) end",
                crate::host::MAX_DEPTH + 1
            ),
        ),
        &clock,
    );
    let (runner, _entered) = FakeRunner::scripted(vec![]);
    let runner_calls = runner.clone();
    assert_eq!(
        tool_call(&ext, "deep", runner).unwrap(),
        Some(Value::String(format!(
            "host.tool: `args` nests deeper than {}",
            crate::host::MAX_DEPTH
        )))
    );
    assert!(runner_calls.calls().is_empty());
}

#[test]
fn mutating_the_shared_args_after_the_yield_changes_nothing() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "fetch",
            "function() local args = { path = \"x\" }\n\
             local inner = coroutine.create(function() return host.tool(\"read\", args) end)\n\
             local _, tag, kind, spec = coroutine.resume(inner)\n\
             args.path = function() end\n\
             local result, message = coroutine.yield(tag, kind, spec)\n\
             local _, outcome = coroutine.resume(inner, result, message)\n\
             return outcome.content[1].text end",
        ),
        &clock,
    );
    let (runner, _entered) = FakeRunner::scripted(vec![Ok(completed("file text"))]);
    let runner_calls = runner.clone();
    assert_eq!(
        tool_call(&ext, "fetch", runner).unwrap(),
        Some(Value::String("file text".to_owned()))
    );
    // The check captured the arguments as JSON text before the yield, so
    // the function stored after it never reached the runner.
    assert_eq!(
        runner_calls.calls(),
        vec![(
            "ext".to_owned(),
            "read".to_owned(),
            serde_json::json!({"path": "x"})
        )]
    );
}

#[test]
fn a_runner_refusal_raises_its_string_for_pcall() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "selfish",
            "function() local ok, err = pcall(host.tool, \"selfish\", {})\n\
             return tostring(ok) .. \"|\" .. type(err) .. \"|\" .. err end",
        ),
        &clock,
    );
    let (runner, _entered) = FakeRunner::scripted(vec![Err(
        "host.tool: the tool `selfish` cannot run itself".to_owned(),
    )]);
    assert_eq!(
        tool_call(&ext, "selfish", runner).unwrap(),
        Some(Value::String(
            "false|string|host.tool: the tool `selfish` cannot run itself".to_owned()
        ))
    );
}

#[test]
fn a_runner_refusal_uncaught_fails_the_tool_call() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "selfish",
            "function() return host.tool(\"selfish\", {}) end",
        ),
        &clock,
    );
    let (runner, _entered) =
        FakeRunner::scripted(vec![
            Err("host.tool: inner-call depth exceeds 8".to_owned()),
        ]);
    match tool_call(&ext, "selfish", runner) {
        Err(Error::Lua { message, .. }) => {
            assert_eq!(message, "host.tool: inner-call depth exceeds 8")
        }
        other => panic!("the refusal fails the tool call: {other:?}"),
    }
}

#[test]
fn tool_in_the_entry_script_raises_before_it_yields() {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-tool");
    std::fs::write(
        dir.path().join("init.lua"),
        "local ok, err = pcall(host.tool, \"read\", {})\n\
         ENTRY = tostring(ok) .. \"/\" .. tostring(err:find(\"host.tool\") ~= nil)\n\
         fiber.command(\"go\", { timeout = 5000, run = function() return ENTRY end })\n",
    )
    .unwrap();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    ));
    let (runner, _entered) = FakeRunner::scripted(vec![]);
    let runner_calls = runner.clone();
    // The entry script ran: `host.tool` raised a string before any yield.
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.command",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || held.command("go", "")
        )
        .unwrap(),
        "false/true"
    );
    assert!(runner_calls.calls().is_empty());
}

#[test]
fn request_reads_name_and_args_and_maps_an_empty_array_to_an_object() {
    let lua = mlua::Lua::new();
    let spec = lua.create_table().unwrap();
    spec.set("name", "read").unwrap();
    spec.set("args", r#"{"path":"x"}"#).unwrap();
    let request = super::super::request_from(
        "tool",
        Some(&mlua::Value::Table(spec)),
        None,
        std::path::Path::new("/w"),
        1 << 20,
    )
    .unwrap()
    .expect("a tool yield is a request");
    let super::super::Request::Tool(request) = request else {
        panic!("a tool yield is a tool request");
    };
    assert_eq!(request.name, "read");
    assert_eq!(request.arguments, serde_json::json!({"path": "x"}));
    // An empty table yields `"[]"`, which reads as no arguments.
    let empty = lua.create_table().unwrap();
    empty.set("name", "read").unwrap();
    empty.set("args", "[]").unwrap();
    let request = super::super::request_from(
        "tool",
        Some(&mlua::Value::Table(empty)),
        None,
        std::path::Path::new("/w"),
        1 << 20,
    )
    .unwrap()
    .expect("a tool yield is a request");
    let super::super::Request::Tool(request) = request else {
        panic!("a tool yield is a tool request");
    };
    assert_eq!(request.arguments, serde_json::json!({}));
}

#[test]
fn outcome_names_only_status_content_and_reason_or_error() {
    let mut full = completed("hi");
    full.details = Some(serde_json::json!({"diff": "x"}));
    full.artifact = Some("artifacts/a.log".to_owned());
    full.changes = Some(Vec::new());
    full.process = Some(contract::shapes::Process {
        exit_code: None,
        signal: None,
        timed_out: false,
    });
    full.control = Some(contract::events::Control::default());
    full.changed_by = Some(vec!["fiber.test/tag".to_owned()]);
    full.provider_item = Some(serde_json::json!({}));
    let outcome = super::outcome(&full).unwrap();
    let mut keys: Vec<&str> = outcome
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    // Exactly `status` and `content`: a field added later never reaches the
    // script unless `outcome` names it.
    assert_eq!(keys, ["content", "status"]);
    let mut refused = denied("standing_rule");
    refused.details = Some(serde_json::json!({"diff": "x"}));
    let outcome = super::outcome(&refused).unwrap();
    let mut keys: Vec<&str> = outcome
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, ["content", "reason", "status"]);
}

/// Waits for this caller to park at its clock deadline before the test
/// advances fake time.
#[track_caller]
fn wait_parked(clock: &FakeClock, caller: std::thread::ThreadId, until: Instant) {
    assert!(
        clock.await_thread_parked(caller, Some(until), fakes::Deadline::after(WAIT)),
        "the caller never parked on the extension's clock within {WAIT:?}"
    );
}

/// What a `host.tool` outside a running tool call answers: an error in the
/// calling code, which `pcall` catches as a string.
const OUTSIDE: &str = "false|host.tool: called outside a running tool call";

#[test]
fn tool_from_a_hook_timer_effects_search_provider_or_command_is_an_error() {
    let clock = FakeClock::new();
    let (dir, ext) = extension(
        &format!(
            "TIMER_RESULT = \"unfired\"\n\
             fiber.hook(\"before_tool\", {{ timeout = 5000, on_failure = \"blocking\",\n\
             run = function() local ok, err = pcall(host.tool, \"read\", {{}}) return tostring(ok) .. \"|\" .. err end }})\n\
             fiber.command(\"via_command\", {{ timeout = 5000,\n\
             run = function() local ok, err = pcall(host.tool, \"read\", {{}}) return tostring(ok) .. \"|\" .. err end }})\n\
             fiber.command(\"arm\", {{ timeout = 5000, run = function()\n\
             host.after(100, function() local ok, err = pcall(host.tool, \"read\", {{}})\n\
             TIMER_RESULT = tostring(ok) .. \"|\" .. err end, {{ timeout = 5000 }})\n\
             return \"armed\" end }})\n\
             fiber.command(\"read_timer\", {{ timeout = 5000, run = function() return TIMER_RESULT end }})\n\
             fiber.search_backend(\"via_search\", {{ timeout = 5000,\n\
             run = function() local ok, err = pcall(host.tool, \"read\", {{}})\n\
             SEARCH_RESULT = tostring(ok) .. \"|\" .. err\n\
             return {{}} end }})\n\
             SEARCH_RESULT = \"unfired\"\n\
             fiber.command(\"read_search\", {{ timeout = 5000, run = function() return SEARCH_RESULT end }})\n\
             {}",
            tool(
                "via_tool",
                "function() local ok, err = pcall(host.tool, \"read\", {}) return tostring(ok) .. \"|\" .. err end",
            ) + "fiber.tool(\"via_effects\", { description = \"d\", input_schema = { type = \"object\" },\n\
             effects = function() local ok, err = pcall(host.tool, \"read\", {})\n\
             EFFECTS_RESULT = tostring(ok) .. \"|\" .. err\n\
             return { effects = {}, reversible = true } end,\n\
             timeout = 5000, run = function() return \"\" end })\n\
             EFFECTS_RESULT = \"unfired\"\n\
             fiber.command(\"read_effects\", { timeout = 5000, run = function() return EFFECTS_RESULT end })\n",
        ),
        &clock,
    );
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    std::fs::write(
        dir.path().join("providers/p.lua"),
        "fiber.provider(\"p\", { sign = { timeout = 5000,\n\
         run = function() local ok, err = pcall(host.tool, \"read\", {}) return tostring(ok) .. \"|\" .. err end } })\n",
    )
    .unwrap();
    let (runner, _entered) = FakeRunner::scripted(vec![]);
    let runner_calls = runner.clone();
    // A hook has no outer call to anchor one.
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.hook",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || held.hook("before_tool", 0, json!({}))
        )
        .unwrap(),
        Value::String(OUTSIDE.to_owned())
    );
    // A command is no tool call.
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.command",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || held.command("via_command", "")
        )
        .unwrap(),
        OUTSIDE
    );
    // The timer thread must be waiting at the due time before fake time moves.
    let (timer_done_tx, timer_done_rx) = mpsc::channel();
    crate::lua::on_timer_done(move || {
        let _sent = timer_done_tx.send(());
    });
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.command",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || held.command("arm", "")
        )
        .unwrap(),
        "armed"
    );
    let due = clock.origin() + Duration::from_millis(100);
    assert!(
        clock.await_parked(due, fakes::Deadline::after(WAIT)),
        "the timer waits for its due time"
    );
    clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_millis(100),
        &Deadline::after(WAIT),
    );
    Deadline::after(WAIT)
        .recv(&timer_done_rx)
        .expect("the timer callback completed");
    crate::lua::clear_timer_done_hook();
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.command",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || held.command("read_timer", "")
        )
        .unwrap(),
        OUTSIDE
    );
    // An effects function is no tool call.
    let held = Arc::clone(&ext);
    fakes::within(
        "held.tool_effects",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            held.tool_effects("via_effects", json!({}))
                .expect("the effects function ran")
        },
    );
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.command",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || held.command("read_effects", "")
        )
        .unwrap(),
        OUTSIDE
    );
    // A search backend is no tool call.
    let held = Arc::clone(&ext);
    fakes::within(
        "held.search_run",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let cancel = fakes::CancelToken::new();
            held.search_run("via_search", json!({}), &cancel)
        },
    )
    .expect("the search backend ran");
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.command",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || held.command("read_search", "")
        )
        .unwrap(),
        OUTSIDE
    );
    // A provider function is no tool call.
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.provider_call",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || held.provider_call("p", "sign", Value::Null)
        )
        .unwrap(),
        Value::String(OUTSIDE.to_owned())
    );
    // A tool call with no runner is none either.
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.tool_run",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || held.tool_run("via_tool", json!({}), &fakes::CancelToken::new())
        )
        .unwrap(),
        Some(Value::String(OUTSIDE.to_owned()))
    );
    // Nothing started tool work: the runner saw no call.
    assert!(runner_calls.calls().is_empty());
    drop(runner);
}

#[test]
fn the_runner_goes_when_the_call_returns() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &tool(
            "fetch",
            "function() local o = host.tool(\"read\", {}) return o.content[1].text end",
        ),
        &clock,
    );
    let (runner, entered, release_tx) = FakeRunner::holding();
    let caller = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let cancel = fakes::CancelToken::new();
        done_tx
            .send(caller.tool_run_asking(
                "fetch",
                json!({}),
                &cancel,
                Some(Arc::new(runner) as Arc<dyn InnerCalls>),
            ))
            .expect("the caller answered");
    });
    Deadline::after(WAIT)
        .recv(&entered)
        .expect("the runner was entered");
    // Registered right after the push: one runner while the call runs.
    assert_eq!(ext.runners_held(), 1);
    release_tx
        .send(Ok(completed("file text")))
        .expect("the test releases the held call");
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&done_rx)
            .expect("the call returned")
            .unwrap(),
        Some(Value::String("file text".to_owned()))
    );
    assert_eq!(ext.runners_held(), 0);
}

#[test]
fn a_timed_out_call_drops_its_runner_and_its_late_answer() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &format!(
            "{}{}",
            tool_with_timeout("slow", 200, "function() return host.tool(\"read\", {}) end",),
            tool("fast", "function() return \"fast\" end"),
        ),
        &clock,
    );
    let (runner, entered, release_tx) = FakeRunner::holding();
    let caller = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    let caller_thread = std::thread::spawn(move || {
        let cancel = fakes::CancelToken::new();
        done_tx
            .send(caller.tool_run_asking(
                "slow",
                json!({}),
                &cancel,
                Some(Arc::new(runner) as Arc<dyn InnerCalls>),
            ))
            .expect("the caller answered");
    });
    Deadline::after(WAIT)
        .recv(&entered)
        .expect("the runner was entered");
    assert_eq!(ext.runners_held(), 1);
    // The caller must be in its clock wait before the advance wakes it.
    wait_parked(
        &clock,
        caller_thread.thread().id(),
        clock.origin() + Duration::from_millis(200) + Duration::from_secs(1),
    );
    // At its timeout and the grace: the parked call fails by its deadline.
    clock.advance_when_parked(
        Some(clock.origin() + Duration::from_millis(1200)),
        Some(caller_thread.thread().id()),
        Duration::from_millis(1200),
        &Deadline::after(WAIT),
    );
    assert!(
        matches!(
            Deadline::after(WAIT)
                .recv(&done_rx)
                .expect("the slow call returned"),
            Err(Error::Timeout { .. })
        ),
        "the parked tool fails at its deadline"
    );
    caller_thread.join().expect("the caller thread exited");
    // The late answer is dropped: the VM still runs the next call, and no
    // runner is left behind.
    release_tx
        .send(Ok(completed("late")))
        .expect("the test releases the held call");
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.tool_run_asking",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                let cancel = fakes::CancelToken::new();
                held.tool_run_asking("fast", json!({}), &cancel, None)
            }
        )
        .unwrap(),
        Some(Value::String("fast".to_owned()))
    );
    assert_eq!(ext.runners_held(), 0);
}

#[test]
fn a_cancelled_call_drops_its_runner_and_its_late_answer() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &format!(
            "{}{}",
            tool("slow", "function() return host.tool(\"read\", {}) end"),
            tool("fast", "function() return \"fast\" end"),
        ),
        &clock,
    );
    let (runner, entered, release_tx) = FakeRunner::holding();
    let cancel = fakes::CancelToken::new();
    let fire = cancel.clone();
    let caller = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    let caller_thread = std::thread::spawn(move || {
        done_tx
            .send(caller.tool_run_asking(
                "slow",
                json!({}),
                &cancel,
                Some(Arc::new(runner) as Arc<dyn InnerCalls>),
            ))
            .expect("the caller answered");
    });
    Deadline::after(WAIT)
        .recv(&entered)
        .expect("the runner was entered");
    assert_eq!(ext.runners_held(), 1);
    // The caller must be in its clock wait before the cancel wakes it.
    wait_parked(
        &clock,
        caller_thread.thread().id(),
        clock.origin() + Duration::from_secs(6),
    );
    fire.cancel();
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&done_rx)
            .expect("the slow call returned")
            .unwrap(),
        None,
        "a cancelled call returns no content and no error"
    );
    caller_thread.join().expect("the caller thread exited");
    release_tx
        .send(Ok(completed("late")))
        .expect("the test releases the held call");
    let held = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "held.tool_run_asking",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                let cancel = fakes::CancelToken::new();
                held.tool_run_asking("fast", json!({}), &cancel, None)
            }
        )
        .unwrap(),
        Some(Value::String("fast".to_owned()))
    );
    assert_eq!(ext.runners_held(), 0);
}

/// A hook runs while a tool waits on its inner call.
#[test]
fn a_hook_runs_while_a_tool_waits_on_its_inner_call() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &format!(
            "fiber.hook(\"before_tool\", {{ timeout = 5000, on_failure = \"blocking\",\n\
             run = function() return {{ seen = true }} end }})\n\
             {}",
            tool(
                "fetch",
                "function() local o = host.tool(\"read\", {}) return o.content[1].text end",
            )
        ),
        &clock,
    );
    struct HookRunner {
        ext: Arc<LuaExtension>,
    }
    impl InnerCalls for HookRunner {
        fn run(
            &self,
            _extension: &str,
            name: &str,
            _arguments: Value,
        ) -> Result<Box<ToolCallCompleted>, String> {
            assert_eq!(name, "read");
            // The tool is parked, not a stream job, so its own extension's
            // hook runs while it waits.
            let seen = self
                .ext
                .hook("before_tool", 0, json!({}))
                .map_err(|e| e.to_string())?;
            assert_eq!(seen, json!({"seen": true}));
            Ok(completed("inner done"))
        }
    }
    let caller = Arc::clone(&ext);
    let runner = HookRunner {
        ext: Arc::clone(&ext),
    };
    assert_eq!(
        fakes::within(
            "caller.tool_run_asking",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                let cancel = fakes::CancelToken::new();
                caller.tool_run_asking(
                    "fetch",
                    json!({}),
                    &cancel,
                    Some(Arc::new(runner) as Arc<dyn InnerCalls>),
                )
            }
        )
        .unwrap(),
        Some(Value::String("inner done".to_owned()))
    );
}

#[test]
fn a_tool_runs_while_another_tool_waits_on_its_inner_call() {
    let clock = FakeClock::new();
    let (_dir, ext) = extension(
        &format!(
            "{}{}",
            tool(
                "fetch",
                "function() local o = host.tool(\"read\", {}) return o.content[1].text end",
            ),
            tool("other", "function() return \"other says hi\" end"),
        ),
        &clock,
    );
    struct ToolRunner {
        ext: Arc<LuaExtension>,
    }
    impl InnerCalls for ToolRunner {
        fn run(
            &self,
            _extension: &str,
            name: &str,
            _arguments: Value,
        ) -> Result<Box<ToolCallCompleted>, String> {
            assert_eq!(name, "read");
            // The parked tool holds no hook, and another tool runs in the
            // gaps beside it.
            let got = self
                .ext
                .tool_run("other", json!({}), &fakes::CancelToken::new())
                .map_err(|e| e.to_string())?;
            let Some(Value::String(text)) = got else {
                panic!("the other tool ran: {got:?}");
            };
            Ok(completed(&format!("saw {text}")))
        }
    }
    let caller = Arc::clone(&ext);
    let runner = ToolRunner {
        ext: Arc::clone(&ext),
    };
    assert_eq!(
        fakes::within(
            "caller.tool_run_asking",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                let cancel = fakes::CancelToken::new();
                caller.tool_run_asking(
                    "fetch",
                    json!({}),
                    &cancel,
                    Some(Arc::new(runner) as Arc<dyn InnerCalls>),
                )
            }
        )
        .unwrap(),
        Some(Value::String("saw other says hi".to_owned()))
    );
}
