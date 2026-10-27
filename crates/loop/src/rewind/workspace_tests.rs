//! Tests for `change_workspace` path resolution (`docs/events.md`,
//! "Change workspace"): every path class resolves or is refused before
//! anything starts.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use contract::ErrorCode;

use super::{is_same_folder, resolve};

fn workspace() -> fakes::TempDir {
    let home = fakes::TempDir::new("fiber-move-workspace");
    std::fs::create_dir_all(home.path().join("work")).unwrap();
    home
}

fn current(home: &fakes::TempDir) -> std::path::PathBuf {
    home.path().join("work")
}

fn refused(message: &str, typed: &str) {
    assert_eq!(
        message,
        format!("\"{typed}\" is missing or not a directory."),
        "{typed}"
    );
}

#[test]
fn a_relative_path_resolves_against_the_admitting_workspace() {
    let home = workspace();
    let target = home.path().join("other");
    std::fs::create_dir_all(&target).unwrap();
    let resolved = resolve("../other", &current(&home), None).unwrap();
    assert_eq!(resolved, std::fs::canonicalize(&target).unwrap());
}

#[test]
fn an_absolute_path_stands_as_written() {
    let home = workspace();
    let target = home.path().join("other");
    std::fs::create_dir_all(&target).unwrap();
    let typed = target.display().to_string();
    let resolved = resolve(&typed, &current(&home), None).unwrap();
    assert_eq!(resolved, std::fs::canonicalize(&target).unwrap());
}

#[test]
fn a_path_with_spaces_resolves() {
    let home = workspace();
    let target = home.path().join("with space");
    std::fs::create_dir_all(&target).unwrap();
    let resolved = resolve("../with space", &current(&home), None).unwrap();
    assert_eq!(resolved, std::fs::canonicalize(&target).unwrap());
}

#[test]
fn tilde_expands_against_the_hub_home() {
    let home = workspace();
    let dir = home.path().join("target");
    std::fs::create_dir_all(&dir).unwrap();
    for typed in ["~", "~/", "~/target"] {
        let resolved = resolve(typed, &current(&home), Some(home.path())).unwrap();
        let expected = if typed == "~/target" {
            dir.clone()
        } else {
            home.path().to_path_buf()
        };
        assert_eq!(
            resolved,
            std::fs::canonicalize(&expected).unwrap(),
            "{typed}"
        );
    }
}

#[test]
fn tilde_without_a_usable_home_is_invalid_arguments_naming_the_typed_path() {
    let home = workspace();
    for typed in ["~", "~/", "~/target"] {
        let rejection = resolve(typed, &current(&home), None).unwrap_err();
        assert_eq!(rejection.code, ErrorCode::InvalidArguments, "{typed}");
        refused(&rejection.message, typed);
    }
}

#[test]
fn an_empty_path_is_invalid_arguments() {
    let home = workspace();
    let rejection = resolve("", &current(&home), Some(home.path())).unwrap_err();
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    refused(&rejection.message, "");
}

#[test]
fn a_missing_path_is_invalid_arguments_naming_the_typed_path() {
    let home = workspace();
    for typed in ["../gone", "/definitely/not/here-either"] {
        let rejection = resolve(typed, &current(&home), Some(home.path())).unwrap_err();
        assert_eq!(rejection.code, ErrorCode::InvalidArguments, "{typed}");
        refused(&rejection.message, typed);
    }
}

#[test]
fn a_file_is_not_a_directory() {
    let home = workspace();
    let file = current(&home).join("note.txt");
    std::fs::write(&file, "hi\n").unwrap();
    let typed = file.display().to_string();
    let rejection = resolve(&typed, &current(&home), None).unwrap_err();
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    refused(&rejection.message, &typed);
}

#[test]
fn a_tilde_user_prefix_is_a_relative_path() {
    let home = workspace();
    let rejection = resolve("~other/place", &current(&home), Some(home.path())).unwrap_err();
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    refused(&rejection.message, "~other/place");
}

