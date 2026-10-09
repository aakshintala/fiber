#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use fakes::clock::FakeClock;
use serde_json::json;

use super::login_provider;
use crate::Error;
use crate::oauth::SystemBrowser;
use crate::providers::Providers;

/// Installs the extension `name` registering `provider` in `home`.
fn install(home: &std::path::Path, name: &str, provider: &str) {
    let dir = home.join("extensions").join(name);
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({ "name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 }).to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers").join(format!("{provider}.json")),
        json!({
            "name": provider,
            "login": "browser",
            "models": [
                { "id": "m", "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 },
            ],
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("init.lua"),
        r#"fiber.provider("acme", {
          credential = { timeout = 60000, run = function(arg)
            return { token = "t", expires_at = 4102444800 }
          end }
        })"#,
    )
    .unwrap();
}

#[test]
fn login_provider_returns_the_extension_s_provider() {
    let root = fakes::TempDir::new("fiber-login-provider");
    let home = root.path().join("home");
    install(&home, "acme-ext", "acme");
    let (providers, notices) = Providers::load(&home).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(providers.extension_of("acme"), Some("acme-ext"));

    let provider = login_provider(
        &home,
        &providers,
        "acme",
        Arc::new(SystemBrowser::default()),
        FakeClock::new(),
    )
    .unwrap();
    assert_eq!(provider.name(), "acme");
    assert_eq!(provider.functions().unwrap(), ["credential"]);
}

#[test]
fn login_provider_applies_the_manifest_memory_cap_in_mib() {
    let root = fakes::TempDir::new("fiber-login-provider-memory");
    let home = root.path().join("home");
    install(&home, "acme-ext", "acme");
    let dir = home.join("extensions").join("acme-ext");
    fs::write(
        dir.join("extension.json"),
        json!({
            "name": "acme-ext",
            "version": "v1.0.0",
            "fiber": "0.1.0",
            "api": 1,
            "memory_mib": 2,
        })
        .to_string(),
    )
    .unwrap();
    let mut init = format!("-- {}\n", "x".repeat(1_200_000));
    init.push_str(
        r#"fiber.provider("acme", {
          credential = { timeout = 60000, run = function()
            return { token = "t", expires_at = 4102444800 }
          end }
        })"#,
    );
    fs::write(dir.join("init.lua"), init).unwrap();
    let (providers, notices) = Providers::load(&home).unwrap();
    assert!(notices.is_empty(), "{notices:?}");

    let provider = login_provider(
        &home,
        &providers,
        "acme",
        Arc::new(SystemBrowser::default()),
        FakeClock::new(),
    )
    .unwrap();

    let functions = fakes::within(
        "provider VM registration",
        Duration::from_secs(5),
        move || provider.functions(),
    );
    assert_eq!(functions.unwrap(), ["credential"]);
}

#[test]
fn login_provider_for_a_provider_no_extension_registers_is_provider_missing() {
    let root = fakes::TempDir::new("fiber-login-provider-missing");
    let home = root.path().join("home");
    install(&home, "acme-ext", "acme");
    let (providers, _) = Providers::load(&home).unwrap();

    let Err(error) = login_provider(
        &home,
        &providers,
        "other",
        Arc::new(SystemBrowser::default()),
        FakeClock::new(),
    ) else {
        panic!("a provider no extension registers logs in");
    };
    assert!(
        matches!(&error, Error::ProviderMissing { provider } if provider == "other"),
        "{error:?}"
    );
}

#[test]
fn login_provider_without_an_installed_package_is_a_config_error() {
    let root = fakes::TempDir::new("fiber-login-provider-uninstalled");
    let home = root.path().join("home");
    install(&home, "acme-ext", "acme");
    let (providers, _) = Providers::load(&home).unwrap();
    fs::remove_dir_all(home.join("extensions").join("acme-ext")).unwrap();

    let Err(error) = login_provider(
        &home,
        &providers,
        "acme",
        Arc::new(SystemBrowser::default()),
        FakeClock::new(),
    ) else {
        panic!("a login without an installed package resolves");
    };
    assert!(matches!(&error, Error::Config(_)), "{error:?}");
}
