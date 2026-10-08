//! The terminal's theme from `tui.theme` (`docs/configuration.md`, "Keys";
//! `docs/tui.md`, "Themes"): `main` reads the theme file, and the terminal
//! parses it.

use std::io;
use std::path::Path;

use config::Config;

/// The theme `tui.theme` names. Unset or `auto` follows the terminal's
/// appearance; `dark` and `light` are always the built-ins and read
/// nothing; any other name is `themes/<name>.json` in `home`, read with
/// `read`, its contents or the read error carried as text. A name that is
/// empty, starts with `.` or holds a path separator is refused unread, so
/// `tui.theme` never reaches outside `themes/`.
pub(crate) fn setting(
    home: &Path,
    config: &Config,
    read: &dyn Fn(&Path) -> io::Result<String>,
) -> tui::ThemeSetting {
    let name = config
        .get("tui.theme", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    match name.as_deref() {
        None | Some("auto") => tui::ThemeSetting::Follow,
        Some("dark") => tui::ThemeSetting::Dark,
        Some("light") => tui::ThemeSetting::Light,
        Some(name) if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\']) => {
            tui::ThemeSetting::File {
                name: name.to_owned(),
                text: Err("not a theme name".to_owned()),
            }
        }
        Some(name) => tui::ThemeSetting::File {
            name: name.to_owned(),
            text: read(&home.join("themes").join(format!("{name}.json")))
                .map_err(|error| error.to_string()),
        },
    }
}

#[cfg(test)]
#[path = "theme_setting_tests.rs"]
mod tests;
