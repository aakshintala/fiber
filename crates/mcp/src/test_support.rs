//! One harness for the crate's tests: the fixture directory, the fake
//! clock, and every wait with its named deadline.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use contract::clock::Clock;
use contract::tool::Tool;
use fakes::Deadline;
use fakes::TempDir;
use fakes::clock::FakeClock;
use serde_json::Value;

use crate::prompt::Prompts;
use crate::server::{OpenServer, StartError};
use crate::slot::{Served, Slot};
use crate::start::{
    DEFAULT_CALL_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, ServerSpec, Servers, Started, StdioSpec,
    Transport,
};

impl ServerSpec {
    /// The stdio half of the spec, panicking for a remote one: tests
    /// that only run stdio servers read every declaration field through
    /// here, so the transport enum stays exhaustive at each site.
    pub(crate) fn stdio(&self) -> &StdioSpec {
        match &self.transport {
            Transport::Stdio(stdio) => stdio,
            Transport::Http { .. } => panic!("a stdio-only test met a remote spec"),
        }
    }

    /// The mutable stdio half of the spec, panicking for a remote one.
    pub(crate) fn stdio_mut(&mut self) -> &mut StdioSpec {
        match &mut self.transport {
            Transport::Stdio(stdio) => stdio,
            Transport::Http { .. } => panic!("a stdio-only test met a remote spec"),
        }
    }
}

/// How long a test waits for a thread or a child, in real time.
///
/// The largest round value that keeps every test's serial deadlines within
/// half of nextest's 120 s kill: the worst test,
/// `a_reap_unlists_under_the_shared_list_lock_before_it_waits`, makes three
/// (start, the lock wait, and the stop: 3 x 10 s = 30 s). A passing run
/// never waits on it; it only bounds a hang.
pub(crate) const WITHIN: Duration = fakes::MUST_SUCCEED_WITHIN;

/// One real-time poll of a file or a child's exit.
const POLL: Duration = Duration::from_millis(50);

/// One fixture directory and the fake clock every test drives. The setup
/// arms a watchdog matching the fixture directory before any test starts
/// a server: every fixture server runs with the directory as its
/// argument, so a failure or a timeout still kills the detached groups
/// when the setup drops.
pub(crate) struct Setup {
    /// The fixture directory: `tools.json`, `prompts.json`, results.
    pub dir: TempDir,
    /// The fake clock the server waits on.
    pub fake: Arc<FakeClock>,
    /// Kills every process whose command line names the fixture
    /// directory, and their groups, when the setup drops.
    #[allow(dead_code, reason = "held only for its drop")]
    guard: fakes::Watchdog,
}

/// Fixture binaries warmed past their first-exec stall in this test
/// process, keyed by binary path: nextest runs one test per process, so
/// the first start of each binary warms and later starts skip it. The
/// registry mutex is released before any warm runs, so concurrent starts
/// share one cell without holding a lock across the bounded child wait.
static WARMED: OnceLock<Mutex<HashMap<std::path::PathBuf, Arc<OnceLock<()>>>>> = OnceLock::new();

/// Warms `path` past its first-exec stall once per test process: a
/// `fail-start` directory makes the fixture exit 1 before reading
/// anything, so the warm-up never blocks on input. The warm runs under
/// `fakes::START_BOUND` before the test's own deadline starts, so the
/// stall never eats the success-wait margin.
fn warm_once(path: &Path) {
    let cell = WARMED
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("the warmed set")
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(OnceLock::new()))
        .clone();
    cell.get_or_init(|| {
        let warm_dir = TempDir::new("fiber-mcp-warm");
        std::fs::write(warm_dir.path().join("fail-start"), "").expect("warm-up dir");
        let warm_arg = warm_dir.path().display().to_string();
        fakes::warm_up(path, &[warm_arg.as_str()], 1);
    });
}

impl Setup {
    /// A fresh directory and clock, with the guard armed before any
    /// start: the directory is known here, while no server pid is.
    pub(crate) fn new() -> Self {
        let dir = TempDir::new("fiber-mcp-support");
        let guard = fakes::Watchdog::matching(&dir.path().display().to_string());
        Self {
            dir,
            fake: FakeClock::new(),
            guard,
        }
    }

    /// A fresh directory holding `tools`, with a new clock.
    pub(crate) fn with_tools(tools: &Value) -> Self {
        let setup = Self::new();
        setup.tools(tools);
        setup
    }

    /// The clock as the server sees it.
    pub(crate) fn clock(&self) -> Arc<dyn Clock> {
        self.fake.clone()
    }

    /// The fixture directory.
    pub(crate) fn workspace(&self) -> std::path::PathBuf {
        self.dir.path().to_path_buf()
    }

