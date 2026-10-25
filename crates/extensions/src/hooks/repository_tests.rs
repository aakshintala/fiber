//! Session hooks from pinned copies: the filtered configuration, retention
//! over skipped replacements, and the required gate.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use config::{Config, ProjectKey, Sources};
use contract::events::OfferedKind;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::{
    Fetch, Index, RepositorySet, SessionExtensions, StateSeed, Store, declared_items, hash,
};

const KEY: &str = "-tmp-project-hooks-.git";

fn project() -> ProjectKey {
    ProjectKey::new(KEY).unwrap()
}

struct Setup {
    tmp: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let tmp = fakes::TempDir::new("fiber-hooks-session");
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        fs::create_dir_all(tmp.path().join("workspace")).unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(tmp.path().join("workspace"))
            .status()
            .unwrap();
        assert!(status.success());
        Self { tmp }
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.tmp.path().join("workspace")
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.workspace().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn hooks(&self, value: &Value) {
        self.write(
            ".fiber/config/hooks.json",
            &json!({"hooks": value}).to_string(),
        );
    }

    fn config(&self, overrides: &[&str]) -> Config {
        Config::load(Sources {
            home: self.home(),
            workspace: self.workspace(),
            project: project(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap()
    }

    fn approve(&self, name: &str) -> String {
        let clock = FakeClock::new();
        let items = declared_items(&self.workspace(), &*clock).unwrap();
        let item = items
            .into_iter()
            .find(|i| i.kind == OfferedKind::Hook && i.name == name)
            .unwrap();
        let digest = hash(&mut Index::load(&self.home()), &item).unwrap();
        Store::new(&self.home(), &project(), clock)
            .approve(&item, &digest)
            .unwrap();
        digest
    }

    fn set(&self, skipped: &BTreeSet<(String, String)>) -> RepositorySet {
        RepositorySet::load(
            &self.home(),
            &project(),
            &self.workspace(),
            skipped,
            Fetch::new(crate::Origin::github(), "0.0.0", FakeClock::new()),
        )
    }

    fn load(&self, config: &Config, set: RepositorySet) -> SessionExtensions {
        let home = self.home();
        SessionExtensions::load_repository(
            &home,
            config,
            FakeClock::new(),
            Arc::new(fakes::FakeLock::new()),
            None,
            StateSeed::new(),
            set,
        )
    }
}

fn empty_skipped() -> BTreeSet<(String, String)> {
    BTreeSet::new()
}

#[test]
fn filtered_config_withholds_unapproved_and_merges_approved() {
    let setup = Setup::new();
    setup.write("scripts/repo.sh", "repo\n");
    setup.write("scripts/person.sh", "person\n");
    // Person's entry for `mine`, repository's unapproved entry for `theirs`.
    setup.write(
        ".fiber/config/hooks.json",
        &json!({"hooks": {
            "theirs": {"point": "after_tool", "command": "scripts/repo.sh", "timeout": 1000, "on_failure": "non-blocking"},
        }}).to_string(),
    );
    let config = setup.config(&[
        r#"extensions."github.com/aakshintala/fiber/extensions/hooks".settings.hooks={"mine": {"point": "after_tool", "command": "scripts/person.sh", "timeout": 1000, "on_failure": "non-blocking"}}"#,
    ]);
    let session = setup.load(&config, setup.set(&empty_skipped()));
    let filtered = session.hooks_config(&config);
    let (merged, _) = filtered
        .extensions()
        .merged(&config::full_name("hooks"), &["hooks"])
        .unwrap();
    // The person's entry runs unchanged; the unapproved repository entry is
    // withheld before the merge.
    assert_eq!(
        merged,
        json!({"hooks": {"mine": {"point": "after_tool", "command": "scripts/person.sh", "timeout": 1000, "on_failure": "non-blocking"}}})
    );
    // Approved merges key by key under the person's layers.
    setup.write("scripts/repo.sh", "repo\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/repo.sh", "timeout": 1000, "on_failure": "non-blocking"}}));
    let hash = setup.approve("fmt");
    let session = setup.load(&setup.config(&[]), setup.set(&empty_skipped()));
    let filtered = session.hooks_config(&setup.config(&[]));
    let (merged, _) = filtered
        .extensions()
        .merged(&config::full_name("hooks"), &["hooks"])
        .unwrap();
    let entry = merged.get("hooks").and_then(|v| v.get("fmt")).unwrap();
    assert_eq!(
        entry.get("command").and_then(Value::as_str).unwrap(),
        setup
            .home()
            .join("pinned")
            .join(&hash)
            .join("scripts/repo.sh")
            .display()
            .to_string()
    );
}

