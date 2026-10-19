//! Provider reads before an install commits, including replacement and cleanup.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use extensions::{Origin, Providers, Request};
use serde_json::json;

fn bounded(f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        tx.send(()).unwrap();
    });
    fakes::Deadline::after(Duration::from_secs(10)).recv_or_fail(&rx, "planned provider read");
}

fn source(root: &Path, name: &str, provider: &str, model: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(dir.join("extension.json"), json!({"name":format!("fiber.test/{name}"),"version":"0.1.0","fiber":"0.1.0","api":1,"providers":{provider:["https://example.com"]}}).to_string()).unwrap();
    fs::write(dir.join("providers").join(format!("{provider}.json")), json!({"name":provider,"models":[{"id":model,"protocol":"openai-responses","base_url":"https://example.com","context_window":1000}]}).to_string()).unwrap();
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
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&data).unwrap()).unwrap();
        value["models"][0]["prompt_addendum"] = json!("addenda/pc.md");
        fs::write(&data, value.to_string()).unwrap();
        fs::create_dir_all(c.join("addenda/pc.md")).unwrap();
        let clock = fakes::clock::FakeClock::new();
        extensions::plan(
            &home,
            &Request::Path(a),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .unwrap()
        .commit()
        .unwrap();
        let before = extensions::list(&home, &*clock).unwrap().installed;
        let files = ["extension.json", "providers/pa.json", ".fiber.json"];
        let bytes: Vec<_> = files
            .iter()
            .map(|file| fs::read(home.join("extensions/fiber.test-a").join(file)).unwrap())
            .collect();
        let plan = extensions::plan(
            &home,
            &Request::Path(c.clone()),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .unwrap();
        let staged = plan.items().next().unwrap().staged().to_path_buf();
        let error = Providers::load_planned(&home, &plan).unwrap_err();
        assert_eq!(error.code(), contract::ErrorCode::ConfigInvalid);
        assert!(error.to_string().contains("addenda/pc.md"));
        drop(plan);
        assert!(!staged.exists());
        assert_eq!(extensions::list(&home, &*clock).unwrap().installed, before);
        let after: Vec<_> = files
            .iter()
            .map(|file| fs::read(home.join("extensions/fiber.test-a").join(file)).unwrap())
            .collect();
        assert_eq!(after, bytes);
        extensions::plan(
            &home,
            &Request::Path(c),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .unwrap()
        .commit()
        .unwrap();
        let (providers, notices) = Providers::load(&home).unwrap();
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
        .unwrap()
        .commit()
        .unwrap();
        let plan = extensions::plan(
            &home,
            &Request::Path(b),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .unwrap();
        let staged = plan.items().next().unwrap().staged().to_path_buf();
        let (providers, notices) = Providers::load_planned(&home, &plan).unwrap();
        assert_eq!(providers.names().collect::<Vec<_>>(), ["pa", "pb"]);
        assert_eq!(providers.get("pb").unwrap().models[0].id, "new");
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
        .unwrap()
        .commit()
        .unwrap();
        source(root.path(), "a", "pa", "replacement");
        fs::write(
            home.join("extensions/fiber.test-a/providers/pa.json"),
            "not JSON",
        )
        .unwrap();
        let plan = extensions::plan(
            &home,
            &Request::Path(a),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .unwrap();
        let staged = plan.items().next().unwrap().staged().to_path_buf();
        let (providers, _) = Providers::load_planned(&home, &plan).unwrap();
        assert_eq!(providers.names().collect::<Vec<_>>(), ["pa"]);
        assert_eq!(providers.get("pa").unwrap().models[0].id, "replacement");
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
        .unwrap();
        let staged = plan.items().next().unwrap().staged().to_path_buf();
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
        fs::create_dir_all(&cache).unwrap();
        let clock = fakes::clock::FakeClock::new();
        let plan = extensions::plan(
            &home,
            &Request::Path(b),
            "0.1.0",
            &Origin::github(),
            &*clock,
        )
        .unwrap();
        let staged = plan.items().next().unwrap().staged().to_path_buf();
        let error = Providers::load_planned(&home, &plan).unwrap_err();
        assert_eq!(error.code(), contract::ErrorCode::IoFailed);
        assert!(error.to_string().contains(&cache.display().to_string()));
        drop(plan);
        assert!(!staged.exists());
    });
}
