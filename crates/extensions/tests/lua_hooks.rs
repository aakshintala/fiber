//! A session's `after_tool` hooks under their timeouts, on the fake clock
//! (`docs/extensions.md`, "When a hook fails" and "How an extension runs").

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::{Setup, install, manifest, write};
use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{CallStatus, ExtensionExec};
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks};
use contract::inbox::Delivery;
use extensions::{LuaExtension, SessionExtensions};
use fakes::clock::FakeClock;
use serde_json::Map;

/// The session's per-path lock, offered to `host.fs`: this test never
/// takes it, so it runs every call straight through.
struct NoLock;

impl contract::files::PathLock for NoLock {
    fn hold(&self, _path: &Path, run: &mut dyn FnMut()) {
        run();
    }

    fn hold_all(&self, _paths: &[PathBuf], run: &mut dyn FnMut()) {
        run();
    }
}

/// How long a test waits for a signal or an answer before failing. A hook
/// that is stopped returns on the fake clock, so this fires only when one
/// is never stopped.
const WAIT: Duration = Duration::from_secs(5);

/// When the hook cannot stop the VM, the caller waits 1 second more
/// (`docs/extensions.md`, "How an extension runs").
const GRACE: Duration = Duration::from_secs(1);

/// A hook that spins on its first call, after reading the signal module
/// `go_spin`, and appends `|<short>` on every later call.
fn spinning(on_failure: &str, timeout: u64, short: &str) -> String {
    format!(
        "local calls = 0\n\
         fiber.hook(\"after_tool\", {{ timeout = {timeout}, on_failure = \"{on_failure}\",\n\
           run = function(call)\n\
             calls = calls + 1\n\
             if calls == 1 then require(\"go_spin\") while true do end end\n\
             return {{ content = call.content .. \"|{short}\" }}\n\
           end }})\n"
    )
}

/// Installs `fiber.test/<short>` with the entry script `init`, and returns
/// its installed directory.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn installed(setup: &Setup, short: &str, init: &str) -> PathBuf {
    let name = format!("fiber.test/{short}");
    let src = setup.source(short, &manifest(&name), &[]);
    write(&src.join("init.lua"), init);
    install(&setup.home(), &src, "0.1.0").unwrap();
    setup.home().join("extensions").join(name.replace('/', "-"))
}

/// `require("go_spin")` in `dir` signals that the hook has started: the
/// loader opens the fifo for read, the writer here reports it and closes the
/// fifo, and the module reads empty.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn go_module(dir: &Path) -> mpsc::Receiver<()> {
    let path = dir.join("go_spin.lua");
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    assert!(made.unwrap().success(), "mkfifo {path:?}");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let held = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        drop(held);
    });
    rx
}

#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn load(setup: &Setup, overrides: &[&str], clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
    })
    .unwrap();
    // On its own thread under `WAIT`: the fake clock never ends a wait the
    // runtime does not end itself.
    let home = setup.home();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks));
    });
    Arc::new(
        rx.recv_timeout(WAIT)
            .expect("waited for the extensions to load"),
    )
}

