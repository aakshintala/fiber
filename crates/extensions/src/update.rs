//! Staging `fiber update`: the whole release is fetched and checked, every
//! installed extension is staged under one lock hold, and the commit puts
//! them in place with zero downloads inside. Dropping the staged update
//! installs nothing and removes what was staged.

use std::fs;
use std::path::{Path, PathBuf};

use contract::clock::Clock;

use crate::Error;
use crate::git::Origin;
use crate::install::{Scratch, io, remove, unique_name};
use crate::installed::read;
use crate::manage::{Adopted, Plan, plan_updates};
use crate::prepare::prepare_binary;
use crate::release::{
    DOCS, EXTENSIONS, Release, fetch_verified, layout, stage_docs_from, swap_docs_in,
    unpack_release_archive,
};
use crate::resolve::nightly_parts;

/// The version compatibility checks stage against: a nightly target
/// stands in for its base triple, anything else stages as itself, so a
/// nightly release satisfies the same-base stable minimums its own
/// archives state.
fn compat(version: &str) -> String {
    match nightly_parts(version) {
        Ok((base, _)) => format!("{}.{}.{}", base[0], base[1], base[2]),
        Err(_) => version.to_owned(),
    }
}

/// The fiber repository holding the release tags.
const FIBER_REPO: &str = "github.com/aakshintala/fiber";

/// The release `fiber update` moves to: where its archives live, the
/// version, and the target triple selecting the binary archive. The
/// release tag's commit is resolved inside, never passed in.
pub struct Target {
    /// Where releases are published, such as
    /// `https://github.com/aakshintala/fiber/releases`.
    pub base: String,
    /// The release's version, such as `0.3.0`.
    pub version: String,
    /// The target triple, selecting the `fiber-<triple>.tar.gz` archive,
    /// such as `aarch64-apple-darwin`.
    pub triple: String,
}

/// The staged update: every installed extension resolved, the release's
/// archives unpacked and checked, the docs fresh beside `docs/`, the
/// staged binary in the scratch directory. Commit puts the extensions in
/// place and swaps the docs, then the caller renames the staged binary
/// last. Dropping it installs nothing and removes what was staged.
pub struct StagedUpdate<'c> {
    plan: Plan<'c>,
    _scratch: Scratch,
    home: PathBuf,
    docs_fresh: Option<PathBuf>,
    binary: PathBuf,
    binary_dir: Option<PathBuf>,
}

impl<'c> StagedUpdate<'c> {
    /// The extensions the commit will put in place.
    pub fn plan(&self) -> &Plan<'c> {
        &self.plan
    }

    /// Puts every staged extension in place with no download inside, swaps
    /// the staged docs in, and returns the staged `fiber` binary's path:
    /// the caller renames it into place last. The staged binary sits
    /// beside the installed one when its path was given, so that rename
    /// is same-filesystem and atomic; otherwise it sits in the home as
    /// before. The lock is released before the docs swap.
    pub fn commit(mut self) -> Result<PathBuf, Error> {
        self.plan.commit_prepared()?;
        self.plan.unlock();
        if let Some(fresh) = &self.docs_fresh {
            swap_docs_in(&self.home, fresh)?;
        }
        let staged = self
            .binary_dir
            .as_deref()
            .unwrap_or(&self.home)
            .join(format!("{}.new", unique_name(".fiber-update")));
        // The scratch directory can sit on another filesystem than the
        // staged path, so a rename across devices falls back to a copy.
        if let Err(rename) = fs::rename(&self.binary, &staged) {
            if rename.kind() == std::io::ErrorKind::CrossesDevices {
                fs::copy(&self.binary, &staged).map_err(io(&staged))?;
            } else {
                return Err(io(&staged)(rename));
            }
        }
        Ok(staged)
    }
}

impl Drop for StagedUpdate<'_> {
    fn drop(&mut self) {
        // The plan's drop removes the members' fresh copies and the
        // scratch guard removes the archives and unpack directories.
        if let Some(fresh) = &self.docs_fresh {
            remove(fresh).unwrap_or(());
        }
    }
}

/// Fetches and checks the release's binary, docs and first-party
/// extensions archives, resolves the release tag's commit, stages what
/// every installed extension moves to under one lock hold, replaces the
/// archive-covered members from the release, and prepares every staged
/// member, so the commit downloads nothing. A removed first-party
/// extension stays removed: only installed names are adopted from the
/// archives, keeping their requested and floor.
pub fn stage_update<'c>(
    home: &Path,
    release: &Target,
    origin: &Origin,
    clock: &'c dyn Clock,
    stage_docs: bool,
    installed_binary: Option<&Path>,
) -> Result<StagedUpdate<'c>, Error> {
    let scratch = Scratch::create(&std::env::temp_dir(), "fiber-update", 0o700)?;
    let archives = Release {
        base: release.base.clone(),
        version: release.version.clone(),
        // Fetching needs only the base and version; the tag's commit is
        // resolved below.
        commit: String::new(),
    };
    let binary_file = format!("fiber-{}.tar.gz", release.triple);
    let binary_archive = fetch_verified(&archives, &binary_file)?;
    let binary_dir = scratch.path().join("binary");
    fs::create_dir(&binary_dir).map_err(io(&binary_dir))?;
    unpack_release_archive(&binary_archive, &binary_dir, &binary_file)?;
    let binary = binary_dir.join("fiber");
    if !binary.is_file() {
        return Err(Error::BadArchive {
            archive: binary_file,
            why: "holds no `fiber` binary".into(),
        });
    }
    let extensions_archive = fetch_verified(&archives, EXTENSIONS)?;
    let extensions_dir = scratch.path().join("extensions");
    fs::create_dir(&extensions_dir).map_err(io(&extensions_dir))?;
    unpack_release_archive(&extensions_archive, &extensions_dir, EXTENSIONS)?;
    let shorts = layout(&extensions_dir, &compat(&release.version))?;
    let tag = format!("v{}", release.version);
    let commit = origin.tag_commit(FIBER_REPO, &tag, clock)?;
    let mut plan = plan_updates(home, &compat(&release.version), origin, clock)?;
    // The docs stage with the rest and swap after checks and approval:
    // an already-newest run whose docs are present fetches no docs
    // archive and stages none.
    let docs_fresh = stage_docs
        .then(|| {
            let archive = fetch_verified(&archives, DOCS)?;
            stage_docs_from(home, &archive, scratch.path())
        })
        .transpose()?;
    let installed = read(home)?;
    for short in &shorts {
        let name = config::full_name(short);
        if let Some(member) = installed.installed.iter().find(|i| i.name == name) {
            plan.adopt(
                home,
                &compat(&release.version),
                &Adopted {
                    installed: member,
                    version: &release.version,
                    commit: &commit,
                    source: &extensions_dir.join(short),
                },
            )?;
        }
    }
    // Only the member binaries pre-stage, before the person's approval:
    // each member's install step runs once after approval, at the commit.
    for item in plan.items() {
        prepare_binary(item.staged(), &item.manifest)?;
    }
    Ok(StagedUpdate {
        plan,
        _scratch: scratch,
        home: home.into(),
        docs_fresh,
        binary,
        binary_dir: installed_binary
            .and_then(Path::parent)
            .map(Path::to_path_buf),
    })
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
