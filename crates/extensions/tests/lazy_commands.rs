//! A lazy extension (`docs/extensions.md`, "Commands and screens"): the
//! manifest's `commands` list it without starting its VM, its first command
//! run starts it, and it registers only the commands it declares.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{CommandInfo, Event, Ui};
use contract::extension::ExtensionDoor;
use contract::inbox::Rejection;
use extensions::{Origin, Request, SessionExtensions, plan};
use fakes::clock::FakeClock;
use fakes::{Deadline, NoLock};
use serde_json::{Value, json};

/// How long a test waits for one result before failing.
const WAIT: Duration = fakes::MUST_SUCCEED_WITHIN;

/// What `ExtensionDoor::command` returns.
type Called = Result<Box<dyn FnOnce() + Send>, Rejection>;

/// A Fiber home and workspace in a temporary directory.
struct Env {
    root: fakes::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-lazy-commands");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// A file in the workspace, where `host.fs` may write.
    fn marker(&self, name: &str) -> PathBuf {
        self.workspace().join(name)
    }

    /// Installs `fiber.test/<short>` with `extra` merged into its manifest and
    /// `files` (relative path, text) in its package; returns its installed
    /// directory.
    fn install(&self, short: &str, extra: &Value, files: &[(&str, &str)]) -> PathBuf {
        let name = format!("fiber.test/{short}");
        let mut manifest = json!({ "name": name, "version": "v1", "fiber": "0.1.0", "api": 1 })
            .as_object()
            .cloned()
            .unwrap();
        manifest.extend(extra.as_object().unwrap().clone());
        let manifest = Value::Object(manifest);
        let source = self.root.path().join("src").join(short);
        write(&source.join("extension.json"), &manifest.to_string());
        for (path, text) in files {
            write(&source.join(path), text);
        }
        plan(
            &self.home(),
            &Request::Path(source),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        self.home().join("extensions").join(config::dir_name(&name))
    }

    fn config(&self, overrides: &[&str]) -> Config {
        Config::load(Sources {
            home: self.home(),
            workspace: self.workspace(),
            project: ProjectKey::new("p").unwrap(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap()
    }

    /// Loads the session on a thread, under a deadline.
    fn load(&self, overrides: &[&str], clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
        let config = self.config(overrides);
        let home = self.home();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
            let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks, None));
        });
        Arc::new(Deadline::after(WAIT).recv_or_fail(&rx, "the extensions load"))
    }
}

fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

/// Lua that adds one `x` to the counter file `path`.
fn bump(path: &Path) -> String {
    let path = serde_json::to_string(&path.to_string_lossy()).unwrap();
    format!(
        "do local ok, old = pcall(host.fs.read, {path}); host.fs.write({path}, (ok and old or '') .. 'x') end\n"
    )
}

/// How many times the script that bumps `path` ran.
fn count(path: &Path) -> usize {
    fs::read_to_string(path).map_or(0, |text| text.len())
}

/// An entry script that counts its runs in `path`, then runs `body`.
fn counting(path: &Path, body: &str) -> String {
    format!("{}{body}\n", bump(path))
}

/// `fiber.command` registering `name` with a `run` that runs `body`.
fn register(name: &str, body: &str) -> String {
    format!(
        "fiber.command({name:?}, {{ timeout = 5000, description = 'from lua', run = function(text) {body} end }})\n"
    )
}

/// The manifest `commands` value declaring each of `names`.
fn declare(names: &[&str]) -> Value {
    let mut map = serde_json::Map::new();
    for name in names {
        map.insert(
            (*name).to_owned(),
            json!({ "description": format!("{name} declared") }),
        );
    }
    json!({ "commands": map })
}

fn export_declared() -> Value {
    json!({ "commands": { "export": {
        "description": "Write the transcript as Markdown", "argument_hint": "[path]"
    } } })
}

fn info(name: &str, description: &str, hint: Option<&str>, tag: &str) -> CommandInfo {
    CommandInfo {
        name: name.to_owned(),
        description: description.to_owned(),
        argument_hint: hint.map(str::to_owned),
        tag: tag.to_owned(),
        extension: true,
    }
}

