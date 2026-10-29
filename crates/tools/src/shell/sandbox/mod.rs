//! The operating-system sandbox policies (`docs/sandbox.md`, "On Linux",
//! "On macOS" and "How read denies are built"). Prepared before spawn: on
//! Linux installed in the child between fork and exec, on macOS applied by
//! spawning through `sandbox-exec`; a setup error aborts spawn.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::collections::VecDeque;
#[cfg(target_os = "linux")]
use std::os::unix::io::OwnedFd;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Component;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::ExitStatus;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};

/// The settings for one session's OS sandbox, resolved from the
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
/// and falls back to ordinary review. Overlap carries the configured path
/// and, when a resolved target overlaps instead, that target after the
/// arrow; the missing-binary case names the binary it looked for; every
/// other case names no path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The person turned the sandbox off.
    Off,
    /// This platform never applies either policy.
    UnsupportedPlatform,
    /// The kernel's Landlock ABI is below the floor of 2.
    AbiBelow2,
    /// `sandbox-exec` is missing or unusable at the recorded path, so no
    /// call on macOS can be confined.
    SandboxExecUnavailable {
        /// The binary path the check looked for.
        path: PathBuf,
    },
    /// A deny path, lexical or resolved, sits inside a writable grant.
    DenyInsideWrite {
        /// The configured `sandbox.deny_read` path, or Fiber home's
        /// `credentials/` for the built-in deny.
        configured: PathBuf,
        /// The path that overlaps the writable root: the configured path
        /// itself, or its resolved target when that one overlaps instead.
        overlapping: PathBuf,
        /// The writable root it sits inside.
        root: PathBuf,
    },
    /// A deny path could not be resolved: a symlink loop, more than 40
    /// symlink hops, or a link `readlink` could not read.
    UnresolvableDeny {
        /// The configured `sandbox.deny_read` path, or Fiber home's
        /// `credentials/` for the built-in deny.
        configured: PathBuf,
    },
    /// A writable root could not be resolved through symlinks.
    UnresolvedWrite {
        /// The workspace, temp or `allow_write` root that failed.
        root: PathBuf,
    },
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => write!(f, "the sandbox is off"),
            Self::UnsupportedPlatform => {
                write!(f, "the Linux sandbox is unavailable on this platform")
            }
            Self::AbiBelow2 => write!(f, "Landlock ABI is below 2"),
            Self::SandboxExecUnavailable { path } => {
                write!(f, "sandbox-exec is unavailable at {}", path.display())
            }
            Self::DenyInsideWrite {
                configured,
                overlapping,
                root,
            } => {
                // A generic read-deny label for every source: the deny set
                // also carries credential paths, which must never read as
                // `sandbox.deny_read` attributions.
                write!(f, "sandbox read-deny path {}", configured.display())?;
                if overlapping != configured {
                    write!(f, " \u{2192} {}", overlapping.display())?;
                }
                write!(f, " is inside a writable path {}", root.display())
            }
            Self::UnresolvableDeny { configured } => write!(
                f,
                "sandbox.deny_read path {} could not be resolved",
                configured.display()
            ),
            Self::UnresolvedWrite { root } => {
                write!(
                    f,
                    "sandbox write grant {} could not be resolved",
                    root.display()
                )
            }
        }
    }
}

impl Unavailable {
    /// The code from `docs/errors.md`: deny/write overlap is a bad
    /// configuration, every other unavailable case is a tool error, matching
    /// the startup notice. One fixed code per case, with no wildcard arm.
    pub fn code(&self) -> contract::ErrorCode {
        match self {
            Self::DenyInsideWrite { .. } => contract::ErrorCode::InvalidArguments,
            Self::Off => contract::ErrorCode::ToolError,
            Self::UnsupportedPlatform => contract::ErrorCode::ToolError,
            Self::AbiBelow2 => contract::ErrorCode::ToolError,
            Self::SandboxExecUnavailable { .. } => contract::ErrorCode::ToolError,
            Self::UnresolvableDeny { .. } => contract::ErrorCode::ToolError,
            Self::UnresolvedWrite { .. } => contract::ErrorCode::ToolError,
        }
    }
}

/// A prepared policy, built before fork and installed in the child. The
/// ruleset rides `Some` until `install` takes it; the seccomp program is
/// only borrowed, never moved or freed, in the child.
#[cfg(target_os = "linux")]
pub(crate) struct Prepared {
    /// The Landlock ruleset with every grant added, ready to restrict.
    /// Taken by `install`, which consumes it through `restrict_self`.
    ruleset: Option<landlock::RulesetCreated>,
    /// The compiled seccomp program denying sockets and `io_uring_setup`.
    program: seccompiler::BpfProgram,
}

