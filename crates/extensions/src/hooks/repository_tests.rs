//! Session hooks from pinned copies: the filtered configuration, retention
//! over skipped replacements, and the required gate.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
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
        // `fakes::git` runs under `MUST_SUCCEED_WITHIN`, so a stalled git
        // fails naming the wait instead of hanging the runner
        // (`docs/testing.md`, "Waits and timeouts").
        fakes::git::git(&tmp.path().join("workspace"), &["init", "-q"]);
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
        let gathered = set.hooks_config(config);
        let gathered = gathered
            .extensions()
            .merged(&config::full_name("hooks"), &["hooks"]);
        let session = SessionExtensions::load_repository(
            &home,
            config,
            FakeClock::new(),
            Arc::new(fakes::FakeLock::new()),
            None,
            StateSeed::new(),
            set,
            None,
        );
        let filtered = session.hooks_config(config);
        assert_eq!(
            gathered.unwrap(),
            filtered
                .extensions()
                .merged(&config::full_name("hooks"), &["hooks"])
                .unwrap(),
            "gathered and session views must agree before any reload"
        );
        session
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
fn gathered_hooks_keep_personal_names_and_only_approved_repository_names() {
    let setup = Setup::new();
    setup.write("scripts/repo.sh", "repo\n");
    setup.hooks(&json!({
        "approved": {"point":"after_tool", "command":"scripts/repo.sh", "timeout":1000, "on_failure":"blocking"},
        "withheld": {"point":"after_tool", "command":"scripts/repo.sh", "timeout":1000, "on_failure":"blocking"},
        "personal": {"point":"after_tool", "command":"scripts/repo.sh", "timeout":1000, "on_failure":"non-blocking"}
    }));
    setup.approve("approved");
    let person = setup.home().join("config/hooks.json");
    fs::create_dir_all(person.parent().unwrap()).unwrap();
    fs::write(person, json!({"hooks":{"personal":{"point":"after_tool","command":"true","timeout":1000,"on_failure":"blocking"}}}).to_string()).unwrap();
    let config = setup.config(&[
        r#"extensions."github.com/aakshintala/fiber/extensions/hooks".settings.hooks.run={"point":"after_tool","command":"true","timeout":1000,"on_failure":"blocking"}"#,
    ]);
    let set = setup.set(&empty_skipped());
    let filtered = set.hooks_config(&config);
    // Blocker names arrive sorted (`docs/invocation.md:1160`).
    assert_eq!(
        crate::hooks_state(&filtered, &[]).unwrap(),
        crate::HooksState::MissingBlocking {
            names: vec!["approved".into(), "personal".into(), "run".into()]
        }
    );
    let session = setup.load(&config, set);
    assert_eq!(
        crate::hooks_state(&session.hooks_config(&config), &[]).unwrap(),
        crate::hooks_state(&filtered, &[]).unwrap()
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
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    fn run_pinned(path: &str) -> String {
        // Every blocking wait runs under a deadline (`docs/testing.md`,
        // "Waits and timeouts"): the pinned script runs on a thread and the
        // test receives its output with `MUST_SUCCEED_WITHIN`.
        let child = Command::new("/bin/sh")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let output = fakes::finished(
            "run the pinned hook",
            child,
            &[],
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        );
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    let setup = Setup::new();
    setup.write("scripts/v1.sh", "#!/bin/sh\necho v1\n");
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
    let v1_path = v1_command.as_str().unwrap().to_owned();
    assert!(v1_path.contains(&v1_hash));
    // The old approved copy runs.
    assert_eq!(run_pinned(&v1_path), "v1\n");
    // Edit to v2 and observe the replacement offer before skipping it.
    setup.write("scripts/v2.sh", "#!/bin/sh\necho v2\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/v2.sh", "timeout": 1000, "on_failure": "non-blocking"}}));
    let items = declared_items(&setup.workspace(), &*FakeClock::new()).unwrap();
    let v2_item = items
        .into_iter()
        .find(|i| i.kind == OfferedKind::Hook && i.name == "fmt")
        .unwrap();
    let v2_hash = hash(&mut Index::load(&setup.home()), &v2_item).unwrap();
    assert_ne!(v1_hash, v2_hash);
    let store = Store::new(&setup.home(), &project(), FakeClock::new());
    let mut index = Index::load(&setup.home());
    let items = declared_items(&setup.workspace(), &*FakeClock::new()).unwrap();
    let offered = crate::pending(&store, &mut index, items).unwrap();
    assert!(
        offered
            .iter()
            .any(|p| p.offered.name == "fmt" && p.offered.hash == v2_hash),
        "the edited hook is offered as its replacement"
    );
    let mut skipped = BTreeSet::new();
    skipped.insert(("fmt".to_owned(), v2_hash.clone()));
    let reloaded = session.reload_repository(
        &setup.home(),
        &config,
        FakeClock::new(),
        Arc::new(fakes::FakeLock::new()),
        setup.set(&skipped),
        None,
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
    // The old copy still runs across the skip.
    assert_eq!(run_pinned(kept.as_str().unwrap()), "v1\n");
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
        None,
    );
    let now = switched
        .hooks_config(&config)
        .extensions()
        .get(&config::full_name("hooks"), &["hooks"], "hooks.fmt.command")
        .unwrap()
        .unwrap();
    assert!(now.as_str().unwrap().contains(&v2_hash));
    assert_ne!(now, v1_command);
    assert_eq!(run_pinned(now.as_str().unwrap()), "v2\n");
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
    // The lone blocker is named (`docs/invocation.md:1160`).
    assert_eq!(
        crate::hooks_state(&filtered, &[]).unwrap(),
        crate::HooksState::MissingBlocking {
            names: vec!["guard".to_owned()]
        }
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
        None,
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
fn gather_failure_withholds_repository_contribution() {
    // A read/hash failure on a named file fails the whole hook gather: the
    // set marks it failed and the session withholds the repository layer
    // instead of passing it raw (`docs/extensions.md`, "Hooks declared in
    // configuration": unapproved entries are withheld). Flipping either
    // `hooks_failed` assignment or the `has_repository_hooks` disjunct lets
    // the raw repository entry through and fails this test.
    let setup = Setup::new();
    setup.write("scripts/run.sh", "#!/bin/sh\necho hi\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/run.sh", "timeout": 1000, "on_failure": "non-blocking"}}));
    let digest = setup.approve("fmt");
    // Rewrite the named file with different bytes and deny reads: the index
    // cache misses on size/mtime, the open fails, hashing fails, and the whole
    // hook gather fails even though the declaration itself still reads.
    // (`resolve` still sees the file via metadata, so gathering reaches the
    // hash; a deleted file would instead read as unapproved new content.)
    setup.write(
        "scripts/run.sh",
        "#!/bin/sh\necho changed to miss the index cache\n",
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let file = setup.workspace().join("scripts/run.sh");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            fs::File::open(&file).is_err(),
            "the gather source cannot be read"
        );
    }
    let set = setup.set(&empty_skipped());
    assert!(set.hooks_failed, "a hash failure marks the gather failed");
    assert!(set.hooks.is_empty());
    assert!(set.hook_failures.is_empty());
    let config = setup.config(&[]);
    // The raw repository layer still declares `fmt`.
    let (raw, _) = config
        .extensions()
        .merged(&config::full_name("hooks"), &["hooks"])
        .unwrap();
    assert!(raw.get("hooks").and_then(|v| v.get("fmt")).is_some());
    let session = setup.load(&config, set);
    let filtered = session.hooks_config(&config);
    let (merged, _) = filtered
        .extensions()
        .merged(&config::full_name("hooks"), &["hooks"])
        .unwrap();
    assert!(
        merged.get("hooks").and_then(|v| v.get("fmt")).is_none(),
        "the failed gather withholds the repository entry: {merged:?}"
    );
    // The approval record is untouched: the digest still names the approved
    // content.
    assert!(!digest.is_empty());
}

