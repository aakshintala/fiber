//! A repository's approved servers: what `repository_servers` allows,
//! what it maps, and what it holds.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};

use config::ProjectKey;
use contract::events::OfferedKind;
use fakes::clock::FakeClock;
use serde_json::json;

use super::declared_tests::Repo;
use super::{Index, ServerApproval, hash, repository_servers};
use crate::Error;

const KEY: &str = "-tmp-project-.git";

fn project() -> ProjectKey {
    ProjectKey::new(KEY).unwrap()
}

/// A repository whose server runs `./srv.sh` with one argument.
fn repo() -> Repo {
    let repo = Repo::new();
    repo.write("srv/run.sh", "echo\n");
    repo.config(
        &json!({"mcp": {"servers": {"db": {"command": "./srv/run.sh", "args": ["srv/run.sh"]}}}}),
    );
    repo
}

fn item_hash(repo: &Repo) -> (super::RepoItem, String) {
    let item = repo.item(OfferedKind::McpServer, "db");
    let hash = hash(&mut Index::scratch(), &item).unwrap();
    (item, hash)
}

fn approve(repo: &Repo, name: &str) -> String {
    let item = repo.item(OfferedKind::McpServer, name);
    let hash = hash(&mut Index::scratch(), &item).unwrap();
    super::Store::new(&repo.home(), &project(), FakeClock::new())
        .approve(&item, &hash)
        .unwrap();
    hash
}

fn approved(
    repo: &Repo,
    skipped: &BTreeSet<(String, String)>,
) -> BTreeMap<String, Result<ServerApproval, Error>> {
    repository_servers(
        &repo.home(),
        &project(),
        &repo.root(),
        FakeClock::new(),
        skipped,
    )
    .unwrap()
}

fn empty() -> BTreeSet<(String, String)> {
    BTreeSet::new()
}

#[test]
fn an_approved_server_maps_to_its_copy() {
    let repo = repo();
    let hash = approve(&repo, "db");
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Approved(approved)) = got.remove("db").unwrap() else {
        panic!("db is approved");
    };
    assert_eq!(approved.name, "db");
    assert_eq!(approved.hash, hash);
    let want = repo.home().join("pinned").join(&hash).join("srv/run.sh");
    assert_eq!(approved.arg("./srv/run.sh"), want.display().to_string());
    assert_eq!(approved.arg("srv/run.sh"), want.display().to_string());
    assert!(want.is_file());
    assert_eq!(approved.arg("/bin/bash"), "/bin/bash");
    assert_eq!(approved.arg("node"), "node");
    assert_eq!(approved.arg("--port"), "--port");
}

#[test]
fn an_unapproved_server_is_reported_with_its_hash() {
    let repo = repo();
    let (_, hash) = item_hash(&repo);
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Unapproved {
        hash: seen,
        required,
    }) = got.remove("db").unwrap()
    else {
        panic!("db is unapproved");
    };
    assert_eq!(seen, hash);
    assert!(!required);
}

#[test]
fn a_never_is_unapproved_with_the_repositorys_required() {
    for required in [false, true] {
        let repo = Repo::new();
        repo.write("srv/run.sh", "echo\n");
        repo.config(
            &json!({"mcp": {"servers": {"db": {"command": "./srv/run.sh", "required": required}}}}),
        );
        let item = repo.item(OfferedKind::McpServer, "db");
        let hash = hash(&mut Index::scratch(), &item).unwrap();
        super::Store::new(&repo.home(), &project(), FakeClock::new())
            .never(&item, &hash)
            .unwrap();
        let mut got = approved(&repo, &empty());
        let Ok(ServerApproval::Unapproved {
            hash: seen,
            required: seen_required,
        }) = got.remove("db").unwrap()
        else {
            panic!("db is unapproved");
        };
        assert_eq!(seen, hash);
        assert_eq!(seen_required, required);
    }
}

