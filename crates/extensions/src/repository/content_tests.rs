//! The content hash and `pinned.json`.

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use contract::events::OfferedKind;
use serde_json::{Value, json};

use super::content::hash_paths;
use super::declared_tests::Repo;
use super::{Index, RepoItem, hash};

fn server(repo: &Repo, args: &[&str]) -> RepoItem {
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "scripts/run.sh", "args": args}}}}));
    repo.item(OfferedKind::McpServer, "db")
}

fn hashed(repo: &Repo, args: &[&str]) -> String {
    hash(&mut Index::load(&repo.home()), &server(repo, args)).unwrap()
}

fn is_hex_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn set_mtime(path: &Path, seconds: u64) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

#[test]
fn the_hash_is_lowercase_hex_sha256() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    assert!(is_hex_sha256(&hashed(&repo, &[])));
}

#[test]
fn the_same_content_in_two_checkouts_has_one_hash() {
    let (a, b) = (Repo::new(), Repo::new());
    for repo in [&a, &b] {
        repo.write("scripts/run.sh", "echo");
        repo.write("lib/x.js", "x");
    }
    assert_ne!(a.root(), b.root());
    for kind_args in [vec!["lib/x.js"], vec![]] {
        assert_eq!(hashed(&a, &kind_args), hashed(&b, &kind_args));
    }
    for repo in [&a, &b] {
        repo.hooks(&json!({"h": {"point": "after_tool", "command": "scripts/run.sh"}}));
    }
    let hook =
        |repo: &Repo| hash(&mut Index::scratch(), &repo.item(OfferedKind::Hook, "h")).unwrap();
    assert_eq!(hook(&a), hook(&b));
}

#[test]
fn one_changed_byte_changes_the_hash() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo a");
    let before = hashed(&repo, &[]);
    repo.write("scripts/run.sh", "echo b");
    assert_ne!(hashed(&repo, &[]), before);
}

#[test]
fn a_changed_exec_bit_changes_the_hash() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    let plain = hashed(&repo, &[]);
    repo.executable("scripts/run.sh");
    let executable = hashed(&repo, &[]);
    assert_ne!(plain, executable);
    // Any one of the three execute bits counts.
    for mode in [0o744, 0o654, 0o645] {
        fs::set_permissions(
            repo.root().join("scripts/run.sh"),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        assert_eq!(hashed(&repo, &[]), executable, "{mode:o}");
    }
    fs::set_permissions(
        repo.root().join("scripts/run.sh"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(hashed(&repo, &[]), plain);
}

#[test]
fn a_changed_declaration_changes_the_hash() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    let base = hashed(&repo, &[]);
    assert_ne!(hashed(&repo, &["--ro"]), base);
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "scripts/run.sh", "args": [], "env": {"A": "1"}}}}}));
    let item = repo.item(OfferedKind::McpServer, "db");
    assert_ne!(hash(&mut Index::scratch(), &item).unwrap(), base);
    repo.config(&json!({"mcp": {"servers": {"other": {"command": "scripts/run.sh", "args": []}}}}));
    let item = repo.item(OfferedKind::McpServer, "other");
    assert_ne!(hash(&mut Index::scratch(), &item).unwrap(), base);
}

#[test]
fn key_order_in_the_declaration_does_not_change_the_hash() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    repo.write(
        ".fiber/config.json",
        r#"{"mcp": {"servers": {"db": {"command": "scripts/run.sh", "args": [], "required": true}}}}"#,
    );
    let one = hash(
        &mut Index::scratch(),
        &repo.item(OfferedKind::McpServer, "db"),
    )
    .unwrap();
    repo.write(
        ".fiber/config.json",
        r#"{"mcp": {"servers": {"db": {"required": true, "args": [], "command": "scripts/run.sh"}}}}"#,
    );
    let two = hash(
        &mut Index::scratch(),
        &repo.item(OfferedKind::McpServer, "db"),
    )
    .unwrap();
    assert_eq!(one, two);
}

#[test]
fn a_renamed_file_changes_the_hash() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    repo.write("scripts/renamed.sh", "echo");
    let a = hashed(&repo, &["scripts/renamed.sh"]);
    fs::remove_file(repo.root().join("scripts/renamed.sh")).unwrap();
    repo.write("scripts/other.sh", "echo");
    let b = hashed(&repo, &["scripts/other.sh"]);
    assert_ne!(a, b);
}