#[test]
fn failed_gather_retains_running_snapshot_and_hold() {
    // The old approved copy keeps running when a replacement cannot even be
    // gathered: only an approved and materialized replacement swaps it
    // (`docs/extensions.md`, "Hooks declared in configuration"). Removing the
    // `hooks_failed` disjunct in `reload_repository` drops the old snapshot
    // and fails this test; removing the hold push loses the hold.
    let setup = Setup::new();
    setup.write("scripts/v1.sh", "#!/bin/sh\necho v1\n");
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
    let holds_before = session.holds.len();
    assert!(!session.hook_snapshots.is_empty());
    // Rewrite the named file with different bytes and deny reads, as above,
    // so the next gather fails entirely instead of reading as new content.
    setup.write("scripts/v1.sh", "#!/bin/sh\necho changed\n");
    {
        use std::os::unix::fs::PermissionsExt;
        let file = setup.workspace().join("scripts/v1.sh");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            fs::File::open(&file).is_err(),
            "the gather source cannot be read"
        );
    }
    let failed = setup.set(&empty_skipped());
    assert!(failed.hooks_failed);
    let reloaded = session.reload_repository(
        &setup.home(),
        &config,
        FakeClock::new(),
        Arc::new(fakes::FakeLock::new()),
        failed,
        None,
    );
    let kept = reloaded
        .hooks_config(&config)
        .extensions()
        .get(&config::full_name("hooks"), &["hooks"], "hooks.fmt.command")
        .unwrap()
        .unwrap();
    assert_eq!(kept, v1_command, "a failed gather keeps the old snapshot");
    assert!(
        reloaded.hook_snapshots.contains_key("fmt"),
        "the old snapshot survives the failed gather"
    );
    assert!(
        reloaded.holds.len() >= holds_before,
        "the old copy stays held: {} -> {}",
        holds_before,
        reloaded.holds.len()
    );
}

