//! An extension's manifest and a provider's data (`docs/configuration.md`,
//! "An extension's manifest" and "A provider's data"). Only this crate reads
//! either file; `extensions` installs and registers what they describe.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::error::ConfigError;
use crate::home::one_file_name;
use crate::names::dir_name;
use crate::secret::CredentialSource;

/// The fields of `extension.json` that installing and loading read. Fields
/// this build does not read are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    /// The extension's name, which is also where it is fetched from.
    pub name: String,
    /// The extension's own version, such as `v1.4.0`.
    pub version: String,
    /// The lowest Fiber version it runs on, such as `0.3.0`.
    pub fiber: String,
    /// The extension API's major version it was written for.
    pub api: u64,
    /// The other extensions it depends on, each with a minimum version.
    #[serde(default)]
    pub depends: BTreeMap<String, String>,
    /// The native binaries it ships, by platform such as `darwin-arm64`.
    #[serde(default)]
    pub binaries: BTreeMap<String, Binary>,
    /// For a process extension, the program it runs.
    #[serde(default)]
    pub process: Option<Process>,
    /// The command Fiber runs in the extension's directory at install and at
    /// every update, such as `["npm", "ci"]`.
    #[serde(default)]
    pub install: Option<Vec<String>>,
    /// A file in the package whose text goes in the system prompt.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Raises a Lua extension's memory cap above the default of 1 MiB.
    #[serde(default)]
    pub memory_mib: Option<u64>,
    /// The built-in tools and commands it replaces.
    #[serde(default)]
    pub replaces: Vec<String>,
    /// Files whose text goes in the opening message, from the extension's
    /// data directories.
    #[serde(default)]
    pub opening: Option<Opening>,
    /// The top-level settings keys a repository's file may set
    /// (`docs/configuration.md`, "Extension settings").
    #[serde(default)]
    pub repo_settings: Vec<String>,
    /// The names the extension reads with `host.secret`; none by default
    /// (`docs/configuration.md`, "Secrets").
    #[serde(default)]
    pub secrets: Vec<String>,
    /// The providers it registers, each with its base URLs; a `{name}`
    /// URL is a per-account host's pattern. None by default.
    #[serde(default)]
    pub providers: BTreeMap<String, Vec<String>>,
}

/// An extension section's files in the opening message
/// (`docs/configuration.md`, "An extension's manifest").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Opening {
    /// Paths in the extension's machine data directory.
    #[serde(default)]
    pub machine: Vec<String>,
    /// Paths in the extension's project data directory.
    #[serde(default)]
    pub project: Vec<String>,
    /// The section's optional byte budget.
    #[serde(default)]
    pub budget_bytes: Option<u64>,
}

/// One platform's binary: where it is downloaded from and its checksum.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Binary {
    /// The download URL.
    pub url: String,
    /// The lowercase hex SHA-256 of the file.
    pub sha256: String,
}

/// A process extension's program.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Process {
    /// The program.
    pub program: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// One per-account host placeholder a provider's models name
/// (`docs/model-routing.md`, "A per-account host").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Placeholder {
    /// The environment variable read when the extension's setting is unset.
    #[serde(default)]
    pub env: Option<String>,
}

/// `providers/<name>.json`: one provider an extension registers as data.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProviderData {
    /// The provider's name, the first half of every model reference.
    pub name: String,
    /// Where its credential comes from: the data's `credential`, `login`
    /// and `credential_label`.
    #[serde(flatten)]
    pub credential_from: CredentialFrom,
    /// The stored credential it reads, defaulting to its own name.
    /// Providers in one package that share a key name the same one.
    #[serde(default)]
    pub credential_name: Option<String>,
    /// Headers sent on every request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The per-account host placeholders its models' `base_url`s name, by
    /// name; absent means none.
    #[serde(default)]
    pub placeholders: BTreeMap<String, Placeholder>,
    /// Its models.
    pub models: Vec<ModelData>,
    /// The package's own reviewer default: `fiber login` preselects it
    /// when no reviewer is saved yet, and it is the final fallback when no
    /// reviewer is configured (`docs/model-routing.md`, "What a provider
    /// extension declares").
    #[serde(default)]
    pub reviewer_model: Option<String>,
}