#[test]
fn a_changed_pinned_file_needs_a_new_approval() {
    let repo = repo();
    approve(&repo, "db");
    repo.write("srv/run.sh", "changed\n");
    let item = repo.item(OfferedKind::McpServer, "db");
    let now = hash(&mut Index::scratch(), &item).unwrap();
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Unapproved { hash: seen, .. }) = got.remove("db").unwrap() else {
        panic!("db is unapproved");
    };
    assert_eq!(seen, now);
}

#[test]
fn a_changed_declaration_needs_a_new_approval() {
    let repo = repo();
    approve(&repo, "db");
    repo.config(
        &json!({"mcp": {"servers": {"db": {"command": "./srv/run.sh", "args": ["srv/run.sh"], "env": {"MODE": "fast"}}}}}),
    );
    let item = repo.item(OfferedKind::McpServer, "db");
    let now = hash(&mut Index::scratch(), &item).unwrap();
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Unapproved { hash: seen, .. }) = got.remove("db").unwrap() else {
        panic!("db is unapproved");
    };
    assert_eq!(seen, now);
}

#[test]
fn a_pruned_copy_is_rebuilt() {
    let repo = repo();
    let hash = approve(&repo, "db");
    fs::remove_dir_all(repo.home().join("pinned").join(&hash)).unwrap();
    fs::remove_file(repo.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Approved(approved)) = got.remove("db").unwrap() else {
        panic!("db is approved again");
    };
    assert!(
        repo.home()
            .join("pinned")
            .join(&hash)
            .join("srv/run.sh")
            .is_file()
    );
    assert_eq!(
        approved.arg("./srv/run.sh"),
        repo.home()
            .join("pinned")
            .join(&hash)
            .join("srv/run.sh")
            .display()
            .to_string()
    );
}

#[test]
fn a_copy_that_cannot_be_made_fails_only_that_server() {
    let repo = Repo::new();
    repo.write("a/run.sh", "a\n");
    repo.write("b/run.sh", "b\n");
    repo.config(&json!({"mcp": {"servers":
        {"a": {"command": "a/run.sh"}, "b": {"command": "b/run.sh"}}}}));
    let hash_a = approve(&repo, "a");
    approve(&repo, "b");
    // Break A's copy: no directory, and a directory where the marker goes,
    // so removing the marker fails and nothing is rebuilt.
    fs::remove_dir_all(repo.home().join("pinned").join(&hash_a)).unwrap();
    fs::remove_file(repo.home().join("pinned").join(format!("{hash_a}.ready"))).unwrap();
    fs::create_dir(repo.home().join("pinned").join(format!("{hash_a}.ready"))).unwrap();
    let mut got = approved(&repo, &empty());
    assert!(got.remove("a").unwrap().is_err());
    let Ok(ServerApproval::Approved(_)) = got.remove("b").unwrap() else {
        panic!("b stays approved");
    };
}

#[test]
fn a_skip_beats_a_broken_copy() {
    let repo = Repo::new();
    repo.write("a/run.sh", "a\n");
    repo.config(&json!({"mcp": {"servers": {"a": {"command": "a/run.sh"}}}}));
    let hash_a = approve(&repo, "a");
    fs::remove_dir_all(repo.home().join("pinned").join(&hash_a)).unwrap();
    fs::remove_file(repo.home().join("pinned").join(format!("{hash_a}.ready"))).unwrap();
    fs::create_dir(repo.home().join("pinned").join(format!("{hash_a}.ready"))).unwrap();
    let skipped = BTreeSet::from([("a".to_owned(), hash_a.clone())]);
    let mut got = approved(&repo, &skipped);
    assert!(matches!(
        got.remove("a").unwrap(),
        Ok(ServerApproval::Skipped)
    ));
    // No copy was attempted: the marker is still the directory the test made.
    assert!(
        repo.home()
            .join("pinned")
            .join(format!("{hash_a}.ready"))
            .is_dir()
    );
}