#[test]
fn hook_source_attribution_agrees_with_filtered_values() {
    let setup = Setup::new();
    setup.write("scripts/repo.sh", "x\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/repo.sh", "timeout": 1000, "on_failure": "non-blocking"}}));
    setup.approve("fmt");
    let config = setup.config(&[]);
    let session = setup.load(&config, setup.set(&empty_skipped()));
    let filtered = session.hooks_config(&config);
    let repo_file = setup.workspace().join(".fiber/config/hooks.json");
    let (value, source) = filtered
        .extensions()
        .get_layered(&config::full_name("hooks"), &["hooks"], "hooks.fmt.command")
        .unwrap()
        .0
        .unwrap();
    assert!(value.as_str().unwrap().contains("pinned"));
    assert_eq!(source, config::Source::Repository(repo_file));
}

#[test]
fn host_config_set_stays_visible_under_the_overlay() {
    let setup = Setup::new();
    setup.write("scripts/repo.sh", "x\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/repo.sh", "timeout": 1000, "on_failure": "non-blocking"}}));
    setup.approve("fmt");
    let config = setup.config(&[]);
    let session = setup.load(&config, setup.set(&empty_skipped()));
    let mut filtered = session.hooks_config(&config);
    filtered
        .extensions_mut()
        .set(
            &config::full_name("hooks"),
            config::Scope::Machine,
            "hooks.mine.point",
            json!("after_tool"),
        )
        .unwrap();
    let got = filtered
        .extensions()
        .get(&config::full_name("hooks"), &["hooks"], "hooks.mine.point")
        .unwrap();
    assert_eq!(got, Some(json!("after_tool")));
    // The approved entry is still the pinned copy.
    let got = filtered
        .extensions()
        .get(&config::full_name("hooks"), &["hooks"], "hooks.fmt.command")
        .unwrap()
        .unwrap();
    assert!(got.as_str().unwrap().contains("pinned"));
}

#[test]
fn skip_retains_old_and_approval_switches_to_new() {
    let setup = Setup::new();
    setup.write("scripts/v1.sh", "v1\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/v1.sh", "timeout": 1000, "on_failure": "non-blocking"}}));
    let v1_hash = setup.approve("fmt");
    let config = setup.config(&[]);
    let session = setup.load(&config, setup.set(&empty_skipped()));
    let v1_command = session
        .hooks_config(&config)
        .extensions()
        .get(&config::full_name("hooks"), &["hooks"], "hooks.fmt.command")
        .unwrap()
        .unwrap();
    assert!(v1_command.as_str().unwrap().contains(&v1_hash));
    // Edit to v2, observe the replacement, skip it, reload: the old still runs.
    setup.write("scripts/v2.sh", "v2\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/v2.sh", "timeout": 1000, "on_failure": "non-blocking"}}));
    let items = declared_items(&setup.workspace(), &*FakeClock::new()).unwrap();
    let v2_item = items
        .into_iter()
        .find(|i| i.kind == OfferedKind::Hook && i.name == "fmt")
        .unwrap();
    let v2_hash = hash(&mut Index::load(&setup.home()), &v2_item).unwrap();
    assert_ne!(v1_hash, v2_hash);
    let mut skipped = BTreeSet::new();
    skipped.insert(("fmt".to_owned(), v2_hash.clone()));
    let reloaded = session.reload_repository(
        &setup.home(),
        &config,
        FakeClock::new(),
        Arc::new(fakes::FakeLock::new()),
        setup.set(&skipped),
    );
    let kept = reloaded
        .hooks_config(&config)
        .extensions()
        .get(&config::full_name("hooks"), &["hooks"], "hooks.fmt.command")
        .unwrap()
        .unwrap();
    assert_eq!(
        kept, v1_command,
        "a skipped replacement keeps the old snapshot"
    );
    // Approve v2 and reload: the new copy runs.
    Store::new(&setup.home(), &project(), FakeClock::new())
        .approve(&v2_item, &v2_hash)
        .unwrap();
    let switched = reloaded.reload_repository(
        &setup.home(),
        &config,
        FakeClock::new(),
        Arc::new(fakes::FakeLock::new()),
        setup.set(&empty_skipped()),
    );
    let now = switched
        .hooks_config(&config)
        .extensions()
        .get(&config::full_name("hooks"), &["hooks"], "hooks.fmt.command")
        .unwrap()
        .unwrap();
    assert!(now.as_str().unwrap().contains(&v2_hash));
    assert_ne!(now, v1_command);
}

