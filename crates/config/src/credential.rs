//! Finding a provider's key (`docs/model-routing.md`, "Credentials"): the
//! stored credential of the label first, then the source the person
//! configured for that label, then, for the label `default`, the one the
//! provider's data declares.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use contract::Secret;
use serde_json::{Map, Value};

use crate::Config;
use crate::error::ConfigError;
use crate::extension::ProviderData;
use crate::secret::{CredentialSource, credential_labels, read_credential};

/// The label of the source a provider's data declares, and of the key
/// `fiber login` stores without a label.
const DEFAULT_LABEL: &str = "default";

/// A key that was read, and the file it was read from.
#[derive(Debug)]
pub struct Read {
    /// The key.
    pub secret: Secret,
    /// The canonical path of the `file` source the key was read from;
    /// `None` for a stored credential, an `env` or a `command` source.
    pub file: Option<PathBuf>,
}

/// Runs a credential source's built command and collects its output.
pub type Runner<'a> = &'a dyn Fn(&mut Command) -> io::Result<Output>;

/// The key of each `command` source that has run, by stored credential
/// name, label and command. A clone of the [`Config`] shares it.
#[derive(Clone, Default)]
pub(crate) struct CommandRuns(Arc<Mutex<BTreeMap<RunKey, Secret>>>);

/// The stored credential name, the label and the command's words.
type RunKey = (String, String, Vec<String>);

/// A command's key, or why it gave none.
type Ran = Result<Secret, String>;

impl CommandRuns {
    /// The recorded key for `key`, running `run` and recording its key first
    /// when there is none. A failure is not recorded, so the next call runs
    /// the command again. The lock is held across the run, so concurrent
    /// callers share one run.
    fn once(&self, key: RunKey, run: impl FnOnce() -> Ran) -> Ran {
        let mut runs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(secret) = runs.get(&key) {
            return Ok(secret.clone());
        }
        let secret = run()?;
        runs.insert(key, secret.clone());
        Ok(secret)
    }
}

/// Resolves a `file` source's path to the file that is read.
type Canonical<'a> = &'a dyn Fn(&Path) -> io::Result<PathBuf>;

impl Config {
    /// The label a session of this provider uses when nothing else names
    /// one: `providers."<name>".credential` from the global or per-project
    /// layer, else `default`.
    pub fn credential_label(&self, provider: &ProviderData) -> String {
        self.merged(None)
            .get("providers")
            .and_then(|p| p.get(&provider.name))
            .and_then(|p| p.get("credential"))
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_LABEL)
            .to_owned()
    }

    /// The provider's key under `label`. The stored credential
    /// `credentials/<stored>/<label>`, where `<stored>` is the credential
    /// the provider's data names or its own name, comes first: when it
    /// exists but cannot be used, that is the error, and no other source is
    /// tried under the label. Otherwise `providers."<name>".credentials."<label>"`
    /// from the global or per-project layer, and for `default` only, the
    /// provider's own source. A command runs the first time this is called for
    /// its credential and label, and its key serves every later call on this
    /// configuration and its clones, so a command that gave a key runs once
    /// per process. A command that failed runs again on the next call.
    pub fn credential(&self, provider: &ProviderData, label: &str) -> Result<Secret, ConfigError> {
        self.credential_with(provider, label, &|command: &mut Command| command.output())
            .map(|read| read.secret)
    }

    /// As [`Config::credential`], running a `command` source through `run`,
    /// which gets the built command: program, arguments, stdin and stderr
    /// null, stdout piped. A `file` source is canonicalised and read through
    /// its canonical path, which [`Read::file`] names, so the bytes read and
    /// the path the credential deny protects are the same file
    /// (`docs/permissions.md`, "Credentials"). When the path cannot be
    /// canonicalised nothing is read.
    pub fn credential_with(
        &self,
        provider: &ProviderData,
        label: &str,
        run: Runner<'_>,
    ) -> Result<Read, ConfigError> {
        self.credential_through(provider, label, run, &|file: &Path| fs::canonicalize(file))
    }

    fn credential_through(
        &self,
        provider: &ProviderData,
        label: &str,
        run: Runner<'_>,
        canonical: Canonical<'_>,
    ) -> Result<Read, ConfigError> {
        let name = &provider.name;
        let stored = provider.credential_name.as_deref().unwrap_or(name);
        let missing = |why: String| ConfigError::CredentialMissing {
            provider: name.clone(),
            why,
        };
        if let Some(secret) = read_credential(&self.home, stored, label)? {
            return usable(secret.expose())
                .map(|secret| Read { secret, file: None })
                .ok_or_else(|| ConfigError::CredentialFailed {
                    provider: name.clone(),
                    why: format!("credentials/{stored}/{label} is empty"),
                });
        }
        let merged = self.merged(None);
        let configured = configured(&merged, name);
        let from_config = configured
            .and_then(|labels| labels.get(label))
            .and_then(source);
        let own = (label == DEFAULT_LABEL)
            .then(|| provider.credential.clone())
            .flatten();
        let Some(source) = from_config.or(own) else {
            let listed = match self.labels(provider).as_slice() {
                [] => "none".to_owned(),
                labels => labels.join(", "),
            };
            return Err(missing(format!(
                "nothing is stored in credentials/{stored}/{label}, and no source is configured for it. The labels for `{name}` are: {listed}"
            )));
        };
        let mut read_from = None;
        let found = match &source {
            CredentialSource::Env(var) => match std::env::var(var) {
                Ok(value) => {
                    usable(&value).ok_or_else(|| format!("the environment variable {var} is empty"))
                }
                Err(_) => Err(format!("the environment variable {var} is not set")),
            },
            CredentialSource::File(file) => match canonical(file)
                .and_then(|real| fs::read_to_string(&real).map(|value| (real, value)))
            {
                Ok((real, value)) => {
                    read_from = Some(real);
                    usable(&value).ok_or_else(|| format!("{} is empty", file.display()))
                }
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
            CredentialSource::Command(argv) => self
                .commands
                .once((stored.to_owned(), label.to_owned(), argv.clone()), || {
                    command(argv, run)
                }),
        };
        found
            .map(|secret| Read {
                secret,
                file: read_from,
            })
            .map_err(|from| {
                missing(format!(
                    "nothing is stored in credentials/{stored}/{label}, and {from}"
                ))
            })
    }
}

