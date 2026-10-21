#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use crate::test_support::write_record;
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use fakes::Deadline;
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
    let mut manifest = json!({ "name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 });
    manifest["providers"] = serde_json::Value::Object(
        [(provider.to_owned(), json!(["http://127.0.0.1:1/v1"]))]
            .into_iter()
            .collect(),
    );
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
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
        dir.join("providers/acme.lua"),
        r#"fiber.provider("acme", {
          credential = { timeout = 60000, run = function(arg)
            return { token = "t", expires_at = 4102444800 }
          end }
        })"#,
    )
    .unwrap();
}

#[test]
fn constructing_a_login_provider_runs_no_lua() {
    let root = fakes::TempDir::new("lazy-login");
    let home = root.path().join("home");
    install(&home, "acme-ext", "acme");
    let marker = root.path().join("init-ran");
    fs::write(
        home.join("extensions/acme-ext/init.lua"),
        format!("host.fs.write({:?}, 'x')", marker.display().to_string()),
    )
    .unwrap();
    let (providers, _) = Providers::load(&home).unwrap();
    let marker_copy = marker.clone();
    fakes::within(
        "lazy login construction",
        Deadline::after(Duration::from_secs(5)),
        move || {
            let provider = login_provider(
                &home,
                &providers,
                "acme",
                Arc::new(SystemBrowser::default()),
                FakeClock::new(),
            )
            .unwrap();
            assert_eq!(provider.name(), "acme");
            assert!(!marker_copy.exists());
            assert_eq!(provider.functions().unwrap(), ["credential"]);
        },
    );
    assert_eq!(fs::read_to_string(marker).unwrap(), "x");
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
            "providers": {"acme": ["http://127.0.0.1:1/v1"]},
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
    fs::write(dir.join("providers/acme.lua"), init).unwrap();
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
        Deadline::after(Duration::from_secs(5)),
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

#[test]
fn login_provider_fails_when_the_vm_registers_an_undeclared_provider() {
    let root = fakes::TempDir::new("fiber-login-undeclared");
    let home = root.path().join("home");
    install(&home, "acme-ext", "acme");
    let dir = home.join("extensions").join("acme-ext");
    fs::write(
        dir.join("extension.json"),
        json!({ "name": "acme-ext", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
                "providers": {"acme": ["http://127.0.0.1:1/v1"]} })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers/acme.lua"),
        r#"fiber.provider("acme", {
          credential = { timeout = 60000, run = function(arg)
            return { token = "t", expires_at = 4102444800 }
          end }
        })
        fiber.provider("rogue", {
          credential = { timeout = 60000, run = function(arg)
            return { token = "t", expires_at = 4102444800 }
          end }
        })"#,
    )
    .unwrap();
    let (providers, _) = Providers::load(&home).unwrap();
    let provider = login_provider(
        &home,
        &providers,
        "acme",
        Arc::new(SystemBrowser::default()),
        FakeClock::new(),
    )
    .unwrap();
    let err = fakes::within(
        "refused provider login",
        Deadline::after(Duration::from_secs(5)),
        move || provider.login("acme", None, crate::LoginMethod::Browser),
    )
    .unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::ExtensionFailed);
    assert!(err.to_string().contains("rogue"), "{err}");
}

/// Declares the browser provider `provider` of the repository extension
/// `name` in `workspace`: its manifest, its data file and its Lua
/// credential script.
fn declare_repo(workspace: &std::path::Path, name: &str, provider: &str) {
    fs::create_dir_all(workspace).unwrap();
    fakes::git::git(workspace, &["init", "--quiet"]);
    let dir = workspace.join("repo");
    fs::create_dir_all(dir.join("providers")).unwrap();
    let mut manifest = json!({ "name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 });
    manifest["providers"] = serde_json::Value::Object(
        [(provider.to_owned(), json!(["http://127.0.0.1:1/v1"]))]
            .into_iter()
            .collect(),
    );
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
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
        dir.join("providers").join(format!("{provider}.lua")),
        format!(
            r#"fiber.provider("{provider}", {{
          credential = {{ timeout = 60000, run = function(arg)
            return {{ token = "t", expires_at = 4102444800 }}
          end }}
        }})"#
        ),
    )
    .unwrap();
    let config_dir = workspace.join(".fiber");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.json"),
        json!({"repository_extensions": [{"path": "repo"}]}).to_string(),
    )
    .unwrap();
}

