//! `run_login` and `run_logout`: the lookup, the one-snapshot approval,
//! and the stored and deleted token, with the input and both output
//! streams in memory.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::{Layer, insert_entry};
use contract::ErrorCode;
use contract::events::OfferedKind;
use contract::mcp_remote::McpRemote;
use contract::shapes::Failure;
use extensions::{Browser, Index, Store};
use fakes::Deadline;
use serde_json::json;

use super::{mcp_login, mcp_logout, run_login, run_logout};
use crate::CliError;
use crate::mcp::Streams;
use crate::project_of;

const URL: &str = "https://example.test/mcp";
const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Records the login calls and shows one authorization URL; the token key
/// is fixed, so two names over one URL share one file.
struct FakeRemote {
    calls: Mutex<Vec<String>>,
    fail: Option<(ErrorCode, String)>,
}

impl FakeRemote {
    fn ok() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail: None,
        }
    }

    fn failing(code: ErrorCode, message: &str) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail: Some((code, message.to_owned())),
        }
    }
}

impl McpRemote for FakeRemote {
    fn has_userinfo(&self, url: &str) -> bool {
        url.contains('@')
    }

    fn token_key(&self, _url: &str) -> Result<String, Failure> {
        Ok(KEY.to_owned())
    }

    fn login(
        &self,
        _files: &dyn contract::mcp_tokens::TokenFiles,
        url: &str,
        present: &dyn Fn(&str),
        _clock: &dyn contract::clock::Clock,
    ) -> Result<(), Failure> {
        self.calls.lock().unwrap().push(url.to_owned());
        present("https://auth.example/authorize?test=1");
        match &self.fail {
            None => Ok(()),
            Some((code, message)) => Err(doors::failure(code.clone(), message.clone())),
        }
    }
}

struct FakeBrowser {
    opened: Mutex<Vec<String>>,
}

impl FakeBrowser {
    fn new() -> Self {
        Self {
            opened: Mutex::new(Vec::new()),
        }
    }
}

impl Browser for FakeBrowser {
    fn open(&self, url: &str) {
        self.opened.lock().unwrap().push(url.to_owned());
    }

    fn show(&self, _url: &str, _code: &str) {}

    fn attended(&self) -> bool {
        true
    }
}

struct Setup {
    tmp: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let tmp = fakes::TempDir::new("fiber-mcp-login");
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        fs::create_dir_all(tmp.path().join("ws")).unwrap();
        Self { tmp }
    }

    fn home(&self) -> std::path::PathBuf {
        self.tmp.path().join("home")
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.tmp.path().join("ws")
    }

    fn declare(&self, layer: Layer, name: &str, value: serde_json::Value) {
        let (_, project) = project_of(&self.home(), &self.workspace()).unwrap();
        insert_entry(
            &self.home(),
            &self.workspace(),
            &project,
            layer,
            "mcp.servers",
            name,
            value,
        )
        .unwrap();
    }

    fn declare_remote(&self, layer: Layer, name: &str, url: &str) {
        self.declare(layer, name, json!({"url": url}));
    }

    fn approve(&self, name: &str) {
        let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
        let (_, project) = project_of(&self.home(), &self.workspace()).unwrap();
        let items = extensions::declared_items(&self.workspace(), clock.as_ref()).unwrap();
        let item = items
            .iter()
            .find(|item| item.kind == OfferedKind::McpServer && item.name == name)
            .unwrap();
        let mut index = Index::load(&self.home());
        let hash = extensions::hash(&mut index, item).unwrap();
        index.save().unwrap_or(());
        Store::new(&self.home(), &project, clock)
            .approve(item, &hash)
            .unwrap();
    }

    fn repo_file(&self) -> std::path::PathBuf {
        self.workspace().join(".fiber/config.json")
    }

    fn write_repo(&self, servers: serde_json::Value) {
        fs::create_dir_all(self.workspace().join(".fiber")).unwrap();
        fs::write(
            self.repo_file(),
            json!({"mcp": {"servers": servers}}).to_string(),
        )
        .unwrap();
    }
}

