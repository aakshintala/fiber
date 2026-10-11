//! A Fiber home and extension sources in a temporary directory, removed when
//! dropped. Tests never touch the real home.

#![allow(dead_code, reason = "each test file uses a different part")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::signing::{SignRequest, Signer};
use extensions::{CredentialPair, Error, LuaExtension, LuaProvider, Origin, Request, plan};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

/// Writes a healthy extension install record beside its manifest.
pub(crate) fn write_record(dir: &Path) {
    let text = fs::read_to_string(dir.join("extension.json")).unwrap();
    let manifest: Value = serde_json::from_str(&text).unwrap();
    let name = manifest.get("name").and_then(|n| n.as_str()).unwrap();
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("v0.0.0");
    fs::write(
        dir.join(".fiber.json"),
        json!({"name": name, "version": version, "requested": true, "source": {"path": "/p"}})
            .to_string(),
    )
    .unwrap();
}

/// How far each round drives the fake clock: a day past any install or git
/// bound, so the deadline, the grace and the drain all elapse.
const FAR: Duration = Duration::from_secs(24 * 3600);

/// Bounds the fake-clock jumps while driving a stalled calibration.
const ROUNDS: u32 = 30;

/// One bounded wait for the run's answer or park signal.
const ROUND: Duration = Duration::from_millis(200);

/// Rounds of waiting for the stalled run to wait on the clock: 15 waits of
/// `ROUND` are about 3 s of wall clock, the hang guard for a run that never
/// waits.
const PARK_ROUNDS: u32 = 15;

/// Rounds of waiting for the stalled process to prove it is stuck: 8 rounds
/// of two bounded waits are about 3 s of wall clock, the hang guard for a
/// stall that never appears.
const STUCK_ROUNDS: u32 = 8;