#[test]
fn a_skipped_approval_holds_no_copy() {
    let repo = repo();
    let hash = approve(&repo, "db");
    let skipped = BTreeSet::from([("db".to_owned(), hash.clone())]);
    let mut got = approved(&repo, &skipped);
    assert!(matches!(
        got.remove("db").unwrap(),
        Ok(ServerApproval::Skipped)
    ));
    // The copy is not held: an exclusive lock on the marker succeeds.
    let ready = File::options()
        .read(true)
        .open(repo.home().join("pinned").join(format!("{hash}.ready")))
        .unwrap();
    assert!(ready.try_lock().is_ok());
}

#[test]
fn an_approval_in_one_repository_covers_another_on_this_machine() {
    let repo = repo();
    approve(&repo, "db");
    // The same declaration and file bytes elsewhere: the hash never covers
    // the repository's location, so no new approval is asked.
    let elsewhere = repo.root().join("other");
    fs::create_dir_all(elsewhere.join(".fiber")).unwrap();
    fs::create_dir_all(elsewhere.join("srv")).unwrap();
    fs::write(
        elsewhere.join(".fiber/config.json"),
        json!({"mcp": {"servers": {"db": {"command": "./srv/run.sh", "args": ["srv/run.sh"]}}}})
            .to_string(),
    )
    .unwrap();
    fs::write(elsewhere.join("srv/run.sh"), "echo\n").unwrap();
    let got = repository_servers(
        &repo.home(),
        &project(),
        &elsewhere,
        FakeClock::new(),
        &empty(),
    )
    .unwrap();
    assert!(matches!(
        got.get("db"),
        Some(Ok(ServerApproval::Approved(_)))
    ));
}

#[test]
fn a_repository_with_no_server_touches_nothing_in_home() {
    let repo = Repo::new();
    let got = approved(&repo, &empty());
    assert!(got.is_empty());
    let entries: Vec<_> = fs::read_dir(repo.home()).unwrap().collect();
    assert!(entries.is_empty());
}

#[test]
fn a_gather_error_is_the_outer_error() {
    let repo = Repo::bare();
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "./srv/run.sh"}}}}));
    let e = repository_servers(
        &repo.home(),
        &project(),
        &repo.root().join("missing"),
        FakeClock::new(),
        &empty(),
    )
    .unwrap_err();
    assert!(matches!(e, Error::Io { .. }), "{e}");
}

#[test]
fn a_guard_ignores_an_existing_directory_argument() {
    let repo = repo();
    approve(&repo, "db");
    repo.write("late/child.txt", "not a program\n");
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Approved(approved)) = got.remove("db").unwrap() else {
        panic!("db is approved");
    };
    let guard = approved.guard(&["late".to_owned()]);
    assert!(guard.check().is_ok());
}

#[test]
fn a_guard_rejects_a_symlink_loop() {
    let repo = repo();
    approve(&repo, "db");
    repo.link("loop", &repo.root().join("loop"));
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Approved(approved)) = got.remove("db").unwrap() else {
        panic!("db is approved");
    };
    let guard = approved.guard(&["loop".to_owned()]);
    let error = guard.check().unwrap_err();
    assert!(
        matches!(&error, Error::Pin { item, why } if item == "db" && why.contains("loop")),
        "{error}"
    );
}

#[test]
fn a_guard_allows_a_regular_file_outside_the_repository() {
    let repo = repo();
    approve(&repo, "db");
    let outside = repo.elsewhere("outside/tool.sh", "tool\n");
    repo.link("external", &outside);
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Approved(approved)) = got.remove("db").unwrap() else {
        panic!("db is approved");
    };
    let guard = approved.guard(&["external".to_owned()]);
    assert!(guard.check().is_ok());
}

