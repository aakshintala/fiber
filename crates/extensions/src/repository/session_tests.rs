//! A session's offer: what it lists, and recording a decision on content
//! that may have changed since.

use config::ProjectKey;
use contract::ErrorCode;
use contract::events::{OfferDecision, OfferedItem, OfferedKind};
use contract::repository::{Decided, RepositoryCode, Unapproved};
use fakes::MUST_SUCCEED_WITHIN;
use serde_json::json;

use super::declared_tests::Repo;
use super::{Decision, Fetch, Index, SessionOffer, Store, hash, pending};
use crate::Origin;

fn project() -> ProjectKey {
    ProjectKey::new("-p").unwrap()
}

fn session(repo: &Repo) -> SessionOffer {
    // A missing git program: nothing here has dependencies, so nothing may
    // fetch, and any attempt fails instead of hanging on the network.
    let clock = fakes::clock::FakeClock::new();
    SessionOffer::new(
        &repo.home(),
        &project(),
        &repo.root(),
        Fetch::new(
            Origin::new("fiber-no-such-git-program", |name| name.to_owned()),
            "0.1.0",
            clock,
        ),
    )
}

fn store(repo: &Repo) -> Store {
    Store::new(&repo.home(), &project(), fakes::clock::FakeClock::new())
}

fn unapproved(repo: &Repo) -> Vec<Unapproved> {
    session(repo).unapproved().unwrap()
}

fn one(repo: &Repo, kind: OfferedKind, name: &str) -> OfferedItem {
    unapproved(repo)
        .into_iter()
        .find(|u| u.offered.kind == kind && u.offered.name == name)
        .unwrap()
        .offered
}

fn decision(repo: &Repo, item: &OfferedItem) -> Option<Decision> {
    store(repo).decision(item.kind, &item.hash)
}

#[test]
fn file_returns_a_listed_files_text() {
    let repo = Repo::new();
    repo.write("tools/db.sh", "echo db\n");
    repo.hooks(&json!({"db": {"command": "tools/db.sh"}}));
    let offered = one(&repo, OfferedKind::Hook, "db");
    assert_eq!(offered.files, ["tools/db.sh"]);
    assert_eq!(session(&repo).file(&offered.files[0]).unwrap(), "echo db\n");
}

fn file_failure(repo: &Repo, path: &str, code: ErrorCode) -> String {
    let failure = session(repo).file(path).unwrap_err();
    assert_eq!(failure.code, code);
    assert!(failure.message.contains(path), "{}", failure.message);
    failure.message
}

#[test]
fn a_missing_file_is_io_failed_naming_it() {
    file_failure(&Repo::bare(), "missing", ErrorCode::IoFailed);
}

#[test]
fn a_link_now_pointing_outside_is_invalid_arguments() {
    let repo = Repo::new();
    repo.write("tools/db.sh", "echo db");
    repo.hooks(&json!({"db": {"command": "tools/db.sh"}}));
    let item = one(&repo, OfferedKind::Hook, "db");
    let outside = repo.elsewhere("secret", "PROTECTED_BYTES");
    std::fs::remove_file(repo.root().join("tools/db.sh")).unwrap();
    repo.link("tools/db.sh", &outside);
    let message = file_failure(&repo, &item.files[0], ErrorCode::InvalidArguments);
    assert!(!message.contains("PROTECTED_BYTES"));
}

#[test]
fn a_directory_is_invalid_arguments() {
    let repo = Repo::bare();
    std::fs::create_dir(repo.root().join("directory")).unwrap();
    file_failure(&repo, "directory", ErrorCode::InvalidArguments);
}

#[test]
fn a_fifo_is_invalid_arguments_and_does_not_block() {
    use std::sync::mpsc;
    let repo = Repo::bare();
    fakes::fifo(repo.root().join("fifo"));
    let code = session(&repo);
    let (tx, rx) = mpsc::channel();
    let _reader = std::thread::spawn(move || {
        tx.send(code.file("fifo")).unwrap();
    });
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
    let failure = deadline
        .recv_or_fail(&rx, "refuse the offered FIFO without blocking")
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::InvalidArguments);
    assert!(failure.message.contains("fifo"));
}

#[test]
fn a_file_that_is_not_utf8_is_invalid_arguments() {
    let repo = Repo::bare();
    std::fs::write(repo.root().join("binary"), [0xff]).unwrap();
    file_failure(&repo, "binary", ErrorCode::InvalidArguments);
}

