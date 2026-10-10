//! `fiber login` and `fiber logout` (`docs/configuration.md`, "Secrets";
//! `docs/model-routing.md`, "Logging in"), with a fake key reader so no test
//! needs a terminal.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::io::{self, BufRead, Cursor, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use config::{
    Config, CredentialFile, ProjectKey, Secret, Sources, read_credential, store_credential,
};
use contract::ErrorCode;
use contract::shapes::Failure;
use extensions::Providers;
use serde_json::{Value, json};

use doors::failure;

use super::{
    KeyReader, LoginIo, LoginName, LoginStored, LogoutTarget, Plain, credential_key, finish, login,
    login_store, login_targets, logout, run_login, run_logout, write_prompt,
};

const KEY: &str = "sk-live-7f3a9c0d1e2b";

/// Gives one key, or an error, and records that it was asked.

/// Writes the install record `extensions/<dir>/.fiber.json` holds, so the
/// directory is healthy: a directory with no record is damaged and its
/// providers are left out (`docs/extensions.md`, "Installing").
fn write_record(dir: &std::path::Path) {
    let text = std::fs::read_to_string(dir.join("extension.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    let name = manifest.get("name").and_then(|n| n.as_str()).unwrap();
    let version = manifest.get("version").and_then(|v| v.as_str()).unwrap_or("v0.0.0");
    std::fs::write(
        dir.join(".fiber.json"),
        serde_json::json!({"name": name, "version": version, "requested": true, "source": {"path": "/p"}}).to_string(),
    )
    .unwrap();
}

struct Fake {
    key: io::Result<&'static str>,
    asked: usize,
}

impl Fake {
    fn new(key: &'static str) -> Self {
        Self {
            key: Ok(key),
            asked: 0,
        }
    }
}

impl KeyReader for Fake {
    fn read_key(
        &mut self,
        prompt: &str,
        _input: &mut dyn BufRead,
        err: &mut dyn Write,
    ) -> io::Result<Secret> {
        err.write_all(prompt.as_bytes())?;
        self.asked += 1;
        match &self.key {
            Ok(key) => Ok(Secret::new(key.trim().to_owned())),
            Err(e) => Err(io::Error::new(e.kind(), "no terminal")),
        }
    }
}

struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-login");
        fs::create_dir_all(root.path().join("home")).unwrap();
        fs::create_dir_all(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs a provider `name`; `credential_name` is the stored
    /// credential it reads, `source` the key source its data declares.
    fn install(&self, name: &str, credential_name: Option<&str>, source: Option<Value>) {
        let dir = self.home().join("extensions").join(name);
        let mut data = json!({
            "name": name,
            "models": [{"id": "m", "protocol": "openai-responses", "base_url": "http://x/v1", "context_window": 1000}],
        });
        if let Some(stored) = credential_name {
            data["credential_name"] = json!(stored);
        }
        if let Some(source) = source {
            data["credential"] = source;
        }
        fs::create_dir_all(dir.join("providers")).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
        )
        .unwrap();
    write_record(&dir);
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
    }

    /// Installs the extension `extension` with no provider, declaring
    /// `secrets`.
    fn declare(&self, extension: &str, secrets: &[&str]) {
        let dir = self.home().join("extensions").join(extension);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": extension, "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "secrets": secrets})
                .to_string(),
        )
        .unwrap();
    write_record(&dir);
    }

    fn providers(&self) -> Providers {
        let (providers, notices) = Providers::load(&self.home()).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        providers
    }

    fn config(&self) -> Config {
        Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: Vec::new(),
        })
        .unwrap()
    }

    fn write_config(&self, value: &Value) {
        fs::write(self.home().join("config.json"), value.to_string()).unwrap();
    }

    fn global(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.home().join("config.json")).unwrap()).unwrap()
    }

    fn stored(&self, name: &str, label: &str) -> Option<String> {
        read_credential(&self.home(), name, label)
            .unwrap()
            .map(|s| s.expose().to_owned())
    }

    /// Logs in with `typed` on stdin and `keys` reading the key.
    fn login(
        &self,
        provider: Option<&str>,
        terminal: bool,
        typed: &str,
        keys: &mut Fake,
    ) -> (Result<(), Failure>, String) {
        self.login_as(provider, None, terminal, typed, keys)
    }

    /// Like [`Setup::login`], with an `--as` label.
    fn login_as(
        &self,
        provider: Option<&str>,
        label: Option<&str>,
        terminal: bool,
        typed: &str,
        keys: &mut Fake,
    ) -> (Result<(), Failure>, String) {
        let mut err = Vec::new();
        let providers = self.providers();
        let result = login(
            provider,
            label,
            &mut LoginIo {
                home: &self.home(),
                providers: &providers,
                terminal,
                stdin: &mut Cursor::new(typed.to_owned()),
                err: &mut err,
                keys,
                device: false,
                clock: fakes::clock::FakeClock::new(),
            },
        );
        (result, String::from_utf8(err).unwrap())
    }

    fn logout(&self, provider: Option<&str>) -> (Result<(), Failure>, String) {
        self.logout_target(provider, LogoutTarget::Only)
    }

    fn logout_target(
        &self,
        provider: Option<&str>,
        target: LogoutTarget<'_>,
    ) -> (Result<(), Failure>, String) {
        let mut err = Vec::new();
        let result = logout(
            provider,
            target,
            &self.home(),
            &self.providers(),
            &self.config(),
            &mut err,
        );
        (result, String::from_utf8(err).unwrap())
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn failed(result: Result<(), Failure>) -> Failure {
    result.unwrap_err()
}

#[test]
fn a_key_is_stored_in_a_0600_file_and_named_by_one_line_without_the_key() {
    let setup = Setup::new();
    setup.install("openrouter", None, None);
    let (result, err) = setup.login(Some("openrouter"), false, "", &mut Fake::new(KEY));
    result.unwrap();
    let file = setup.home().join("credentials/openrouter/default");
    assert_eq!(fs::read_to_string(&file).unwrap(), KEY);
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&setup.home().join("credentials/openrouter")), 0o700);
    assert_eq!(mode(&setup.home().join("credentials")), 0o700);
    assert_eq!(err, "fiber: stored credentials/openrouter/default\n");
}