#[test]
fn the_workspace_itself_counts_as_the_same_folder() {
    let home = workspace();
    let current = current(&home);
    let canonical = std::fs::canonicalize(&current).unwrap();
    // As typed, with a trailing slash, and through a symlink.
    let link = home.path().join("link");
    std::os::unix::fs::symlink(&current, &link).unwrap();
    for typed in [
        current.display().to_string(),
        format!("{}/", current.display()),
        link.display().to_string(),
    ] {
        let resolved = resolve(&typed, &current, None).unwrap();
        assert_eq!(resolved, canonical, "{typed}");
        assert!(is_same_folder(&resolved, &current), "{typed}");
    }
    // Just below and just above the edge still move.
    let parent = std::fs::canonicalize(home.path()).unwrap();
    let other = home.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    assert!(!is_same_folder(&parent, &current));
    assert!(!is_same_folder(
        &std::fs::canonicalize(&other).unwrap(),
        &current
    ));
}

#[test]
fn a_moved_aftermath_refuses_change_workspace_closing() {
    use contract::commands::ChangeWorkspaceArgs;
    use contract::inbox::{Ack, Delivery};
    use contract::{CommandId, ErrorCode};

    use crate::rewind::command::refuse_after_rewound;

    for (make, message) in [
        (
            (|ack| {
                Delivery::ChangeWorkspace(
                    CommandId("c_cd".into()),
                    ChangeWorkspaceArgs {
                        path: "../other".into(),
                    },
                    ack,
                )
            }) as fn(Ack) -> Delivery,
            "The session was moved and takes no more commands.",
        ),
        (
            (|ack| {
                Delivery::Rewind(
                    CommandId("c_r".into()),
                    contract::commands::RewindArgs::Rewind {
                        from_session_id: None,
                        seq: None,
                        summarise: false,
                    },
                    ack,
                )
            }) as fn(Ack) -> Delivery,
            "The session was rewound and takes no more commands.",
        ),
    ] {
        let (tx, rx) = std::sync::mpsc::channel();
        refuse_after_rewound(make(Ack(Box::new(move |answer| {
            let _sent = tx.send(answer);
        }))));
        let rejection = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .expect("the refusal answers");
        match rejection {
            Err(rejection) => {
                assert_eq!(rejection.code, ErrorCode::Closing);
                assert_eq!(rejection.message, message);
            }
            other => panic!("refused closing, got {other:?}"),
        }
    }
    // Nothing to answer is dropped, not refused.
    refuse_after_rewound(Delivery::Cancelled);
}

#[test]
fn rewound_from_decodes_a_moved_last_line() {
    use contract::{Seq, SessionId};

    use crate::rewind::rewound_from;

    let old = SessionId("s_old".into());
    let new = SessionId("s_new".into());
    let line = contract::Envelope {
        kind: "moved".into(),
        session_id: old.clone(),
        ts: 0,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(9)),
        payload: serde_json::json!({
            "new_session_id": new.0,
            "seq": 8,
            "workspace": "/work/other",
        })
        .as_object()
        .unwrap()
        .clone(),
    };
    let (point, summary) = rewound_from(Some(line.clone()), &new, &old).unwrap();
    assert_eq!(point.session_id, old);
    assert_eq!(point.seq, Seq(8));
    assert_eq!(point.project, None);
    assert_eq!(summary, None);
    // Another child's name, or any other last line, is not a continuation.
    let other = SessionId("s_other".into());
    assert!(rewound_from(Some(line.clone()), &other, &old).is_err());
    let mut turn = line;
    turn.kind = "turn_completed".into();
    assert!(rewound_from(Some(turn), &new, &old).is_err());
    assert!(rewound_from(None, &new, &old).is_err());
}

#[test]
fn a_symlink_out_of_the_workspace_moves() {
    let home = workspace();
    let current = current(&home);
    let outside = fakes::TempDir::new("fiber-move-outside");
    let link = current.join("out");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    let resolved = resolve("out", &current, None).unwrap();
    assert_eq!(resolved, std::fs::canonicalize(outside.path()).unwrap());
    assert!(!is_same_folder(&resolved, &current));
}
