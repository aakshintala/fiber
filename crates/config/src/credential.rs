//! Finding a provider's key (`docs/model-routing.md`, "Credentials"): the
//! stored credential first, then the source the person configured, then the
//! one the provider's data declares.

use std::fs;
use std::io::ErrorKind;
use std::process::{Command, Stdio};

use crate::Config;
use crate::error::ConfigError;
use crate::extension::ProviderData;
use crate::secret::{CredentialSource, Secret, read_secret};

impl Config {
    /// The provider's key. A stored credential owns its provider: when
    /// `credentials/<name>` exists but cannot be used, that is the error, and
    /// no other source is tried. Otherwise `providers."<name>".credential`
    /// from the global or per-project layer replaces the provider's own
    /// source. A command runs each time this is called; the caller asks once
    /// per process.
    pub fn credential(&self, provider: &ProviderData) -> Result<Secret, ConfigError> {
        let name = &provider.name;
        let missing = |why: String| ConfigError::CredentialMissing {
            provider: name.clone(),
            why,
        };
        if let Some(stored) = read_secret(&self.home, name)? {
            return usable(stored.expose())
                .ok_or_else(|| missing(format!("credentials/{name} is empty")));
        }
        let configured = self
            .merged(None)
            .get("providers")
            .and_then(|p| p.get(name))
            .and_then(|p| p.get("credential"))
            .map(|v| serde_json::from_value::<CredentialSource>(v.clone()))
            .and_then(Result::ok);
        let Some(source) = configured.or_else(|| provider.credential.clone()) else {
            return Err(missing(format!(
                "nothing is stored in credentials/{name}, and the provider declares no other source"
            )));
        };
        let found = match &source {
            CredentialSource::Env(var) => match std::env::var(var) {
                Ok(value) => usable(&value),
                Err(_) => None,
            },
            CredentialSource::File(file) => match fs::read_to_string(file) {
                Ok(value) => usable(&value),
                Err(e) if e.kind() == ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(ConfigError::Io {
                        file: file.clone(),
                        source,
                    });
                }
            },
            CredentialSource::Command(argv) => run(argv),
        };
        found.ok_or_else(|| {
            let from = match &source {
                CredentialSource::Env(var) => format!("the environment variable {var} is not set"),
                CredentialSource::File(file) => format!("{} does not exist", file.display()),
                CredentialSource::Command(argv) => {
                    format!("`{}` printed no key", argv.join(" "))
                }
            };
            missing(format!(
                "nothing is stored in credentials/{name}, and {from}"
            ))
        })
    }
}

/// The value with surrounding whitespace removed, `None` when that leaves
/// nothing.
fn usable(value: &str) -> Option<Secret> {
    let value = value.trim();
    (!value.is_empty()).then(|| Secret::new(value.into()))
}

/// What the command prints on stdout when it succeeds.
fn run(argv: &[String]) -> Option<Secret> {
    let (program, args) = argv.split_first()?;
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    usable(&String::from_utf8_lossy(&output.stdout))
}