/// Runs `command` on a thread; the result arrives on the returned channel.
fn call(session: &Arc<SessionExtensions>, id: &str, name: &str) -> mpsc::Receiver<Called> {
    let (tx, rx) = mpsc::channel();
    let session = Arc::clone(session);
    let (id, name) = (contract::CommandId(id.to_owned()), name.to_owned());
    std::thread::spawn(move || {
        let door: &dyn ExtensionDoor = &*session;
        let _sent = tx.send(door.command(&id, &name, "a.md"));
    });
    rx
}

/// Runs `command` and waits for its admission or rejection.
#[track_caller]
fn run(session: &Arc<SessionExtensions>, name: &str) -> Called {
    Deadline::after(WAIT).recv_or_fail(&call(session, "c_1", name), "the command's answer")
}

#[track_caller]
fn rejection(called: Called) -> Rejection {
    match called {
        Err(rejection) => rejection,
        Ok(_) => panic!("the command was admitted"),
    }
}

fn names(session: &SessionExtensions) -> Vec<String> {
    session.commands().into_iter().map(|c| c.name).collect()
}

#[test]
fn a_declared_command_is_listed_and_no_vm_starts() {
    let env = Env::new();
    let started = env.marker("started");
    let init = counting(&started, &register("export", ""));
    env.install("x", &export_declared(), &[("init.lua", &init)]);
    let session = env.load(&[], FakeClock::new());
    // docs/extensions.md:1356-57: a lazy extension's declared commands list without starting its VM.
    assert_eq!(count(&started), 0, "load started the VM");
    assert_eq!(
        session.commands(),
        vec![info(
            "export",
            "Write the transcript as Markdown",
            Some("[path]"),
            "fiber.test/x"
        )]
    );
    assert_eq!(
        session
            .loaded()
            .into_iter()
            .map(|e| e.name)
            .collect::<Vec<_>>(),
        ["fiber.test/x"]
    );
}

#[test]
fn a_neighbour_without_a_commands_key_starts_at_load_and_lists_no_hint() {
    let env = Env::new();
    let lazy_started = env.marker("lazy");
    let plain_started = env.marker("plain");
    env.install(
        "lazy",
        &export_declared(),
        &[(
            "init.lua",
            &counting(&lazy_started, &register("export", "")),
        )],
    );
    env.install(
        "plain",
        &json!({}),
        &[(
            "init.lua",
            &counting(&plain_started, &register("other", "")),
        )],
    );
    let session = env.load(&[], FakeClock::new());
    // docs/extensions.md:313-16: a lazy extension's `init.lua` waits for a command; any other runs at session start.
    assert_eq!(count(&lazy_started), 0);
    assert_eq!(count(&plain_started), 1);
    assert_eq!(
        session.commands(),
        vec![
            info(
                "export",
                "Write the transcript as Markdown",
                Some("[path]"),
                "fiber.test/lazy"
            ),
            info("other", "from lua", None, "fiber.test/plain"),
        ]
    );
}

#[test]
fn an_empty_commands_key_is_lazy_with_no_commands() {
    let env = Env::new();
    let started = env.marker("started");
    env.install(
        "x",
        &json!({ "commands": {} }),
        &[("init.lua", &counting(&started, &register("sneaky", "")))],
    );
    let session = env.load(&[], FakeClock::new());
    // docs/extensions.md:70-71: a `commands` key, `{}` included, makes the extension lazy.
    assert_eq!(count(&started), 0, "an empty `commands` still starts it");
    assert!(session.commands().is_empty());
    assert_eq!(session.loaded_names(), ["fiber.test/x"]);
}