#[test]
fn a_path_with_dotdot_that_leaves_the_workspace_is_invalid_arguments() {
    let repo = Repo::bare();
    repo.elsewhere("secret", "PROTECTED_BYTES");
    let message = file_failure(&repo, "../secret", ErrorCode::InvalidArguments);
    assert!(!message.contains("PROTECTED_BYTES"));
}

#[test]
fn an_absolute_path_outside_is_invalid_arguments() {
    let repo = Repo::bare();
    let outside = repo.elsewhere("secret", "PROTECTED_BYTES");
    let message = file_failure(
        &repo,
        outside.to_str().unwrap(),
        ErrorCode::InvalidArguments,
    );
    assert!(!message.contains("PROTECTED_BYTES"));
}

#[test]
fn an_unreadable_file_is_io_failed() {
    use std::os::unix::fs::PermissionsExt;
    assert!(
        !rustix::process::geteuid().is_root(),
        "run tests as a non-root user: root bypasses file modes"
    );
    let repo = Repo::bare();
    repo.write("unreadable", "secret");
    std::fs::set_permissions(
        repo.root().join("unreadable"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    file_failure(&repo, "unreadable", ErrorCode::IoFailed);
}

#[test]
fn a_repository_that_declares_nothing_lists_nothing_and_writes_no_index() {
    let repo = Repo::new();
    assert!(unapproved(&repo).is_empty());
    assert!(!repo.home().join("pinned.json").exists());
}

#[test]
fn items_come_in_offer_order_with_the_summary_an_install_shows() {
    let repo = Repo::new();
    repo.package("tools/b", "fiber.test/b", &json!({}));
    repo.package("tools/a", "fiber.test/a", &json!({}));
    repo.config(&json!({
        "repository_extensions": [{"path": "tools/b"}, {"path": "tools/a"}],
        "mcp": {"servers": {"z": {"command": "x"}, "y": {"url": "https://x"}}},
    }));
    repo.hooks(&json!({"fmt": {"point": "x"}, "build": {"point": "y"}}));
    let got = unapproved(&repo);
    let names: Vec<(OfferedKind, &str, bool)> = got
        .iter()
        .map(|u| (u.offered.kind, u.offered.name.as_str(), u.never))
        .collect();
    assert_eq!(
        names,
        vec![
            (OfferedKind::Extension, "fiber.test/b", false),
            (OfferedKind::Extension, "fiber.test/a", false),
            (OfferedKind::Hook, "build", false),
            (OfferedKind::Hook, "fmt", false),
            (OfferedKind::McpServer, "y", false),
            (OfferedKind::McpServer, "z", false),
        ]
    );
    let shown: Vec<OfferedItem> = pending(&store(&repo), &mut Index::scratch(), repo.items())
        .unwrap()
        .into_iter()
        .map(|p| p.offered)
        .collect();
    let offered: Vec<OfferedItem> = got.into_iter().map(|u| u.offered).collect();
    assert_eq!(offered, shown);
}

#[test]
fn a_required_hook_is_offered_as_required() {
    let repo = Repo::new();
    repo.hooks(&json!({"fmt": {"point": "x", "required": true}}));
    assert!(one(&repo, OfferedKind::Hook, "fmt").required);
}

#[test]
fn an_approved_item_is_not_listed_and_a_never_marked_one_is_listed_as_never() {
    let repo = Repo::new();
    repo.hooks(&json!({"a": {"point": "x"}, "b": {"point": "y"}}));
    let session = session(&repo);
    let a = one(&repo, OfferedKind::Hook, "a");
    let b = one(&repo, OfferedKind::Hook, "b");
    assert_eq!(
        session.decide(&a, OfferDecision::Approve).unwrap(),
        Decided::Recorded
    );
    assert_eq!(
        session.decide(&b, OfferDecision::Never).unwrap(),
        Decided::Recorded
    );
    let left = unapproved(&repo);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].offered.name, "b");
    assert!(left[0].never);
}

