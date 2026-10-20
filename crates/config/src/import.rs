//! One-time import of another harness's configuration
//! (`docs/configuration.md`, "Importing"): reads `~/.claude` (Claude Code)
//! or `~/.codex` (Codex), copies what maps into Fiber home, names the rest
//! in a left-behind list, and writes a receipt so the next run for the same
//! source succeeds without reading the source again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use contract::clock::Clock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Source;
use crate::error::ConfigError;
use crate::home::{plain, read, read_bytes};
use crate::keys;
use crate::layout::global_config_file;
use crate::write::atomic::{locked, write_atomic};
use crate::{get_global, valid_env_key};

/// Which harness to import from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    /// Claude Code, read from `~/.claude`.
    ClaudeCode,
    /// Codex, read from `~/.codex`.
    Codex,
}

impl ImportSource {
    /// The name the receipt and the report use: `claude-code` or `codex`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }

    /// The source's home directory name under the person's home.
    fn dir_name(&self) -> &'static str {
        match self {
            Self::ClaudeCode => ".claude",
            Self::Codex => ".codex",
        }
    }

    /// The receipt file's name at the top of Fiber home.
    fn receipt_name(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "import-claude-code.json",
            Self::Codex => "import-codex.json",
        }
    }

    /// The instruction file's name, in the source and in Fiber home.
    fn instruction_name(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "CLAUDE.md",
            Self::Codex => "AGENTS.md",
        }
    }

    /// The flat prompt directory's name in the source: `commands` or
    /// `prompts`.
    fn flat_dir(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "commands",
            Self::Codex => "prompts",
        }
    }

    /// Reads the source's MCP servers: `settings.json` `mcpServers` for
    /// Claude Code, `config.toml` `[mcp_servers]` for Codex.
    fn read_servers(&self, source_home: &Path) -> (Vec<RawServer>, Vec<LeftBehind>) {
        match self {
            Self::ClaudeCode => read_claude_servers(&source_home.join("settings.json")),
            Self::Codex => read_codex_servers(&source_home.join("config.toml")),
        }
    }
}

/// What the import copied, by final name in Fiber home.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Imported {
    /// MCP servers written under `mcp.servers`, by name.
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    /// Skills written under `skills/`, by directory name.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Flat commands and prompts wrapped as prompt templates, by skill
    /// directory name.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Instruction files written at the top of Fiber home, by file name.
    #[serde(default)]
    pub instructions: Vec<String>,
}

/// One source item the import did not copy, and why. The reason never
/// holds a secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeftBehind {
    /// The source item, by path or name, never a value.
    pub item: String,
    /// Why it was not imported.
    pub reason: String,
}

/// The import's result: what landed and what was left behind with reasons.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    /// What the import copied, by final name.
    #[serde(default)]
    pub imported: Imported,
    /// What the import did not copy, with reasons.
    #[serde(default)]
    pub left_behind: Vec<LeftBehind>,
}

/// The receipt at the top of Fiber home: the source, the time and the
/// report, so a completed receipt short-circuits the next run.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Receipt {
    source: String,
    imported_at: u64,
    #[serde(default)]
    imported: Imported,
    #[serde(default)]
    left_behind: Vec<LeftBehind>,
}

/// Copies another harness's configuration into Fiber home, once. With a
/// completed receipt for `source`, succeeds with the receipt's report
/// without reading `source_home` at all. Otherwise classifies every source
/// item first — secrets and malformed entries are refused before any write
/// — then writes one file at a time and the receipt last, only on full
/// success. A write failure aborts with an error and no receipt for the
/// source, so a retry completes what is missing. `clock` stamps the
/// receipt.
pub fn import_source(
    home: &Path,
    source_home: &Path,
    source: ImportSource,
    clock: &dyn Clock,
) -> Result<ImportReport, ConfigError> {
    let receipt_file = home.join(source.receipt_name());
    if let Some(report) = read_receipt(&receipt_file, source)? {
        return Ok(report);
    }
    let mut planned = classify(home, source_home, source)?;
    write_all(home, &mut planned)?;
    let report = planned.report();
    write_receipt(&receipt_file, source, clock, &report)?;
    Ok(report)
}

/// Reads a completed receipt: the report it holds when its source matches,
/// `None` when there is none, it does not parse, or it names another
/// source.
fn read_receipt(file: &Path, source: ImportSource) -> Result<Option<ImportReport>, ConfigError> {
    let Some(bytes) = read_bytes(file)? else {
        return Ok(None);
    };
    let receipt: Receipt = match serde_json::from_slice(&bytes) {
        Ok(receipt) => receipt,
        Err(_) => return Ok(None),
    };
    if receipt.source == source.name() {
        Ok(Some(ImportReport {
            imported: receipt.imported,
            left_behind: receipt.left_behind,
        }))
    } else {
        Ok(None)
    }
}

/// Everything the classify phase found, before any write.
struct Planned {
    skills: Vec<PlannedFile>,
    commands: Vec<PlannedFile>,
    instructions: Vec<PlannedFile>,
    servers: Vec<PlannedServer>,
    left_behind: Vec<LeftBehind>,
}

