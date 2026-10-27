//! `fiber release-install <version>`: the internal step `install.sh` runs
//! once the binary is in place (`docs/releasing.md`, "Installing"). It puts
//! this binary's own release's docs and first-party extensions in place in
//! Fiber home. `main` parses argv and dispatches here.

use std::io::{self, Write};
use std::path::Path;

use contract::clock::Clock;
use contract::releases::RELEASES;
use extensions::Release;

use crate::{CliError, finish};

/// `fiber release-install <version> [--base-url <url>]`: installs the docs
/// and first-party extensions of `version`, which must be this binary's own
/// `fiber_version`, recording `commit` as each extension's source. Fiber
/// home is resolved, and created only once everything has been checked.
/// `base_url` replaces [`RELEASES`] for tests.
pub fn release_install(
    version: &str,
    base_url: Option<&str>,
    fiber_version: &str,
    commit: Option<&str>,
    clock: &dyn Clock,
) -> i32 {
    let ran = config::fiber_home_path_from_env()
        .map_err(CliError::from)
        .and_then(|home| {
            run(
                &home,
                version,
                base_url,
                fiber_version,
                commit,
                clock,
                &mut io::stderr(),
            )
        });
    finish(ran)
}

/// Checks the version and the commit before any request, installs, and
/// writes one line per extension installed, then one for the docs.
fn run(
    home: &Path,
    version: &str,
    base_url: Option<&str>,
    fiber_version: &str,
    commit: Option<&str>,
    clock: &dyn Clock,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    if version != fiber_version {
        return Err(CliError::ReleaseVersionMismatch {
            fiber_version: fiber_version.to_owned(),
            version: version.to_owned(),
        });
    }
    let Some(commit) = commit.filter(|commit| !commit.is_empty()) else {
        return Err(CliError::ReleaseNoCommit);
    };
    let release = Release {
        base: base_url.unwrap_or(RELEASES).to_owned(),
        version: version.to_owned(),
        commit: commit.to_owned(),
    };
    let names = extensions::install_release(home, &release, clock)?;
    // A closed stderr leaves nobody to tell.
    for name in &names {
        writeln!(err, "fiber: installed {}", config::short_name(name)).unwrap_or(());
    }
    writeln!(err, "fiber: installed docs").unwrap_or(());
    Ok(())
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;