/// Asks the hooks about a completed call whose output is `x`, on its own
/// thread.
fn ask(session: &Arc<SessionExtensions>) -> mpsc::Receiver<AfterToolAnswer> {
    let (tx, rx) = mpsc::channel();
    let session = Arc::clone(session);
    std::thread::spawn(move || {
        let arguments = Map::new();
        let answer = session.after_tool(&AfterToolCall {
            tool: "read",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: "x",
            details: None,
            process: None,
        });
        match tx.send(answer) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    rx
}

/// Asks once with the first hook spinning: waits for the spin to start and
/// the caller to park at `timeout` plus the grace, moves the clock by
/// `timeout`, and returns the answer.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn timed_out(
    session: &Arc<SessionExtensions>,
    clock: &Arc<FakeClock>,
    went: &mpsc::Receiver<()>,
    timeout: Duration,
) -> AfterToolAnswer {
    let asked = clock.now();
    let answer = ask(session);
    went.recv_timeout(WAIT)
        .expect("waited for the hook to start");
    assert!(
        clock.await_parked(asked + timeout + GRACE, WAIT),
        "waited for the caller to park at the hook's grace"
    );
    clock.advance(timeout);
    answer.recv_timeout(WAIT).expect("waited for the answer")
}

fn content(answer: &AfterToolAnswer) -> Option<&str> {
    match &answer.outcome {
        AfterToolOutcome::Changed { content, .. } => content.as_deref(),
        AfterToolOutcome::Unchanged | AfterToolOutcome::Withheld { .. } => None,
    }
}

#[test]
#[allow(clippy::indexing_slicing, reason = "the length is asserted first")]
#[allow(clippy::expect_used, reason = "a failure is the test's")]
fn a_non_blocking_hook_past_its_timeout_is_dropped_and_the_next_extension_still_runs() {
    let setup = Setup::new();
    let dir = installed(&setup, "a", &spinning("non-blocking", 50, "a"));
    installed(
        &setup,
        "b",
        "fiber.hook(\"after_tool\", { timeout = 1000, on_failure = \"blocking\",\n\
           run = function(call) return { content = call.content .. \"|b\" } end })\n",
    );
    let went = go_module(&dir);
    let clock = FakeClock::new();
    let session = load(&setup, &[], clock.clone());
    let answer = timed_out(&session, &clock, &went, Duration::from_millis(50));
    assert_eq!(content(&answer), Some("x|b"));
    assert_eq!(answer.changed_by, ["fiber.test/b"]);
    assert_eq!(answer.notices.len(), 1);
    assert_eq!(answer.notices[0].code, ErrorCode::HookFailed);
    assert_eq!(answer.notices[0].extension.as_deref(), Some("fiber.test/a"));
    assert!(
        answer.notices[0].message.contains("50 ms timeout"),
        "{}",
        answer.notices[0].message
    );
    // The hook stopped the loop and left the VM up: the next call runs it.
    let again = ask(&session)
        .recv_timeout(WAIT)
        .expect("waited for the second answer");
    assert_eq!(content(&again), Some("x|a|b"));
    assert!(again.notices.is_empty(), "{:?}", again.notices);
}

#[test]
#[allow(clippy::expect_used, reason = "a failure is the test's")]
fn a_blocking_hook_past_its_timeout_withholds_the_output() {
    let setup = Setup::new();
    let dir = installed(&setup, "a", &spinning("blocking", 50, "a"));
    let went = go_module(&dir);
    let clock = FakeClock::new();
    let session = load(&setup, &[], clock.clone());
    let answer = timed_out(&session, &clock, &went, Duration::from_millis(50));
    assert_eq!(
        answer.outcome,
        AfterToolOutcome::Withheld {
            extension: "fiber.test/a".into()
        }
    );
    assert_eq!(answer.changed_by, ["fiber.test/a"]);
}

#[test]
#[allow(clippy::expect_used, reason = "a failure is the test's")]
fn hook_timeout_ms_is_the_timeout_the_hook_is_stopped_at() {
    let setup = Setup::new();
    let dir = installed(&setup, "a", &spinning("blocking", 60_000, "a"));
    let went = go_module(&dir);
    let clock = FakeClock::new();
    let session = load(
        &setup,
        &["extensions.\"fiber.test/a\".hook_timeout_ms=20"],
        clock.clone(),
    );
    let answer = timed_out(&session, &clock, &went, Duration::from_millis(20));
    assert_eq!(
        answer.outcome,
        AfterToolOutcome::Withheld {
            extension: "fiber.test/a".into()
        }
    );
}

// `host.exec` through Lua and the log (`docs/extensions.md`, "Host calls"):
// a command callback runs a program in its own process group and gets
// `{ exit_code, signal, stdout, stderr }` back, while the session log gets
// one `extension_exec` line per started run.

/// Writes `init` as the entry script of `fiber.test/<short>` and returns a
/// `LuaExtension` started in a session on the setup's workspace.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn exec_extension(
    setup: &Setup,
    short: &str,
    init: &str,
    clock: Arc<FakeClock>,
) -> Arc<LuaExtension> {
    let dir = setup.home().join(short);
    write(&dir.join("init.lua"), init);
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    Arc::new(
        LuaExtension::new(format!("fiber.test/{short}"), dir, setup.home(), clock).with_session(
            extensions::Session {
                config,
                repo_settings: Vec::new(),
                locks: Arc::new(NoLock),
            },
        ),
    )
}

/// Runs `command` on its own thread under `WAIT`, so a callback the runtime
/// fails to stop fails the test instead of hanging it.
#[allow(clippy::panic, reason = "a test helper; a hang is the test's failure")]
fn exec_call(ext: &Arc<LuaExtension>, command: &str) -> Result<String, extensions::Error> {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    let name = command.to_owned();
    std::thread::spawn(move || tx.send(ext.command(&name, "")));
    match rx.recv_timeout(WAIT) {
        Ok(result) => result,
        Err(_) => panic!("`{command}` did not return within {WAIT:?}"),
    }
}

