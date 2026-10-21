//! The local configuration seam installs with no prompt and checks configuration first.

#![allow(
    clippy::expect_used,
    reason = "test fixture setup must stop on failure"
)]

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use contract::{ErrorCode, Secret};
use serde_json::json;
use tui::{Configure, LoginNeed};

use crate::configure::Seam;
use crate::test_support::Rig;
use fakes::Deadline;

const NAME: &str = "github.com/aakshintala/fiber/providers/openai";

fn repository() -> fakes::TempDir {
    let repo = fakes::git::repo("fiber-configure-install-source");
    let dir = repo.path().join("providers/openai");
    fs::create_dir_all(dir.join("providers")).expect("provider directory");
    fs::write(dir.join("extension.json"), json!({"name":NAME,"version":"0.1.0","fiber":env!("CARGO_PKG_VERSION"),"api":1,"providers":{"openai":["https://example.com"]}}).to_string()).expect("manifest");
    fs::write(dir.join("providers/openai.json"), json!({"name":"openai","models":[{"id":"m","protocol":"openai-responses","base_url":"https://example.com","context_window":1000}]}).to_string()).expect("provider data");
    fakes::git::git(repo.path(), &["add", "."]);
    fakes::git::git(repo.path(), &["commit", "--quiet", "-m", "provider"]);
    fakes::git::git(repo.path(), &["tag", "v0.1.0"]);
    repo
}

fn origin(repo: &std::path::Path) -> Arc<extensions::Origin> {
    let repo = repo.to_path_buf();
    Arc::new(extensions::Origin::new("git", move |name| {
        assert_eq!(name, "github.com/aakshintala/fiber");
        format!("file://{}", repo.display())
    }))
}

fn summary() -> Vec<String> {
    vec![
        format!("Install {NAME} from {NAME}"),
        "Version v0.1.0".into(),
        "Provider openai: https://example.com".into(),
    ]
}

fn call(
    seam: &Seam,
    workspace: &std::path::Path,
) -> Result<Option<serde_json::Value>, contract::shapes::Failure> {
    use hub::Machine;
    seam.configure(
        "install_provider",
        json!({"workspace":workspace,"provider":"openai"})
            .as_object()
            .expect("request object"),
        fakes::clock::FakeClock::new(),
    )
}

#[test]
fn hub_install_returns_summary_and_omits_absent_login_errors() {
    fakes::within(
        "hub provider install",
        Deadline::after(Duration::from_secs(20)),
        || {
            let rig = Rig::new("fiber-configure-install-wire");
            let repo = repository();
            let seam = Seam::with_origin(rig.home.clone(), origin(repo.path()));
            let result = call(&seam, &rig.workspace).unwrap().unwrap();
            assert_eq!(
                result,
                json!({"installed":[NAME],"summary":summary(),"login":[{"provider":"openai"}]})
            );
            let key = rig.home.join("credentials/openai/default");
            fs::create_dir_all(&key).unwrap();
            let result = call(&seam, &rig.workspace).unwrap().unwrap();
            assert_eq!(result["installed"], json!([NAME]));
            assert_eq!(result["login"][0]["provider"], "openai");
            assert!(
                result["login"][0]["error"]
                    .as_str()
                    .unwrap()
                    .contains(&key.display().to_string())
            );
        },
    );
}

#[test]
fn hub_install_rejections_preserve_configuration_and_fetch_error_codes() {
    fakes::within(
        "hub provider install failure",
        Deadline::after(Duration::from_secs(20)),
        || {
            let rig = Rig::new("fiber-configure-install-wire-error");
            let seam = Seam::with_origin(rig.home.clone(), origin(&rig.root.path().join("absent")));
            let failure = call(&seam, &rig.workspace).unwrap_err();
            assert_eq!(failure.code, ErrorCode::ExtensionNotFound);
            assert!(failure.message.contains("not found"));
            fs::write(rig.home.join("config.json"), "not JSON").unwrap();
            let seam = Seam::with_origin(
                rig.home.clone(),
                Arc::new(extensions::Origin::new("git", |_| {
                    panic!("invalid configuration must not fetch")
                })),
            );
            let failure = call(&seam, &rig.workspace).unwrap_err();
            assert_eq!(failure.code, ErrorCode::ConfigInvalid);
            assert!(failure.message.contains("config.json"));
        },
    );
}

#[test]
fn fresh_install_returns_summary_and_login_then_stored_key_removes_login() {
    fakes::within(
        "configure provider install",
        Deadline::after(Duration::from_secs(20)),
        || {
            let rig = Rig::new("fiber-configure-install");
            let repo = repository();
            let seam = Seam::with_origin(rig.home.clone(), origin(repo.path()));
            let clock = fakes::clock::FakeClock::new();
            let answer = seam
                .install_provider(&rig.workspace, "openai", clock.clone())
                .unwrap();
            assert_eq!(answer.installed, [NAME]);
            assert_eq!(answer.summary, summary());
            assert_eq!(
                answer.login,
                [LoginNeed {
                    provider: "openai".into(),
                    error: None
                }]
            );
            seam.store_key("openai", None, Secret::new("test-key".into()))
                .unwrap();
            let answer = seam
                .install_provider(&rig.workspace, "openai", clock)
                .unwrap();
            assert_eq!(answer.installed, [NAME]);
            assert!(answer.login.is_empty());
        },
    );
}

#[test]
fn invalid_configuration_rejects_before_any_fetch() {
    let rig = Rig::new("fiber-configure-install-config");
    fs::write(rig.home.join("config.json"), "not JSON").unwrap();
    let seam = Seam::with_origin(
        rig.home.clone(),
        Arc::new(extensions::Origin::new("git", |_| {
            panic!("configuration must load before fetching")
        })),
    );
    let error = seam
        .install_provider(&rig.workspace, "openai", fakes::clock::FakeClock::new())
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigInvalid);
    assert!(!rig.home.join("extensions").exists());
}

#[test]
fn scripted_is_not_a_first_party_install() {
    let rig = Rig::new("fiber-configure-install-scripted");
    let seam = Seam::with_origin(
        rig.home.clone(),
        Arc::new(extensions::Origin::new("git", |_| {
            panic!("scripted must never fetch")
        })),
    );
    let error = seam
        .install_provider(&rig.workspace, "scripted", fakes::clock::FakeClock::new())
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArguments);
    assert_eq!(
        error.message,
        "No first-party package serves the provider `scripted`."
    );
    assert!(!rig.home.join("extensions").exists());
}
