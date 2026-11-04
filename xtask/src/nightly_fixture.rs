//! The `nightly-fixture` subcommand (`docs/ci.md`, "Releases"): prunes a
//! bare clone down to nightly release tags, so the probe updates against a
//! deterministic nightly-only origin. Both `--keep` versions are bare
//! captured values: each is matched through [`tag_for_version`], the one
//! shared version-to-tag conversion, following
//! `crates/extensions/src/update.rs`.

use std::path::Path;
use std::process::Command;

use super::nightly_pair::{is_nightly_tag, tag_for_version};

/// The subcommand's usage line, shared by every arity and flag fault.
const USAGE: &str = "usage: cargo xtask nightly-fixture --repo <dir> --keep <bare-version>...";

/// Lists every tag in `repo`.
fn list_tags(repo: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("tag")
        .arg("--list")
        .output()
        .map_err(|error| {
            format!(
                "nightly-fixture: could not list tags in {}: {error}",
                repo.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "nightly-fixture: could not list tags in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// Deletes `tag` from `repo`.
fn delete_tag(repo: &Path, tag: &str) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("tag")
        .arg("-d")
        .arg(tag)
        .output()
        .map_err(|error| {
            format!(
                "nightly-fixture: could not delete tag {tag} in {}: {error}",
                repo.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "nightly-fixture: could not delete tag {tag} in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Prunes `repo` to the keeps only: deletes every tag that is not one of
/// the bare keeps (nightly or not) and returns the remaining count. Every
/// bare keep must resolve to an existing nightly tag; a missing or
/// non-nightly keep errors naming it before anything is deleted.
pub(crate) fn prune(repo: &Path, keeps_bare: &[String]) -> Result<usize, String> {
    let tags = list_tags(repo)?;
    let mut keep_tags = Vec::with_capacity(keeps_bare.len());
    for keep in keeps_bare {
        let tag = tag_for_version(keep);
        if !is_nightly_tag(&tag) {
            return Err(format!(
                "nightly-fixture: --keep {keep} is not a nightly version; the fixture keeps nightly tags only"
            ));
        }
        if !tags.iter().any(|listed| listed == &tag) {
            return Err(format!(
                "nightly-fixture: {} holds no tag {tag} for --keep {keep}",
                repo.display()
            ));
        }
        keep_tags.push(tag);
    }
    let mut remaining = 0;
    for tag in &tags {
        if keep_tags.iter().any(|kept| kept == tag) {
            remaining += 1;
            continue;
        }
        delete_tag(repo, tag)?;
    }
    Ok(remaining)
}

/// Runs the subcommand over `args`, printing the remaining tag count.
pub(crate) fn run(args: &[String]) -> Result<bool, String> {
    let mut repo: Option<String> = None;
    let mut keeps: Vec<String> = Vec::new();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        if flag == "--repo" {
            let dir = rest
                .next()
                .ok_or_else(|| "--repo needs a directory".to_owned())?;
            repo = Some(dir.clone());
        } else if flag == "--keep" {
            let keep = rest
                .next()
                .ok_or_else(|| "--keep needs a version".to_owned())?;
            keeps.push(keep.clone());
        } else {
            return Err(format!("{USAGE}; unknown flag {flag}"));
        }
    }
    let Some(repo) = repo else {
        return Err(format!("{USAGE}; --repo is required"));
    };
    if keeps.is_empty() {
        return Err(format!("{USAGE}; --keep is required at least once"));
    }
    let remaining = prune(Path::new(&repo), &keeps)?;
    println!("{remaining}");
    Ok(true)
}

#[cfg(test)]
#[path = "nightly_fixture_tests.rs"]
mod tests;
