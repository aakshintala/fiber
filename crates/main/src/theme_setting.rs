//! The terminal's theme from `tui.theme` (`docs/configuration.md`, "Keys";
//! `docs/tui.md`, "Themes"): `main` supplies the reader and the package
//! list, `config` resolves the file, and the terminal parses what it
//! returns. Only `config` knows the layout of Fiber home
//! (`docs/architecture.md`, "The call rules").

use std::io;
use std::path::Path;

use config::Config;

/// The theme `tui.theme` names. Unset or `auto` follows the terminal's
/// appearance; `dark` and `light` are always the built-ins and read
/// nothing; any other name is `themes/<name>.json` in `home`, then in each
/// healthy, enabled installed extension's, read with `read`, its contents or the
/// read error carried as text. Fiber home wins over any extension, and
/// between extensions the first by directory name in `extensions/` wins.
/// A switched-off extension's themes are not listed or loaded: one naming
/// them reads as a missing file. Only an absent file passes the name on: a file that is there but is
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
    let enabled = |extension: &str| extensions::is_enabled(config, extension);
    named(home, name.as_deref(), read, &enabled)
}

/// The theme `name` gives `tui.theme`, by the rules [`setting`] reads it
/// with: what `/settings` applies when a theme is chosen. `enabled`
/// names the extensions that load; a switched-off extension's files are
/// skipped, as if missing.
pub(crate) fn named(
    home: &Path,
    name: Option<&str>,
    read: &dyn Fn(&Path) -> io::Result<String>,
    enabled: &dyn Fn(&str) -> bool,
) -> tui::ThemeSetting {
    match name {
        None | Some("auto") => tui::ThemeSetting::Follow,
        Some("dark") => tui::ThemeSetting::Dark,
        Some("light") => tui::ThemeSetting::Light,
        Some(name) => tui::ThemeSetting::File {
            name: name.to_owned(),
            text: config::theme_text(home, name, &extensions::package_names(home), enabled, read)
                .map_err(|error| error.to_string()),
        },
    }
}

/// Every theme [`named`] can load, each once, sorted: Fiber home's
/// `themes/` and each healthy, enabled installed extension's. A
/// switched-off extension contributes none.
pub(crate) fn names(home: &Path, enabled: &dyn Fn(&str) -> bool) -> Vec<String> {
    config::theme_names(home, &extensions::package_names(home), enabled)
}

#[cfg(test)]
#[path = "theme_setting_tests.rs"]
mod tests;