/// Where a provider's credential comes from (`docs/configuration.md`, "A
/// provider's data"), read from the data's `credential`, `login` and
/// `credential_label`. `credential_label` stands alone: a provider whose
/// `credential()` reads nothing stored has no key source and no login.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "RawCredentialFrom")]
pub enum CredentialFrom {
    /// Fiber stores it: the key `fiber login` stores, else the key
    /// `credential` finds, or the login `login` runs.
    Stored {
        /// How its key is found when no credential is stored.
        credential: Option<CredentialSource>,
        /// How `fiber login` signs in for this provider: `browser` runs the
        /// provider's `credential()` login instead of reading a key. Absent
        /// means a key.
        login: Option<Login>,
    },
    /// Its `credential()` supplies it and reads nothing Fiber stores; the
    /// data's `credential_label` says what a credential label names
    /// (`docs/model-routing.md`, "What a provider extension declares").
    Function(CredentialLabelSources),
}

impl CredentialFrom {
    /// How its key is found when no credential is stored.
    pub fn credential(&self) -> Option<&CredentialSource> {
        match self {
            Self::Stored { credential, .. } => credential.as_ref(),
            Self::Function(_) => None,
        }
    }

    /// How `fiber login` signs in for this provider; `None` means a key.
    pub fn login(&self) -> Option<Login> {
        match self {
            Self::Stored { login, .. } => *login,
            Self::Function(_) => None,
        }
    }

    /// The paths a `credential()` that reads nothing Fiber stores draws
    /// secrets from; none for a stored credential.
    pub fn credential_paths(&self) -> &[CredentialPath] {
        match self {
            Self::Stored { .. } => &[],
            Self::Function(sources) => &sources.credential_paths,
        }
    }
}

/// The data's `credential`, `login` and `credential_label` as written.
#[derive(Deserialize)]
struct RawCredentialFrom {
    #[serde(default)]
    credential: Option<CredentialSource>,
    #[serde(default)]
    login: Option<Login>,
    #[serde(default)]
    credential_label: Option<CredentialLabelSources>,
}

impl TryFrom<RawCredentialFrom> for CredentialFrom {
    type Error = &'static str;

    fn try_from(raw: RawCredentialFrom) -> Result<Self, Self::Error> {
        match (raw.credential_label, raw.credential, raw.login) {
            (Some(sources), None, None) => Ok(Self::Function(sources)),
            (Some(_), _, _) => {
                Err("`credential_label` is never declared beside `login` or `credential`")
            }
            (None, credential, login) => Ok(Self::Stored { credential, login }),
        }
    }
}

/// What a credential label names for a provider whose `credential()` reads
/// nothing Fiber stores (`docs/configuration.md`, "A provider's data").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialLabelSources {
    /// What a label names, such as `AWS profile`.
    pub names: String,
    /// What the `default` label reads, such as the environment.
    pub default: String,
    /// The paths its `credential()` draws secrets from, which the
    /// credential deny protects (`docs/permissions.md`, "Credentials").
    #[serde(default)]
    pub credential_paths: Vec<CredentialPath>,
}

/// One path a provider's `credential()` draws secrets from
/// (`docs/configuration.md`, "A provider's data").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialPath {
    /// The path, such as `~/.aws/credentials`.
    pub path: String,
    /// A variable whose value, when set and not empty, replaces `path`.
    #[serde(default)]
    pub env: Option<String>,
    /// One name appended after that choice, such as a file in a directory
    /// the variable moves.
    #[serde(default)]
    pub join: Option<String>,
}

impl CredentialPath {
    /// The path this entry names under `lookup`'s environment: `env`'s
    /// value when it is set and not empty, else `path`; a leading `~` or
    /// `~/` read from `HOME`; then `join`. `None` when that names nothing:
    /// an empty text, a `~` with `HOME` unset or empty, or a `join` that is
    /// not one name. Nothing is collapsed and no file is read.
    pub fn resolve(&self, lookup: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
        let set = |name: &str| lookup(name).filter(|value| !value.is_empty());
        let text = self
            .env
            .as_deref()
            .and_then(set)
            .unwrap_or_else(|| self.path.clone());
        let mut path = match text.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                PathBuf::from(format!("{}{rest}", set("HOME")?))
            }
            _ if text.is_empty() => return None,
            _ => PathBuf::from(text),
        };
        if let Some(name) = &self.join {
            if name.is_empty() || name == "." || name == ".." || name.contains('/') {
                return None;
            }
            path.push(name);
        }
        Some(path)
    }
}

