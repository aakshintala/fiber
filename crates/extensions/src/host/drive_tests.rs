//! `host.drive` (`docs/extensions.md`, "Host calls"): the Lua half and the
//! scheduler arm, with a fake door standing in for the session.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use contract::ErrorCode;
use contract::events::{CommandInfo, CommandResult};
use contract::extension::Drive;
use contract::inbox::{Ack, Answer, Rejection};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, Value};

use crate::{Error, LuaExtension};

/// Wall-clock bound on a wait for the extension's thread.
const WAIT: Duration = Duration::from_secs(5);

/// Runs the blocking call `f` on a thread and receives its result with a
/// deadline: calling code that blocks is a wait too (`docs/testing.md`,
/// "Waits and timeouts").
#[track_caller]
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || done_tx.send(f()));
    match Deadline::after(WAIT).recv(&done_rx) {
        Ok(answer) => answer,
        Err(_) => panic!("the call did not return within {WAIT:?}"),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `clock` as the extension's clock.
fn clocked(clock: &Arc<FakeClock>) -> Arc<dyn contract::clock::Clock> {
    Arc::clone(clock) as _
}

/// One recorded `host.drive` call: the extension, the command and its arguments.
type Call = (String, String, Map<String, Value>);

/// A fake door: records each drive and answers from a queue, or parks the
/// call when told to hold.
#[derive(Clone)]
struct FakeDrive {
    inner: Arc<Inner>,
}

struct Inner {
    calls: Mutex<Vec<Call>>,
    called: mpsc::Sender<()>,
    replies: Mutex<VecDeque<Answer>>,
    hold: bool,
    parked: Mutex<Vec<Ack>>,
}

impl FakeDrive {
    fn new(replies: Vec<Answer>, hold: bool) -> (Self, mpsc::Receiver<()>) {
        let (called_tx, called_rx) = mpsc::channel();
        (
            Self {
                inner: Arc::new(Inner {
                    calls: Mutex::new(Vec::new()),
                    called: called_tx,
                    replies: Mutex::new(replies.into()),
                    hold,
                    parked: Mutex::new(Vec::new()),
                }),
            },
            called_rx,
        )
    }

    fn calls(&self) -> Vec<Call> {
        lock(&self.inner.calls).clone()
    }

    /// Answers every parked call with `answer`.
    fn answer_parked(&self, answer: Answer) {
        for parked in lock(&self.inner.parked).drain(..) {
            match &answer {
                Ok(result) => parked.0(Ok(result.clone())),
                Err(rejection) => parked.0(Err(rejection.clone())),
            }
        }
    }
}

impl Drive for FakeDrive {
    fn drive(&self, extension: &str, command: &str, args: Map<String, Value>, answer: Ack) {
        lock(&self.inner.calls).push((extension.to_owned(), command.to_owned(), args));
        match self.inner.called.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        if self.inner.hold {
            lock(&self.inner.parked).push(answer);
            return;
        }
        match lock(&self.inner.replies).pop_front().unwrap_or(Ok(None)) {
            Ok(result) => answer.0(Ok(result)),
            Err(rejection) => answer.0(Err(rejection)),
        }
    }
}

/// The extension `init`, with `drive` answering from `replies`.
fn extension(
    init: &str,
    clock: &Arc<FakeClock>,
    replies: Vec<Answer>,
) -> (fakes::TempDir, Arc<LuaExtension>, FakeDrive) {
    let dir = fakes::TempDir::new("fiber-drive");
    std::fs::write(dir.path().join("init.lua"), init).unwrap();
    let (drive, _called) = FakeDrive::new(replies, false);
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(clock),
    ));
    ext.set_driver(Arc::new(drive.clone()) as Arc<dyn Drive>);
    (dir, ext, drive)
}

fn busy() -> Answer {
    Err(Rejection {
        code: ErrorCode::Busy,
        message: "a turn is running".into(),
    })
}

#[test]
fn drive_returns_true_with_no_result() {
    let clock = FakeClock::new();
    let (_dir, ext, drive) = extension(
        "fiber.command(\"go\", { timeout = 5000, run = function() return tostring(host.drive(\"tools\", {})) end })\n",
        &clock,
        vec![Ok(None)],
    );
    let held = Arc::clone(&ext);
    assert_eq!(within(move || held.command("go", "")).unwrap(), "true");
    // The door saw the extension's name, the command and no arguments.
    assert_eq!(
        drive.calls(),
        vec![("ext".to_owned(), "tools".to_owned(), Map::new())]
    );
}

