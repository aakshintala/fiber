//! The person's home for skills (`docs/system-prompt.md`, "The
//! person's files"): where `~/.agents/skills` is read from.
//! The docs line's place (`docs/system-prompt.md`, "The docs line"):
//! the installed `docs/` tree, or the website when it is missing.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The website's docs address for a stable build
/// (`docs/ci.md`, "The user docs site").
const STABLE_DOCS: &str = "https://aakshintala.com/fiber/";
/// The website's docs address for a nightly build
/// (`docs/ci.md`, "The user docs site").
const NIGHTLY_DOCS: &str = "https://aakshintala.com/fiber/nightly/";

/// Where the session's docs line points: the installed `docs/` tree
/// when it is a directory in Fiber home, else the website, nightly for
/// a nightly build. `fiber doctor` reports the same missing `docs/`.
pub(crate) fn docs_place(home: &Path, version: &str) -> r#loop::DocsPlace {
    let docs = config::docs_dir(home);
    if docs.is_dir() {
        r#loop::DocsPlace::Installed(docs)
    } else if extensions::nightly_parts(version).is_ok() {
        r#loop::DocsPlace::Website(NIGHTLY_DOCS)
    } else {
        r#loop::DocsPlace::Website(STABLE_DOCS)
    }
}

/// The person's home, where `~/.agents/skills` is read from: the `HOME`
/// value, when it is set to an absolute path. Unset, empty or relative
/// leaves that place unread.
pub(crate) fn agents_home(home_env: Option<OsString>) -> Option<PathBuf> {
    home_env
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

/// The already-read check the import runs
/// (`docs/configuration.md`, "Importing"): discovery's own code over
/// Fiber home and the person's home, so a skill discovery would load
/// from another personal source is left behind instead of copied. This
/// module owns the person's-home rule above, so it builds the check.
pub(crate) fn personal_source_check(
    fiber_home: PathBuf,
    agents_home: Option<PathBuf>,
    clock: Arc<dyn contract::clock::Clock>,
) -> Arc<dyn contract::skills::PersonalSourceCheck> {
    Arc::new(move |dirs: &[PathBuf]| {
        r#loop::personal_source_paths(
            &fiber_home,
            agents_home.as_deref(),
            dirs,
            Arc::clone(&clock),
        )
    })
}

#[cfg(test)]
#[path = "prompt_files_tests.rs"]
mod tests;