/// Checks whether the sandbox can be attempted for `workspace`. An
/// unavailable case produces the startup notice; a call decided confined
/// then fails at spawn instead of running without the policy. Linux applies
/// the Landlock policy and macOS the Seatbelt policy; other builds report
/// unavailable.
pub fn check_available(settings: &Settings, workspace: &Path) -> Result<(), Unavailable> {
    if !settings.enabled {
        return Err(Unavailable::Off);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = workspace;
        Err(Unavailable::UnsupportedPlatform)
    }
    #[cfg(target_os = "linux")]
    {
        check_available_linux(settings, workspace)
    }
    #[cfg(target_os = "macos")]
    {
        check_available_with_exec(settings, workspace, Path::new(SANDBOX_EXEC))
    }
}

#[cfg(target_os = "linux")]
fn check_available_linux(settings: &Settings, workspace: &Path) -> Result<(), Unavailable> {
    use landlock::{Compatible as _, RulesetAttr as _};
    // Floor Landlock ABI 2: require the V2 read access and verify the
    // kernel creates the ruleset. An old kernel or a kernel without
    // Landlock fails here and reports unavailable.
    let probe = landlock::Ruleset::default()
        .set_compatibility(landlock::CompatLevel::HardRequirement)
        .handle_access(landlock::AccessFs::from_read(landlock::ABI::V2));
    let Ok(ruleset) = probe else {
        return Err(Unavailable::AbiBelow2);
    };
    if ruleset.create().is_err() {
        return Err(Unavailable::AbiBelow2);
    }
    let temp = std::env::temp_dir();
    let roots: Vec<&Path> = vec![workspace, &temp];
    check_roots(settings, &roots)
}

/// The macOS availability check: the Seatbelt binary exists first, then the
/// existing overlap and resolution rules decide. `executable` is injected
/// so a test can point at a nonexistent fixture path; production passes
/// `/usr/bin/sandbox-exec`, hardcoded and never `$PATH`, so a command
/// cannot inject a malicious wrapper.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
fn check_available_with_exec(
    settings: &Settings,
    workspace: &Path,
    executable: &Path,
) -> Result<(), Unavailable> {
    let usable = std::fs::metadata(executable).is_ok_and(|meta| meta.is_file());
    if !usable {
        return Err(Unavailable::SandboxExecUnavailable {
            path: executable.to_path_buf(),
        });
    }
    // The temp directory reads `$TMPDIR`, and `/tmp` is the historical
    // temp: both are writable roots, like on Linux plus `/tmp`.
    let temp = std::env::temp_dir();
    let slash_tmp = Path::new("/tmp");
    let roots: Vec<&Path> = vec![workspace, &temp, slash_tmp];
    check_roots(settings, &roots)
}

