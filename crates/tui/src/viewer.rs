//! Opening an image in the system viewer: the worker writes the
//! file to a private copy first, then runs `open` or `xdg-open` over
//! it and reaps it (`docs/tui.md`, "Images"). The copy lives under
//! `cache/images/` in Fiber home (`docs/state.md`), which is always
//! safe to delete.

use std::collections::hash_map::RandomState;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::hash::BuildHasher;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::clock::Clock;

use crate::Input;
use crate::helper::Ended;
use crate::image::View;

/// The program opening a file: `open` on macOS, `xdg-open` elsewhere.
pub(crate) fn command(macos: bool) -> Vec<String> {
    vec![if macos { "open" } else { "xdg-open" }.to_owned()]
}

/// Opens `view` on a worker thread, answering once with
/// [`Input::Viewed`]: nothing on success, or why the open failed. A
/// failed write removes its temporary file, and a second open
/// replaces the copy through a rename, so a viewer reading the
/// previous copy sees it whole.
pub(crate) fn spawn(
    argv: &[String],
    dir: &Path,
    view: View,
    out: &Sender<Input>,
    clock: &Arc<dyn Clock>,
) {
    let argv = argv.to_owned();
    let dir = dir.to_path_buf();
    let out = out.clone();
    let clock = Arc::clone(clock);
    // The worker outlives the frame that asked: it answers on the
    // loop's channel, which a closed loop drops.
    drop(crate::sources::builder("tui-viewer").spawn(move || {
        let id = view.id;
        let name = view.name.clone();
        let generation = view.generation;
        let result = open(&argv, &dir, &view, &clock);
        drop(out.send(Input::Viewed {
            id,
            name,
            generation,
            result,
        }));
    }));
}

/// Writes `view`'s copy, then runs the viewer over it.
fn open(argv: &[String], dir: &Path, view: &View, clock: &Arc<dyn Clock>) -> Result<(), String> {
    let name = file_name(&view.name)?;
    let bytes = STANDARD
        .decode(&view.data)
        .map_err(|error| format!("`{name}` is not base64: {error}"))?;
    let path = write_copy(dir, &view.session, name, &bytes)?;
    run(argv, &path, clock)
}

/// The file name the copy is written under: only the last path
/// component, never a path the hub chose, so a stray `..` cannot
/// escape the directory.
fn file_name(name: &str) -> Result<&str, String> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(format!("`{name}` is not a file name"));
    }
    Ok(name)
}

/// Writes `bytes` to `dir/<session>-<name>` through a rename, so a
/// viewer reading the previous copy sees it whole. The directory is
/// 0700 and the copy 0600, enforced on an existing directory too.
fn write_copy(
    dir: &Path,
    session: &str,
    name: &str,
    bytes: &[u8],
) -> Result<std::path::PathBuf, String> {
    fs::create_dir_all(dir)
        .map_err(|error| format!("`{}` could not be written: {error}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("`{}` could not be written: {error}", dir.display()))?;
    let tmp = dir.join(format!(
        ".{session}-{name}.{:016x}.tmp",
        RandomState::new().hash_one(name)
    ));
    let wrote = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|error| format!("`{name}` could not be written: {error}"))
        .and_then(|mut file| {
            file.write_all(bytes)
                .map_err(|error| format!("`{name}` could not be written: {error}"))
        });
    if let Err(error) = wrote {
        fs::remove_file(&tmp).unwrap_or(());
        return Err(error);
    }
    let path = dir.join(format!("{session}-{name}"));
    if let Err(error) = fs::rename(&tmp, &path) {
        fs::remove_file(&tmp).unwrap_or(());
        return Err(format!("`{name}` could not be written: {error}"));
    }
    Ok(path)
}

/// Runs the viewer over `path` under the helper's deadline and reaps it: a
/// spawn error or a non-zero exit is why the open failed, and nothing
/// retries. A viewer still running at the deadline is stopped and is not a
/// failure.
fn run(argv: &[String], path: &Path, clock: &Arc<dyn Clock>) -> Result<(), String> {
    let program = argv
        .first()
        .ok_or_else(|| "no program opens images".to_owned())?;
    let argv: Vec<OsString> = argv
        .iter()
        .map(OsString::from)
        .chain([path.as_os_str().to_owned()])
        .collect();
    match crate::helper::run_bounded(&argv, None, clock) {
        Ok(Ended::Exited(status)) if !status.success() => {
            Err(format!("`{program}` exited with {status}"))
        }
        Ok(Ended::Exited(_) | Ended::DeadlinePassed) => Ok(()),
        Err(error) => Err(format!("`{program}` could not run: {error}")),
    }
}

#[cfg(test)]
#[path = "viewer_tests.rs"]
mod tests;