/// The next `extension_exec` delivery on `inbox` under `WAIT`.
#[allow(
    clippy::panic,
    reason = "a test helper; a missing line is the test's failure"
)]
fn next_exec(inbox: &mpsc::Receiver<Delivery>) -> ExtensionExec {
    match inbox.recv_timeout(WAIT) {
        Ok(Delivery::ExtensionExec(exec)) => exec,
        Ok(other) => panic!("expected extension_exec, got {other:?}"),
        Err(_) => panic!("waited {WAIT:?} for extension_exec"),
    }
}

const PWD_INIT: &str = r#"
fiber.command("pwd", { timeout = 5000, run = function()
  return json.encode(host.exec("sh", {"-c", "pwd"}))
end })
"#;

#[test]
fn exec_pwd_returns_the_workspace() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let ext = exec_extension(&setup, "exec", PWD_INIT, clock);
    let body: serde_json::Value =
        serde_json::from_str(&exec_call(&ext, "pwd").expect("the pwd command runs")).unwrap();
    assert_eq!(body["exit_code"], 0, "pwd exits 0");
    let stdout = body["stdout"].as_str().unwrap();
    let expected = setup.workspace().canonicalize().unwrap();
    assert_eq!(Path::new(stdout.trim()), expected.as_path());
    assert_eq!(body["stderr"], "");
}

#[test]
fn exec_sends_one_extension_exec_to_the_inbox() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let ext = exec_extension(&setup, "exec", PWD_INIT, clock);
    let (tx, rx) = mpsc::channel();
    ext.deliver_to(tx);
    exec_call(&ext, "pwd").expect("the pwd command runs");
    let exec = next_exec(&rx);
    assert_eq!(exec.extension, "fiber.test/exec");
    assert_eq!(exec.program, "sh");
    assert_eq!(exec.args, vec!["-c".to_owned(), "pwd".to_owned()]);
    assert!(Path::new(&exec.cwd).is_absolute(), "the cwd is absolute");
    assert_eq!(Path::new(&exec.cwd), setup.workspace().as_path());
    assert_eq!(exec.process.exit_code, Some(0));
    assert_eq!(exec.process.signal, None);
    assert!(!exec.process.timed_out);
}

#[test]
fn a_run_before_deliver_to_is_delivered_after_it() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let ext = exec_extension(&setup, "exec", PWD_INIT, clock);
    let (tx, rx) = mpsc::channel();
    exec_call(&ext, "pwd").expect("the pwd command runs");
    ext.deliver_to(tx);
    let exec = next_exec(&rx);
    assert_eq!(exec.program, "sh");
    assert_eq!(exec.process.exit_code, Some(0));
}

#[test]
fn exec_in_init_fails_loading_with_the_entry_script_error() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let ext = exec_extension(
        &setup,
        "exec",
        "local r = host.exec(\"sh\", {\"-c\", \"true\"})\n",
        clock,
    );
    let err = exec_call(&ext, "pwd").unwrap_err();
    let extensions::Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(
        message.contains("host.exec: not available while init.lua runs"),
        "{message}"
    );
}

#[test]
fn a_bad_argument_list_raises() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let ext = exec_extension(
        &setup,
        "exec",
        r#"
fiber.command("bad", { timeout = 5000, run = function()
  return host.exec("sh", {"-c", 42})
end })
"#,
        clock,
    );
    let err = exec_call(&ext, "bad").unwrap_err();
    let extensions::Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(
        message.contains("host.exec: every argument must be a string"),
        "{message}"
    );
}

// Timers (`docs/extensions.md`, "Host calls"): `host.after` and `host.every`
// fire in the gaps of the session's stream, on the extension's injected
// clock, and stop on `:cancel()`.

/// Makes `dir/name` a fifo the timer callbacks below write to signal they
/// fired: `host.fs.write` blocks until the test opens it for reading.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
fn timer_fifo(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    if !made.map(|status| status.success()).unwrap_or(false) {
        panic!("mkfifo {} failed", path.display());
    }
    path
}

