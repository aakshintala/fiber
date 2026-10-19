//! Provider reads before an install commits, including replacement and cleanup.

#![allow(
    clippy::expect_used,
    reason = "test fixtures; failures must stop the test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use extensions::{Origin, Providers, Request};
use serde_json::json;

fn bounded(f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        tx.send(()).expect("test fixture operation succeeds");
    });
    fakes::Deadline::after(Duration::from_secs(10)).recv_or_fail(&rx, "planned provider read");
}

fn source(root: &Path, name: &str, provider: &str, model: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("providers")).expect("test fixture operation succeeds");
    fs::write(dir.join("extension.json"), json!({"name":format!("fiber.test/{name}"),"version":"0.1.0","fiber":"0.1.0","api":1,"providers":{provider:["https://example.com"]}}).to_string()).expect("test fixture operation succeeds");
    fs::write(dir.join("providers").join(format!("{provider}.json")), json!({"name":provider,"models":[{"id":model,"protocol":"openai-responses","base_url":"https://example.com","context_window":1000}]}).to_string()).expect("test fixture operation succeeds");
    dir
}

#[test]
fn providers_planned_rejects_staged_addenda_but_installed_addenda_leave_a_notice() {
    bounded(|| {
        let root = fakes::TempDir::new("providers-planned-addendum");
        let home = root.path().join("home");
        let a = source(root.path(), "a", "pa", "old");
        let c = source(root.path(), "c", "pc", "new");
        let data = c.join("providers/pc.json");
        let mut value: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&data).expect("test fixture operation succeeds"),
        )
        .expect("test fixture operation succeeds");
        value["models"][0]["prompt_addendum"] = json!("addenda/pc.md");
        fs::write(&data, value.to_string()).expect("test fixture operation succeeds");
        fs::create_dir_all(c.join("addenda/pc.md")).expect("test fixture operation succeeds");
        let clock = fakes::clock::FakeClock::new();
        extensions::plan(
            &home,
            &Request::Path(a),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .expect("test fixture operation succeeds")
        .commit()
        .expect("test fixture operation succeeds");
        let before = extensions::list(&home, &*clock)
            .expect("test fixture operation succeeds")
            .installed;
        let files = ["extension.json", "providers/pa.json", ".fiber.json"];
        let bytes: Vec<_> = files
            .iter()
            .map(|file| {
                fs::read(home.join("extensions/fiber.test-a").join(file))
                    .expect("test fixture operation succeeds")
            })
            .collect();
        let plan = extensions::plan(
            &home,
            &Request::Path(c.clone()),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .expect("test fixture operation succeeds");
        let staged = plan
            .items()
            .next()
            .expect("test fixture operation succeeds")
            .staged()
            .to_path_buf();
        let error = Providers::load_planned(&home, &plan).unwrap_err();
        assert_eq!(error.code(), contract::ErrorCode::ConfigInvalid);
        assert!(error.to_string().contains("addenda/pc.md"));
        drop(plan);
        assert!(!staged.exists());
        assert_eq!(
            extensions::list(&home, &*clock)
                .expect("test fixture operation succeeds")
                .installed,
            before
        );
        let after: Vec<_> = files
            .iter()
            .map(|file| {
                fs::read(home.join("extensions/fiber.test-a").join(file))
                    .expect("test fixture operation succeeds")
            })
            .collect();
        assert_eq!(after, bytes);
        extensions::plan(
            &home,
            &Request::Path(c),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .expect("test fixture operation succeeds")
        .commit()
        .expect("test fixture operation succeeds");
        let (providers, notices) = Providers::load(&home).expect("test fixture operation succeeds");
        assert_eq!(providers.names().collect::<Vec<_>>(), ["pa"]);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].code, contract::ErrorCode::ExtensionFailed);
        assert!(notices[0].message.contains("addenda/pc.md"));
    });
}

