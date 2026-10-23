//! `host.delegate` (`docs/extensions.md`, "Host calls"): the Lua half's checks,
//! the entry-script refusal, the yielded spec and the reply shape, through
//! `LuaExtension` with `set_delegate` and a fake `HostDelegate`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::shapes::Failure;
use contract::{ErrorCode, JobId};
use fakes::Deadline;
use fakes::clock::FakeClock;
use mlua::{Lua, Value as LuaValue};
use serde_json::{Map, Value};

use crate::LuaExtension;

/// What a fake `start` answers.
type Answer = Result<JobId, Failure>;
/// A `start` the fake recorded: the extension's name and its arguments.
type Arrival = (String, Map<String, Value>);

/// A `HostDelegate` whose `start` records its call, signals it, holds the
/// call whose spec names `hold_description` until released, then answers as
/// configured. `bind` runs for every answered `Ok`, as the service calls it
/// before any child spawns.
struct Fake {
    calls: Mutex<Vec<Arrival>>,
    arrived: mpsc::Sender<(String, Map<String, Value>)>,
    hold_description: Option<String>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
    held_answer: Mutex<Answer>,
    answer: Mutex<Answer>,
    cancels: Mutex<Vec<Vec<JobId>>>,
    retains: Mutex<Vec<Vec<String>>>,
}

impl Fake {
    /// Answers every `start` with `answer`, never holding one.
    fn answering(answer: Answer) -> (Arc<Self>, mpsc::Receiver<Arrival>) {
        let (arrived_tx, arrived_rx) = mpsc::channel();
        (
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                arrived: arrived_tx,
                hold_description: None,
                release: Mutex::new(None),
                held_answer: Mutex::new(answer.clone()),
                answer: Mutex::new(answer),
                cancels: Mutex::new(Vec::new()),
                retains: Mutex::new(Vec::new()),
            }),
            arrived_rx,
        )
    }

    /// Holds the call whose spec names `hold` until its release is sent,
    /// answering it with `held`; every other call gets `answer` at once.
    fn holding(
        held: Answer,
        answer: Answer,
        hold: &str,
    ) -> (Arc<Self>, mpsc::Receiver<Arrival>, mpsc::Sender<()>) {
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        (
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                arrived: arrived_tx,
                hold_description: Some(hold.to_owned()),
                release: Mutex::new(Some(release_rx)),
                held_answer: Mutex::new(held),
                answer: Mutex::new(answer),
                cancels: Mutex::new(Vec::new()),
                retains: Mutex::new(Vec::new()),
            }),
            arrived_rx,
            release_tx,
        )
    }

    fn calls(&self) -> Vec<Arrival> {
        self.calls.lock().unwrap().clone()
    }

    fn retains(&self) -> Vec<Vec<String>> {
        self.retains.lock().unwrap().clone()
    }
}

impl contract::extension::HostDelegate for Fake {
    fn start(
        &self,
        extension: &str,
        arguments: &Map<String, Value>,
        bind: &mut dyn FnMut(&JobId),
    ) -> Answer {
        let arguments = arguments.clone();
        self.calls
            .lock()
            .unwrap()
            .push((extension.to_owned(), arguments.clone()));
        let _sent = self.arrived.send((extension.to_owned(), arguments.clone()));
        let held = self.hold_description.as_ref().is_some_and(|wanted| {
            arguments.get("description").and_then(Value::as_str) == Some(wanted.as_str())
        });
        if held {
            let release = self.release.lock().unwrap().take().expect("one held start");
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&release)
                .expect("the test releases the held start");
            let answer = self.held_answer.lock().unwrap().clone();
            if let Ok(id) = &answer {
                bind(id);
            }
            return answer;
        }
        let answer = self.answer.lock().unwrap().clone();
        if let Ok(id) = &answer {
            bind(id);
        }
        answer
    }

    fn retain(&self, loaded: &[String]) -> usize {
        self.retains.lock().unwrap().push(loaded.to_vec());
        0
    }

    fn cancel(&self, jobs: &[JobId]) -> usize {
        self.cancels.lock().unwrap().push(jobs.to_vec());
        0
    }
}

/// `clock` as the extension's clock.
fn clocked(clock: &Arc<FakeClock>) -> Arc<dyn contract::clock::Clock> {
    Arc::clone(clock) as _
}