/// Resolves writable roots through symlinks first, so the overlap check and
/// the grants below see through aliases. Both the lexical and the resolved
/// root count: an alias must not smuggle a deny inside.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn check_roots(settings: &Settings, roots: &[&Path]) -> Result<(), Unavailable> {
    let mut writable = Vec::new();
    let mut extended: Vec<&Path> = roots.to_vec();
    extended.extend(settings.allow_write.iter().map(PathBuf::as_path));
    for root in extended {
        match std::fs::canonicalize(root) {
            Ok(resolved) => {
                writable.push(normalize_absolute(root));
                writable.push(resolved);
            }
            Err(_) => {
                return Err(Unavailable::UnresolvedWrite {
                    root: root.to_path_buf(),
                });
            }
        }
    }
    let resolved = resolve_denies(settings)?;
    for deny in &resolved {
        let lexical = normalize_absolute(&deny.configured);
        for candidate in std::iter::once(&lexical).chain(deny.target.as_ref()) {
            for root in &writable {
                if candidate.starts_with(root) {
                    return Err(Unavailable::DenyInsideWrite {
                        configured: deny.configured.clone(),
                        overlapping: candidate.clone(),
                        root: root.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// The denied paths for one spawn: Fiber home's `credentials/` plus the
/// person's extra denies, each at its configured lexical location.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn denied_paths(settings: &Settings) -> Vec<PathBuf> {
    let mut denied = Vec::new();
    denied.push(settings.fiber_home.join("credentials"));
    denied.extend(settings.deny_read.iter().cloned());
    denied
}

/// One configured deny with its `readlink`-resolved target, when symlinks
/// lead elsewhere.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ResolvedDeny {
    /// The configured path, at its lexical location.
    configured: PathBuf,
    /// The fully resolved target, when a symlink component leads away from
    /// the configured path.
    target: Option<PathBuf>,
}

/// The most symlink hops one deny resolution follows.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_SYMLINK_HOPS: usize = 40;

/// Resolves every configured deny through `readlink`, dangling links
/// included. A relative link target resolves against the link's directory.
/// A loop, more than 40 hops, or an unreadable link makes the sandbox
/// unavailable: a half-resolved target could leave the true target granted.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn resolve_denies(settings: &Settings) -> Result<Vec<ResolvedDeny>, Unavailable> {
    let mut resolved = Vec::new();
    for configured in denied_paths(settings) {
        let target = resolve_target(&configured)?;
        resolved.push(ResolvedDeny { configured, target });
    }
    Ok(resolved)
}

/// Resolves `path` through each symlink component with `readlink`,
/// following dangling links to their (possibly missing) targets. Returns
/// `None` when no symlink component leads away. Stops at the first missing
/// non-symlink component and appends the remaining lexical suffix, so the
/// deny still names the configured target without granting an ancestor. A
/// relative link target resolves against the link's directory.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn resolve_target(path: &Path) -> Result<Option<PathBuf>, Unavailable> {
    let absolute = normalize_absolute(path);
    let mut pending = path_components(&absolute);
    let mut current = PathBuf::from("/");
    let mut visited: Vec<PathBuf> = Vec::new();
    let mut hops = 0;
    let mut saw_link = false;
    while let Some(component) = pending.pop_front() {
        let next = current.join(component);
        match std::fs::symlink_metadata(&next) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                current = next;
                for remaining in pending {
                    current.push(remaining);
                }
                break;
            }
            Err(_) => return Err(unresolvable(path)),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                saw_link = true;
                let target = follow_link(&next, path, &mut visited, &mut hops)?;
                let mut target_components = path_components(&target);
                target_components.append(&mut pending);
                pending = target_components;
                current = PathBuf::from("/");
            }
            Ok(_) => current = next,
        }
    }
    Ok(saw_link.then_some(current))
}

/// Follows one link, rejecting loops and paths beyond the shared hop budget.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn follow_link(
    link: &Path,
    configured: &Path,
    visited: &mut Vec<PathBuf>,
    hops: &mut usize,
) -> Result<PathBuf, Unavailable> {
    if visited.iter().any(|seen| seen == link) {
        return Err(unresolvable(configured));
    }
    visited.push(link.to_path_buf());
    *hops = (*hops)
        .checked_add(1)
        .ok_or_else(|| unresolvable(configured))?;
    if *hops > MAX_SYMLINK_HOPS {
        return Err(unresolvable(configured));
    }
    let raw = std::fs::read_link(link).map_err(|_| unresolvable(configured))?;
    let target = if raw.is_absolute() {
        raw
    } else if let Some(parent) = link.parent() {
        parent.join(raw)
    } else {
        return Err(unresolvable(configured));
    };
    Ok(normalize_absolute(&target))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn path_components(path: &Path) -> VecDeque<std::ffi::OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_os_string()),
            Component::RootDir
            | Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_) => None,
        })
        .collect()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unresolvable(configured: &Path) -> Unavailable {
    Unavailable::UnresolvableDeny {
        configured: configured.to_path_buf(),
    }
}

/// Prepares the Landlock grants and the seccomp program. A setup error
/// aborts spawn; the call fails and never runs unconfined.
#[cfg(target_os = "linux")]
pub(crate) fn prepare(settings: &Settings, workspace: &Path) -> Result<Prepared, String> {
    use landlock::{Access as _, Compatible as _, RulesetAttr as _, RulesetCreatedAttr as _};
    check_available(settings, workspace).map_err(|unavailable| unavailable.to_string())?;
    let selected = landlock::ABI::V2;
    let mut rules: Vec<landlock::PathBeneath<OwnedFd>> = Vec::new();
    let temp = std::env::temp_dir();
    // Grant the resolved writable roots, so an aliased workspace stays
    // writable through its alias.
    let mut roots: Vec<&Path> = vec![workspace, &temp];
    roots.extend(settings.allow_write.iter().map(PathBuf::as_path));
    for root in roots {
        let resolved = std::fs::canonicalize(root).map_err(|cause| {
            format!("sandbox setup failed resolving {}: {cause}", root.display())
        })?;
        let rule_fd = open_nofollow(&resolved).map_err(|cause| {
            format!(
                "sandbox setup failed opening {}: {cause}",
                resolved.display()
            )
        })?;
        let access = landlock::AccessFs::from_all(selected);
        rules.push(landlock::PathBeneath::new(rule_fd, access));
    }
    // Deny every configured path and every resolved target, skipping no
    // entry that leads to any of them.
    let resolved_denies =
        resolve_denies(settings).map_err(|unavailable| unavailable.to_string())?;
    let mut exclusions: Vec<PathBuf> = Vec::new();
    for deny in &resolved_denies {
        exclusions.push(normalize_absolute(&deny.configured));
        if let Some(target) = &deny.target {
            exclusions.push(normalize_absolute(target));
        }
    }
    for deny in &resolved_denies {
        add_ancestor_grants(
            &deny.configured,
            &exclusions,
            selected,
            Path::new("/"),
            &mut rules,
        )?;
        if let Some(target) = &deny.target {
            add_ancestor_grants(target, &exclusions, selected, Path::new("/"), &mut rules)?;
        }
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
        ruleset: Some(created),
        program,
    })
}

