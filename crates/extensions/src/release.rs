//! Putting a release's docs and first-party extensions in place in Fiber
//! home: the internal step `install.sh` runs (`docs/releasing.md`,
//! "Installing"). Every download and check finishes, in a scratch
//! directory, before anything in Fiber home is created, written or removed.
//! Then, under the extensions lock, each extension is staged beside
//! `extensions/` and the docs beside `docs/`, the extensions are renamed into
//! place, then the docs.

mod unpack;

use std::fs::{self};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use contract::clock::Clock;

use crate::Error;
use crate::install::{
    Provenance, Record, Scratch, check, commit_all, copy, io, remove, stage, unique_name,
};
use crate::installed::lock;
use crate::prepare::{Observe, download_observed};
use unpack::{LIMITS, unpack};

/// The release's docs archive.
pub(crate) const DOCS: &str = "fiber-docs.tar.gz";
/// The release's first-party extensions archive.
pub(crate) const EXTENSIONS: &str = "fiber-extensions.tar.gz";

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

/// Downloads `file` from the release and its `.sha256`, checks the bytes
/// against the checksum file, and returns the bytes. A download failure
/// names the file; a mismatch is [`Error::ArchiveChecksum`] naming the
/// archive. The observed entry writes one `extension_release` line per
/// GET on `observe`.
pub(crate) fn fetch_verified_observed(
    release: &Release,
    file: &str,
    observe: &Observe<'_>,
) -> Result<Vec<u8>, Error> {
    let fetched = fetch_observed(release, file, observe)?;
    verify(release, file, &fetched)?;
    Ok(fetched.bytes)
}

/// Unpacks the release archive `archive` into `into`, an empty directory
/// the caller made, holding the unpack to [`unpack`]'s limits. A refusal
/// is [`Error::BadArchive`] naming the archive.
pub(crate) fn unpack_release_archive(archive: &[u8], into: &Path, name: &str) -> Result<(), Error> {
    unpack(archive, into, name, &LIMITS)
}

/// Unpacks the release's docs archive into the scratch directory and
/// copies it to a fresh staging directory beside `docs/`, returning the
/// fresh directory. An archive holding no entries is [`Error::BadArchive`]
/// naming the docs archive, before anything in Fiber home is created.
pub(crate) fn stage_docs_from(
    home: &Path,
    archive: &[u8],
    scratch: &Path,
) -> Result<PathBuf, Error> {
    let into = scratch.join("docs");
    fs::create_dir(&into).map_err(io(&into))?;
    unpack(archive, &into, DOCS, &LIMITS)?;
    if into.read_dir().map_err(io(&into))?.next().is_none() {
        return Err(bad(DOCS, "holds no entries".into()));
    }
    let fresh = home.join(format!("{}.new", unique_name(".docs")));
    remove(&fresh)?;
    copy(&into, &fresh)?;
    Ok(fresh)
}

/// Swaps staged docs `fresh` into `docs/` in Fiber home, keeping the
/// previous docs when the swap cannot be finished.
pub(crate) fn swap_docs_in(home: &Path, fresh: &Path) -> Result<(), Error> {
    let old = home.join(format!("{}.old", unique_name(".docs")));
    swap_docs(fresh, &config::docs_dir(home), &old, &|from, to| {
        fs::rename(from, to)
    })
}

/// Puts the release's docs at `docs/` when it is absent: downloads and
/// checks the docs archive, unpacks it and swaps it in. A present `docs/`
/// is left alone without any download. The observed entry carries the
/// downloads' lines; [`install_docs_if_missing`] stays silent.
pub fn install_docs_if_missing(home: &Path, release: &Release) -> Result<(), Error> {
    install_docs_if_missing_observed(
        home,
        release,
        &Observe {
            debug: None,
            clock: &support::clock::System,
        },
    )
}

