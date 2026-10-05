//! `fiber login --as` and `fiber logout --as | --all` (`docs/model-routing.md`,
//! "Logging in"; `docs/configuration.md`, "Secrets"), through the helpers of
//! the parent test module.

use super::super::chosen_label;
use super::*;

fn store(setup: &Setup, name: &str, label: &str) {
    store_credential(&setup.home(), name, label, &Secret::new(KEY.into())).unwrap();
}

#[test]
fn the_label_is_the_flag_then_the_email_then_default() {
    assert_eq!(chosen_label(Some("work"), Some("a@b.c")), "work");
    assert_eq!(chosen_label(Some("work"), None), "work");
    assert_eq!(chosen_label(None, Some("a@b.c")), "a@b.c");
    assert_eq!(chosen_label(None, None), "default");
}

#[test]
fn as_stores_under_the_label_and_names_it_in_the_global_file_once() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    let (result, err) = setup.login_as(Some("acme"), Some("work"), false, "", &mut Fake::new(KEY));
    result.unwrap();
    let file = setup.home().join("credentials/acme/work");
    assert_eq!(fs::read_to_string(&file).unwrap(), KEY);
    assert_eq!(mode(&file), 0o600);
    assert_eq!(err, "fiber: stored credentials/acme/work\n");
    assert!(!err.contains(KEY));
    assert!(!setup.home().join("credentials/acme/default").exists());
    assert_eq!(
        setup.global(),
        json!({"providers": {"acme": {"credential": "work"}}})
    );
    let (result, err) = setup.login_as(
        Some("acme"),
        Some("personal"),
        false,
        "",
        &mut Fake::new("other"),
    );
    result.unwrap();
    assert_eq!(err, "fiber: stored credentials/acme/personal\n");
    assert_eq!(setup.stored("acme", "personal").as_deref(), Some("other"));
    assert_eq!(setup.stored("acme", "work").as_deref(), Some(KEY));
    assert_eq!(
        setup.global(),
        json!({"providers": {"acme": {"credential": "work"}}})
    );
}

#[test]
fn without_as_a_key_login_stores_default() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    let (result, err) = setup.login(Some("acme"), false, "", &mut Fake::new(KEY));
    result.unwrap();
    assert_eq!(err, "fiber: stored credentials/acme/default\n");
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
    assert_eq!(
        setup.global(),
        json!({"providers": {"acme": {"credential": "default"}}})
    );
}

#[test]
fn as_on_a_shared_credential_provider_stores_under_the_shared_directory() {
    let setup = Setup::new();
    setup.install("opencode-go", Some("opencode"), None);
    let (result, err) = setup.login_as(
        Some("opencode-go"),
        Some("work"),
        false,
        "",
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(err, "fiber: stored credentials/opencode/work\n");
    assert_eq!(setup.stored("opencode", "work").as_deref(), Some(KEY));
    assert!(!setup.home().join("credentials/opencode-go").exists());
}

#[test]
fn an_already_stored_label_is_refused_naming_as_and_the_key_is_never_asked() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    store(&setup, "acme", "work");
    let mut keys = Fake::new("new-key");
    let (result, err) = setup.login_as(Some("acme"), Some("work"), false, "", &mut keys);
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::Usage);
    assert_eq!(
        e.message,
        "credentials/acme/work is already stored; log in under another label with --as <label>, or run `fiber logout acme --as work` first. Run `fiber --help` for usage."
    );
    assert_eq!(err, "");
    assert_eq!(keys.asked, 0);
    assert_eq!(setup.stored("acme", "work").as_deref(), Some(KEY));
    assert!(!setup.home().join("config.json").exists());
    // Another label is free.
    let (result, _) = setup.login_as(Some("acme"), Some("home"), false, "", &mut keys);
    result.unwrap();
}