/// How `fiber login` signs in for a provider
/// (`docs/configuration.md`, "A provider's data").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Login {
    /// Runs the provider's `credential()` login in a browser (or by device
    /// code with `--device`).
    Browser,
}

/// One model in a provider's data (`docs/model-routing.md`, "What a provider
/// extension declares").
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelData {
    /// The model's id, as the vendor spells it.
    pub id: String,
    /// Its display name, such as `Claude Opus 5.5`; absent when the
    /// provider data names none.
    #[serde(default)]
    pub name: Option<String>,
    /// The wire protocol it speaks.
    pub protocol: Protocol,
    /// Its base URL.
    pub base_url: String,
    /// The flags the protocol reads. Fiber never guesses one.
    #[serde(default)]
    pub compat: Map<String, Value>,
    /// Whether deferred tools work for it; absent means false.
    #[serde(default)]
    pub deferred_tools: bool,
    /// Extra request body fields.
    #[serde(default)]
    pub extra_body: Map<String, Value>,
    /// Its context window, in tokens.
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Its output token limit.
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// The input kinds it takes, such as `text` and `image`.
    #[serde(default)]
    pub input: Vec<String>,
    /// Its prices.
    #[serde(default)]
    pub cost: Option<contract::provider::Cost>,
    /// Whether a subscription login serves it; absent means false.
    #[serde(default)]
    pub subscription: bool,
    /// The vendor's hosted-search tool type, exactly as it is sent, such as
    /// `web_search_20250305`; absent when the model's provider hosts no
    /// search for it.
    #[serde(default)]
    pub web_search: Option<String>,
    /// The thinking levels the model takes (`docs/model-routing.md`,
    /// "Thinking"); absent means it takes none.
    #[serde(default)]
    pub thinking_levels: Vec<contract::ThinkingLevel>,
    /// The model's own default level, used when no session choice or
    /// configured value names one (`docs/model-routing.md`, "Thinking").
    #[serde(default)]
    pub thinking_default: Option<contract::ThinkingLevel>,
    /// A Markdown file in the extension package whose text is appended to
    /// the system prompt for this model only
    /// (`docs/system-prompt.md`, "The model's addendum").
    #[serde(default)]
    pub prompt_addendum: Option<String>,
}

/// A wire protocol (`docs/model-routing.md`, "Protocols and providers").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// `anthropic-messages`.
    AnthropicMessages,
    /// `openai-completions`.
    OpenaiCompletions,
    /// `openai-responses`.
    OpenaiResponses,
    /// `google-generative-ai`.
    GoogleGenerativeAi,
    /// `bedrock-converse`.
    BedrockConverse,
    /// `scripted`: reads a script file instead of a socket. Built into the
    /// binary, so no package's data may declare it
    /// (`docs/model-routing.md`, "The scripted provider").
    #[serde(skip_deserializing)]
    Scripted,
}

impl Protocol {
    /// Whether the protocol reads back this hosted-search tool type:
    /// `anthropic-messages` reads `web_search_20250305`,
    /// `openai-responses` reads `web_search`, `openai-completions` reads
    /// `openrouter:web_search`, and `google-generative-ai` reads
    /// `google_search`; every other pair is unread.
    pub fn reads_web_search(self, kind: &str) -> bool {
        match self {
            Self::AnthropicMessages => kind == "web_search_20250305",
            Self::OpenaiResponses => kind == "web_search",
            Self::GoogleGenerativeAi => kind == "google_search",
            Self::OpenaiCompletions => kind == "openrouter:web_search",
            Self::BedrockConverse | Self::Scripted => false,
        }
    }

    /// The request fields Fiber builds for this protocol, which a model's
    /// `extra_body` may not name (`docs/model-routing.md`, "Extra request
    /// body fields"), in the doc table's order.
    pub fn reserved_body_fields(self) -> &'static [&'static str] {
        match self {
            Self::AnthropicMessages => &[
                "model",
                "system",
                "messages",
                "tools",
                "tool_choice",
                "stream",
            ],
            Self::OpenaiCompletions => &["model", "messages", "tools", "tool_choice", "stream"],
            Self::OpenaiResponses => &[
                "model",
                "instructions",
                "input",
                "tools",
                "tool_choice",
                "stream",
            ],
            Self::GoogleGenerativeAi => &["systemInstruction", "contents", "tools", "toolConfig"],
            Self::BedrockConverse => &["system", "messages", "toolConfig"],
            // No request body is built.
            Self::Scripted => &[],
        }
    }
}