#[test]
fn approved_replacement_copy_failure_retains_running_snapshot_and_hold() {
    // Only an approved and materialized replacement swaps the running
    // snapshot (`docs/extensions.md`, "Hooks declared in configuration"):
    // when the approved replacement's copy cannot be made, the failure is
    // classified in `hook_failures` and the old snapshot and hold survive.
    // Removing the `hook_failures` disjunct in `reload_repository` drops
    // the old snapshot and fails this test; removing the hold push loses
    // the hold.
    let setup = Setup::new();
    setup.write("scripts/v1.sh", "#!/bin/sh\necho v1\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/v1.sh", "timeout": 1000, "on_failure": "non-blocking", "required": true}}));
    let v1_hash = setup.approve("fmt");
    let config = setup.config(&[]);
    let session = setup.load(&config, setup.set(&empty_skipped()));
    let v1_command = session
        .hooks_config(&config)
        .extensions()
        .get(&config::full_name("hooks"), &["hooks"], "hooks.fmt.command")
        .unwrap()
        .unwrap();
    let v1_path = v1_command.as_str().unwrap().to_owned();
    assert!(v1_path.contains(&v1_hash));
    let holds_before = session.holds.len();
    assert!(!session.hook_snapshots.is_empty());
    // Approve the replacement, then break its copy: the gather still
    // succeeds, so the failure lands in `hook_failures`, never the gather
    // flag.
    setup.write("scripts/v2.sh", "#!/bin/sh\necho v2\n");
    setup.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/v2.sh", "timeout": 1000, "on_failure": "non-blocking", "required": true}}));
    let v2_hash = setup.approve("fmt");
    assert_ne!(v1_hash, v2_hash);
    fs::remove_dir_all(setup.home().join("pinned").join(&v2_hash)).unwrap();
    fs::remove_file(setup.home().join("pinned").join(format!("{v2_hash}.ready"))).unwrap();
    fs::create_dir(setup.home().join("pinned").join(format!("{v2_hash}.ready"))).unwrap();
    let failed = setup.set(&empty_skipped());
    assert!(
        !failed.hooks_failed,
        "the gather succeeds: the copy failure is classified, not gathered"
    );
    assert!(
        failed
            .hook_failures
            .iter()
            .any(|f| f.name == "fmt" && f.required),
        "the approved replacement's copy failure: {:?}",
        failed
            .hook_failures
            .iter()
            .map(|f| &f.name)
            .collect::<Vec<_>>()
    );
    assert!(
        !failed.hooks.contains_key("fmt"),
        "no approved replacement materialized"
    );
    let reloaded = session.reload_repository(
        &setup.home(),
        &config,
        FakeClock::new(),
        Arc::new(fakes::FakeLock::new()),
        failed,
        None,
    );
    // The required gate still reports the copy failure ...
    let (name, _) = reloaded
        .first_hook_required_failure()
        .expect("the required copy failure");
    assert_eq!(name, "fmt");
    // ... but the old snapshot and hold survive it.
    let kept = reloaded
        .hooks_config(&config)
        .extensions()
        .get(&config::full_name("hooks"), &["hooks"], "hooks.fmt.command")
        .unwrap()
        .unwrap();
    assert_eq!(
        kept, v1_command,
        "a failed replacement keeps the old snapshot"
    );
    assert!(
        reloaded.hook_snapshots.contains_key("fmt"),
        "the old snapshot survives the failed replacement"
    );
    assert!(
        reloaded.holds.len() >= holds_before,
        "the old copy stays held: {} -> {}",
        holds_before,
        reloaded.holds.len()
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
    // The repository marks `fmt` `required`, so its copy failure is what the
    // required gate fails the start with (`docs/extensions.md`, "Hooks
    // declared in configuration": a `required` copy failure stops the start).
    // The message is the copy failure itself, never empty: flipping it to
    // empty fails this test.
    let (name, detail) = session
        .first_hook_required_failure()
        .expect("the required copy failure");
    assert_eq!(name, "fmt");
    assert!(
        !detail.is_empty(),
        "the failure names its cause: {detail:?}"
    );
    assert!(
        detail.contains("fmt") || !detail.contains("it did not start"),
        "the failure carries the copy error: {detail:?}"
    );
}
