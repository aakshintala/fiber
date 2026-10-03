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
    /// The provider's key. The stored credential it names, or its own
    /// name when it names none, comes first: when `credentials/<stored>`
    /// exists but cannot be used, that is the error, and no other source
    /// is tried. Otherwise `providers."<name>".credential` from the global
    /// or per-project layer replaces the provider's own source. A command
    /// runs each time this is called; the caller asks once per process.
    pub fn credential(&self, provider: &ProviderData) -> Result<Secret, ConfigError> {
        let name = &provider.name;
        let stored = provider.credential_name.as_deref().unwrap_or(name);
        let missing = |why: String| ConfigError::CredentialMissing {
            provider: name.clone(),
            why,
        };
        if let Some(secret) = read_secret(&self.home, stored)? {
            return usable(secret.expose()).ok_or_else(|| ConfigError::CredentialFailed {
                provider: name.clone(),
                why: format!("credentials/{stored} is empty"),
            });
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
                "nothing is stored in credentials/{stored}, and the provider declares no other source"
            )));
        };
        let found = match &source {
            CredentialSource::Env(var) => match std::env::var(var) {
                Ok(value) => {
                    usable(&value).ok_or_else(|| format!("the environment variable {var} is empty"))
                }
                Err(_) => Err(format!("the environment variable {var} is not set")),
            },
            CredentialSource::File(file) => match fs::read_to_string(file) {
                Ok(value) => usable(&value).ok_or_else(|| format!("{} is empty", file.display())),
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    Err(format!("{} does not exist", file.display()))
                }
                Err(source) => {
                    return Err(ConfigError::Io {
                        file: file.clone(),
                        source,
                    });
                }
            },
            CredentialSource::Command(argv) => run(argv),
        };
        found.map_err(|from| {
            missing(format!(
                "nothing is stored in credentials/{stored}, and {from}"
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

/// What the command prints on stdout when it succeeds, or why it gave no
/// key.
fn run(argv: &[String]) -> Result<Secret, String> {
    let shown = argv.join(" ");
    let Some((program, args)) = argv.split_first() else {
        return Err("the configured command is empty".into());
    };
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("`{shown}` could not be started: {e}"))?;
    if !output.status.success() {
        return Err(format!("`{shown}` failed ({})", output.status));
    }
    usable(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| format!("`{shown}` printed no key"))
}
