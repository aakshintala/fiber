//! `run_add`: what it writes, what it stores, and what it refuses, with
//! the input and both output streams in memory.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_arguments,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::io::{self, BufRead, Cursor, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::{
    ConfigError, CredentialFile, Layer, Secret, delete_secret, read_secret, store_secret,
};
use contract::ErrorCode;
use contract::events::OfferedKind;
use contract::mcp_remote::McpRemote;
use contract::shapes::Failure;
use extensions::{Decision, Index, Store};
use serde_json::{Value, json};

use super::{SecretInput, Streams, lock_label, run_add, settle_secrets};
use crate::login::KeyReader;
use crate::project_of;

const URL: &str = "https://example.test/mcp";

/// Whether the seam reports userinfo: the tests own the answer, so a url
/// that carries userinfo is refused only through the seam, never by a
/// second check in this crate.
struct FakeRemote(bool);

impl McpRemote for FakeRemote {
    fn has_userinfo(&self, _url: &str) -> bool {
        self.0
    }
}

/// Reads each `--secret` value as one exact line of the input, in flag
/// order, through the same reader the production path uses.
struct FakeKeys;

impl KeyReader for FakeKeys {
    fn read_key(
        &mut self,
        _prompt: &str,
        _input: &mut dyn BufRead,
        _err: &mut dyn Write,
    ) -> io::Result<Secret> {
        panic!("a secret is read exactly, never trimmed");
    }

    fn read_key_exact(
        &mut self,
        _prompt: &str,
        input: &mut dyn BufRead,
        _err: &mut dyn Write,
    ) -> io::Result<Secret> {
        crate::login::read_exact_line(input)
    }
}

struct Setup {
    tmp: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let tmp = fakes::TempDir::new("fiber-mcp-add");
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        fs::create_dir_all(tmp.path().join("ws")).unwrap();
        Self { tmp }
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.tmp.path().join("ws")
    }

    fn file(&self, layer: Layer) -> PathBuf {
        let (_, project) = project_of(&self.home(), &self.workspace()).unwrap();
        match layer {
            Layer::Global => self.home().join("config.json"),
            Layer::Project => self
                .home()
                .join("projects")
                .join(project.as_str())
                .join("config.json"),
            Layer::Repository => self.workspace().join(".fiber/config.json"),
        }
    }

    fn read(&self, layer: Layer) -> Value {
        serde_json::from_str(&fs::read_to_string(self.file(layer)).unwrap()).unwrap()
    }
}

struct Run {
    result: Result<(), Failure>,
    out: String,
    err: String,
}

