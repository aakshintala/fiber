//! The sandbox-setup cost (`docs/performance.md`): one confined shell
//! call's `prepare` against a home fixture with about 75 entries along
//! the walked deny paths (`docs/sandbox.md`, "How read denies are built").

use std::path::{Path, PathBuf};

use contract::clock::Clock;

use crate::idle::{Ctx, Samples, Workload, ms};

/// The `sandbox setup` timing workload: one `prepare` against the home
/// fixture, reporting `sandbox_setup_ms`.
pub(crate) const WORKLOADS: [Workload; 1] = [Workload {
    name: "sandbox setup",
    timing: true,
    run: measure,
}];

/// Readable entries beside the built-in deny inside the fixture home:
/// the walk grants every readable sibling of each existing ancestor, so
/// these stand in for a real home's entries.
const SIBLINGS_IN_FIBER_HOME: usize = 60;
/// Readable entries beside the fixture home in the fixture root, outside
/// the denied path: the walk grants these as the root's siblings.
const SIBLINGS_IN_ROOT: usize = 15;

/// The home fixture one `prepare` runs against: a root outside the
/// system temp directory, which the sandbox grants as writable, so a deny
/// beneath it would overlap and report unavailable instead of confining.
/// The root carries the shared `fiber-tools-shell-sandbox-protected`
/// prefix, so one glob sweeps every leftover.
pub(crate) struct Fixture {
    /// Held so the root lives until `prepare` returns.
    _dir: fakes::TempDir,
    /// The enabled settings with only the built-in `credentials/` deny.
    pub(crate) settings: tools::SandboxSettings,
    /// The workspace outside the system temp root.
    pub(crate) workspace: PathBuf,
}

/// Builds the home fixture with only the built-in `credentials/` deny:
/// an enabled sandbox with no extra denies, and the sibling entries the
/// ancestor walk grants.
pub(crate) fn fixture() -> Result<Fixture, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set".to_owned())?;
    let dir = fakes::TempDir::under(Path::new(&home), "fiber-tools-shell-sandbox-protected");
    let fiber_home = dir.path().join("fiber-home");
    std::fs::create_dir_all(fiber_home.join("credentials"))
        .map_err(|err| format!("creating the fixture deny: {err}"))?;
    for sibling in 0..SIBLINGS_IN_FIBER_HOME {
        write_entry(&fiber_home.join(format!("entry-{sibling:02}")))?;
    }
    for sibling in 0..SIBLINGS_IN_ROOT {
        write_entry(&dir.path().join(format!("entry-{sibling:02}")))?;
    }
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace)
        .map_err(|err| format!("creating the fixture workspace: {err}"))?;
    let settings = tools::SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home,
    };
    Ok(Fixture {
        _dir: dir,
        settings,
        workspace,
    })
}

/// Writes one readable sibling entry.
fn write_entry(path: &Path) -> Result<(), String> {
    std::fs::write(path, b"sandbox-setup sibling")
        .map_err(|err| format!("writing {}: {err}", path.display()))
}

/// Times one `prepare`: the fixture builds outside the window, and a
/// `prepare` error becomes the workload's `Err` and lands in the result
/// file's `failures`.
pub(crate) fn time_prepare(
    clock: &dyn Clock,
    settings: &tools::SandboxSettings,
    workspace: &Path,
) -> Result<Samples, String> {
    let start = clock.now();
    tools::prepare(settings, workspace, None).map_err(|error| error.to_string())?;
    Ok(vec![("sandbox_setup_ms", ms(clock, start))])
}

/// Times one `prepare` against the home fixture.
fn measure(ctx: &Ctx<'_>, _notes: &mut Vec<String>) -> Result<Samples, String> {
    let fixture = fixture()?;
    time_prepare(ctx.clock, &fixture.settings, &fixture.workspace)
}

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod tests;