/// One file to write under `skills/`: a copied skill or a wrapped flat
/// command or prompt.
struct PlannedFile {
    /// The directory name under `skills/`.
    name: String,
    /// The `SKILL.md` bytes.
    bytes: Vec<u8>,
}

/// One MCP server to write under `mcp.servers`.
struct PlannedServer {
    /// The entry's name.
    name: String,
    /// The entry's value.
    value: Value,
}

impl Planned {
    fn report(&self) -> ImportReport {
        let mut imported = Imported::default();
        for skill in &self.skills {
            imported.skills.push(skill.name.clone());
        }
        for command in &self.commands {
            imported.commands.push(command.name.clone());
        }
        for instruction in &self.instructions {
            imported.instructions.push(instruction.name.clone());
        }
        for server in &self.servers {
            imported.mcp_servers.push(server.name.clone());
        }
        ImportReport {
            imported,
            left_behind: self.left_behind.clone(),
        }
    }
}

/// Reads the source and Fiber home's current state, planning every write
/// and every left-behind entry. Reads only; writes nothing.
fn classify(home: &Path, source_home: &Path, source: ImportSource) -> Result<Planned, ConfigError> {
    let mut left_behind = Vec::new();
    let mut skills = Vec::new();
    let mut commands = Vec::new();
    if plain(source_home, true)? {
        read_skills(source_home, &mut skills, &mut left_behind)?;
        read_flat(source, source_home, &mut commands, &mut left_behind)?;
        top_level_left_behind(source, source_home, &mut left_behind)?;
    }
    let mut instructions = Vec::new();
    read_instruction(
        source,
        source_home,
        home,
        &mut instructions,
        &mut left_behind,
    )?;
    let (raw_servers, mut server_left) = source.read_servers(source_home);
    left_behind.append(&mut server_left);
    let mut servers = Vec::new();
    for raw in raw_servers {
        classify_server(raw, &mut servers, &mut left_behind);
    }
    allocate_names(home, &mut skills, &mut commands, &mut servers)?;
    Ok(Planned {
        skills,
        commands,
        instructions,
        servers,
        left_behind,
    })
}

