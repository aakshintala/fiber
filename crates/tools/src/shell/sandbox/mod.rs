//! The Linux Landlock plus seccomp policy (`docs/sandbox.md`, "On Linux"
//! and "How read denies are built"). Prepared before spawn, installed in the
//! child between fork and exec; a setup error aborts spawn.

use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::io::OwnedFd;
use std::path::{Component, Path, PathBuf};

/// The settings for one session's Linux sandbox, resolved from the
/// person-only configuration (`docs/configuration.md`, "Keys").
#[derive(Clone, Debug)]
pub struct Settings {
    /// Whether the person turned the sandbox on.
    pub enabled: bool,
    /// Extra writable paths the person granted.
    pub allow_write: Vec<PathBuf>,
    /// Extra unreadable paths the person denied.
    pub deny_read: Vec<PathBuf>,
    /// Fiber home, for the `credentials/` deny.
    pub fiber_home: PathBuf,
}

/// Why the sandbox is unavailable for a session. The caller shows one notice
/// naming the path when there is one and falls back to ordinary review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    /// The reason, naming the path when the deny overlaps a write grant.
    pub reason: String,
}

/// A prepared policy, built before fork and installed in the child.
pub(crate) struct Prepared {
    /// The Landlock ruleset with every grant added, ready to restrict.
    #[cfg(target_os = "linux")]
    ruleset: landlock::RulesetCreated,
    /// The compiled seccomp program denying sockets and `io_uring_setup`.
    #[cfg(target_os = "linux")]
    program: seccompiler::BpfProgram,
}

/// Checks whether the sandbox can be attempted for `workspace`. Unavailable
/// means run without it with one notice, not a failed call. Only Linux
/// applies this policy; other builds report unavailable.
pub fn check_available(settings: &Settings, workspace: &Path) -> Result<(), Unavailable> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (settings, workspace);
        return Err(Unavailable {
            reason: "the Linux sandbox is unavailable on this platform".to_owned(),
        });
    }
    #[cfg(target_os = "linux")]
    {
        check_available_linux(settings, workspace)
    }
}

#[cfg(target_os = "linux")]
fn check_available_linux(settings: &Settings, workspace: &Path) -> Result<(), Unavailable> {
    use landlock::{Compatible as _, RulesetAttr as _};
    if !settings.enabled {
        return Err(Unavailable {
            reason: "the sandbox is off".to_owned(),
        });
    }
    // Floor Landlock ABI 2: require the V2 read access and verify the
    // kernel creates the ruleset. An old kernel or a kernel without
    // Landlock fails here and reports unavailable.
    let probe = landlock::Ruleset::default()
        .set_compatibility(landlock::CompatLevel::HardRequirement)
        .handle_access(landlock::AccessFs::from_read(landlock::ABI::V2));
    let Ok(ruleset) = probe else {
        return Err(Unavailable {
            reason: "Landlock ABI is below 2".to_owned(),
        });
    };
    if ruleset.create().is_err() {
        return Err(Unavailable {
            reason: "Landlock ABI is below 2".to_owned(),
        });
    }
    let temp = std::env::temp_dir();
    let mut writable = Vec::new();
    writable.push(normalize_or_canonical(workspace));
    writable.push(normalize_or_canonical(&temp));
    for path in &settings.allow_write {
        writable.push(normalize_or_canonical(path));
    }
    for deny in denied_paths(settings) {
        let candidate = normalize_absolute(&deny);
        for root in &writable {
            if candidate == *root || candidate.starts_with(join_slash(root)) {
                return Err(Unavailable {
                    reason: format!(
                        "sandbox.deny_read path {} is inside a writable path {}",
                        deny.display(),
                        root.display()
                    ),
                });
            }
        }
    }
    Ok(())
}

/// The denied paths for one spawn: Fiber home's `credentials/` plus the
/// person's extra denies.
fn denied_paths(settings: &Settings) -> Vec<PathBuf> {
    let mut denied = Vec::new();
    denied.push(settings.fiber_home.join("credentials"));
    denied.extend(settings.deny_read.iter().cloned());
    denied
}