#[test]
fn a_key_is_trimmed_and_an_empty_one_stores_nothing() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    for empty in ["", "  \n", "\t"] {
        let (result, err) = setup.login(Some("acme"), false, "", &mut Fake::new(empty));
        let e = failed(result);
        assert_eq!(e.code, ErrorCode::Usage, "{empty:?}");
        assert!(e.message.starts_with("No key was given"), "{}", e.message);
        assert_eq!(err, "");
    }
    assert!(!setup.home().join("credentials/acme/default").exists());
    assert!(!setup.home().join("config.json").exists());
    let (result, _) = setup.login(Some("acme"), false, "", &mut Fake::new("  sk-a \n"));
    result.unwrap();
    assert_eq!(setup.stored("acme", "default").as_deref(), Some("sk-a"));
}

#[test]
fn providers_that_share_a_credential_store_under_the_one_directory() {
    let setup = Setup::new();
    setup.install("opencode-go", Some("opencode"), None);
    setup.install("opencode-zen", Some("opencode"), None);
    let (result, err) = setup.login(Some("opencode-zen"), false, "", &mut Fake::new(KEY));
    result.unwrap();
    assert_eq!(err, "fiber: stored credentials/opencode/default\n");
    assert_eq!(setup.stored("opencode", "default").as_deref(), Some(KEY));
    assert!(!setup.home().join("credentials/opencode-zen").exists());
    // The sibling reads the same file, so it is already logged in.
    let (result, _) = setup.login(Some("opencode-go"), false, "", &mut Fake::new("other"));
    let e = failed(result);
    assert!(
        e.message.contains(
            "credentials/opencode/default is already stored; log in under another label with --as <label>, or run `fiber logout opencode-go --as default` first"
        ),
        "{}",
        e.message
    );
}