/// An extension named `ext` whose entry script is `init`, with the fake
/// service set, on its own clock. The inbox is wired unless `inbox` is
/// false, so the no-inbox refusal is reachable.
fn rigged(
    init: &str,
    fake: Option<&Arc<Fake>>,
    inbox: bool,
) -> (fakes::TempDir, Arc<LuaExtension>, Arc<FakeClock>) {
    let dir = fakes::TempDir::new("fiber-delegate");
    std::fs::write(dir.path().join("init.lua"), init).unwrap();
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    ));
    if let Some(fake) = fake {
        ext.set_delegate(Arc::clone(fake) as Arc<dyn contract::extension::HostDelegate>);
    }
    if inbox {
        let (tx, _rx) = mpsc::channel();
        ext.deliver_to(tx);
    }
    (dir, ext, clock)
}

/// Registers a `go` command running `body`.
fn command_init(body: &str) -> String {
    format!("fiber.command(\"go\", {{ timeout = 5000, run = function()\n{body}\nend }})\n")
}

/// Runs the `go` command, failing the test when it does not return in time.
#[track_caller]
fn go(ext: &Arc<LuaExtension>) -> Result<String, crate::Error> {
    let held = Arc::clone(ext);
    fakes::within(
        "held.command",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || held.command("go", ""),
    )
}

/// The `pcall` verdict for a call with this spec and opts: `ok` with the
/// started id, a table refusal as its code and message, or a string refusal
/// as-is. The function and its arguments reach the `pcall` separately, so
/// the `pcall` catches the call: invoking it while building the `pcall`'s
/// arguments would evaluate it outside the `pcall`. Keeping the call whole
/// also keeps the chunk's errors free of a closure's position prefix.
fn probe(spec: &str, opts: &str) -> String {
    probe_after("", spec, opts)
}

/// As `probe`, with `setup` (usually spec-building statements) run first,
/// for specs no single expression builds.
fn probe_after(setup: &str, spec: &str, opts: &str) -> String {
    format!(
        "{setup}\n\
         local ok, err = pcall(host.delegate, {spec}, {opts})\n\
         if ok then return \"ok/\" .. tostring(err) end\n\
         if type(err) == \"table\" then return \"table/\" .. tostring(err.code) .. \"/\" .. tostring(err.message) end\n\
         return \"string/\" .. tostring(err)"
    )
}

const GOOD_SPEC: &str = "{ description = \"scan\", prompt = \"p\", model = \"deep\" }";
const GOOD_OPTS: &str = "{ timeout = 5000, run = function() end }";

/// A well-shaped spec and opts, for calls that reach the service.
const GOOD_CALL: (&str, &str) = (GOOD_SPEC, GOOD_OPTS);

#[test]
fn a_command_returns_the_id_the_service_started() {
    let (fake, _arrived) = Fake::answering(Ok(JobId("j_1".into())));
    let (_dir, ext, _clock) = rigged(
        &command_init(
            "return host.delegate({ description = \"scan\", prompt = \"p\", model = \"deep\" }, { timeout = 5000, run = function() end })",
        ),
        Some(&fake),
        true,
    );
    assert_eq!(go(&ext).unwrap(), "j_1");
    assert_eq!(
        fake.calls(),
        vec![(
            "ext".to_owned(),
            serde_json::json!({ "description": "scan", "prompt": "p", "model": "deep" })
                .as_object()
                .unwrap()
                .clone()
        )],
        "the service saw the extension's name and the spec as a JSON map"
    );
}

#[test]
fn bad_shapes_raise_before_any_start() {
    let (fake, _arrived) = Fake::answering(Ok(JobId("j_1".into())));
    for (spec, opts, message) in [
        (
            "42",
            GOOD_OPTS,
            "string/host.delegate: `spec` must be a table",
        ),
        (
            GOOD_SPEC,
            "nil",
            "string/host.delegate: `timeout` must be a whole number of milliseconds above 0",
        ),
        (
            GOOD_SPEC,
            "{ run = function() end }",
            "string/host.delegate: `timeout` must be a whole number of milliseconds above 0",
        ),
        (
            GOOD_SPEC,
            "{ timeout = 0, run = function() end }",
            "string/host.delegate: `timeout` must be a whole number of milliseconds above 0",
        ),
        (
            GOOD_SPEC,
            "{ timeout = 1.5, run = function() end }",
            "string/host.delegate: `timeout` must be a whole number of milliseconds above 0",
        ),
        (
            GOOD_SPEC,
            "{ timeout = \"1\", run = function() end }",
            "string/host.delegate: `timeout` must be a whole number of milliseconds above 0",
        ),
        (
            GOOD_SPEC,
            "{ timeout = 5000 }",
            "string/host.delegate: `run` must be a function",
        ),
    ] {
        let (_dir, ext, _clock) = rigged(&command_init(&probe(spec, opts)), Some(&fake), true);
        assert_eq!(go(&ext).unwrap(), message, "for {spec}, {opts}");
    }
    assert!(fake.calls().is_empty(), "no shape refusal reaches start");
}