#[test]
fn the_manifest_description_and_hint_win_over_the_ones_lua_registers() {
    let env = Env::new();
    let init = "fiber.command('export', { timeout = 5000, description = 'from lua', argument_hint = 'lua', run = function() end })";
    env.install("x", &export_declared(), &[("init.lua", init)]);
    let session = env.load(&[], FakeClock::new());
    let listed = session.commands();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].description, "Write the transcript as Markdown");
    assert_eq!(listed[0].argument_hint.as_deref(), Some("[path]"));
}

#[test]
fn a_configured_rename_applies_to_a_declared_name() {
    let env = Env::new();
    env.install("x", &export_declared(), &[("init.lua", "")]);
    let session = env.load(
        &[r#"extensions."fiber.test/x".commands."export"="dump""#],
        FakeClock::new(),
    );
    assert_eq!(names(&session), ["dump"]);
}

#[test]
fn a_declared_name_another_extension_registers_goes_to_neither() {
    let env = Env::new();
    let lazy_started = env.marker("lazy");
    env.install(
        "lazy",
        &export_declared(),
        &[(
            "init.lua",
            &counting(&lazy_started, &register("export", "")),
        )],
    );
    env.install(
        "plain",
        &json!({}),
        &[("init.lua", &register("export", ""))],
    );
    let session = env.load(&[], FakeClock::new());
    assert!(session.commands().is_empty());
    let notices: Vec<_> = session
        .notices()
        .into_iter()
        .filter(|n| n.code == ErrorCode::CommandConflict)
        .collect();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].message.contains("fiber.test/lazy"));
    assert!(notices[0].message.contains("fiber.test/plain"));
    // docs/extensions.md:1356-57: a lazy extension's declared commands list without starting its VM.
    assert_eq!(count(&lazy_started), 0);
}

#[test]
fn a_declared_built_in_name_needs_replaces() {
    let env = Env::new();
    let declared = json!({ "commands": { "model": { "description": "d" } } });
    env.install("x", &declared, &[("init.lua", "")]);
    let session = env.load(&[], FakeClock::new());
    assert!(session.loaded().is_empty());
    assert!(
        session
            .notices()
            .iter()
            .any(|n| n.code == ErrorCode::ExtensionFailed && n.message.contains("`model`")),
        "{:?}",
        session.notices()
    );

    let env = Env::new();
    let replacing = json!({
        "commands": { "model": { "description": "d" } }, "replaces": ["model"]
    });
    env.install("x", &replacing, &[("init.lua", "")]);
    let session = env.load(&[], FakeClock::new());
    assert_eq!(names(&session), ["model"]);
    assert_eq!(session.loaded_names(), ["fiber.test/x"]);
}

#[test]
fn a_declared_command_without_an_entry_script_is_listed() {
    let env = Env::new();
    env.install("x", &export_declared(), &[]);
    let session = env.load(&[], FakeClock::new());
    assert_eq!(names(&session), ["export"]);
}