#[test]
fn files_hash_in_path_order_whatever_order_they_come_in() {
    let repo = Repo::new();
    repo.write("a", "1");
    repo.write("b", "2");
    let (a, b) = (repo.root().join("a"), repo.root().join("b"));
    let kind = OfferedKind::McpServer;
    let forward = hash_paths(
        &mut Index::scratch(),
        "x",
        kind,
        None,
        [("a", a.as_path()), ("b", b.as_path())].into_iter(),
    )
    .unwrap();
    let backward = hash_paths(
        &mut Index::scratch(),
        "x",
        kind,
        None,
        [("b", b.as_path()), ("a", a.as_path())].into_iter(),
    )
    .unwrap();
    assert_eq!(forward, backward);
}

#[test]
fn the_hash_is_framed_so_neighbouring_fields_cannot_run_together() {
    let repo = Repo::new();
    repo.write("a", "bc");
    repo.write("ab", "c");
    let (a, ab) = (repo.root().join("a"), repo.root().join("ab"));
    let on = |kind, declaration: Option<&Value>, rel: &str, path: &Path| {
        hash_paths(
            &mut Index::scratch(),
            "x",
            kind,
            declaration,
            [(rel, path)].into_iter(),
        )
        .unwrap()
    };
    let server = OfferedKind::McpServer;
    // The same bytes behind different names.
    assert_ne!(on(server, None, "a", &a), on(server, None, "ab", &ab));
    // The same files as another kind of item.
    assert_ne!(
        on(server, None, "a", &a),
        on(OfferedKind::Hook, None, "a", &a)
    );
    assert_ne!(
        on(server, None, "a", &a),
        on(OfferedKind::Extension, None, "a", &a)
    );
    // A declaration that ends in what the next field starts with.
    let none = on(server, None, "a", &a);
    let empty = json!({});
    assert_ne!(on(server, Some(&empty), "a", &a), none);
    let nul = json!({"name": "a\u{0}", "entry": {}});
    let plain = json!({"name": "a", "entry": {}});
    assert_ne!(
        on(server, Some(&nul), "a", &a),
        on(server, Some(&plain), "a", &a)
    );
}

#[test]
fn an_unchanged_file_is_not_read_again() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    let mut index = Index::load(&repo.home());
    let item = server(&repo, &[]);
    let first = hash(&mut index, &item).unwrap();
    index.save().unwrap();
    // A file the person can no longer read still has its recorded hash.
    let file = repo.root().join("scripts/run.sh");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    let mut index = Index::load(&repo.home());
    let item = server(&repo, &[]);
    let second = hash(&mut index, &item);
    assert_eq!(second.unwrap(), first);
}

#[test]
fn a_changed_size_or_modification_time_reads_the_file_again() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo aa");
    let file = repo.root().join("scripts/run.sh");
    set_mtime(&file, 1_000_000);
    let mut index = Index::load(&repo.home());
    let first = hash(&mut index, &server(&repo, &[])).unwrap();
    index.save().unwrap();

    // The same size and time: the recorded hash is trusted.
    fs::write(&file, "echo bb").unwrap();
    set_mtime(&file, 1_000_000);
    let mut index = Index::load(&repo.home());
    assert_eq!(hash(&mut index, &server(&repo, &[])).unwrap(), first);

    // Another time, the same size.
    set_mtime(&file, 1_000_001);
    let mut index = Index::load(&repo.home());
    let second = hash(&mut index, &server(&repo, &[])).unwrap();
    assert_ne!(second, first);
    index.save().unwrap();

    // Another size, the same time.
    fs::write(&file, "echo bbbb").unwrap();
    set_mtime(&file, 1_000_001);
    let mut index = Index::load(&repo.home());
    assert_ne!(hash(&mut index, &server(&repo, &[])).unwrap(), second);
}