#[test]
fn approve_records_the_approval_and_the_pinned_copy_and_never_records_never() {
    let repo = Repo::new();
    repo.write("scripts/a.sh", "a");
    repo.hooks(&json!({"a": {"point": "x", "command": "scripts/a.sh"}, "b": {"point": "y"}}));
    let session = session(&repo);
    let a = one(&repo, OfferedKind::Hook, "a");
    let b = one(&repo, OfferedKind::Hook, "b");
    assert_eq!(
        session.decide(&a, OfferDecision::Approve).unwrap(),
        Decided::Recorded
    );
    assert_eq!(decision(&repo, &a), Some(Decision::Approve));
    assert!(
        store(&repo)
            .copy_dir(&a.hash)
            .join("scripts/a.sh")
            .is_file()
    );
    assert_eq!(
        session.decide(&b, OfferDecision::Never).unwrap(),
        Decided::Recorded
    );
    assert_eq!(decision(&repo, &b), Some(Decision::Never));
    assert!(!store(&repo).copy_dir(&b.hash).exists());
}

#[test]
fn content_changed_since_the_offer_records_nothing_and_is_listed_with_its_new_hash() {
    let repo = Repo::new();
    repo.write("scripts/a.sh", "a");
    repo.hooks(&json!({"a": {"point": "x", "command": "scripts/a.sh"}}));
    let offered = one(&repo, OfferedKind::Hook, "a");
    repo.write("scripts/a.sh", "changed");
    for choice in [OfferDecision::Approve, OfferDecision::Never] {
        assert_eq!(
            session(&repo).decide(&offered, choice).unwrap(),
            Decided::Obsolete
        );
    }
    assert_eq!(decision(&repo, &offered), None);
    let now = one(&repo, OfferedKind::Hook, "a");
    let expected = hash(&mut Index::scratch(), &repo.item(OfferedKind::Hook, "a")).unwrap();
    assert_ne!(now.hash, offered.hash);
    assert_eq!(now.hash, expected);
    assert_eq!(decision(&repo, &now), None);
}

#[test]
fn an_item_no_longer_declared_records_nothing() {
    let repo = Repo::new();
    repo.hooks(&json!({"a": {"point": "x"}}));
    let offered = one(&repo, OfferedKind::Hook, "a");
    repo.hooks(&json!({"b": {"point": "x"}}));
    assert_eq!(
        session(&repo)
            .decide(&offered, OfferDecision::Approve)
            .unwrap(),
        Decided::Obsolete
    );
    assert_eq!(decision(&repo, &offered), None);
}

#[test]
fn a_skip_is_never_recorded() {
    let repo = Repo::new();
    repo.hooks(&json!({"a": {"point": "x"}}));
    let offered = one(&repo, OfferedKind::Hook, "a");
    assert_eq!(
        session(&repo)
            .decide(&offered, OfferDecision::Skip)
            .unwrap(),
        Decided::Obsolete
    );
    assert_eq!(decision(&repo, &offered), None);
}

#[test]
fn a_declaration_error_fails_with_its_code() {
    let repo = Repo::new();
    repo.config(&json!({"repository_extensions": [{"path": "/abs"}]}));
    let failure = session(&repo).unapproved().unwrap_err();
    assert_eq!(failure.code, ErrorCode::ConfigInvalid);
    assert!(failure.message.contains("/abs"), "{}", failure.message);
}

#[test]
fn a_failure_to_record_names_the_item() {
    let repo = Repo::new();
    repo.hooks(&json!({"a": {"point": "x"}}));
    let offered = one(&repo, OfferedKind::Hook, "a");
    // A file where the approvals directory belongs makes recording fail.
    let approvals = repo.home().join("projects/-p");
    std::fs::create_dir_all(&approvals).unwrap();
    std::fs::write(approvals.join("approvals"), "").unwrap();
    let failure = session(&repo)
        .decide(&offered, OfferDecision::Never)
        .unwrap_err();
    assert!(
        failure
            .message
            .starts_with("could not record never for hook a: "),
        "{}",
        failure.message
    );
}

/// The `Debug` names the offer and every field it prints: a body replaced
/// with an empty `Ok` (the surviving mutant) prints none of them.
#[test]
fn session_offer_debug_names_the_type_and_its_fields() {
    let repo = Repo::bare();
    let text = format!("{:?}", session(&repo));
    assert!(text.contains("SessionOffer"), "{text}");
    assert!(text.contains("home"), "{text}");
    assert!(text.contains("store"), "{text}");
    assert!(text.contains("workspace"), "{text}");
    // The clock is skipped: `Arc<dyn Clock>` has no `Debug`.
    assert!(!text.contains("clock"), "{text}");
}