#[test]
fn a_disabled_extension_lists_none() {
    let env = Env::new();
    let started = env.marker("started");
    env.install(
        "x",
        &export_declared(),
        &[("init.lua", &counting(&started, ""))],
    );
    let session = env.load(
        &[r#"extensions."fiber.test/x".enabled=false"#],
        FakeClock::new(),
    );
    assert!(session.commands().is_empty());
    assert!(session.loaded().is_empty());
    // docs/extensions.md:1360-61: a disabled extension lists no command, so nothing starts.
    assert_eq!(count(&started), 0);
}

#[test]
fn a_status_event_reaches_the_emitter_after_the_first_run() {
    let env = Env::new();
    let started = env.marker("started");
    let init = counting(&started, &register("export", "host.status('ran ' .. text)"));
    env.install("x", &export_declared(), &[("init.lua", &init)]);
    let session = env.load(&[], FakeClock::new());
    let (log, events) = fakes::EventLog::channel();
    session.emit_to(Arc::new(log));
    // docs/extensions.md:1375-79: a command's first run starts the extension, and a start that fails rejects it.
    assert_eq!(count(&started), 0);
    let release = run(&session, "export").unwrap_or_else(|r| panic!("rejected: {r:?}"));
    assert_eq!(count(&started), 1, "the run is accepted after the start");
    assert!(events.try_recv().is_err(), "run ran before release");
    release();
    let wait = Deadline::after(WAIT);
    loop {
        let event = wait.recv_or_fail(&events, "the status event");
        if let Event::ExtensionUi(ui) = event {
            assert_eq!(ui.extension, "fiber.test/x");
            assert_eq!(
                ui.ui,
                Ui::Status {
                    status: "ran a.md".into()
                }
            );
            break;
        }
    }
    // docs/extensions.md:1375-79: a command's first run starts the extension, and a start that fails rejects it.
    assert_eq!(count(&started), 1);
}

/// An entry script that declares `export`, whose `run` reports `ran`, then
/// registers `extra` on top of it.
fn with_export(started: &Path, extra: &str) -> String {
    counting(
        started,
        &format!("{}{extra}", register("export", "host.status('ran')")),
    )
}

const TOOL: &str = "fiber.tool('t', { description = 'd', input_schema = { type = 'object', properties = {} }, effects = { effects = { 'reads' }, paths = { 'note.txt' }, reversible = true }, timeout = 1000, run = function() return 'x' end })";
const HOOK: &str = "fiber.hook('after_tool', { timeout = 1000, on_failure = 'non-blocking', run = function(c) return c end })";
const WATCH: &str = "fiber.watch({'delegate_finished'}, { timeout = 1000, run = function() end })";
const BACKEND: &str =
    "fiber.search_backend('s', { timeout = 1000, run = function() return {} end })";

/// Installs a lazy `export` whose entry script is `init`, and loads it with
/// an emitter that records what the commands report.
struct Lazy {
    env: Env,
    session: Arc<SessionExtensions>,
    events: mpsc::Receiver<Event>,
}

impl Lazy {
    fn new(init: impl Fn(&Path) -> String, declared: &Value, files: &[(&str, &str)]) -> Self {
        let env = Env::new();
        let init = init(&env.marker("started"));
        let mut all = vec![("init.lua", init.as_str())];
        all.extend_from_slice(files);
        env.install("x", declared, &all);
        let session = env.load(&[], FakeClock::new());
        let (log, events) = fakes::EventLog::channel();
        session.emit_to(Arc::new(log));
        Self {
            env,
            session,
            events,
        }
    }

    fn started(&self) -> usize {
        count(&self.env.marker("started"))
    }

    /// Runs `name`, releases it and returns the status it reported.
    #[track_caller]
    fn ran(&self, name: &str) -> String {
        let release = run(&self.session, name).unwrap_or_else(|r| panic!("rejected: {r:?}"));
        release();
        let wait = Deadline::after(WAIT);
        loop {
            let event = wait.recv_or_fail(&self.events, "the status event");
            if let Event::ExtensionUi(ui) = event
                && let Ui::Status { status } = ui.ui
            {
                return status;
            }
        }
    }

    #[track_caller]
    fn rejected(&self, name: &str) -> Rejection {
        rejection(run(&self.session, name))
    }
}

#[track_caller]
fn assert_failed(rejection: &Rejection, command: &str, needles: &[&str]) {
    assert_eq!(rejection.code, ErrorCode::ExtensionFailed, "{rejection:?}");
    assert!(
        rejection
            .message
            .starts_with(&format!("Command `{command}` did not run: ")),
        "{}",
        rejection.message
    );
    for needle in needles {
        assert!(rejection.message.contains(needle), "{}", rejection.message);
    }
}

/// An undeclared registration of `extra` fails the whole extension, once.
#[track_caller]
fn undeclared_registration_fails(extra: &str, kind: &str) {
    let lazy = Lazy::new(
        |started| with_export(started, extra),
        &export_declared(),
        &[],
    );
    assert_failed(
        &lazy.rejected("export"),
        "export",
        &[kind, "`fiber.test/x`"],
    );
    assert_failed(&lazy.rejected("export"), "export", &[kind]);
    // docs/extensions.md:1377-80: a failed start stays failed for the session; the script does not run again.
    assert_eq!(lazy.started(), 1, "the entry script ran again");
    assert!(
        lazy.events.try_recv().is_err(),
        "a command of the failed extension ran"
    );
}

#[test]
fn an_undeclared_command_fails_the_extension() {
    undeclared_registration_fails(&register("other", ""), "the command `other`");
}

#[test]
fn a_tool_fails_the_extension() {
    undeclared_registration_fails(TOOL, "the tool `t`");
}

#[test]
fn a_hook_fails_the_extension() {
    undeclared_registration_fails(HOOK, "a hook at `after_tool`");
}

#[test]
fn a_watcher_fails_the_extension() {
    undeclared_registration_fails(WATCH, "a watcher");
}

#[test]
fn a_search_backend_fails_the_extension() {
    undeclared_registration_fails(BACKEND, "the search backend `s`");
}

/// A lazy `export` that also declares the Lua provider `p`.
fn with_provider(init: impl Fn(&Path) -> String) -> Lazy {
    let declared = json!({
        "commands": { "export": { "description": "d" } },
        "providers": { "p": [] },
    });
    let provider =
        "fiber.provider('p', { models = { timeout = 100, run = function() return {} end } })";
    Lazy::new(init, &declared, &[("providers/p.lua", provider)])
}

#[test]
fn a_failed_check_leaves_the_extensions_provider_serving() {
    let lazy = with_provider(|started| with_export(started, TOOL));
    assert_failed(&lazy.rejected("export"), "export", &["the tool `t`"]);
    let providers = lazy.session.lua_providers();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].1.functions().unwrap(), ["models"]);
}