#[test]
fn unapproved_hooks_gate_when_the_extension_is_absent() {
    let setup = Setup::new();
    setup.write("scripts/repo.sh", "x\n");
    // An unapproved repository entry is withheld before the merge, so even a
    // blocking one does not gate the start when the extension is absent.
    setup.hooks(&json!({"guard": {"point": "after_tool", "command": "scripts/repo.sh", "timeout": 1000, "on_failure": "blocking"}}));
    let config = setup.config(&[]);
    let session = setup.load(&config, setup.set(&empty_skipped()));
    let filtered = session.hooks_config(&config);
    assert_eq!(
        crate::hooks_state(&filtered, &[]).unwrap(),
        crate::HooksState::Ready
    );
    setup.hooks(&json!({"note": {"point": "after_tool", "command": "scripts/repo.sh", "timeout": 1000, "on_failure": "non-blocking"}}));
    let config = setup.config(&[]);
    let session = setup.load(&config, setup.set(&empty_skipped()));
    let filtered = session.hooks_config(&config);
    assert_eq!(
        crate::hooks_state(&filtered, &[]).unwrap(),
        crate::HooksState::Ready
    );
    // Once approved, a blocking entry gates the start without the extension.
    setup.hooks(&json!({"guard": {"point": "after_tool", "command": "scripts/repo.sh", "timeout": 1000, "on_failure": "blocking"}}));
    setup.approve("guard");
    let config = setup.config(&[]);
    let session = setup.load(&config, setup.set(&empty_skipped()));
    let filtered = session.hooks_config(&config);
    assert_eq!(
        crate::hooks_state(&filtered, &[]).unwrap(),
        crate::HooksState::MissingBlocking
    );
}

#[test]
fn deleted_hooks_drop_the_old_snapshot() {
    let setup = Setup::new();
    setup.write("scripts/v1.sh", "v1\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/v1.sh", "timeout": 1000, "on_failure": "non-blocking"}}));
    setup.approve("fmt");
    let config = setup.config(&[]);
    let session = setup.load(&config, setup.set(&empty_skipped()));
    assert!(!session.hook_snapshots.is_empty());
    // Delete the declaration and reload: the old snapshot drops.
    setup.hooks(&json!({}));
    let reloaded = session.reload_repository(
        &setup.home(),
        &config,
        FakeClock::new(),
        Arc::new(fakes::FakeLock::new()),
        setup.set(&empty_skipped()),
    );
    assert!(reloaded.hook_snapshots.is_empty());
}

#[test]
fn non_required_copy_failures_leave_a_hook_failed_notice() {
    use contract::ErrorCode;
    let setup = Setup::new();
    setup.write("scripts/fmt.sh", "x\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh", "timeout": 1000, "on_failure": "non-blocking"}}));
    let hash = setup.approve("fmt");
    fs::remove_dir_all(setup.home().join("pinned").join(&hash)).unwrap();
    fs::remove_file(setup.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
    fs::create_dir(setup.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
    let set = setup.set(&empty_skipped());
    assert!(set.hook_failures.is_empty());
    assert!(
        set.notices
            .iter()
            .any(|n| n.code == ErrorCode::HookFailed && n.message.contains("`fmt`"))
    );
}

#[test]
fn required_copy_failures_are_queryable_by_repository_required() {
    let setup = Setup::new();
    setup.write("scripts/fmt.sh", "x\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh", "timeout": 1000, "on_failure": "non-blocking", "required": true}}));
    let hash = setup.approve("fmt");
    // Break the copy so materialization fails.
    fs::remove_dir_all(setup.home().join("pinned").join(&hash)).unwrap();
    fs::remove_file(setup.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
    fs::create_dir(setup.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
    let session = setup.load(&setup.config(&[]), setup.set(&empty_skipped()));
    assert_eq!(session.hook_required_failures(), vec!["fmt".to_owned()]);
}
