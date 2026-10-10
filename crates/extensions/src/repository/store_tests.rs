//! Approvals and pinned copies: where they land, what they hold, and what a
//! failure leaves behind.

use std::fs::{self, File};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, UNIX_EPOCH};

use config::ProjectKey;
use contract::clock::Clock as _;
use contract::events::OfferedKind;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::declared_tests::Repo;
use super::{Decision, Index, RepoItem, Store, hash};
use crate::Error;
use crate::host::exec::{GRACE, GROUP_POLL};
use crate::prepare::INSTALL_STEP_DEADLINE;

const KEY: &str = "-tmp-project-.git";

fn store(repo: &Repo) -> Store {
    Store::new(
        &repo.home(),
        &ProjectKey::new(KEY).unwrap(),
        fakes::clock::FakeClock::new(),
    )
}

fn hashed(item: &RepoItem) -> String {
    hash(&mut Index::scratch(), item).unwrap()
}

fn hook(repo: &Repo) -> RepoItem {
    repo.write("scripts/fmt.sh", "cargo fmt\n");
    repo.executable("scripts/fmt.sh");
    repo.hooks(
        &json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh", "timeout": 30000}}),
    );
    repo.item(OfferedKind::Hook, "fmt")
}

fn server(repo: &Repo) -> RepoItem {
    repo.write("srv/run.js", "run\n");
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "node", "args": ["srv/run.js"]}}}}));
    repo.item(OfferedKind::McpServer, "db")
}

fn extension(repo: &Repo, extra: &Value) -> RepoItem {
    repo.package("pkg", "fiber.test/p", extra);
    repo.write(".gitignore", "node_modules/\n");
    repo.write("pkg/node_modules/dep/index.js", "ignored");
    repo.config(&json!({"repository_extensions": [{"path": "pkg"}]}));
    repo.item(OfferedKind::Extension, "fiber.test/p")
}