/// Copies each `skills/*/SKILL.md` directory into the plan. A skill entry
/// without a `SKILL.md`, or one that cannot be read, joins left-behind.
fn read_skills(
    source_home: &Path,
    skills: &mut Vec<PlannedFile>,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<(), ConfigError> {
    let dir = source_home.join("skills");
    let mut names = read_dir_names(&dir)?;
    names.sort();
    for name in names {
        let entry = dir.join(&name);
        if !is_dir(&entry) {
            left_behind.push(LeftBehind {
                item: format!("skills/{name}"),
                reason: "is not a directory".into(),
            });
            continue;
        }
        match read_bytes(&entry.join("SKILL.md"))? {
            Some(bytes) => skills.push(PlannedFile {
                name: name.clone(),
                bytes,
            }),
            None => left_behind.push(LeftBehind {
                item: format!("skills/{name}/SKILL.md"),
                reason: "holds no SKILL.md".into(),
            }),
        }
    }
    Ok(())
}

/// Wraps each flat `commands/*.md` or `prompts/*.md` file as a prompt
/// template: the same directory shape holding a synthesized header (its
/// name from the file stem, its description from the first non-empty line,
/// model invocation disabled) plus the file's bytes as the body. A file
/// with no description joins left-behind: the wrap would be invalid.
fn read_flat(
    source: ImportSource,
    source_home: &Path,
    commands: &mut Vec<PlannedFile>,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<(), ConfigError> {
    let flat = source.flat_dir();
    let dir = source_home.join(flat);
    let mut names = read_dir_names(&dir)?;
    names.sort();
    for name in names {
        let path = dir.join(&name);
        if !is_file(&path) || !name.ends_with(".md") {
            left_behind.push(LeftBehind {
                item: format!("{flat}/{name}"),
                reason: "is not a command file".into(),
            });
            continue;
        }
        let Some(bytes) = read_bytes(&path)? else {
            left_behind.push(LeftBehind {
                item: format!("{flat}/{name}"),
                reason: "cannot be read".into(),
            });
            continue;
        };
        let stem = name.trim_end_matches(".md").to_owned();
        let description = first_line(&bytes);
        if description.is_empty() {
            left_behind.push(LeftBehind {
                item: format!("{flat}/{name}"),
                reason: "holds no description to wrap".into(),
            });
            continue;
        }
        let mut wrapped = format!(
            "---\nname: {stem}\ndescription: {description}\ndisable-model-invocation: true\n---\n\n"
        );
        wrapped.push_str(&String::from_utf8_lossy(&bytes));
        commands.push(PlannedFile {
            name: stem,
            bytes: wrapped.into_bytes(),
        });
    }
    Ok(())
}

/// The body of a wrapped template's header: the first non-empty line, or
/// the file's name when the file holds no text.
fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    String::new()
}

/// Plans the instruction file: copied when Fiber home holds nothing under
/// its name or the same bytes, left behind when the target is occupied,
/// since a suffixed fixed name is never read.
fn read_instruction(
    source: ImportSource,
    source_home: &Path,
    home: &Path,
    instructions: &mut Vec<PlannedFile>,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<(), ConfigError> {
    let name = source.instruction_name();
    let Some(bytes) = read_bytes(&source_home.join(name))? else {
        return Ok(());
    };
    match read_bytes(&home.join(name))? {
        None => instructions.push(PlannedFile {
            name: name.to_owned(),
            bytes,
        }),
        Some(current) if current == bytes => instructions.push(PlannedFile {
            name: name.to_owned(),
            bytes,
        }),
        Some(_) => left_behind.push(LeftBehind {
            item: name.to_owned(),
            reason: format!("{name} already exists in Fiber home"),
        }),
    }
    Ok(())
}

/// Names one source's other top-level entries in left-behind: everything
/// unmapped lands there with its category and reason, never silently.
fn top_level_left_behind(
    source: ImportSource,
    source_home: &Path,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<(), ConfigError> {
    let mapped: &[&str] = match source {
        ImportSource::ClaudeCode => &["skills", "commands", "CLAUDE.md", "settings.json"],
        ImportSource::Codex => &["skills", "prompts", "config.toml", "AGENTS.md"],
    };
    let mut names = read_dir_names(source_home)?;
    names.sort();
    for name in names {
        if mapped.contains(&name.as_str()) {
            continue;
        }
        left_behind.push(LeftBehind {
            item: name.clone(),
            reason: unmapped_reason(&name).into(),
        });
    }
    Ok(())
}

/// Why a top-level source entry is not imported.
fn unmapped_reason(name: &str) -> &'static str {
    if matches!(name, "auth.json" | "auth" | "credentials") {
        return "credentials are never imported";
    }
    if name == "agents" {
        return "agents are not configuration";
    }
    if name == "hooks" || name == "hooks.json" {
        return "hooks are not imported";
    }
    if matches!(name, "projects" | "sessions" | "history" | "todos" | "todo") {
        return "sessions, history and todos are not imported";
    }
    if name == "permissions" {
        return "permissions are not imported";
    }
    "is not configuration Fiber imports"
}

/// One directory's entry names. A missing directory holds nothing, never
/// an error. An entry whose name is not one file name joins no plan: the
/// caller names it left behind under its lossy spelling.
fn read_dir_names(dir: &Path) -> Result<Vec<String>, ConfigError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ConfigError::Io {
                file: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ConfigError::Io {
            file: dir.to_path_buf(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if crate::home::one_file_name(&name) {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// Whether `path` is a directory, following links. Anything else, or a
/// missing path, is not.
fn is_dir(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Whether `path` is a regular file, following links. Anything else, or a
/// missing path, is not.
fn is_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

/// One MCP server as the source states it, before validation.
struct RawServer {
    name: String,
    fields: BTreeMap<String, RawValue>,
}

/// One source field's value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RawValue {
    Text(String),
    Flag(bool),
    List(Vec<String>),
    Table(BTreeMap<String, String>),
}

/// Reads Claude Code's `settings.json` `mcpServers` object.
fn read_claude_servers(file: &Path) -> (Vec<RawServer>, Vec<LeftBehind>) {
    let mut left_behind = Vec::new();
    let Some(bytes) = (match read_bytes(file) {
        Ok(bytes) => bytes,
        Err(e) => {
            left_behind.push(LeftBehind {
                item: "settings.json".into(),
                reason: format!("cannot be read: {e}"),
            });
            return (Vec::new(), left_behind);
        }
    }) else {
        return (Vec::new(), left_behind);
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            left_behind.push(LeftBehind {
                item: "settings.json".into(),
                reason: "does not parse as JSON".into(),
            });
            return (Vec::new(), left_behind);
        }
    };
    let Value::Object(top) = value else {
        left_behind.push(LeftBehind {
            item: "settings.json".into(),
            reason: "is not a JSON object".into(),
        });
        return (Vec::new(), left_behind);
    };
    for key in top.keys() {
        if key != "mcpServers" {
            left_behind.push(LeftBehind {
                item: format!("settings.json {key}"),
                reason: "only `mcpServers` is imported from settings.json".into(),
            });
        }
    }
    let Some(servers) = top.get("mcpServers") else {
        return (Vec::new(), left_behind);
    };
    let Value::Object(entries) = servers else {
        left_behind.push(LeftBehind {
            item: "settings.json mcpServers".into(),
            reason: "is not an object".into(),
        });
        return (Vec::new(), left_behind);
    };
    let mut raw = Vec::new();
    for (name, entry) in entries {
        let Value::Object(fields) = entry else {
            left_behind.push(LeftBehind {
                item: format!("mcpServers {name}"),
                reason: "is not an object".into(),
            });
            continue;
        };
        let mut server = RawServer {
            name: name.clone(),
            fields: BTreeMap::new(),
        };
        for (key, value) in fields {
            match json_field(value) {
                Some(field) => {
                    server.fields.insert(key.clone(), field);
                }
                None => {
                    server.fields.insert(
                        key.clone(),
                        RawValue::Table(BTreeMap::from([(
                            String::new(),
                            "unrepresentable".to_owned(),
                        )])),
                    );
                }
            }
        }
        raw.push(server);
    }
    (raw, left_behind)
}

/// One JSON field as a raw value: strings, booleans, string lists and
/// string tables. Anything else is `None`, and the caller refuses the
/// server holding it.
fn json_field(value: &Value) -> Option<RawValue> {
    match value {
        Value::String(text) => Some(RawValue::Text(text.clone())),
        Value::Bool(flag) => Some(RawValue::Flag(*flag)),
        Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .map(RawValue::List),
        Value::Object(table) => table
            .iter()
            .map(|(key, item)| item.as_str().map(|text| (key.clone(), text.to_owned())))
            .collect::<Option<BTreeMap<_, _>>>()
            .map(RawValue::Table),
        Value::Null | Value::Number(_) => None,
    }
}

/// Validates one server entry against the field map: `command` to
/// `command`, `args` to `args`, string `env` values to `env`, `url` to
/// `url`, and `required` only when the source declares it. Anything else —
/// both transports, neither, a secret or malformed value, an extra key —
/// joins left-behind, never interpreted.
fn classify_server(
    raw: RawServer,
    servers: &mut Vec<PlannedServer>,
    left_behind: &mut Vec<LeftBehind>,
) {
    let item = format!("server {}", raw.name);
    let mut refuse = |reason: String| {
        left_behind.push(LeftBehind {
            item: item.clone(),
            reason,
        });
    };
    if !valid_server_name(&raw.name) {
        refuse("is not a name Fiber writes".into());
        return;
    }
    for key in raw.fields.keys() {
        if !matches!(
            key.as_str(),
            "command" | "args" | "env" | "url" | "required"
        ) {
            refuse(format!("holds a key Fiber does not import: `{key}`"));
            return;
        }
    }
    let command = match raw.fields.get("command") {
        None => None,
        Some(RawValue::Text(command)) if !command.is_empty() => Some(command.clone()),
        Some(RawValue::Text(_)) => {
            refuse("has an empty `command`".into());
            return;
        }
        Some(RawValue::Flag(_) | RawValue::List(_) | RawValue::Table(_)) => {
            refuse("`command` is not a string".into());
            return;
        }
    };
    let url = match raw.fields.get("url") {
        None => None,
        Some(RawValue::Text(url)) => {
            if url_has_userinfo(url) {
                refuse("`url` carries a user name or password".into());
                return;
            }
            Some(url.clone())
        }
        Some(RawValue::Flag(_) | RawValue::List(_) | RawValue::Table(_)) => {
            refuse("`url` is not a string".into());
            return;
        }
    };
    match (&command, &url) {
        (Some(_), Some(_)) => {
            refuse("declares both `command` and `url`".into());
            return;
        }
        (None, None) => {
            refuse("declares neither `command` nor `url`".into());
            return;
        }
        _ => {}
    }
    let mut entry = Map::new();
    if let Some(url) = url {
        entry.insert("url".to_owned(), Value::from(url));
    } else if let Some(command) = command {
        entry.insert("command".to_owned(), Value::from(command));
        if let Some(args) = raw.fields.get("args") {
            let RawValue::List(args) = args else {
                refuse("`args` is not a list of strings".into());
                return;
            };
            if !args.is_empty() {
                entry.insert(
                    "args".to_owned(),
                    Value::Array(args.iter().cloned().map(Value::from).collect()),
                );
            }
        }
        match raw.fields.get("env") {
            None => {}
            Some(RawValue::Table(table)) => {
                let mut env = Map::new();
                for (key, text) in table {
                    if text == "unrepresentable" {
                        refuse(format!("`env` holds a secret value for `{key}`"));
                        return;
                    }
                    if !valid_env_key(key) {
                        refuse(format!("`env` key `{key}` is not a variable name"));
                        return;
                    }
                    if is_secret_key(key) {
                        refuse(format!("`env` holds a secret value for `{key}`"));
                        return;
                    }
                    env.insert(key.clone(), Value::from(text.clone()));
                }
                if !env.is_empty() {
                    entry.insert("env".to_owned(), Value::Object(env));
                }
            }
            Some(_) => {
                refuse("`env` is not an object of strings".into());
                return;
            }
        }
    }
    if let Some(required) = raw.fields.get("required") {
        match required {
            RawValue::Flag(true) => {
                entry.insert("required".to_owned(), Value::from(true));
            }
            // `false` is the default: without it the entry reads the same.
            RawValue::Flag(false) => {}
            RawValue::Text(_) | RawValue::List(_) | RawValue::Table(_) => {
                refuse("`required` is not true or false".into());
                return;
            }
        }
    }
    commit_server_entry(raw.name, entry, servers);
}

/// Records one validated server entry.
fn commit_server_entry(name: String, entry: Map<String, Value>, servers: &mut Vec<PlannedServer>) {
    servers.push(PlannedServer {
        name,
        value: Value::Object(entry),
    });
}

/// Whether `name` is a server name Fiber writes: non-empty, with none of
/// the bytes `fiber mcp add` refuses.
fn valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && !name.bytes().any(|byte| {
            byte == b'"' || byte == b'/' || byte.is_ascii_whitespace() || byte.is_ascii_control()
        })
}

/// Whether an `env` key names a secret: any `_`-separated word of it is a
/// secret word, so the value is a credential whatever it holds and the
/// server joins left-behind without its value.
fn is_secret_key(key: &str) -> bool {
    const SECRET_WORDS: [&str; 8] = [
        "TOKEN",
        "SECRET",
        "AUTH",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "PRIVATE",
        "KEY",
    ];
    key.split('_')
        .any(|word| SECRET_WORDS.contains(&word.to_uppercase().as_str()))
}

/// Whether `url` carries a user name or password: an `@` in its authority,
/// as `fiber mcp add` refuses it.
fn url_has_userinfo(url: &str) -> bool {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    authority.contains('@')
}

/// Reads Codex's `config.toml` `[mcp_servers]` tables through the narrow
/// subset reader: section headers plus scalar `command`, `args`, `env`,
/// `url` and `required` lines only. Anything outside that subset — other
/// tables, other keys, multi-line or otherwise unparseable values — is
/// refused per server to left-behind, never interpreted.
fn read_codex_servers(file: &Path) -> (Vec<RawServer>, Vec<LeftBehind>) {
    let mut left_behind = Vec::new();
    let Some(bytes) = (match read_bytes(file) {
        Ok(bytes) => bytes,
        Err(e) => {
            left_behind.push(LeftBehind {
                item: "config.toml".into(),
                reason: format!("cannot be read: {e}"),
            });
            return (Vec::new(), left_behind);
        }
    }) else {
        return (Vec::new(), left_behind);
    };
    let body = match String::from_utf8(bytes) {
        Ok(body) => body,
        Err(_) => {
            left_behind.push(LeftBehind {
                item: "config.toml".into(),
                reason: "is not UTF-8 text".into(),
            });
            return (Vec::new(), left_behind);
        }
    };
    let mut reader = TomlReader::default();
    for line in body.lines() {
        reader.read_line(line);
    }
    reader.finish(&mut left_behind);
    (reader.servers(), left_behind)
}

/// One section the narrow TOML reader tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TomlSection {
    /// A table outside `[mcp_servers]`.
    Other(String),
    /// `[mcp_servers]` itself: its keys are not server declarations.
    Mcp,
    /// One `[mcp_servers.<name>]` table.
    Server(String),
}

/// The narrow Codex `config.toml` reader: section headers plus scalar
/// lines only, refusing everything outside the subset per server.
#[derive(Debug, Default)]
struct TomlReader {
    section: Option<TomlSection>,
    servers: BTreeMap<String, BTreeMap<String, RawValue>>,
    refused: BTreeMap<String, String>,
    other_tables: BTreeSet<String>,
    top_keys: BTreeSet<String>,
    header_refused: bool,
}

impl TomlReader {
    /// Reads one line: a header switches sections, a `key = value` line
    /// lands in the current server, and anything unparseable refuses the
    /// current server without interpreting it.
    fn read_line(&mut self, raw: &str) {
        let Some(line) = strip_comment(raw) else {
            self.refuse_current("holds a value Fiber cannot read across lines");
            return;
        };
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            self.read_header(header.trim());
            return;
        }
        if line.starts_with('[') {
            self.refuse_header();
            return;
        }
        let Some((key, value)) = line.split_once('=') else {
            self.refuse_current("holds a line Fiber cannot read");
            return;
        };
        let key = key.trim();
        if !is_bare_key(key) {
            self.refuse_current("holds a key Fiber cannot read");
            return;
        }
        match &self.section {
            None => {
                self.top_keys.insert(key.to_owned());
            }
            Some(TomlSection::Other(_)) => {}
            Some(TomlSection::Mcp) => {
                self.refused
                    .entry("[mcp_servers]".to_owned())
                    .or_insert_with(|| {
                        format!("`{key}` is not a server declaration under `[mcp_servers]`")
                    });
            }
            Some(TomlSection::Server(name)) => {
                let name = name.clone();
                match parse_toml_value(value.trim()) {
                    Some(field) => {
                        self.servers
                            .entry(name)
                            .or_default()
                            .insert(key.to_owned(), field);
                    }
                    None => {
                        self.refused.entry(name).or_insert_with(|| {
                            format!("`{key}` holds a value Fiber does not read")
                        });
                    }
                }
            }
        }
    }

    /// Tracks one table header: `[mcp_servers]`, one
    /// `[mcp_servers.<name>]`, or any other table. A bare dotted name is a
    /// nested table, not one server; only a quoted name may hold a dot.
    fn read_header(&mut self, header: &str) {
        if header == "mcp_servers" {
            self.section = Some(TomlSection::Mcp);
        } else if let Some(rest) = header.strip_prefix("mcp_servers.") {
            let rest = rest.trim();
            let quoted = rest.starts_with(['\"', '\'']);
            let name = unquote(rest);
            if name.is_empty() || !quoted && name.contains('.') {
                self.other_tables.insert(header.to_owned());
                self.section = Some(TomlSection::Other(header.to_owned()));
            } else {
                self.section = Some(TomlSection::Server(name));
            }
        } else if header.is_empty() {
            self.refuse_header();
        } else {
            self.other_tables.insert(header.to_owned());
            self.section = Some(TomlSection::Other(header.to_owned()));
        }
    }

    /// Refuses the current server, keeping the first reason: the line
    /// cannot be trusted, so nothing under it is interpreted.
    fn refuse_current(&mut self, reason: &str) {
        if let Some(TomlSection::Server(name)) = &self.section {
            self.refused
                .entry(name.clone())
                .or_insert_with(|| reason.to_owned());
        }
    }

    /// Records one unreadable header: the scope past it is unknown, so the
    /// file's shape past it stays out of the plan.
    fn refuse_header(&mut self) {
        if !self.header_refused {
            self.header_refused = true;
        }
        self.section = Some(TomlSection::Other(String::new()));
    }

    /// Moves every refused server and every unmapped table and top-level
    /// key into left-behind.
    fn finish(&mut self, left_behind: &mut Vec<LeftBehind>) {
        for (name, reason) in std::mem::take(&mut self.refused) {
            if name == "[mcp_servers]" {
                left_behind.push(LeftBehind {
                    item: "config.toml [mcp_servers]".into(),
                    reason,
                });
                continue;
            }
            self.servers.remove(&name);
            left_behind.push(LeftBehind {
                item: format!("config.toml [mcp_servers.{name}]"),
                reason,
            });
        }
        for table in std::mem::take(&mut self.other_tables) {
            if table.is_empty() {
                continue;
            }
            left_behind.push(LeftBehind {
                item: format!("config.toml [{table}]"),
                reason: "only `[mcp_servers.*]` tables are imported".into(),
            });
        }
        for key in std::mem::take(&mut self.top_keys) {
            left_behind.push(LeftBehind {
                item: format!("config.toml {key}"),
                reason: "top-level keys are not imported".into(),
            });
        }
        if self.header_refused {
            left_behind.push(LeftBehind {
                item: "config.toml".into(),
                reason: "holds a table header Fiber cannot read".into(),
            });
        }
    }

    /// The servers the reader kept, in name order.
    fn servers(self) -> Vec<RawServer> {
        self.servers
            .into_iter()
            .map(|(name, fields)| RawServer { name, fields })
            .collect()
    }
}

/// Strips a `#` comment outside quotes. `None` when a quote is left open:
/// the value may continue on the next line, which the subset never reads.
fn strip_comment(line: &str) -> Option<&str> {
    let mut quote = None;
    let bytes = line.as_bytes();
    let mut index = 0;
    while let Some(byte) = bytes.get(index).copied() {
        if let Some(open) = quote {
            if byte == b'\\' && open == b'"' && index + 1 < bytes.len() {
                index += 2;
                continue;
            }
            if byte == open {
                quote = None;
            }
        } else if byte == b'"' || byte == b'\'' {
            quote = Some(byte);
        } else if byte == b'#' {
            return Some(line.get(..index).unwrap_or(""));
        }
        index += 1;
    }
    if quote.is_some() {
        return None;
    }
    Some(line)
}

/// Whether `key` is one bare `key`: letters, digits, `_` and `-`, never
/// empty and never dotted.
fn is_bare_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Removes one layer of matching quotes from a table name.
fn unquote(name: &str) -> String {
    let bytes = name.as_bytes();
    if let [first, .., last] = bytes
        && first == last
        && (*first == b'"' || *first == b'\'')
    {
        return name.get(1..name.len() - 1).unwrap_or("").to_owned();
    }
    name.to_owned()
}

/// Parses one scalar TOML value: a double-quoted or literal string, a
/// boolean, a single-line list of strings, or a single-line inline table
/// of strings. `None` for anything else, including a value that runs past
/// the line.
fn parse_toml_value(text: &str) -> Option<RawValue> {
    if let Some(rest) = text.strip_prefix('"') {
        return parse_basic_string(rest).map(RawValue::Text);
    }
    if let Some(rest) = text.strip_prefix('\'') {
        return rest
            .find('\'')
            .and_then(|end| {
                (rest.get(end + 1..).unwrap_or("").trim().is_empty())
                    .then(|| rest.get(..end).unwrap_or("").to_owned())
            })
            .map(RawValue::Text);
    }
    if text == "true" {
        return Some(RawValue::Flag(true));
    }
    if text == "false" {
        return Some(RawValue::Flag(false));
    }
    if let Some(inner) = text
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return parse_toml_list(inner);
    }
    if let Some(inner) = text
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    {
        return parse_toml_table(inner);
    }
    None
}

/// Parses one double-quoted string's body: the closing quote must end the
/// value, so a string that runs past it is `None`.
fn parse_basic_string(rest: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = rest.chars();
    loop {
        match chars.next()? {
            '"' => {
                if chars.as_str().trim().is_empty() {
                    return Some(out);
                }
                return None;
            }
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                _ => return None,
            },
            char => out.push(char),
        }
    }
}