/// Prepares the Landlock grants and the seccomp program. A setup error
/// aborts spawn; the call fails and never runs unconfined.
#[cfg(target_os = "linux")]
pub(crate) fn prepare(settings: &Settings, workspace: &Path) -> Result<Prepared, String> {
    use landlock::{Access as _, Compatible as _, RulesetAttr as _, RulesetCreatedAttr as _};
    check_available(settings, workspace).map_err(|unavailable| unavailable.reason)?;
    let selected = landlock::ABI::V2;
    let mut rules: Vec<landlock::PathBeneath<OwnedFd>> = Vec::new();
    let temp = std::env::temp_dir();
    let mut writable = Vec::new();
    writable.push(workspace.to_path_buf());
    writable.push(temp);
    writable.extend(settings.allow_write.iter().cloned());
    for root in &writable {
        let rule_fd = open_nofollow(root)
            .map_err(|cause| format!("sandbox setup failed opening {}: {cause}", root.display()))?;
        let access = landlock::AccessFs::from_all(selected);
        rules.push(landlock::PathBeneath::new(rule_fd, access));
    }
    for deny in denied_paths(settings) {
        add_ancestor_grants(&deny, selected, &mut rules)?;
    }
    let created = landlock::Ruleset::default()
        .set_compatibility(landlock::CompatLevel::BestEffort)
        .handle_access(landlock::AccessFs::from_all(selected))
        .map_err(|cause| format!("sandbox setup failed: {cause:?}"))?
        .create()
        .map_err(|cause| format!("sandbox setup failed: {cause:?}"))?;
    let mut created = created;
    for rule in rules {
        created = created
            .add_rule(rule)
            .map_err(|cause| format!("sandbox setup failed adding a grant: {cause:?}"))?;
    }
    let program = compile_seccomp()?;
    Ok(Prepared {
        ruleset: created,
        program,
    })
}

/// Installs a prepared policy in the child between fork and exec. Only
/// syscalls run here; success performs no allocation.
#[cfg(target_os = "linux")]
pub(crate) fn install(prepared: Prepared) -> Result<(), String> {
    prepared
        .ruleset
        .restrict_self()
        .map_err(|cause| format!("the sandbox could not be applied: {cause:?}"))?;
    seccompiler::apply_filter(&prepared.program)
        .map_err(|cause| format!("the sandbox could not be applied: {cause}"))?;
    Ok(())
}

/// A stub for non-Linux builds, which never apply the Linux policy.
#[cfg(not(target_os = "linux"))]
pub(crate) fn prepare(_settings: &Settings, _workspace: &Path) -> Result<Prepared, String> {
    Err("the Linux sandbox is unavailable on this platform".to_owned())
}

/// A stub for non-Linux builds.
#[cfg(not(target_os = "linux"))]
pub(crate) fn install(_prepared: Prepared) -> Result<(), String> {
    Err("the Linux sandbox is unavailable on this platform".to_owned())
}

/// A stub `Prepared` for non-Linux builds.
#[cfg(not(target_os = "linux"))]
pub(crate) struct Prepared;