/// The copy directories in `pinned/`, sorted; the lock file and the
/// `.ready` markers are not copies.
fn pinned_names(repo: &Repo) -> Vec<String> {
    let Ok(entries) = fs::read_dir(repo.home().join("pinned")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Every entry in `pinned/`, files and directories, sorted.
fn pinned_entries(repo: &Repo) -> Vec<String> {
    let Ok(entries) = fs::read_dir(repo.home().join("pinned")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn approvals_dir(repo: &Repo, project: bool) -> PathBuf {
    if project {
        repo.home().join("projects").join(KEY).join("approvals")
    } else {
        repo.home().join("approvals")
    }
}

/// The `.ready` marker beside a copy in `pinned/`.
fn ready(repo: &Repo, hash: &str) -> PathBuf {
    repo.home().join("pinned").join(format!("{hash}.ready"))
}

#[test]
fn an_extension_copies_its_listed_files_and_runs_its_install_step_in_the_copy() {
    let repo = Repo::new();
    let item = extension(
        &repo,
        &json!({"install": ["sh", "-c", "pwd > where.txt; mkdir node_modules; echo built > node_modules/built"]}),
    );
    let hash = hashed(&item);
    store(&repo).approve(&item, &hash).unwrap();
    let copy = repo.home().join("pinned").join(&hash);
    assert_eq!(
        fs::read_to_string(copy.join("init.lua")).unwrap(),
        "-- entry\n"
    );
    assert!(copy.join("extension.json").is_file());
    // What the step built is there; what git ignores in the repository is not
    // copied.
    assert_eq!(
        fs::read_to_string(copy.join("node_modules/built")).unwrap(),
        "built\n"
    );
    assert!(!copy.join("node_modules/dep").exists());
    assert!(copy.join("where.txt").is_file());
    // The repository is untouched.
    assert!(!repo.root().join("pkg/where.txt").exists());
    assert!(!repo.root().join("pkg/node_modules/built").exists());
    assert_eq!(pinned_names(&repo), std::slice::from_ref(&hash));
    assert_eq!(
        store(&repo).decision(OfferedKind::Extension, &hash),
        Some(Decision::Approve)
    );
}

#[test]
fn approvals_land_per_project_for_extensions_and_hooks_and_per_machine_for_servers() {
    let repo = Repo::new();
    let items = [
        (extension(&repo, &json!({})), true),
        (hook(&repo), true),
        (server(&repo), false),
    ];
    for (item, project) in items {
        let hash = hashed(&item);
        store(&repo).approve(&item, &hash).unwrap();
        assert!(
            approvals_dir(&repo, project).join(&hash).is_file(),
            "{:?}",
            item.kind
        );
        assert!(
            !approvals_dir(&repo, !project).join(&hash).exists(),
            "{:?}",
            item.kind
        );
        let other = ProjectKey::new("-another").unwrap();
        let elsewhere = Store::new(&repo.home(), &other, fakes::clock::FakeClock::new())
            .decision(item.kind, &hash);
        assert_eq!(elsewhere.is_some(), !project, "{:?}", item.kind);
    }
}

#[test]
fn a_hook_or_server_copies_the_named_files_at_their_repository_paths() {
    let repo = Repo::new();
    for item in [hook(&repo), server(&repo)] {
        let hash = hashed(&item);
        store(&repo).approve(&item, &hash).unwrap();
        let copy = repo.home().join("pinned").join(&hash);
        for file in &item.files {
            assert_eq!(
                fs::read(copy.join(&file.rel)).unwrap(),
                fs::read(repo.root().join(&file.rel)).unwrap()
            );
        }
        let count = walk(&copy);
        assert_eq!(count, item.files.len(), "only the named files");
    }
}

fn walk(dir: &std::path::Path) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            if path.is_dir() { walk(&path) } else { 1 }
        })
        .sum()
}

#[test]
fn a_copy_keeps_the_mode_of_a_script() {
    let repo = Repo::new();
    let item = hook(&repo);
    let hash = hashed(&item);
    store(&repo).approve(&item, &hash).unwrap();
    let mode = fs::metadata(
        repo.home()
            .join("pinned")
            .join(&hash)
            .join("scripts/fmt.sh"),
    )
    .unwrap()
    .permissions()
    .mode();
    assert_eq!(mode & 0o111, 0o111);
}

#[test]
fn a_decision_file_is_one_json_line_naming_the_decision_kind_name_and_declaration() {
    let repo = Repo::new();
    repo.write("scripts/fmt.sh", "x");
    repo.hooks(&json!({"fmt": {"point": "after_tool", "command": "cargo", "args": ["fmt"], "timeout": 30000, "on_failure": "non-blocking"}}));
    let item = repo.item(OfferedKind::Hook, "fmt");
    let hash = hashed(&item);
    store(&repo).approve(&item, &hash).unwrap();
    let text = fs::read_to_string(approvals_dir(&repo, true).join(&hash)).unwrap();
    assert!(text.ends_with('\n'), "{text}");
    // The key order is the serializer's, so the fields are asserted parsed,
    // not as text.
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        body.get("decision").and_then(Value::as_str),
        Some("approve")
    );
    assert_eq!(body.get("kind").and_then(Value::as_str), Some("hook"));
    assert_eq!(body.get("name").and_then(Value::as_str), Some("fmt"));
    assert_eq!(
        body.get("files").and_then(|f| f.as_array()).map(Vec::len),
        Some(0)
    );
    assert_eq!(body.get("declaration"), item.declaration.as_ref());
    // The repository's text reaches no path: the name is only in the body.
    assert!(fs::read_dir(approvals_dir(&repo, true)).unwrap().all(|e| {
        let name = e.unwrap().file_name().to_string_lossy().into_owned();
        name == hash
    }));
}

#[test]
fn a_failing_install_step_records_nothing_and_leaves_no_copy() {
    let repo = Repo::new();
    let good = extension(&repo, &json!({}));
    let good_hash = hashed(&good);
    store(&repo).approve(&good, &good_hash).unwrap();

    let bad = extension(
        &repo,
        &json!({"install": ["sh", "-c", "echo broken >&2; exit 3"]}),
    );
    let bad_hash = hashed(&bad);
    assert_ne!(bad_hash, good_hash);
    let e = store(&repo).approve(&bad, &bad_hash).unwrap_err();
    assert!(
        matches!(&e, Error::InstallExited { name, .. } if name == "fiber.test/p"),
        "{e}"
    );
    assert!(e.to_string().contains("broken"), "{e}");
    assert_eq!(
        store(&repo).decision(OfferedKind::Extension, &bad_hash),
        None
    );
    // The earlier approval and its copy stay, and no scratch is left.
    assert_eq!(
        store(&repo).decision(OfferedKind::Extension, &good_hash),
        Some(Decision::Approve)
    );
    assert_eq!(pinned_names(&repo), [good_hash]);
    assert!(!ready(&repo, &bad_hash).exists());
}

