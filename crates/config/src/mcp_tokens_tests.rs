use super::McpTokenFiles;
use crate::write::atomic::{Stage, fail_at};
use contract::mcp_tokens::{TokenFileError, TokenFiles};
use serde_json::json;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn reads_do_not_create_a_credential_directory() {
    let dir = fakes::TempDir::new("mcp-empty");
    let files = McpTokenFiles::new(dir.path().into());
    assert_eq!(files.read("abc").unwrap(), None);
    assert!(!dir.path().join("credentials").exists());
}

#[test]
fn writes_are_private_and_reads_do_not_wait_for_the_lock() {
    let dir = fakes::TempDir::new("mcp-private");
    let files = McpTokenFiles::new(dir.path().into());
    let held = files.try_lock("abc").unwrap().unwrap();
    held.write(&json!({"url":"https://h/x"})).unwrap();
    assert_eq!(held.read().unwrap(), Some(json!({"url":"https://h/x"})));
    assert_eq!(files.read("abc").unwrap(), held.read().unwrap());
    for (path, expected) in [
        ("credentials", 0o700),
        ("credentials/mcp", 0o700),
        ("credentials/mcp/abc", 0o600),
    ] {
        assert_eq!(
            fs::metadata(dir.path().join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            expected
        );
    }
    let second = McpTokenFiles::new(dir.path().into());
    assert!(second.try_lock("abc").unwrap().is_none());
    drop(held);
    assert!(second.try_lock("abc").unwrap().is_some());
}

#[test]
fn a_symlinked_mcp_directory_is_refused() {
    let dir = fakes::TempDir::new("mcp-symlink");
    fs::create_dir(dir.path().join("credentials")).unwrap();
    fs::create_dir(dir.path().join("elsewhere")).unwrap();
    symlink(
        dir.path().join("elsewhere"),
        dir.path().join("credentials/mcp"),
    )
    .unwrap();
    let files = McpTokenFiles::new(dir.path().into());
    assert!(matches!(files.read("abc"), Err(TokenFileError::Refused(_))));
    assert!(matches!(
        files.try_lock("abc"),
        Err(TokenFileError::Refused(_))
    ));
    assert_eq!(
        fs::read_dir(dir.path().join("elsewhere")).unwrap().count(),
        0
    );
}

#[test]
fn write_errors_distinguish_whether_the_rename_happened() {
    for stage in [Stage::BeforeRename, Stage::AfterRename] {
        let dir = fakes::TempDir::new("mcp-write-failure");
        let files = McpTokenFiles::new(dir.path().into());
        let held = files.try_lock("abc").unwrap().unwrap();
        held.write(&json!({"token":"old"})).unwrap();
        let old = fs::read(dir.path().join("credentials/mcp/abc")).unwrap();
        fail_at(Some(stage));
        let error = held.write(&json!({"token":"new"})).unwrap_err();
        fail_at(None);
        assert_eq!(error.code(), contract::ErrorCode::IoFailed);
        match stage {
            Stage::BeforeRename => {
                assert!(matches!(error, TokenFileError::Unwritable(_)));
                assert_eq!(
                    fs::read(dir.path().join("credentials/mcp/abc")).unwrap(),
                    old
                );
            }
            Stage::AfterRename => {
                assert!(matches!(error, TokenFileError::Unsynced(_)));
                assert_eq!(held.read().unwrap(), Some(json!({"token":"new"})));
            }
        }
    }
}