    /// The cache directory for session starts.
    pub(crate) fn cache(&self) -> std::path::PathBuf {
        self.dir.path().join("cache")
    }

    /// The session artifacts directory for binary content.
    pub(crate) fn artifacts(&self) -> std::path::PathBuf {
        self.dir.path().join("artifacts")
    }

    /// Writes `body` to `name` under the fixture directory.
    pub(crate) fn write(&self, name: &str, body: &str) {
        let path = self.dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture parent");
        }
        std::fs::write(path, body).expect("fixture file");
    }

    /// Writes `tools.json`.
    pub(crate) fn tools(&self, tools: &Value) {
        self.write("tools.json", &tools.to_string());
    }

    /// Writes `prompts.json` from a value.
    pub(crate) fn prompts(&self, prompts: &Value) {
        self.write("prompts.json", &prompts.to_string());
    }

    /// Writes `prompts.json` from a raw body (including `error`).
    pub(crate) fn prompts_raw(&self, body: &str) {
        self.write("prompts.json", body);
    }

    /// Writes one `call-<tool>.json` result body.
    pub(crate) fn result(&self, tool: &str, body: &str) {
        self.write(&format!("call-{tool}.json"), body);
    }

    /// Writes one `prompt-<name>.json` result body.
    pub(crate) fn prompt_result(&self, name: &str, body: &str) {
        self.write(&format!("prompt-{name}.json"), body);
    }

    /// A remote spec for `url` on this directory's cache and clock.
    pub(crate) fn remote_spec(&self, name: &str, url: &str) -> ServerSpec {
        self.remote_spec_with(name, url, Arc::new(FakeTokens::default()))
    }

    pub(crate) fn remote_spec_with(
        &self,
        name: &str,
        url: &str,
        tokens: Arc<dyn contract::mcp_tokens::TokenFiles>,
    ) -> ServerSpec {
        ServerSpec {
            name: name.to_owned(),
            transport: Transport::Http {
                url: url.to_owned(),
                tokens,
            },
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            call_timeout: DEFAULT_CALL_TIMEOUT,
            enabled: None,
            disabled: Vec::new(),
            hints: BTreeMap::new(),
            required: false,
        }
    }

    /// A spec running the shared fixture on this directory.
    pub(crate) fn spec(&self, name: &str) -> ServerSpec {
        ServerSpec {
            name: name.to_owned(),
            transport: Transport::Stdio(StdioSpec {
                command: fakes::mcp_fixture().display().to_string(),
                args: vec![self.dir.path().display().to_string()],
                env: BTreeMap::new(),
                secrets: BTreeMap::new(),
                reader: std::sync::Arc::new(|_| Err(crate::SecretError::Missing)),
                run: None,
            }),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            call_timeout: DEFAULT_CALL_TIMEOUT,
            enabled: None,
            disabled: Vec::new(),
            hints: BTreeMap::new(),
            required: false,
        }
    }

    /// Starts `specs` through the public `start`, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn start(&self, specs: Vec<ServerSpec>) -> Started {
        self.start_with_images(specs, FakeImages::ok())
    }

    /// Starts `specs` through the public `start` with `images` as the
    /// image child, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn start_with_images(
        &self,
        specs: Vec<ServerSpec>,
        images: std::sync::Arc<FakeImages>,
    ) -> Started {
        self.start_with_images_within(specs, images, WITHIN)
    }

    /// As [`start_with_images`], with a caller-given `within` for the
    /// guard; the default stays [`WITHIN`].
    #[track_caller]
    pub(crate) fn start_with_images_within(
        &self,
        specs: Vec<ServerSpec>,
        images: std::sync::Arc<FakeImages>,
        within: Duration,
    ) -> Started {
        warm_once(&fakes::mcp_fixture());
        let workspace = self.workspace();
        let artifacts = self.artifacts();
        let cache = self.cache();
        let clock = self.clock();
        let images: std::sync::Arc<dyn contract::images::Images> = images;
        // Every session server leaves `outstanding` when its start
        // reports, so when the guard below fires the set holds exactly
        // the servers that had not reported. `fakes::within` fixes its
        // message upfront, so the start runs on its own thread and the
        // expiry reads the set: a repeat names the server whose child
        // stalled.
        let outstanding: std::sync::Arc<std::sync::Mutex<BTreeSet<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(BTreeSet::new()));
        let watching = std::sync::Arc::clone(&outstanding);
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("mcp-start".to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::start::start_observed_tracked(
                        specs,
                        &workspace,
                        &artifacts,
                        images,
                        &cache,
                        &clock,
                        "0.0.0",
                        None,
                        Some(watching),
                    )
                }));
                done.send(outcome).unwrap_or(());
            })
            .expect("a thread for the start");
        match Deadline::after(within).recv(&finished) {
            Ok(outcome) => outcome.unwrap_or_else(|payload| std::panic::resume_unwind(payload)),
            Err(_) => {
                let waiting = outstanding
                    .lock()
                    .expect("the outstanding set")
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                panic!("waited {within:?} for the start, waiting on [{waiting}]");
            }
        }
    }

    /// Starts the fixture directly, expecting success, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn start_expect(&self, timeout: Duration) -> OpenServer {
        warm_once(&fakes::mcp_fixture());
        let script = fakes::mcp_fixture().display().to_string();
        let workspace = self.workspace();
        let arg = workspace.display().to_string();
        Self::start_result(&script, &[arg], &workspace, &self.clock(), timeout)
            .expect("the fixture server starts")
    }

    /// Starts `command` with an explicit workspace and clock, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn start_result(
        command: &str,
        args: &[String],
        workspace: &std::path::Path,
        clock: &Arc<dyn Clock>,
        timeout: Duration,
    ) -> Result<OpenServer, StartError> {
        warm_once(&fakes::mcp_fixture());
        let command = command.to_owned();
        let args = args.to_owned();
        let workspace = workspace.to_path_buf();
        let clock = Arc::clone(clock);
        fakes::within("the server start", Deadline::after(WITHIN), move || {
            crate::server::Server::start(
                &command,
                &args,
                &BTreeMap::new(),
                &workspace,
                &clock,
                timeout,
                "0.0.0",
            )
        })
    }

    /// Finds `name` among `started`'s tools.
    pub(crate) fn tool(&self, started: &Started, name: &str) -> Arc<dyn Tool> {
        started
            .tools
            .iter()
            .find(|(_, tool)| tool.definition().name == name)
            .unwrap_or_else(|| panic!("tool {name} is declared"))
            .1
            .clone()
    }

    /// Runs `tool` with no arguments, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn run(&self, tool: &Arc<dyn Tool>) -> contract::tool::Output {
        let tool = Arc::clone(tool);
        fakes::within("the call", Deadline::after(WITHIN), move || {
            tool.run(
                &Default::default(),
                &fakes::CancelToken::new(),
                &fakes::Recorder::default(),
            )
        })
    }

    /// Runs `prompts.get`, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn get(
        &self,
        prompts: &Prompts,
        server: &str,
        prompt: &str,
        arguments: &str,
        cancel: &fakes::CancelToken,
    ) -> contract::tool::Output {
        let prompts = prompts.clone();
        let server = server.to_owned();
        let prompt = prompt.to_owned();
        let arguments = arguments.to_owned();
        let cancel = cancel.clone();
        fakes::within("the prompt get", Deadline::after(WITHIN), move || {
            prompts.get(&server, &prompt, &arguments, &cancel)
        })
    }

    /// Serves `slot`, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn serve(&self, slot: &Arc<Slot>) -> Served {
        let slot = Arc::clone(slot);
        fakes::within("the serve", Deadline::after(WITHIN), move || slot.serve())
    }

    /// Stops `servers`, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn stop(&self, servers: Servers) {
        fakes::within("the stop", Deadline::after(WITHIN), move || {
            servers.stop();
        });
    }

    /// The fixture's child pid.
    pub(crate) fn pid(&self) -> u32 {
        std::fs::read_to_string(self.dir.path().join("pid.txt"))
            .expect("pid.txt")
            .trim()
            .parse()
            .expect("a pid")
    }

    /// The fixture's grandchild pid, when the `grandchild` switch is set:
    /// `<dir>/grandchild.txt` holds it on one line.
    pub(crate) fn grandchild(&self) -> u32 {
        std::fs::read_to_string(self.dir.path().join("grandchild.txt"))
            .expect("grandchild.txt")
            .trim()
            .parse()
            .expect("a pid")
    }

    /// Whether the fixture spawned (its `pid.txt` exists).
    pub(crate) fn spawned(&self) -> bool {
        self.dir.path().join("pid.txt").exists()
    }

    /// Clears the spawn signal, so a test can tell whether a later call spawns.
    pub(crate) fn forget_spawn(&self) {
        std::fs::remove_file(self.dir.path().join("pid.txt")).expect("pid.txt");
    }

    /// The fixture's request log.
    pub(crate) fn requests(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("requests.log")).unwrap_or_default()
    }

    /// Waits until the log holds `count` lines naming `method`.
    #[track_caller]
    pub(crate) fn await_requests(&self, method: &str, count: usize) {
        let path = self.dir.path().join("requests.log");
        let method = method.to_owned();
        await_until(&format!("{count} `{method}` lines"), move || {
            std::fs::read_to_string(&path)
                .unwrap_or_default()
                .matches(method.as_str())
                .count()
                >= count
        });
    }
}