#[test]
fn a_label_already_stored_is_refused_and_the_file_is_untouched() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    store_credential(&setup.home(), "acme", "default", &Secret::new("old".into())).unwrap();
    let mut keys = Fake::new(KEY);
    let (result, err) = setup.login(Some("acme"), false, "", &mut keys);
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::Usage);
    assert_eq!(
        e.message,
        "credentials/acme/default is already stored; log in under another label with --as <label>, or run `fiber logout acme --as default` first. Run `fiber --help` for usage."
    );
    assert_eq!(err, "");
    assert_eq!(keys.asked, 0);
    assert_eq!(setup.stored("acme", "default").as_deref(), Some("old"));
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn the_first_label_is_written_to_the_global_file_once() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    setup.write_config(&json!({"model": "acme/m"}));
    setup
        .login(Some("acme"), false, "", &mut Fake::new(KEY))
        .0
        .unwrap();
    assert_eq!(
        setup.global(),
        json!({"model": "acme/m", "providers": {"acme": {"credential": "default"}}})
    );
    assert_eq!(
        setup
            .config()
            .get("providers.acme.credential", None)
            .unwrap()
            .0,
        json!("default")
    );
}

#[test]
fn a_credential_key_already_set_is_not_overwritten() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    setup.write_config(&json!({"providers": {"acme": {"credential": "work"}}}));
    setup
        .login(Some("acme"), false, "", &mut Fake::new(KEY))
        .0
        .unwrap();
    assert_eq!(setup.global()["providers"]["acme"]["credential"], "work");
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
}

#[test]
fn a_provider_name_with_a_dot_is_quoted_in_the_key() {
    assert_eq!(
        credential_key("acme.api"),
        r#"providers."acme.api".credential"#
    );
    assert_eq!(credential_key("acme"), "providers.acme.credential");
    let setup = Setup::new();
    setup.install("acme.api", None, None);
    setup
        .login(Some("acme.api"), false, "", &mut Fake::new(KEY))
        .0
        .unwrap();
    assert_eq!(
        setup.global(),
        json!({"providers": {"acme.api": {"credential": "default"}}})
    );
}

#[test]
fn a_config_write_that_fails_leaves_no_stored_key_so_a_retry_works() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    fs::write(setup.home().join("config.json"), "{\"model\": ,").unwrap();
    let (result, err) = setup.login(Some("acme"), false, "", &mut Fake::new(KEY));
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::ConfigInvalid);
    assert!(!e.message.contains(KEY));
    assert_eq!(err, "");
    assert_eq!(setup.stored("acme", "default"), None);
    fs::write(setup.home().join("config.json"), "{}").unwrap();
    setup
        .login(Some("acme"), false, "", &mut Fake::new(KEY))
        .0
        .unwrap();
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
}

#[test]
fn a_login_while_another_holds_the_lock_fails_and_stores_nothing() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    let held = CredentialFile::new(&setup.home(), "acme", "default")
        .unwrap()
        .try_lock()
        .unwrap()
        .unwrap();
    let mut keys = Fake::new(KEY);
    let (result, _) = setup.login(Some("acme"), false, "", &mut keys);
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::IoFailed);
    assert_eq!(e.message, "another login for acme is running");
    assert_eq!(keys.asked, 0);
    drop(held);
    assert_eq!(setup.stored("acme", "default"), None);
    setup
        .login(Some("acme"), false, "", &mut Fake::new(KEY))
        .0
        .unwrap();
}

#[test]
fn an_unknown_provider_is_a_usage_error_naming_the_installed_ones() {
    let setup = Setup::new();
    let (result, _) = setup.login(Some("nope"), false, "", &mut Fake::new(KEY));
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::Usage);
    assert!(
        e.message.contains("no provider is installed"),
        "{}",
        e.message
    );
    setup.install("zed", None, None);
    setup.install("alpha", None, None);
    let (result, _) = setup.login(Some("nope"), false, "", &mut Fake::new(KEY));
    let e = failed(result);
    assert_eq!(
        e.message,
        "`nope` is neither an installed provider nor a declared secret; the installed providers are alpha, zed, and no installed extension declares a secret. Run `fiber --help` for usage."
    );
    let (result, _) = setup.logout(Some("nope"));
    assert_eq!(failed(result).code, ErrorCode::Usage);
    assert!(!setup.home().join("credentials").exists());
}