#[test]
fn an_install_step_that_cannot_start_records_nothing() {
    let repo = Repo::new();
    let item = extension(&repo, &json!({"install": ["no-such-program-fiber"]}));
    let hash = hashed(&item);
    let e = store(&repo).approve(&item, &hash).unwrap_err();
    assert!(matches!(e, Error::InstallStep { .. }), "{e}");
    assert!(pinned_names(&repo).is_empty());
    assert!(!ready(&repo, &hash).exists());
    assert!(!approvals_dir(&repo, true).join(&hash).exists());
}

#[test]
fn never_records_the_decision_and_makes_no_copy() {
    let repo = Repo::new();
    let item = hook(&repo);
    let hash = hashed(&item);
    store(&repo).never(&item, &hash).unwrap();
    assert_eq!(
        store(&repo).decision(OfferedKind::Hook, &hash),
        Some(Decision::Never)
    );
    assert!(pinned_names(&repo).is_empty());
}

#[test]
fn a_later_decision_replaces_an_earlier_one() {
    let repo = Repo::new();
    let item = hook(&repo);
    let hash = hashed(&item);
    let store = store(&repo);
    store.approve(&item, &hash).unwrap();
    store.never(&item, &hash).unwrap();
    assert_eq!(
        store.decision(OfferedKind::Hook, &hash),
        Some(Decision::Never)
    );
    store.approve(&item, &hash).unwrap();
    assert_eq!(
        store.decision(OfferedKind::Hook, &hash),
        Some(Decision::Approve)
    );
    assert_eq!(pinned_names(&repo), [hash]);
}

#[test]
fn the_same_content_approved_twice_is_one_copy() {
    let repo = Repo::new();
    let item = extension(
        &repo,
        &json!({"install": ["sh", "-c", "echo x >> counter"]}),
    );
    let hash = hashed(&item);
    let store = store(&repo);
    store.approve(&item, &hash).unwrap();
    store.approve(&item, &hash).unwrap();
    assert_eq!(pinned_names(&repo), std::slice::from_ref(&hash));
    // The second approval did not run the step again.
    let counter =
        fs::read_to_string(repo.home().join("pinned").join(&hash).join("counter")).unwrap();
    assert_eq!(counter, "x\n");
}

#[test]
fn an_approval_without_its_copy_is_given_a_copy_again() {
    let repo = Repo::new();
    let item = hook(&repo);
    let hash = hashed(&item);
    let store = store(&repo);
    store.approve(&item, &hash).unwrap();
    fs::remove_dir_all(repo.home().join("pinned")).unwrap();
    store.approve(&item, &hash).unwrap();
    assert_eq!(pinned_names(&repo), [hash]);
}

#[test]
fn a_file_changed_between_hashing_and_copying_fails_with_the_mismatch() {
    let repo = Repo::new();
    let item = server(&repo);
    let hash = hashed(&item);
    fs::write(repo.root().join("srv/run.js"), "different\n").unwrap();
    let e = store(&repo).approve(&item, &hash).unwrap_err();
    assert!(
        matches!(&e, Error::ChangedWhileCopying { item } if item == "db"),
        "{e}"
    );
    assert!(pinned_names(&repo).is_empty());
    assert!(!ready(&repo, &hash).exists());
    assert!(!approvals_dir(&repo, false).join(&hash).exists());
}

#[test]
fn a_hash_that_is_not_hex_names_no_path() {
    let repo = Repo::new();
    let item = hook(&repo);
    let store = store(&repo);
    for bad in ["", "../x", "abc", &"A".repeat(64), &"g".repeat(64)] {
        assert!(store.approve(&item, bad).is_err(), "{bad}");
        assert!(store.never(&item, bad).is_err(), "{bad}");
        assert_eq!(store.decision(OfferedKind::Hook, bad), None);
    }
    assert!(!repo.home().join("pinned").exists());
}

#[test]
fn a_decision_that_is_missing_or_unreadable_is_none() {
    let repo = Repo::new();
    let hash = "a".repeat(64);
    let store = store(&repo);
    assert_eq!(store.decision(OfferedKind::Hook, &hash), None);
    let dir = approvals_dir(&repo, true);
    fs::create_dir_all(&dir).unwrap();
    for text in [
        "",
        "nonsense",
        r#"{"decision": "maybe"}"#,
        r#"{"decision": 5}"#,
        "{}",
    ] {
        fs::write(dir.join(&hash), text).unwrap();
        assert_eq!(store.decision(OfferedKind::Hook, &hash), None, "{text}");
    }
}