#[test]
fn bad_specs_raise_before_any_start_and_store_no_handle() {
    let (fake, _arrived) = Fake::answering(Ok(JobId("j_1".into())));
    for (spec, message) in [
        (
            "{ \"a\", \"b\" }",
            "string/host.delegate: `spec` must be a table of named arguments",
        ),
        (
            "{ description = \"s\", prompt = \"p\", model = function() end }",
            "string/host.delegate: `spec` json: a function cannot be JSON",
        ),
    ] {
        let (_dir, ext, _clock) = rigged(&command_init(&probe(spec, GOOD_OPTS)), Some(&fake), true);
        assert_eq!(go(&ext).unwrap(), message, "for {spec}");
    }
    // A cycle nests past the snapshot's depth instead of overflowing it.
    let setup = "local spec = { description = \"s\" } spec.self = spec";
    let (_dir, ext, _clock) = rigged(
        &command_init(&probe_after(setup, "spec", GOOD_OPTS)),
        Some(&fake),
        true,
    );
    assert_eq!(
        go(&ext).unwrap(),
        "string/host.delegate: `spec` json: a table nests deeper than 125"
    );
    // Nesting past the snapshot's depth fails the same way.
    let mut deep = "1".to_owned();
    for _ in 0..130 {
        deep = format!("{{ v = {deep} }}");
    }
    let setup = format!("local spec = {{ description = \"s\", deep = {deep} }}");
    let (_dir, ext, _clock) = rigged(
        &command_init(&probe_after(&setup, "spec", GOOD_OPTS)),
        Some(&fake),
        true,
    );
    assert_eq!(
        go(&ext).unwrap(),
        "string/host.delegate: `spec` json: a table nests deeper than 125"
    );
    assert!(fake.calls().is_empty(), "no spec refusal reaches start");
}

#[test]
fn an_empty_table_snapshot_reaches_start_as_an_empty_object() {
    let (fake, _arrived) = Fake::answering(Ok(JobId("j_empty".into())));
    let (_dir, ext, _clock) = rigged(&command_init(&probe("{}", GOOD_OPTS)), Some(&fake), true);
    assert_eq!(go(&ext).unwrap(), "ok/j_empty");
    assert_eq!(
        fake.calls(),
        vec![("ext".to_owned(), Map::new())],
        "the empty table snapshots as {{}} and yields"
    );
}

#[test]
fn without_an_inbox_the_call_fails_closed_then_reaches_start() {
    let (fake, _arrived) = Fake::answering(Ok(JobId("j_1".into())));
    let (_dir, ext, _clock) = rigged(
        &command_init(&probe(GOOD_CALL.0, GOOD_CALL.1)),
        Some(&fake),
        false,
    );
    assert_eq!(
        go(&ext).unwrap(),
        "table/closing/host.delegate: the session is not running yet"
    );
    assert!(fake.calls().is_empty(), "no start before the inbox");
    let (tx, _rx) = mpsc::channel();
    ext.deliver_to(tx);
    assert_eq!(go(&ext).unwrap(), "ok/j_1", "the same call reaches start");
    assert_eq!(fake.calls().len(), 1, "one start after the inbox");
}

#[test]
fn service_refusals_reach_pcall_as_their_code_and_message() {
    for (code, message) in [
        (ErrorCode::DepthExceeded, "at depth 2 delegates stop"),
        (ErrorCode::ToolError, "ten delegates already run"),
    ] {
        let (fake, _arrived) = Fake::answering(Err(Failure {
            code: code.clone(),
            message: message.to_owned(),
            retry_after_ms: None,
            provider: None,
        }));
        let (_dir, ext, _clock) = rigged(
            &command_init(&probe(GOOD_CALL.0, GOOD_CALL.1)),
            Some(&fake),
            true,
        );
        let name = if code == ErrorCode::DepthExceeded {
            "depth_exceeded"
        } else {
            "tool_error"
        };
        assert_eq!(go(&ext).unwrap(), format!("table/{name}/{message}"));
    }
}

