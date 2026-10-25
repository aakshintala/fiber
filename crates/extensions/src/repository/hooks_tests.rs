//! A repository's approved hooks: what `repository_hooks` allows, what it
//! maps, and what it holds.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;

use contract::events::OfferedKind;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::declared_tests::Repo;
use super::hooks::repository_hooks;
use super::{HookState, Index, hash};

const KEY: &str = "-tmp-project-.git";

fn project() -> config::ProjectKey {
    config::ProjectKey::new(KEY).unwrap()
}

fn empty() -> BTreeSet<(String, String)> {
    BTreeSet::new()
}

fn approve(repo: &Repo, name: &str) -> String {
    let item = repo.item(OfferedKind::Hook, name);
    let hash = hash(&mut Index::scratch(), &item).unwrap();
    super::Store::new(&repo.home(), &project(), FakeClock::new())
        .approve(&item, &hash)
        .unwrap();
    hash
}

fn approved_map(
    repo: &Repo,
    skipped: &BTreeSet<(String, String)>,
) -> std::collections::BTreeMap<String, Result<HookState, super::HookFailure>> {
    let (map, _holds) = repository_hooks(
        &repo.home(),
        &project(),
        &repo.root(),
        FakeClock::new(),
        skipped,
    )
    .unwrap();
    map
}

#[test]
fn inside_command_and_args_map_to_pinned_paths() {
    let repo = Repo::new();
    repo.write("scripts/fmt.sh", "cargo fmt\n");
    repo.write("scripts/args.txt", "args\n");
    repo.hooks(&json!({"fmt": {
        "point": "after_tool",
        "command": "scripts/fmt.sh",
        "args": ["scripts/args.txt"],
        "timeout": 1000,
        "on_failure": "non-blocking",
    }}));
    let hash = approve(&repo, "fmt");
    let mut got = approved_map(&repo, &empty());
    let Ok(HookState::Approved(approved)) = got.remove("fmt").unwrap() else {
        panic!("fmt is approved");
    };
    assert_eq!(approved.hash, hash);
    let want_cmd = repo
        .home()
        .join("pinned")
        .join(&hash)
        .join("scripts/fmt.sh");
    let want_arg = repo
        .home()
        .join("pinned")
        .join(&hash)
        .join("scripts/args.txt");
    assert!(want_cmd.is_file());
    let entry = approved.entry.as_object().unwrap();
    assert_eq!(
        entry.get("command").and_then(Value::as_str).unwrap(),
        want_cmd.display().to_string()
    );
    let args = entry.get("args").and_then(Value::as_array).unwrap();
    assert_eq!(args[0].as_str().unwrap(), want_arg.display().to_string());
}

#[test]
fn outside_strings_stay_exact_and_bare_names_stay() {
    let repo = Repo::new();
    let outside = repo.elsewhere("secret.sh", "secret\n");
    let outside_str = outside.display().to_string();
    repo.hooks(&json!({"fmt": {
        "point": "after_tool",
        "command": outside_str,
        "args": ["sh", "--flag", "missing.txt", "/bin/echo"],
        "timeout": 1000,
        "on_failure": "non-blocking",
    }}));
    let hash = approve(&repo, "fmt");
    let mut got = approved_map(&repo, &empty());
    let Ok(HookState::Approved(approved)) = got.remove("fmt").unwrap() else {
        panic!("fmt is approved");
    };
    let entry = approved.entry.as_object().unwrap();
    assert_eq!(
        entry.get("command").and_then(Value::as_str).unwrap(),
        outside_str
    );
    let args = entry.get("args").and_then(Value::as_array).unwrap();
    assert_eq!(args[0].as_str().unwrap(), "sh");
    assert_eq!(args[1].as_str().unwrap(), "--flag");
    assert_eq!(args[2].as_str().unwrap(), "missing.txt");
    assert_eq!(args[3].as_str().unwrap(), "/bin/echo");
    assert_eq!(approved.hash, hash);
}

#[test]
fn absolute_symlink_spelling_is_preserved_not_canonicalized() {
    let repo = Repo::new();
    let target = repo.elsewhere("real.sh", "real\n");
    let link_path = repo.root().parent().unwrap().join("shell-link");
    let _ignored = fs::remove_file(&link_path).is_ok();
    symlink(&target, &link_path).unwrap();
    let link_str = link_path.display().to_string();
    let canonical = target.canonicalize().unwrap().display().to_string();
    assert_ne!(link_str, canonical);
    repo.hooks(&json!({"fmt": {
        "point": "after_tool",
        "command": link_str,
        "timeout": 1000,
        "on_failure": "non-blocking",
    }}));
    let hash = approve(&repo, "fmt");
    let mut got = approved_map(&repo, &empty());
    let Ok(HookState::Approved(approved)) = got.remove("fmt").unwrap() else {
        panic!("fmt is approved");
    };
    let entry = approved.entry.as_object().unwrap();
    assert_eq!(
        entry.get("command").and_then(Value::as_str).unwrap(),
        link_str
    );
    assert_ne!(
        entry.get("command").and_then(Value::as_str).unwrap(),
        canonical
    );
    assert_eq!(approved.hash, hash);
}

