//! Tests for the viewer worker: its program per system, the copy
//! it writes, and every way an open fails.

use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;
use std::time::Duration;

use base64::Engine as _;

use super::{command, spawn};
use crate::Input;
use crate::image::View;

/// One deadline per receive: the worker answers promptly.
const DEADLINE: Duration = Duration::from_secs(5);

/// A view of `bytes` under `name`.
fn view(name: &str, bytes: &[u8]) -> View {
    View {
        id: 1,
        name: name.to_owned(),
        session: "s_aaaaaaaaaaaaaaaa".to_owned(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        generation: 1,
    }
}

/// Spawns the worker and waits for its answer.
fn opened(argv: &[String], dir: &std::path::Path, view: View) -> Result<(), String> {
    let (out, rx) = mpsc::channel();
    spawn(argv, dir, view, &out);
    match rx.recv_timeout(DEADLINE) {
        Ok(Input::Viewed { result, .. }) => result,
        Ok(
            Input::Bytes(_)
            | Input::Hub(_)
            | Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::Resize
            | Input::Files { .. }
            | Input::FindDue(_)
            | Input::Image { .. }
            | Input::Models(_)
            | Input::Tick,
        ) => panic!("the worker answered something else"),
        Err(err) => panic!("waited {DEADLINE:?} for the worker: {err}"),
    }
}

#[test]
fn the_command_per_system() {
    assert_eq!(command(true), ["open".to_owned()]);
    assert_eq!(command(false), ["xdg-open".to_owned()]);
}

#[test]
fn spawn_writes_the_file_0600_in_a_0700_dir_and_runs_the_viewer() {
    let dir = fakes::TempDir::new("fiber-viewer-open");
    let images = dir.path().join("images");
    let marker = dir.path().join("opened");
    // The viewer prints the copy's path to the marker file: the test
    // runs a script it wrote through `/bin/sh`, never directly.
    let program = fakes::script(dir.path(), "viewer", "echo \"$2\" > \"$1\"\n");
    let argv = [
        "/bin/sh".to_owned(),
        program.to_str().unwrap_or_default().to_owned(),
        marker.to_str().unwrap_or_default().to_owned(),
    ];
    let result = opened(&argv, &images, view("shot.png", b"bytes"));
    assert_eq!(result, Ok(()));
    let mode = std::fs::metadata(&images)
        .unwrap_or_else(|err| panic!("stat: {err}"))
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
    let path = images.join("s_aaaaaaaaaaaaaaaa-shot.png");
    let mode = std::fs::metadata(&path)
        .unwrap_or_else(|err| panic!("stat: {err}"))
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(
        std::fs::read(&path).unwrap_or_else(|err| panic!("read: {err}")),
        b"bytes"
    );
    assert_eq!(
        std::fs::read_to_string(&marker)
            .unwrap_or_else(|err| panic!("read: {err}"))
            .trim(),
        path.to_str().unwrap_or_default()
    );
}

#[test]
fn a_viewer_that_exits_non_zero_is_an_error() {
    let dir = fakes::TempDir::new("fiber-viewer-fails");
    let program = fakes::script(dir.path(), "viewer", "exit 3\n");
    let argv = [
        "/bin/sh".to_owned(),
        program.to_str().unwrap_or_default().to_owned(),
    ];
    let result = opened(&argv, dir.path(), view("shot.png", b"bytes"));
    assert!(result.is_err());
}

#[test]
fn a_missing_program_is_an_error() {
    let dir = fakes::TempDir::new("fiber-viewer-missing");
    let argv = ["/nonexistent/fiber-viewer-opens".to_owned()];
    let result = opened(&argv, dir.path(), view("shot.png", b"bytes"));
    assert!(result.is_err());
}

#[test]
fn no_viewer_program_is_an_error() {
    // With no opener on PATH the loop queues nothing to run: the
    // worker says why instead of spawning nothing.
    let dir = fakes::TempDir::new("fiber-viewer-none");
    let result = opened(&[], dir.path(), view("shot.png", b"bytes"));
    assert!(result.is_err());
}

#[test]
fn a_name_with_a_slash_is_refused() {
    let dir = fakes::TempDir::new("fiber-viewer-name");
    let argv = ["true".to_owned()];
    // The file name is the path's last component: a stray `..` cannot
    // escape the directory either.
    for name in ["a/b.png", "..", ".", ""] {
        let result = opened(&argv, dir.path(), view(name, b"bytes"));
        assert!(result.is_err(), "{name}");
    }
    let left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap_or_else(|err| panic!("read: {err}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|err| panic!("entry: {err}"))
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(left.is_empty());
}

#[test]
fn malformed_base64_is_an_error() {
    let dir = fakes::TempDir::new("fiber-viewer-base64");
    let argv = ["true".to_owned()];
    let mut bad = view("shot.png", b"bytes");
    bad.data = "!!!".to_owned();
    assert!(opened(&argv, dir.path(), bad).is_err());
}

#[test]
fn a_second_open_replaces_the_copy_through_a_rename() {
    use std::os::unix::fs::MetadataExt;
    let dir = fakes::TempDir::new("fiber-viewer-replace");
    let argv = ["true".to_owned()];
    assert_eq!(
        opened(&argv, dir.path(), view("shot.png", b"first")),
        Ok(())
    );
    let first = std::fs::metadata(dir.path().join("s_aaaaaaaaaaaaaaaa-shot.png"))
        .unwrap_or_else(|err| panic!("stat: {err}"))
        .ino();
    assert_eq!(
        opened(&argv, dir.path(), view("shot.png", b"second")),
        Ok(())
    );
    let second = std::fs::metadata(dir.path().join("s_aaaaaaaaaaaaaaaa-shot.png"))
        .unwrap_or_else(|err| panic!("stat: {err}"))
        .ino();
    // The rename replaced the copy: a new inode, and no `.tmp` file
    // remains beside it.
    assert_ne!(first, second);
    assert_eq!(
        std::fs::read(dir.path().join("s_aaaaaaaaaaaaaaaa-shot.png"))
            .unwrap_or_else(|err| panic!("read: {err}")),
        b"second"
    );
    let left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap_or_else(|err| panic!("read: {err}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|err| panic!("entry: {err}"))
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(left, ["s_aaaaaaaaaaaaaaaa-shot.png"]);
}

#[test]
fn a_failed_write_leaves_no_temporary_file() {
    let dir = fakes::TempDir::new("fiber-viewer-readonly");
    // A read-only parent: the images directory cannot be created, so
    // the write fails before any temporary file exists.
    let parent = dir.path().join("ro");
    std::fs::create_dir_all(&parent).unwrap_or_else(|err| panic!("mkdir: {err}"));
    let mut permissions = std::fs::metadata(&parent)
        .unwrap_or_else(|err| panic!("stat: {err}"))
        .permissions();
    permissions.set_mode(0o500);
    std::fs::set_permissions(&parent, permissions).unwrap_or_else(|err| panic!("chmod: {err}"));
    let argv = ["true".to_owned()];
    let result = opened(&argv, &parent.join("images"), view("shot.png", b"bytes"));
    assert!(result.is_err());
    let left: Vec<String> = std::fs::read_dir(&parent)
        .unwrap_or_else(|err| panic!("read: {err}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|err| panic!("entry: {err}"))
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(left.is_empty());
    let mut permissions = std::fs::metadata(&parent)
        .unwrap_or_else(|err| panic!("stat: {err}"))
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&parent, permissions).unwrap_or_else(|err| panic!("chmod: {err}"));
}
