//! One harness for the crate's tests: the fixture directory, the fake
//! clock, and every wait with its named deadline.

use std::collections::BTreeMap;
use std::sync::Arc;
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
use crate::start::{DEFAULT_CALL_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, ServerSpec, Servers, Started};

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

/// One fixture directory and the fake clock every test drives.
pub(crate) struct Setup {
    /// The fixture directory: `tools.json`, `prompts.json`, results.
    pub dir: TempDir,
    /// The fake clock the server waits on.
    pub fake: Arc<FakeClock>,
}

impl Setup {
    /// A fresh directory and clock.
    pub(crate) fn new() -> Self {
        Self {
            dir: TempDir::new("fiber-mcp-support"),
            fake: FakeClock::new(),
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

    /// A spec running the shared fixture on this directory.
    pub(crate) fn spec(&self, name: &str) -> ServerSpec {
        ServerSpec {
            name: name.to_owned(),
            command: fakes::mcp_fixture().display().to_string(),
            args: vec![self.dir.path().display().to_string()],
            env: BTreeMap::new(),
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
        let workspace = self.workspace();
        let cache = self.cache();
        let clock = self.clock();
        fakes::within("the start", WITHIN, move || {
            crate::start::start(specs, &workspace, &cache, &clock, "0.0.0")
        })
    }

    /// Starts the fixture directly, expecting success, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn start_expect(&self, timeout: Duration) -> OpenServer {
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
        let command = command.to_owned();
        let args = args.to_owned();
        let workspace = workspace.to_path_buf();
        let clock = Arc::clone(clock);
        fakes::within("the server start", WITHIN, move || {
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
        fakes::within("the call", WITHIN, move || {
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
        fakes::within("the prompt get", WITHIN, move || {
            prompts.get(&server, &prompt, &arguments, &cancel)
        })
    }

    /// Serves `slot`, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn serve(&self, slot: &Arc<Slot>) -> Served {
        let slot = Arc::clone(slot);
        fakes::within("the serve", WITHIN, move || slot.serve())
    }

    /// Stops `servers`, with a deadline naming the wait.
    #[track_caller]
    pub(crate) fn stop(&self, servers: Servers) {
        fakes::within("the stop", WITHIN, move || {
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

    /// Waits until `kill -0` fails for `pid`: the child was reaped.
    #[track_caller]
    pub(crate) fn await_reaped(&self, pid: u32) {
        await_until(&format!("pid {pid} to be reaped"), move || {
            !fakes::kill_pid(pid, "0").expect("probe")
        });
    }
}

/// Waits until `probe` holds, polling inside one `within`: the crate's one
/// poll loop. `probe` runs on the watcher's thread; the deadline names the
/// wait and the failure names what was waited for.
#[track_caller]
pub(crate) fn await_until(what: &str, mut probe: impl FnMut() -> bool + Send + 'static) {
    fakes::within(what, WITHIN, move || {
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