#[test]
fn login_provider_resolves_a_repository_only_browser_provider_to_its_copy() {
    let root = fakes::TempDir::new("repo-login");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    declare_repo(&workspace, "fiber.test/acme", "acme");
    let clock = FakeClock::new();
    let project = config::ProjectKey::new("pin").unwrap();
    let items = crate::declared_items(&workspace, &*clock).unwrap();
    let item = items
        .into_iter()
        .find(|found| found.name == "fiber.test/acme")
        .unwrap();
    let mut index = crate::Index::load(&home);
    let digest = crate::hash(&mut index, &item).unwrap();
    crate::Store::new(&home, &project, clock.clone())
        .approve(&item, &digest)
        .unwrap();
    // No personal install: only the pinned copy holds the extension, so a
    // home-layout resolution fails while the copy resolves.
    let set = crate::RepositorySet::load_pinned(&home, &project, &workspace, clock.clone());
    let (providers, notices) = Providers::load_all_repository(&home, &set).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        providers.provider_source_dir("acme"),
        Some(crate::Store::new(&home, &project, clock.clone()).copy_dir(&digest))
    );
    let provider = login_provider(
        &home,
        &providers,
        "acme",
        Arc::new(SystemBrowser::default()),
        clock,
    )
    .unwrap();
    assert_eq!(provider.name(), "acme");
    assert_eq!(provider.functions().unwrap(), ["credential"]);
}

/// Installs the `strings` dependency beside `name`, declares it in `name`'s
/// manifest, and points `name`'s provider at the `require`d module: the login
/// VM serves the declared dependency the way the session VM does.
fn install_with_strings_dep(home: &std::path::Path, name: &str, provider: &str, require: &str) {
    install(home, name, provider);
    let dir = home.join("extensions").join(name);
    let mut manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("extension.json")).unwrap()).unwrap();
    manifest["depends"] = json!({"strings": "1.0"});
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    let dep = home.join("extensions").join("strings");
    fs::create_dir_all(&dep).unwrap();
    fs::write(
        dep.join("extension.json"),
        json!({"name": "strings", "version": "v1.0.0", "fiber": "0.1.0", "api": 1}).to_string(),
    )
    .unwrap();
    write_record(&dep);
    fs::write(dep.join("trim.lua"), "return \"trimmed\"").unwrap();
    fs::write(
        dir.join("providers").join(format!("{provider}.lua")),
        format!(
            r#"local trim = require("{require}")
fiber.provider("{provider}", {{
  credential = {{ timeout = 60000, run = function(arg)
    return {{ token = trim, expires_at = 4102444800 }}
  end }}
}})"#,
        ),
    )
    .unwrap();
}

#[test]
fn a_login_provider_loads_a_declared_dependency_module() {
    let root = fakes::TempDir::new("fiber-login-dep-loads");
    let home = root.path().join("home");
    install_with_strings_dep(&home, "acme-ext", "acme", "strings:trim");
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
    // The provider file requires the dependency at load: a refusal would
    // fail the load, so listing its functions proves the require served.
    assert_eq!(provider.functions().unwrap(), ["credential"]);
}

#[test]
fn a_login_provider_refuses_a_module_outside_its_dependencies() {
    let root = fakes::TempDir::new("fiber-login-dep-refused");
    let home = root.path().join("home");
    install_with_strings_dep(&home, "acme-ext", "acme", "other:trim");
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
    let err = provider.functions().unwrap_err();
    assert!(
        err.to_string()
            .contains("`dependency 'other' not installed`"),
        "{err}"
    );
}

