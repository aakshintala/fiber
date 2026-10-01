//! `fiber install <local path>` (`docs/extensions.md`, "Installing"): the
//! extension's directory is copied whole into `extensions/<name>/` in Fiber
//! home, and renamed into place only once every check has passed, so a
//! failed install leaves nothing behind.

use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::{API, Error};

/// Installs the extension at `source` into Fiber home, replacing an installed
/// copy of the same name, and returns its name. It refuses an extension whose
/// manifest needs a newer Fiber than `fiber_version` or a different API, and
/// one whose provider data does not read.
pub fn install(home: &Path, source: &Path, fiber_version: &str) -> Result<String, Error> {
    let manifest = config::read_manifest(source)?;
    if version(&manifest.fiber)? > version(fiber_version)? {
        return Err(Error::NeedsNewerFiber {
            name: manifest.name,
            needs: manifest.fiber,
            running: fiber_version.into(),
        });
    }
    if manifest.api != API {
        return Err(Error::ApiVersion {
            name: manifest.name,
            api: manifest.api,
        });
    }
    let slug = slug(&manifest.name)?;
    let root = home.join("extensions");
    let target = root.join(&slug);
    let fresh = root.join(format!(".{slug}.{}.new", std::process::id()));
    let old = root.join(format!(".{slug}.{}.old", std::process::id()));
    remove(&fresh)?;
    let copied =
        copy(source, &fresh).and_then(|()| config::read_providers(&fresh).map_err(Error::from));
    if let Err(e) = copied {
        remove(&fresh)?;
        return Err(e);
    }
    let replacing = fs::symlink_metadata(&target).is_ok();
    if replacing {
        fs::rename(&target, &old).map_err(io(&target))?;
    }
    fs::rename(&fresh, &target).map_err(io(&target))?;
    if replacing {
        remove(&old)?;
    }
    Ok(manifest.name)
}

/// `extensions/<name>/`'s directory name: every `/` becomes `-`, as for a
/// project key (`docs/state.md`, "Extensions").
fn slug(name: &str) -> Result<String, Error> {
    let slug = name.replace('/', "-");
    if slug.is_empty() || slug.starts_with('.') || slug.contains('\0') {
        return Err(Error::BadName { name: name.into() });
    }
    Ok(slug)
}

/// `0.3.0` or `v0.3.0` as three numbers compared in order
/// (`docs/dependencies.md`, "Written ourselves").
fn version(text: &str) -> Result<[u64; 3], Error> {
    let bad = || Error::BadVersion { text: text.into() };
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut out = [0; 3];
    for slot in &mut out {
        *slot = parts.next().and_then(|p| p.parse().ok()).ok_or_else(bad)?;
    }
    match parts.next() {
        Some(_) => Err(bad()),
        None => Ok(out),
    }
}

/// Copies a directory tree, keeping symbolic links as links.
fn copy(from: &Path, to: &Path) -> Result<(), Error> {
    fs::create_dir_all(to).map_err(io(to))?;
    for entry in fs::read_dir(from).map_err(io(from))? {
        let entry = entry.map_err(io(from))?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(io(&src))?;
        if kind.is_dir() {
            copy(&src, &dst)?;
        } else if kind.is_symlink() {
            symlink(fs::read_link(&src).map_err(io(&src))?, &dst).map_err(io(&dst))?;
        } else {
            fs::copy(&src, &dst).map_err(io(&dst))?;
        }
    }
    Ok(())
}

fn remove(dir: &Path) -> Result<(), Error> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(io(dir)(e)),
        Ok(()) | Err(_) => Ok(()),
    }
}

fn io(path: &Path) -> impl Fn(io::Error) -> Error {
    let path: PathBuf = path.to_path_buf();
    move |source| Error::Io {
        path: path.clone(),
        source,
    }
}