fn stamp(repo: &Repo, project: bool, hash: &str, seconds: u64) {
    let file = File::options()
        .write(true)
        .open(approvals_dir(repo, project).join(hash))
        .unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

#[test]
fn the_previous_version_is_the_newest_other_approval_of_the_same_item_with_a_copy() {
    let repo = Repo::new();
    let store = store(&repo);
    let mut hashes = Vec::new();
    for (n, body) in ["one\n", "two\n", "three\n"].into_iter().enumerate() {
        repo.write("scripts/fmt.sh", body);
        repo.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh"}}));
        let item = repo.item(OfferedKind::Hook, "fmt");
        let hash = hashed(&item);
        store.approve(&item, &hash).unwrap();
        stamp(&repo, true, &hash, 1000 + n as u64);
        hashes.push(hash);
    }
    // Another hook, and a never, are not versions of `fmt`.
    repo.hooks(&json!({"other": {"point": "after_tool", "command": "scripts/fmt.sh"}}));
    let other = repo.item(OfferedKind::Hook, "other");
    let other_hash = hashed(&other);
    store.approve(&other, &other_hash).unwrap();
    stamp(&repo, true, &other_hash, 9000);

    let kind = OfferedKind::Hook;
    let newest = store.previous(kind, "fmt", "new").unwrap();
    assert_eq!(newest.hash, hashes[2]);
    assert_eq!(newest.files, ["scripts/fmt.sh"]);
    assert_eq!(
        store.previous(kind, "fmt", &hashes[2]).unwrap().hash,
        hashes[1]
    );
    // Its copy is gone, so it is not a version to compare with.
    fs::remove_dir_all(repo.home().join("pinned").join(&hashes[2])).unwrap();
    assert_eq!(store.previous(kind, "fmt", "new").unwrap().hash, hashes[1]);
    // A never is not a version.
    repo.write("scripts/fmt.sh", "two\n");
    repo.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh"}}));
    let item = repo.item(OfferedKind::Hook, "fmt");
    store.never(&item, &hashes[1]).unwrap();
    assert_eq!(store.previous(kind, "fmt", "new").unwrap().hash, hashes[0]);
    assert!(store.previous(kind, "none", "new").is_none());
    assert!(
        store
            .previous(OfferedKind::Extension, "fmt", "new")
            .is_none()
    );
}

#[test]
fn a_declaration_is_kept_with_the_previous_version() {
    let repo = Repo::new();
    let item = hook(&repo);
    let hash = hashed(&item);
    store(&repo).approve(&item, &hash).unwrap();
    let previous = store(&repo)
        .previous(OfferedKind::Hook, "fmt", "new")
        .unwrap();
    assert_eq!(previous.declaration, item.declaration);
}

#[test]
fn an_install_step_runs_at_the_pinned_path_and_leaves_a_ready_marker() {
    let repo = Repo::new();
    let item = extension(&repo, &json!({"install": ["sh", "-c", "pwd > where.txt"]}));
    let hash = hashed(&item);
    store(&repo).approve(&item, &hash).unwrap();
    let copy = repo.home().join("pinned").join(&hash);
    let canonical = fs::canonicalize(&copy).unwrap();
    assert_eq!(
        fs::read_to_string(copy.join("where.txt")).unwrap(),
        format!("{}\n", canonical.display()),
    );
    assert!(ready(&repo, &hash).is_file());
    // The step ran in no scratch directory: the copy, its marker and the
    // lock are everything in `pinned/`.
    assert_eq!(
        pinned_entries(&repo),
        [".lock", &hash, &format!("{hash}.ready")]
    );
}

#[test]
fn a_copy_without_a_ready_marker_is_cleared_and_built_again() {
    let repo = Repo::new();
    let item = extension(
        &repo,
        &json!({"install": ["sh", "-c", "echo x >> counter"]}),
    );
    let hash = hashed(&item);
    let dir = repo.home().join("pinned").join(&hash);
    store(&repo).approve(&item, &hash).unwrap();
    // A kill before the marker was written: the copy is unfinished, with
    // junk in it.
    fs::remove_file(ready(&repo, &hash)).unwrap();
    fs::write(dir.join("junk"), "junk").unwrap();
    store(&repo).approve(&item, &hash).unwrap();
    assert!(!dir.join("junk").exists());
    assert_eq!(fs::read_to_string(dir.join("counter")).unwrap(), "x\n");
    assert!(ready(&repo, &hash).is_file());
}

#[test]
fn a_copy_with_a_ready_marker_is_kept() {
    let repo = Repo::new();
    let item = extension(
        &repo,
        &json!({"install": ["sh", "-c", "echo x >> counter"]}),
    );
    let hash = hashed(&item);
    let dir = repo.home().join("pinned").join(&hash);
    store(&repo).approve(&item, &hash).unwrap();
    fs::write(dir.join("junk"), "junk").unwrap();
    store(&repo).approve(&item, &hash).unwrap();
    // The copy was finished, so it stays untouched: the junk is still
    // there and the step did not run again.
    assert_eq!(fs::read_to_string(dir.join("junk")).unwrap(), "junk");
    assert_eq!(fs::read_to_string(dir.join("counter")).unwrap(), "x\n");
}

#[test]
fn a_failing_install_step_leaves_no_copy_and_no_ready_marker() {
    let repo = Repo::new();
    let item = extension(
        &repo,
        &json!({"install": ["sh", "-c", "mkdir built; exit 3"]}),
    );
    let hash = hashed(&item);
    store(&repo).approve(&item, &hash).unwrap_err();
    // The step built into the copy before it failed, and all of it went.
    assert!(!repo.home().join("pinned").join(&hash).exists());
    assert!(!ready(&repo, &hash).exists());
    assert!(pinned_names(&repo).is_empty());
}

#[test]
fn a_rebuild_that_fails_clears_the_stale_ready_marker() {
    let repo = Repo::new();
    let flag = repo.home().join("fail");
    let step = format!(
        "test ! -e '{}' || {{ echo broken >&2; exit 3; }}; echo x >> counter",
        flag.display()
    );
    let item = extension(&repo, &json!({"install": ["sh", "-c", step]}));
    let hash = hashed(&item);
    store(&repo).approve(&item, &hash).unwrap();
    // The copy is gone but its marker is left, as when a copy is deleted
    // outside an approval.
    fs::remove_dir_all(repo.home().join("pinned").join(&hash)).unwrap();
    assert!(ready(&repo, &hash).is_file());
    // The rebuild runs the step again and fails: the stale marker goes
    // with it, so no partial directory is left beside a marker the next
    // approval would accept.
    fs::write(&flag, "fail").unwrap();
    let e = store(&repo).approve(&item, &hash).unwrap_err();
    assert!(
        matches!(&e, Error::InstallExited { name, .. } if name == "fiber.test/p"),
        "{e}"
    );
    assert!(!repo.home().join("pinned").join(&hash).exists());
    assert!(!ready(&repo, &hash).exists());
    assert!(pinned_names(&repo).is_empty());
    // With the failure gone, the next approval builds the copy again.
    fs::remove_file(&flag).unwrap();
    store(&repo).approve(&item, &hash).unwrap();
    let copy = repo.home().join("pinned").join(&hash);
    assert_eq!(fs::read_to_string(copy.join("counter")).unwrap(), "x\n");
    assert!(ready(&repo, &hash).is_file());
}

#[test]
fn scratch_and_temporary_names_never_repeat() {
    let (a, b) = (super::store::next(), super::store::next());
    assert_ne!(a, b);
    let c = super::store::next();
    assert_ne!(b, c);
    assert_ne!(a, c);
}

#[test]
fn a_ready_marker_that_cannot_be_removed_fails_before_the_install_step_runs() {
    let repo = Repo::new();
    let item = extension(&repo, &json!({"install": ["sh", "-c", "touch ../ran"]}));
    let hash = hashed(&item);
    let marker = ready(&repo, &hash);
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::create_dir(&marker).unwrap();
    let e = store(&repo).approve(&item, &hash).unwrap_err();
    assert!(
        matches!(&e, Error::Io { path, .. } if path == &marker),
        "{e}"
    );
    assert!(e.to_string().contains(&marker.display().to_string()), "{e}");
    // The step never ran: its side effect outside the copy is absent. A
    // build that ignored the removal failure would run the step before
    // failing at the marker, leaving this behind.
    assert!(!repo.home().join("pinned").join("ran").exists());
}

#[test]
fn a_stray_file_where_the_copy_goes_is_replaced_by_the_copy() {
    let repo = Repo::new();
    let item = extension(&repo, &json!({}));
    let hash = hashed(&item);
    let copy = repo.home().join("pinned").join(&hash);
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    fs::write(&copy, "stray").unwrap();
    assert!(!ready(&repo, &hash).exists());
    store(&repo).approve(&item, &hash).unwrap();
    assert!(fs::symlink_metadata(&copy).unwrap().file_type().is_dir());
    assert!(copy.join("init.lua").is_file());
    assert!(copy.join("extension.json").is_file());
    assert!(ready(&repo, &hash).is_file());
}

#[test]
fn a_stray_symlink_where_the_copy_goes_is_replaced_by_the_copy() {
    let repo = Repo::new();
    let item = extension(&repo, &json!({}));
    let hash = hashed(&item);
    let copy = repo.home().join("pinned").join(&hash);
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    symlink("nowhere-fiber-test", &copy).unwrap();
    assert!(!ready(&repo, &hash).exists());
    store(&repo).approve(&item, &hash).unwrap();
    assert!(fs::symlink_metadata(&copy).unwrap().file_type().is_dir());
    assert!(copy.join("init.lua").is_file());
    assert!(copy.join("extension.json").is_file());
    assert!(ready(&repo, &hash).is_file());
}

/// An install step that never finishes approves nothing and leaves no copy:
/// the approval is recorded only after the copy builds.
#[test]
fn a_stalled_install_step_approves_nothing_and_leaves_no_copy() {
    const WITHIN: Duration = fakes::MUST_SUCCEED_WITHIN;

    let repo = Repo::new();
    let ready = fakes::children::Ready::new(&repo.home());
    // The step ignores SIGTERM, so the stop runs the full grace to SIGKILL.
    // Its pid line proves the trap is set before the clock moves.
    let script = format!(
        "trap '' TERM\necho $$ > '{}'\nwhile :; do :; done\n",
        ready.path().display()
    );
    let item = extension(&repo, &json!({"install": ["sh", "-c", script]}));
    let hash = hashed(&item);
    let kind = item.kind;
    let worker_hash = hash.clone();
    let clock = FakeClock::new();
    let home = repo.home();
    let project = ProjectKey::new(KEY).unwrap();
    let worker_clock = Arc::clone(&clock);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("stalled approve".into())
        .spawn(move || {
            let store = Store::new(&home, &project, worker_clock);
            let _sent = done_tx.send(store.approve(&item, &worker_hash));
        })
        .unwrap();
    let pid = ready.wait(WITHIN)[0];
    // The spinner shares this test's group: match its argv by pid alone,
    // so a panic anywhere below still kills it.
    let watchdog = fakes::Watchdog::matching(&ready.path().display().to_string());
    assert!(
        clock.await_parked(clock.now() + GROUP_POLL, WITHIN),
        "waited {WITHIN:?} for the step to park while running"
    );
    // Past the install-step deadline the run stops.
    clock.advance(INSTALL_STEP_DEADLINE + Duration::from_secs(1));
    let kill_at = clock.now() + GRACE;
    let answered = crate::stall::await_grace_or_answer(&clock, &done_rx, kill_at);
    if answered.is_none() {
        clock.advance(GRACE);
    }
    let answer = match answered {
        Some(answer) => answer,
        None => done_rx
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("waited {WITHIN:?} for the stalled approval")),
    };
    let err = answer.expect_err("a stalled step approves nothing");
    assert!(
        matches!(err, Error::InstallExited { .. }),
        "a stalled step fails as its install step failed: {err}"
    );
    assert!(
        pinned_names(&repo).is_empty(),
        "a failed step leaves no copy"
    );
    assert!(
        store(&repo).decision(kind, &hash).is_none(),
        "a failed step records no approval"
    );
    assert!(
        !fakes::kill_pid(pid, "0").expect("a pid probe runs"),
        "the stopped step is gone"
    );
    watchdog.stand_down(WITHIN);
}

/// The `Debug` names the store and every field it prints: a body replaced
/// with an empty `Ok` (the surviving mutant) prints none of them.
#[test]
fn store_debug_names_the_type_and_its_fields() {
    let repo = Repo::bare();
    let text = format!("{:?}", store(&repo));
    assert!(text.contains("Store"), "{text}");
    assert!(text.contains("home"), "{text}");
    assert!(text.contains("project"), "{text}");
    // The clock is skipped: `Arc<dyn Clock>` has no `Debug`.
    assert!(!text.contains("clock"), "{text}");
}