/// Installs a prepared policy in the child between fork and exec. Takes the
/// ruleset out of the borrowed policy and consumes it through
/// `restrict_self`, whose drop only closes a file descriptor: a syscall,
/// safe here. The seccomp program is only borrowed, so its vector is never
/// moved or freed in the child. On failure returns the raw errno with no
/// formatting or allocation; the caller writes it to the sentinel and
/// aborts spawn.
#[cfg(target_os = "linux")]
pub(crate) fn install(prepared: &mut Prepared) -> Result<(), i32> {
    let ruleset = prepared.ruleset.take().ok_or(libc::EINVAL)?;
    ruleset.restrict_self().map_err(|cause| errno_of(&cause))?;
    seccompiler::apply_filter(&prepared.program).map_err(|cause| errno_of(&cause))?;
    Ok(())
}

/// The errno behind a Landlock or seccomp failure, walking the error's
/// sources down to the `io::Error` the failing syscall left. Matching
/// would name each crate's variants; the walk reads only the standard
/// source chain, with no allocation and no formatting.
#[cfg(target_os = "linux")]
fn errno_of(cause: &(dyn std::error::Error + 'static)) -> i32 {
    let mut next: Option<&(dyn std::error::Error + 'static)> = Some(cause);
    while let Some(error) = next {
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            return io.raw_os_error().unwrap_or(libc::EIO);
        }
        next = error.source();
    }
    libc::EIO
}

/// The Seatbelt wrapper, hardcoded and never `$PATH`: a command cannot
/// inject a malicious wrapper by planting one earlier on the path.
#[cfg(target_os = "macos")]
pub(crate) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// A prepared Seatbelt policy: the inline profile plus its `-D` bindings.
/// Every dynamic filesystem path travels only as a separately supplied
/// `sandbox-exec -DNAME=<path>` binding, referenced in the profile through
/// its fixed code-owned parameter name; no path text is interpolated into
/// the profile source, the command text, or a shell fragment.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub(crate) struct Prepared {
    /// The inline Seatbelt source, with fixed parameter names only.
    pub(crate) profile: String,
    /// The `(name, path)` bindings passed as `-Dname=path` argv entries.
    pub(crate) bindings: Vec<(String, String)>,
}

/// Prepares the Seatbelt profile: a setup error aborts spawn, so the call
/// fails closed and never runs unconfined.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn prepare(settings: &Settings, workspace: &Path) -> Result<Prepared, String> {
    check_available(settings, workspace).map_err(|unavailable| unavailable.to_string())?;
    build_profile(settings, workspace)
}