#[test]
fn an_invalid_label_fails_before_the_key_is_asked_and_writes_nothing() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    for label in ["", "a/b", "..", ".", "x.lock", "x.tmp"] {
        let mut keys = Fake::new(KEY);
        let (result, err) = setup.login_as(Some("acme"), Some(label), false, "", &mut keys);
        let e = failed(result);
        assert_eq!(e.code, ErrorCode::InvalidArguments, "{label:?}");
        assert!(!e.message.contains(KEY), "{}", e.message);
        assert_eq!(err, "", "{label:?}");
        assert_eq!(keys.asked, 0, "{label:?}");
    }
    assert!(!setup.home().join("credentials").exists());
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn an_email_is_a_valid_label() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    let (result, err) = setup.login_as(
        Some("acme"),
        Some("alice@example.com"),
        false,
        "",
        &mut Fake::new(KEY),
    );
    result.unwrap();
    assert_eq!(err, "fiber: stored credentials/acme/alice@example.com\n");
    assert_eq!(
        setup.stored("acme", "alice@example.com").as_deref(),
        Some(KEY)
    );
}

#[test]
fn a_failed_global_write_removes_the_chosen_label_not_default() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    store(&setup, "acme", "default");
    // A config.json that is a directory makes the global write fail.
    fs::create_dir(setup.home().join("config.json")).unwrap();
    let (result, err) = setup.login_as(Some("acme"), Some("work"), false, "", &mut Fake::new(KEY));
    let e = failed(result);
    assert!(!e.message.contains(KEY), "{}", e.message);
    assert_eq!(err, "");
    assert_eq!(setup.stored("acme", "work"), None);
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
}

#[test]
fn logout_as_deletes_only_that_label_and_names_it() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    store(&setup, "acme", "work");
    store(&setup, "acme", "default");
    let (result, err) = setup.logout_target(Some("acme"), LogoutTarget::Label("work"));
    result.unwrap();
    assert_eq!(err, "fiber: removed credentials/acme/work\n");
    assert_eq!(setup.stored("acme", "work"), None);
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
}

#[test]
fn logout_as_a_missing_label_names_the_stored_ones_and_deletes_nothing() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    store(&setup, "acme", "work");
    store(&setup, "acme", "default");
    let (result, err) = setup.logout_target(Some("acme"), LogoutTarget::Label("missing"));
    let e = failed(result);
    assert_eq!(e.code, ErrorCode::CredentialMissing);
    assert_eq!(
        e.message,
        "no stored credential missing for acme; the stored labels are default, work"
    );
    assert_eq!(err, "");
    assert_eq!(setup.stored("acme", "work").as_deref(), Some(KEY));
    assert_eq!(setup.stored("acme", "default").as_deref(), Some(KEY));
}

#[test]
fn logout_with_a_flag_and_nothing_stored_is_the_empty_case_failure() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    for target in [LogoutTarget::Label("missing"), LogoutTarget::All] {
        let (result, err) = setup.logout_target(Some("acme"), target);
        let e = failed(result);
        assert_eq!(e.code, ErrorCode::CredentialMissing);
        assert_eq!(e.message, "no stored credential for acme");
        assert_eq!(err, "");
    }
}

#[test]
fn logout_all_deletes_every_label_in_sorted_order_one_line_each() {
    let setup = Setup::new();
    setup.install("opencode-go", Some("opencode"), None);
    setup.install("opencode-zen", Some("opencode"), None);
    for label in ["work", "default", "home"] {
        store(&setup, "opencode", label);
    }
    let (result, err) = setup.logout_target(Some("opencode-go"), LogoutTarget::All);
    result.unwrap();
    assert_eq!(
        err,
        "fiber: removed credentials/opencode/default, which opencode-zen also reads\n\
         fiber: removed credentials/opencode/home, which opencode-zen also reads\n\
         fiber: removed credentials/opencode/work, which opencode-zen also reads\n"
    );
    for label in ["work", "default", "home"] {
        assert_eq!(setup.stored("opencode", label), None, "{label}");
    }
}

#[test]
fn logout_without_a_flag_deletes_the_one_label_whatever_it_is() {
    let setup = Setup::new();
    setup.install("acme", None, None);
    store(&setup, "acme", "work");
    let (result, err) = setup.logout(Some("acme"));
    result.unwrap();
    assert_eq!(err, "fiber: removed credentials/acme/work\n");
    assert_eq!(setup.stored("acme", "work"), None);
}
