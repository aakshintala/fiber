//! The hub's login service on disk (`docs/invocation.md`, "The hub"): which
//! service manager runs it, its name, where its unit file lives, and the
//! file's text. One service per Fiber home: the name encodes the home's
//! path, so two homes never share one.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::shapes::Failure;

use crate::{failed, usage};

/// The longest service name, in bytes. A home whose name is longer is
/// refused; the socket path limit keeps a real home far below it.
const NAME_MAX: usize = 240;

/// The service manager that runs the hub on this platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Manager {
    /// launchd, in the logged-in user's `gui/<uid>` domain.
    Launchd { uid: u32 },
    /// systemd's user instance.
    Systemd,
}

impl Manager {
    /// launchd on macOS, systemd everywhere else.
    pub(crate) fn current(uid: u32) -> Self {
        if cfg!(target_os = "macos") {
            Self::Launchd { uid }
        } else {
            Self::Systemd
        }
    }
}

/// The hub's login service for one Fiber home.
#[derive(Debug)]
pub(crate) struct Service {
    pub(crate) manager: Manager,
    /// The launchd label, or the systemd unit's name without `.service`.
    pub(crate) name: String,
    /// The plist or unit file.
    pub(crate) unit: PathBuf,
    /// The Fiber binary the service runs.
    pub(crate) exe: PathBuf,
    /// The Fiber home the service's hub serves.
    pub(crate) home: PathBuf,
}

impl Service {
    /// The service for `home`, running `exe`, with its unit file found
    /// through the environment `var` reads.
    pub(crate) fn locate(
        manager: Manager,
        home: &Path,
        exe: &Path,
        var: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<Self, Failure> {
        let name = name(home)?;
        let unit = unit_path(manager, &name, var)?;
        Ok(Self {
            manager,
            name,
            unit,
            exe: exe.to_path_buf(),
            home: home.to_path_buf(),
        })
    }

    /// The unit file's text, carrying `port` on its first comment line so a
    /// port change is a change of the file.
    pub(crate) fn render(&self, port: Option<u16>) -> Result<String, Failure> {
        let exe = plain(&self.exe)?;
        let home = plain(&self.home)?;
        let port = port.map_or_else(|| "none".to_owned(), |port| port.to_string());
        Ok(match self.manager {
            Manager::Systemd => {
                let exec = [exe, "hub", "serve", "--installed"]
                    .map(|arg| format!("\"{}\"", systemd_quoted(arg).replace('$', "$$")))
                    .join(" ");
                format!(
                    "# hub.port {port}\n\
                     [Unit]\n\
                     Description=Fiber hub\n\
                     \n\
                     [Service]\n\
                     ExecStart={exec}\n\
                     Environment=\"FIBER_HOME={}\"\n\
                     KillMode=process\n\
                     Restart=on-failure\n\
                     RestartSec=10\n\
                     SuccessExitStatus=129 130 143\n\
                     \n\
                     [Install]\n\
                     WantedBy=default.target\n",
                    systemd_quoted(home)
                )
            }
            Manager::Launchd { .. } => {
                let args = [exe, "hub", "serve", "--installed"]
                    .map(|arg| format!("<string>{}</string>", xml(arg)))
                    .concat();
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                     <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                     <!-- hub.port {port} -->\n\
                     <plist version=\"1.0\">\n\
                     <dict>\n\
                     \x20 <key>Label</key><string>{}</string>\n\
                     \x20 <key>ProgramArguments</key><array>{args}</array>\n\
                     \x20 <key>EnvironmentVariables</key><dict><key>FIBER_HOME</key><string>{}</string></dict>\n\
                     \x20 <key>RunAtLoad</key><true/>\n\
                     \x20 <key>KeepAlive</key><true/>\n\
                     \x20 <key>AbandonProcessGroup</key><true/>\n\
                     </dict>\n\
                     </plist>\n",
                    xml(&self.name),
                    xml(home)
                )
            }
        })
    }
}

/// `fiber-hub-` and the home's path after its leading `/`: each `/` becomes
/// `-`, each byte in `[A-Za-z0-9.]` is kept, and every other byte becomes
/// `_` and two lowercase hex digits. `-` comes only from `/` and `_` always
/// starts a three-byte escape, so two homes never share a name.
pub(crate) fn name(home: &Path) -> Result<String, Failure> {
    let bytes = home.as_os_str().as_bytes();
    let bytes = bytes.strip_prefix(b"/").unwrap_or(bytes);
    let mut name = String::from("fiber-hub-");
    for &byte in bytes {
        match byte {
            b'/' => name.push('-'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' => name.push(char::from(byte)),
            _ => name.push_str(&format!("_{byte:02x}")),
        }
    }
    if name.len() > NAME_MAX {
        return Err(usage(format!(
            "Fiber home's path is too long to name the hub's login service ({} bytes, at most \
             {NAME_MAX}); set FIBER_HOME to a shorter path.",
            name.len()
        )));
    }
    Ok(name)
}

/// Where the service's unit file lives: on macOS
/// `$HOME/Library/LaunchAgents/<name>.plist`; on Linux
/// `$XDG_CONFIG_HOME/systemd/user/<name>.service` when that variable is
/// absolute, else under `$HOME/.config`.
pub(crate) fn unit_path(
    manager: Manager,
    name: &str,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, Failure> {
    let home = || {
        var("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or_else(|| usage("HOME must be an absolute path to find the hub's login service."))
    };
    Ok(match manager {
        Manager::Launchd { .. } => home()?
            .join("Library/LaunchAgents")
            .join(format!("{name}.plist")),
        Manager::Systemd => {
            let config = match var("XDG_CONFIG_HOME").map(PathBuf::from) {
                Some(config) if config.is_absolute() => config,
                Some(_) | None => home()?.join(".config"),
            };
            config.join("systemd/user").join(format!("{name}.service"))
        }
    })
}

/// The path as UTF-8 with no control character, which neither unit format
/// can carry safely.
fn plain(path: &Path) -> Result<&str, Failure> {
    path.to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .ok_or_else(|| {
            failed(
                ErrorCode::IoFailed,
                format!(
                    "{path:?} is not UTF-8 or holds a control character, so the hub's login \
                     service cannot name it."
                ),
            )
        })
}

/// Escapes text inside a systemd double-quoted value: `\`, `"` and the
/// specifier `%`.
fn systemd_quoted(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
}

/// Escapes text inside a plist `<string>`.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
#[path = "hub_unit_tests.rs"]
mod tests;
