//! Putting a release's docs and first-party extensions in place in Fiber
//! home: the internal step `install.sh` runs (`docs/releasing.md`,
//! "Installing"). Every download and check finishes, in a scratch
//! directory, before anything in Fiber home is created, written or removed.
//! Then, under the extensions lock, each extension is staged beside
//! `extensions/` and the docs beside `docs/`, the extensions are renamed into
//! place, then the docs.

mod unpack;

use std::fs::{self, DirBuilder};
use std::io::{self, ErrorKind};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use contract::clock::Clock;

use crate::Error;
use crate::install::{Provenance, Record, check, commit_all, copy, io, remove, stage};
use crate::installed::lock;
use crate::prepare::{download, hex};
use unpack::{LIMITS, unpack};

/// The release's docs archive.
pub(crate) const DOCS: &str = "fiber-docs.tar.gz";
/// The release's first-party extensions archive.
pub(crate) const EXTENSIONS: &str = "fiber-extensions.tar.gz";

/// How many scratch directory names a call tries before it fails.
const SCRATCH_TRIES: usize = 64;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The release whose docs and first-party extensions are put in place.
#[derive(Debug, Clone)]
pub struct Release {
    /// Where releases are published, such as
    /// `https://github.com/aakshintala/fiber/releases`.
    pub base: String,
    /// The release's version, which is the running binary's own, such as
    /// `0.3.0`.
    pub version: String,
    /// The commit the binary was built from, recorded as each extension's
    /// `source.commit`.
    pub commit: String,
}

/// Downloads the release's docs and extensions archives, checks each
/// SHA-256, unpacks and checks them, then puts each first-party extension
/// in place under `extensions/` and the docs at `docs/` in Fiber home,
/// creating Fiber home if it is missing. Returns each extension installed,
/// by full name, sorted. A failure before the first rename leaves Fiber
/// home as it was.
pub fn install_release(
    home: &Path,
    release: &Release,
    clock: &dyn Clock,
) -> Result<Vec<String>, Error> {
    install_release_with(home, release, clock, &|from, to| fs::rename(from, to))
}