impl Config {
    /// Every credential label `provider` has: each stored under
    /// `credentials/<stored>/`, each configured at
    /// `providers."<name>".credentials`, and `default` when the provider's
    /// data declares a source. Sorted, each once; an unreadable directory
    /// lists nothing from it.
    pub fn labels(&self, provider: &ProviderData) -> Vec<String> {
        let stored = provider
            .credential_name
            .as_deref()
            .unwrap_or(&provider.name);
        let mut labels: BTreeSet<String> = credential_labels(&self.home, stored)
            .unwrap_or_default()
            .into_iter()
            .collect();
        labels.extend(
            configured(&self.merged(None), &provider.name)
                .into_iter()
                .flat_map(|labels| labels.keys().cloned()),
        );
        if provider.credential.is_some() {
            labels.insert(DEFAULT_LABEL.into());
        }
        labels.into_iter().collect()
    }
}

impl Config {
    /// The path of every `file` credential source configuration declares,
    /// as written: each `providers."<name>".credentials."<label>"` that
    /// [`Config::credential`] would read, and each of `providers`' own
    /// sources. Each appears once. The credential deny protects them
    /// (`docs/permissions.md`, "Credentials").
    pub fn credential_files<'a>(
        &self,
        providers: impl IntoIterator<Item = &'a ProviderData>,
    ) -> Vec<PathBuf> {
        let merged = self.merged(None);
        let names = merged
            .get("providers")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(Map::keys);
        let labelled = names
            .filter_map(|name| configured(&merged, name))
            .flat_map(Map::values)
            .filter_map(source);
        let own = providers.into_iter().filter_map(|p| p.credential.clone());
        let files: BTreeSet<PathBuf> = labelled
            .chain(own)
            .filter_map(|source| match source {
                // An empty path names no file and is never read.
                CredentialSource::File(file) => (!file.as_os_str().is_empty()).then_some(file),
                CredentialSource::Env(_) | CredentialSource::Command(_) => None,
            })
            .collect();
        files.into_iter().collect()
    }
}

/// The labels `providers."<name>".credentials` configures in `merged`.
fn configured<'a>(merged: &'a Value, name: &str) -> Option<&'a Map<String, Value>> {
    merged
        .get("providers")
        .and_then(|p| p.get(name))
        .and_then(|p| p.get("credentials"))
        .and_then(Value::as_object)
}

/// The source a configured label's `value` declares; `None` when it is
/// not one.
fn source(value: &Value) -> Option<CredentialSource> {
    serde_json::from_value(value.clone()).ok()
}

/// The value with surrounding whitespace removed, `None` when that leaves
/// nothing.
fn usable(value: &str) -> Option<Secret> {
    let value = value.trim();
    (!value.is_empty()).then(|| Secret::new(value.into()))
}

/// What the command prints on stdout when `run` runs it and it succeeds,
/// or why it gave no key. The command is named by its program alone: its
/// arguments may hold a key.
fn command(argv: &[String], run: Runner<'_>) -> Result<Secret, String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("the configured command is empty".into());
    };
    let output = run(Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()))
    .map_err(|e| format!("`{program}` could not be started: {e}"))?;
    if !output.status.success() {
        return Err(format!("`{program}` failed ({})", output.status));
    }
    usable(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| format!("`{program}` printed no key"))
}

#[cfg(test)]
#[path = "credential_tests.rs"]
mod tests;