#[test]
fn a_declared_command_the_script_did_not_register_is_rejected_alone() {
    let lazy = Lazy::new(
        |started| counting(started, &register("a", "host.status('a ran')")),
        &declare(&["a", "b"]),
        &[],
    );
    assert_failed(
        &lazy.rejected("b"),
        "b",
        &["declares the command `b`", "did not register"],
    );
    assert_eq!(lazy.ran("a"), "a ran");
    // docs/extensions.md:1381-83: concurrent first runs share one start, one `init.lua` run.
    assert_eq!(lazy.started(), 1);
}

#[test]
fn a_refused_registration_of_a_declared_command_says_why() {
    let lazy = Lazy::new(
        |started| counting(started, "fiber.command('b', {})"),
        &declare(&["b"]),
        &[],
    );
    assert_failed(
        &lazy.rejected("b"),
        "b",
        &["did not register", "missing `timeout`"],
    );
}

#[test]
fn a_declared_command_without_an_entry_script_is_rejected() {
    let env = Env::new();
    env.install("x", &export_declared(), &[]);
    let session = env.load(&[], FakeClock::new());
    assert_failed(
        &rejection(run(&session, "export")),
        "export",
        &["declares the command `export`"],
    );
}

#[test]
fn a_lua_error_in_the_entry_script_is_sticky() {
    let lazy = Lazy::new(
        |started| counting(started, "error('boom')"),
        &export_declared(),
        &[],
    );
    assert_failed(&lazy.rejected("export"), "export", &["boom"]);
    assert_failed(&lazy.rejected("export"), "export", &["boom"]);
    // docs/extensions.md:1377-80: a failed start stays failed for the session; the script does not run again.
    assert_eq!(lazy.started(), 1);
}

#[test]
fn a_run_starts_the_extension_once_before_it_is_accepted() {
    let lazy = Lazy::new(|started| with_export(started, ""), &export_declared(), &[]);
    // docs/extensions.md:1375-79: a command's first run starts the extension, and a start that fails rejects it.
    assert_eq!(lazy.started(), 0);
    assert_eq!(lazy.ran("export"), "ran");
    assert_eq!(lazy.ran("export"), "ran");
    assert_eq!(lazy.started(), 1);
}