#[test]
fn a_provider_name_that_climbs_out_of_credentials_is_refused() {
    let setup = Setup::new();
    setup.install("acme", Some("../escape"), None);
    let (result, _) = setup.login(Some("acme"), false, "", &mut Fake::new(KEY));
    assert_eq!(failed(result).code, ErrorCode::InvalidArguments);
    assert!(!setup.home().join("escape").exists());
}

fn menu_setup() -> Setup {
    let setup = Setup::new();
    setup.install("alpha", None, None);
    setup.install("beta", None, None);
    setup
}

#[test]
fn the_menu_picks_a_provider_by_number_or_by_name() {
    for (typed, stored) in [
        ("1\n", "alpha"),
        ("2\n", "beta"),
        (" beta \n", "beta"),
        ("alpha\n", "alpha"),
    ] {
        let setup = menu_setup();
        let (result, err) = setup.login(None, true, typed, &mut Fake::new(KEY));
        result.unwrap();
        assert!(
            err.starts_with(
                "Providers:\n  1) alpha\n  2) beta\nProvider or secret, by number or name: "
            ),
            "{err}"
        );
        assert!(err.contains(&format!("Key for {stored}: ")), "{err}");
        assert!(
            err.ends_with(&format!("fiber: stored credentials/{stored}/default\n")),
            "{err}"
        );
        assert_eq!(
            setup.stored(stored, "default").as_deref(),
            Some(KEY),
            "{typed:?}"
        );
        assert!(!err.contains(KEY));
    }
}

#[test]
fn the_menu_refuses_a_bad_answer_and_stores_nothing() {
    for typed in [
        "0\n",
        "3\n",
        "-1\n",
        "gamma\n",
        "\n",
        "",
        "99999999999999999999\n",
    ] {
        let setup = menu_setup();
        let mut keys = Fake::new(KEY);
        let (result, _) = setup.login(None, true, typed, &mut keys);
        let e = failed(result);
        assert_eq!(e.code, ErrorCode::Usage, "{typed:?}");
        if typed.trim().is_empty() {
            assert!(e.message.contains("Nothing was chosen"), "{typed:?}");
        } else {
            assert!(e.message.contains("is neither a listed"), "{typed:?}");
        }
        assert_eq!(keys.asked, 0, "{typed:?}");
        assert!(!setup.home().join("credentials/alpha").exists());
        assert!(!setup.home().join("credentials/beta").exists());
    }
}

#[test]
fn without_a_terminal_no_provider_is_a_usage_error_that_reads_nothing() {
    let setup = menu_setup();
    let mut keys = Fake::new(KEY);
    let (result, err) = setup.login(None, false, "1\n", &mut keys);
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::Usage);
    assert!(e.message.contains("no terminal"), "{}", e.message);
    assert_eq!(err, "");
    assert_eq!(keys.asked, 0);
}

#[test]
fn a_terminal_with_no_provider_installed_says_so() {
    let setup = Setup::new();
    let (result, _) = setup.login(None, true, "1\n", &mut Fake::new(KEY));
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::Usage);
    assert_eq!(
        e.message,
        "No provider is installed, and no installed extension declares a secret. Run `fiber --help` for usage."
    );
}

#[test]
fn the_prompt_is_for_a_terminal_only_and_a_failed_read_stores_nothing() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    let (result, err) = setup.login(Some("acme"), false, "", &mut Fake::new(KEY));
    result.unwrap();
    assert!(!err.contains("Key for"), "{err}");
    fs::remove_dir_all(setup.home().join("credentials")).unwrap();
    fs::remove_file(setup.home().join("config.json")).unwrap();
    let mut broken = Fake {
        key: Err(io::Error::from(io::ErrorKind::Other)),
        asked: 0,
    };
    let (result, err) = setup.login(Some("acme"), true, "", &mut broken);
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::IoFailed);
    assert_eq!(err, "Key for acme: ");
    assert_eq!(setup.stored("acme", "default"), None);
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn logout_deletes_the_one_stored_key_and_names_it() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    store_credential(&setup.home(), "acme", "work", &Secret::new(KEY.into())).unwrap();
    let (result, err) = setup.logout(Some("acme"));
    result.unwrap();
    assert_eq!(err, "fiber: removed credentials/acme/work\n");
    assert!(!setup.home().join("credentials/acme/work").exists());
    let e = failed(setup.logout(Some("acme")).0);
    assert_eq!(e.code, ErrorCode::CredentialMissing);
    assert_eq!(e.message, "no stored credential for acme");
}

