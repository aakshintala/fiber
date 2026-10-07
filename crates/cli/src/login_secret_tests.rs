//! `fiber login <name>` for a secret an installed extension declares
//! (`docs/configuration.md`, "Secrets"; `docs/invocation.md`, "Fiber
//! itself"), through the helpers of the parent test module.

use std::process::ExitStatus;

use super::*;

const NAME: &str = "acme.api_key";

/// The secret's file in Fiber home.
fn file(setup: &Setup, name: &str) -> PathBuf {
    setup.home().join("credentials").join(name)
}

#[test]
fn a_declared_secret_is_stored_trimmed_in_a_0600_file_and_named_without_its_value() {
    let setup = Setup::new();
    setup.declare("acme", &[NAME]);
    let (result, err) = setup.login(Some(NAME), false, "", &mut Fake::new("  v1  \n"));
    result.unwrap();
    let file = file(&setup, NAME);
    assert_eq!(fs::read_to_string(&file).unwrap(), "v1");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&setup.home().join("credentials")), 0o700);
    assert_eq!(err, "fiber: stored credentials/acme.api_key\n");
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn a_secret_already_stored_is_replaced_and_the_result_says_so() {
    let setup = Setup::new();
    setup.declare("acme", &[NAME]);
    setup
        .login(Some(NAME), false, "", &mut Fake::new("v1"))
        .0
        .unwrap();
    let (result, err) = setup.login(Some(NAME), false, "", &mut Fake::new("v2"));
    result.unwrap();
    assert_eq!(fs::read_to_string(file(&setup, NAME)).unwrap(), "v2");
    assert_eq!(err, "fiber: replaced credentials/acme.api_key\n");
}

#[test]
fn an_empty_value_is_a_usage_error_and_stores_nothing() {
    let setup = Setup::new();
    setup.declare("acme", &[NAME]);
    let (result, err) = setup.login(Some(NAME), false, "", &mut Fake::new("  \n"));
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::Usage);
    assert_eq!(
        e.message,
        "No value was given; nothing was stored. Run `fiber --help` for usage."
    );
    assert_eq!(err, "");
    assert!(!file(&setup, NAME).exists());
}

#[test]
fn as_with_a_declared_secret_is_a_usage_error_that_reads_nothing() {
    let setup = Setup::new();
    setup.declare("acme", &[NAME]);
    let mut keys = Fake::new(KEY);
    let (result, err) = setup.login_as(Some(NAME), Some("work"), false, "", &mut keys);
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::Usage);
    assert_eq!(
        e.message,
        "--as applies only to a provider, and `acme.api_key` is a declared secret. Run `fiber --help` for usage."
    );
    assert_eq!(err, "");
    assert_eq!(keys.asked, 0);
    assert!(!setup.home().join("credentials").exists());
}

#[test]
fn an_unknown_name_is_a_usage_error_listing_the_providers_and_the_secrets() {
    let unknown = |setup: &Setup| {
        let mut keys = Fake::new(KEY);
        let (result, _) = setup.login(Some("acme.api_kye"), false, "", &mut keys);
        let e = failed(result);
        assert_eq!(e.code, ErrorCode::Usage);
        assert_eq!(keys.asked, 0);
        assert!(!setup.home().join("credentials").exists());
        e.message
    };
    let prefix = "`acme.api_kye` is neither an installed provider nor a declared secret; ";
    let suffix = ". Run `fiber --help` for usage.";
    let setup = Setup::new();
    assert_eq!(
        unknown(&setup),
        format!(
            "{prefix}no provider is installed, and no installed extension declares a secret{suffix}"
        )
    );
    setup.install("acme", None, None);
    assert_eq!(
        unknown(&setup),
        format!(
            "{prefix}the installed providers are acme, and no installed extension declares a secret{suffix}"
        )
    );
    setup.declare("acme-secrets", &[NAME, "acme.url"]);
    assert_eq!(
        unknown(&setup),
        format!(
            "{prefix}the installed providers are acme, and the declared secrets are acme.api_key, acme.url{suffix}"
        )
    );
    let setup = Setup::new();
    setup.declare("acme-secrets", &[NAME]);
    assert_eq!(
        unknown(&setup),
        format!(
            "{prefix}no provider is installed, and the declared secrets are acme.api_key{suffix}"
        )
    );
}

#[test]
fn a_name_that_is_both_a_provider_and_a_secret_logs_in_to_the_provider() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    setup.declare("acme-secrets", &["acme"]);
    let (result, err) = setup.login(Some("acme"), false, "", &mut Fake::new(KEY));
    result.unwrap();
    assert_eq!(err, "fiber: stored credentials/acme/default\n");
    assert!(file(&setup, "acme").is_dir());
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
    // Neither the menu nor the unknown-name message lists it as a secret.
    let (result, err) = setup.login(None, true, "\n", &mut Fake::new(KEY));
    assert_eq!(failed(result).code, ErrorCode::Usage);
    assert_eq!(
        err,
        "Providers:\n  1) acme\nProvider or secret, by number or name: "
    );
    let (result, _) = setup.login(Some("nope"), false, "", &mut Fake::new(KEY));
    assert!(
        failed(result)
            .message
            .contains("no installed extension declares a secret"),
    );
}

fn secret_menu_setup() -> Setup {
    let setup = menu_setup();
    setup.declare("acme", &[NAME]);
    setup
}