#[test]
fn concurrent_first_runs_share_one_start() {
    let both = |extra: &'static str| {
        let lazy = Lazy::new(
            move |started| {
                counting(
                    started,
                    &format!(
                        "{}{}{extra}",
                        register("a", "host.status('a')"),
                        register("b", "host.status('b')")
                    ),
                )
            },
            &declare(&["a", "b"]),
            &[],
        );
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let calls: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|name| {
                let (tx, rx) = mpsc::channel();
                let session = Arc::clone(&lazy.session);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let door: &dyn ExtensionDoor = &*session;
                    let id = contract::CommandId(format!("c_{name}"));
                    let _sent = tx.send(door.command(&id, name, ""));
                });
                rx
            })
            .collect();
        let answers: Vec<Called> = calls
            .iter()
            .map(|rx| Deadline::after(WAIT).recv_or_fail(rx, "a concurrent first run"))
            .collect();
        (lazy, answers)
    };
    let (lazy, answers) = both("");
    assert!(answers.iter().all(Result::is_ok));
    // docs/extensions.md:1381-83: concurrent first runs share one start, one `init.lua` run.
    assert_eq!(lazy.started(), 1);
    let (lazy, answers) = both(TOOL);
    for (answer, name) in answers.into_iter().zip(["a", "b"]) {
        assert_failed(&rejection(answer), name, &["the tool `t`"]);
    }
    assert_eq!(lazy.started(), 1);
}

/// A driver that records each `host.drive`.
struct Driver(mpsc::Sender<(String, String)>);

impl contract::extension::Drive for Driver {
    fn drive(
        &self,
        extension: &str,
        command: &str,
        _args: serde_json::Map<String, Value>,
        answer: contract::inbox::Ack,
    ) {
        let _sent = self.0.send((extension.to_owned(), command.to_owned()));
        answer.0(Ok(None));
    }
}

#[test]
fn the_driver_reaches_a_lazy_extension() {
    let lazy = Lazy::new(
        |started| counting(started, &register("export", "host.drive('tools', {})")),
        &export_declared(),
        &[],
    );
    let (tx, driven) = mpsc::channel();
    lazy.session.drive_to(Arc::new(Driver(tx)));
    let release = run(&lazy.session, "export").unwrap_or_else(|r| panic!("rejected: {r:?}"));
    release();
    assert_eq!(
        Deadline::after(WAIT).recv_or_fail(&driven, "the drive"),
        ("fiber.test/x".to_owned(), "tools".to_owned())
    );
}

#[test]
fn a_misplaced_provider_call_fails_the_extension_and_drops_its_data() {
    let lazy = Lazy::new(
        |started| counting(started, "fiber.provider('p', {})"),
        &export_declared(),
        &[],
    );
    assert!(lazy.session.rejected().is_empty());
    assert_failed(&lazy.rejected("export"), "export", &["providers/p.lua"]);
    assert_failed(&lazy.rejected("export"), "export", &["providers/p.lua"]);
    assert_eq!(lazy.session.rejected(), ["fiber.test/x"]);
    // docs/extensions.md:1377-80: a failed start stays failed for the session; the script does not run again.
    assert_eq!(lazy.started(), 1);
}

/// The instant a caller waiting on an entry script gives up: its 2 second
/// bound and the 1 second grace after it (`docs/extensions.md`, "How an
/// extension runs").
const ABANDON_AT: Duration = Duration::from_millis(3000);