#[test]
fn a_closing_start_reaches_pcall_with_the_sweep_message() {
    let (fake, _arrived) = Fake::answering(Err(Failure {
        code: ErrorCode::Closing,
        message: "The session is ending, so no delegate was started.".to_owned(),
        retry_after_ms: None,
        provider: None,
    }));
    let (_dir, ext, _clock) = rigged(
        &command_init(&probe(GOOD_CALL.0, GOOD_CALL.1)),
        Some(&fake),
        true,
    );
    assert_eq!(
        go(&ext).unwrap(),
        "table/closing/The session is ending, so no delegate was started."
    );
}

#[test]
fn a_timer_cannot_change_what_the_held_start_sends() {
    let (fake, arrived, release) = Fake::holding(
        Ok(JobId("j_held".into())),
        Ok(JobId("j_other".into())),
        "held",
    );
    let init = "host.after(100, function() spec.prompt = \"changed\" end, { timeout = 5000 })\n"
        .to_owned()
        + &command_init(
            "spec = { description = \"held\", prompt = \"p\", model = \"deep\" }\nreturn host.delegate(spec, { timeout = 5000, run = function() end })",
        );
    let (_dir, ext, clock) = rigged(&init, Some(&fake), true);
    let held = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done_tx.send(held.command("go", ""));
    });
    let (extension, _arguments) = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&arrived)
        .expect("the held start arrives");
    assert_eq!(extension, "ext");
    let due = clock.origin() + Duration::from_millis(100);
    let timer = clock
        .mark_parked(due, fakes::Deadline::after(MUST_SUCCEED_WITHIN))
        .expect("the timer waits for its due time")
        .threads();
    assert_eq!(timer.len(), 1, "only the timer parks at its due time");
    let timer = timer.into_iter().next().expect("the timer thread");
    clock.advance_when_parked(
        Some(due),
        Some(timer),
        Duration::from_millis(100),
        &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
    );
    release.send(()).expect("the held start is released");
    assert_eq!(
        fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&done_rx)
            .expect("the command returns")
            .unwrap(),
        "j_held"
    );
    assert_eq!(
        fake.calls(),
        vec![(
            "ext".to_owned(),
            serde_json::json!({ "description": "held", "prompt": "p", "model": "deep" })
                .as_object()
                .unwrap()
                .clone()
        )],
        "the timer's write landed after the snapshot"
    );
}

#[test]
fn entry_script_and_a_missing_service_raise_closing() {
    let (fake, _arrived) = Fake::answering(Ok(JobId("j_1".into())));
    let init = format!("host.delegate({GOOD_SPEC}, {GOOD_OPTS})\n");
    let (_dir, ext, _clock) = rigged(&init, Some(&fake), true);
    let held = Arc::clone(&ext);
    match fakes::within(
        "held.commands",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || held.commands(),
    ) {
        Err(crate::Error::Lua { message, .. }) => {
            assert_eq!(message, "host.delegate: not available while init.lua runs");
        }
        other => panic!("the entry script fails to load: {other:?}"),
    }
    assert!(fake.calls().is_empty(), "the entry script starts nothing");
    let (_dir, ext, _clock) = rigged(&command_init(&probe(GOOD_CALL.0, GOOD_CALL.1)), None, true);
    assert_eq!(
        go(&ext).unwrap(),
        "table/closing/host.delegate: the session is closing"
    );
}

#[test]
fn a_hook_and_a_timer_can_each_call_it() {
    let (fake, arrived) = Fake::answering(Ok(JobId("j_hook".into())));
    let init = "fiber.hook(\"before_message\", { phase = \"transform\", timeout = 5000, on_failure = \"blocking\",\n".to_owned()
        + "run = function(call) host.delegate({ description = \"hooked\", prompt = \"p\", model = \"deep\" }, { timeout = 5000, run = function() end }) return { content = call.content } end })\n"
        + "host.after(100, function() host.delegate({ description = \"ticked\", prompt = \"p\", model = \"deep\" }, { timeout = 5000, run = function() end }) end, { timeout = 5000 })\n";
    let (_dir, ext, clock) = rigged(&init, Some(&fake), true);
    let returned = ext
        .hook("before_message", 0, serde_json::json!({ "content": [] }))
        .unwrap();
    assert_eq!(returned, serde_json::json!({ "content": [] }));
    let (extension, arguments) = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&arrived)
        .expect("the hook's start arrives");
    assert_eq!(extension, "ext");
    assert_eq!(arguments.get("description"), Some(&Value::from("hooked")));
    let due = clock.origin() + Duration::from_millis(100);
    clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_millis(100),
        &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let (_, arguments) = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&arrived)
        .expect("the timer's start arrives");
    assert_eq!(arguments.get("description"), Some(&Value::from("ticked")));
}

