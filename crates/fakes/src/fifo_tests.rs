use std::os::unix::fs::FileTypeExt as _;

use super::fifo;
use crate::TempDir;

#[test]
fn makes_a_fifo() {
    let dir = TempDir::new("fiber-fifo");
    let path = dir.path().join("pipe");
    fifo(&path);
    let file_type = std::fs::symlink_metadata(&path).unwrap().file_type();
    assert!(file_type.is_fifo(), "{} is not a fifo", path.display());
    assert!(!file_type.is_file(), "{} is a regular file", path.display());
}

#[test]
fn an_existing_path_fails_naming_the_path() {
    let dir = TempDir::new("fiber-fifo-exists");
    let path = dir.path().join("pipe");
    fifo(&path);
    let shown = path.display().to_string();
    let failed =
        std::panic::catch_unwind(|| fifo(&path)).expect_err("a second fifo at one path fails");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the path: {message}"
    );
}

#[test]
fn a_missing_parent_fails_naming_the_path() {
    let dir = TempDir::new("fiber-fifo-parent");
    let path = dir.path().join("nope/pipe");
    let shown = path.display().to_string();
    let failed = std::panic::catch_unwind(|| fifo(&path)).expect_err("a fifo with no parent fails");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the path: {message}"
    );
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else {
        "a non-string panic".to_owned()
    }
}