#[test]
fn drive_returns_the_result_table() {
    let clock = FakeClock::new();
    let (_dir, ext, _drive) = extension(
        "fiber.command(\"go\", { timeout = 5000, run = function() return host.drive(\"commands\", {}).commands[1].name end })\n",
        &clock,
        vec![Ok(Some(CommandResult::Commands {
            commands: vec![CommandInfo {
                name: "sync".into(),
                description: "Sync now.".into(),
                argument_hint: None,
                tag: "fiber.test/worker".into(),
            }],
        }))],
    );
    let held = Arc::clone(&ext);
    assert_eq!(within(move || held.command("go", "")).unwrap(), "sync");
}

#[test]
fn drive_rejection_raises_code_and_message_for_pcall() {
    let clock = FakeClock::new();
    let (_dir, ext, _drive) = extension(
        "fiber.command(\"go\", { timeout = 5000, run = function()\n\
         local ok, err = pcall(host.drive, \"steer\", { content = {{ type = \"text\", text = \"hi\" }} })\n\
         return tostring(ok) .. \" \" .. err.code .. \" \" .. err.message\n\
         end })\n",
        &clock,
        vec![busy()],
    );
    // `pcall` catches the table with exactly `code` and `message`.
    let held = Arc::clone(&ext);
    assert_eq!(
        within(move || held.command("go", "")).unwrap(),
        "false busy a turn is running"
    );
}

#[test]
fn drive_rejection_uncaught_fails_with_the_message() {
    let clock = FakeClock::new();
    let (_dir, ext, _drive) = extension(
        "fiber.command(\"go\", { timeout = 5000, run = function() return host.drive(\"steer\", {}) end })\n",
        &clock,
        vec![busy()],
    );
    // Uncaught, the callback fails with the message text, never a table dump.
    let held = Arc::clone(&ext);
    match within(move || held.command("go", "")) {
        Err(Error::Lua { message, .. }) => assert_eq!(message, "a turn is running"),
        other => panic!("the drive fails its callback: {other:?}"),
    }
}

#[test]
fn drive_before_drive_to_raises_closing() {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-drive");
    std::fs::write(
        dir.path().join("init.lua"),
        "fiber.command(\"go\", { timeout = 5000, run = function()\n\
         local ok, err = pcall(host.drive, \"tools\", {})\n\
         return err.code\n\
         end })\n",
    )
    .unwrap();
    // No `drive_to`: only the entry script could see that, so `closing`.
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    ));
    let held = Arc::clone(&ext);
    assert_eq!(within(move || held.command("go", "")).unwrap(), "closing");
}

#[test]
fn drive_argument_errors_raise_strings() {
    let clock = FakeClock::new();
    let (_dir, ext, _drive) = extension(
        "fiber.command(\"go\", { timeout = 5000, run = function()\n\
         local _, e1 = pcall(host.drive, 42)\n\
         local _, e2 = pcall(host.drive, \"tools\", \"nope\")\n\
         return type(e1) .. \"/\" .. type(e2) .. \"/\" .. tostring(e1:find(\"must be a string\") ~= nil) .. \"/\" .. tostring(e2:find(\"must be a table\") ~= nil)\n\
         end })\n",
        &clock,
        vec![],
    );
    // A wrong argument is an error in the calling code: a string, not a table.
    let held = Arc::clone(&ext);
    assert_eq!(
        within(move || held.command("go", "")).unwrap(),
        "string/string/true/true"
    );
}

#[test]
fn drive_in_the_entry_script_raises_closing() {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-drive");
    std::fs::write(
        dir.path().join("init.lua"),
        "host.drive(\"tools\", {})\n\
         fiber.command(\"go\", { timeout = 5000, run = function() end })\n",
    )
    .unwrap();
    let ext = LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    );
    // `drive_to` is set only after the entry script runs.
    let ext = Arc::new(ext);
    let held = Arc::clone(&ext);
    match within(move || held.commands()) {
        Err(Error::Lua { message, .. }) => {
            assert_eq!(message, "host.drive: not available while init.lua runs");
        }
        other => panic!("the entry script fails to load: {other:?}"),
    }
}

#[test]
fn drive_parked_past_deadline_drops_the_late_answer() {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-drive");
    std::fs::write(
        dir.path().join("init.lua"),
        "fiber.command(\"slow\", { timeout = 200, run = function() return tostring(host.drive(\"steer\", {})) end })\n\
         fiber.command(\"fast\", { timeout = 5000, run = function() return \"fast\" end })\n",
    )
    .unwrap();
    let (drive, called) = FakeDrive::new(Vec::new(), true);
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    ));
    ext.set_driver(Arc::new(drive.clone()) as Arc<dyn Drive>);
    let caller = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || done_tx.send(caller.command("slow", "")));
    // The drive reached the door, so the callback is parked on it.
    Deadline::after(WAIT)
        .recv(&called)
        .expect("the drive reached the door");
    // Past its timeout and the grace: the parked call fails by its deadline.
    clock.advance(Duration::from_secs(5));
    assert!(
        matches!(
            Deadline::after(WAIT)
                .recv(&done_rx)
                .expect("the slow call returned"),
            Err(Error::Timeout { .. })
        ),
        "the parked drive fails at its deadline"
    );
    // The late answer is dropped: the VM still runs the next command.
    drive.answer_parked(Ok(None));
    let held = Arc::clone(&ext);
    assert_eq!(within(move || held.command("fast", "")).unwrap(), "fast");
    assert_eq!(
        drive.calls(),
        vec![("ext".to_owned(), "steer".to_owned(), Map::new())]
    );
}

