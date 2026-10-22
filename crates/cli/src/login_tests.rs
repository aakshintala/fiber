//! `fiber login` and `fiber logout` (`docs/configuration.md`, "Secrets";
//! `docs/model-routing.md`, "Logging in"), with a fake key reader so no test
//! needs a terminal.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use crate::test_support::{
    approve_repo, declare_repo, never_repo, repo_data, repo_manifest, write_record,
};
use std::fs;
use std::io::{self, BufRead, Cursor, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

use config::{
    Config, CredentialFile, ProjectKey, Secret, Sources, read_credential, store_credential,
};
use contract::shapes::Failure;
use contract::{self, ErrorCode};
use extensions::Providers;
use serde_json::{Value, json};

use doors::failure;

use super::{
    ClaimProbe, KeyReader, LoginIo, LoginName, LoginStored, LogoutTarget, Plain, ReviewerModels,
    credential_key, login, login_store, login_targets, login_with_notices, logout, reviewer_stage,
    run_login, run_logout, store, targets, write_prompt,
};
use crate::{CliError, Refusal, finish};

const KEY: &str = "sk-live-7f3a9c0d1e2b";

/// Gives one key, or an error, and records that it was asked.
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

    fn read_key_exact(
        &mut self,
        prompt: &str,
        input: &mut dyn BufRead,
        err: &mut dyn Write,
    ) -> io::Result<Secret> {
        err.write_all(prompt.as_bytes())?;
        self.asked += 1;
        super::read_exact_line(input)
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
        let mut manifest = json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
        manifest["providers"] = serde_json::Value::Object(
            [(name.to_owned(), json!(["http://x/v1"]))]
                .into_iter()
                .collect(),
        );
        fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
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
    ) -> (Result<(), CliError>, String) {
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
    ) -> (Result<(), CliError>, String) {
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

    fn logout(&self, provider: Option<&str>) -> (Result<(), CliError>, String) {
        self.logout_target(provider, LogoutTarget::Only)
    }

    fn logout_target(
        &self,
        provider: Option<&str>,
        target: LogoutTarget<'_>,
    ) -> (Result<(), CliError>, String) {
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

fn failed(result: Result<(), CliError>) -> CliError {
    result.unwrap_err()
}

#[test]
fn login_with_notices_prints_the_config_notice_as_one_line_first() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    setup.write_config(&json!({"frobnicate": 1}));
    let mut err = Vec::new();
    let (providers, loading) = Providers::load(&setup.home()).unwrap();
    assert!(loading.is_empty(), "{loading:?}");
    let mut keys = Fake::new(KEY);
    let result = login_with_notices(
        Some("acme"),
        None,
        &mut LoginIo {
            home: &setup.home(),
            providers: &providers,
            terminal: false,
            stdin: &mut Cursor::new(""),
            err: &mut err,
            keys: &mut keys,
            device: false,
            clock: fakes::clock::FakeClock::new(),
        },
        loading,
    );
    result.unwrap();
    assert_eq!(
        String::from_utf8(err).unwrap(),
        format!(
            "{}: ignored `frobnicate`, which this Fiber does not know.\n\
             fiber: stored credentials/acme/default\n",
            setup.home().join("config.json").display()
        )
    );
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
}

#[test]
fn login_with_a_config_that_does_not_load_fails_instead_of_logging_in() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    // The project's file is invalid JSON, so the notices load fails, while
    // the global file the login writes stays fine: the host step's own
    // load still fails the login instead of skipping to a success report.
    let (_, project) = crate::project_of(&setup.home(), &std::env::current_dir().unwrap()).unwrap();
    let file = setup
        .home()
        .join("projects")
        .join(project.as_str())
        .join("config.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "not json").unwrap();
    let mut err = Vec::new();
    let (providers, loading) = Providers::load(&setup.home()).unwrap();
    let mut keys = Fake::new(KEY);
    let result = login_with_notices(
        Some("acme"),
        None,
        &mut LoginIo {
            home: &setup.home(),
            providers: &providers,
            terminal: false,
            stdin: &mut Cursor::new(""),
            err: &mut err,
            keys: &mut keys,
            device: false,
            clock: fakes::clock::FakeClock::new(),
        },
        loading,
    );
    let error = failed(result);
    assert!(matches!(error, CliError::Config(_)), "{error:?}");
    assert_eq!(String::from_utf8(err).unwrap(), "");
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
        assert!(
            matches!(
                e,
                CliError::StoreRefused {
                    refusal: Refusal::NoKeyGiven,
                    hint: true,
                }
            ),
            "{empty:?}: {e:?}"
        );
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
        matches!(
            e,
            CliError::StoreRefused {
                refusal: Refusal::AlreadyStored { .. },
                hint: true,
            }
        ),
        "{e:?}"
    );
    assert!(
        e.to_string().contains(
            "credentials/opencode/default is already stored; log in under another label with --as <label>, or run `fiber logout opencode-go --as default` first"
        ),
        "{e}"
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
    assert!(
        matches!(
            e,
            CliError::StoreRefused {
                refusal: Refusal::AlreadyStored {
                    name: _,
                    stored: _,
                    label: _,
                },
                hint: true,
            }
        ),
        "{e:?}"
    );
    assert_eq!(
        e.to_string(),
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
    assert!(matches!(e, CliError::Config(_)), "{e:?}");
    assert!(!e.to_string().contains(KEY));
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
    assert!(
        matches!(e, CliError::LoginRunning { ref name } if name == "acme"),
        "{e:?}"
    );
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
    assert!(
        matches!(
            e,
            CliError::StoreRefused {
                refusal: Refusal::UnknownName { .. },
                hint: true,
            }
        ),
        "{e:?}"
    );
    setup.install("zed", None, None);
    setup.install("alpha", None, None);
    let (result, _) = setup.login(Some("nope"), false, "", &mut Fake::new(KEY));
    let e = failed(result);
    assert_eq!(
        e.to_string(),
        "`nope` is neither an installed provider nor a declared secret; the installed providers are alpha, zed, and no installed extension declares a secret. Run `fiber --help` for usage."
    );
    let (result, _) = setup.logout(Some("nope"));
    assert!(matches!(
        failed(result),
        CliError::ProviderNotInstalled { .. }
    ));
    assert!(!setup.home().join("credentials").exists());
}

#[test]
fn a_provider_name_that_climbs_out_of_credentials_is_refused() {
    let setup = Setup::new();
    setup.install("acme", Some("../escape"), None);
    let (result, _) = setup.login(Some("acme"), false, "", &mut Fake::new(KEY));
    let e = failed(result);
    assert!(matches!(e, CliError::CredentialFile(_)), "{e:?}");
    assert_eq!(e.code(), ErrorCode::InvalidArguments);
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
        ("1\n1\n", "alpha"),
        ("2\n1\n", "beta"),
        (" beta \n1\n", "beta"),
        ("alpha\n1\n", "alpha"),
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
        // The stored credential precedes the reviewer question: each
        // installed provider lists its one model `m`.
        let stored_at = err
            .find(&format!("fiber: stored credentials/{stored}/default\n"))
            .unwrap();
        let asked_at = err.find("Reviewer for ").unwrap();
        assert!(stored_at < asked_at, "{err}");
        assert!(
            err.ends_with(&format!("fiber: reviewer {stored} m\n")),
            "{err}"
        );
        assert_eq!(
            setup.stored(stored, "default").as_deref(),
            Some(KEY),
            "{typed:?}"
        );
        assert_eq!(
            setup.global()["providers"][stored]["reviewer_model"],
            json!("m"),
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
        if typed.trim().is_empty() {
            assert!(matches!(e, CliError::NothingChosen), "{typed:?}: {e:?}");
        } else {
            assert!(
                matches!(e, CliError::ChoiceUnknown { .. }),
                "{typed:?}: {e:?}"
            );
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
    assert!(matches!(e, CliError::LoginNeedsName), "{e:?}");
    assert_eq!(err, "");
    assert_eq!(keys.asked, 0);
}

#[test]
fn a_terminal_with_no_provider_installed_says_so() {
    let setup = Setup::new();
    let (result, _) = setup.login(None, true, "1\n", &mut Fake::new(KEY));
    let e = failed(result);
    assert!(matches!(e, CliError::NothingToLogInTo), "{e:?}");
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
    assert!(matches!(e, CliError::Terminal(_)), "{e:?}");
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
    assert!(
        matches!(e, CliError::NoStoredCredential { ref name } if name == "acme"),
        "{e:?}"
    );
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
    assert!(matches!(e, CliError::LogoutSeveral { .. }), "{e:?}");
    assert_eq!(
        e.to_string(),
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
    assert!(matches!(e, CliError::LogoutNeedsProvider), "{e:?}");
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
        assert!(matches!(e, CliError::KeyFromElsewhere { .. }), "{e:?}");
        assert_eq!(e.to_string(), named);
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
    assert!(matches!(e, CliError::KeyFromElsewhere { .. }), "{e:?}");
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
    assert!(
        matches!(e, CliError::NoStoredCredential { ref name } if name == "acme"),
        "{e:?}"
    );
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
    assert_eq!(finish(Ok::<(), Failure>(())), 0);
    assert_eq!(finish(Err(failure(ErrorCode::Usage, "bad usage"))), 2);
    assert_eq!(finish(Err(failure(ErrorCode::IoFailed, "disk"))), 1);
}

#[test]
fn logout_without_a_provider_is_a_usage_failure() {
    assert_eq!(run_logout(None, LogoutTarget::Only), 2);
}

/// The child's marker: set, the test runs `run_login` and exits with its code.
const CHILD: &str = "FIBER_CLI_TEST_CHILD";

/// How long the re-run child may run before the test kills its group and
/// fails: half of nextest's per-test timeout less the cleanup bound.
const CHILD_DEADLINE: Duration = Duration::from_secs(50);

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
    let output = fakes::rerun_prepared(
        &format!("{name}::login_of_an_unknown_provider_is_a_usage_failure"),
        &[(CHILD, "1")],
        fakes::Deadline::after(CHILD_DEADLINE),
        |command| {
            command.env("FIBER_HOME", home.path());
        },
    );
    assert_eq!(output.status.code(), Some(2), "{}", output.status);
}

#[test]
fn run_login_with_uses_the_callers_reader_while_stdin_is_locked() {
    // The child holds the standard input lock, as `fiber setup` does on a
    // terminal: a login that locks standard input again never returns, so
    // the child is killed at the deadline.
    if std::env::var_os(CHILD).is_some() {
        let _held = io::stdin().lock();
        std::process::exit(super::run_login_with(
            Some("acme"),
            None,
            false,
            fakes::clock::FakeClock::new(),
            Some(&mut Cursor::new("sk-key\n")),
        ));
    }
    let setup = Setup::new();
    setup.install("acme", None, None);
    let name = module_path!().split_once("::").unwrap().1;
    let output = fakes::rerun_prepared(
        &format!("{name}::run_login_with_uses_the_callers_reader_while_stdin_is_locked"),
        &[(CHILD, "1")],
        fakes::Deadline::after(CHILD_DEADLINE),
        |command| {
            command.env("FIBER_HOME", setup.home());
        },
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme/default")).unwrap(),
        "sk-key"
    );
}

#[test]
fn login_of_an_unknown_provider_names_the_home_installed_providers() {
    // Pins `home_and_providers`: the message must name the providers the
    // child's FIBER_HOME installs. An empty home and empty providers keep
    // the exit code but say none is installed.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(run_login(
            Some("no-such-provider-for-the-test"),
            None,
            false,
            fakes::clock::FakeClock::new(),
        ));
    }
    let setup = Setup::new();
    setup.install("acme", None, None);
    let name = module_path!().split_once("::").unwrap().1;
    let output = fakes::rerun_prepared(
        &format!("{name}::login_of_an_unknown_provider_names_the_home_installed_providers"),
        &[(CHILD, "1")],
        fakes::Deadline::after(CHILD_DEADLINE),
        |command| {
            command.env("FIBER_HOME", setup.home());
        },
    );
    assert_eq!(output.status.code(), Some(2), "{}", output.status);
    let stderr = String::from_utf8(output.stderr).unwrap();
    let line = "`no-such-provider-for-the-test` is neither an installed provider nor a \
                declared secret; the installed providers are acme, and no installed \
                extension declares a secret.";
    assert_eq!(stderr.matches(line).count(), 1, "{stderr:?}");
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
    assert!(
        matches!(
            error,
            CliError::StoreRefused {
                refusal: Refusal::AlreadyStored { .. },
                hint: false,
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
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
    assert!(
        matches!(
            error,
            CliError::StoreRefused {
                refusal: Refusal::NoKeyGiven,
                hint: false,
            }
        ),
        "{error:?}"
    );
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
    assert!(
        matches!(
            error,
            CliError::StoreRefused {
                refusal: Refusal::UnknownName { .. },
                hint: false,
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
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
    assert!(
        matches!(error, CliError::LoginRunning { ref name } if name == "acme"),
        "{error:?}"
    );
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

#[test]
fn read_key_exact_keeps_spaces_and_drops_only_the_final_newline() {
    for (input, stored) in [
        (" token \n", " token "),
        ("token\r\n", "token"),
        ("token", "token"),
        ("  \n", "  "),
        ("\n", ""),
        // Only `\n` or `\r\n` is removed: a trailing `\r` with no
        // `\n` stays.
        ("token\r", "token\r"),
        (" \r", " \r"),
        ("\r", "\r"),
    ] {
        let mut keys = Plain;
        let mut err = Vec::new();
        let secret = keys
            .read_key_exact("", &mut Cursor::new(input.as_bytes().to_vec()), &mut err)
            .unwrap();
        assert_eq!(secret.expose(), stored, "input {input:?}");
    }
}

/// How long a test waits for one stage barrier or worker.
const STAGE_WAIT: Duration = Duration::from_secs(10);

fn choices(models: &[&str]) -> ReviewerModels {
    ReviewerModels {
        models: models.iter().map(|model| (*model).to_owned()).collect(),
        selected: None,
        notices: Vec::new(),
    }
}

#[test]
fn a_terminal_key_login_asks_for_the_reviewer_after_storing() {
    let setup = Setup::new();
    setup.install("alpha", None, None);
    let (result, err) = setup.login(Some("alpha"), true, "1\n", &mut Fake::new(KEY));
    result.unwrap();
    let stored_at = err
        .find("fiber: stored credentials/alpha/default\n")
        .unwrap();
    let asked_at = err.find("Reviewer for alpha:").unwrap();
    assert!(stored_at < asked_at, "{err}");
    assert!(err.ends_with("fiber: reviewer alpha m\n"), "{err}");
    assert_eq!(
        setup.global()["providers"]["alpha"]["reviewer_model"],
        json!("m")
    );
    assert!(!err.contains(KEY));
}

#[test]
fn without_a_terminal_a_key_login_asks_nothing_and_keeps_a_saved_reviewer() {
    let setup = Setup::new();
    setup.install("alpha", None, None);
    setup.write_config(&json!({"providers": {"alpha": {"reviewer_model": "old-m"}}}));
    let (result, err) = setup.login(
        Some("alpha"),
        false,
        &format!("{KEY}\n1\n"),
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(err, "fiber: stored credentials/alpha/default\n");
    assert!(!err.contains("Reviewer"), "{err}");
    assert_eq!(
        setup.global()["providers"]["alpha"]["reviewer_model"],
        json!("old-m"),
        "a pipe login writes no reviewer"
    );
}

#[test]
fn a_declared_secret_never_asks_for_a_reviewer() {
    let setup = Setup::new();
    setup.declare("acme", &["api_key"]);
    let (result, err) = setup.login(Some("api_key"), true, "1\n", &mut Fake::new("v1"));
    result.unwrap();
    assert!(
        err.ends_with("fiber: stored credentials/api_key\n"),
        "{err}"
    );
    assert!(!err.contains("Reviewer"), "{err}");
}

#[test]
fn a_failed_store_never_asks_for_a_reviewer() {
    let setup = Setup::new();
    setup.install("alpha", None, None);
    let (first, _) = setup.login(Some("alpha"), true, "1\n", &mut Fake::new(KEY));
    first.unwrap();
    let (result, err) = setup.login_as(
        Some("alpha"),
        Some("default"),
        true,
        "1\n",
        &mut Fake::new(KEY),
    );
    let failure = failed(result);
    assert!(
        matches!(
            failure,
            CliError::StoreRefused {
                refusal: Refusal::AlreadyStored { .. },
                hint: true,
            }
        ),
        "{failure:?}"
    );
    assert!(!err.contains("Reviewer"), "{err}");
}

#[test]
fn cancellation_during_discovery_keeps_everything_and_asks_nothing() {
    let setup = Setup::new();
    setup.install("alpha", None, None);
    store_credential(
        &setup.home(),
        "alpha",
        "default",
        &Secret::new(KEY.to_owned()),
    )
    .unwrap();
    setup.write_config(&json!({"providers": {"alpha": {"reviewer_model": "old-m"}}}));
    let config_before = fs::read(setup.home().join("config.json")).unwrap();
    let credential_before = fs::read(setup.home().join("credentials/alpha/default")).unwrap();
    // `discovery entered after storage`: the stage armed cancellation
    // before discovery, not at terminal input.
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    let home = setup.home();
    let worker_stopped = Arc::clone(&stopped);
    let (done, finished) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut err = Vec::new();
        let mut reading = Cursor::new(Vec::new());
        let result = reviewer_stage(
            &home,
            "alpha",
            &mut reading,
            &mut err,
            &|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(choices(&["m1"]))
            },
            &worker_stopped,
            false,
            fakes::clock::FakeClock::new(),
            None,
        );
        done.send((result, String::from_utf8(err).unwrap()))
            .unwrap_or(());
    });
    assert!(
        fakes::Deadline::after(STAGE_WAIT).recv(&entered_rx).is_ok(),
        "waited {STAGE_WAIT:?} for discovery entered after storage"
    );
    stopped.store(true, Ordering::SeqCst);
    release_tx.send(()).unwrap();
    let (result, err) = match fakes::Deadline::after(STAGE_WAIT).recv(&finished) {
        Ok(outcome) => outcome,
        Err(_) => panic!("waited {STAGE_WAIT:?} for the cancelled stage to finish"),
    };
    worker.join().expect("the stage worker joined");
    result.unwrap();
    assert_eq!(
        err, "fiber: no reviewer was set for alpha; the credential stays.\n",
        "no prompt, no discovery notice: {err}"
    );
    assert_eq!(
        fs::read(setup.home().join("config.json")).unwrap(),
        config_before,
        "the previous reviewer setting stands"
    );
    assert_eq!(
        fs::read(setup.home().join("credentials/alpha/default")).unwrap(),
        credential_before
    );
}

#[test]
fn cancellation_before_the_claim_writes_nothing() {
    let setup = Setup::new();
    setup.install("alpha", None, None);
    store_credential(
        &setup.home(),
        "alpha",
        "default",
        &Secret::new(KEY.to_owned()),
    )
    .unwrap();
    setup.write_config(&json!({"providers": {"alpha": {"reviewer_model": "old-m"}}}));
    let config_before = fs::read(setup.home().join("config.json")).unwrap();
    let (before_entered_tx, before_entered_rx) = mpsc::channel();
    let (before_release_tx, before_release_rx) = mpsc::channel();
    let (after_entered_tx, after_entered_rx) = mpsc::channel();
    let (_after_release_tx, after_release_rx) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    let home = setup.home();
    let worker_stopped = Arc::clone(&stopped);
    let (done, finished) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut err = Vec::new();
        let mut reading = Cursor::new(b"1\n".to_vec());
        let probe = ClaimProbe {
            before_entered: before_entered_tx,
            before_release: before_release_rx,
            after_entered: after_entered_tx,
            after_release: after_release_rx,
        };
        let result = reviewer_stage(
            &home,
            "alpha",
            &mut reading,
            &mut err,
            &|| Ok(choices(&["m1"])),
            &worker_stopped,
            false,
            fakes::clock::FakeClock::new(),
            Some(&probe),
        );
        done.send((result, String::from_utf8(err).unwrap()))
            .unwrap_or(());
    });
    // `before reviewer save claim`: selection made, cancellation not yet
    // recorded; recording it now must prevent the write entirely.
    assert!(
        fakes::Deadline::after(STAGE_WAIT)
            .recv(&before_entered_rx)
            .is_ok(),
        "waited {STAGE_WAIT:?} for before reviewer save claim"
    );
    stopped.store(true, Ordering::SeqCst);
    before_release_tx.send(()).unwrap();
    let (result, err) = match fakes::Deadline::after(STAGE_WAIT).recv(&finished) {
        Ok(outcome) => outcome,
        Err(_) => panic!("waited {STAGE_WAIT:?} for the cancelled save to finish"),
    };
    worker.join().expect("the stage worker joined");
    result.unwrap();
    assert!(err.contains("no reviewer was set"), "{err}");
    assert!(!err.contains("fiber: reviewer"), "{err}");
    assert!(
        after_entered_rx.try_recv().is_err(),
        "the claim never ran, so no later barrier was reached"
    );
    assert_eq!(
        fs::read(setup.home().join("config.json")).unwrap(),
        config_before,
        "set_reviewer never ran"
    );
}

#[test]
fn a_claimed_save_survives_cancellation_before_the_write() {
    for valid in [true, false] {
        let setup = Setup::new();
        setup.install("alpha", None, None);
        store_credential(
            &setup.home(),
            "alpha",
            "default",
            &Secret::new(KEY.to_owned()),
        )
        .unwrap();
        if valid {
            fs::write(
                setup.home().join("config.json"),
                "{\"providers\": {\"alpha\": {\"credential\": \"default\"}}, \"kept\": {\"a\": 1}}",
            )
            .unwrap();
        } else {
            // Invalid under the held lock: the claimed write still runs
            // and reports its precise-key failure, never a cancellation.
            fs::write(setup.home().join("config.json"), "{bad").unwrap();
        }
        // The global lock held while the claim wins: the save cannot have
        // waited on it yet when cancellation lands.
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(setup.home().join("config.json.lock"))
            .unwrap();
        lock_file.lock().unwrap();
        let (before_entered_tx, before_entered_rx) = mpsc::channel();
        let (before_release_tx, before_release_rx) = mpsc::channel();
        let (after_entered_tx, after_entered_rx) = mpsc::channel();
        let (after_release_tx, after_release_rx) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let home = setup.home();
        let worker_stopped = Arc::clone(&stopped);
        let (done, finished) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut err = Vec::new();
            let mut reading = Cursor::new(b"1\n".to_vec());
            let probe = ClaimProbe {
                before_entered: before_entered_tx,
                before_release: before_release_rx,
                after_entered: after_entered_tx,
                after_release: after_release_rx,
            };
            let result = reviewer_stage(
                &home,
                "alpha",
                &mut reading,
                &mut err,
                &|| Ok(choices(&["m1"])),
                &worker_stopped,
                false,
                fakes::clock::FakeClock::new(),
                Some(&probe),
            );
            done.send((result, String::from_utf8(err).unwrap()))
                .unwrap_or(());
        });
        // The claim runs uncancelled: release it straight through.
        assert!(
            fakes::Deadline::after(STAGE_WAIT)
                .recv(&before_entered_rx)
                .is_ok(),
            "waited {STAGE_WAIT:?} for before reviewer save claim"
        );
        before_release_tx.send(()).unwrap();
        // `reviewer save claimed before writer`: the claim won;
        // cancellation now cannot veto the write.
        assert!(
            fakes::Deadline::after(STAGE_WAIT)
                .recv(&after_entered_rx)
                .is_ok(),
            "waited {STAGE_WAIT:?} for reviewer save claimed before writer"
        );
        stopped.store(true, Ordering::SeqCst);
        drop(lock_file);
        after_release_tx.send(()).unwrap();
        let (result, err) = match fakes::Deadline::after(STAGE_WAIT).recv(&finished) {
            Ok(outcome) => outcome,
            Err(_) => panic!("waited {STAGE_WAIT:?} for the claimed save to finish"),
        };
        worker.join().expect("the stage worker joined");
        if valid {
            result.unwrap();
            assert!(err.contains("fiber: reviewer alpha m1"), "{err}");
            assert!(!err.contains("no reviewer was set"), "{err}");
            assert_eq!(
                setup.global(),
                json!({
                    "providers": {"alpha": {"credential": "default", "reviewer_model": "m1"}},
                    "kept": {"a": 1},
                }),
                "the claimed save and the sibling stand"
            );
        } else {
            let failure = result.unwrap_err();
            assert!(
                matches!(failure, CliError::ReviewerNotSaved { .. }),
                "{failure:?}"
            );
            assert!(
                failure
                    .to_string()
                    .contains("providers.alpha.reviewer_model"),
                "{failure}"
            );
            assert!(!err.contains("fiber: reviewer"), "{err}");
            assert!(!err.contains("no reviewer was set"), "{err}");
            assert_eq!(
                fs::read_to_string(setup.home().join("config.json")).unwrap(),
                "{bad"
            );
        }
    }
}

#[test]
fn dropping_the_sig_guard_restores_sigint_handling() {
    // The guard records SIGINT on the stage latch; dropping it must
    // unregister, so a later SIGINT reaches only the earlier handler. The
    // backup is registered after the guard, so its signal proves the
    // guard's handler already ran for that raise.
    let stopped = Arc::new(AtomicBool::new(false));
    let guard = super::arm_sigint(Arc::clone(&stopped)).unwrap();
    let (fired_tx, fired_rx) = mpsc::channel();
    let mut backup = signal_hook::iterator::Signals::new([signal_hook::consts::SIGINT]).unwrap();
    let backup_close = backup.handle();
    let watcher = thread::spawn(move || {
        for _ in 0..2 {
            if backup.forever().next().is_none() {
                break;
            }
            fired_tx.send(()).unwrap_or(());
        }
    });
    let within = fakes::Deadline::after(Duration::from_secs(10));
    signal_hook::low_level::raise(signal_hook::consts::SIGINT).unwrap();
    within.recv_or_fail(&fired_rx, "the backup handler after the first raise");
    assert!(
        stopped.load(Ordering::SeqCst),
        "the armed guard records SIGINT"
    );
    drop(guard);
    stopped.store(false, Ordering::SeqCst);
    signal_hook::low_level::raise(signal_hook::consts::SIGINT).unwrap();
    within.recv_or_fail(&fired_rx, "the backup handler after the drop");
    assert!(
        !stopped.load(Ordering::SeqCst),
        "a dropped guard records nothing"
    );
    backup_close.close();
    watcher.join().expect("the backup watcher joined");
}

/// Installs the `acme` provider whose model base URL names `placeholders`.
fn install_host(home: &Path, placeholders: Value) {
    let dir = home.join("extensions").join("acme");
    fs::create_dir_all(dir.join("providers")).unwrap();
    let mut manifest = json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = serde_json::Value::Object(
        [("acme".to_owned(), json!(["https://{workspace}/v1"]))]
            .into_iter()
            .collect(),
    );
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
    fs::write(
        dir.join("providers").join("acme.json"),
        json!({
            "name": "acme",
            "placeholders": placeholders,
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1", "context_window": 1000}],
        })
        .to_string(),
    )
    .unwrap();
}

/// Installs the `acme` provider with two gaps: `workspace`, filled from
/// `FIBER_TEST_2909_UNSET_HOST` when set, and `other`, with no variable.
fn install_two_gaps(home: &Path) {
    let dir = home.join("extensions").join("acme");
    fs::create_dir_all(dir.join("providers")).unwrap();
    let mut manifest = json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = serde_json::Value::Object(
        [("acme".to_owned(), json!(["https://{workspace}/v1/{other}"]))]
            .into_iter()
            .collect(),
    );
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
    fs::write(
        dir.join("providers").join("acme.json"),
        json!({
            "name": "acme",
            "placeholders": {"workspace": {"env": "FIBER_TEST_2909_UNSET_HOST"}, "other": {}},
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1/{other}", "context_window": 1000}],
        })
        .to_string(),
    )
    .unwrap();
}

/// The global `config/acme.json` the host answers save.
fn saved_host(setup: &Setup) -> Value {
    serde_json::from_str(
        &fs::read_to_string(setup.home().join("config").join("acme.json")).unwrap(),
    )
    .unwrap()
}

#[test]
fn a_terminal_key_login_asks_for_the_host_after_storing() {
    let setup = Setup::new();
    install_host(&setup.home(), json!({"workspace": {}}));
    let (result, err) = setup.login(
        Some("acme"),
        true,
        "adb-1.example\n1\n",
        &mut Fake::new(KEY),
    );
    result.unwrap();
    let asked = err.find("`workspace`: ").unwrap();
    let stored = err
        .find("fiber: stored credentials/acme/default\n")
        .unwrap();
    assert!(asked < stored, "{err:?}");
    assert_eq!(saved_host(&setup), json!({"workspace": "adb-1.example"}));
    assert!(err.ends_with("fiber: reviewer acme m\n"), "{err:?}");
    assert!(!err.contains(KEY));
}

#[test]
fn a_pasted_url_is_saved_stripped() {
    let setup = Setup::new();
    install_host(&setup.home(), json!({"workspace": {}}));
    let (result, err) = setup.login(
        Some("acme"),
        true,
        "https://adb-1.example/\n1\n",
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(saved_host(&setup), json!({"workspace": "adb-1.example"}));
    assert!(err.ends_with("fiber: reviewer acme m\n"), "{err:?}");
}

#[test]
fn a_non_host_answer_is_refused_and_asked_again() {
    let setup = Setup::new();
    install_host(&setup.home(), json!({"workspace": {}}));
    let (result, err) = setup.login(
        Some("acme"),
        true,
        "not a host!!\nadb-1.example\n1\n",
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(
        err.matches("`workspace` is not a host; press Ctrl-D to skip.")
            .count(),
        1,
        "{err:?}"
    );
    assert_eq!(err.matches("`workspace`: ").count(), 2, "{err:?}");
    assert_eq!(saved_host(&setup), json!({"workspace": "adb-1.example"}));
}

#[test]
fn a_preset_setting_skips_the_host_question() {
    let setup = Setup::new();
    install_host(&setup.home(), json!({"workspace": {}}));
    let dir = setup.home().join("config");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("acme.json"), r#"{"workspace":"adb-1.example"}"#).unwrap();
    let (result, err) = setup.login(Some("acme"), true, "1\n", &mut Fake::new(KEY));
    result.unwrap();
    assert!(!err.contains("`workspace`"), "{err:?}");
    assert!(err.ends_with("fiber: reviewer acme m\n"), "{err:?}");
    assert_eq!(saved_host(&setup), json!({"workspace": "adb-1.example"}));
}

#[test]
fn a_present_but_invalid_setting_asks_nothing() {
    // Presence, not validity, decides: no prompt, and the reviewer still
    // runs against the data file's models.
    let setup = Setup::new();
    install_host(&setup.home(), json!({"workspace": {}}));
    let dir = setup.home().join("config");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("acme.json"), r#"{"workspace":"not a host!!"}"#).unwrap();
    let (result, err) = setup.login(Some("acme"), true, "1\n", &mut Fake::new(KEY));
    result.unwrap();
    assert!(!err.contains("`workspace`"), "{err:?}");
    assert!(err.ends_with("fiber: reviewer acme m\n"), "{err:?}");
}

/// The child's marker: set, the login runs with the child's environment.
const HOST_CHILD: &str = "FIBER_CLI_TEST_HOST_CHILD";

/// The environment variable the child's placeholder reads.
const HOST_ENV: &str = "FIBER_TEST_2909_HOST";

/// Installs the `acme` provider whose `workspace` reads `HOST_ENV`.
fn install_host_env(home: &Path) {
    install_host(home, json!({"workspace": {"env": HOST_ENV}}));
}

#[test]
fn a_preset_environment_value_skips_the_host_question() {
    if std::env::var_os(HOST_CHILD).is_some() {
        let setup = Setup::new();
        install_host_env(&setup.home());
        let (result, err) = setup.login(Some("acme"), true, "1\n", &mut Fake::new(KEY));
        result.unwrap();
        assert!(!err.contains("`workspace`"), "{err:?}");
        assert!(err.ends_with("fiber: reviewer acme m\n"), "{err:?}");
        assert!(!setup.home().join("config").join("acme.json").exists());
        return;
    }
    let name = module_path!().split_once("::").unwrap().1;
    let output = fakes::rerun_prepared(
        &format!("{name}::a_preset_environment_value_skips_the_host_question"),
        &[(HOST_CHILD, "1"), (HOST_ENV, "adb-9.example")],
        fakes::Deadline::after(CHILD_DEADLINE),
        |_| {},
    );
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn a_present_but_invalid_environment_value_asks_nothing() {
    // Presence, not validity, decides: no prompt even when the set value
    // is not a host.
    if std::env::var_os(HOST_CHILD).is_some() {
        let setup = Setup::new();
        install_host_env(&setup.home());
        let (result, err) = setup.login(Some("acme"), true, "1\n", &mut Fake::new(KEY));
        result.unwrap();
        assert!(!err.contains("`workspace`"), "{err:?}");
        assert!(err.ends_with("fiber: reviewer acme m\n"), "{err:?}");
        assert!(!setup.home().join("config").join("acme.json").exists());
        return;
    }
    let name = module_path!().split_once("::").unwrap().1;
    let output = fakes::rerun_prepared(
        &format!("{name}::a_present_but_invalid_environment_value_asks_nothing"),
        &[(HOST_CHILD, "1"), (HOST_ENV, "not a host!!")],
        fakes::Deadline::after(CHILD_DEADLINE),
        |_| {},
    );
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn without_a_terminal_a_key_login_prints_one_remedy_line_per_gap() {
    let setup = Setup::new();
    install_two_gaps(&setup.home());
    let (result, err) = setup.login(
        Some("acme"),
        false,
        &format!("{KEY}\n"),
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(
        err,
        "fiber: `other` has no value and no environment variable fills it; \
         set it in `config/acme.json` in Fiber home.\n\
         fiber: `workspace` has no value; set it in `config/acme.json` in Fiber home, \
         or export `FIBER_TEST_2909_UNSET_HOST`.\n\
         fiber: stored credentials/acme/default\n",
        "{err:?}"
    );
    assert!(!setup.home().join("config").join("acme.json").exists());
}

#[test]
fn without_a_terminal_a_login_with_every_placeholder_present_prints_no_remedy() {
    // Gaps are presence: a present-but-invalid setting still fills the
    // line count with nothing, and today's output is unchanged.
    let setup = Setup::new();
    install_host(
        &setup.home(),
        json!({"workspace": {"env": "FIBER_TEST_2909_UNSET_HOST"}}),
    );
    let dir = setup.home().join("config");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("acme.json"), r#"{"workspace":"not a host!!"}"#).unwrap();
    let (result, err) = setup.login(
        Some("acme"),
        false,
        &format!("{KEY}\n"),
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(err, "fiber: stored credentials/acme/default\n", "{err:?}");
}

#[test]
fn as_label_asks_the_same_but_saves_the_host_globally() {
    let setup = Setup::new();
    install_host(&setup.home(), json!({"workspace": {}}));
    let (result, err) = setup.login_as(
        Some("acme"),
        Some("work"),
        true,
        "adb-1.example\n1\n",
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert!(err.contains("`workspace`: "), "{err:?}");
    assert_eq!(saved_host(&setup), json!({"workspace": "adb-1.example"}));
    assert_eq!(setup.stored("acme", "work").as_deref(), Some(KEY));
    assert!(
        err.contains("fiber: stored credentials/acme/work\n"),
        "{err:?}"
    );
}

#[test]
fn a_project_empty_hides_the_saved_host_and_names_its_file() {
    let setup = Setup::new();
    install_host(&setup.home(), json!({"workspace": {}}));
    let (_, project) = crate::project_of(&setup.home(), &std::env::current_dir().unwrap()).unwrap();
    let dir = setup
        .home()
        .join("projects")
        .join(project.as_str())
        .join("config");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("acme.json"), r#"{"workspace":""}"#).unwrap();
    let (result, err) = setup.login(
        Some("acme"),
        true,
        "adb-1.example\n1\n",
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(saved_host(&setup), json!({"workspace": "adb-1.example"}));
    let file = dir.join("acme.json");
    assert!(
        err.contains(&format!(
            "fiber: saved `workspace` globally, but {} sets it empty and hides it.",
            file.display()
        )),
        "{err:?}"
    );
    assert!(err.ends_with("fiber: reviewer acme m\n"), "{err:?}");
}

#[test]
fn a_broken_project_config_fails_the_login_instead_of_reporting_success() {
    // The store never reads the project file, so the key lands before the
    // host step loads it: the broken file must fail the login, never read
    // as no gaps with a success report.
    let setup = Setup::new();
    install_host(&setup.home(), json!({"workspace": {}}));
    let (_, project) = crate::project_of(&setup.home(), &std::env::current_dir().unwrap()).unwrap();
    let dir = setup.home().join("projects").join(project.as_str());
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.json"), "{bad").unwrap();
    let (result, err) = setup.login(Some("acme"), true, "1\n", &mut Fake::new(KEY));
    let error = failed(result);
    assert!(matches!(error, CliError::Config(_)), "{error:?}");
    assert!(!err.contains("fiber: stored"), "{err:?}");
}

/// Installs the `acme` provider whose host placeholder is the dotted
/// setting `parent.child`: the model URL needs no placeholder, so the
/// reviewer still lists it once the gap is answered.
fn install_dotted_host(home: &Path) {
    let dir = home.join("extensions").join("acme");
    fs::create_dir_all(dir.join("providers")).unwrap();
    let mut manifest = json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = serde_json::Value::Object(
        [("acme".to_owned(), json!(["https://fixed.example/v1"]))]
            .into_iter()
            .collect(),
    );
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
    fs::write(
        dir.join("providers").join("acme.json"),
        json!({
            "name": "acme",
            "placeholders": {"parent.child": {}},
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://fixed.example/v1", "context_window": 1000}],
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn a_nested_project_empty_hides_the_saved_host_and_names_its_file() {
    // The mask reads the dotted setting through the parsed path, so a
    // nested empty project value still names its file after the save.
    let setup = Setup::new();
    install_dotted_host(&setup.home());
    let (_, project) = crate::project_of(&setup.home(), &std::env::current_dir().unwrap()).unwrap();
    let dir = setup
        .home()
        .join("projects")
        .join(project.as_str())
        .join("config");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("acme.json"), r#"{"parent":{"child":""}}"#).unwrap();
    let (result, err) = setup.login(
        Some("acme"),
        true,
        "adb-1.example\n1\n",
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(
        saved_host(&setup),
        json!({"parent": {"child": "adb-1.example"}})
    );
    let file = dir.join("acme.json");
    assert!(
        err.contains(&format!(
            "fiber: saved `parent.child` globally, but {} sets it empty and hides it.",
            file.display()
        )),
        "{err:?}"
    );
    assert!(err.ends_with("fiber: reviewer acme m\n"), "{err:?}");
}

#[test]
fn end_of_input_at_the_host_prompt_saves_nothing_and_still_ends_zero() {
    // EOF stops the questions: an empty typed line would ask again, but
    // 0 bytes end the host step with nothing saved for any gap.
    let setup = Setup::new();
    install_host(&setup.home(), json!({"workspace": {}}));
    let (result, err) = setup.login(Some("acme"), true, "", &mut Fake::new(KEY));
    result.unwrap();
    assert_eq!(err.matches("`workspace`: ").count(), 1, "{err:?}");
    assert!(!err.contains("is not a host"), "{err:?}");
    assert!(!setup.home().join("config").join("acme.json").exists());
    assert!(
        err.contains("fiber: stored credentials/acme/default\n"),
        "{err:?}"
    );
    assert!(
        err.contains("fiber: no reviewer was set for acme; the credential stays.\n"),
        "{err:?}"
    );
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
}

#[test]
fn every_gap_is_asked_before_the_reviewer_runs() {
    // Two gaps, both answered: with `is_ended` stuck true the loop
    // would stop after the first answer, so the second prompt and its
    // save pin the loop continuing on a saved answer.
    let setup = Setup::new();
    install_two_gaps(&setup.home());
    let (result, err) = setup.login(
        Some("acme"),
        true,
        "adb-1.example\nadb-2.example\n1\n",
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(err.matches("`other`: ").count(), 1, "{err:?}");
    assert_eq!(
        err.matches("`workspace` (`FIBER_TEST_2909_UNSET_HOST`): ")
            .count(),
        1,
        "{err:?}"
    );
    assert_eq!(
        saved_host(&setup),
        json!({"other": "adb-1.example", "workspace": "adb-2.example"})
    );
    assert!(err.ends_with("fiber: reviewer acme m\n"), "{err:?}");
}

#[test]
fn end_of_input_stops_before_the_next_gap() {
    // EOF at the first of two gaps ends the questions at once: with
    // `is_ended` stuck false the loop would ask on, so the missing
    // second prompt pins the loop ending on end of input.
    let setup = Setup::new();
    install_two_gaps(&setup.home());
    let (result, err) = setup.login(Some("acme"), true, "", &mut Fake::new(KEY));
    result.unwrap();
    assert_eq!(err.matches("`other`: ").count(), 1, "{err:?}");
    assert!(!err.contains("`workspace`"), "{err:?}");
    assert!(!setup.home().join("config").join("acme.json").exists());
    assert!(
        err.contains("fiber: no reviewer was set for acme; the credential stays.\n"),
        "{err:?}"
    );
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
}

#[test]
fn a_pipe_login_runs_remedies_for_a_provider_but_no_host_step_for_a_secret() {
    // `!terminal && is_some`: the conjunction holds for a provider, so
    // the pipe prints its remedy lines before the stored line; it fails
    // for a secret, so the stored line follows at once with no host
    // step to fail: a broken project config still stores the secret,
    // while `||` would route it through the failing host step.
    let provider = Setup::new();
    install_host(&provider.home(), json!({"workspace": {}}));
    let (result, err) = provider.login(
        Some("acme"),
        false,
        &format!("{KEY}\n"),
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(
        err,
        "fiber: `workspace` has no value and no environment variable fills it; \
         set it in `config/acme.json` in Fiber home.\n\
         fiber: stored credentials/acme/default\n",
        "{err:?}"
    );
    let secret = Setup::new();
    secret.declare("acme", &["api_key"]);
    let (_, project) =
        crate::project_of(&secret.home(), &std::env::current_dir().unwrap()).unwrap();
    let dir = secret.home().join("projects").join(project.as_str());
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.json"), "{bad").unwrap();
    let (result, err) = secret.login(Some("api_key"), false, "", &mut Fake::new("v1"));
    result.unwrap();
    assert_eq!(err, "fiber: stored credentials/api_key\n", "{err:?}");
}

impl Setup {
    /// The installed providers merged with `workspace`'s approved
    /// repository set, as `fiber login` reads them there.
    fn workspace_providers(&self, workspace: &Path) -> Providers {
        let clock = fakes::clock::FakeClock::new();
        let (_, project) = crate::project_of(&self.home(), workspace).unwrap();
        let set = extensions::RepositorySet::load_pinned(&self.home(), &project, workspace, clock);
        let (providers, notices) = Providers::load_all_repository(&self.home(), &set).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        providers
    }
}

#[test]
fn login_targets_finds_a_repository_provider_inside_the_project_but_not_outside() {
    let setup = Setup::new();
    let manifest = repo_manifest("fiber.test/acme", &json!({"acme": ["http://x/v1"]}));
    let data = repo_data("acme", "http://x/v1", &["m"]);
    declare_repo(
        &setup.root.path().join("workspace"),
        &[("repo", manifest, &[("providers/acme.json", data.as_str())])],
    );
    approve_repo(
        &setup.home(),
        &setup.root.path().join("workspace"),
        "fiber.test/acme",
    );
    let workspace = setup.root.path().join("workspace");
    let inside = targets(&setup.workspace_providers(&workspace));
    assert!(
        inside.contains(&LoginName::Provider("acme".to_owned())),
        "{inside:?}"
    );
    // Outside the project the approved provider is no login target, and
    // the home-only helper never lists it either.
    let elsewhere = setup.root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let outside = targets(&setup.workspace_providers(&elsewhere));
    assert!(
        !outside.contains(&LoginName::Provider("acme".to_owned())),
        "{outside:?}"
    );
    assert!(
        !login_targets(&setup.home())
            .unwrap()
            .contains(&LoginName::Provider("acme".to_owned()))
    );
}

#[test]
fn login_store_stores_a_key_for_a_repository_provider() {
    let setup = Setup::new();
    let manifest = repo_manifest("fiber.test/acme", &json!({"acme": ["http://x/v1"]}));
    let data = repo_data("acme", "http://x/v1", &["m"]);
    declare_repo(
        &setup.root.path().join("workspace"),
        &[("repo", manifest, &[("providers/acme.json", data.as_str())])],
    );
    approve_repo(
        &setup.home(),
        &setup.root.path().join("workspace"),
        "fiber.test/acme",
    );
    let workspace = setup.root.path().join("workspace");
    let providers = setup.workspace_providers(&workspace);
    let stored = store(&setup.home(), &providers, "acme", None, |_| {
        Ok(Secret::new(KEY.to_owned()))
    })
    .unwrap();
    assert_eq!(stored.path, "credentials/acme/default");
    assert!(!stored.replaced);
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
}

#[test]
fn an_empty_typed_line_is_refused_and_asked_again() {
    // An empty line is a typed answer, not EOF: it prints the problem
    // line and asks again, and only EOF ends the questions.
    let setup = Setup::new();
    install_host(&setup.home(), json!({"workspace": {}}));
    let (result, err) = setup.login(
        Some("acme"),
        true,
        "\nadb-1.example\n1\n",
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(
        err.matches("`workspace` is not a host; press Ctrl-D to skip.")
            .count(),
        1,
        "{err:?}"
    );
    assert_eq!(saved_host(&setup), json!({"workspace": "adb-1.example"}));
}

#[test]
fn a_project_empty_over_a_global_answer_reads_as_no_value() {
    // The mask the login names, through the existing settings path: the
    // global answer stays written, but the project layer's empty string
    // wins the merge, so the model list still reports no value.
    let setup = Setup::new();
    install_host(
        &setup.home(),
        json!({"workspace": {"env": "FIBER_TEST_2909_UNSET_HOST"}}),
    );
    let mut config = setup.config();
    config
        .extensions_mut()
        .set(
            "acme",
            config::Scope::Machine,
            "workspace",
            Value::String("adb-1.example".to_owned()),
        )
        .unwrap();
    let dir = setup.home().join("projects").join("p").join("config");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("acme.json"), r#"{"workspace":""}"#).unwrap();
    let config = setup.config();
    let mut providers = setup.providers();
    let env = |_: &str| None;
    let notices = providers.fill_placeholders(&config, &env).unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].message,
        "The model `acme/m` needs the setting `workspace` for its base URL, \
         which has no value, and `FIBER_TEST_2909_UNSET_HOST` is not set in the hub's \
         environment (your login shell). Run `fiber login acme`, \
         or set `workspace` in `config/acme.json` in Fiber home, \
         or export it in your shell profile and run `fiber hub refresh`."
    );
}

/// Installs the synthetic browser provider `acme`: `login: browser`
/// with a local `credential()` returning a test token, and one data
/// model whose base URL names the `workspace` placeholder. The flow opens
/// nothing and contacts nothing.
fn install_browser_host(home: &Path) {
    let dir = home.join("extensions").join("acme");
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({ "name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
                "providers": {"acme": ["https://{workspace}/v1"]} })
        .to_string(),
    )
    .unwrap();
    write_record(&dir);
    fs::write(
        dir.join("providers").join("acme.json"),
        json!({
            "name": "acme",
            "login": "browser",
            "placeholders": {"workspace": {}},
            "models": [{
                "id": "m", "protocol": "openai-responses",
                "base_url": "https://{workspace}/v1", "context_window": 1000
            }]
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers").join("acme.lua"),
        r#"fiber.provider("acme", {
          credential = { timeout = 60000, run = function()
            local stored = host.oauth.refresh(function()
              return { token = "test-token", expires_at = 4102444800 }
            end)
            return { token = stored.token, expires_at = stored.expires_at, email = "alice@example.com" }
          end }
        })"#,
    )
    .unwrap();
}

#[test]
fn a_terminal_browser_login_asks_for_the_host_after_storing() {
    let setup = Setup::new();
    install_browser_host(&setup.home());
    let home = setup.home();
    let providers = setup.providers();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut stdin = Cursor::new("adb-1.example\n1\n");
        let mut keys = Plain;
        let result = login(
            Some("acme"),
            None,
            &mut LoginIo {
                home: &home,
                providers: &providers,
                terminal: true,
                stdin: &mut stdin,
                err: &mut output,
                keys: &mut keys,
                device: false,
                clock: fakes::clock::FakeClock::new(),
            },
        );
        match done.send((result, String::from_utf8(output).unwrap())) {
            Ok(()) | Err(_) => {}
        }
    });
    let (result, err) = fakes::Deadline::after(STAGE_WAIT)
        .recv(&finished)
        .unwrap_or_else(|_| panic!("browser login did not finish within {STAGE_WAIT:?}"));
    result.unwrap();
    let asked = err.find("`workspace`: ").unwrap();
    let stored = err
        .find("fiber: stored credentials/acme/alice@example.com\n")
        .unwrap();
    assert!(asked < stored, "{err:?}");
    assert_eq!(saved_host(&setup), json!({"workspace": "adb-1.example"}));
    assert!(err.ends_with("fiber: reviewer acme m\n"), "{err:?}");
}

#[test]
fn unapproved_and_never_repository_names_never_log_in() {
    let setup = Setup::new();
    let ghost = repo_manifest("fiber.test/ghost", &json!({"ghost": ["http://x/v1"]}));
    let ghost_data = repo_data("ghost", "http://x/v1", &["m"]);
    let sketchy = repo_manifest("fiber.test/sketchy", &json!({"sketchy": ["http://x/v1"]}));
    let sketchy_data = repo_data("sketchy", "http://x/v1", &["m"]);
    declare_repo(
        &setup.root.path().join("workspace"),
        &[
            (
                "ghost",
                ghost,
                &[("providers/ghost.json", ghost_data.as_str())],
            ),
            (
                "sketchy",
                sketchy,
                &[("providers/sketchy.json", sketchy_data.as_str())],
            ),
        ],
    );
    never_repo(
        &setup.home(),
        &setup.root.path().join("workspace"),
        "fiber.test/ghost",
    );
    let workspace = setup.root.path().join("workspace");
    let providers = setup.workspace_providers(&workspace);
    for name in ["ghost", "sketchy"] {
        let names = targets(&providers);
        assert!(
            !names.contains(&LoginName::Provider(name.to_owned())),
            "{name}: {names:?}"
        );
        let Err(error) = store(&setup.home(), &providers, name, None, |_| {
            Ok(Secret::new(KEY.to_owned()))
        }) else {
            panic!("{name} logs in without approval");
        };
        assert!(matches!(error, CliError::StoreRefused { .. }), "{error:?}");
    }
}