/// Builds the profile and bindings from canonicalized roots. Writable roots
/// are the canonicalized workspace, the system temp directory (which reads
/// `$TMPDIR`), `/tmp`, and `allow_write`; deny rules cover the Fiber-home
/// `credentials/` directory, every configured and resolved read deny, and
/// the git and `.fiber/` protections below.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
fn build_profile(settings: &Settings, workspace: &Path) -> Result<Prepared, String> {
    let workspace = canonicalize_root("the workspace", workspace)?;
    let temp = canonicalize_root("the temp directory", &std::env::temp_dir())?;
    let slash_tmp = canonicalize_root("the temp directory", Path::new("/tmp"))?;
    let mut bindings = vec![
        ("FIBER_WS".to_owned(), path_text(&workspace)),
        ("FIBER_TMPDIR".to_owned(), path_text(&temp)),
        ("FIBER_TMP".to_owned(), path_text(&slash_tmp)),
    ];
    let mut profile = String::from(
        "(version 1)\n\
         ; Fiber's macOS policy for model-issued shell calls: deny by\n\
         ; default, so a call reaches only what the rules below grant. Every\n\
         ; dynamic path arrives as a separately supplied `-DNAME=<path>`\n\
         ; binding and is named below only through its fixed code-owned\n\
         ; parameter; no path text is interpolated into this source.\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow sysctl-read)\n\
         (allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\"))\n\
         (allow file-read* (subpath \"/\"))\n\
         (allow file-read* file-write* (literal \"/dev/null\"))\n\
         (allow file-read* file-write* file-ioctl (regex #\"^/dev/ttys[0-9]+$\"))\n\
         (allow file-write* (subpath (param \"FIBER_WS\")))\n\
         (allow file-write* (subpath (param \"FIBER_TMPDIR\")))\n\
         (allow file-write* (subpath (param \"FIBER_TMP\")))\n",
    );
    for (index, root) in settings.allow_write.iter().enumerate() {
        let resolved = canonicalize_root("an allow_write path", root)?;
        let name = format!("FIBER_ALLOW_{index}");
        profile.push_str(&format!(
            "(allow file-write* (subpath (param \"{name}\")))\n"
        ));
        bindings.push((name, path_text(&resolved)));
    }
    let git = discover_git(&workspace);
    let gitdir = git.gitdir.unwrap_or_else(|| workspace.join(".git"));
    let common = git.commondir.unwrap_or_else(|| workspace.join(".git"));
    bindings.push(("FIBER_GITDIR".to_owned(), path_text(&gitdir)));
    bindings.push(("FIBER_COMMONDIR".to_owned(), path_text(&common)));
    profile.push_str(
        "(allow file-write* (subpath (param \"FIBER_GITDIR\")))\n\
         (allow file-write* (subpath (string-append (param \"FIBER_COMMONDIR\") \"/objects\")))\n\
         (allow file-write* (subpath (string-append (param \"FIBER_COMMONDIR\") \"/refs\")))\n\
         (allow file-write* (subpath (string-append (param \"FIBER_COMMONDIR\") \"/logs\")))\n",
    );
    let mut deny_index = 0;
    for deny in resolve_denies(settings).map_err(|unavailable| unavailable.to_string())? {
        // Both the lexical path and its resolved target: Seatbelt judges
        // the access by the resolved vnode, so a symlink deny must bind
        // where the link leads as well as where it is configured.
        let mut spellings = vec![normalize_absolute(&deny.configured)];
        if let Some(target) = deny.target {
            spellings.push(normalize_absolute(&target));
        }
        for spelling in spellings {
            let name = format!("FIBER_DENY_{deny_index}");
            deny_index += 1;
            profile.push_str(&format!("(deny file-read* (subpath (param \"{name}\")))\n"));
            bindings.push((name, path_text(&spelling)));
        }
    }
    profile.push_str(
        "; The git and `.fiber/` protections: deny wins over the workspace\n\
         ; and gitdir grants above, so commits keep working while these\n\
         ; paths stay unwritable, on main checkouts and on worktrees alike.\n\
         (deny file-write* (literal (string-append (param \"FIBER_WS\") \"/.git/config\")))\n\
         (deny file-write* (subpath (string-append (param \"FIBER_WS\") \"/.git/hooks\")))\n\
         (deny file-write* (literal (string-append (param \"FIBER_WS\") \"/.git\")))\n\
         (deny file-write* (subpath (string-append (param \"FIBER_WS\") \"/.fiber\")))\n\
         (deny file-write* (subpath (string-append (param \"FIBER_COMMONDIR\") \"/hooks\")))\n\
         (deny file-write* (literal (string-append (param \"FIBER_COMMONDIR\") \"/config\")))\n\
         ; Sockets reach nothing: deny-default refuses what no rule grants,\n\
         ; and the explicit denies below name the refusal. Unix sockets have\n\
         ; no Seatbelt operation of their own; the default deny covers their\n\
         ; creation, and no setting grants one.\n\
         (deny network-outbound)\n\
         (deny network-inbound)\n\
         (deny network-bind)\n\
         (deny system-socket)\n\
         (deny mach-lookup (global-name \"com.apple.launchd\"))\n",
    );
    Ok(Prepared { profile, bindings })
}

/// Canonicalizes one writable root for the profile, naming it on failure so
/// the call fails closed instead of running unconfined.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
fn canonicalize_root(label: &str, root: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(root).map_err(|cause| {
        format!(
            "sandbox setup failed resolving {label} {}: {cause}",
            root.display()
        )
    })
}