/// Waits until the stalled run parks on the clock or answers, returning an
/// early answer at once. A test moves fake time only once the run is waiting
/// on it, past its own clock check; a run that answers without ever waiting
/// fails on its own answer, not on a jump past bounds it never computed.
#[track_caller]
fn wait_parked<T: Send>(
    clock: &fakes::clock::FakeClock,
    done: &std::sync::mpsc::Receiver<T>,
) -> Option<T> {
    for _ in 0..PARK_ROUNDS {
        if !clock.parked().is_empty() {
            return None;
        }
        match Deadline::after(ROUND).recv(done) {
            Ok(done) => return Some(done),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
    }
    panic!("the stalled run parks on the clock");
}

/// Waits until the process holding `stall` on its command line has outlived
/// a bounded wait, returning an early answer at once. A process seen running
/// across the wait is stuck, not starting: stopping a starter reports a
/// timeout the stall never caused, so the clock moves only after the second
/// sighting. A stall that answers instead fails on its own answer.
#[track_caller]
fn await_stuck<T: Send>(done: &std::sync::mpsc::Receiver<T>, stall: &str) -> Option<T> {
    for _ in 0..STUCK_ROUNDS {
        if !fakes::matching(stall).unwrap().is_empty() {
            // Up: still up after a bounded wait means stuck, not starting;
            // an answer meanwhile ends this at once.
            match Deadline::after(ROUND).recv(done) {
                Ok(done) => return Some(done),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the stalled run returns")
                }
            }
            if !fakes::matching(stall).unwrap().is_empty() {
                return None;
            }
        }
        match Deadline::after(ROUND).recv(done) {
            Ok(done) => return Some(done),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
    }
    panic!("the stalled process runs");
}

/// Waits for the run's next park after a clock advance, or returns an early
/// answer. A later clock advance cannot collapse the park's boundary.
#[track_caller]
fn wait_parked_since<T: Send>(
    clock: &fakes::clock::FakeClock,
    mark: &fakes::clock::Mark,
    done: &std::sync::mpsc::Receiver<T>,
) -> Option<T> {
    for _ in 0..PARK_ROUNDS {
        if clock.await_any_parked_since(mark, ROUND) {
            return None;
        }
        match done.try_recv() {
            Ok(done) => return Some(done),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
    }
    panic!("the stalled run parks on the clock after an advance");
}

/// Drives `clock` until the stalled run on `done` answers: the run first
/// parks on the clock, and the process holding `stall` on its command line
/// first proves it is stuck, so whatever instant the run computes its bounds
/// at, the next jump lands past them. Each later jump waits for a fresh park
/// acknowledgement, so the deadline, grace and drain cannot collapse into
/// one instant. An early answer ends the rounds; a run that never answers
/// fails, naming the stall.
#[track_caller]
pub(crate) fn drive<T: Send>(
    clock: &fakes::clock::FakeClock,
    done: std::sync::mpsc::Receiver<T>,
    stall: &str,
) -> T {
    // Both acknowledgements precede the first jump: stopping a starter on
    // the way up reports a timeout the stall never caused.
    if let Some(done) = wait_parked(clock, &done) {
        return done;
    }
    if let Some(done) = await_stuck(&done, stall) {
        return done;
    }
    for _ in 0..ROUNDS {
        match done.try_recv() {
            Ok(done) => return done,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
        let mark = clock.advance_marked(FAR);
        if let Some(done) = wait_parked_since(clock, &mark, &done) {
            return done;
        }
    }
    panic!("the stalled run returns");
}

pub(crate) struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    pub(crate) fn new() -> Self {
        let root = fakes::TempDir::new("fiber-extensions");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.root.path().to_path_buf()
    }

    pub(crate) fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// An extension source directory named `dir` with this manifest and
    /// these provider files.
    pub(crate) fn source(&self, dir: &str, manifest: &Value, providers: &[Value]) -> PathBuf {
        let path = self.root.path().join("src").join(dir);
        write(&path.join("extension.json"), &manifest.to_string());
        for provider in providers {
            let name = provider["name"].as_str().unwrap();
            write(
                &path.join("providers").join(format!("{name}.json")),
                &provider.to_string(),
            );
        }
        path
    }
}

pub(crate) fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

pub(crate) fn manifest(name: &str) -> Value {
    json!({ "name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 })
}

/// A provider with these model ids, each on `openai-responses`.
pub(crate) fn provider(name: &str, ids: &[&str]) -> Value {
    let models: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "id": id, "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 }))
        .collect();
    json!({ "name": name, "credential": { "env": "FIBER_TEST_UNSET_KEY" }, "models": models })
}

/// Installs the extension in `source` the way `fiber extension install <path>` does
/// and returns its name.
pub(crate) fn install(home: &Path, source: &Path, fiber: &str) -> Result<String, Error> {
    let names = plan(
        home,
        &Request::Path(source.into()),
        fiber,
        &Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )?
    .commit()?;
    Ok(names.into_iter().next().unwrap())
}

/// Installs `fiber.test/<short>` with `fields` merged into its manifest and
/// the entry script `init`, and returns its installed directory.
pub(crate) fn install_lua(setup: &Setup, short: &str, fields: &Value, init: &str) -> PathBuf {
    let name = format!("fiber.test/{short}");
    let mut listed = manifest(&name);
    if let Value::Object(extra) = fields
        && let Value::Object(into) = &mut listed
    {
        for (key, value) in extra {
            into.insert(key.clone(), value.clone());
        }
    }
    let source = setup.source(short, &listed, &[]);
    write(&source.join("init.lua"), init);
    install(&setup.home(), &source, "0.1.0").unwrap();
    setup
        .home()
        .join("extensions")
        .join(config::dir_name(&name))
}

/// A config over the test home and workspace, with these overrides.
pub(crate) fn config(setup: &Setup, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
    })
    .unwrap()
}

/// A Lua provider whose `models()` is `models_run`, with a `credential`
/// function: what most `add_lua` tests use, so the credential gate lets
/// `models()` run.
pub(crate) fn lua_named(
    setup: &Setup,
    dir: &str,
    provider: &str,
    models_run: &str,
) -> Arc<LuaProvider> {
    let ext = setup.home().join(dir);
    write(
        &ext.join("init.lua"),
        &format!(
            "fiber.provider(\"{provider}\", {{ \
             credential = {{ timeout = 1000, run = function() \
             return {{ token = \"test-token\", expires_at = 1893456000 }} end }}, \
             models = {{ timeout = 1000, run = function() return {models_run} end }} }})\n"
        ),
    );
    let extension = Arc::new(LuaExtension::new(
        "acme-ext",
        ext,
        setup.home(),
        FakeClock::new(),
    ));
    LuaProvider::new(extension, provider)
}

/// A test-local provider `p` on `clock`: `credential` and `sign` run
/// `credential_run` and `sign_run`, each absent when its option is `None`.
pub(crate) fn script_provider(
    setup: &Setup,
    clock: Arc<FakeClock>,
    credential_run: Option<&str>,
    sign_run: Option<&str>,
) -> Arc<LuaProvider> {
    let mut spec = Vec::new();
    if let Some(run) = credential_run {
        spec.push(format!(
            "credential = {{ timeout = 60000, run = function() return {run} end }}"
        ));
    }
    if let Some(run) = sign_run {
        spec.push(format!(
            "sign = {{ timeout = 60000, run = function(request) return {run} end }}"
        ));
    }
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!("fiber.provider(\"p\", {{ {} }})\n", spec.join(", ")),
    );
    let extension = Arc::new(LuaExtension::new("ext", dir, setup.home(), clock));
    LuaProvider::new(extension, "p")
}

/// The pair `credential`/`label`.
pub(crate) fn pair(credential: &str, label: &str) -> CredentialPair {
    CredentialPair {
        credential: credential.to_owned(),
        label: label.to_owned(),
    }
}

/// The value of `name` in `headers`, case-sensitively.
pub(crate) fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
}

/// How long a sign waits before it fails the test instead of hanging it.
const SIGN_WITHIN: Duration = Duration::from_secs(5);

/// Signs one request with `headers` already on it, on its own thread under
/// [`SIGN_WITHIN`], so a call that never returns fails the test instead of
/// hanging it.
#[track_caller]
pub(crate) fn sign_with(
    signer: &Arc<dyn Signer>,
    headers: &[(String, String)],
) -> Result<Vec<(String, String)>, contract::signing::Error> {
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    let body = br#"{"model":"m"}"#.to_vec();
    let signer = Arc::clone(signer);
    let owned: Vec<(String, String)> = headers.to_vec();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(signer.sign(&SignRequest {
            method: "POST",
            url: &url,
            headers: &owned,
            body: &body,
        }))
    });
    match Deadline::after(SIGN_WITHIN).recv(&rx) {
        Ok(answer) => answer,
        Err(_) => panic!("the sign did not return within {SIGN_WITHIN:?}"),
    }
}

/// `require("go_<name>")` in `dir` signals that the callback has started:
/// the loader opens the fifo for read, the writer here reports it and
/// closes the fifo, and the module reads empty.
pub(crate) fn go_module(dir: &Path, name: &str) -> mpsc::Receiver<()> {
    let path = dir.join(format!("go_{name}.lua"));
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