#[test]
fn the_menu_offers_the_secrets_after_the_providers_and_stores_a_pick() {
    for typed in ["3\n", "acme.api_key\n"] {
        let setup = secret_menu_setup();
        let (result, err) = setup.login(None, true, typed, &mut Fake::new("v1"));
        result.unwrap();
        assert_eq!(
            err,
            "Providers:\n  1) alpha\n  2) beta\nSecrets:\n  3) acme.api_key\n\
             Provider or secret, by number or name: Value for acme.api_key: \
             fiber: stored credentials/acme.api_key\n",
            "{typed:?}"
        );
        assert_eq!(fs::read_to_string(file(&setup, NAME)).unwrap(), "v1");
        assert!(!setup.home().join("credentials/alpha").exists());
        assert!(!setup.home().join("credentials/beta").exists());
    }
    // One past the last secret is no pick.
    let setup = secret_menu_setup();
    let (result, _) = setup.login(None, true, "4\n", &mut Fake::new("v1"));
    assert!(failed(result).message.contains("is neither a listed"));
}

#[test]
fn the_menu_with_secrets_alone_offers_them_from_one() {
    let setup = Setup::new();
    setup.declare("acme", &[NAME]);
    let (result, err) = setup.login(None, true, "1\n", &mut Fake::new("v1"));
    result.unwrap();
    assert!(
        err.starts_with("Secrets:\n  1) acme.api_key\nProvider or secret, by number or name: "),
        "{err}"
    );
    assert_eq!(fs::read_to_string(file(&setup, NAME)).unwrap(), "v1");
}

#[test]
fn a_menu_pick_of_a_secret_with_as_is_a_usage_error_that_reads_nothing() {
    let setup = secret_menu_setup();
    let mut keys = Fake::new(KEY);
    let (result, _) = setup.login_as(None, Some("work"), true, "3\n", &mut keys);
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::Usage);
    assert!(
        e.message.starts_with(
            "--as applies only to a provider, and `acme.api_key` is a declared secret."
        ),
        "{}",
        e.message
    );
    assert_eq!(keys.asked, 0);
    assert!(!setup.home().join("credentials").exists());
    // A provider picked with `--as` stores under the label.
    let (result, _) = setup.login_as(None, Some("work"), true, "1\n", &mut Fake::new(KEY));
    result.unwrap();
    assert_eq!(setup.stored("alpha", "work").as_deref(), Some(KEY));
}

#[test]
fn a_declared_secret_over_a_providers_directory_fails_and_leaves_it() {
    let setup = Setup::new();
    setup.declare("acme", &["opencode"]);
    store_credential(
        &setup.home(),
        "opencode",
        "default",
        &Secret::new(KEY.into()),
    )
    .unwrap();
    let mut keys = Fake::new("other");
    let (result, err) = setup.login(Some("opencode"), false, "", &mut keys);
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::ConfigInvalid, "{}", e.message);
    assert!(!e.message.contains("other"));
    assert_eq!(err, "");
    assert_eq!(keys.asked, 0);
    assert_eq!(setup.stored("opencode", "default").as_deref(), Some(KEY));
}

/// Runs the test `test` of this module in a child with `FIBER_HOME` set to
/// `home` and `stdin` on a pipe, and returns how it exited. The child is
/// killed, and the test fails, at `CHILD_DEADLINE`.
fn child(test: &str, home: &Path, stdin: &str) -> ExitStatus {
    let module = module_path!().split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{module}::{test}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", home)
        .env(CHILD, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // A child that refuses before it reads may close the pipe first.
    match child.stdin.take().unwrap().write_all(stdin.as_bytes()) {
        Ok(()) | Err(_) => {}
    }
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait().unwrap()));
    let Ok(status) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for `fiber login` in {test} to exit");
    };
    status
}

#[test]
fn run_login_stores_a_declared_secret_from_a_pipe() {
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(run_login(Some(NAME), None));
    }
    let setup = Setup::new();
    setup.declare("acme", &[NAME]);
    let status = child(
        "run_login_stores_a_declared_secret_from_a_pipe",
        &setup.home(),
        "v1\n",
    );
    assert_eq!(status.code(), Some(0), "{status}");
    let file = file(&setup, NAME);
    assert_eq!(fs::read_to_string(&file).unwrap(), "v1");
    assert_eq!(mode(&file), 0o600);
}

#[test]
fn run_login_of_a_mistyped_secret_is_a_usage_failure() {
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(run_login(Some("acme.api_kye"), None));
    }
    let setup = Setup::new();
    setup.declare("acme", &[NAME]);
    let status = child(
        "run_login_of_a_mistyped_secret_is_a_usage_failure",
        &setup.home(),
        "v1\n",
    );
    assert_eq!(status.code(), Some(2), "{status}");
    assert!(!setup.home().join("credentials").exists());
}

#[test]
fn run_login_of_a_secret_with_as_is_a_usage_failure() {
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(run_login(Some(NAME), Some("work")));
    }
    let setup = Setup::new();
    setup.declare("acme", &[NAME]);
    let status = child(
        "run_login_of_a_secret_with_as_is_a_usage_failure",
        &setup.home(),
        "v1\n",
    );
    assert_eq!(status.code(), Some(2), "{status}");
    assert!(!setup.home().join("credentials").exists());
}