/// The binding text for one path. Paths are carried as argv values, never
/// parsed by a shell, so no quoting applies here.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Where the commits of the workspace's checkout live: the worktree's own
/// gitdir, or the checkout's `.git`, and the shared common directory.
#[cfg(target_os = "macos")]
struct GitLayout {
    /// The directory git writes the worktree's `HEAD`, index and logs to.
    gitdir: Option<PathBuf>,
    /// The shared directory holding `objects/`, `refs/` and `logs/`.
    commondir: Option<PathBuf>,
}

/// Whether `path` looks like a git directory: an existing directory holding
/// the entries git itself keeps there (`HEAD`, `commondir`, or `objects`).
/// A hostile `.git` pointer names an absolute path the checkout author
/// chose, so the pointer target earns a write grant only when it is a
/// recognized layout; anything else grants neither directory.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
fn is_git_layout(path: &Path) -> bool {
    path.is_dir()
        && (path.join("HEAD").exists()
            || path.join("commondir").exists()
            || path.join("objects").is_dir())
}

/// Discovers the git directories without running git: a `.git` directory is
/// both, a `.git` file points at the gitdir whose `commondir` file points at
/// the common directory, and anything else grants no git directory. A
/// pointer target outside a recognized git layout grants neither directory,
/// so `gitdir: /` never earns a write grant at the filesystem root. Reads
/// are capped, so a hostile checkout cannot make prepare read unboundedly.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
fn discover_git(workspace: &Path) -> GitLayout {
    let dot_git = workspace.join(".git");
    let layout = GitLayout {
        gitdir: None,
        commondir: None,
    };
    let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
        return layout;
    };
    if meta.is_dir() {
        return GitLayout {
            gitdir: Some(dot_git.clone()),
            commondir: Some(dot_git),
        };
    }
    if !meta.is_file() {
        return layout;
    }
    let pointer = read_capped(&dot_git);
    let Some(rest) = pointer.strip_prefix("gitdir: ") else {
        return layout;
    };
    let declared = rest.lines().next().unwrap_or_default().trim();
    if declared.is_empty() {
        return layout;
    }
    let gitdir = if Path::new(declared).is_absolute() {
        PathBuf::from(declared)
    } else {
        workspace.join(declared)
    };
    let gitdir = rooted_path(&gitdir);
    let common = match read_capped(&gitdir.join("commondir")).lines().next() {
        Some(line) if !line.trim().is_empty() => {
            let text = line.trim();
            if Path::new(text).is_absolute() {
                PathBuf::from(text)
            } else {
                gitdir.join(text)
            }
        }
        _ => gitdir.clone(),
    };
    let common = rooted_path(&common);
    if !is_git_layout(&gitdir) || !is_git_layout(&common) {
        return layout;
    }
    GitLayout {
        gitdir: Some(gitdir),
        commondir: Some(common),
    }
}

/// Reads at most 4 KiB: the git pointer files prepare reads are one line.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
fn read_capped(path: &Path) -> String {
    use std::io::Read as _;
    let mut text = String::new();
    if let Ok(file) = std::fs::File::open(path)
        && file.take(4096).read_to_string(&mut text).is_err()
    {
        return String::new();
    }
    text
}

/// The canonical path when it resolves, else the lexical absolute: deny
/// rules still name the configured spelling when the target is missing.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
fn rooted_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_absolute(path))
}

/// The fixed prefix of the first line a confined wrapper prints before it
/// runs the user program. Private: only the strip adapter recognizes it,
/// and only on the first output line.
#[cfg(target_os = "macos")]
const MARKER_PREFIX: &str = "FIBER_SANDBOX_CONFINED";

/// How many pre-newline bytes the strip adapter buffers before it stops
/// waiting for the marker and fails closed. The marker is tens of bytes;
/// wrapper diagnostics stay far below this, and anything longer without a
/// newline cannot be a confined marker.
#[cfg(target_os = "macos")]
const MARKER_STASH_CAP: usize = 65_536;

/// Mints one marker token per call from the process id and a counter: the
/// token only has to be unique per call, and no clock is read.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn marker_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let count = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{}-{count}", std::process::id())
}

/// Wraps the user command so the shell prints the fixed-prefix marker line
/// to its stdout before it runs the user program. The marker write precedes
/// user exec in the same shell process, so its bytes precede any user byte
/// on the pipe; the exit status stays the command's.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn wrap_command(command: &str, token: &str) -> String {
    format!("printf '%s\\n' '{MARKER_PREFIX} {token}'; {command}")
}