/// As [`install_docs_if_missing`], writing the downloads' lines on
/// `observe`.
pub fn install_docs_if_missing_observed(
    home: &Path,
    release: &Release,
    observe: &Observe<'_>,
) -> Result<(), Error> {
    if fs::symlink_metadata(config::docs_dir(home)).is_ok() {
        return Ok(());
    }
    let scratch = Scratch::create(&std::env::temp_dir(), "fiber-release-docs", 0o700)?;
    let archive = fetch_verified_observed(release, DOCS, observe)?;
    let fresh = stage_docs_from(home, &archive, scratch.path())?;
    swap_docs_in(home, &fresh)
}

/// The newest stable release tag: the tag `releases/latest` redirects
/// to. A fetch failure is [`Error::Download`] naming the URL. The
/// redirect target is read, never the body. The observed entry carries
/// the probe's line; [`latest_stable_tag`] stays silent.
pub fn latest_stable_tag(web_base: &str) -> Result<String, Error> {
    latest_stable_tag_observed(
        web_base,
        &Observe {
            debug: None,
            clock: &support::clock::System,
        },
    )
}

/// As [`latest_stable_tag`], writing the probe's line on `observe`.
pub fn latest_stable_tag_observed(web_base: &str, observe: &Observe<'_>) -> Result<String, Error> {
    use ureq::ResponseExt as _;
    let url = format!("{}/releases/latest", web_base.trim_end_matches('/'));
    let fail = |why: String| Error::Download {
        name: "stable".into(),
        why: format!("{url}: {why}"),
    };
    let agent = net::agent(
        net::config().proxy(ureq::Proxy::try_from_env()).build(),
        std::sync::Arc::new(()),
        ureq::unversioned::resolver::DefaultResolver::default(),
        net::LIMITS,
    );
    // The send stamp: `None` when silent, so no clock is read at info.
    let start = observe.stamp();
    let response = match agent.get(&url).call() {
        Ok(response) => response,
        Err(error) => {
            // A status-bearing failure keeps the status and the header
            // timing, as a download's does; a failure before a response
            // carries neither.
            if let ureq::Error::StatusCode(status) = &error {
                observe.line(&url, start, observe.stamp(), Some(*status), 0);
            } else {
                observe.line(&url, start, None, None, 0);
            }
            return Err(fail(error.to_string()));
        }
    };
    observe.line(
        &url,
        start,
        observe.stamp(),
        Some(response.status().as_u16()),
        0,
    );
    let tag = response
        .get_uri()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default();
    Ok(tag.to_owned())
}

/// The newest nightly release tag the release list names: the maximum
/// over (base version, date) across entries whose tag matches
/// `vMAJOR.MINOR.PATCH-nightly.YYYYMMDD` with `prerelease` true and
/// `draft` false. Anything else in the list is skipped; no match is
/// [`Error::NoNightly`] naming the channel, and a list that cannot be
/// fetched or read is [`Error::Download`] naming the nightly list's URL.
/// The observed entry carries the list's line; [`fetch_nightly_tag`]
/// stays silent.
pub fn fetch_nightly_tag(api_base: &str) -> Result<String, Error> {
    fetch_nightly_tag_observed(
        api_base,
        &Observe {
            debug: None,
            clock: &support::clock::System,
        },
    )
}