#[test]
fn a_missing_or_damaged_index_costs_a_rehash_and_nothing_else() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    let mut index = Index::load(&repo.home());
    let first = hash(&mut index, &server(&repo, &[])).unwrap();
    index.save().unwrap();
    let file = repo.home().join("pinned.json");
    assert!(file.is_file());
    for damaged in [
        "",
        "not json",
        "[]",
        r#"{"x": {"size": "a"}}"#,
        "{\"/p\": {\"size\": 1, \"mtime_ns\": 1, \"hash\": \"zz\"}}",
    ] {
        fs::write(&file, damaged).unwrap();
        let mut index = Index::load(&repo.home());
        assert_eq!(
            hash(&mut index, &server(&repo, &[])).unwrap(),
            first,
            "{damaged}"
        );
        index.save().unwrap();
        let rewritten: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(rewritten.as_object().unwrap().len(), 1, "{damaged}");
    }
    fs::remove_file(&file).unwrap();
    let mut index = Index::load(&repo.home());
    assert_eq!(hash(&mut index, &server(&repo, &[])).unwrap(), first);
}

#[test]
fn the_index_records_size_modification_time_and_hash_of_each_path() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    let file = repo.root().join("scripts/run.sh");
    set_mtime(&file, 5);
    let mut index = Index::load(&repo.home());
    hash(&mut index, &server(&repo, &[])).unwrap();
    index.save().unwrap();
    let saved: Value =
        serde_json::from_slice(&fs::read(repo.home().join("pinned.json")).unwrap()).unwrap();
    let key = file.canonicalize().unwrap();
    let entry = &saved[key.to_str().unwrap()];
    assert_eq!(entry["size"], 4);
    assert_eq!(entry["mtime_ns"], 5_000_000_000_u64);
    assert!(is_hex_sha256(entry["hash"].as_str().unwrap()), "{entry}");
}

#[test]
fn an_index_that_did_not_change_is_not_written_and_a_pre_epoch_time_is_not_recorded() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    let mut index = Index::load(&repo.home());
    index.save().unwrap();
    assert!(!repo.home().join("pinned.json").exists());
    let file = repo.root().join("scripts/run.sh");
    let handle = File::options().write(true).open(&file).unwrap();
    handle
        .set_modified(UNIX_EPOCH - Duration::from_secs(10))
        .unwrap();
    drop(handle);
    let before = hash(&mut index, &server(&repo, &[])).unwrap();
    index.save().unwrap();
    assert!(!repo.home().join("pinned.json").exists());
    assert_eq!(hash(&mut index, &server(&repo, &[])).unwrap(), before);
}

#[test]
fn a_file_larger_than_one_chunk_hashes_by_its_whole_content() {
    let repo = Repo::new();
    let big = "x".repeat(200_000);
    repo.write("scripts/run.sh", &big);
    let one = hashed(&repo, &[]);
    repo.write("scripts/run.sh", &format!("{big}y"));
    let two = hashed(&repo, &[]);
    assert_ne!(one, two);
    repo.write("scripts/run.sh", &format!("y{}", &big[1..]));
    assert_ne!(hashed(&repo, &[]), one);
}

#[test]
fn a_file_that_vanishes_after_listing_is_an_error_naming_the_item() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    let item = server(&repo, &[]);
    fs::remove_file(repo.root().join("scripts/run.sh")).unwrap();
    let e = hash(&mut Index::scratch(), &item).unwrap_err();
    assert!(e.to_string().starts_with("`db`:"), "{e}");
}

#[test]
fn an_entry_whose_hash_is_not_a_digest_is_not_trusted() {
    let repo = Repo::new();
    repo.write("scripts/run.sh", "echo");
    let mut index = Index::load(&repo.home());
    let first = hash(&mut index, &server(&repo, &[])).unwrap();
    index.save().unwrap();
    let file = repo.home().join("pinned.json");
    let saved: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let good = saved.as_object().unwrap().values().next().unwrap()["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    for bad in [
        "ab".to_owned(),
        good[..63].to_owned(),
        format!("{good}0"),
        "zz".repeat(32),
        "é".repeat(32),
    ] {
        let mut broken = saved.clone();
        for entry in broken.as_object_mut().unwrap().values_mut() {
            entry["hash"] = Value::String(bad.clone());
        }
        fs::write(&file, broken.to_string()).unwrap();
        let mut index = Index::load(&repo.home());
        assert_eq!(
            hash(&mut index, &server(&repo, &[])).unwrap(),
            first,
            "{bad}"
        );
    }
}