/// Adds the ancestor-walk grants for one denied path. For each existing
/// ancestor directory on the way, grants read on every entry except the one
/// leading to the denied path. Missing components stop the walk.
#[cfg(target_os = "linux")]
fn add_ancestor_grants(
    denied: &Path,
    abi: landlock::ABI,
    rules: &mut Vec<landlock::PathBeneath<OwnedFd>>,
) -> Result<(), String> {
    let absolute = normalize_absolute(denied);
    let mut components: Vec<std::ffi::OsString> = Vec::new();
    for component in absolute.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => components.push(name.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                components.pop();
            }
            Component::Prefix(_) => {}
        }
    }
    let mut prefix = PathBuf::from("/");
    for (index, name) in components.iter().enumerate() {
        let is_last = index + 1 == components.len();
        if is_last {
            break;
        }
        let ancestor = prefix.clone();
        let next_path = ancestor.join(name);
        if std::fs::symlink_metadata(&next_path).is_err() {
            break;
        }
        let entries = std::fs::read_dir(&ancestor).map_err(|cause| {
            format!(
                "sandbox setup failed listing {}: {cause}",
                ancestor.display()
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|cause| {
                format!(
                    "sandbox setup failed listing {}: {cause}",
                    ancestor.display()
                )
            })?;
            if entry.file_name() == *name {
                continue;
            }
            let path = ancestor.join(entry.file_name());
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            let Ok(fd) = open_nofollow(&path) else {
                continue;
            };
            let access = landlock::AccessFs::from_read(abi);
            rules.push(landlock::PathBeneath::new(fd, access));
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&next_path) {
            if metadata.file_type().is_symlink() {
                break;
            }
        } else {
            break;
        }
        prefix = next_path;
    }
    Ok(())
}

/// Opens `path` with `O_PATH | O_NOFOLLOW | O_CLOEXEC`, so a symlink never
/// resolves to its target.
fn open_nofollow(path: &Path) -> Result<OwnedFd, String> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map(OwnedFd::from)
        .map_err(|cause| format!("opening {}: {cause}", path.display()))
}

/// Compiles the seccomp filter denying sockets and `io_uring_setup` with
/// `EPERM`. All sockets are denied; there is no allow-list in this slice.
/// An empty rule vector matches only on the syscall number, denying the
/// call unconditionally.
#[cfg(target_os = "linux")]
fn compile_seccomp() -> Result<seccompiler::BpfProgram, String> {
    use seccompiler::{SeccompAction, SeccompFilter};
    use std::collections::BTreeMap;
    let mut map: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = BTreeMap::new();
    for syscall in [
        libc::SYS_socket,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_io_uring_setup,
    ] {
        map.insert(syscall, Vec::new());
    }
    let filter = SeccompFilter::new(
        map,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        std::env::consts::ARCH
            .try_into()
            .map_err(|_| "sandbox setup failed: unknown architecture for seccomp".to_owned())?,
    )
    .map_err(|cause| format!("sandbox setup failed compiling seccomp: {cause:?}"))?;
    filter
        .try_into()
        .map_err(|cause| format!("sandbox setup failed compiling seccomp: {cause:?}"))
}

/// Canonicalizes when the path exists, otherwise normalizes lexically.
fn normalize_or_canonical(path: &Path) -> PathBuf {
    canonicalize_existing(path).unwrap_or_else(|| normalize_absolute(path))
}

/// Canonicalizes when the path exists, for the overlap check.
fn canonicalize_existing(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// Makes `path` absolute and normalizes `.` and `..` lexically, without
/// touching the filesystem.
fn normalize_absolute(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        out
    }
}

/// Joins a trailing slash for prefix matching, so `/tmp/foo2` does not match
/// writable root `/tmp/foo`.
fn join_slash(root: &Path) -> PathBuf {
    let mut with_slash = root.to_path_buf();
    with_slash.push("");
    with_slash
}

/// The rule count and the ancestor-walk time for the PR body, measured on
/// Linux for the owner's home directory. Test-only: its one caller keeps
/// the figures for the record.
#[cfg(all(target_os = "linux", test))]
pub(crate) fn measure_home_walk(
    clock: &dyn contract::clock::Clock,
) -> (usize, std::time::Duration) {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/root"), PathBuf::from);
    let settings = Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: home,
    };
    let start = clock.now();
    let mut rules: Vec<landlock::PathBeneath<OwnedFd>> = Vec::new();
    for deny in denied_paths(&settings) {
        // A deny point that cannot be walked contributes no grants; the
        // sandbox still confines everything else, so the measurement
        // skips the failure instead of aborting the count.
        let _walked = add_ancestor_grants(&deny, landlock::ABI::V2, &mut rules);
    }
    (rules.len(), clock.now().saturating_duration_since(start))
}

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod tests;
