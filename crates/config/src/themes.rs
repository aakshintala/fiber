//! The terminal's themes (`docs/tui.md`, "Themes"): the text of the theme
//! file `tui.theme` names, from Fiber home's `themes/` or an installed
//! extension's package directory. Fiber home wins over any extension, and
//! between extensions the first in the given package order wins. Only
//! `config` knows these paths (`docs/architecture.md`, "The call rules");
//! `main` supplies the package list and the enabled filter.

use std::io;
use std::path::{Path, PathBuf};

use crate::ConfigError;

/// The text of the theme `name`: Fiber home's `themes/<name>.json`, then
/// each enabled package's in the given order. A name that fails the shared
/// rule is refused unread, so `tui.theme` never reaches outside `themes/`.
/// Only absence passes the name on: a file that is there but whose read
/// fails with any error other than `NotFound` is the theme. When every
/// source lacks the file, the text is home's `NotFound` error string.
/// `enabled` is not called when home supplies the file.
pub fn theme_text(
    home: &Path,
    name: &str,
    packages: &[(String, PathBuf)],
    enabled: &dyn Fn(&str) -> bool,
    read: &dyn Fn(&Path) -> io::Result<String>,
) -> Result<String, ConfigError> {
    if !is_name(name) {
        return Err(ConfigError::BadThemeName);
    }
    let file = format!("{name}.json");
    let home_error = match read(&home.join("themes").join(&file)) {
        Ok(text) => return Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => error,
        Err(error) => return Err(ConfigError::ThemeRead(error.to_string())),
    };
    for (extension, package) in packages {
        if !enabled(extension) {
            continue;
        }
        match read(&package.join("themes").join(&file)) {
            Ok(text) => return Ok(text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ConfigError::ThemeRead(error.to_string())),
        }
    }
    Err(ConfigError::ThemeRead(home_error.to_string()))
}

/// Every theme [`theme_text`] can load, each once, sorted: Fiber home's
/// `themes/` and each enabled package's. A switched-off extension
/// contributes none.
pub fn theme_names(
    home: &Path,
    packages: &[(String, PathBuf)],
    enabled: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut names = Vec::new();
    collect(&home.join("themes"), &mut names);
    for (extension, package) in packages {
        if enabled(extension) {
            collect(&package.join("themes"), &mut names);
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Whether `name` can name a theme file: non-empty, no leading `.`, no
/// path separator, and not a reserved name. Listing and loading share
/// this rule, so every listed name loads.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && !matches!(name, "auto" | "dark" | "light")
}

/// Adds the loadable theme names in `dir` to `names`: `.json` files whose
/// stem passes the shared rule. Best-effort: an unreadable directory or
/// entry contributes nothing.
fn collect(dir: &Path, names: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(stem) = entry
            .file_name()
            .into_string()
            .ok()
            .and_then(|name| name.strip_suffix(".json").map(str::to_owned))
        else {
            continue;
        };
        if is_name(&stem) {
            names.push(stem);
        }
    }
}