/// As [`fetch_nightly_tag`], writing the list's line on `observe`.
pub fn fetch_nightly_tag_observed(api_base: &str, observe: &Observe<'_>) -> Result<String, Error> {
    use crate::resolve::nightly_parts;
    let url = format!("{}/releases?per_page=100", api_base.trim_end_matches('/'));
    let bytes = download_observed("nightly", &url, observe)?;
    let list: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| Error::Download {
            name: "nightly".into(),
            why: format!("{url}: {err}"),
        })?;
    let Some(entries) = list.as_array() else {
        return Err(Error::Download {
            name: "nightly".into(),
            why: format!("{url}: the release list is not an array"),
        });
    };
    let mut best: Option<([u64; 3], String, String)> = None;
    for entry in entries {
        let (Some(tag), Some(true), Some(false)) = (
            entry.get("tag_name").and_then(|tag| tag.as_str()),
            entry.get("prerelease").and_then(|flag| flag.as_bool()),
            entry.get("draft").and_then(|flag| flag.as_bool()),
        ) else {
            continue;
        };
        let Ok((base, date)) = nightly_parts(tag) else {
            continue;
        };
        let newer = best.as_ref().is_none_or(|(held_base, held_date, _)| {
            (base, date.clone()) > (*held_base, held_date.clone())
        });
        if newer {
            best = Some((base, date, tag.to_owned()));
        }
    }
    best.map(|(_, _, tag)| tag).ok_or_else(|| Error::NoNightly {
        url,
        why: "the release list holds no matching pre-release".into(),
    })
}
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
    install_release_observed(
        home,
        release,
        clock,
        &Observe {
            debug: None,
            clock: &support::clock::System,
        },
    )
}

/// As [`install_release`], writing the downloads' lines on `observe`.
pub fn install_release_observed(
    home: &Path,
    release: &Release,
    clock: &dyn Clock,
    observe: &Observe<'_>,
) -> Result<Vec<String>, Error> {
    install_release_with(
        home,
        release,
        clock,
        &|from, to| fs::rename(from, to),
        observe,
    )
}

/// [`install_release`], with the rename the extensions commit and the docs
/// swap use.
pub(crate) fn install_release_with(
    home: &Path,
    release: &Release,
    clock: &dyn Clock,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
    observe: &Observe<'_>,
) -> Result<Vec<String>, Error> {
    let docs = fetch_observed(release, DOCS, observe)?;
    let extensions = fetch_observed(release, EXTENSIONS, observe)?;
    verify(release, DOCS, &docs)?;
    verify(release, EXTENSIONS, &extensions)?;
    let scratch = Scratch::create(&std::env::temp_dir(), "fiber-release", 0o700)?;
    let docs_dir = unpack_into(&scratch, &docs.bytes, DOCS, "docs")?;
    if fs::read_dir(&docs_dir)
        .map_err(io(&docs_dir))?
        .next()
        .is_none()
    {
        return Err(bad(DOCS, "holds no entries".into()));
    }
    let extensions_dir = unpack_into(&scratch, &extensions.bytes, EXTENSIONS, "extensions")?;
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
            floor: None,
        };
        let source = extensions_dir.join(short);
        let (_, _, paths) = stage(home, &source, &release.version, &record)?;
        staging.0.push(paths.fresh.clone());
        staged.push(paths);
        names.push(name);
    }
    let stem = unique_name(".docs");
    let fresh = home.join(format!("{stem}.new"));
    let old = home.join(format!("{stem}.old"));
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

/// One release file and its `.sha256` file, downloaded: one
/// `extension_release` line per GET on `observe`.
struct Fetched {
    bytes: Vec<u8>,
    sha256: Vec<u8>,
}

fn fetch_observed(release: &Release, file: &str, observe: &Observe<'_>) -> Result<Fetched, Error> {
    let sum = format!("{file}.sha256");
    Ok(Fetched {
        bytes: download_observed(file, &url(&release.base, &release.version, file), observe)?,
        sha256: download_observed(&sum, &url(&release.base, &release.version, &sum), observe)?,
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
    let got = support::sha256_hex(&fetched.bytes);
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
pub(crate) fn layout(dir: &Path, fiber_version: &str) -> Result<Vec<String>, Error> {
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

/// Unpacks `archive` into a new `<scratch>/<sub>/`.
fn unpack_into(scratch: &Scratch, archive: &[u8], name: &str, sub: &str) -> Result<PathBuf, Error> {
    let into = scratch.path().join(sub);
    fs::create_dir(&into).map_err(io(&into))?;
    unpack(archive, &into, name, &LIMITS)?;
    Ok(into)
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;
