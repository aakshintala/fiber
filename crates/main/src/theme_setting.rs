//! The terminal's theme from `tui.theme` (`docs/configuration.md`, "Keys";
//! `docs/tui.md`, "Themes"): `main` reads the theme file, from Fiber home's
//! `themes/` or an installed extension's, and the terminal parses it.

use std::io;
use std::path::Path;

use config::Config;

/// The theme `tui.theme` names. Unset or `auto` follows the terminal's
/// appearance; `dark` and `light` are always the built-ins and read
/// nothing; any other name is `themes/<name>.json` in `home`, then in each
/// healthy installed extension's, read with `read`, its contents or the
/// read error carried as text. Fiber home wins over any extension, and
/// between extensions the first by directory name in `extensions/` wins.
/// Only an absent file passes the name on: a file that is there but is
/// refused, or whose read fails with any error other than `NotFound`, is
/// the theme. A name that fails the shared rule is refused unread, so
/// `tui.theme` never reaches outside `themes/`.
pub(crate) fn setting(
    home: &Path,
    config: &Config,
    read: &dyn Fn(&Path) -> io::Result<String>,
) -> tui::ThemeSetting {
    let name = config
        .get("tui.theme", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    named(home, name.as_deref(), read)
}

/// The theme `name` gives `tui.theme`, by the rules [`setting`] reads it
/// with: what `/settings` applies when a theme is chosen.
pub(crate) fn named(
    home: &Path,
    name: Option<&str>,
    read: &dyn Fn(&Path) -> io::Result<String>,
) -> tui::ThemeSetting {
    match name {
        None | Some("auto") => tui::ThemeSetting::Follow,
        Some("dark") => tui::ThemeSetting::Dark,
        Some("light") => tui::ThemeSetting::Light,
        Some(name) if !is_name(name) => tui::ThemeSetting::File {
            name: name.to_owned(),
            text: Err("not a theme name".to_owned()),
        },
        Some(name) => tui::ThemeSetting::File {
            name: name.to_owned(),
            text: lookup(home, name, read),
        },
    }
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

/// The text of the theme `name`: Fiber home's `themes/<name>.json`, then
/// each healthy extension's in directory order. Only absence passes the
/// name on; when every source lacks the file, the text is home's
/// `NotFound` error string.
fn lookup(
    home: &Path,
    name: &str,
    read: &dyn Fn(&Path) -> io::Result<String>,
) -> Result<String, String> {
    let file = format!("{name}.json");
    let home_error = match read(&home.join("themes").join(&file)) {
        Ok(text) => return Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => error,
        Err(error) => return Err(error.to_string()),
    };
    for package in extensions::package_dirs(home) {
        match read(&package.join("themes").join(&file)) {
            Ok(text) => return Ok(text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err(home_error.to_string())
}

/// Every theme [`named`] can load, each once, sorted: Fiber home's
/// `themes/` and each healthy installed extension's.
pub(crate) fn names(home: &Path) -> Vec<String> {
    let mut names = Vec::new();
    collect(&home.join("themes"), &mut names);
    for package in extensions::package_dirs(home) {
        collect(&package.join("themes"), &mut names);
    }
    names.sort();
    names.dedup();
    names
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

#[cfg(test)]
#[path = "theme_setting_tests.rs"]
mod tests;