struct Run {
    result: Result<(), CliError>,
    err: String,
}

fn login(
    setup: &Setup,
    name: &str,
    remote: &FakeRemote,
    browser: &FakeBrowser,
    between: &dyn Fn(),
) -> (Run, String) {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = run_login(
        &setup.home(),
        &setup.workspace(),
        name,
        remote,
        browser,
        between,
        Streams {
            out: &mut out,
            err: &mut err,
        },
        &clock,
    );
    (
        Run {
            result,
            err: String::from_utf8(err).unwrap(),
        },
        String::from_utf8(out).unwrap(),
    )
}

fn logout(setup: &Setup, name: &str, remote: &FakeRemote) -> Run {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = run_logout(
        &setup.home(),
        &setup.workspace(),
        name,
        remote,
        Streams {
            out: &mut out,
            err: &mut err,
        },
    );
    assert_eq!(String::from_utf8(out).unwrap(), "");
    Run {
        result,
        err: String::from_utf8(err).unwrap(),
    }
}

/// A stored token file for `KEY`, as the login would leave it.
fn store(setup: &Setup) {
    let file = config::CredentialFile::new(&setup.home(), "mcp", KEY).unwrap();
    let held = file.try_lock().unwrap().unwrap();
    held.write(&json!({"url": URL})).unwrap();
}

#[test]
fn an_unknown_name_is_a_usage_failure_naming_it() {
    let setup = Setup::new();
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let (ran, _) = login(&setup, "zzz", &remote, &browser, &|| {});
    let failure = ran.result.unwrap_err();
    assert!(
        matches!(failure, CliError::McpServerUnknown { ref server } if server == "zzz"),
        "{failure:?}"
    );
    assert!(remote.calls.lock().unwrap().is_empty());
}

#[test]
fn a_stdio_server_is_a_usage_failure() {
    let setup = Setup::new();
    setup.declare(
        Layer::Global,
        "x",
        json!({"command": "npx", "args": ["-y"]}),
    );
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let (ran, _) = login(&setup, "x", &remote, &browser, &|| {});
    let failure = ran.result.unwrap_err();
    assert!(
        matches!(failure, CliError::McpServerStdio { ref server } if server == "x"),
        "{failure:?}"
    );
    assert!(remote.calls.lock().unwrap().is_empty());
    let logout = logout(&setup, "x", &remote);
    assert!(matches!(
        logout.result.unwrap_err(),
        CliError::McpServerStdio { .. }
    ));
}

#[test]
fn a_login_passes_the_effective_url_and_reports_the_file() {
    let setup = Setup::new();
    setup.declare_remote(Layer::Global, "docs", URL);
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let (ran, _) = login(&setup, "docs", &remote, &browser, &|| {});
    assert!(
        ran.result.is_ok(),
        "{:?}",
        ran.result.err().map(|e| e.to_string())
    );
    assert_eq!(remote.calls.lock().unwrap().as_slice(), [URL]);
    assert_eq!(
        browser.opened.lock().unwrap().as_slice(),
        ["https://auth.example/authorize?test=1"]
    );
    assert_eq!(ran.err, format!("fiber: stored credentials/mcp/{KEY}\n"));
}

#[test]
fn a_repository_server_nobody_approved_is_refused() {
    let setup = Setup::new();
    setup.declare_remote(Layer::Repository, "docs", URL);
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let (ran, _) = login(&setup, "docs", &remote, &browser, &|| {});
    let failure = ran.result.unwrap_err();
    assert!(
        matches!(failure, CliError::McpUnapproved { ref server } if server == "docs"),
        "{failure:?}"
    );
    assert!(remote.calls.lock().unwrap().is_empty());
    assert!(browser.opened.lock().unwrap().is_empty());
}

