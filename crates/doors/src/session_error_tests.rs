//! One row per [`SessionError`](super::SessionError) case: its code, its
//! sentence, and its contract record. The forwarding rows hold codes and
//! retry data a hard-coded conversion drops.

use std::io;
use std::path::PathBuf;

use contract::shapes::{Failure, ProviderFailure};
use contract::{ErrorCode, SCHEMA_VERSION, SessionId};

use super::SessionError;
use crate::socket::SOCKET_PATH_MAX;

fn other(why: &str) -> io::Error {
    io::Error::other(why.to_owned())
}

fn not_found(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, why.to_owned())
}

fn id() -> SessionId {
    SessionId("s_1".to_owned())
}

fn json_error() -> serde_json::Error {
    serde_json::from_slice::<serde_json::Value>(&[0xff]).unwrap_err()
}

fn git_failed(stderr: &str) -> worktree::Error {
    worktree::Error::Git {
        path: PathBuf::from("/x/w"),
        command: "worktree add".to_owned(),
        stderr: stderr.to_owned(),
    }
}

#[test]
fn each_session_error_maps_to_its_code_sentence_and_record() {
    let json_source = json_error();
    let rows: Vec<(&str, SessionError, ErrorCode, String)> = vec![
        (
            "no prompt",
            SessionError::NoPrompt,
            ErrorCode::Usage,
            "No prompt. Run `fiber ask \"<prompt>\"` or `fiber ask < <file>`.".to_owned(),
        ),
        (
            "stdin not utf8",
            SessionError::StdinNotUtf8,
            ErrorCode::Usage,
            "stdin is not UTF-8 text.".to_owned(),
        ),
        (
            "stdin unreadable",
            SessionError::StdinUnreadable {
                source: other("closed"),
            },
            ErrorCode::IoFailed,
            "stdin could not be read: closed".to_owned(),
        ),
        (
            "socket path too long",
            SessionError::SocketPathTooLong {
                max: SOCKET_PATH_MAX,
            },
            ErrorCode::Usage,
            format!(
                "FIBER_HOME is too long: a session's socket path must fit in \
                 {SOCKET_PATH_MAX} bytes. Set FIBER_HOME to a shorter path."
            ),
        ),
        (
            "socket held",
            SessionError::SocketHeld {
                socket: PathBuf::from("/x/run/s_1"),
            },
            ErrorCode::SessionHeld,
            "/x/run/s_1: another session is running".to_owned(),
        ),
        (
            "socket io",
            SessionError::SocketIo {
                path: PathBuf::from("/x/run/s_1"),
                source: not_found("missing"),
            },
            ErrorCode::IoFailed,
            "/x/run/s_1: missing".to_owned(),
        ),
        (
            "shared io",
            SessionError::SharedIo {
                path: PathBuf::from("/x/run/s_1.lock"),
                source: not_found("missing"),
            },
            ErrorCode::IoFailed,
            "/x/run/s_1.lock: entity not found.".to_owned(),
        ),
        (
            "subscribe clone",
            SessionError::SubscribeClone {
                socket: PathBuf::from("/x/run/s_1"),
                source: other("closed"),
            },
            ErrorCode::IoFailed,
            "/x/run/s_1: closed".to_owned(),
        ),
        (
            "command encode",
            SessionError::CommandEncode {
                source: json_error(),
            },
            ErrorCode::IoFailed,
            format!("a command could not be written as JSON: {json_source}"),
        ),
        (
            "stdout failed",
            SessionError::StdoutFailed {
                source: other("closed"),
            },
            ErrorCode::IoFailed,
            "stdout could not be written: closed".to_owned(),
        ),
        (
            "rejected",
            SessionError::Rejected {
                code: ErrorCode::SessionHasDependents,
                message: "It has forks.".to_owned(),
            },
            ErrorCode::SessionHasDependents,
            "It has forks.".to_owned(),
        ),
        (
            "rejected carries a second wire code",
            SessionError::Rejected {
                code: ErrorCode::Usage,
                message: "busy".to_owned(),
            },
            ErrorCode::Usage,
            "busy".to_owned(),
        ),
        (
            "version mismatch",
            SessionError::VersionMismatch {
                id: id(),
                version: 3,
            },
            ErrorCode::SessionHeld,
            format!(
                "session s_1 runs schema version 3, this Fiber runs schema version \
                 {SCHEMA_VERSION}; close the session or let it exit, then resume"
            ),
        ),
        (
            "session ended",
            SessionError::SessionEnded { id: id() },
            ErrorCode::Closing,
            "session s_1 ended before its turn completed".to_owned(),
        ),
        (
            "thread spawn",
            SessionError::ThreadSpawn {
                source: other("closed"),
            },
            ErrorCode::IoFailed,
            "cannot start a thread: closed".to_owned(),
        ),
        (
            "worktree failed",
            SessionError::WorktreeFailed {
                message: "git worktree add failed for the worktree /x/w.".to_owned(),
                source: git_failed("fatal: no such branch"),
            },
            ErrorCode::IoFailed,
            "git worktree add failed for the worktree /x/w.".to_owned(),
        ),
        (
            "worktree access",
            SessionError::WorktreeAccess {
                message: "git rev-parse failed for the worktree /x/w.".to_owned(),
                source: git_failed("fatal: no such path"),
            },
            ErrorCode::IoFailed,
            "git rev-parse failed for the worktree /x/w.".to_owned(),
        ),
        (
            "repository gone",
            SessionError::RepositoryGone {
                repository: PathBuf::from("/repo"),
                path: PathBuf::from("/x/w"),
            },
            ErrorCode::IoFailed,
            "The repository /repo is gone, so the worktree /x/w cannot be made again.".to_owned(),
        ),
        (
            "share failed",
            SessionError::ShareFailed {
                path: PathBuf::from("/x/w"),
                kind: io::ErrorKind::InvalidInput,
            },
            ErrorCode::IoFailed,
            "/x/w: invalid input parameter.".to_owned(),
        ),
        (
            "worktree in use",
            SessionError::WorktreeInUse {
                path: PathBuf::from("/x/w"),
            },
            ErrorCode::IoFailed,
            "The worktree /x/w is in use by another process, so Fiber did not make it again."
                .to_owned(),
        ),
    ];
    for (name, error, code, message) in &rows {
        assert_eq!(&error.code(), code, "code, row: {name}");
        assert_eq!(&error.to_string(), message, "message, row: {name}");
        let record = Failure::from(error);
        assert_eq!(&record.code, code, "record code, row: {name}");
        assert_eq!(&record.message, message, "record message, row: {name}");
        assert_eq!(record.retry_after_ms, None, "record retry, row: {name}");
        assert_eq!(record.provider, None, "record provider, row: {name}");
    }
}