/// A first run of `export` whose entry script never returns: runs it on a
/// thread, moves the clock by each of `steps` once the caller is parked at the
/// abandon instant, and returns the answer after the last. Every earlier
/// step leaves the call parked.
#[track_caller]
fn stalled_first_run(init: &str, hold: bool, steps: &[u64]) -> Called {
    let env = Env::new();
    let dir = env.install("x", &export_declared(), &[("init.lua", init)]);
    let fifo = dir.join("hold.lua");
    if hold {
        fakes::fifo(&fifo);
    }
    let clock = FakeClock::new();
    let session = env.load(&[], Arc::clone(&clock));
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    if hold {
        std::thread::spawn(move || {
            let held = fs::OpenOptions::new().write(true).open(fifo).unwrap();
            held_tx.send(()).unwrap();
            let _released = release_rx.recv();
            drop(held);
        });
    }
    let asked = clock.now();
    let (tx, answer) = mpsc::channel();
    let caller = {
        let session = Arc::clone(&session);
        std::thread::spawn(move || {
            let door: &dyn ExtensionDoor = &*session;
            let id = contract::CommandId("c_1".into());
            let _sent = tx.send(door.command(&id, "export", ""));
        })
    };
    if hold {
        Deadline::after(WAIT).recv_or_fail(&held_rx, "the entry script reads its file");
    }
    let (last, earlier) = steps.split_last().unwrap();
    for ms in earlier {
        let moved = clock.advance_when_parked(
            Some(asked + ABANDON_AT),
            Some(caller.thread().id()),
            Duration::from_millis(*ms),
            &Deadline::after(WAIT),
        );
        // The caller parks at the abandon instant again, so it has not
        // returned: the answer would have come before the park.
        assert!(
            clock.await_parked_since(&moved, Some(asked + ABANDON_AT), Deadline::after(WAIT)),
            "the call was not parked again after {ms} ms"
        );
        assert!(
            answer.try_recv().is_err(),
            "the call returned after {ms} ms"
        );
    }
    // The last step may pass the abandon instant, which
    // `advance_when_parked` refuses, so it waits for the park itself.
    assert!(clock.await_parked(asked + ABANDON_AT, Deadline::after(WAIT)));
    #[allow(
        clippy::disallowed_methods,
        reason = "late wake: one advance past a park proven at its exact instant just before (ruling 6261259790)"
    )]
    clock.advance(Duration::from_millis(*last));
    let called = Deadline::after(WAIT).recv_or_fail(&answer, "the first run's answer");
    drop(release_tx);
    // A second call after the failure is rejected at once, with no clock
    // move.
    assert_failed(
        &rejection(run(&session, "export")),
        "export",
        &["`fiber.test/x`"],
    );
    called
}

#[test]
fn a_script_that_spins_is_stopped_at_the_load_bound() {
    for steps in [vec![1999, 1], vec![2001]] {
        let called = stalled_first_run("while true do end", false, &steps);
        assert_failed(&rejection(called), "export", &["`fiber.test/x`"]);
    }
}

#[test]
fn a_script_that_cannot_be_interrupted_is_abandoned_after_the_grace() {
    for steps in [vec![2999, 1], vec![3000], vec![3001]] {
        let called = stalled_first_run("require('hold')", true, &steps);
        assert_failed(&rejection(called), "export", &["`fiber.test/x`"]);
    }
}

/// Reloads `old` over the same home, on a thread, under a deadline.
fn reload(env: &Env, old: &Arc<SessionExtensions>) -> Arc<SessionExtensions> {
    let (home, config) = (env.home(), env.config(&[]));
    let old = Arc::clone(old);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        let _sent = tx.send(old.reload(&home, &config, FakeClock::new(), locks));
    });
    let session = Arc::new(Deadline::after(WAIT).recv_or_fail(&rx, "the reload"));
    let (log, _events) = fakes::EventLog::channel();
    session.emit_to(Arc::new(log));
    session
}

#[test]
fn a_reload_leaves_a_lazy_extension_that_never_started_unstarted() {
    let lazy = Lazy::new(|started| with_export(started, ""), &export_declared(), &[]);
    let session = reload(&lazy.env, &lazy.session);
    // docs/extensions.md:1520-22: a reload runs `init.lua` again, except for a lazy extension that never started.
    assert_eq!(lazy.started(), 0);
    assert_eq!(names(&session), ["export"]);
}