/// Reads one fifo write on a worker under `WAIT`: opening for read blocks
/// until a firing's `host.fs.write` opens it for writing, so a received
/// line proves the firing reached its write.
fn read_fifo(path: PathBuf) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut content = Vec::new();
        let result = std::fs::File::open(&path)
            .and_then(|mut file| std::io::Read::read_to_end(&mut file, &mut content));
        match result {
            Ok(_) => tx.send(content),
            Err(_) => tx.send(Vec::new()),
        }
    });
    rx
}

/// The next fifo line under `WAIT`.
#[allow(
    clippy::panic,
    reason = "a test helper; a missing firing is the test's failure"
)]
fn next_line(rx: &mpsc::Receiver<Vec<u8>>, what: &str) -> Vec<u8> {
    match rx.recv_timeout(WAIT) {
        Ok(line) => line,
        Err(_) => panic!("waited {WAIT:?} for {what}"),
    }
}

/// Asserts no fifo line arrives: a timer that must not have fired.
fn no_line(rx: &mpsc::Receiver<Vec<u8>>, what: &str) {
    assert!(
        rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "{what} fired, and it must not have"
    );
}

/// A timer extension in `setup` whose timers write `dir/` fifos, with a
/// `nop` command that also triggers loading its entry script.
fn timer_extension(
    setup: &Setup,
    init: &str,
    clock: Arc<FakeClock>,
) -> (Arc<LuaExtension>, PathBuf) {
    let dir = setup.home().join("tim");
    let init = format!(
        "local dir = [[{}]]\n\
         fiber.command(\"nop\", {{ timeout = 5000, run = function() return \"nop\" end }})\n\
         {init}",
        dir.display(),
    );
    (exec_extension(setup, "tim", &init, clock), dir)
}

#[test]
fn a_timer_set_in_init_fires_after_load() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (ext, dir) = timer_extension(
        &setup,
        "host.after(50, function() host.fs.write(dir .. \"/fired.fifo\", \"x\") end, \
         { timeout = 1000 })\n",
        clock.clone(),
    );
    let fired = read_fifo(timer_fifo(&dir, "fired.fifo"));
    exec_call(&ext, "nop").expect("loading runs the entry script");
    clock.advance(Duration::from_millis(1000));
    assert_eq!(next_line(&fired, "the init timer"), b"x");
}

#[test]
fn after_fires_once() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (ext, dir) = timer_extension(
        &setup,
        "local n = 0\n\
         fiber.command(\"arm\", { timeout = 5000, run = function()\n\
           host.after(50, function()\n\
             n = n + 1\n\
             host.fs.write(dir .. \"/fired.fifo\", \"x\")\n\
           end, { timeout = 1000 })\n\
           return \"armed\"\n\
         end })\n\
         fiber.command(\"count\", { timeout = 5000, run = function() return tostring(n) end })\n",
        clock.clone(),
    );
    let fired = read_fifo(timer_fifo(&dir, "fired.fifo"));
    assert_eq!(exec_call(&ext, "arm").unwrap(), "armed");
    clock.advance(Duration::from_millis(1000));
    assert_eq!(next_line(&fired, "the after timer"), b"x");
    assert_eq!(exec_call(&ext, "count").unwrap(), "1");
    clock.advance(Duration::from_secs(10));
    assert_eq!(exec_call(&ext, "count").unwrap(), "1");
}

#[test]
fn every_fires_again_ms_after_each_firing_ends() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (ext, dir) = timer_extension(
        &setup,
        "local n = 0\n\
         fiber.command(\"arm\", { timeout = 5000, run = function()\n\
           host.every(50, function()\n\
             n = n + 1\n\
             host.fs.write(dir .. \"/f\" .. n .. \".fifo\", \"x\")\n\
           end, { timeout = 1000 })\n\
           return \"armed\"\n\
         end })\n",
        clock.clone(),
    );
    let first = read_fifo(timer_fifo(&dir, "f1.fifo"));
    let second = read_fifo(timer_fifo(&dir, "f2.fifo"));
    assert_eq!(exec_call(&ext, "arm").unwrap(), "armed");
    clock.advance(Duration::from_millis(1000));
    assert_eq!(next_line(&first, "the first firing"), b"x");
    // The first firing ended at the frozen now, so its `every` is due 50 ms
    // later: 49 ms must not fire it, 2 ms more must.
    clock.advance(Duration::from_millis(49));
    no_line(&second, "the second firing 1 ms early");
    clock.advance(Duration::from_millis(2));
    assert_eq!(next_line(&second, "the second firing"), b"x");
}