#[test]
fn forwarded_returns_the_failure_unchanged() {
    let failure = Failure {
        code: ErrorCode::SessionHeld,
        message: "held".to_owned(),
        retry_after_ms: Some(1),
        provider: Some(ProviderFailure {
            name: "acme".to_owned(),
            status: Some(429),
            message: "slow down".to_owned(),
        }),
    };
    let owned = SessionError::Forwarded(failure.clone());
    assert_eq!(owned.code(), ErrorCode::SessionHeld);
    assert_eq!(owned.to_string(), "held");
    assert_eq!(Failure::from(owned), failure);
    let borrowed = SessionError::Forwarded(failure.clone());
    assert_eq!(Failure::from(&borrowed), failure);
}

#[test]
fn rejected_keeps_an_empty_message_verbatim() {
    let empty = SessionError::Rejected {
        code: ErrorCode::SessionHasDependents,
        message: String::new(),
    };
    assert_eq!(empty.to_string(), "");
    assert_eq!(Failure::from(&empty).message, "");
}

#[test]
fn socket_and_shared_io_do_not_share_a_format() {
    let socket = SessionError::SocketIo {
        path: PathBuf::from("/x/run/s_1"),
        source: not_found("missing"),
    };
    let shared = SessionError::SharedIo {
        path: PathBuf::from("/x/run/s_1"),
        source: not_found("missing"),
    };
    assert_eq!(socket.to_string(), "/x/run/s_1: missing");
    assert_eq!(shared.to_string(), "/x/run/s_1: entity not found.");
    assert_ne!(socket.to_string(), shared.to_string());
}

#[test]
fn worktree_failed_reads_its_source_code() {
    let usage = SessionError::WorktreeFailed {
        message: "gone".to_owned(),
        source: worktree::Error::NotARepository {
            path: PathBuf::from("/x"),
        },
    };
    assert_eq!(usage.code(), ErrorCode::Usage);
    assert_eq!(Failure::from(&usage).code, ErrorCode::Usage);
}

#[test]
fn malformed_wire_rejections_stay_unknown_and_empty() {
    let empty = serde_json::Map::new();
    let rejection = crate::attach::rejection(&empty);
    assert!(
        matches!(
            &rejection,
            SessionError::Rejected { code, message }
            if *code == ErrorCode::Other("unknown".to_owned()) && message.is_empty()
        ),
        "a missing code and message stay unknown and empty: {rejection:?}"
    );
    assert_eq!(rejection.code(), ErrorCode::Other("unknown".to_owned()));
    let payload: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({"code": 42, "message": 7})).unwrap();
    let rejection = crate::attach::rejection(&payload);
    assert!(
        matches!(
            &rejection,
            SessionError::Rejected { code, message }
            if *code == ErrorCode::Other("unknown".to_owned()) && message.is_empty()
        ),
        "a non-string code and message stay unknown and empty: {rejection:?}"
    );
    assert_eq!(rejection.code(), ErrorCode::Other("unknown".to_owned()));
}

#[test]
fn worktree_failures_hide_git_stderr_from_debug() {
    const SECRET: &str = "sekret-stderr-token";
    let failed = SessionError::WorktreeFailed {
        message: "git worktree add failed for the worktree /x/w.".to_owned(),
        source: git_failed(SECRET),
    };
    let access = SessionError::WorktreeAccess {
        message: "git rev-parse failed for the worktree /x/w.".to_owned(),
        source: git_failed(SECRET),
    };
    let failed_debug = format!("{:?}", failed);
    assert!(
        !failed_debug.contains(SECRET),
        "WorktreeFailed debug: {failed:?}"
    );
    assert!(
        failed_debug.contains("WorktreeFailed"),
        "WorktreeFailed debug names its variant: {failed:?}"
    );
    assert!(
        failed_debug.contains("git worktree add failed for the worktree /x/w."),
        "WorktreeFailed debug keeps its safe message: {failed:?}"
    );
    let access_debug = format!("{:?}", access);
    assert!(
        !access_debug.contains(SECRET),
        "WorktreeAccess debug: {access:?}"
    );
    assert!(
        access_debug.contains("WorktreeAccess"),
        "WorktreeAccess debug names its variant: {access:?}"
    );
    assert!(
        access_debug.contains("git rev-parse failed for the worktree /x/w."),
        "WorktreeAccess debug keeps its safe message: {access:?}"
    );
    assert_eq!(
        failed.to_string(),
        "git worktree add failed for the worktree /x/w."
    );
    assert!(std::error::Error::source(&failed).is_some());
    assert!(std::error::Error::source(&access).is_some());
}