/// Splits one comma-separated list at top level, outside quotes and
/// nesting: `None` when a quote is left open or brackets nest, which a
/// scalar line never holds.
fn split_top_level(text: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut depth: usize = 0;
    let mut quote = None;
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut index = 0;
    while let Some(byte) = bytes.get(index).copied() {
        if let Some(open) = quote {
            if byte == b'\\' && open == b'"' {
                index += 2;
                continue;
            }
            if byte == open {
                quote = None;
            }
        } else if byte == b'"' || byte == b'\'' {
            quote = Some(byte);
        } else if byte == b'[' || byte == b'{' {
            depth += 1;
        } else if byte == b']' || byte == b'}' {
            depth = depth.saturating_sub(1);
        } else if byte == b',' && depth == 0 {
            parts.push(text.get(start..index).unwrap_or("").trim());
            start = index + 1;
        }
        index += 1;
    }
    if quote.is_some() {
        return None;
    }
    parts.push(text.get(start..).unwrap_or("").trim());
    Some(parts)
}

/// Parses a single-line list of strings.
fn parse_toml_list(inner: &str) -> Option<RawValue> {
    if inner.trim().is_empty() {
        return Some(RawValue::List(Vec::new()));
    }
    let mut items = Vec::new();
    for part in split_top_level(inner)? {
        items.push(parse_toml_string(part)?);
    }
    Some(RawValue::List(items))
}