#[allow(
    clippy::too_many_arguments,
    reason = "test helper; each caller names its own shape"
)]
fn go(
    setup: &Setup,
    layer: Layer,
    name: &str,
    url: Option<&str>,
    env: &[&str],
    secrets: &[&str],
    command: &[&str],
    userinfo: bool,
    secret_lines: &str,
) -> Run {
    let env: Vec<String> = env.iter().map(|s| (*s).to_owned()).collect();
    let secrets: Vec<String> = secrets.iter().map(|s| (*s).to_owned()).collect();
    let command: Vec<String> = command.iter().map(|s| (*s).to_owned()).collect();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let mut keys = FakeKeys;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = run_add(
        &setup.home(),
        &setup.workspace(),
        layer,
        name,
        url,
        &env,
        &secrets,
        &command,
        &FakeRemote(userinfo),
        SecretInput {
            keys: &mut keys,
            input: &mut Cursor::new(secret_lines.as_bytes().to_vec()),
            terminal: false,
        },
        Streams {
            out: &mut out,
            err: &mut err,
        },
        &clock,
    );
    Run {
        result,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

fn ok(setup: &Setup, layer: Layer, name: &str, url: Option<&str>, env: &[&str], secrets: &[&str], command: &[&str], lines: &str) -> Run {
    go(setup, layer, name, url, env, secrets, command, false, lines)
}

/// Every regular file under `home` and `workspace` holding `needle`.
fn files_holding(home: &Path, workspace: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![home.to_path_buf(), workspace.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = fs::read_dir(&dir).unwrap();
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.is_file()
                && fs::read(&path)
                    .map(|bytes| {
                        bytes
                            .windows(needle.len().max(1))
                            .any(|window| window == needle)
                    })
                    .unwrap_or(false)
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The approval recorded for the repository declaration `name`, if the
/// declaration is there to hash.
fn decision_of(setup: &Setup, name: &str) -> Option<Decision> {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (_, project) = project_of(&setup.home(), &setup.workspace()).unwrap();
    let items = extensions::declared_items(&setup.workspace(), clock.as_ref()).unwrap();
    let item = items
        .iter()
        .find(|item| item.kind == OfferedKind::McpServer && item.name == name)?;
    let mut index = Index::load(&setup.home());
    let hash = extensions::hash(&mut index, item).unwrap();
    Store::new(&setup.home(), &project, clock).decision(OfferedKind::McpServer, &hash)
}

#[test]
fn a_remote_add_writes_the_url_in_the_chosen_layer() {
    for layer in [Layer::Global, Layer::Project, Layer::Repository] {
        let setup = Setup::new();
        let ran = ok(&setup, layer, "docs", Some(URL), &[], &[], &[], "");
        assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
        assert_eq!(
            setup.read(layer),
            json!({"mcp": {"servers": {"docs": {"url": URL}}}})
        );
        for other in [Layer::Global, Layer::Project, Layer::Repository] {
            if other != layer {
                assert!(!setup.file(other).exists(), "{other:?} was written");
            }
        }
    }
}

#[test]
fn a_stdio_add_writes_command_args_and_env_with_secret_references() {
    let setup = Setup::new();
    let ran = ok(
        &setup,
        Layer::Global,
        "x",
        None,
        &["A=1"],
        &["TOKEN"],
        &["npx", "-y", "pkg"],
        "tok-1\n",
    );
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(
        setup.read(Layer::Global),
        json!({"mcp": {"servers": {"x": {
            "command": "npx",
            "args": ["-y", "pkg"],
            "env": {"A": "1", "TOKEN": {"secret": "mcp.x.TOKEN"}},
        }}}})
    );
    // The value lands beside the configuration, mode 0600, and in no
    // configuration file: only its own credentials file may hold it.
    let credential = setup.home().join("credentials/mcp.x.TOKEN");
    assert_eq!(fs::read_to_string(&credential).unwrap(), "tok-1");
    assert_eq!(
        fs::metadata(&credential).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        files_holding(&setup.home(), &setup.workspace(), b"tok-1"),
        [credential]
    );
    assert!(!ran.out.contains("tok-1"), "stdout: {}", ran.out);
    assert!(!ran.err.contains("tok-1"), "stderr: {}", ran.err);
}

#[test]
fn secrets_are_read_in_flag_order_and_empty_parts_stay_unwritten() {
    let setup = Setup::new();
    let ran = ok(
        &setup,
        Layer::Global,
        "x",
        None,
        &[],
        &["B", "A"],
        &["npx"],
        "b-val\na-val\n",
    );
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(
        read_secret(&setup.home(), "mcp.x.B").unwrap().unwrap().expose(),
        "b-val"
    );
    assert_eq!(
        read_secret(&setup.home(), "mcp.x.A").unwrap().unwrap().expose(),
        "a-val"
    );
    // No args and `-e` values: both stay unwritten.
    assert_eq!(
        setup.read(Layer::Global),
        json!({"mcp": {"servers": {"x": {
            "command": "npx",
            "env": {"B": {"secret": "mcp.x.B"}, "A": {"secret": "mcp.x.A"}},
        }}}})
    );
}

#[test]
fn a_value_with_spaces_round_trips_through_one_stdin_line() {
    let setup = Setup::new();
    let ran = ok(&setup, Layer::Global, "x", None, &[], &["TOKEN"], &["npx"], " padded \n");
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(
        read_secret(&setup.home(), "mcp.x.TOKEN").unwrap().unwrap().expose(),
        " padded "
    );
}

#[test]
fn an_empty_secret_value_is_a_usage_failure_and_writes_nothing() {
    let setup = Setup::new();
    let ran = ok(&setup, Layer::Global, "x", None, &[], &["TOKEN"], &["npx"], "\n");
    let failure = ran.result.unwrap_err();
    assert_eq!(failure.code, ErrorCode::Usage);
    assert!(!setup.file(Layer::Global).exists());
    assert!(!setup.home().join("credentials").exists(), "a secret was stored");
}

#[test]
fn an_existing_secret_is_replaced_and_named() {
    let setup = Setup::new();
    store_secret(&setup.home(), "mcp.x.TOKEN", &Secret::new("old".into())).unwrap();
    let ran = ok(&setup, Layer::Global, "x", None, &[], &["TOKEN"], &["npx"], "new\n");
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(
        read_secret(&setup.home(), "mcp.x.TOKEN").unwrap().unwrap().expose(),
        "new"
    );
    assert!(ran.err.contains("fiber: replaced credentials/mcp.x.TOKEN"), "stderr: {}", ran.err);
    assert!(!ran.err.contains("new"), "stderr: {}", ran.err);
}

#[test]
fn a_url_with_userinfo_is_config_invalid_and_writes_nothing() {
    let setup = Setup::new();
    let ran = go(
        &setup,
        Layer::Global,
        "docs",
        Some("https://user@example.test/mcp"),
        &[],
        &[],
        &[],
        true,
        "",
    );
    let failure = ran.result.unwrap_err();
    assert_eq!(failure.code, ErrorCode::ConfigInvalid);
    assert!(failure.message.contains("docs"), "{}", failure.message);
    assert!(!setup.file(Layer::Global).exists());
    assert!(!setup.home().join("credentials").exists());
}

#[test]
fn userinfo_is_asked_of_the_seam_alone() {
    // The seam says no userinfo: the url is written as is.
    let setup = Setup::new();
    let ran = go(
        &setup,
        Layer::Global,
        "docs",
        Some("https://user@example.test/mcp"),
        &[],
        &[],
        &[],
        false,
        "",
    );
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(
        setup.read(Layer::Global),
        json!({"mcp": {"servers": {"docs": {"url": "https://user@example.test/mcp"}}}})
    );
}

#[test]
fn a_bad_name_is_a_usage_failure_and_writes_nothing() {
    for name in ["", "a\"b", "a/b", "a b", "a\tb", "a\x01b", "a\0b"] {
        let setup = Setup::new();
        let ran = ok(&setup, Layer::Global, name, Some(URL), &[], &[], &[], "");
        let failure = ran.result.unwrap_err();
        assert_eq!(failure.code, ErrorCode::Usage, "name {name:?}");
        assert!(!setup.file(Layer::Global).exists(), "name {name:?}");
        assert!(!setup.home().join("credentials").exists(), "name {name:?}");
    }
    for name in ["docs", "a.b", "x.lock", "docs-2_x"] {
        let setup = Setup::new();
        let ran = ok(&setup, Layer::Global, name, Some(URL), &[], &[], &[], "");
        assert!(ran.result.is_ok(), "name {name:?}: {:?}", ran.result.err().map(|e| e.message));
    }
}

#[test]
fn a_bad_variable_is_a_usage_failure_and_writes_nothing() {
    // No `=`, an empty KEY, a KEY starting with a digit, and a secret KEY
    // holding `=`.
    for (env, secrets) in [
        (&["NOEQUALS"] as &[&str], &[] as &[&str]),
        (&["=x"], &[]),
        (&["1A=x"], &[]),
        (&[], &["A=B"]),
        (&[], &[""]),
    ] {
        let setup = Setup::new();
        let ran = ok(&setup, Layer::Global, "x", None, env, secrets, &["npx"], "v\n");
        let failure = ran.result.unwrap_err();
        assert_eq!(failure.code, ErrorCode::Usage, "env {env:?} secrets {secrets:?}");
        assert!(!setup.file(Layer::Global).exists());
        assert!(!setup.home().join("credentials").exists());
    }
    // An empty `-e` value is allowed.
    let setup = Setup::new();
    let ran = ok(&setup, Layer::Global, "x", None, &["E="], &[], &["npx"], "");
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(
        setup.read(Layer::Global),
        json!({"mcp": {"servers": {"x": {"command": "npx", "env": {"E": ""}}}}})
    );
}

#[test]
fn a_key_that_is_both_plain_and_secret_is_a_usage_failure() {
    let setup = Setup::new();
    let ran = ok(&setup, Layer::Global, "x", None, &["K=1"], &["K"], &["npx"], "v\n");
    let failure = ran.result.unwrap_err();
    assert_eq!(failure.code, ErrorCode::Usage);
    assert!(failure.message.contains("K"), "{}", failure.message);
    assert!(!setup.file(Layer::Global).exists());
    assert!(!setup.home().join("credentials").exists());
}

#[test]
fn the_shape_is_exactly_one_transport_and_no_flags_with_a_url() {
    // Both a url and a command, or neither.
    for (url, command) in [
        (Some(URL), &["npx"] as &[&str]),
        (None, &[]),
    ] {
        let setup = Setup::new();
        let ran = ok(&setup, Layer::Global, "x", url, &[], &[], command, "");
        assert_eq!(ran.result.unwrap_err().code, ErrorCode::Usage);
        assert!(!setup.file(Layer::Global).exists());
    }
    // `-e` and `--secret` with a url.
    for (env, secrets) in [(&["A=1"] as &[&str], &[] as &[&str]), (&[], &["A"])] {
        let setup = Setup::new();
        let ran = ok(&setup, Layer::Global, "x", Some(URL), env, secrets, &[], "v\n");
        assert_eq!(ran.result.unwrap_err().code, ErrorCode::Usage);
        assert!(!setup.file(Layer::Global).exists());
        assert!(!setup.home().join("credentials").exists());
    }
    // An empty program after `--`.
    let setup = Setup::new();
    let ran = ok(&setup, Layer::Global, "x", None, &[], &[], &[""], "");
    assert_eq!(ran.result.unwrap_err().code, ErrorCode::Usage);
    assert!(!setup.file(Layer::Global).exists());
}

#[test]
fn a_duplicate_name_in_the_layer_is_refused_and_keeps_the_bytes() {
    let setup = Setup::new();
    let first = ok(&setup, Layer::Global, "docs", Some(URL), &[], &[], &[], "");
    assert!(first.result.is_ok());
    let before = fs::read(setup.file(Layer::Global)).unwrap();
    let ran = ok(&setup, Layer::Global, "docs", Some("https://other.test/mcp"), &[], &[], &[], "");
    let failure = ran.result.unwrap_err();
    assert_eq!(failure.code, ErrorCode::Usage);
    assert!(failure.message.contains("fiber mcp remove"), "{}", failure.message);
    assert!(failure.message.contains("global"), "{}", failure.message);
    assert_eq!(fs::read(setup.file(Layer::Global)).unwrap(), before);
    // The same name in another layer is accepted.
    let ran = ok(&setup, Layer::Project, "docs", Some(URL), &[], &[], &[], "");
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
}

#[test]
fn a_duplicate_name_restores_the_previous_secret() {
    let setup = Setup::new();
    let first = ok(
        &setup,
        Layer::Global,
        "x",
        None,
        &[],
        &["TOKEN"],
        &["npx"],
        "winner\n",
    );
    assert!(first.result.is_ok());
    // The loser stores its own value, sees the refusal, and puts the
    // winner's bytes back.
    let ran = ok(
        &setup,
        Layer::Global,
        "x",
        None,
        &[],
        &["TOKEN"],
        &["npx"],
        "loser\n",
    );
    assert_eq!(ran.result.unwrap_err().code, ErrorCode::Usage);
    assert_eq!(
        read_secret(&setup.home(), "mcp.x.TOKEN").unwrap().unwrap().expose(),
        "winner"
    );
}

#[test]
fn an_add_while_one_of_the_name_runs_fails_busy_and_writes_nothing() {
    let setup = Setup::new();
    // The winner's per-name lock, held as a running add holds it.
    let guard = CredentialFile::new(&setup.home(), "mcp-add", &lock_label("docs")).unwrap();
    let _held = guard.try_lock().unwrap().unwrap();
    let ran = ok(
        &setup,
        Layer::Global,
        "docs",
        None,
        &[],
        &["TOKEN"],
        &["npx"],
        "v\n",
    );
    let failure = ran.result.unwrap_err();
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert!(failure.message.contains("another `fiber mcp add` of `docs` is running"), "{}", failure.message);
    assert!(!setup.file(Layer::Global).exists());
    assert!(
        read_secret(&setup.home(), "mcp.docs.TOKEN").unwrap().is_none(),
        "a secret was stored without the declaration"
    );
}

#[test]
fn a_declaration_write_that_fails_puts_each_secret_back() {
    let setup = Setup::new();
    store_secret(&setup.home(), "mcp.x.OLD", &Secret::new("old".into())).unwrap();
    fs::write(setup.file(Layer::Global), "{invalid").unwrap();
    let ran = ok(
        &setup,
        Layer::Global,
        "x",
        None,
        &[],
        &["OLD", "NEW"],
        &["npx"],
        "new-old\nnew-new\n",
    );
    assert_eq!(ran.result.unwrap_err().code, ErrorCode::ConfigInvalid);
    assert_eq!(
        read_secret(&setup.home(), "mcp.x.OLD").unwrap().unwrap().expose(),
        "old"
    );
    assert!(
        read_secret(&setup.home(), "mcp.x.NEW").unwrap().is_none(),
        "a secret stored by a failed add was left behind"
    );
}

#[test]
fn settle_keeps_the_secrets_after_the_rename_and_restores_before_it() {
    let setup = Setup::new();
    let stored = vec![
        ("mcp.x.A".to_owned(), Some(Secret::new("old".into()))),
        ("mcp.x.B".to_owned(), None),
    ];
    store_secret(&setup.home(), "mcp.x.A", &Secret::new("new-a".into())).unwrap();
    store_secret(&setup.home(), "mcp.x.B", &Secret::new("new-b".into())).unwrap();
    let before = ConfigError::Json {
        file: setup.file(Layer::Global),
        line: 1,
        column: 1,
    };
    settle_secrets(&setup.home(), &stored, &before);
    assert_eq!(
        read_secret(&setup.home(), "mcp.x.A").unwrap().unwrap().expose(),
        "old"
    );
    assert!(read_secret(&setup.home(), "mcp.x.B").unwrap().is_none());
    store_secret(&setup.home(), "mcp.x.A", &Secret::new("new-a".into())).unwrap();
    store_secret(&setup.home(), "mcp.x.B", &Secret::new("new-b".into())).unwrap();
    let after = ConfigError::Unsynced {
        file: setup.file(Layer::Global),
        source: io::Error::other("injected"),
    };
    settle_secrets(&setup.home(), &stored, &after);
    assert_eq!(
        read_secret(&setup.home(), "mcp.x.A").unwrap().unwrap().expose(),
        "new-a"
    );
    assert_eq!(
        read_secret(&setup.home(), "mcp.x.B").unwrap().unwrap().expose(),
        "new-b"
    );
    // The helper never fails: deleting a missing file is false, not an
    // error.
    delete_secret(&setup.home(), "mcp.x.B").unwrap();
}

#[test]
fn a_repo_add_records_the_approval_and_a_global_one_records_none() {
    let setup = Setup::new();
    let ran = ok(&setup, Layer::Repository, "docs", Some(URL), &[], &[], &[], "");
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(decision_of(&setup, "docs"), Some(Decision::Approve));
    let ran = ok(&setup, Layer::Repository, "other", Some(URL), &[], &[], &[], "");
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(decision_of(&setup, "other"), Some(Decision::Approve));
    assert_eq!(decision_of(&setup, "docs"), Some(Decision::Approve));
    // Without `--repo` nothing is recorded.
    let setup = Setup::new();
    let ran = ok(&setup, Layer::Global, "docs", Some(URL), &[], &[], &[], "");
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert!(!setup.home().join("approvals").exists(), "an approval was recorded");
}

#[test]
fn secret_values_never_reach_the_outputs_or_a_second_layer() {
    let setup = Setup::new();
    let ran = ok(
        &setup,
        Layer::Repository,
        "x",
        None,
        &["A=1"],
        &["TOKEN"],
        &["npx"],
        "s3cr3t-v4lu3\n",
    );
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert!(!ran.out.contains("s3cr3t-v4lu3"), "stdout: {}", ran.out);
    assert!(!ran.err.contains("s3cr3t-v4lu3"), "stderr: {}", ran.err);
    assert_eq!(
        files_holding(&setup.home(), &setup.workspace(), b"s3cr3t-v4lu3"),
        [setup.home().join("credentials/mcp.x.TOKEN")]
    );
}

/// The public entry only shapes the declaration; the shape round-trips:
/// a value holding `=` comes back whole through the first one.
#[test]
fn split_declaration_reads_the_shape_back_as_raw_parts() {
    let remote = super::McpDeclaration::Remote { url: URL.to_owned() };
    assert_eq!(
        super::split_declaration(&remote),
        (Some(URL), Vec::new(), Vec::new(), Vec::new())
    );
    let stdio = super::McpDeclaration::Stdio {
        command: "npx".to_owned(),
        args: vec!["-y".to_owned()],
        env: vec![("A".to_owned(), "B=C".to_owned())],
        secrets: vec!["TOKEN".to_owned()],
    };
    assert_eq!(
        super::split_declaration(&stdio),
        (
            None,
            vec!["A=B=C".to_owned()],
            vec!["TOKEN".to_owned()],
            vec!["npx".to_owned(), "-y".to_owned()],
        )
    );
}