/// Whether the confined wrapper proved it runs inside the sandbox, decided
/// from the first output line. Shared between the reader thread, which
/// feeds it through the strip adapter, and the spawn site, which reads it
/// after the drive loop.
#[cfg(target_os = "macos")]
pub(crate) struct MarkerWatch {
    /// The exact first line a confined wrapper prints.
    expected: Vec<u8>,
    /// Set once the first line matches the expected marker.
    seen: AtomicBool,
    /// The first line, or the bytes before end-of-file without one: the
    /// wrapper's diagnostic when the marker never arrives, capped.
    head: Mutex<Vec<u8>>,
    /// Wakes the drive loop parked in `drive_confined`: the reader observes
    /// the marker on its own thread, and a quiet command offers no later
    /// output to wake on, so the observation itself must wake the loop.
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[cfg(target_os = "macos")]
impl MarkerWatch {
    /// Watches for one call's marker line.
    pub(crate) fn new(token: &str) -> Self {
        Self {
            expected: format!("{MARKER_PREFIX} {token}").into_bytes(),
            seen: AtomicBool::new(false),
            head: Mutex::new(Vec::new()),
            wake: Mutex::new(None),
        }
    }

    /// Installs the drive loop's wake, called once when the marker is
    /// first observed. Installed by the spawn site after the shared state
    /// exists; without it the loop still wakes on later output or EOF.
    pub(crate) fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *support::lock(&self.wake) = Some(wake);
    }

    /// Whether the wrapper's first line was the marker.
    pub(crate) fn observed(&self) -> bool {
        self.seen.load(Ordering::SeqCst)
    }

    /// Records the wrapper's first line: a match confirms confinement, a
    /// miss fails it closed. A pty translates the newline to
    /// carriage-return newline, so one trailing carriage return is not the
    /// user program's.
    pub(crate) fn check(&self, mut line: &[u8]) -> bool {
        if let Some(stripped) = line.strip_suffix(b"\r") {
            line = stripped;
        }
        let seen = line == self.expected.as_slice();
        if seen && !self.seen.swap(true, Ordering::SeqCst) {
            // First observation only: a quiet command offers no later
            // output, so this wake is what hands its parked drive loop
            // off right after the marker. Cloned out from under the
            // lock, so the wake never runs while this mutex is held.
            let wake = support::lock(&self.wake).clone();
            if let Some(wake) = wake {
                wake();
            }
        }
        support::lock(&self.head).extend_from_slice(line);
        seen
    }

    /// Records end-of-file without a marker: whatever the wrapper wrote is
    /// the diagnostic the fail-closed cause names.
    pub(crate) fn miss(&self, bytes: &[u8]) {
        support::lock(&self.head).extend_from_slice(bytes);
    }

    /// The recorded first line or pre-EOF bytes, lossy and capped.
    pub(crate) fn head_text(&self) -> String {
        let head = support::lock(&self.head);
        let mut text = String::from_utf8_lossy(&head).into_owned();
        text.truncate(4096);
        text
    }
}

/// A reader that buffers the first output line, recognizes the confined
/// marker there, and suppresses exactly that line. Every later byte passes
/// through untouched, so user output is byte-identical past the marker; a
/// user program echoing the marker text afterwards is user data, never
/// stripped.
#[cfg(target_os = "macos")]
pub(crate) struct StripMarker<R> {
    inner: R,
    /// Bytes before the first newline, not yet classified.
    stash: Vec<u8>,
    /// Classified user bytes waiting to be read.
    pending: Vec<u8>,
    /// The marker is classified: later reads pass through.
    done: bool,
    watch: Arc<MarkerWatch>,
}

#[cfg(target_os = "macos")]
impl<R: std::io::Read> StripMarker<R> {
    /// Strips one call's marker line, reporting to `watch`.
    pub(crate) fn new(inner: R, watch: Arc<MarkerWatch>) -> Self {
        Self {
            inner,
            stash: Vec::new(),
            pending: Vec::new(),
            done: false,
            watch,
        }
    }

    /// Drains the classified bytes into `buf`.
    fn drain(&mut self, buf: &mut [u8]) -> usize {
        let count = self.pending.len().min(buf.len());
        if let (Some(front), Some(slot)) = (self.pending.get(..count), buf.get_mut(..count)) {
            slot.copy_from_slice(front);
            self.pending.drain(..count);
            count
        } else {
            0
        }
    }
}

