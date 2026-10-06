//! An extension's manifest and a provider's data (`docs/configuration.md`,
//! "An extension's manifest" and "A provider's data"). Only this crate reads
//! either file; `extensions` installs and registers what they describe.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::error::ConfigError;
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

/// `providers/<name>.json`: one provider an extension registers as data.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProviderData {
    /// The provider's name, the first half of every model reference.
    pub name: String,
    /// How its key is found when no credential is stored.
    #[serde(default)]
    pub credential: Option<CredentialSource>,
    /// The stored credential it reads, defaulting to its own name.
    /// Providers in one package that share a key name the same one.
    #[serde(default)]
    pub credential_name: Option<String>,
    /// Headers sent on every request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Its models.
    pub models: Vec<ModelData>,
    /// A small, fast model of this provider for the reviewer when
    /// `reviewer.model` is unset (`docs/model-routing.md`, "What a provider
    /// extension declares").
    #[serde(default)]
    pub reviewer_model: Option<String>,
}

/// One model in a provider's data (`docs/model-routing.md`, "What a provider
/// extension declares").
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelData {
    /// The model's id, as the vendor spells it.
    pub id: String,
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
    pub cost: Option<Cost>,
    /// Whether a subscription login serves it; absent means false.
    #[serde(default)]
    pub subscription: bool,
    /// The vendor's hosted-search tool type, exactly as it is sent, such as
    /// `web_search_20250305`; absent when the model's provider hosts no
    /// search for it.
    #[serde(default)]
    pub web_search: Option<String>,
}

/// A model's prices, in US dollars per million tokens.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Cost {
    /// Input tokens.
    pub input: f64,
    /// Output tokens.
    pub output: f64,
    /// Tokens read from the prompt cache.
    #[serde(default)]
    pub cache_read: Option<f64>,
    /// Tokens written to the prompt cache.
    #[serde(default)]
    pub cache_write: Option<f64>,
    /// Higher input sizes, each with its own four prices; absent means none.
    #[serde(default)]
    pub tiers: Vec<Tier>,
}

/// One price tier when cost varies by request size. A tier states all four
/// prices.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Tier {
    /// Input tokens above which this tier's prices apply to the whole call.
    pub input_tokens_above: u64,
    /// Input tokens.
    pub input: f64,
    /// Output tokens.
    pub output: f64,
    /// Tokens read from the prompt cache.
    pub cache_read: f64,
    /// Tokens written to the prompt cache.
    pub cache_write: f64,
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
}

impl Protocol {
    /// Whether the protocol reads back this hosted-search tool type: only
    /// `anthropic-messages` reads `web_search_20250305`.
    pub fn reads_web_search(self, kind: &str) -> bool {
        matches!(self, Self::AnthropicMessages) && kind == "web_search_20250305"
    }
}

const MIB: u64 = 1 << 20;

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
    Ok(manifest)
}

/// Reads every `providers/<name>.json` in an extension's directory, in name
/// order. A file whose `name` is not its file name is invalid.
pub fn read_providers(dir: &Path) -> Result<Vec<ProviderData>, ConfigError> {
    let dir = dir.join("providers");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(ConfigError::Io { file: dir, source }),
    };
    let mut files = Vec::new();
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
    files
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