#[test]
fn cancel_before_the_due_time_stops_it_and_twice_is_a_no_op() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (ext, dir) = timer_extension(
        &setup,
        "local t = nil\n\
         fiber.command(\"arm\", { timeout = 5000, run = function()\n\
           t = host.after(10000, function()\n\
             host.fs.write(dir .. \"/fired.fifo\", \"x\")\n\
           end, { timeout = 1000 })\n\
           return \"armed\"\n\
         end })\n\
         fiber.command(\"cancel\", { timeout = 5000, run = function()\n\
           t:cancel()\n\
           t:cancel()\n\
           return \"cancelled\"\n\
         end })\n",
        clock.clone(),
    );
    let fired = read_fifo(timer_fifo(&dir, "fired.fifo"));
    assert_eq!(exec_call(&ext, "arm").unwrap(), "armed");
    assert_eq!(exec_call(&ext, "cancel").unwrap(), "cancelled");
    clock.advance(Duration::from_secs(20));
    no_line(&fired, "the cancelled timer");
}

#[test]
fn cancel_inside_its_own_every_callback_stops_it() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (ext, dir) = timer_extension(
        &setup,
        "local n = 0\n\
         local t = nil\n\
         t = host.every(50, function()\n\
           n = n + 1\n\
           host.fs.write(dir .. \"/f\" .. n .. \".fifo\", \"x\")\n\
           if n == 2 then t:cancel() end\n\
         end, { timeout = 1000 })\n\
         fiber.command(\"nop\", { timeout = 5000, run = function() return \"nop\" end })\n",
        clock.clone(),
    );
    let first = read_fifo(timer_fifo(&dir, "f1.fifo"));
    let second = read_fifo(timer_fifo(&dir, "f2.fifo"));
    let third = read_fifo(timer_fifo(&dir, "f3.fifo"));
    exec_call(&ext, "nop").expect("loading sets the timer");
    clock.advance(Duration::from_millis(1000));
    assert_eq!(next_line(&first, "the first firing"), b"x");
    // The second firing is due 50 ms after the first one ended.
    clock.advance(Duration::from_millis(1000));
    assert_eq!(next_line(&second, "the second firing"), b"x");
    clock.advance(Duration::from_secs(10));
    no_line(&third, "a third firing after the cancel");
}

#[test]
fn a_missing_or_bad_timeout_raises_at_the_call() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (ext, _) = timer_extension(
        &setup,
        "fiber.command(\"bad\", { timeout = 5000, run = function(kind)\n\
           if kind == \"missing\" then return host.after(10, function() end) end\n\
           if kind == \"zero\" then return host.after(10, function() end, { timeout = 0 }) end\n\
           return host.every(10, function() end, { timeout = \"soon\" })\n\
         end })\n",
        clock,
    );
    for (kind, call) in [
        ("missing", "host.after"),
        ("zero", "host.after"),
        ("string", "host.every"),
    ] {
        let (tx, rx) = mpsc::channel();
        let ext = Arc::clone(&ext);
        std::thread::spawn(move || tx.send(ext.command("bad", kind)));
        let err = match rx.recv_timeout(WAIT) {
            Ok(result) => result.unwrap_err(),
            Err(_) => panic!("`bad {kind}` did not return within {WAIT:?}"),
        };
        let extensions::Error::Lua { message, .. } = &err else {
            panic!("{err:?}")
        };
        assert!(
            message.contains(&format!(
                "{call}: `timeout` must be a whole number of milliseconds above 0"
            )),
            "{message}"
        );
    }
}