#[test]
fn an_effects_function_gets_invalid_arguments() {
    let (fake, _arrived) = Fake::answering(Ok(JobId("j_1".into())));
    let init = "fiber.tool(\"t\", { description = \"d\", input_schema = { type = \"object\" },\n"
        .to_owned()
        + "effects = function()\n"
        + "  local ok, err = pcall(host.delegate, { description = \"s\", prompt = \"p\", model = \"m\" }, { timeout = 5, run = function() end })\n"
        + "  if ok then error(\"the effects call started\", 0) end\n"
        + "  error(err.code .. \"/\" .. err.message, 0)\n"
        + "end,\n"
        + "timeout = 5000, run = function() return \"\" end })\n";
    let (_dir, ext, _clock) = rigged(&init, Some(&fake), true);
    match ext.tool_effects("t", Value::Null) {
        Err(crate::Error::Lua { message, .. }) => {
            assert_eq!(
                message,
                "invalid_arguments/host.delegate: not available here"
            );
        }
        other => panic!("the effects function is refused: {other:?}"),
    }
    assert!(fake.calls().is_empty(), "no effects call reaches start");
}

#[test]
fn a_second_timer_fires_while_one_start_is_held() {
    let (fake, arrived, release) = Fake::holding(
        Ok(JobId("j_first".into())),
        Ok(JobId("j_second".into())),
        "first",
    );
    let init = "host.after(100, function() host.delegate({ description = \"second\", prompt = \"p\", model = \"deep\" }, { timeout = 5000, run = function() end }) end, { timeout = 5000 })\n".to_owned()
        + &command_init(
            "return host.delegate({ description = \"first\", prompt = \"p\", model = \"deep\" }, { timeout = 5000, run = function() end })",
        );
    let (_dir, ext, clock) = rigged(&init, Some(&fake), true);
    let held = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done_tx.send(held.command("go", ""));
    });
    let (_, first) = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&arrived)
        .expect("the first start arrives");
    assert_eq!(first.get("description"), Some(&Value::from("first")));
    let due = clock.origin() + Duration::from_millis(100);
    let timer = clock
        .mark_parked(due, fakes::Deadline::after(MUST_SUCCEED_WITHIN))
        .expect("the timer waits for its due time")
        .threads();
    assert_eq!(timer.len(), 1, "only the timer parks at its due time");
    let timer = timer.into_iter().next().expect("the timer thread");
    clock.advance_when_parked(
        Some(due),
        Some(timer),
        Duration::from_millis(100),
        &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let (_, second) = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&arrived)
        .expect("the timer's start arrives");
    assert_eq!(second.get("description"), Some(&Value::from("second")));
    release.send(()).expect("the held start is released");
    assert_eq!(
        fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&done_rx)
            .expect("the command returns")
            .unwrap(),
        "j_first"
    );
}

