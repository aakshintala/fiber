#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::Path;

use contract::{Seq, SessionId};
use serde_json::json;

use super::{Context, point_dir};

fn started(id: &str, workspace: &str, parent: Option<(&str, u64, &str)>) -> String {
    let mut payload = json!({
        "workspace": workspace,
        "variables": {"path": "", "names": [], "source": "inherited"}
    });
    if let Some((session, seq, project)) = parent {
        payload["forked_from"] = json!({"session_id": session, "seq": seq, "project": project});
    }
    format!(
        "{}\n",
        json!({
            "kind": "session_started", "session_id": id, "seq": 0,
            "ts": 0, "schema_version": 1, "payload": payload
        })
    )
}

#[test]
fn located_migrated_session_matches_recorded_project_and_refuses_another() {
    let home = fakes::TempDir::new("main-rewind-located-migrated");
    let identity = Path::new("/target/repository");
    let id = SessionId("s_parent".into());
    let sessions = log::sessions_dir(home.path(), identity);
    let dir = log::SessionDir::new(&sessions, &id);
    fs::create_dir_all(dir.path()).unwrap();
    fs::write(dir.events(), started(&id.0, "/target/repository", None)).unwrap();
    let context = Context {
        home: home.path().to_path_buf(),
        identity: identity.to_path_buf(),
        sessions,
        reads: std::sync::Arc::new(crate::switch::Reads::default()),
        _cancel: crate::switch::CancelOnDrop(std::sync::Arc::new(crate::switch::Reads::default())),
    };
    assert_eq!(
        super::located_session(&context, &id).unwrap().path(),
        dir.path()
    );
    fs::write(dir.events(), started(&id.0, "/wrong/repository", None)).unwrap();
    assert!(super::located_session(&context, &id).is_none());
}

#[test]
fn point_dir_keeps_the_physical_parent_directory_after_a_project_move() {
    let home = fakes::TempDir::new("main-rewind-migrated-parent");
    let source = Path::new("/source/repository");
    let target = Path::new("/target/repository");
    let source_key = log::project_key(source);
    let target_key = log::project_key(target);
    let parent = SessionId("s_parent".into());
    let child = SessionId("s_child".into());
    let parent_dir = log::session_dir(home.path(), &source_key, &parent);
    fs::create_dir_all(&parent_dir).unwrap();
    fs::write(
        parent_dir.join("events.jsonl"),
        started(&parent.0, "/source/repository", None),
    )
    .unwrap();
    let child_dir = log::session_dir(home.path(), &target_key, &child);
    fs::create_dir_all(&child_dir).unwrap();
    fs::write(
        child_dir.join("events.jsonl"),
        started(
            &child.0,
            "/target/repository",
            Some((&parent.0, 0, &source_key)),
        ),
    )
    .unwrap();

    let context = Context {
        home: home.path().to_path_buf(),
        identity: target.to_path_buf(),
        sessions: log::sessions_dir(home.path(), target),
        reads: std::sync::Arc::new(crate::switch::Reads::default()),
        _cancel: crate::switch::CancelOnDrop(std::sync::Arc::new(crate::switch::Reads::default())),
    };
    let point = contract::shapes::Point {
        session_id: parent,
        seq: Seq(0),
        project: Some(source_key),
    };
    assert_eq!(
        point_dir(child_dir.as_path(), &context.sessions, &point),
        parent_dir
    );
}