#[test]
fn a_login_provider_serves_a_copy_local_dependency() {
    let root = fakes::TempDir::new("repo-login-copy-dep");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    // The dependency repository `example.com/acme/strings` holding its
    // `trim` module.
    let dep_repo = root.path().join("repos").join("strings");
    fs::create_dir_all(&dep_repo).unwrap();
    fakes::git::git(&dep_repo, &["init", "--quiet"]);
    fs::write(
        dep_repo.join("extension.json"),
        json!({"name": "example.com/acme/strings", "version": "v1.0.0", "fiber": "0.1.0", "api": 1}).to_string(),
    )
    .unwrap();
    fs::write(dep_repo.join("trim.lua"), "return \"trimmed\"").unwrap();
    fakes::git::git(&dep_repo, &["add", "."]);
    fakes::git::git(
        &dep_repo,
        &["commit", "--quiet", "--allow-empty", "-m", "release"],
    );
    fakes::git::git(&dep_repo, &["tag", "v1.0.0"]);
    // The repository extension declares the dependency and requires it
    // from its provider script: the login VM serves the declared module
    // the way the session VM does.
    fs::create_dir_all(&workspace).unwrap();
    fakes::git::git(&workspace, &["init", "--quiet"]);
    let dir = workspace.join("repo");
    fs::create_dir_all(dir.join("providers")).unwrap();
    let mut manifest =
        json!({"name": "fiber.test/acme", "version": "v1.0.0", "fiber": "0.1.0", "api": 1});
    manifest["depends"] = json!({"example.com/acme/strings": "1.0"});
    manifest["providers"] = serde_json::Value::Object(
        [("acme".to_owned(), json!(["http://127.0.0.1:1/v1"]))]
            .into_iter()
            .collect(),
    );
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    fs::write(
        dir.join("providers").join("acme.json"),
        json!({
            "name": "acme",
            "login": "browser",
            "models": [
                { "id": "m", "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 },
            ],
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers").join("acme.lua"),
        "local trim = require(\"example.com/acme/strings:trim\")\n\
         fiber.provider(\"acme\", {\n\
           credential = { timeout = 60000, run = function(arg)\n\
             return { token = trim, expires_at = 4102444800 }\n\
           end }\n\
         })",
    )
    .unwrap();
    fs::create_dir_all(workspace.join(".fiber")).unwrap();
    fs::write(
        workspace.join(".fiber").join("config.json"),
        json!({"repository_extensions": [{"path": "repo"}]}).to_string(),
    )
    .unwrap();
    let clock = FakeClock::new();
    let project = config::ProjectKey::new("pin").unwrap();
    let items = crate::declared_items(&workspace, &*clock).unwrap();
    let item = items
        .into_iter()
        .find(|found| found.name == "fiber.test/acme")
        .unwrap();
    let mut index = crate::Index::load(&home);
    let digest = crate::hash(&mut index, &item).unwrap();
    let origin = crate::Origin::new("git", move |repo: &str| {
        if repo == "example.com/acme/strings" {
            format!("file://{}", dep_repo.display())
        } else {
            "file:///nonexistent-repository".to_owned()
        }
    });
    crate::Store::new(&home, &project, clock.clone())
        .fetching(crate::Fetch::new(origin, "0.1.0", clock.clone()))
        .approve(&item, &digest)
        .unwrap();
    // The dependency installs under the copy's own `.fiber-depends`, never
    // Fiber home: the login derives its repository layout from the same
    // resolved directory, so no home fallback leaks in.
    let copy_dep = home
        .join("pinned")
        .join(&digest)
        .join(".fiber-depends")
        .join("extensions")
        .join(config::dir_name("example.com/acme/strings"))
        .join("trim.lua");
    assert!(copy_dep.is_file(), "{}", copy_dep.display());
    assert!(
        !home
            .join("extensions")
            .join(config::dir_name("example.com/acme/strings"))
            .exists()
    );
    let set = crate::RepositorySet::load_pinned(&home, &project, &workspace, clock.clone());
    let (providers, notices) = Providers::load_all_repository(&home, &set).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        providers.provider_source_dir("acme"),
        Some(crate::Store::new(&home, &project, clock.clone()).copy_dir(&digest))
    );
    let provider = login_provider(
        &home,
        &providers,
        "acme",
        Arc::new(SystemBrowser::default()),
        clock,
    )
    .unwrap();
    assert_eq!(provider.name(), "acme");
    // The provider script requires the dependency at load: a refusal would
    // fail the load, so listing its functions proves the copy-local require
    // served.
    assert_eq!(provider.functions().unwrap(), ["credential"]);
}