const MIB: u64 = 1 << 20;

/// Whether `path` names a file inside a data directory: non-empty and made
/// only of normal components.
fn is_inside(path: &str) -> bool {
    !path.contains('\\')
        && path.as_bytes().get(1) != Some(&b':')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

/// Reads the UTF-8 text of `relative` inside the extension package `dir`.
/// The path must pass the `is_inside` rule (relative, no `.`/`..`/empty
/// part, no `\`, no drive letter), the file must exist, and it must be
/// UTF-8. Any failure is a `ConfigError` naming `key` and the file, so one
/// check serves both `prompt` and `prompt_addendum`.
pub fn read_package_text(dir: &Path, relative: &str, key: &str) -> Result<String, ConfigError> {
    if !is_inside(relative) {
        return Err(ConfigError::WrongType {
            source_name: dir.join(relative).display().to_string(),
            key: key.into(),
            expected: "a path relative to the package directory and inside it".into(),
        });
    }
    let file = dir.join(relative);
    if let (Ok(canonical_dir), Ok(canonical_file)) =
        (std::fs::canonicalize(dir), std::fs::canonicalize(&file))
        && !canonical_file.starts_with(&canonical_dir)
    {
        return Err(ConfigError::WrongType {
            source_name: file.display().to_string(),
            key: key.into(),
            expected: "a path relative to the package directory and inside it".into(),
        });
    }
    let bytes = fs::read(&file).map_err(|source| ConfigError::WrongType {
        source_name: file.display().to_string(),
        key: key.into(),
        expected: format!("a file that reads ({source})"),
    })?;
    String::from_utf8(bytes).map_err(|_| ConfigError::WrongType {
        source_name: file.display().to_string(),
        key: key.into(),
        expected: "UTF-8 text".into(),
    })
}

/// Whether `dir` holds an extension's manifest: `extension.json` at
/// its top is a regular file. A probe only, never a read and never an
/// error (`docs/configuration.md`, "Files and format").
pub fn has_manifest(dir: &Path) -> bool {
    dir.join("extension.json").is_file()
}

/// Reads `extension.json` at the top of an extension's directory.
pub fn read_manifest(dir: &Path) -> Result<Manifest, ConfigError> {
    let file = dir.join("extension.json");
    let manifest: Manifest = read_typed(&file, "an extension's manifest")?;
    if let Some(n) = manifest.memory_mib
        && (n == 0
            || n.checked_mul(MIB)
                .is_none_or(|bytes| usize::try_from(bytes).is_err()))
    {
        return Err(ConfigError::WrongType {
            source_name: file.display().to_string(),
            key: "memory_mib".into(),
            expected: "a whole number of MiB above 0".into(),
        });
    }
    if let Some(opening) = &manifest.opening {
        for path in opening.machine.iter().chain(opening.project.iter()) {
            if !is_inside(path) {
                return Err(ConfigError::WrongType {
                    source_name: file.display().to_string(),
                    key: "opening".into(),
                    expected: "paths relative to the data directory and inside it".into(),
                });
            }
        }
    }
    if manifest.providers.keys().any(|name| !one_file_name(name)) {
        return Err(ConfigError::WrongType {
            source_name: file.display().to_string(),
            key: "providers".into(),
            expected: "names that are each one file name in providers/".into(),
        });
    }
    if manifest.secrets.iter().any(|name| !one_file_name(name)) {
        return Err(ConfigError::WrongType {
            source_name: file.display().to_string(),
            key: "secrets".into(),
            expected: "names that are each one file name in credentials/".into(),
        });
    }
    Ok(manifest)
}

/// Lists every `providers/*.json` in an extension's directory, in name
/// order. A missing directory is an empty list.
pub fn provider_files(dir: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let dir = dir.join("providers");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(ConfigError::Io { file: dir, source }),
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let file = entry
            .map_err(|source| ConfigError::Io {
                file: dir.clone(),
                source,
            })?
            .path();
        if file.extension().is_some_and(|e| e == "json") {
            files.push(file);
        }
    }
    files.sort();
    Ok(files)
}