#[test]
fn an_approved_repository_server_logs_in_with_the_snapshot_url() {
    let setup = Setup::new();
    setup.declare_remote(Layer::Repository, "docs", URL);
    setup.approve("docs");
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let (ran, _) = login(&setup, "docs", &remote, &browser, &|| {});
    assert!(
        ran.result.is_ok(),
        "{:?}",
        ran.result.err().map(|e| e.to_string())
    );
    assert_eq!(remote.calls.lock().unwrap().as_slice(), [URL]);
}

#[test]
fn a_declaration_changed_after_approval_is_refused() {
    let setup = Setup::new();
    setup.declare_remote(Layer::Repository, "docs", URL);
    setup.approve("docs");
    setup.write_repo(json!({"docs": {"url": "https://other.test/mcp"}}));
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let (ran, _) = login(&setup, "docs", &remote, &browser, &|| {});
    let failure = ran.result.unwrap_err();
    assert!(
        matches!(failure, CliError::McpUnapproved { .. }),
        "{failure:?}"
    );
    assert!(remote.calls.lock().unwrap().is_empty());
}

#[test]
fn an_approval_for_b_does_not_carry_to_a() {
    // The entry read is unapproved A; the hook writes approved B and
    // approves it, then writes A back. The login is refused, never
    // calls, and stores nothing.
    let setup = Setup::new();
    setup.declare_remote(Layer::Repository, "docs", "https://a.test/mcp");
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let between = || {
        setup.write_repo(json!({"docs": {"url": "https://b.test/mcp"}}));
        setup.approve("docs");
        setup.write_repo(json!({"docs": {"url": "https://a.test/mcp"}}));
    };
    let (ran, _) = login(&setup, "docs", &remote, &browser, &between);
    let failure = ran.result.unwrap_err();
    assert!(
        matches!(failure, CliError::McpUnapproved { .. }),
        "{failure:?}"
    );
    assert!(remote.calls.lock().unwrap().is_empty());
    assert!(!setup.home().join("credentials").exists());
}

#[test]
fn a_hook_that_swaps_in_a_sends_the_login_to_b_alone() {
    // The entry read is approved B; the hook swaps in unapproved A. The
    // login goes to B's URL only, never A's.
    let setup = Setup::new();
    setup.declare_remote(Layer::Repository, "docs", "https://b.test/mcp");
    setup.approve("docs");
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let between = || {
        setup.write_repo(json!({"docs": {"url": "https://a.test/mcp"}}));
    };
    let (ran, _) = login(&setup, "docs", &remote, &browser, &between);
    assert!(
        ran.result.is_ok(),
        "{:?}",
        ran.result.err().map(|e| e.to_string())
    );
    assert_eq!(
        remote.calls.lock().unwrap().as_slice(),
        ["https://b.test/mcp"]
    );
}

#[test]
fn a_hook_that_swaps_in_b_leaves_an_unapproved_a_refused() {
    // The entry read is unapproved A; the hook swaps in approved B. The
    // login is refused: A was read.
    let setup = Setup::new();
    setup.declare_remote(Layer::Repository, "docs", "https://a.test/mcp");
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let between = || {
        setup.write_repo(json!({"docs": {"url": "https://b.test/mcp"}}));
        setup.approve("docs");
    };
    let (ran, _) = login(&setup, "docs", &remote, &browser, &between);
    assert!(matches!(
        ran.result.unwrap_err(),
        CliError::McpUnapproved { .. }
    ));
    assert!(remote.calls.lock().unwrap().is_empty());
}

#[test]
fn a_config_invalid_login_names_the_server_with_no_code() {
    let setup = Setup::new();
    setup.declare_remote(Layer::Global, "docs", URL);
    let remote = FakeRemote::failing(
        ErrorCode::ConfigInvalid,
        "its authorization server does not offer PKCE with S256",
    );
    let browser = FakeBrowser::new();
    let (ran, _) = login(&setup, "docs", &remote, &browser, &|| {});
    let failure = ran.result.unwrap_err();
    assert!(
        matches!(
            failure,
            CliError::McpLoginRefused {
                ref server,
                ref message
            } if server == "docs"
                && message.contains("its authorization server does not offer PKCE with S256")
                && !message.contains("config_invalid")
        ),
        "{failure:?}"
    );
}