#[test]
fn logging_in_then_out_leaves_the_global_label_and_no_key() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    setup
        .login(Some("acme"), false, "", &mut Fake::new(KEY))
        .0
        .unwrap();
    setup.logout(Some("acme")).0.unwrap();
    assert_eq!(setup.stored("acme", "default"), None);
    // The label stays: the next login has nothing to write and still works.
    assert_eq!(setup.global()["providers"]["acme"]["credential"], "default");
    setup
        .login(Some("acme"), false, "", &mut Fake::new(KEY))
        .0
        .unwrap();
}

#[test]
fn logging_out_of_a_shared_credential_names_the_providers_that_also_read_it() {
    let setup = Setup::new();
    setup.install("opencode-go", Some("opencode"), None);
    setup.install("opencode-zen", Some("opencode"), None);
    setup.install("other", None, None);
    store_credential(
        &setup.home(),
        "opencode",
        "default",
        &Secret::new(KEY.into()),
    )
    .unwrap();
    let (result, err) = setup.logout(Some("opencode-zen"));
    result.unwrap();
    assert_eq!(
        err,
        "fiber: removed credentials/opencode/default, which opencode-go also reads\n"
    );
    assert_eq!(setup.stored("opencode", "default"), None);
}

#[test]
fn several_stored_labels_are_a_usage_error_listing_them_and_delete_nothing() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    for label in ["work", "default"] {
        store_credential(&setup.home(), "acme", label, &Secret::new(KEY.into())).unwrap();
    }
    let (result, err) = setup.logout(Some("acme"));
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::Usage);
    assert_eq!(
        e.message,
        "`acme` has several stored credentials: default, work; name one with --as <label>, or use --all. Run `fiber --help` for usage."
    );
    assert_eq!(err, "");
    assert_eq!(setup.stored("acme", "work").as_deref(), Some(KEY));
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
}

#[test]
fn logout_without_a_provider_is_a_usage_error() {
    let setup = Setup::new();
    let e = failed(setup.logout(None).0);
    assert_eq!(e.code, ErrorCode::Usage);
    assert_eq!(e.message, crate::LOGOUT_SHAPE);
}

#[test]
fn a_key_from_the_environment_a_file_or_a_command_is_named_and_not_removed() {
    for (source, named) in [
        (
            json!({"env": "ACME_KEY"}),
            "acme's key comes from the environment variable ACME_KEY; fiber logout cannot remove it",
        ),
        (
            json!({"file": "/etc/acme.key"}),
            "acme's key comes from the file /etc/acme.key; fiber logout cannot remove it",
        ),
        (
            json!({"command": ["pass", "show", "sk-live-secret-arg"]}),
            "acme's key comes from the command pass; fiber logout cannot remove it",
        ),
    ] {
        let setup = Setup::new();
        setup.install("acme", None, Some(source));
        let (result, err) = setup.logout(Some("acme"));
        let e = failed(result);
        assert_eq!(e.code, ErrorCode::CredentialMissing);
        assert_eq!(e.message, named);
        assert_eq!(err, "");
        assert!(!setup.home().join("credentials").exists());
    }
}

#[test]
fn a_configured_source_comes_before_the_providers_own_in_label_order() {
    let setup = Setup::new();
    setup.install("acme", None, Some(json!({"env": "OWN_KEY"})));
    // Written by hand with `work` first: the order is the labels', not the
    // file's.
    fs::write(
        setup.home().join("config.json"),
        r#"{"providers": {"acme": {"credentials": {
            "work": {"env": "WORK_KEY"},
            "home": {"file": "/tmp/home.key"}
        }}}}"#,
    )
    .unwrap();
    let e = failed(setup.logout(Some("acme")).0);
    assert_eq!(
        e.message,
        "acme's key comes from the file /tmp/home.key; fiber logout cannot remove it"
    );
}