/// Reads every `providers/<name>.json` in an extension's directory, in name
/// order. A file whose `name` is not its file name is invalid.
pub fn read_providers(dir: &Path) -> Result<Vec<ProviderData>, ConfigError> {
    provider_files(dir)?
        .into_iter()
        .map(|file| {
            let data: ProviderData = read_typed(&file, "a provider's data")?;
            if file
                .file_stem()
                .is_some_and(|stem| stem == data.name.as_str())
            {
                Ok(data)
            } else {
                Err(ConfigError::WrongType {
                    source_name: file.display().to_string(),
                    key: "name".into(),
                    expected: "the file's own name, without `.json`".into(),
                })
            }
        })
        .collect()
}

fn read_typed<T: DeserializeOwned>(file: &Path, expected: &'static str) -> Result<T, ConfigError> {
    let bytes = fs::read(file).map_err(|source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|e| {
        let (line, column) = (e.line(), e.column());
        if e.is_data() {
            ConfigError::Shape {
                file: file.to_path_buf(),
                line,
                column,
                expected,
            }
        } else {
            ConfigError::Json {
                file: file.to_path_buf(),
                line,
                column,
            }
        }
    })
}

/// An extension's declared setting keys: the manifest's `repo_settings`
/// plus every placeholder name across its `providers/*.json`
/// (`docs/configuration.md`, "Extension settings" and
/// `docs/model-routing.md`, "A per-account host"). The only two declared
/// key lists in the code, so the unknown-setting gate checks a key's
/// first segment against this union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredSettings {
    /// The top-level settings keys a repository's file may set, never a
    /// placeholder name: a placeholder's setting is never a
    /// `repo_settings` key, so a repository never sets it.
    pub repo_settings: Vec<String>,
    /// Every placeholder name the provider data declares.
    pub placeholders: BTreeSet<String>,
}

/// Reads the declared settings of the extension in `dir`: the manifest's
/// `repo_settings` and the placeholder names of every provider file. An
/// unreadable manifest or provider file is an error, writing nothing: the
/// caller writes nothing on any `Err`. Never a repository's working
/// files: the caller passes the installed or approved directory.
pub fn declared_settings_in(dir: &Path) -> Result<DeclaredSettings, ConfigError> {
    let manifest = read_manifest(dir)?;
    let mut placeholders = BTreeSet::new();
    for provider in read_providers(dir)? {
        placeholders.extend(provider.placeholders.into_keys());
    }
    // A placeholder's setting is never a `repo_settings` key: even a
    // manifest listing one there keeps it out, so the repository gate
    // and the read filter refuse it as undeclared there.
    let repo_settings = manifest
        .repo_settings
        .into_iter()
        .filter(|name| !placeholders.contains(name))
        .collect();
    Ok(DeclaredSettings {
        repo_settings,
        placeholders,
    })
}

/// The installed directory of the extension `extension` spells, when its
/// manifest is present: `extensions/<slug>/` under Fiber home, the slug
/// naming a first-party extension by its short name. A probe only, never
/// a read and never an error.
pub fn installed_dir(home: &Path, extension: &str) -> Option<PathBuf> {
    let dir = home
        .join("extensions")
        .join(dir_name(&crate::names::full_name(extension)));
    dir.join("extension.json").is_file().then_some(dir)
}

/// `value` as a host: at most one leading `https://` and one trailing `/`
/// are stripped, and what remains must be ASCII letters, digits, `.` and
/// `-`, with an optional `:` and a port from 0 to 65535
/// (`docs/model-routing.md`, "A per-account host"). The result is a
/// sub-slice of `value`, never a copy. A pure string function: no I/O,
/// and the refusal never repeats the value.
pub fn host_shape(value: &str) -> Option<&str> {
    let rest = value.strip_prefix("https://").unwrap_or(value);
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return None;
    }
    if let Some(port) = port {
        // Only digits read as a port: `u16` parsing alone would take a
        // leading `+`, so the digit check stays while the empty check
        // goes, an empty port failing the parse below just the same.
        if !port.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if port.parse::<u16>().is_err() {
            return None;
        }
    }
    Some(rest)
}

#[cfg(test)]
#[path = "extension_set_tests.rs"]
mod extension_set_tests;