/// [`install_release`], with the rename the extensions commit and the docs
/// swap use.
pub(crate) fn install_release_with(
    home: &Path,
    release: &Release,
    clock: &dyn Clock,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Result<Vec<String>, Error> {
    let docs = fetch(release, DOCS)?;
    let extensions = fetch(release, EXTENSIONS)?;
    verify(release, DOCS, &docs)?;
    verify(release, EXTENSIONS, &extensions)?;
    let scratch = scratch(&std::env::temp_dir())?;
    let docs_dir = scratch.unpack(&docs.bytes, DOCS, "docs")?;
    if fs::read_dir(&docs_dir)
        .map_err(io(&docs_dir))?
        .next()
        .is_none()
    {
        return Err(bad(DOCS, "holds no entries".into()));
    }
    let extensions_dir = scratch.unpack(&extensions.bytes, EXTENSIONS, "extensions")?;
    let shorts = layout(&extensions_dir, &release.version)?;

    config::create_fiber_home(home)?;
    let _lock = lock(home, clock)?;
    let mut staging = Staging(Vec::new());
    let mut staged = Vec::new();
    let mut names = Vec::new();
    for short in &shorts {
        let name = config::full_name(short);
        let record = Record {
            name: name.clone(),
            provenance: Provenance::Git {
                commit: release.commit.clone(),
            },
            version: release.version.clone(),
            requested: true,
        };
        let source = extensions_dir.join(short);
        let (_, _, paths) = stage(home, scratch.n, &source, &release.version, &record)?;
        staging.0.push(paths.fresh.clone());
        staged.push(paths);
        names.push(name);
    }
    let pid = std::process::id();
    let fresh = home.join(format!(".docs.{pid}.{}.new", scratch.n));
    let old = home.join(format!(".docs.{pid}.{}.old", scratch.n));
    remove(&fresh)?;
    staging.0.push(fresh.clone());
    copy(&docs_dir, &fresh)?;
    commit_all(&staged, rename, |_, _| Ok(()))?;
    swap_docs(&fresh, &home.join("docs"), &old, rename)?;
    names.sort();
    Ok(names)
}

/// The staging directories this call made, removed when it ends. A
/// successful rename has already moved each one away.
struct Staging(Vec<PathBuf>);

impl Drop for Staging {
    fn drop(&mut self) {
        for dir in &self.0 {
            remove(dir).unwrap_or(());
        }
    }
}

/// One release file and its `.sha256` file, downloaded.
struct Fetched {
    bytes: Vec<u8>,
    sha256: Vec<u8>,
}

fn fetch(release: &Release, file: &str) -> Result<Fetched, Error> {
    let sum = format!("{file}.sha256");
    Ok(Fetched {
        bytes: download(file, &url(&release.base, &release.version, file))?,
        sha256: download(&sum, &url(&release.base, &release.version, &sum))?,
    })
}

/// Checks a downloaded archive against its `.sha256` file.
fn verify(release: &Release, file: &str, fetched: &Fetched) -> Result<(), Error> {
    let sum = format!("{file}.sha256");
    let expected = std::str::from_utf8(&fetched.sha256)
        .ok()
        .and_then(expected_digest)
        .ok_or_else(|| {
            bad(
                &sum,
                "does not start with a SHA-256 of 64 hex digits".into(),
            )
        })?;
    let got = hex(ring::digest::digest(&ring::digest::SHA256, &fetched.bytes).as_ref());
    if got != expected {
        return Err(Error::ArchiveChecksum {
            archive: file.into(),
            url: url(&release.base, &release.version, file),
        });
    }
    Ok(())
}

/// Where a release file is: `<base>/download/v<version>/<file>`.
pub(crate) fn url(base: &str, version: &str, file: &str) -> String {
    format!("{}/download/v{version}/{file}", base.trim_end_matches('/'))
}

/// The digest a `.sha256` file holds: its first whitespace-separated field,
/// which must be exactly 64 hex digits in either case, lowercased. That
/// reads a bare digest and `sha256sum`'s `<digest>  <name>`.
pub(crate) fn expected_digest(text: &str) -> Option<String> {
    let field = text.split_whitespace().next()?;
    (field.len() == 64 && field.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| field.to_ascii_lowercase())
}

fn bad(archive: &str, why: String) -> Error {
    Error::BadArchive {
        archive: archive.into(),
        why,
    }
}

/// Checks the extensions archive's top level: only directories, each a
/// first-party short name whose manifest names that extension, declares no
/// binaries and no install step, and passes every check staging makes.
/// Returns the short names, sorted.
fn layout(dir: &Path, fiber_version: &str) -> Result<Vec<String>, Error> {
    let mut shorts = Vec::new();
    for entry in fs::read_dir(dir).map_err(io(dir))? {
        let entry = entry.map_err(io(dir))?;
        let path = entry.path();
        let short = entry.file_name().to_string_lossy().into_owned();
        let refuse = |why: &str| bad(EXTENSIONS, format!("`{short}` {why}"));
        if !fs::symlink_metadata(&path).map_err(io(&path))?.is_dir() {
            return Err(refuse("at the top level is not a directory"));
        }
        let name = config::full_name(&short);
        if name == short {
            return Err(refuse("is not a first-party extension's short name"));
        }
        let (manifest, _) = check(&path, fiber_version)?;
        if manifest.name != name {
            return Err(refuse(&format!(
                "holds a manifest that names `{}`",
                manifest.name
            )));
        }
        if !manifest.binaries.is_empty() || manifest.install.is_some() {
            return Err(refuse("declares binaries or an install step"));
        }
        shorts.push(short);
    }
    if shorts.is_empty() {
        return Err(bad(EXTENSIONS, "holds no extension".into()));
    }
    shorts.sort();
    Ok(shorts)
}

/// Moves `fresh` to `target`. A previous `target` is moved to `old` first,
/// moved back if `fresh` cannot take its place, and removed once it has. If
/// it cannot be moved back either, it stays at `old`, and the error names
/// it.
fn swap_docs(
    fresh: &Path,
    target: &Path,
    old: &Path,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), Error> {
    let replacing = match fs::symlink_metadata(target) {
        Ok(_) => true,
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(e) => return Err(io(target)(e)),
    };
    if replacing {
        rename(target, old).map_err(io(target))?;
    }
    if let Err(e) = rename(fresh, target) {
        let error = io(target)(e);
        if replacing && rename(old, target).is_err() {
            return Err(Error::Rollback {
                why: error.to_string(),
                stuck: vec![old.to_path_buf()],
            });
        }
        return Err(error);
    }
    if replacing {
        remove(old)?;
    }
    Ok(())
}

/// A directory only this call uses, removed when it is dropped. `n` also
/// names this call's staging directories.
struct Scratch {
    path: PathBuf,
    n: usize,
}

impl Scratch {
    /// Unpacks `archive` into a new `<scratch>/<sub>/`.
    fn unpack(&self, archive: &[u8], name: &str, sub: &str) -> Result<PathBuf, Error> {
        let into = self.path.join(sub);
        fs::create_dir(&into).map_err(io(&into))?;
        unpack(archive, &into, name, &LIMITS)?;
        Ok(into)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        remove(&self.path).unwrap_or(());
    }
}

/// Makes `<dir>/fiber-release-<pid>-<n>`, mode 0700, with a name no other
/// call has: a name already taken moves on to the next `n`.
fn scratch(dir: &Path) -> Result<Scratch, Error> {
    let pid = std::process::id();
    for _ in 0..SCRATCH_TRIES {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("fiber-release-{pid}-{n}"));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(Scratch { path, n }),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(&path)(e)),
        }
    }
    Err(io(dir)(io::Error::other(
        "every scratch directory name tried is taken",
    )))
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;