#[test]
fn a_stored_key_is_deleted_even_when_a_source_is_configured() {
    let setup = Setup::new();
    setup.install("acme", None, Some(json!({"env": "OWN_KEY"})));
    store_credential(&setup.home(), "acme", "default", &Secret::new(KEY.into())).unwrap();
    setup.logout(Some("acme")).0.unwrap();
    assert_eq!(setup.stored("acme", "default"), None);
}

#[test]
fn no_stored_key_and_no_source_is_a_failure_that_names_neither() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    let e = failed(setup.logout(Some("acme")).0);
    assert_eq!(e.code, ErrorCode::CredentialMissing);
    assert_eq!(e.message, "no stored credential for acme");
}

#[test]
fn a_prompt_is_written_before_the_key_is_read_and_an_empty_one_writes_nothing() {
    let mut err = Vec::new();
    let key = Plain
        .read_key("Key: ", &mut Cursor::new(" k \n"), &mut err)
        .unwrap();
    assert_eq!(key.expose(), "k");
    assert_eq!(err, b"Key: ");
    let mut quiet = Vec::new();
    write_prompt("", &mut quiet).unwrap();
    assert!(quiet.is_empty());
}

#[test]
fn fail_gives_the_failures_exit_code() {
    assert_eq!(crate::fail(failure(ErrorCode::Usage, "bad usage")), 2);
    assert_eq!(crate::fail(failure(ErrorCode::IoFailed, "disk")), 1);
}

#[test]
fn finish_is_zero_on_success_and_the_exit_code_on_failure() {
    assert_eq!(finish(Ok(())), 0);
    assert_eq!(finish(Err(failure(ErrorCode::Usage, "bad usage"))), 2);
    assert_eq!(finish(Err(failure(ErrorCode::IoFailed, "disk"))), 1);
}

#[test]
fn logout_without_a_provider_is_a_usage_failure() {
    assert_eq!(run_logout(None, LogoutTarget::Only), 2);
}

/// The child's marker: set, the test runs `run_login` and exits with its code.
const CHILD: &str = "FIBER_CLI_TEST_CHILD";

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn login_of_an_unknown_provider_is_a_usage_failure() {
    // Fails in `installed`, before any lock, prompt or read of stdin. It runs
    // in a child with an empty Fiber home and no stdin, so neither the
    // owner's home nor a terminal can change the outcome.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(run_login(
            Some("no-such-provider-for-the-test"),
            None,
            false,
            fakes::clock::FakeClock::new(),
        ));
    }
    let home = fakes::TempDir::new("fiber-login-child");
    let name = module_path!().split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::login_of_an_unknown_provider_is_a_usage_failure"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", home.path())
        .env(CHILD, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait().unwrap()));
    let Ok(status) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for `fiber login` of an unknown provider to exit");
    };
    assert_eq!(status.code(), Some(2), "{status}");
}

#[test]
fn login_targets_list_providers_then_secrets() {
    let setup = Setup::new();
    setup.install("beta", None, None);
    setup.install("alpha", None, None);
    setup.declare("acme", &["beta", "acme.api_key"]);
    assert_eq!(
        login_targets(&setup.home()).unwrap(),
        [
            LoginName::Provider("alpha".to_owned()),
            LoginName::Provider("beta".to_owned()),
            LoginName::Secret("acme.api_key".to_owned()),
        ]
    );
}

#[test]
fn login_targets_with_nothing_installed_is_empty() {
    let setup = Setup::new();
    assert!(login_targets(&setup.home()).unwrap().is_empty());
}

#[test]
fn login_store_stores_the_key_and_the_first_label() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    let stored = login_store(&setup.home(), "acme", None, Secret::new(KEY.into())).unwrap();
    assert_eq!(
        stored,
        LoginStored {
            path: "credentials/acme/default".to_owned(),
            replaced: false,
        }
    );
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
    assert_eq!(
        setup.global(),
        json!({"providers": {"acme": {"credential": "default"}}})
    );
}