/// Parses a single-line inline table of strings.
fn parse_toml_table(inner: &str) -> Option<RawValue> {
    if inner.trim().is_empty() {
        return Some(RawValue::Table(BTreeMap::new()));
    }
    let mut table = BTreeMap::new();
    for part in split_top_level(inner)? {
        let (key, value) = part.split_once('=')?;
        let key = key.trim();
        if !is_bare_key(key) {
            return None;
        }
        table.insert(key.to_owned(), parse_toml_string(value.trim())?);
    }
    Some(RawValue::Table(table))
}

/// Parses one double-quoted or literal string value.
fn parse_toml_string(text: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix('"') {
        return parse_basic_string(rest);
    }
    if let Some(rest) = text.strip_prefix('\'') {
        return rest.find('\'').and_then(|end| {
            (rest.get(end + 1..).unwrap_or("").trim().is_empty())
                .then(|| rest.get(..end).unwrap_or("").to_owned())
        });
    }
    None
}

/// Gives colliding skill, command and server names a `-2`, `-3` suffix,
/// and reports the instruction targets a suffix cannot save: an occupied
/// instruction file joins left-behind, since a suffixed fixed name is
/// never read. Entries identical to what Fiber home already holds are
/// kept under their names and skipped at write time.
fn allocate_names(
    home: &Path,
    skills: &mut [PlannedFile],
    commands: &mut [PlannedFile],
    servers: &mut [PlannedServer],
) -> Result<(), ConfigError> {
    let mut taken: BTreeSet<String> = read_dir_names(&home.join("skills"))?.into_iter().collect();
    for skill in skills.iter_mut().chain(commands.iter_mut()) {
        if skill_taken(home, &skill.name, &skill.bytes, &taken) {
            skill.name = suffixed(&taken, &skill.name);
        }
        taken.insert(skill.name.clone());
    }
    let existing: BTreeMap<String, Value> = match get_global(home, "mcp.servers")? {
        Some(Value::Object(map)) => map.into_iter().collect(),
        Some(_) => {
            return Err(ConfigError::WrongType {
                source_name: global_config_file(home).display().to_string(),
                key: "mcp.servers".into(),
                expected: "an object".into(),
            });
        }
        None => BTreeMap::new(),
    };
    let mut taken_names: BTreeSet<String> = existing.keys().cloned().collect();
    for server in servers.iter() {
        taken_names.insert(server.name.clone());
    }
    for server in servers.iter_mut() {
        match existing.get(&server.name) {
            Some(current) if *current == server.value => {}
            Some(_) => {
                let free = suffixed(&taken_names, &server.name);
                taken_names.insert(free.clone());
                server.name = free;
            }
            None => {}
        }
    }
    Ok(())
}