/// A fake door whose `drive` blocks until the test releases it: the held
/// driver that proves the extension's thread serves other work meanwhile.
struct BlockingDrive {
    calls: Mutex<Vec<Call>>,
    called: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl Drive for BlockingDrive {
    fn drive(&self, extension: &str, command: &str, args: Map<String, Value>, answer: Ack) {
        lock(&self.calls).push((extension.to_owned(), command.to_owned(), args));
        match self.called.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        // A fake held until the test releases it: no deadline of its own.
        lock(&self.release)
            .recv()
            .expect("the test releases the held drive");
        lock(&self.order).push("released");
        answer.0(Ok(None));
    }
}

#[test]
fn a_provider_runs_while_a_driven_command_is_held() {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-drive");
    std::fs::write(
        dir.path().join("init.lua"),
        "fiber.command(\"hold\", { timeout = 5000, run = function() return tostring(host.drive(\"steer\", {})) end })\n\
         fiber.provider(\"p\", { sign = { timeout = 5000, run = function() return {} end } })\n",
    )
    .unwrap();
    let (called_tx, called_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let order = Arc::new(Mutex::new(Vec::new()));
    let drive = Arc::new(BlockingDrive {
        calls: Mutex::new(Vec::new()),
        called: called_tx,
        release: Mutex::new(release_rx),
        order: Arc::clone(&order),
    });
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    ));
    ext.set_driver(Arc::clone(&drive) as Arc<dyn Drive>);
    // The driven command blocks inside the door, off the worker thread.
    let holder = Arc::clone(&ext);
    let (hold_tx, hold_rx) = mpsc::channel();
    std::thread::spawn(move || hold_tx.send(holder.command("hold", "")));
    Deadline::after(WAIT)
        .recv(&called_rx)
        .expect("the drive reached the door");
    // A provider callback runs while the driven command is still held.
    let caller = Arc::clone(&ext);
    within(move || {
        caller
            .provider_call("p", "sign", Value::Null)
            .expect("the provider ran while the drive was held")
    });
    lock(&order).push("provider_done");
    release_tx.send(()).expect("the test releases the drive");
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&hold_rx)
            .expect("the held command returned")
            .unwrap(),
        "true"
    );
    lock(&order).push("drive_finished");
    assert_eq!(
        lock(&order).as_slice(),
        ["provider_done", "released", "drive_finished"]
    );
    assert_eq!(
        lock(&drive.calls).clone(),
        vec![("ext".to_owned(), "steer".to_owned(), Map::new())]
    );
}

#[test]
fn drive_request_reads_command_and_args() {
    let lua = mlua::Lua::new();
    let spec = lua.create_table().unwrap();
    spec.set("command", "steer").unwrap();
    let args = lua.create_table().unwrap();
    args.set("text", "hi").unwrap();
    spec.set("args", args).unwrap();
    let request = super::super::request_from(
        "drive",
        Some(&mlua::Value::Table(spec)),
        None,
        std::path::Path::new("/w"),
        1 << 20,
    )
    .unwrap()
    .expect("a drive yield is a request");
    let super::super::Request::Drive(request) = request else {
        panic!("a drive yield is a drive request");
    };
    assert_eq!(request.command, "steer");
    assert_eq!(
        request.args,
        serde_json::json!({"text": "hi"})
            .as_object()
            .unwrap()
            .clone()
    );
    // An empty table is no arguments; a list is a calling-code error.
    let empty = lua.create_table().unwrap();
    empty.set("command", "tools").unwrap();
    empty.set("args", lua.create_table().unwrap()).unwrap();
    let request = super::super::request_from(
        "drive",
        Some(&mlua::Value::Table(empty)),
        None,
        std::path::Path::new("/w"),
        1 << 20,
    )
    .unwrap()
    .expect("a drive yield is a request");
    let super::super::Request::Drive(request) = request else {
        panic!("a drive yield is a drive request");
    };
    assert!(request.args.is_empty());
    let list = lua.create_table().unwrap();
    list.set("command", "tools").unwrap();
    let items = lua.create_table().unwrap();
    items.set(1, "x").unwrap();
    list.set("args", items).unwrap();
    assert!(
        super::super::request_from(
            "drive",
            Some(&mlua::Value::Table(list)),
            None,
            std::path::Path::new("/w"),
            1 << 20,
        )
        .is_err(),
        "a list is not arguments"
    );
}
