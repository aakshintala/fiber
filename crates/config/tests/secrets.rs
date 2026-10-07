//! `docs/configuration.md`, "Secrets": a secret is `credentials/<name>` in
//! Fiber home, mode 0600 in a 0700 directory, and its value never reaches
//! configuration output, `Debug`, an error or a notice.

mod common;

use std::fs;

use common::{Setup, mode};
use config::{
    CredentialFile, CredentialSource, Secret, credential_labels, delete_credential,
    delete_credential_held, read_credential, read_secret, store_credential, store_secret,
};
use contract::ErrorCode;

const VALUE: &str = "sk-live-7f3a9c0d1e2b";

#[test]
fn a_stored_secret_is_a_0600_file_in_a_0700_credentials_directory() {
    let setup = Setup::new();
    store_secret(&setup.home(), "acme.api_key", &Secret::new(VALUE.into())).unwrap();
    let file = setup.home().join("credentials/acme.api_key");
    assert_eq!(fs::read_to_string(&file).unwrap(), VALUE);
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&setup.home().join("credentials")), 0o700);
    let read = read_secret(&setup.home(), "acme.api_key").unwrap().unwrap();
    assert_eq!(read.expose(), VALUE);
    assert_eq!(read, Secret::new(VALUE.into()));
}

#[test]
fn storing_a_secret_again_replaces_it() {
    let setup = Setup::new();
    store_secret(&setup.home(), "openrouter", &Secret::new("old".into())).unwrap();
    store_secret(&setup.home(), "openrouter", &Secret::new(VALUE.into())).unwrap();
    assert_eq!(
        read_secret(&setup.home(), "openrouter")
            .unwrap()
            .unwrap()
            .expose(),
        VALUE
    );
    assert_eq!(
        fs::read_dir(setup.home().join("credentials"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn a_missing_secret_is_none() {
    let setup = Setup::new();
    assert_eq!(read_secret(&setup.home(), "nothing").unwrap(), None);
}

#[test]
fn a_secret_that_cannot_be_read_is_an_error_without_its_value() {
    let setup = Setup::new();
    fs::create_dir_all(setup.home().join("credentials/dir")).unwrap();
    let e = read_secret(&setup.home(), "dir").unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
}

#[test]
fn a_name_that_is_not_one_file_name_is_refused() {
    let setup = Setup::new();
    for name in ["", ".", "..", "../config.json", "a/b", "a\0b"] {
        let e = read_secret(&setup.home(), name).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArguments, "{name:?}");
        assert_eq!(
            e.to_string(),
            format!("`{name}` is not a secret's name: it must be one file name in credentials/.")
        );
        let e = store_secret(&setup.home(), name, &Secret::new(VALUE.into())).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArguments, "{name:?}");
    }
    assert_eq!(fs::read_dir(setup.home()).unwrap().count(), 0);
}

#[test]
fn a_secret_never_prints() {
    let secret = Secret::new(VALUE.into());
    assert_eq!(format!("{secret:?}"), "Secret(redacted)");
    assert_eq!(format!("{:?}", Some(&secret)), "Some(Secret(redacted))");
}

#[test]
fn a_command_source_debug_prints_no_argument() {
    let source = CredentialSource::Command(vec!["echo".into(), VALUE.into()]);
    let printed = format!("{source:?}");
    assert!(printed.contains("echo"), "{printed}");
    assert!(!printed.contains(VALUE), "{printed}");
}

#[test]
fn a_secret_pasted_into_configuration_reaches_no_error_or_notice() {
    for (text, fails) in [
        (format!(r#"{{"api_key": "{VALUE}"}}"#), false),
        (format!(r#"{{"handoff": {{"tokens": "{VALUE}"}}}}"#), true),
        (format!(r#"{{"model": "{VALUE}",}}"#), true),
        (
            format!(r#"{{"providers": {{"x": {{"credential": {{"key": "{VALUE}"}}}}}}}}"#),
            true,
        ),
    ] {
        let setup = Setup::new();
        setup.write(&setup.global(), &text);
        match setup.load(&[]) {
            Ok(config) => {
                assert!(!fails, "{text}");
                assert_eq!(config.notices().len(), 1);
                assert!(!format!("{:?}", config.notices()).contains(VALUE));
                assert!(!config.merged(None).to_string().contains(VALUE));
                assert!(!format!("{config:?}").contains(VALUE));
            }
            Err(e) => {
                assert!(fails, "{text}");
                assert!(!e.to_string().contains(VALUE), "{e}");
                assert!(!format!("{e:?}").contains(VALUE), "{e:?}");
            }
        }
    }
}

#[test]
fn a_stored_secret_is_not_part_of_the_configuration() {
    let setup = Setup::new();
    store_secret(&setup.home(), "openrouter", &Secret::new(VALUE.into())).unwrap();
    setup.write(
        &setup.global(),
        r#"{"providers": {"openrouter": {"credentials": {"work": {"env": "OPENROUTER_API_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(!config.merged(None).to_string().contains(VALUE));
    assert!(!format!("{config:?}").contains(VALUE));
    assert_eq!(
        config
            .get("providers.openrouter.credentials.work", None)
            .unwrap()
            .0,
        serde_json::json!({"env": "OPENROUTER_API_KEY"})
    );
}

#[test]
fn a_credential_that_is_a_symbolic_link_is_refused() {
    let setup = Setup::new();
    let outside = setup.root().join("planted");
    fs::write(&outside, VALUE).unwrap();
    fs::create_dir_all(setup.home().join("credentials")).unwrap();
    let link = setup.home().join("credentials/openrouter");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let e = read_secret(&setup.home(), "openrouter").unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(
        e.to_string().starts_with(&link.display().to_string()),
        "{e}"
    );
    assert!(!e.to_string().contains(VALUE));
}

#[test]
fn a_credentials_directory_that_is_a_symbolic_link_is_refused() {
    let setup = Setup::new();
    let outside = setup.root().join("planted");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("openrouter"), VALUE).unwrap();
    let link = setup.home().join("credentials");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let e = read_secret(&setup.home(), "openrouter").unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(
        e.to_string().starts_with(&link.display().to_string()),
        "{e}"
    );
    let e = store_secret(&setup.home(), "acme", &Secret::new(VALUE.into())).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(!outside.join("acme").exists());
}

#[test]
fn a_stored_credential_is_a_0600_file_in_a_0700_provider_directory() {
    let setup = Setup::new();
    store_credential(&setup.home(), "acme", "work", &Secret::new(VALUE.into())).unwrap();
    let file = setup.home().join("credentials/acme/work");
    assert_eq!(fs::read_to_string(&file).unwrap(), VALUE);
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&setup.home().join("credentials/acme")), 0o700);
    assert_eq!(mode(&setup.home().join("credentials")), 0o700);
    let read = read_credential(&setup.home(), "acme", "work")
        .unwrap()
        .unwrap();
    assert_eq!(read.expose(), VALUE);
    assert_eq!(
        read_credential(&setup.home(), "acme", "other").unwrap(),
        None
    );
    store_credential(&setup.home(), "acme", "work", &Secret::new("new".into())).unwrap();
    assert_eq!(
        read_credential(&setup.home(), "acme", "work")
            .unwrap()
            .unwrap()
            .expose(),
        "new"
    );
}

#[test]
fn a_label_that_is_not_one_plain_file_name_is_refused() {
    let setup = Setup::new();
    for label in [
        "",
        ".",
        "..",
        "a/b",
        "../x",
        "default.lock",
        "default.tmp",
        "a.tmp",
    ] {
        let e =
            store_credential(&setup.home(), "acme", label, &Secret::new(VALUE.into())).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArguments, "{label:?}");
        assert!(
            read_credential(&setup.home(), "acme", label).is_err(),
            "{label:?}"
        );
    }
    // Only the suffix is refused.
    store_credential(&setup.home(), "acme", "lock.x", &Secret::new(VALUE.into())).unwrap();
    store_credential(&setup.home(), "acme", "tmp", &Secret::new(VALUE.into())).unwrap();
    assert!(!setup.home().join("credentials/acme/default.lock").exists());
}

#[test]
fn a_stored_credential_that_is_a_symbolic_link_is_refused() {
    let setup = Setup::new();
    let outside = setup.root().join("planted");
    fs::write(&outside, VALUE).unwrap();
    fs::create_dir_all(setup.home().join("credentials/acme")).unwrap();
    let link = setup.home().join("credentials/acme/work");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let e = read_credential(&setup.home(), "acme", "work").unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(!e.to_string().contains(VALUE));
}

#[test]
fn a_bare_secret_and_a_provider_directory_of_one_name_collide() {
    let setup = Setup::new();
    store_secret(&setup.home(), "acme", &Secret::new(VALUE.into())).unwrap();
    let e = read_credential(&setup.home(), "acme", "work").unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    let e =
        store_credential(&setup.home(), "acme", "work", &Secret::new(VALUE.into())).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme")).unwrap(),
        VALUE
    );
}

#[test]
fn a_stored_credential_is_deleted_and_the_provider_directory_stays() {
    let setup = Setup::new();
    store_credential(&setup.home(), "acme", "work", &Secret::new(VALUE.into())).unwrap();
    store_credential(&setup.home(), "acme", "home", &Secret::new(VALUE.into())).unwrap();
    assert!(delete_credential(&setup.home(), "acme", "work").unwrap());
    assert!(!setup.home().join("credentials/acme/work").exists());
    // A label is still stored: the directory stays.
    assert!(setup.home().join("credentials/acme/home").exists());
    assert_eq!(credential_labels(&setup.home(), "acme").unwrap(), ["home"]);
    assert!(delete_credential(&setup.home(), "acme", "home").unwrap());
    assert!(setup.home().join("credentials/acme").is_dir());
    assert!(credential_labels(&setup.home(), "acme").unwrap().is_empty());
}

#[test]
fn deleting_a_credential_that_is_not_stored_is_false() {
    let setup = Setup::new();
    assert!(!delete_credential(&setup.home(), "acme", "work").unwrap());
    store_credential(&setup.home(), "acme", "home", &Secret::new(VALUE.into())).unwrap();
    assert!(!delete_credential(&setup.home(), "acme", "work").unwrap());
    assert!(setup.home().join("credentials/acme/home").exists());
}

#[test]
fn deleting_a_credential_keeps_its_lock_file_and_directory() {
    let setup = Setup::new();
    let file = CredentialFile::new(&setup.home(), "acme", "default").unwrap();
    let lock = file.try_lock().unwrap().unwrap();
    store_credential(&setup.home(), "acme", "default", &Secret::new(VALUE.into())).unwrap();
    drop(lock);
    assert!(delete_credential(&setup.home(), "acme", "default").unwrap());
    assert!(!setup.home().join("credentials/acme/default").exists());
    assert!(setup.home().join("credentials/acme/default.lock").exists());
}

#[test]
fn deleting_while_a_login_holds_the_lock_is_refused_and_removes_nothing() {
    let setup = Setup::new();
    let file = CredentialFile::new(&setup.home(), "acme", "default").unwrap();
    let lock = file.try_lock().unwrap().unwrap();
    store_credential(&setup.home(), "acme", "default", &Secret::new(VALUE.into())).unwrap();
    let e = delete_credential(&setup.home(), "acme", "default").unwrap_err();
    assert_eq!(e.code(), ErrorCode::IoFailed);
    assert!(setup.home().join("credentials/acme/default").exists());
    assert!(setup.home().join("credentials/acme/default.lock").exists());
    // The holder itself may remove it, and the lock file is still there.
    assert!(delete_credential_held(&setup.home(), "acme", "default", &lock).unwrap());
    assert!(!setup.home().join("credentials/acme/default").exists());
    assert!(setup.home().join("credentials/acme/default.lock").exists());
    assert!(!delete_credential_held(&setup.home(), "acme", "default", &lock).unwrap());
    drop(lock);
    // The next login takes the same lock file.
    assert!(file.try_lock().unwrap().is_some());
}

#[test]
fn deleting_refuses_a_name_that_is_not_one_file_name_and_a_symbolic_link() {
    let setup = Setup::new();
    let outside = setup.root().join("planted");
    fs::write(&outside, VALUE).unwrap();
    for (name, label) in [
        ("..", "x"),
        ("a/b", "x"),
        ("acme", "../x"),
        ("acme", "a.lock"),
    ] {
        let e = delete_credential(&setup.home(), name, label).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArguments, "{name} {label}");
    }
    fs::create_dir_all(setup.home().join("credentials/acme")).unwrap();
    std::os::unix::fs::symlink(&outside, setup.home().join("credentials/acme/work")).unwrap();
    let e = delete_credential(&setup.home(), "acme", "work").unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(outside.exists());
    // A provider directory that is a link is refused, and nothing behind it goes.
    let behind = setup.root().join("behind");
    fs::create_dir_all(&behind).unwrap();
    fs::write(behind.join("work"), VALUE).unwrap();
    std::os::unix::fs::symlink(&behind, setup.home().join("credentials/other")).unwrap();
    let e = delete_credential(&setup.home(), "other", "work").unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(behind.join("work").exists());
}