#[test]
fn login_store_under_a_label() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    let stored = login_store(&setup.home(), "acme", Some("work"), Secret::new(KEY.into())).unwrap();
    assert_eq!(stored.path, "credentials/acme/work");
    assert!(!stored.replaced);
    assert_eq!(setup.stored("acme", "work").as_deref(), Some(KEY));
}

#[test]
fn login_store_refuses_an_already_stored_label_without_the_cli_hint() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    store_credential(&setup.home(), "acme", "default", &Secret::new("old".into())).unwrap();
    let error = login_store(&setup.home(), "acme", None, Secret::new(KEY.into())).unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert_eq!(
        error.message,
        "credentials/acme/default is already stored; log in under another label with --as <label>, or run `fiber logout acme --as default` first."
    );
    assert_eq!(setup.stored("acme", "default").as_deref(), Some("old"));
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn login_store_refuses_an_empty_key_and_stores_nothing() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    let error = login_store(&setup.home(), "acme", None, Secret::new(String::new())).unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert_eq!(error.message, "No key was given; nothing was stored.");
    assert!(!setup.home().join("credentials/acme/default").exists());
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn login_store_replaces_a_declared_secret_and_says_so() {
    let setup = Setup::new();
    setup.declare("acme", &["acme.api_key"]);
    let first = login_store(
        &setup.home(),
        "acme.api_key",
        None,
        Secret::new("v1".into()),
    )
    .unwrap();
    assert_eq!(
        first,
        LoginStored {
            path: "credentials/acme.api_key".to_owned(),
            replaced: false,
        }
    );
    let second = login_store(
        &setup.home(),
        "acme.api_key",
        None,
        Secret::new("v2".into()),
    )
    .unwrap();
    assert_eq!(second.path, "credentials/acme.api_key");
    assert!(second.replaced);
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme.api_key")).unwrap(),
        "v2"
    );
}

#[test]
fn login_store_of_an_unknown_name_is_a_usage_refusal() {
    let setup = Setup::new();
    let error = login_store(&setup.home(), "nope", None, Secret::new(KEY.into())).unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert_eq!(
        error.message,
        "`nope` is neither an installed provider nor a declared secret; no provider is installed, and no installed extension declares a secret."
    );
    assert!(!setup.home().join("credentials").exists());
}

#[test]
fn login_store_while_another_holds_the_lock_stores_nothing() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    let held = CredentialFile::new(&setup.home(), "acme", "default")
        .unwrap()
        .try_lock()
        .unwrap()
        .unwrap();
    let error = login_store(&setup.home(), "acme", None, Secret::new(KEY.into())).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.message, "another login for acme is running");
    drop(held);
    assert_eq!(setup.stored("acme", "default"), None);
}

#[path = "login_label_tests.rs"]
mod label;
#[path = "login_secret_tests.rs"]
mod secret;

#[test]
fn login_targets_list_a_browser_provider_as_browser_in_provider_order() {
    let setup = Setup::new();
    setup.install("beta", None, None);
    setup.install("alpha", None, None);
    let path = setup.home().join("extensions/beta/providers/beta.json");
    let mut data: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    data["login"] = json!("browser");
    fs::write(path, data.to_string()).unwrap();
    setup.declare("acme", &["acme.api_key"]);
    assert_eq!(
        login_targets(&setup.home()).unwrap(),
        [
            LoginName::Provider("alpha".to_owned()),
            LoginName::Browser("beta".to_owned()),
            LoginName::Secret("acme.api_key".to_owned()),
        ]
    );
}

#[test]
fn the_menu_numbers_a_browser_provider_among_the_providers() {
    let setup = Setup::new();
    setup.install("alpha", None, None);
    setup.install("beta", None, None);
    let path = setup.home().join("extensions/beta/providers/beta.json");
    let mut data: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    data["login"] = json!("browser");
    fs::write(path, data.to_string()).unwrap();
    let (result, err) = setup.login(None, true, "1\n", &mut Fake::new(KEY));
    result.unwrap();
    assert!(
        err.starts_with(
            "Providers:\n  1) alpha\n  2) beta\nProvider or secret, by number or name: "
        ),
        "{err}"
    );
    assert_eq!(setup.stored("alpha", "default").as_deref(), Some(KEY));
}