#[test]
fn a_broken_hooks_file_fails_the_login_as_config_not_forwarded() {
    // The repository declares the server, so the login reaches the
    // approval read; the hooks settings file is not an object of hooks,
    // which only `declared` refuses. A type-erased failure would arrive
    // as `Forwarded` with the same code.
    let setup = Setup::new();
    setup.declare_remote(Layer::Repository, "docs", URL);
    fs::create_dir_all(setup.workspace().join(".fiber/config")).unwrap();
    fs::write(
        setup.workspace().join(".fiber/config/hooks.json"),
        r#"{"hooks": []}"#,
    )
    .unwrap();
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let (ran, _) = login(&setup, "docs", &remote, &browser, &|| {});
    let failure = ran.result.unwrap_err();
    assert!(matches!(failure, CliError::Config(_)), "{failure:?}");
    assert_eq!(failure.code(), ErrorCode::ConfigInvalid);
    assert!(remote.calls.lock().unwrap().is_empty());
}

#[test]
fn a_url_with_userinfo_is_refused_before_the_login() {
    let setup = Setup::new();
    setup.declare_remote(Layer::Global, "docs", "https://user@example.test/mcp");
    let remote = FakeRemote::ok();
    let browser = FakeBrowser::new();
    let (ran, _) = login(&setup, "docs", &remote, &browser, &|| {});
    let failure = ran.result.unwrap_err();
    assert!(
        matches!(failure, CliError::UrlHasUserinfo { ref server } if server == "docs"),
        "{failure:?}"
    );
    assert!(remote.calls.lock().unwrap().is_empty());
}

#[test]
fn a_logout_deletes_the_file_and_leaves_the_lock() {
    let setup = Setup::new();
    setup.declare_remote(Layer::Global, "docs", URL);
    store(&setup);
    let remote = FakeRemote::ok();
    let ran = logout(&setup, "docs", &remote);
    assert!(
        ran.result.is_ok(),
        "{:?}",
        ran.result.err().map(|e| e.to_string())
    );
    assert_eq!(ran.err, "");
    assert!(!setup.home().join("credentials/mcp").join(KEY).exists());
    assert!(
        setup
            .home()
            .join("credentials/mcp")
            .join(format!("{KEY}.lock"))
            .exists()
    );
    // A second logout succeeds and says nothing was stored.
    let ran = logout(&setup, "docs", &remote);
    assert!(
        ran.result.is_ok(),
        "{:?}",
        ran.result.err().map(|e| e.to_string())
    );
    assert!(ran.err.contains("no token was stored"), "{}", ran.err);
    assert!(ran.err.contains("docs"), "{}", ran.err);
}

#[test]
fn a_logout_of_an_unknown_name_names_it() {
    let setup = Setup::new();
    let remote = FakeRemote::ok();
    let ran = logout(&setup, "zzz", &remote);
    let failure = ran.result.unwrap_err();
    assert!(
        matches!(failure, CliError::McpServerUnknown { ref server } if server == "zzz"),
        "{failure:?}"
    );
}

#[test]
fn two_names_over_one_url_share_one_file() {
    let setup = Setup::new();
    setup.declare_remote(Layer::Global, "x", URL);
    setup.declare_remote(Layer::Project, "y", URL);
    store(&setup);
    let remote = FakeRemote::ok();
    let ran = logout(&setup, "y", &remote);
    assert!(
        ran.result.is_ok(),
        "{:?}",
        ran.result.err().map(|e| e.to_string())
    );
    assert!(!setup.home().join("credentials/mcp").join(KEY).exists());
}

#[test]
fn a_url_in_another_case_reaches_the_same_key() {
    let setup = Setup::new();
    setup.declare_remote(Layer::Global, "docs", "HTTPS://EXAMPLE.TEST/mcp");
    store(&setup);
    let remote = FakeRemote::ok();
    let ran = logout(&setup, "docs", &remote);
    assert!(
        ran.result.is_ok(),
        "{:?}",
        ran.result.err().map(|e| e.to_string())
    );
    assert!(!setup.home().join("credentials/mcp").join(KEY).exists());
    // The seam saw the URL as written; the key is what joins them.
    assert_eq!(
        remote.calls.lock().unwrap().len(),
        0,
        "a logout never logs in"
    );
}