/// One fake image child: records the bytes each call was given and answers
/// the configured result immediately, so no test waits on a child process.
pub(crate) struct FakeImages {
    calls: std::sync::Mutex<Vec<Vec<u8>>>,
    result: Result<contract::provider::ImageRef, contract::images::ImageError>,
}

impl FakeImages {
    /// One that answers `result`.
    pub(crate) fn with(
        result: Result<contract::provider::ImageRef, contract::images::ImageError>,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            result,
        })
    }

    /// One that answers a 2x3 PNG reference.
    pub(crate) fn ok() -> std::sync::Arc<Self> {
        Self::with(Ok(contract::provider::ImageRef {
            path: "artifacts/mcp-fake.png".to_owned(),
            mime_type: "image/png".to_owned(),
            width: 2,
            height: 3,
        }))
    }

    /// The bytes each call was given, in order.
    pub(crate) fn calls(&self) -> Vec<Vec<u8>> {
        self.calls.lock().expect("fake images").clone()
    }

    /// How many calls the image child saw.
    pub(crate) fn call_count(&self) -> usize {
        self.calls.lock().expect("fake images").len()
    }
}

impl contract::images::Images for FakeImages {
    fn process(
        &self,
        bytes: &[u8],
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<contract::provider::ImageRef, contract::images::ImageError> {
        self.calls.lock().expect("fake images").push(bytes.to_vec());
        self.result.clone()
    }
}

/// In-memory credential files; channels force lock races without sleeps.
#[derive(Clone, Default)]
pub(crate) struct FakeTokens {
    pub(crate) state: Arc<std::sync::Mutex<FakeTokenState>>,
}
#[derive(Default)]
pub(crate) struct FakeTokenState {
    pub(crate) records: BTreeMap<String, Value>,
    pub(crate) held: bool,
    pub(crate) locks: usize,
    pub(crate) writes: usize,
    pub(crate) failure: Option<bool>,
    pub(crate) unreadable: bool,
    pub(crate) busy: Option<std::sync::mpsc::Sender<()>>,
    pub(crate) written: Option<std::sync::mpsc::Sender<()>>,
}
struct FakeHeld {
    files: FakeTokens,
    key: String,
}
impl Drop for FakeHeld {
    fn drop(&mut self) {
        self.files.state.lock().unwrap().held = false;
    }
}
impl contract::mcp_tokens::TokenFiles for FakeTokens {
    fn read(&self, key: &str) -> Result<Option<Value>, contract::mcp_tokens::TokenFileError> {
        let state = self.state.lock().unwrap();
        if state.unreadable {
            return Err(contract::mcp_tokens::TokenFileError::Unreadable(
                "credential path".into(),
            ));
        }
        Ok(state.records.get(key).cloned())
    }
    fn try_lock(
        &self,
        key: &str,
    ) -> Result<
        Option<Box<dyn contract::mcp_tokens::HeldTokenFile>>,
        contract::mcp_tokens::TokenFileError,
    > {
        let mut state = self.state.lock().unwrap();
        state.locks += 1;
        if state.held {
            if let Some(busy) = state.busy.take() {
                busy.send(()).unwrap();
            }
            return Ok(None);
        }
        state.held = true;
        Ok(Some(Box::new(FakeHeld {
            files: self.clone(),
            key: key.into(),
        })))
    }
}
impl contract::mcp_tokens::HeldTokenFile for FakeHeld {
    fn read(&self) -> Result<Option<Value>, contract::mcp_tokens::TokenFileError> {
        contract::mcp_tokens::TokenFiles::read(&self.files, &self.key)
    }
    fn write(&self, value: &Value) -> Result<(), contract::mcp_tokens::TokenFileError> {
        use contract::mcp_tokens::TokenFileError;
        let mut state = self.files.state.lock().unwrap();
        if state.failure == Some(false) {
            return Err(TokenFileError::Unwritable("credential path".into()));
        }
        state.records.insert(self.key.clone(), value.clone());
        state.writes += 1;
        if let Some(written) = state.written.take() {
            written.send(()).unwrap();
        }
        if state.failure == Some(true) {
            return Err(TokenFileError::Unsynced("credential path".into()));
        }
        Ok(())
    }
}

/// Waits until `probe` holds, polling inside one `within`: the crate's one
/// poll loop. `probe` runs on the watcher's thread; the deadline names the
/// wait and the failure names what was waited for.
#[track_caller]
pub(crate) fn await_until(what: &str, mut probe: impl FnMut() -> bool + Send + 'static) {
    fakes::within(what, Deadline::after(WITHIN), move || {
        loop {
            if probe() {
                return;
            }
            let (_held, probe) = std::sync::mpsc::channel::<()>();
            match Deadline::after(POLL).recv(&probe) {
                Ok(()) | Err(_) => {}
            }
        }
    });
}