#[test]
fn the_chunk_yields_the_snapshot_and_numbers_handles_from_one() {
    let lua = Lua::new();
    let host = lua.create_table().unwrap();
    let tag = lua.create_table().unwrap();
    let entry = std::rc::Rc::new(std::cell::Cell::new(false));
    let failure = lua
        .load("return function(code, message) return { code = code, message = message } end")
        .call::<mlua::Function>(())
        .unwrap();
    super::install(&lua, &host, &tag, entry, failure).unwrap();
    lua.globals().set("host", host).unwrap();
    // Two refusals store nothing: the next good call still gets handle 1.
    for refused in [
        "return host.delegate(42, { timeout = 1, run = function() end })",
        "return host.delegate({ description = \"s\" }, { timeout = 0, run = function() end })",
    ] {
        let Err(_) = lua.load(refused).call::<LuaValue>(()) else {
            panic!("{refused} is refused");
        };
    }
    let thread = lua
        .create_thread(
            lua.load(
                "return host.delegate({ description = \"s\", prompt = \"p\" }, { timeout = 5, run = function() end })",
            )
            .into_function()
            .unwrap(),
        )
        .unwrap();
    let mut resumed = thread.resume::<mlua::MultiValue>(()).unwrap();
    let yielded_tag: LuaValue = resumed.pop_front().unwrap();
    assert!(matches!(yielded_tag, LuaValue::Table(_)), "the tag yields");
    let kind: LuaValue = resumed.pop_front().unwrap();
    assert_eq!(
        kind,
        LuaValue::String(lua.create_string("delegate").unwrap())
    );
    let LuaValue::Table(yielded) = resumed.pop_front().unwrap() else {
        panic!("the yield carries its spec table");
    };
    let spec: String = yielded.get("spec").unwrap();
    assert_eq!(spec, "{\"description\":\"s\",\"prompt\":\"p\"}");
    let handle: u64 = yielded.get("handle").unwrap();
    assert_eq!(handle, 1, "no refused call stored a handle");
    // The id answers the call; a failure raises its table and clears the handle.
    let answered = thread.resume::<String>("j_9".to_owned()).unwrap();
    assert_eq!(answered, "j_9");
    // A failed reply raises its table under `pcall`, and clears the handle:
    // the next good call numbers from 2, not 3.
    let thread = lua
        .create_thread(
            lua.load(
                "local ok, err = pcall(host.delegate, { description = \"s\" }, { timeout = 5, run = function() end })\n\
                 if ok then return 'unexpected-ok:' .. tostring(err) end\n\
                 return err.code .. \"/\" .. err.message",
            )
            .into_function()
            .unwrap(),
        )
        .unwrap();
    let mut resumed = thread.resume::<mlua::MultiValue>(()).unwrap();
    let _: LuaValue = resumed.pop_front().unwrap();
    let _: LuaValue = resumed.pop_front().unwrap();
    let LuaValue::Table(yielded) = resumed.pop_front().unwrap() else {
        panic!("the yield carries its spec table");
    };
    let handle: u64 = yielded.get("handle").unwrap();
    assert_eq!(handle, 2, "the answered call kept its handle");
    let verdict = thread
        .resume::<String>((LuaValue::Nil, "tool_error".to_owned(), "full".to_owned()))
        .unwrap();
    assert_eq!(verdict, "tool_error/full");
    // The cleared handle is reused: the next good call gets handle 2.
    let thread = lua
        .create_thread(
            lua.load(
                "return host.delegate({ description = \"s\" }, { timeout = 5, run = function() end })",
            )
            .into_function()
            .unwrap(),
        )
        .unwrap();
    let mut resumed = thread.resume::<mlua::MultiValue>(()).unwrap();
    let _: LuaValue = resumed.pop_front().unwrap();
    let _: LuaValue = resumed.pop_front().unwrap();
    let LuaValue::Table(yielded) = resumed.pop_front().unwrap() else {
        panic!("the yield carries its spec table");
    };
    let handle: u64 = yielded.get("handle").unwrap();
    assert_eq!(handle, 2, "the failed call cleared its handle");
}

/// `delegate_to` sets the service on every loaded VM and retains the
/// loaded set once: the VM-less extension's name reaches `retain` too,
/// and afterwards the VM extension's command reaches the fake's `start`.
#[test]
fn delegate_to_sets_the_service_and_retains_the_loaded_set() {
    let home = crate::test_support::Home::new();
    home.install(
        "a",
        "fiber.command(\"go\", { timeout = 5000, run = function()\n\
         return host.delegate({ description = \"s\" }, { timeout = 5000, run = function() end })\n\
         end })\n",
    );
    home.install_bare("b");
    let session = home.load(&[]);
    let (fake, _arrived) = Fake::answering(Ok(JobId("j_1".into())));
    session.delegate_to(fake.clone());
    assert_eq!(
        fake.retains(),
        vec![vec!["fiber.test/a".to_owned(), "fiber.test/b".to_owned()]],
        "retain runs once with every loaded name"
    );
    let lua = session
        .lua_extension("fiber.test/a")
        .expect("extension a loads a VM");
    assert!(
        session.lua_extension("fiber.test/b").is_none(),
        "extension b loads no VM"
    );
    let (inbox_tx, _inbox_rx) = mpsc::channel();
    lua.deliver_to(inbox_tx);
    assert_eq!(lua.command("go", "").unwrap(), "j_1");
    assert_eq!(
        fake.calls(),
        vec![(
            "fiber.test/a".to_owned(),
            serde_json::json!({ "description": "s" })
                .as_object()
                .unwrap()
                .clone()
        )],
        "the command reaches the service after delegate_to"
    );
}