#[test]
fn a_queued_command_starts_before_a_due_timer() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (ext, dir) = timer_extension(
        &setup,
        "host.every(10000, function()\n\
           host.fs.write(dir .. \"/tick.fifo\", \"x\")\n\
         end, { timeout = 1000 })\n\
         fiber.command(\"slow\", { timeout = 60000, run = function()\n\
           host.fs.write(dir .. \"/started.fifo\", \"x\")\n\
           local x = 0\n\
           for i = 1, 30000000 do x = x + i end\n\
           return \"slow-\" .. tostring(x > 0)\n\
         end })\n\
         fiber.command(\"quick\", { timeout = 60000, run = function()\n\
           host.fs.write(dir .. \"/bstarted.fifo\", \"x\")\n\
           return \"done\"\n\
         end })\n",
        clock.clone(),
    );
    let started = read_fifo(timer_fifo(&dir, "started.fifo"));
    // No reader yet: whichever of the timer and `quick` reaches its write
    // first blocks, so the test's read order proves the run order.
    timer_fifo(&dir, "tick.fifo");
    timer_fifo(&dir, "bstarted.fifo");
    let (slow_tx, slow_rx) = mpsc::channel();
    let slow_caller = Arc::clone(&ext);
    std::thread::spawn(move || slow_tx.send(slow_caller.command("slow", "")));
    // `slow` runs, spinning in Lua past the advance below; its long
    // timeout never comes due on the fake clock.
    assert_eq!(next_line(&started, "the slow command"), b"x");
    clock.advance(Duration::from_millis(20000));
    // Queued while `slow` runs and the timer is due: `quick` starts
    // before the timer fires.
    let (quick_tx, quick_rx) = mpsc::channel();
    let quick_caller = Arc::clone(&ext);
    std::thread::spawn(move || quick_tx.send(quick_caller.command("quick", "")));
    match slow_rx.recv_timeout(WAIT) {
        Ok(result) => assert_eq!(result.unwrap(), "slow-true"),
        Err(_) => panic!("the slow command did not return within {WAIT:?}"),
    }
    let bstarted = {
        let path = dir.join("bstarted.fifo");
        read_fifo(path)
    };
    assert_eq!(next_line(&bstarted, "the quick command"), b"x");
    match quick_rx.recv_timeout(WAIT) {
        Ok(result) => assert_eq!(result.unwrap(), "done"),
        Err(_) => panic!("the quick command did not return within {WAIT:?}"),
    }
    let tick = read_fifo(dir.join("tick.fifo"));
    assert_eq!(next_line(&tick, "the timer after the commands"), b"x");
}

#[test]
fn a_timer_callback_past_its_timeout_ends_and_every_keeps_firing() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (ext, dir) = timer_extension(
        &setup,
        "local n = 0\n\
         fiber.command(\"arm\", { timeout = 5000, run = function()\n\
           host.every(50, function()\n\
             n = n + 1\n\
             if n == 1 then\n\
               host.fs.write(dir .. \"/started.fifo\", \"x\")\n\
               while true do end\n\
             end\n\
             host.fs.write(dir .. \"/f2.fifo\", \"x\")\n\
           end, { timeout = 200 })\n\
           return \"armed\"\n\
         end })\n\
         fiber.command(\"count\", { timeout = 5000, run = function() return tostring(n) end })\n",
        clock.clone(),
    );
    let started = read_fifo(timer_fifo(&dir, "started.fifo"));
    let second = read_fifo(timer_fifo(&dir, "f2.fifo"));
    assert_eq!(exec_call(&ext, "arm").unwrap(), "armed");
    clock.advance(Duration::from_millis(1000));
    assert_eq!(next_line(&started, "the first firing"), b"x");
    // Past the firing's 200 ms timeout it ends. The `count` call is the
    // barrier: it runs only after the thread is free, so past it the
    // first firing has ended and only the first one has run.
    clock.advance(Duration::from_millis(400));
    assert_eq!(exec_call(&ext, "count").unwrap(), "1");
    // Its `every` is due 50 ms after that end; 500 ms later it fires again.
    clock.advance(Duration::from_millis(500));
    assert_eq!(next_line(&second, "the second firing"), b"x");
    assert_eq!(exec_call(&ext, "count").unwrap(), "2");
}

#[test]
fn a_timer_has_no_provider_credential_to_refresh() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (ext, dir) = timer_extension(
        &setup,
        // A refresh from a timer is refused: it raises inside the firing,
        // which ends silently, and the `every` keeps firing.
        "host.after(50, function()\n\
         host.oauth.refresh(function() return nil end)\n\
         end, { timeout = 1000 })\n\
         local n = 0\n\
         host.every(50, function()\n\
           n = n + 1\n\
           host.fs.write(dir .. \"/f\" .. n .. \".fifo\", \"again\")\n\
         end, { timeout = 1000 })\n\
         fiber.command(\"nop\", { timeout = 5000, run = function() return \"nop\" end })\n",
        clock.clone(),
    );
    let first = read_fifo(timer_fifo(&dir, "f1.fifo"));
    exec_call(&ext, "nop").expect("loading sets the timers");
    clock.advance(Duration::from_millis(1000));
    // The refused refresh ends the firing silently (`docs/extensions.md`:
    // a timer failure is reported nowhere); the `every` keeps firing.
    assert_eq!(next_line(&first, "the every after the refusal"), b"again");
}