#[test]
fn an_approved_server_debug_prints_no_token_or_env_value() {
    let repo = Repo::new();
    repo.write("srv/run.sh", "echo\n");
    repo.config(&json!({"mcp": {"servers": {"db": {
        "command": "./srv/run.sh",
        "args": ["--token=planted2b7e"],
        "env": {"SECRET": "planted9d41"},
    }}}}));
    approve(&repo, "db");
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Approved(approved)) = got.remove("db").unwrap() else {
        panic!("db is approved");
    };
    let printed = format!("{approved:?}");
    assert!(printed.contains("db"), "{printed}");
    assert!(!printed.contains("planted2b7e"), "{printed}");
    assert!(!printed.contains("planted9d41"), "{printed}");
}

#[test]
fn a_guard_debug_prints_no_argument() {
    let repo = repo();
    approve(&repo, "db");
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Approved(approved)) = got.remove("db").unwrap() else {
        panic!("db is approved");
    };
    let guard = approved.guard(&[
        "--token=planted4c1e".to_owned(),
        "planted-path.sh".to_owned(),
    ]);
    let printed = format!("{guard:?}");
    assert!(!printed.contains("planted4c1e"), "{printed}");
    assert!(!printed.contains("planted-path"), "{printed}");
    assert!(printed.contains("db"), "{printed}");
}

#[test]
fn a_retargeted_link_after_gathering_cannot_change_what_runs() {
    let repo = Repo::new();
    let outside = repo.elsewhere("elsewhere/tool.sh", "tool\n");
    repo.link("ext/link", &outside);
    repo.write("ext/evil.sh", "evil\n");
    repo.config(
        &json!({"mcp": {"servers": {"db": {"command": "node", "args": ["ext/link", "late.sh"]}}}}),
    );
    approve(&repo, "db");
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Approved(approved)) = got.remove("db").unwrap() else {
        panic!("db is approved");
    };
    // Retarget the link at the repository's unapproved file after gathering.
    fs::remove_file(repo.root().join("ext/link")).unwrap();
    repo.link("ext/link", &repo.root().join("ext/evil.sh"));
    assert_eq!(
        approved.arg("ext/link"),
        outside.canonicalize().unwrap().display().to_string()
    );
    // A string that named nothing at gathering stays as written, and its
    // guard re-resolves on every call and caches nothing.
    assert_eq!(approved.arg("late.sh"), "late.sh");
    let guard = approved.guard(&[
        "late.sh".to_owned(),
        "ext/link".to_owned(),
        "--port".to_owned(),
    ]);
    assert!(guard.check().is_ok());
    repo.write("late.sh", "late\n");
    let e = guard.check().unwrap_err();
    assert!(
        matches!(&e, Error::Pin { item, why } if item == "db" && why.contains("late.sh")),
        "{e}"
    );
    fs::remove_file(repo.root().join("late.sh")).unwrap();
    assert!(guard.check().is_ok());
    // A missing string outside the repository, a non-path string and a
    // frozen string never fail the guard.
    let open = approved.guard(&[
        "/nonexistent-fiber-599-xyz/abc".to_owned(),
        "--port".to_owned(),
        "ext/link".to_owned(),
    ]);
    assert!(open.check().is_ok());
}

#[test]
fn the_guard_keeps_the_hold_after_the_server_drops() {
    let repo = repo();
    let hash = approve(&repo, "db");
    let mut got = approved(&repo, &empty());
    let Ok(ServerApproval::Approved(approved)) = got.remove("db").unwrap() else {
        panic!("db is approved");
    };
    let guard = approved.guard(&["late.sh".to_owned()]);
    drop(approved);
    let marker = repo.home().join("pinned").join(format!("{hash}.ready"));
    let probe = File::options().read(true).open(&marker).unwrap();
    assert!(probe.try_lock().is_err());
    drop(guard);
    let probe = File::options().read(true).open(&marker).unwrap();
    assert!(probe.try_lock().is_ok());
}
