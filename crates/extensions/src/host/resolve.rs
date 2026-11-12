//! Resolve extension-relative executable paths without allowing them to leave their package.

use std::path::{Component, Path, PathBuf};

/// Resolves a Lua `host.exec` `program` against the extension's directory
/// `package`, which is canonical. A bare name resolves on `PATH` and an
/// absolute path is used as written, so both return unchanged. Otherwise the
/// program joins onto `package`, folding `.` and `..` lexically: a `..` that
/// pops past `package` leaves the directory. The deepest existing ancestor
/// of the folded path is canonicalized and must lie under `package`, and
/// every component below `package` that exists must not be a symlink out:
/// one whose target is missing or lies outside `package` leaves the
/// directory too. A path that stays inside but does not exist yet resolves,
/// and the spawn reports it missing. Filesystem reads only.
///
/// Example: `resolve_in_package("bin/acme", "/h/extensions/acme")` is
/// `Ok("/h/extensions/acme/bin/acme")`.
pub(crate) fn resolve_in_package(program: &str, package: &Path) -> Result<String, String> {
    if !program.contains('/') || program.starts_with('/') {
        return Ok(program.to_owned());
    }
    let refused = || {
        format!(
            "host.exec: {program} leaves the extension directory {}",
            package.display()
        )
    };
    let package_len = package.components().count();
    let mut folded = PathBuf::new();
    let mut len = 0usize;
    for component in package.join(program).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if len <= package_len {
                    return Err(refused());
                }
                folded.pop();
                len -= 1;
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                folded.push(component.as_os_str());
                len += 1;
            }
        }
    }
    let mut ancestor = folded.clone();
    let canonical = loop {
        match ancestor.canonicalize() {
            Ok(canonical) => break canonical,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                if !ancestor.pop() {
                    return Err(refused());
                }
            }
            Err(_) => return Err(refused()),
        }
    };
    if !canonical.starts_with(package) {
        return Err(refused());
    }
    let mut prefix = PathBuf::new();
    let mut depth = 0usize;
    for component in folded.components() {
        prefix.push(component.as_os_str());
        depth += 1;
        if depth <= package_len {
            continue;
        }
        // Only the missing tail ends the walk: the ancestor check above
        // already refused every other failure (a loop, a permission
        // error, a path through a file), so any error here is the tail
        // not existing yet. Any other filesystem failure below the
        // package would have failed that check first, absent a
        // concurrent change to the directory.
        let Ok(meta) = std::fs::symlink_metadata(&prefix) else {
            break;
        };
        if meta.file_type().is_symlink() {
            // A dangling target cannot be shown inside, and neither
            // can a loop: any failure to resolve is a refusal.
            let escapes = match prefix.canonicalize() {
                Ok(target) => !target.starts_with(package),
                Err(_) => true,
            };
            if escapes {
                return Err(refused());
            }
        }
    }
    folded.into_os_string().into_string().map_err(|_| refused())
}