#[cfg(target_os = "macos")]
impl<R: std::io::Read> std::io::Read for StripMarker<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut chunk = [0u8; 1024];
        loop {
            if !self.pending.is_empty() {
                return Ok(self.drain(buf));
            }
            if self.done {
                // The marker is classified: later bytes pass through, and
                // only a true end-of-file returns zero. A suppressed marker
                // that arrived alone must never read as end-of-file: the
                // reader leaving early would close the pipe while the
                // command still runs.
                return self.inner.read(buf);
            }
            let read = self.inner.read(&mut chunk)?;
            if read == 0 {
                self.done = true;
                self.watch.miss(&self.stash);
                self.pending = std::mem::take(&mut self.stash);
                continue;
            }
            self.stash
                .extend_from_slice(chunk.get(..read).unwrap_or_default());
            if self.stash.len() > MARKER_STASH_CAP {
                self.done = true;
                self.watch.miss(&self.stash);
                self.pending = std::mem::take(&mut self.stash);
                continue;
            }
            if let Some(end) = self.stash.iter().position(|&byte| byte == b'\n') {
                let line = self.stash.get(..end).unwrap_or_default().to_vec();
                let hit = self.watch.check(&line);
                self.done = true;
                if hit {
                    // Only the marker line is suppressed: user bytes past
                    // it stay byte-identical.
                    self.pending = self.stash.get(end + 1..).unwrap_or_default().to_vec();
                } else {
                    // No marker: the first line is user or wrapper output
                    // and drains with the rest.
                    self.pending = std::mem::take(&mut self.stash);
                }
                self.stash.clear();
            }
        }
    }
}

/// The fail-closed cause for a call whose wrapper never showed its marker:
/// the wrapper's captured diagnostic and status, naming Seatbelt's cause.
#[cfg(target_os = "macos")]
// mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn apply_cause(watch: &MarkerWatch, status: Option<ExitStatus>) -> String {
    let code = status
        .and_then(|status| status.code())
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let head = watch.head_text();
    if head.is_empty() {
        format!("sandbox-exec ended before the confined marker (status {code})")
    } else {
        format!("sandbox-exec ended before the confined marker (status {code}): {head}")
    }
}

/// A stub for non-Linux builds.
// cargo-mutants runs on Linux, where this platform-only stub is not compiled.
#[cfg_attr(false, mutants::skip)]
#[cfg(not(target_os = "linux"))]
pub(crate) fn install(_prepared: &mut Prepared) -> Result<(), i32> {
    Err(rustix::io::Errno::IO.raw_os_error())
}

/// A stub `Prepared` for builds with no sandbox policy.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) struct Prepared;

/// Adds the ancestor-walk grants for one denied path. For each existing
/// ancestor directory on the way, grants read on every entry except those
/// leading to any denied path in `exclusions`: the union over every
/// configured deny and every resolved target, so one deny's walk never
/// re-grants another deny's subtree. The denied path itself, a missing
/// component and a symlink component get nothing: the walk grants the
/// parent's readable siblings then stops, without moving a missing deny
/// point upward and without following links. `root` is `/` in production;
/// fixtures pass their own root.
#[cfg(target_os = "linux")]
fn add_ancestor_grants(
    denied: &Path,
    exclusions: &[PathBuf],
    abi: landlock::ABI,
    root: &Path,
    rules: &mut Vec<landlock::PathBeneath<OwnedFd>>,
) -> Result<(), String> {
    let absolute = normalize_absolute(denied);
    let relative = match absolute.strip_prefix(root) {
        Ok(relative) => relative.to_path_buf(),
        Err(_) => return Ok(()),
    };
    let mut components: Vec<std::ffi::OsString> = Vec::new();
    for component in relative.components() {
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
    let mut prefix = root.to_path_buf();
    for name in components {
        let ancestor = prefix.clone();
        let next_path = ancestor.join(&name);
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
            // Skip every entry leading to any denied path, not only the
            // current one: Landlock unions grants across denies.
            let path = ancestor.join(entry.file_name());
            if exclusions.iter().any(|denied| denied.starts_with(&path)) {
                continue;
            }
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
        match std::fs::symlink_metadata(&next_path) {
            Err(_) => break,
            Ok(metadata) if metadata.file_type().is_symlink() => break,
            Ok(_) => prefix = next_path,
        }
    }
    Ok(())
}

/// Opens `path` with `O_PATH | O_NOFOLLOW | O_CLOEXEC`, so a symlink never
/// resolves to its target. Linux-only: other platforms have no `O_PATH`,
/// and the grants it opens are never built there. The flags combine with
/// `union`, not `|`, so no mutant can swap the operator
/// (`docs/testing/mutants.md`).
#[cfg(target_os = "linux")]
fn open_nofollow(path: &Path) -> Result<OwnedFd, String> {
    use rustix::fs::OFlags;
    let flags = OFlags::RDONLY
        .union(OFlags::PATH)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    rustix::fs::open(path, flags, rustix::fs::Mode::empty())
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

/// Makes `path` absolute and normalizes `.` and `..` lexically, without
/// touching the filesystem.
#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod tests;