#[test]
fn gathered_inside_symlinks_stay_frozen() {
    let repo = Repo::new();
    repo.write("a.sh", "a\n");
    repo.write("b.sh", "b\n");
    repo.link("link.sh", &repo.root().join("a.sh"));
    repo.hooks(&json!({"fmt": {
        "point": "after_tool",
        "command": "./link.sh",
        "timeout": 1000,
        "on_failure": "non-blocking",
    }}));
    let hash = approve(&repo, "fmt");
    let mut got = approved_map(&repo, &empty());
    let Ok(HookState::Approved(approved)) = got.remove("fmt").unwrap() else {
        panic!("fmt is approved");
    };
    assert_eq!(approved.hash, hash);
    let mapped_before = approved
        .entry
        .get("command")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    assert!(mapped_before.ends_with("a.sh"), "{mapped_before}");
    // Retarget the link after the load: the approved snapshot keeps the
    // gathered resolution, so what runs does not change.
    fs::remove_file(repo.root().join("link.sh")).unwrap();
    repo.link("link.sh", &repo.root().join("b.sh"));
    assert_eq!(
        approved.mapped("./link.sh"),
        mapped_before,
        "the frozen map survives retargeting"
    );
}

#[test]
fn unapproved_and_skipped_are_not_installed() {
    let repo = Repo::new();
    repo.write("scripts/fmt.sh", "x\n");
    repo.hooks(&json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh"}}));
    let item = repo.item(OfferedKind::Hook, "fmt");
    let hash = hash(&mut Index::scratch(), &item).unwrap();
    // No approval: unapproved with hash and required.
    let mut got = approved_map(&repo, &empty());
    let Ok(HookState::Unapproved {
        hash: seen,
        required,
    }) = got.remove("fmt").unwrap()
    else {
        panic!("fmt is unapproved");
    };
    assert_eq!(seen, hash);
    assert!(!required);
    // Skipped: never installed even when approved meanwhile is irrelevant
    // here because there is no approval; a skip holds.
    let mut skipped = BTreeSet::new();
    skipped.insert(("fmt".to_owned(), hash.clone()));
    let mut got = approved_map(&repo, &skipped);
    let Ok(HookState::Skipped) = got.remove("fmt").unwrap() else {
        panic!("fmt is skipped");
    };
    // Approved then skipped: the skip wins over the approval.
    approve(&repo, "fmt");
    let mut got = approved_map(&repo, &skipped);
    let Ok(HookState::Skipped) = got.remove("fmt").unwrap() else {
        panic!("a skip holds even when approved meanwhile");
    };
}

#[test]
fn required_flag_is_repository_only() {
    for required in [false, true] {
        let repo = Repo::new();
        repo.write("scripts/fmt.sh", "x\n");
        repo.hooks(&json!({"fmt": {
            "point": "after_tool",
            "command": "scripts/fmt.sh",
            "required": required,
        }}));
        let hash = approve(&repo, "fmt");
        let mut got = approved_map(&repo, &empty());
        let Ok(HookState::Approved(approved)) = got.remove("fmt").unwrap() else {
            panic!("fmt is approved");
        };
        assert_eq!(approved.required, required);
        assert_eq!(approved.hash, hash);
    }
}

#[test]
fn copy_failures_carry_required() {
    for required in [false, true] {
        let repo = Repo::new();
        repo.write("scripts/fmt.sh", "x\n");
        repo.hooks(&json!({"fmt": {
            "point": "after_tool",
            "command": "scripts/fmt.sh",
            "required": required,
            "timeout": 1000,
            "on_failure": "non-blocking",
        }}));
        let hash = approve(&repo, "fmt");
        // Break the copy: no directory, and a directory where the marker
        // goes, so rebuilding fails and only this hook fails.
        fs::remove_dir_all(repo.home().join("pinned").join(&hash)).unwrap();
        fs::remove_file(repo.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
        fs::create_dir(repo.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
        let mut got = approved_map(&repo, &empty());
        let Err(failure) = got.remove("fmt").unwrap() else {
            panic!("fmt copy fails");
        };
        assert_eq!(failure.required, required);
        assert_eq!(failure.hash, hash);
        assert_eq!(failure.name, "fmt");
    }
}

#[test]
fn pruned_copy_is_rebuilt_with_ensure_copy() {
    let repo = Repo::new();
    repo.write("scripts/fmt.sh", "x\n");
    repo.hooks(&json!({"fmt": {
        "point": "after_tool",
        "command": "scripts/fmt.sh",
        "timeout": 1000,
        "on_failure": "non-blocking",
    }}));
    let hash = approve(&repo, "fmt");
    fs::remove_dir_all(repo.home().join("pinned").join(&hash)).unwrap();
    fs::remove_file(repo.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
    let mut got = approved_map(&repo, &empty());
    let Ok(HookState::Approved(approved)) = got.remove("fmt").unwrap() else {
        panic!("a pruned copy is rebuilt");
    };
    assert!(
        repo.home()
            .join("pinned")
            .join(&hash)
            .join("scripts/fmt.sh")
            .is_file()
    );
    assert_eq!(approved.hash, hash);
}