/// The child's marker: set, the test runs one exit-code wrapper and exits
/// with its code.
const CHILD: &str = "FIBER_CLI_MCP_LOGIN_CHILD";

/// Which wrapper the child runs.
const CASE: &str = "FIBER_CLI_MCP_LOGIN_CASE";

/// The workspace the child declares its servers in.
const WORKSPACE: &str = "FIBER_CLI_MCP_LOGIN_WORKSPACE";

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(10);

/// The child's global declaration of `docs`, as [`Setup::declare_remote`]
/// writes it.
fn child_declare(home: &std::path::Path, workspace: &std::path::Path) {
    let (_, project) = project_of(home, workspace).unwrap();
    insert_entry(
        home,
        workspace,
        &project,
        Layer::Global,
        "mcp.servers",
        "docs",
        json!({"url": URL}),
    )
    .unwrap();
}

/// Runs the child case `case` with `home` as its `FIBER_HOME` and `ws` as
/// its workspace, and returns its exit code.
#[track_caller]
fn child_exit(
    test: &str,
    case: &str,
    home: &std::path::Path,
    ws: &std::path::Path,
    path: Option<&str>,
) -> i32 {
    let output = fakes::rerun_prepared(
        &format!("{test}::mcp_login_wrappers_exit_with_the_command_code"),
        &[(CHILD, "1"), (CASE, case)],
        Deadline::after(CHILD_DEADLINE),
        |command| {
            command
                .env("FIBER_HOME", home)
                .env(WORKSPACE, ws)
                .current_dir(ws);
            if let Some(path) = path {
                command.env("PATH", path);
            }
        },
    );
    output.status.code().expect("the child exits with a code")
}

#[test]
fn mcp_login_wrappers_exit_with_the_command_code() {
    if std::env::var_os(CHILD).is_some() {
        let home = std::path::PathBuf::from(std::env::var("FIBER_HOME").unwrap());
        let workspace = std::path::PathBuf::from(std::env::var(WORKSPACE).unwrap());
        let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
        let remote = FakeRemote::ok();
        let code = match std::env::var(CASE).unwrap().as_str() {
            "login-unknown" => mcp_login("zzz", &remote, clock),
            "login-ok" => {
                child_declare(&home, &workspace);
                mcp_login("docs", &remote, clock)
            }
            "logout-unknown" => mcp_logout("zzz", &remote),
            "logout-ok" => {
                child_declare(&home, &workspace);
                let file = config::CredentialFile::new(&home, "mcp", KEY).unwrap();
                let held = file.try_lock().unwrap().unwrap();
                held.write(&json!({"url": URL})).unwrap();
                drop(held);
                mcp_logout("docs", &remote)
            }
            case => panic!("unknown mcp login child case: {case}"),
        };
        std::process::exit(code);
    }
    let name = module_path!().split_once("::").unwrap().1;
    let setup = Setup::new();
    // An unknown name exits 2, the usage code: never 0, 1 or -1.
    assert_eq!(
        child_exit(
            name,
            "login-unknown",
            &setup.home(),
            &setup.workspace(),
            None
        ),
        2
    );
    assert_eq!(
        child_exit(
            name,
            "logout-unknown",
            &setup.home(),
            &setup.workspace(),
            None
        ),
        2
    );
    // A login and a logout that run exit 0. The login's browser finds no
    // program on `PATH`, so no browser opens.
    assert_eq!(
        child_exit(
            name,
            "login-ok",
            &setup.home(),
            &setup.workspace(),
            Some("/nonexistent")
        ),
        0
    );
    assert_eq!(
        child_exit(name, "logout-ok", &setup.home(), &setup.workspace(), None),
        0
    );
}
