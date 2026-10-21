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
use crate::prepare::prepare;
use crate::release::{
    DOCS, EXTENSIONS, Release, fetch_verified, layout, stage_docs_from, swap_docs_in,
    unpack_release_archive,
};

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
    docs_fresh: PathBuf,
    binary: PathBuf,
}

impl<'c> StagedUpdate<'c> {
    /// The extensions the commit will put in place.
    pub fn plan(&self) -> &Plan<'c> {
        &self.plan
    }

    /// Puts every staged extension in place with no download inside, swaps
    /// the staged docs in, and returns the staged `fiber` binary's path:
    /// the caller renames it into place last. The lock is released before
    /// the docs swap.
    pub fn commit(mut self) -> Result<PathBuf, Error> {
        self.plan.commit_prepared()?;
        self.plan.unlock();
        swap_docs_in(&self.home, &self.docs_fresh)?;
        let staged = self
            .home
            .join(format!("{}.new", unique_name(".fiber-update")));
        fs::rename(&self.binary, &staged).map_err(io(&staged))?;
        Ok(staged)
    }
}

impl Drop for StagedUpdate<'_> {
    fn drop(&mut self) {
        // The plan's drop removes the members' fresh copies and the
        // scratch guard removes the archives and unpack directories.
        remove(&self.docs_fresh).unwrap_or(());
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
    let docs_archive = fetch_verified(&archives, DOCS)?;
    let extensions_archive = fetch_verified(&archives, EXTENSIONS)?;
    let extensions_dir = scratch.path().join("extensions");
    fs::create_dir(&extensions_dir).map_err(io(&extensions_dir))?;
    unpack_release_archive(&extensions_archive, &extensions_dir, EXTENSIONS)?;
    let shorts = layout(&extensions_dir, &release.version)?;
    let tag = format!("v{}", release.version);
    let commit = origin.tag_commit(FIBER_REPO, &tag, clock)?;
    let mut plan = plan_updates(home, &release.version, origin, clock)?;
    let docs_fresh = stage_docs_from(home, &docs_archive, scratch.path())?;
    let installed = read(home)?;
    for short in &shorts {
        let name = config::full_name(short);
        if let Some(member) = installed.installed.iter().find(|i| i.name == name) {
            plan.adopt(
                home,
                &release.version,
                &Adopted {
                    installed: member,
                    version: &release.version,
                    commit: &commit,
                    source: &extensions_dir.join(short),
                },
            )?;
        }
    }
    for item in plan.items() {
        prepare(item.staged(), &item.manifest, clock)?;
    }
    Ok(StagedUpdate {
        plan,
        _scratch: scratch,
        home: home.into(),
        docs_fresh,
        binary,
    })
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