#[test]
fn a_reload_starts_again_a_lazy_extension_that_had_started() {
    let lazy = Lazy::new(|started| with_export(started, ""), &export_declared(), &[]);
    assert_eq!(lazy.ran("export"), "ran");
    let session = reload(&lazy.env, &lazy.session);
    // docs/extensions.md:1520-22: a reload runs `init.lua` again, except for a lazy extension that never started.
    assert_eq!(lazy.started(), 2, "reload returns after the new start");
    let (release, events) = {
        let (log, events) = fakes::EventLog::channel();
        session.emit_to(Arc::new(log));
        (
            run(&session, "export").unwrap_or_else(|r| panic!("{r:?}")),
            events,
        )
    };
    release();
    let wait = Deadline::after(WAIT);
    loop {
        if let Event::ExtensionUi(_) = wait.recv_or_fail(&events, "the status") {
            break;
        }
    }
    // docs/extensions.md:1520-22: a reload runs `init.lua` again, except for a lazy extension that never started.
    assert_eq!(lazy.started(), 2, "the run started it again");
}

#[test]
fn a_failed_extension_stays_failed_until_a_reload_fixes_it() {
    let lazy = Lazy::new(
        |started| with_export(started, TOOL),
        &export_declared(),
        &[],
    );
    assert_failed(&lazy.rejected("export"), "export", &["the tool `t`"]);
    let init = lazy.env.home().join("extensions/fiber.test-x/init.lua");
    write(&init, &with_export(&lazy.env.marker("started"), ""));
    assert_failed(&lazy.rejected("export"), "export", &["the tool `t`"]);
    let session = reload(&lazy.env, &lazy.session);
    // docs/extensions.md:1379 and 1520-22: `reload` tries a failed start again.
    assert_eq!(lazy.started(), 2, "reload starts a failed start again");
    assert!(run(&session, "export").is_ok());
    assert_eq!(lazy.started(), 2);
}

#[test]
fn a_reload_starts_again_a_lazy_extension_a_provider_started() {
    let lazy = with_provider(|started| with_export(started, ""));
    let providers = lazy.session.lua_providers();
    assert_eq!(providers[0].1.functions().unwrap(), ["models"]);
    // docs/extensions.md:1520-22: a lazy extension whose VM started, by a provider's first use too, starts again on reload.
    assert_eq!(lazy.started(), 1, "the provider's first use started it");
    reload(&lazy.env, &lazy.session);
    assert_eq!(lazy.started(), 2, "reload returns after the new start");
}

#[test]
fn a_failed_restart_leaves_one_notice_and_rejects_the_commands() {
    let lazy = Lazy::new(|started| with_export(started, ""), &export_declared(), &[]);
    assert_eq!(lazy.ran("export"), "ran");
    write(
        &lazy.env.home().join("extensions/fiber.test-x/init.lua"),
        "error('boom')",
    );
    let session = reload(&lazy.env, &lazy.session);
    let failed: Vec<_> = session
        .notices()
        .into_iter()
        .filter(|n| n.code == ErrorCode::ExtensionFailed)
        .collect();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].extension.as_deref(), Some("fiber.test/x"));
    assert!(failed[0].message.contains("boom"));
    assert_failed(&rejection(run(&session, "export")), "export", &["boom"]);
}

#[test]
fn a_lazy_extension_that_fails_on_a_misplaced_provider_loses_its_data_at_the_next_add_session() {
    let data = json!({
        "name": "c", "credential": { "env": "FIBER_TEST_UNSET_KEY" },
        "models": [{ "id": "m", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 }],
    })
    .to_string();
    let mut declared = export_declared();
    declared["providers"] = json!({ "c": ["http://127.0.0.1:1/v1"] });
    let lazy = Lazy::new(
        |started| counting(started, "fiber.provider('c', {})"),
        &declared,
        &[("providers/c.json", &data)],
    );
    let (mut providers, notices) = extensions::Providers::load(&lazy.env.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.get("c").is_some());
    assert_failed(&lazy.rejected("export"), "export", &["providers/c.lua"]);
    // The extension failed after the session loaded, so the data stays until
    // the next `add_session` over this session reads the rejection.
    assert!(providers.get("c").is_some());
    let notices = providers.add_session(
        &lazy.session,
        &lazy.env.config(&[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.get("c").is_none());
}
