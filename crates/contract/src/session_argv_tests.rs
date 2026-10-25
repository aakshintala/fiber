#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use super::*;

use crate::{JobId, SessionId};

fn rendered(argv: &[OsString]) -> Vec<String> {
    argv.iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn minimal_value_is_session_id_and_workspace() {
    let args = SessionArgv {
        id: SessionId("s_0123456789abcdef".to_owned()),
        workspace: PathBuf::from("/w"),
        ..SessionArgv::default()
    };
    assert_eq!(
        args.to_argv(),
        [
            OsString::from("session"),
            OsString::from("--id=s_0123456789abcdef"),
            OsString::from("--workspace=/w"),
        ]
    );
}

#[test]
fn full_value_lists_every_field_in_order_with_login_shell_last() {
    let args = SessionArgv {
        id: SessionId("s_0123456789abcdef".to_owned()),
        workspace: PathBuf::from("/w"),
        model: Some("fake/m".to_owned()),
        overrides: vec!["a=1".to_owned(), "b=2".to_owned()],
        prompt: Some("hello".to_owned()),
        resume: true,
        credential: Some("work".to_owned()),
        worktree: true,
        rewound_from: Some(SessionId("s_aaaaaaaaaaaaaaaa".to_owned())),
        continues: Some(SessionId("s_dddddddddddddddd".to_owned())),
        forked_from: Some(SessionId("s_dddddddddddddddd".to_owned())),
        forked_seq: Some(41),
        parent: Some(SessionId("s_bbbbbbbbbbbbbbbb".to_owned())),
        delegate_id: Some(JobId("j_cccccccccccccccc".to_owned())),
        depth: Some(2),
        login_shell: true,
        ..SessionArgv::default()
    };
    assert_eq!(
        rendered(&args.to_argv()),
        [
            "session",
            "--id=s_0123456789abcdef",
            "--workspace=/w",
            "--model=fake/m",
            "-c",
            "a=1",
            "-c",
            "b=2",
            "--resume",
            "--prompt=hello",
            "--credential=work",
            "--worktree",
            "--rewound-from=s_aaaaaaaaaaaaaaaa",
            "--continues=s_dddddddddddddddd",
            "--forked-from=s_dddddddddddddddd",
            "--forked-seq=41",
            "--parent=s_bbbbbbbbbbbbbbbb",
            "--delegate-id=j_cccccccccccccccc",
            "--depth=2",
            "--login-shell",
        ]
    );
    assert_eq!(
        args.to_argv()
            .last()
            .map(|arg| arg.to_string_lossy().into_owned()),
        Some("--login-shell".to_owned())
    );
}

#[test]
fn below_and_above_the_dash_c_edge_attach_or_split() {
    let attached = SessionArgv {
        id: SessionId(String::new()),
        workspace: PathBuf::new(),
        overrides: vec!["-x=1".to_owned()],
        ..SessionArgv::default()
    };
    assert!(rendered(&attached.to_argv()).contains(&"-c-x=1".to_owned()));
    assert!(
        !rendered(&attached.to_argv())
            .iter()
            .any(|arg| arg == "-x=1")
    );
    let plain = SessionArgv {
        overrides: vec!["a=1".to_owned()],
        ..SessionArgv::default()
    };
    let argv = plain.to_argv();
    let at = rendered(&argv)
        .iter()
        .position(|arg| arg == "-c")
        .expect("-c pair");
    assert_eq!(rendered(&argv)[at + 1], "a=1");
}

#[test]
fn credential_and_prompt_starting_with_a_dash_stay_one_value() {
    let args = SessionArgv {
        credential: Some("-label".to_owned()),
        prompt: Some("-go".to_owned()),
        ..SessionArgv::default()
    };
    let argv = rendered(&args.to_argv());
    assert!(argv.contains(&"--credential=-label".to_owned()));
    assert!(argv.contains(&"--prompt=-go".to_owned()));
}

#[test]
fn workspace_named_like_a_flag_stays_one_value() {
    let args = SessionArgv {
        workspace: PathBuf::from("-w"),
        ..SessionArgv::default()
    };
    assert!(rendered(&args.to_argv()).contains(&"--workspace=-w".to_owned()));
}

#[test]
fn non_utf8_workspace_passes_through_byte_for_byte() {
    let args = SessionArgv {
        id: SessionId("s_0123456789abcdef".to_owned()),
        workspace: PathBuf::from(OsString::from_vec(b"/w/\xff".to_vec())),
        ..SessionArgv::default()
    };
    let argv = args.to_argv();
    assert_eq!(argv[2], OsString::from_vec(b"--workspace=/w/\xff".to_vec()));
}

#[test]
fn continues_absent_renders_nothing_and_set_renders_once_after_rewound_from() {
    let absent = SessionArgv {
        rewound_from: Some(SessionId("s_aaaaaaaaaaaaaaaa".to_owned())),
        continues: None,
        ..SessionArgv::default()
    };
    assert!(
        !rendered(&absent.to_argv())
            .iter()
            .any(|arg| arg.starts_with("--continues"))
    );
    let set = SessionArgv {
        rewound_from: Some(SessionId("s_aaaaaaaaaaaaaaaa".to_owned())),
        continues: Some(SessionId("s_dddddddddddddddd".to_owned())),
        ..SessionArgv::default()
    };
    let argv = rendered(&set.to_argv());
    assert_eq!(
        argv.iter()
            .filter(|arg| arg.starts_with("--continues"))
            .count(),
        1
    );
    let rewound_at = argv
        .iter()
        .position(|arg| arg == "--rewound-from=s_aaaaaaaaaaaaaaaa")
        .expect("rewound-from renders");
    assert_eq!(argv[rewound_at + 1], "--continues=s_dddddddddddddddd");
}

#[test]
fn supervised_absent_renders_nothing_and_set_renders() {
    let absent = SessionArgv {
        login_shell: true,
        ..SessionArgv::default()
    };
    assert!(
        !rendered(&absent.to_argv())
            .iter()
            .any(|arg| arg.starts_with("--supervised"))
    );
    let set = SessionArgv {
        login_shell: true,
        supervised: true,
        ..SessionArgv::default()
    };
    let argv = rendered(&set.to_argv());
    // Presence when set, absence when not: no area doc pins the flag's count
    // or its place after `--login-shell`, so the test pins neither, only
    // that a set flag renders.
    assert!(
        argv.iter().any(|arg| arg == "--supervised"),
        "set supervised renders: {argv:?}"
    );
}

#[test]
fn default_renders_as_the_minimal_value_with_empty_id_and_workspace() {
    let args = SessionArgv::default();
    assert_eq!(args.id, SessionId(String::new()));
    assert_eq!(args.workspace, PathBuf::new());
    assert_eq!(
        args.to_argv(),
        [
            OsString::from("session"),
            OsString::from("--id="),
            OsString::from("--workspace="),
        ]
    );
}