/// Whether the skill directory `name` is taken by anything but identical
/// bytes: an occupied directory, or a name the same run already planned.
fn skill_taken(home: &Path, name: &str, bytes: &[u8], taken: &BTreeSet<String>) -> bool {
    if !taken.contains(name) {
        return false;
    }
    read_bytes(&home.join("skills").join(name).join("SKILL.md"))
        .ok()
        .flatten()
        .is_none_or(|current| current != bytes)
}

/// Appends `-2`, `-3` until the name is free.
fn suffixed(taken: &BTreeSet<String>, name: &str) -> String {
    let mut index = 2;
    loop {
        let candidate = format!("{name}-{index}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        index += 1;
    }
}

/// Writes every planned item one file at a time through the locked atomic
/// path, then returns. Any failure aborts with an error and no receipt,
/// so a retry completes what is missing.
fn write_all(home: &Path, planned: &mut Planned) -> Result<(), ConfigError> {
    for skill in &mut planned.skills {
        write_skill(home, skill)?;
    }
    for command in &mut planned.commands {
        write_skill(home, command)?;
    }
    for instruction in &planned.instructions {
        write_instruction(home, instruction)?;
    }
    write_servers(home, &mut planned.servers)?;
    Ok(())
}

/// Writes one skill or wrapped template: an identical `SKILL.md` is
/// skipped, a changed one under a planned name takes a further suffix on
/// the spot, and the plan's name follows it.
fn write_skill(home: &Path, skill: &mut PlannedFile) -> Result<(), ConfigError> {
    let file = home.join("skills").join(&skill.name).join("SKILL.md");
    if let Some(current) = read_bytes(&file)? {
        if current == skill.bytes {
            return Ok(());
        }
        let taken: BTreeSet<String> = read_dir_names(&home.join("skills"))?.into_iter().collect();
        skill.name = suffixed(&taken, &skill.name);
        let file = home.join("skills").join(&skill.name).join("SKILL.md");
        write_atomic(&file, &skill.bytes, 0o666)?;
        return Ok(());
    }
    write_atomic(&file, &skill.bytes, 0o666)?;
    Ok(())
}

/// Writes one instruction file: an identical target is skipped, a target
/// that changed under the plan aborts rather than overwriting a fixed
/// name nobody reads under a suffix.
fn write_instruction(home: &Path, instruction: &PlannedFile) -> Result<(), ConfigError> {
    let file = home.join(&instruction.name);
    if let Some(current) = read_bytes(&file)? {
        if current == instruction.bytes {
            return Ok(());
        }
        return Err(ConfigError::Io {
            file,
            source: std::io::Error::other("changed under the import"),
        });
    }
    write_atomic(&file, &instruction.bytes, 0o666)?;
    Ok(())
}

/// Writes every planned server under `mcp.servers` in one locked write:
/// entries identical to the file's are skipped, and the write is checked
/// against "Keys" before the lock, as `insert_entry` checks it.
fn write_servers(home: &Path, servers: &mut [PlannedServer]) -> Result<(), ConfigError> {
    if servers.is_empty() {
        return Ok(());
    }
    let file = global_config_file(home);
    let mut candidate = Value::Object(Map::new());
    for server in servers.iter() {
        crate::path::set(
            &mut candidate,
            &["mcp".to_owned(), "servers".to_owned(), server.name.clone()],
            server.value.clone(),
        );
    }
    let mut notices = Vec::new();
    if let Value::Object(map) = candidate {
        keys::check(map, &Source::Global(file.clone()), &mut notices)?;
    }
    if !notices.is_empty() {
        return Err(ConfigError::Refused {
            key: "mcp.servers".into(),
            file,
            why: "this Fiber does not know one entry",
        });
    }
    let _lock = locked(&file)?;
    let mut root = read(&file)?.unwrap_or_else(|| Value::Object(Map::new()));
    let mut table = match root.get("mcp").and_then(|mcp| mcp.get("servers")) {
        None => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => {
            return Err(ConfigError::WrongType {
                source_name: file.display().to_string(),
                key: "mcp.servers".into(),
                expected: "an object".into(),
            });
        }
    };
    let mut taken: BTreeSet<String> = table.keys().cloned().collect();
    for name in servers.iter().map(|server| server.name.clone()) {
        taken.insert(name);
    }
    for server in servers.iter_mut() {
        match table.get(&server.name) {
            Some(current) if *current == server.value => {}
            Some(_) => {
                let free = suffixed(&taken, &server.name);
                taken.insert(free.clone());
                table.insert(free.clone(), server.value.clone());
                server.name = free;
            }
            None => {
                table.insert(server.name.clone(), server.value.clone());
            }
        }
    }
    crate::path::set(
        &mut root,
        &["mcp".to_owned(), "servers".to_owned()],
        Value::Object(table),
    );
    let mut text = serde_json::to_string_pretty(&root).map_err(|source| ConfigError::Io {
        file: file.clone(),
        source: source.into(),
    })?;
    text.push('\n');
    write_atomic(&file, text.as_bytes(), 0o666)?;
    Ok(())
}

/// Writes the receipt last, and only on full success: every item above
/// landed, so the next run short-circuits.
fn write_receipt(
    file: &Path,
    source: ImportSource,
    clock: &dyn Clock,
    report: &ImportReport,
) -> Result<(), ConfigError> {
    let imported_at = clock
        .wall()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .unwrap_or(0);
    let receipt = Receipt {
        source: source.name().to_owned(),
        imported_at,
        imported: report.imported.clone(),
        left_behind: report.left_behind.clone(),
    };
    let mut text = serde_json::to_string_pretty(&receipt).map_err(|source| ConfigError::Io {
        file: file.to_path_buf(),
        source: source.into(),
    })?;
    text.push('\n');
    write_atomic(file, text.as_bytes(), 0o666)?;
    Ok(())
}

/// Resolves `~/.claude` or `~/.codex` under `home_env`: test code passes
/// explicit paths, so only the CLI reads the environment.
pub fn source_home(home_env: &Path, source: ImportSource) -> PathBuf {
    home_env.join(source.dir_name())
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