#[test]
fn providers_planned_reads_installed_and_staged_data_and_drops_staging() {
    bounded(|| {
        let root = fakes::TempDir::new("providers-planned-union");
        let home = root.path().join("home");
        let a = source(root.path(), "a", "pa", "old");
        let b = source(root.path(), "b", "pb", "new");
        let clock = fakes::clock::FakeClock::new();
        extensions::plan(
            &home,
            &Request::Path(a),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .expect("test fixture operation succeeds")
        .commit()
        .expect("test fixture operation succeeds");
        let plan = extensions::plan(
            &home,
            &Request::Path(b),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .expect("test fixture operation succeeds");
        let staged = plan
            .items()
            .next()
            .expect("test fixture operation succeeds")
            .staged()
            .to_path_buf();
        let (providers, notices) =
            Providers::load_planned(&home, &plan).expect("test fixture operation succeeds");
        assert_eq!(providers.names().collect::<Vec<_>>(), ["pa", "pb"]);
        assert_eq!(
            providers
                .get("pb")
                .expect("test fixture operation succeeds")
                .models[0]
                .id,
            "new"
        );
        assert!(notices.is_empty());
        drop(plan);
        assert!(!staged.exists());
    });
}

#[test]
fn providers_planned_replaces_installed_data_before_reading_it() {
    bounded(|| {
        let root = fakes::TempDir::new("providers-planned-replace");
        let home = root.path().join("home");
        let a = source(root.path(), "a", "pa", "old");
        let clock = fakes::clock::FakeClock::new();
        extensions::plan(
            &home,
            &Request::Path(a.clone()),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .expect("test fixture operation succeeds")
        .commit()
        .expect("test fixture operation succeeds");
        source(root.path(), "a", "pa", "replacement");
        fs::write(
            home.join("extensions/fiber.test-a/providers/pa.json"),
            "not JSON",
        )
        .expect("test fixture operation succeeds");
        let plan = extensions::plan(
            &home,
            &Request::Path(a),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .expect("test fixture operation succeeds");
        let staged = plan
            .items()
            .next()
            .expect("test fixture operation succeeds")
            .staged()
            .to_path_buf();
        let (providers, _) =
            Providers::load_planned(&home, &plan).expect("test fixture operation succeeds");
        assert_eq!(providers.names().collect::<Vec<_>>(), ["pa"]);
        assert_eq!(
            providers
                .get("pa")
                .expect("test fixture operation succeeds")
                .models[0]
                .id,
            "replacement"
        );
        drop(plan);
        assert!(!staged.exists());
        let b = source(root.path(), "b", "pb", "new");
        let plan = extensions::plan(
            &home,
            &Request::Path(b),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .expect("test fixture operation succeeds");
        let staged = plan
            .items()
            .next()
            .expect("test fixture operation succeeds")
            .staged()
            .to_path_buf();
        let error = Providers::load_planned(&home, &plan).unwrap_err();
        assert_eq!(error.code(), contract::ErrorCode::ConfigInvalid);
        assert!(error.to_string().contains("pa.json"));
        drop(plan);
        assert!(!staged.exists());
    });
}

#[test]
fn providers_planned_rejects_an_unreadable_staged_model_cache() {
    bounded(|| {
        let root = fakes::TempDir::new("providers-planned-cache");
        let home = root.path().join("home");
        let b = source(root.path(), "b", "pb", "new");
        let cache = home.join("cache/models/pb.json");
        fs::create_dir_all(&cache).expect("test fixture operation succeeds");
        let clock = fakes::clock::FakeClock::new();
        let plan = extensions::plan(
            &home,
            &Request::Path(b),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .expect("test fixture operation succeeds");
        let staged = plan
            .items()
            .next()
            .expect("test fixture operation succeeds")
            .staged()
            .to_path_buf();
        let error = Providers::load_planned(&home, &plan).unwrap_err();
        assert_eq!(error.code(), contract::ErrorCode::IoFailed);
        assert!(error.to_string().contains(&cache.display().to_string()));
        drop(plan);
        assert!(!staged.exists());
    });
}
