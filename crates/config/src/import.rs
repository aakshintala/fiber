//! One-time import of another harness's configuration
//! (`docs/configuration.md`, "Importing"): reads `~/.claude` (Claude Code)
//! or `~/.codex` (Codex), copies what maps into Fiber home, names the rest
//! in a left-behind list, and writes a receipt so the next run for the same
//! source succeeds without reading the source again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use contract::clock::Clock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::ConfigError;
use crate::home::{plain, read, read_bytes};
use crate::keys;
use crate::layout::global_config_file;
use crate::write::atomic::{locked, write_atomic};
use crate::{Source, valid_server_name};
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
    fn read_servers(
        &self,
        source_home: &Path,
        budget: &mut Budget,
    ) -> (Vec<RawServer>, Vec<LeftBehind>) {
        match self {
            Self::ClaudeCode => read_claude_servers(&source_home.join("settings.json"), budget),
            Self::Codex => read_codex_servers(&source_home.join("config.toml"), budget),
        }
    }
}

/// Tells the import which source skill directories discovery would already
/// load from another personal source (`docs/configuration.md`,
/// "Importing"): the check maps each given source directory to the
/// `SKILL.md` path discovery logged, and leaves the rest absent. `main`
/// builds the real check over discovery's own code; the import never
/// parses a header itself.
pub trait PersonalSourceCheck: Send + Sync {
    /// Maps each directory in `dirs` discovery would already read to the
    /// `SKILL.md` path it logged; absent entries are not already read.
    fn check(&self, dirs: &[PathBuf]) -> BTreeMap<PathBuf, PathBuf>;
}

impl<F> PersonalSourceCheck for F
where
    F: Fn(&[PathBuf]) -> BTreeMap<PathBuf, PathBuf> + Send + Sync,
{
    fn check(&self, dirs: &[PathBuf]) -> BTreeMap<PathBuf, PathBuf> {
        self(dirs)
    }
}

/// The finds-nothing check: no directory is already read. The doors socket
/// drive defaults to it when no session wired a real one, and tests that
/// need no check pass it.
pub fn no_personal_sources(_dirs: &[PathBuf]) -> BTreeMap<PathBuf, PathBuf> {
    BTreeMap::new()
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

/// Whether the first-run import offer shows: true when neither source
/// has a completed receipt under `home`, so the offer fires at most
/// until the first import lands.
pub fn import_pending(home: &Path) -> bool {
    !home.join(ImportSource::ClaudeCode.receipt_name()).is_file()
        && !home.join(ImportSource::Codex.receipt_name()).is_file()
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
    check: &dyn PersonalSourceCheck,
    clock: &dyn Clock,
) -> Result<ImportReport, ConfigError> {
    let receipt_file = home.join(source.receipt_name());
    if let Some(report) = read_receipt(&receipt_file, source)? {
        return Ok(report);
    }
    let mut planned = classify(home, source_home, source, check)?;
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
fn classify(
    home: &Path,
    source_home: &Path,
    source: ImportSource,
    check: &dyn PersonalSourceCheck,
) -> Result<Planned, ConfigError> {
    let mut left_behind = Vec::new();
    let mut skills = Vec::new();
    let mut commands = Vec::new();
    // One budget per source: every source file read charges what it
    // keeps, so the worst case retained is one cap plus one in-flight file.
    let mut budget = Budget::default();
    if plain(source_home, true)? {
        read_skills(
            source_home,
            &mut skills,
            &mut left_behind,
            &mut budget,
            check,
        )?;
        read_flat(
            source,
            source_home,
            &mut commands,
            &mut left_behind,
            &mut budget,
        )?;
        top_level_left_behind(source, source_home, &mut left_behind)?;
    }
    let mut instructions = Vec::new();
    read_instruction(
        source,
        source_home,
        home,
        &mut instructions,
        &mut left_behind,
        &mut budget,
    )?;
    let (raw_servers, mut server_left) = source.read_servers(source_home, &mut budget);
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
    budget: &mut Budget,
    check: &dyn PersonalSourceCheck,
) -> Result<(), ConfigError> {
    let dir = source_home.join("skills");
    let names = read_source_dir(&dir, "skills", left_behind)?;
    // The already-read consult runs once for every kept entry directory,
    // before any read: a hit joins left-behind with its reason only, with
    // no copy-path read and no budget charge. Past-512 entries were never
    // enumerated, so they stay unchecked and keep the overflow accounting
    // above.
    let entries: Vec<PathBuf> = names.iter().map(|name| dir.join(name)).collect();
    let already = check.check(&entries);
    for name in &names {
        let entry = dir.join(name);
        if let Some(logged) = already.get(&entry) {
            left_behind.push(LeftBehind {
                item: format!("skills/{name}/SKILL.md"),
                reason: format!("already read from {}", logged.display()),
            });
            continue;
        }
        if !entry.is_dir() {
            left_behind.push(LeftBehind {
                item: format!("skills/{name}"),
                reason: "is not a directory".into(),
            });
            continue;
        }
        let item = format!("skills/{name}/SKILL.md");
        match read_source(&entry.join("SKILL.md"), &item, budget, left_behind)? {
            SourceFile::Read(bytes) => skills.push(PlannedFile {
                name: name.clone(),
                bytes,
            }),
            SourceFile::Missing => left_behind.push(LeftBehind {
                item,
                reason: "holds no SKILL.md".into(),
            }),
            // An over-cap file already reported itself; nothing more to say.
            SourceFile::Skipped => {}
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
    budget: &mut Budget,
) -> Result<(), ConfigError> {
    let flat = source.flat_dir();
    let dir = source_home.join(flat);
    for name in read_source_dir(&dir, flat, left_behind)? {
        let path = dir.join(&name);
        if !path.is_file() || !name.ends_with(".md") {
            left_behind.push(LeftBehind {
                item: format!("{flat}/{name}"),
                reason: "is not a command file".into(),
            });
            continue;
        }
        let item = format!("{flat}/{name}");
        let bytes = match read_source(&path, &item, budget, left_behind)? {
            SourceFile::Read(bytes) => bytes,
            SourceFile::Missing => {
                left_behind.push(LeftBehind {
                    item,
                    reason: "cannot be read".into(),
                });
                continue;
            }
            // An over-cap file already reported itself; nothing more to say.
            SourceFile::Skipped => continue,
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
            "---\nname: {stem}\ndescription: {}\ndisable-model-invocation: true\n---\n\n",
            yaml_quoted(&description),
        );
        wrapped.push_str(&String::from_utf8_lossy(&bytes));
        commands.push(PlannedFile {
            name: stem,
            bytes: wrapped.into_bytes(),
        });
    }
    Ok(())
}

/// Renders one single-line description as a double-quoted YAML scalar,
/// so a first line starting with a hash cannot read as a comment and a
/// quote or backslash cannot break the header: Fiber cuts a hash comment
/// to an empty description and leaves the skill out, so the imported
/// command could not be run by name.
fn yaml_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
    budget: &mut Budget,
) -> Result<(), ConfigError> {
    let name = source.instruction_name();
    let bytes = match read_source(&source_home.join(name), name, budget, left_behind)? {
        SourceFile::Read(bytes) => bytes,
        // Missing reads as no instruction; an over-cap file already
        // reported itself.
        SourceFile::Missing | SourceFile::Skipped => return Ok(()),
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
    for name in read_source_dir(source_home, source.dir_name(), left_behind)? {
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

/// A source directory read keeps this many entries: the next one proves
/// there are more without counting them, so enumeration itself is bounded.
const MAX_DIR_KEPT: usize = 512;
/// A source file over this size is left unread: real harness homes hold
/// KB-scale files, so the cap never bites honest setups.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// A source's reads stop past this running total: the excess joins
/// left-behind with reasons, so nothing is silently dropped.
const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;

/// One source's running byte total: every source file read charges what
/// it keeps, so the worst case retained is one cap plus one in-flight file.
#[derive(Default)]
struct Budget {
    used: u64,
}

/// One source file's read: present bytes, a miss, or an over-cap skip the
/// caller says nothing more about, already reported in left-behind.
#[derive(Debug)]
enum SourceFile {
    Read(Vec<u8>),
    Missing,
    Skipped,
}

/// Reads a source file under both caps: missing reads as `Missing`; a file
/// over 1 MiB, or one past the source's running 8 MiB total, is left unread
/// and reported in left-behind under `item`, charging nothing. The metadata
/// sizes above are advisory only: the file can grow between them and the
/// read, so the read itself takes at most one byte past the tighter cap,
/// and an over-cap read joins left-behind under the same reasons. Read
/// failures keep their `ConfigError` for the caller, which refuses or
/// aborts as it does today. A source file vanishing mid-import reads as
/// missing only when the open misses it.
fn read_source(
    file: &Path,
    item: &str,
    budget: &mut Budget,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<SourceFile, ConfigError> {
    let mut handle = match fs::File::open(file) {
        Ok(handle) => handle,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(SourceFile::Missing),
        Err(source) => {
            return Err(ConfigError::Io {
                file: file.to_path_buf(),
                source,
            });
        }
    };
    let len = handle
        .metadata()
        .map(|meta| meta.len())
        .map_err(|source| ConfigError::Io {
            file: file.to_path_buf(),
            source,
        })?;
    if len > MAX_FILE_BYTES {
        left_behind.push(LeftBehind {
            item: item.to_owned(),
            reason: "larger than 1 MiB, left unread".into(),
        });
        return Ok(SourceFile::Skipped);
    }
    if budget.used.saturating_add(len) > MAX_SOURCE_BYTES {
        left_behind.push(LeftBehind {
            item: item.to_owned(),
            reason: "over the 8 MiB read budget, left unread".into(),
        });
        return Ok(SourceFile::Skipped);
    }
    // One byte past the tighter cap proves an over-cap read without
    // keeping it: the metadata size above can predate a concurrent
    // append, so this allocation stays bounded either way.
    let remaining = MAX_SOURCE_BYTES.saturating_sub(budget.used);
    let allowance = MAX_FILE_BYTES.min(remaining);
    let mut capped = handle.by_ref().take(allowance.saturating_add(1));
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut capped, &mut bytes).map_err(|source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    })?;
    let kept = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if kept > allowance {
        // Past the file cap, the read proves the file itself is over
        // 1 MiB; anything shorter only proves the 8 MiB total ran out
        // first, since the cap stopped the read either way.
        let reason = if kept > MAX_FILE_BYTES {
            "larger than 1 MiB, left unread"
        } else {
            "over the 8 MiB read budget, left unread"
        };
        left_behind.push(LeftBehind {
            item: item.to_owned(),
            reason: reason.into(),
        });
        return Ok(SourceFile::Skipped);
    }
    budget.used = budget.used.saturating_add(kept);
    Ok(SourceFile::Read(bytes))
}

/// One source directory's entry names, bounded: at most 513 entries are
/// read, the first 512 kept in sorted order, and a 513th reports `more
/// than 512 entries, keeping the first 512` under `item`. A missing
/// directory holds nothing, never an error.
fn read_source_dir(
    dir: &Path,
    item: &str,
    left_behind: &mut Vec<LeftBehind>,
) -> Result<Vec<String>, ConfigError> {
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
    let mut seen = 0;
    for entry in entries.take(MAX_DIR_KEPT + 1) {
        seen += 1;
        let entry = entry.map_err(|source| ConfigError::Io {
            file: dir.to_path_buf(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if crate::home::one_file_name(&name) {
            names.push(name);
        }
    }
    // A 513th entry proves there are more: the kept 512 stay the report's
    // first 512 in sorted order, and the rest stay unread, named only by
    // this entry.
    names.truncate(MAX_DIR_KEPT);
    names.sort();
    if seen > MAX_DIR_KEPT {
        left_behind.push(LeftBehind {
            item: item.to_owned(),
            reason: "more than 512 entries, keeping the first 512".into(),
        });
    }
    Ok(names)
}

/// One MCP server as the source states it, before validation.
struct RawServer {
    name: String,
    fields: BTreeMap<String, RawValue>,
}

/// One source field's value. `Unsupported` is a value outside the narrow
/// subset (a number, `null`, a nested table): it is never interpreted, and
/// the server holding it is refused. It is a variant rather than a string
/// so a legitimate value cannot spell it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RawValue {
    Text(String),
    Flag(bool),
    List(Vec<String>),
    Table(BTreeMap<String, String>),
    Unsupported,
}

/// Reads Claude Code's `settings.json` `mcpServers` object.
fn read_claude_servers(file: &Path, budget: &mut Budget) -> (Vec<RawServer>, Vec<LeftBehind>) {
    let mut left_behind = Vec::new();
    let bytes = match read_source(file, "settings.json", budget, &mut left_behind) {
        Ok(SourceFile::Read(bytes)) => bytes,
        // Missing reads as no servers; an over-cap file already
        // reported itself.
        Ok(SourceFile::Missing) | Ok(SourceFile::Skipped) => {
            return (Vec::new(), left_behind);
        }
        Err(e) => {
            left_behind.push(LeftBehind {
                item: "settings.json".into(),
                reason: format!("cannot be read: {e}"),
            });
            return (Vec::new(), left_behind);
        }
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
                    server.fields.insert(key.clone(), RawValue::Unsupported);
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
        Some(_) => {
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
        Some(_) => {
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
            RawValue::Text(_) | RawValue::List(_) | RawValue::Table(_) | RawValue::Unsupported => {
                refuse("`required` is not true or false".into());
                return;
            }
        }
    }
    servers.push(PlannedServer {
        name: raw.name,
        value: Value::Object(entry),
    });
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
fn read_codex_servers(file: &Path, budget: &mut Budget) -> (Vec<RawServer>, Vec<LeftBehind>) {
    let mut left_behind = Vec::new();
    let bytes = match read_source(file, "config.toml", budget, &mut left_behind) {
        Ok(SourceFile::Read(bytes)) => bytes,
        // Missing reads as no servers; an over-cap file already
        // reported itself.
        Ok(SourceFile::Missing) | Ok(SourceFile::Skipped) => {
            return (Vec::new(), left_behind);
        }
        Err(e) => {
            left_behind.push(LeftBehind {
                item: "config.toml".into(),
                reason: format!("cannot be read: {e}"),
            });
            return (Vec::new(), left_behind);
        }
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
/// A backslash inside a `"` string escapes the next byte, including a
/// trailing backslash, which ends the scan with the quote still open:
/// `None`, exactly as escaping nothing does. The scan walks the bytes
/// with an iterator, so no index arithmetic can stick a mutant in a loop.
fn strip_comment(line: &str) -> Option<&str> {
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in line.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(open) = quote {
            if byte == b'\\' && open == b'"' {
                escaped = true;
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

/// Splits one comma-separated list at top level, outside quotes: `None`
/// when a quote is left open. Only quotes are tracked, never bracket
/// depth: no string part can hold an unquoted bracket, so a nested
/// bracket fails the value's parse either way, and every input that
/// would reach depth greater than zero returns `None` with or without it.
/// A backslash inside a `"` string escapes the next byte. The scan walks
/// the bytes with an iterator, so no index arithmetic can stick a mutant
/// in a loop.
fn split_top_level(text: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(open) = quote {
            if byte == b'\\' && open == b'"' {
                escaped = true;
                continue;
            }
            if byte == open {
                quote = None;
            }
        } else if byte == b'"' || byte == b'\'' {
            quote = Some(byte);
        } else if byte == b',' {
            parts.push(text.get(start..index).unwrap_or("").trim());
            start = index + 1;
        }
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

/// Appends `-2`, `-3` until the name is free. At most `taken.len()`
/// candidates can be occupied, so the first `taken.len() + 1` suffixes
/// hold a free name and the walk is finite: a flipped exit returns a
/// wrong name instead of hanging. The `+ 2` bound is pinned by the
/// empty-taken unit below, where `* 2` would empty the range and hit
/// the unreachable.
fn suffixed(taken: &BTreeSet<String>, name: &str) -> String {
    for index in 2..=(taken.len() + 2) {
        let candidate = format!("{name}-{index}");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("one of the first taken.len() + 1 suffixes is free");
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
mod tests {
    //! Tests for the one-time import: mapping rows, the narrow reader, the
    //! never-overwrite rule, secrets and the receipt.

    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "a failure is the test's"
    )]

    use std::fs;
    use std::path::{Path, PathBuf};

    use fakes::TempDir;
    use serde_json::{Value, json};

    use super::*;
    use crate::write::atomic::{Stage, fail_at, fail_nth};

    /// One Fiber home beside one source home.
    struct Fixture {
        tmp: TempDir,
        clock: std::sync::Arc<fakes::clock::FakeClock>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                tmp: TempDir::new("fiber-import"),
                clock: fakes::clock::FakeClock::new(),
            }
        }

        /// Writes `content` to `rel` under the source home, creating parents.
        fn write_source(&self, rel: &str, content: &str) {
            self.tmp.write(&format!("source/{rel}"), content);
        }

        fn home(&self) -> PathBuf {
            self.tmp.path().join("home")
        }

        fn source(&self) -> PathBuf {
            self.tmp.path().join("source")
        }

        /// Runs the import for `source` against this fixture.
        fn run(&self, source: ImportSource) -> Result<ImportReport, ConfigError> {
            import_source(
                &self.home(),
                &self.source(),
                source,
                &no_personal_sources,
                self.clock.as_ref(),
            )
        }

        /// Every file under Fiber home holding `needle`.
        fn files_holding(&self, needle: &str) -> Vec<PathBuf> {
            let mut found = Vec::new();
            collect_holding(&self.home(), needle, &mut found);
            found.sort();
            found
        }
    }

    fn collect_holding(dir: &Path, needle: &str, found: &mut Vec<PathBuf>) {
        let entries = fs::read_dir(dir).unwrap();
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() && !path.is_symlink() {
                collect_holding(&path, needle, found);
            } else if path.is_file()
                && fs::read(&path)
                    .map(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
                    .unwrap_or(false)
            {
                found.push(path);
            }
        }
    }

    /// A Claude Code source mirroring the probed layout.
    fn claude_source(source: &Path) {
        fs::create_dir_all(source.join("skills/review")).unwrap();
        fs::write(
            source.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n",
        )
        .unwrap();
        fs::create_dir_all(source.join("commands")).unwrap();
        fs::write(
            source.join("commands/greet.md"),
            "\nHello there.\n\nUsage: greet.\n",
        )
        .unwrap();
        fs::write(source.join("CLAUDE.md"), "Be kind.\n").unwrap();
        fs::write(
            source.join("settings.json"),
            json!({
                "model": "opus",
                "mcpServers": {
                    "notes": {
                        "command": "notes-server",
                        "args": ["--fast"],
                        "env": {"PORT": "8080"},
                        "required": true
                    },
                    "remote": {"url": "https://mcp.example.com/x"}
                }
            })
            .to_string(),
        )
        .unwrap();
        fs::create_dir_all(source.join("agents")).unwrap();
        fs::write(source.join("agents/worker.md"), "worker\n").unwrap();
        fs::write(source.join("auth.json"), "{\"token\": \"x\"}\n").unwrap();
    }

    /// A Codex source mirroring the probed layout.
    fn codex_source(source: &Path) {
        fs::create_dir_all(source.join("skills/lint")).unwrap();
        fs::write(
            source.join("skills/lint/SKILL.md"),
            "---\nname: lint\ndescription: Lints.\n---\n\nLint body.\n",
        )
        .unwrap();
        fs::create_dir_all(source.join("prompts")).unwrap();
        fs::write(source.join("prompts/plan.md"), "Plan it.\n").unwrap();
        fs::write(source.join("AGENTS.md"), "Be thorough.\n").unwrap();
        fs::write(
            source.join("config.toml"),
            "screen_reader_detection_done = true\n\
         \n\
         [features]\n\
         fancy = true\n\
         \n\
         [mcp_servers.alpha]\n\
         command = \"alpha-server\"\n\
         args = [\"--x\", \"y\"]\n\
         env = { PORT = \"1\" }\n\
         required = true\n\
         \n\
         [mcp_servers.beta]\n\
         url = \"https://beta.example.com/mcp\"\n",
        )
        .unwrap();
        fs::write(source.join("auth.json"), "{\"token\": \"x\"}\n").unwrap();
    }

    /// The left-behind reason for `item`, or "" when it is not left behind.
    fn reason_for(report: &ImportReport, item: &str) -> String {
        report
            .left_behind
            .iter()
            .find(|left| left.item == item)
            .map(|left| left.reason.clone())
            .unwrap_or_default()
    }

    #[test]
    fn claude_import_lands_every_mapped_item() {
        let fixture = Fixture::new();
        claude_source(&fixture.source());
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(report.imported.skills, ["review"]);
        assert_eq!(report.imported.commands, ["greet"]);
        assert_eq!(report.imported.instructions, ["CLAUDE.md"]);
        assert_eq!(report.imported.mcp_servers, ["notes", "remote"]);
        assert_eq!(
            fs::read(fixture.home().join("skills/review/SKILL.md")).unwrap(),
            b"---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n"
        );
        assert_eq!(
            fs::read(fixture.home().join("CLAUDE.md")).unwrap(),
            b"Be kind.\n"
        );
        let servers: Value =
            serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            servers.pointer("/mcp/servers/notes").unwrap(),
            &json!({
                "command": "notes-server",
                "args": ["--fast"],
                "env": {"PORT": "8080"},
                "required": true
            })
        );
        assert_eq!(
            servers.pointer("/mcp/servers/remote").unwrap(),
            &json!({"url": "https://mcp.example.com/x"})
        );
        assert!(!reason_for(&report, "agents").is_empty());
        assert!(!reason_for(&report, "auth.json").is_empty());
        assert!(reason_for(&report, "auth.json").contains("credentials"));
        assert!(!reason_for(&report, "settings.json model").is_empty());
        let receipt: Value = serde_json::from_str(
            &fs::read_to_string(fixture.home().join("import-claude-code.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["source"], "claude-code");
        assert_eq!(receipt["imported_at"], 1_700_000_000);
        assert_eq!(receipt["imported"]["skills"], json!(["review"]));
    }

    #[test]
    fn wrapped_commands_carry_a_synthesized_header() {
        let fixture = Fixture::new();
        claude_source(&fixture.source());
        fixture.run(ImportSource::ClaudeCode).unwrap();
        // The name comes from the file stem, the description from the first
        // non-empty line, and model invocation is disabled.
        assert_eq!(
        fs::read(fixture.home().join("skills/greet/SKILL.md")).unwrap(),
        b"---\nname: greet\ndescription: \"Hello there.\"\ndisable-model-invocation: true\n---\n\n\nHello there.\n\nUsage: greet.\n"
    );
    }

    #[test]
    fn hash_quote_and_backslash_descriptions_are_quoted_not_comments() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.source().join("commands")).unwrap();
        fs::write(fixture.source().join("commands/hash.md"), "# Hello.\n").unwrap();
        fs::write(
            fixture.source().join("commands/quote.md"),
            "Say \"hi\" loudly.\n",
        )
        .unwrap();
        fs::write(fixture.source().join("commands/slash.md"), "A \\ path.\n").unwrap();
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(report.imported.commands, ["hash", "quote", "slash"]);
        // Unquoted, `# Hello.` would read as a comment: an empty
        // description, and the skill left out (`loop::skill_header`).
        assert_eq!(
            fs::read(fixture.home().join("skills/hash/SKILL.md")).unwrap(),
            b"---\nname: hash\ndescription: \"# Hello.\"\ndisable-model-invocation: true\n---\n\n# Hello.\n"
        );
        assert_eq!(
            fs::read(fixture.home().join("skills/quote/SKILL.md")).unwrap(),
            b"---\nname: quote\ndescription: \"Say \\\"hi\\\" loudly.\"\ndisable-model-invocation: true\n---\n\nSay \"hi\" loudly.\n"
        );
        assert_eq!(
            fs::read(fixture.home().join("skills/slash/SKILL.md")).unwrap(),
            b"---\nname: slash\ndescription: \"A \\\\ path.\"\ndisable-model-invocation: true\n---\n\nA \\ path.\n"
        );
    }

    #[test]
    fn a_command_without_text_has_no_description_to_wrap() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.source().join("commands")).unwrap();
        fs::write(fixture.source().join("commands/empty.md"), "\n  \n").unwrap();
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert!(report.imported.commands.is_empty());
        assert!(!reason_for(&report, "commands/empty.md").is_empty());
        assert!(!fixture.home().join("skills/empty").exists());
    }

    #[test]
    fn codex_import_lands_every_mapped_item() {
        let fixture = Fixture::new();
        codex_source(&fixture.source());
        let report = fixture.run(ImportSource::Codex).unwrap();
        assert_eq!(report.imported.skills, ["lint"]);
        assert_eq!(report.imported.commands, ["plan"]);
        assert_eq!(report.imported.instructions, ["AGENTS.md"]);
        assert_eq!(report.imported.mcp_servers, ["alpha", "beta"]);
        assert_eq!(
        fs::read(fixture.home().join("skills/plan/SKILL.md")).unwrap(),
        b"---\nname: plan\ndescription: \"Plan it.\"\ndisable-model-invocation: true\n---\n\nPlan it.\n"
    );
        let servers: Value =
            serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            servers.pointer("/mcp/servers/alpha").unwrap(),
            &json!({
                "command": "alpha-server",
                "args": ["--x", "y"],
                "env": {"PORT": "1"},
                "required": true
            })
        );
        assert!(!reason_for(&report, "config.toml [features]").is_empty());
        assert!(!reason_for(&report, "config.toml screen_reader_detection_done").is_empty());
        assert!(!reason_for(&report, "auth.json").is_empty());
        let receipt: Value = serde_json::from_str(
            &fs::read_to_string(fixture.home().join("import-codex.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["source"], "codex");
        assert_eq!(receipt["imported"]["mcp_servers"], json!(["alpha", "beta"]));
    }

    #[test]
    fn missing_source_paths_are_nothing_to_import() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.source()).unwrap();
        fs::write(fixture.source().join("CLAUDE.md"), "Hi.\n").unwrap();
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(report.imported.instructions, ["CLAUDE.md"]);
        assert!(report.imported.skills.is_empty());
        assert!(report.imported.commands.is_empty());
        assert!(report.imported.mcp_servers.is_empty());
        assert!(report.left_behind.is_empty());
    }

    #[test]
    fn a_missing_source_home_imports_nothing() {
        let fixture = Fixture::new();
        let report = fixture.run(ImportSource::Codex).unwrap();
        assert_eq!(report, ImportReport::default());
        assert!(fixture.home().join("import-codex.json").exists());
    }

    #[test]
    fn a_skill_entry_without_a_skill_file_is_left_behind() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.source().join("skills/bare")).unwrap();
        fs::write(fixture.source().join("skills/plain.md"), "x\n").unwrap();
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert!(report.imported.skills.is_empty());
        assert_eq!(
            reason_for(&report, "skills/bare/SKILL.md"),
            "holds no SKILL.md"
        );
        assert_eq!(reason_for(&report, "skills/plain.md"), "is not a directory");
    }

    #[test]
    fn a_non_command_file_is_left_behind() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.source().join("commands/nested")).unwrap();
        fs::write(fixture.source().join("commands/notes.txt"), "x\n").unwrap();
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert!(report.imported.commands.is_empty());
        assert!(!reason_for(&report, "commands/nested").is_empty());
        assert!(!reason_for(&report, "commands/notes.txt").is_empty());
    }

    #[test]
    fn colliding_names_take_a_suffix_and_occupied_instructions_stay() {
        let fixture = Fixture::new();
        claude_source(&fixture.source());
        fs::create_dir_all(fixture.home().join("skills/review")).unwrap();
        fs::write(
            fixture.home().join("skills/review/SKILL.md"),
            "someone else's skill\n",
        )
        .unwrap();
        fs::write(
            fixture.home().join("config.json"),
            json!({"mcp": {"servers": {"notes": {"command": "other"}}}}).to_string(),
        )
        .unwrap();
        fs::write(fixture.home().join("CLAUDE.md"), "mine\n").unwrap();
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(report.imported.skills, ["review-2"]);
        assert!(report.imported.mcp_servers.contains(&"notes-2".to_owned()));
        assert!(report.imported.instructions.is_empty());
        assert_eq!(
            reason_for(&report, "CLAUDE.md"),
            "CLAUDE.md already exists in Fiber home"
        );
        assert_eq!(
            fs::read(fixture.home().join("skills/review-2/SKILL.md")).unwrap(),
            b"---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n"
        );
        // The occupied files stand as they were.
        assert_eq!(
            fs::read(fixture.home().join("skills/review/SKILL.md")).unwrap(),
            b"someone else's skill\n"
        );
        assert_eq!(
            fs::read(fixture.home().join("CLAUDE.md")).unwrap(),
            b"mine\n"
        );
    }

    #[test]
    fn suffixes_count_past_two() {
        let fixture = Fixture::new();
        claude_source(&fixture.source());
        for taken in ["review", "review-2"] {
            fs::create_dir_all(fixture.home().join(format!("skills/{taken}"))).unwrap();
            fs::write(
                fixture.home().join(format!("skills/{taken}/SKILL.md")),
                "other\n",
            )
            .unwrap();
        }
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(report.imported.skills, ["review-3"]);
    }

    #[test]
    fn identical_bytes_are_skipped_without_error() {
        let fixture = Fixture::new();
        claude_source(&fixture.source());
        fs::create_dir_all(fixture.home().join("skills/review")).unwrap();
        fs::write(
            fixture.home().join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n",
        )
        .unwrap();
        fs::write(fixture.home().join("CLAUDE.md"), "Be kind.\n").unwrap();
        fs::write(
            fixture.home().join("config.json"),
            json!({"mcp": {"servers": {
                "notes": {
                    "command": "notes-server",
                    "args": ["--fast"],
                    "env": {"PORT": "8080"},
                    "required": true
                },
                "remote": {"url": "https://mcp.example.com/x"}
            }}})
            .to_string(),
        )
        .unwrap();
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(report.imported.skills, ["review"]);
        assert_eq!(report.imported.instructions, ["CLAUDE.md"]);
        assert_eq!(report.imported.mcp_servers, ["notes", "remote"]);
        assert!(!fixture.home().join("skills/review-2").exists());
    }

    #[test]
    fn a_completed_receipt_short_circuits_without_reading_the_source() {
        let fixture = Fixture::new();
        claude_source(&fixture.source());
        let first = fixture.run(ImportSource::ClaudeCode).unwrap();
        fs::remove_dir_all(fixture.source()).unwrap();
        let second = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_receipt_for_another_source_runs_again() {
        let fixture = Fixture::new();
        claude_source(&fixture.source());
        fixture.tmp.write(
            "home/import-claude-code.json",
            &json!({"source": "codex", "imported_at": 1}).to_string(),
        );
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(report.imported.skills, ["review"]);
    }

    #[test]
    fn a_failed_run_writes_no_receipt_and_its_retry_completes() {
        let fixture = Fixture::new();
        claude_source(&fixture.source());
        // The first skill file writes, the instruction write fails.
        fail_nth(2, Stage::BeforeRename);
        let failed = fixture.run(ImportSource::ClaudeCode);
        fail_at(None);
        assert!(failed.is_err());
        assert!(!fixture.home().join("import-claude-code.json").exists());
        assert!(fixture.home().join("skills/review/SKILL.md").exists());
        assert!(!fixture.home().join("CLAUDE.md").exists());
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(report.imported.skills, ["review"]);
        assert_eq!(report.imported.instructions, ["CLAUDE.md"]);
        assert!(fixture.home().join("CLAUDE.md").exists());
        assert!(fixture.home().join("import-claude-code.json").exists());
    }

    #[test]
    fn settings_json_that_does_not_parse_leaves_its_servers_behind() {
        let fixture = Fixture::new();
        claude_source(&fixture.source());
        fs::write(fixture.source().join("settings.json"), "{oops").unwrap();
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert!(report.imported.mcp_servers.is_empty());
        assert_eq!(
            reason_for(&report, "settings.json"),
            "does not parse as JSON"
        );
        // Everything else still lands, and the receipt records the run.
        assert_eq!(report.imported.skills, ["review"]);
        assert!(fixture.home().join("import-claude-code.json").exists());
    }

    #[test]
    fn settings_json_shapes_outside_the_map_are_left_behind() {
        for (body, item) in [
            ("[]", "settings.json"),
            ("{\"mcpServers\": []}", "settings.json mcpServers"),
            ("{\"mcpServers\": {\"bad\": []}}", "mcpServers bad"),
        ] {
            let fixture = Fixture::new();
            fixture.write_source("settings.json", body);
            let report = fixture.run(ImportSource::ClaudeCode).unwrap();
            assert!(report.imported.mcp_servers.is_empty(), "{body}");
            assert!(!reason_for(&report, item).is_empty(), "{body}");
            assert!(fixture.home().join("import-claude-code.json").exists());
        }
    }

    #[test]
    fn malformed_servers_are_refused_before_anything_is_written() {
        let servers = json!({
            "both": {"command": "a", "url": "https://x.example/"},
            "neither": {"args": []},
            "empty": {"command": ""},
            "numbered": {"command": 3},
            "urlled": {"url": 3},
            "usered": {"url": "https://user:pass@host.example/"},
            "listed": {"command": "a", "args": ["ok", 3]},
            "enved": {"command": "a", "env": "PORT=1"},
            "keyed": {"command": "a", "env": {"not a key": "1"}},
            "objected": {"command": "a", "env": {"PORT": {"secret": "mcp.a.PORT"}}},
            "tokened": {"command": "a", "env": {"API_TOKEN": "sk-canary-9"}},
            "keyed_word": {"command": "a", "env": {"KEY": "sk-canary-8"}},
            "monkeyed": {"command": "a", "env": {"MONKEY": "bananas"}},
            "required": {"command": "a", "required": "yes"},
            "required_false": {"command": "a", "required": false},
            "extra": {"command": "a", "startup_timeout_ms": 1},
            "good": {"command": "a"},
        });
        let fixture = Fixture::new();
        fixture.tmp.write(
            "source/settings.json",
            &json!({"mcpServers": servers}).to_string(),
        );
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(
            report.imported.mcp_servers,
            ["good", "monkeyed", "required_false"]
        );
        for item in [
            "server both",
            "server neither",
            "server empty",
            "server numbered",
            "server urlled",
            "server usered",
            "server listed",
            "server enved",
            "server keyed",
            "server objected",
            "server tokened",
            "server keyed_word",
            "server required",
            "server extra",
        ] {
            assert!(!reason_for(&report, item).is_empty(), "{item}");
        }
        // `MONKEY` holds no secret word, so it imports; `required: false`
        // reads as the default and is left out of the entry.
        assert!(reason_for(&report, "server monkeyed").is_empty());
        let written: Value =
            serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            written.pointer("/mcp/servers/required_false").unwrap(),
            &json!({"command": "a"})
        );
        // No refused value reaches Fiber's files.
        assert!(fixture.files_holding("sk-canary-9").is_empty());
        assert!(fixture.files_holding("sk-canary-8").is_empty());
        let tokened = reason_for(&report, "server tokened");
        assert!(tokened.contains("API_TOKEN"));
        assert!(!tokened.contains("sk-canary-9"));
    }

    #[test]
    fn the_word_unrepresentable_imports_but_outside_values_do_not() {
        let servers = json!({
            "worded": {"command": "a", "env": {"NOTE": "unrepresentable"}},
            "numbered_env": {"command": "a", "env": {"PORT": 1}},
            "numbered_command": {"command": 3},
        });
        let fixture = Fixture::new();
        fixture.tmp.write(
            "source/settings.json",
            &json!({"mcpServers": servers}).to_string(),
        );
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        // The literal word is a value like any other: it lands with its
        // spelling intact.
        assert_eq!(report.imported.mcp_servers, ["worded"]);
        assert!(reason_for(&report, "server worded").is_empty());
        let written: Value =
            serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            written.pointer("/mcp/servers/worded/env/NOTE").unwrap(),
            &json!("unrepresentable")
        );
        // Values outside the subset are refused, never interpreted.
        assert_eq!(
            reason_for(&report, "server numbered_env"),
            "`env` is not an object of strings"
        );
        assert_eq!(
            reason_for(&report, "server numbered_command"),
            "`command` is not a string"
        );
    }

    #[test]
    fn secret_words_match_whole_words_only() {
        assert!(is_secret_key("API_TOKEN"));
        assert!(is_secret_key("token"));
        assert!(is_secret_key("SSH_AUTH_SOCK"));
        assert!(is_secret_key("KEY"));
        assert!(!is_secret_key("MONKEY"));
        assert!(!is_secret_key("PORT"));
        assert!(!is_secret_key("TOKENIZER"));
    }

    #[test]
    fn userinfo_is_an_at_sign_in_the_authority() {
        assert!(!url_has_userinfo("https://mcp.example.com/x"));
        assert!(!url_has_userinfo("https://mcp.example.com/a@b"));
        assert!(!url_has_userinfo("mcp.example.com/x"));
        assert!(url_has_userinfo("https://user@mcp.example.com/"));
        assert!(url_has_userinfo("https://user:pass@mcp.example.com/x"));
        assert!(url_has_userinfo("user@mcp.example.com/x"));
    }

    #[test]
    fn codex_subset_refusals_leave_valid_servers_alone() {
        let fixture = Fixture::new();
        fixture.tmp.write(
            "source/config.toml",
            "top = 1\n\
         \n\
         [other]\n\
         x = 1\n\
         \n\
         [mcp_servers.good]\n\
         command = \"good-server\"\n\
         \n\
         [mcp_servers.multi]\n\
         command = \"open\n\
         continued\"\n\
         \n\
         [mcp_servers.dotted.key]\n\
         command = \"x\"\n\
         \n\
         [mcp_servers.table]\n\
         url = \"https://t.example/\"\n\
         extra = 1\n\
         \n\
         [mcp_servers.bare]\n\
         command = bareword\n\
         \n\
         [mcp_servers.\"sp ace\"]\n\
         command = \"x\"\n\
         \n\
         [mcp_servers.slashed/name]\n\
         command = \"x\"\n\
         \n\
         [broken\n",
        );
        let report = fixture.run(ImportSource::Codex).unwrap();
        assert_eq!(report.imported.mcp_servers, ["good"]);
        assert!(!reason_for(&report, "config.toml [other]").is_empty());
        assert!(!reason_for(&report, "config.toml top").is_empty());
        for item in [
            "config.toml [mcp_servers.multi]",
            "config.toml [mcp_servers.dotted.key]",
            "config.toml [mcp_servers.table]",
            "config.toml [mcp_servers.bare]",
            "server sp ace",
            "server slashed/name",
            "config.toml",
        ] {
            assert!(!reason_for(&report, item).is_empty(), "{item}");
        }
    }

    #[test]
    fn codex_values_parse_quote_aware() {
        let fixture = Fixture::new();
        fixture.tmp.write(
            "source/config.toml",
            "[mcp_servers.hash]\n\
         command = \"h-server\" # a comment\n\
         args = [\"a#b\", 'c']\n\
         env = { FRAG = \"https://x.example/y#z\" }\n\
         \n\
         [mcp_servers.\"quoted.name\"]\n\
         url = \"https://q.example/\"\n",
        );
        let report = fixture.run(ImportSource::Codex).unwrap();
        assert_eq!(report.imported.mcp_servers, ["hash", "quoted.name"]);
        let written: Value =
            serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            written.pointer("/mcp/servers/hash").unwrap(),
            &json!({
                "command": "h-server",
                "args": ["a#b", "c"],
                "env": {"FRAG": "https://x.example/y#z"}
            })
        );
    }

    #[test]
    fn escaped_quotes_survive_comment_stripping() {
        // The escaped quote keeps the string open past the `#`: without
        // the skip the quote closes early and the server is refused. The
        // second value opens with an escape, so stepping back instead of
        // over the backslash unopens the string and swallows the comment.
        let fixture = Fixture::new();
        fixture.tmp.write(
            "source/config.toml",
            "[mcp_servers.esc]\n\
             command = \"a\\\"b\" # a comment\n\
             \n\
             [mcp_servers.lead]\n\
             command = \"\\n\" # a comment\n",
        );
        let report = fixture.run(ImportSource::Codex).unwrap();
        assert_eq!(report.imported.mcp_servers, ["esc", "lead"]);
        let written: Value =
            serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            written.pointer("/mcp/servers/esc").unwrap(),
            &json!({"command": "a\"b"})
        );
        assert_eq!(
            written.pointer("/mcp/servers/lead").unwrap(),
            &json!({"command": "\n"})
        );
    }

    #[test]
    fn escape_sequences_parse_to_their_characters() {
        // One value holding every escape `parse_basic_string` handles, so
        // deleting any arm refuses the server instead of importing it.
        let fixture = Fixture::new();
        fixture.tmp.write(
            "source/config.toml",
            "[mcp_servers.cooked]\n\
             command = \"a\\nb\\tc\\rd\\be\\ff\\\\g\\\"h\"\n\
             \n\
             [mcp_servers.lit]\n\
             command = 'tool'\n",
        );
        let report = fixture.run(ImportSource::Codex).unwrap();
        assert_eq!(report.imported.mcp_servers, ["cooked", "lit"]);
        let written: Value =
            serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            written.pointer("/mcp/servers/cooked").unwrap(),
            &json!({"command": "a\nb\tc\rd\u{8}e\u{c}f\\g\"h"})
        );
        assert_eq!(
            written.pointer("/mcp/servers/lit").unwrap(),
            &json!({"command": "tool"})
        );
    }

    #[test]
    fn commas_and_escapes_inside_quotes_do_not_split() {
        // A comma inside quotes is one item, and a leading escaped quote
        // does not close the string: either misread refuses the server.
        let fixture = Fixture::new();
        fixture.tmp.write(
            "source/config.toml",
            "[mcp_servers.comma]\n\
             command = \"x\"\n\
             args = [\"a,b\", \"c\"]\n\
             \n\
             [mcp_servers.lead]\n\
             command = \"x\"\n\
             args = [\"\\\"a\", \"b\"]\n",
        );
        let report = fixture.run(ImportSource::Codex).unwrap();
        assert_eq!(report.imported.mcp_servers, ["comma", "lead"]);
        let written: Value =
            serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            written.pointer("/mcp/servers/comma").unwrap(),
            &json!({"command": "x", "args": ["a,b", "c"]})
        );
        assert_eq!(
            written.pointer("/mcp/servers/lead").unwrap(),
            &json!({"command": "x", "args": ["\"a", "b"]})
        );
    }

    #[test]
    fn codex_direct_table_keys_are_not_servers() {
        let fixture = Fixture::new();
        fixture.write_source("config.toml", "[mcp_servers]\norphan = 1\n");
        let report = fixture.run(ImportSource::Codex).unwrap();
        assert!(report.imported.mcp_servers.is_empty());
        assert!(!reason_for(&report, "config.toml [mcp_servers]").is_empty());
    }

    #[test]
    fn bare_keys_hold_letters_digits_and_two_marks() {
        assert!(is_bare_key("command"));
        assert!(is_bare_key("a-b_c9"));
        assert!(is_bare_key("_"));
        assert!(!is_bare_key(""));
        assert!(!is_bare_key("a b"));
        assert!(!is_bare_key("a.b"));
    }

    #[test]
    fn a_file_is_not_a_directory_listing() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.source()).unwrap();
        fs::write(fixture.source().join("commands"), "not a dir\n").unwrap();
        assert!(read_dir_names(&fixture.source().join("commands")).is_err());
    }

    #[test]
    fn a_command_directory_named_like_a_file_is_left_behind() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.source().join("commands/dirmd.md")).unwrap();
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert!(report.imported.commands.is_empty());
        assert_eq!(
            reason_for(&report, "commands/dirmd.md"),
            "is not a command file"
        );
    }

    #[test]
    fn an_occupied_skill_directory_is_taken() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.home().join("skills/review")).unwrap();
        fs::write(fixture.home().join("skills/review/SKILL.md"), "other\n").unwrap();
        let taken: BTreeSet<String> = ["review".to_owned()].into_iter().collect();
        assert!(skill_taken(&fixture.home(), "review", b"new\n", &taken));
        assert!(!skill_taken(&fixture.home(), "review", b"other\n", &taken));
        assert!(!skill_taken(&fixture.home(), "fresh", b"new\n", &taken));
    }

    #[test]
    fn an_empty_taken_set_takes_the_first_suffix() {
        // Pins the `taken.len() + 2` bound: with nothing taken the range
        // still holds `-2`, while `taken.len() * 2` empties it and hits
        // the unreachable in `suffixed`.
        assert_eq!(suffixed(&BTreeSet::new(), "review"), "review-2");
    }

    #[test]
    fn allocate_names_suffixes_a_colliding_server() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.home()).unwrap();
        fixture.tmp.write(
            "home/config.json",
            &json!({"mcp": {"servers": {"notes": {"command": "other"}}}}).to_string(),
        );
        let mut servers = vec![PlannedServer {
            name: "notes".to_owned(),
            value: json!({"command": "a"}),
        }];
        allocate_names(&fixture.home(), &mut [], &mut [], &mut servers).unwrap();
        assert_eq!(servers[0].name, "notes-2");
    }

    #[test]
    fn write_servers_suffixes_a_changed_entry() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.home()).unwrap();
        fixture.tmp.write(
            "home/config.json",
            &json!({"mcp": {"servers": {"notes": {"command": "other"}}}}).to_string(),
        );
        let mut servers = vec![PlannedServer {
            name: "notes".to_owned(),
            value: json!({"command": "a"}),
        }];
        write_servers(&fixture.home(), &mut servers).unwrap();
        assert_eq!(servers[0].name, "notes-2");
        let written: Value =
            serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            written.pointer("/mcp/servers/notes").unwrap(),
            &json!({"command": "other"})
        );
        assert_eq!(
            written.pointer("/mcp/servers/notes-2").unwrap(),
            &json!({"command": "a"})
        );
    }

    #[test]
    fn secrets_never_reach_fiber_files_or_the_report_values() {
        let fixture = Fixture::new();
        fixture.tmp.write(
            "source/settings.json",
            &json!({"mcpServers": {
                "vault": {"command": "v", "env": {"VAULT_TOKEN": "sk-canary-7"}},
                "plain": {"command": "p"},
            }})
            .to_string(),
        );
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(report.imported.mcp_servers, ["plain"]);
        assert_eq!(
            reason_for(&report, "server vault"),
            "`env` holds a secret value for `VAULT_TOKEN`"
        );
        assert!(fixture.files_holding("sk-canary-7").is_empty());
        let debug = format!("{report:?}");
        assert!(!debug.contains("sk-canary-7"));
    }

    #[test]
    fn an_instruction_that_changes_under_the_plan_aborts() {
        let fixture = Fixture::new();
        let planned = Planned {
            skills: Vec::new(),
            commands: Vec::new(),
            instructions: vec![PlannedFile {
                name: "CLAUDE.md".to_owned(),
                bytes: b"new\n".to_vec(),
            }],
            servers: Vec::new(),
            left_behind: Vec::new(),
        };
        fs::create_dir_all(fixture.home()).unwrap();
        fs::write(fixture.home().join("CLAUDE.md"), "changed\n").unwrap();
        let mut planned = planned;
        assert!(write_all(&fixture.home(), &mut planned).is_err());
        assert_eq!(
            fs::read(fixture.home().join("CLAUDE.md")).unwrap(),
            b"changed\n"
        );
    }

    #[test]
    fn source_home_paths_resolve_under_the_persons_home() {
        assert_eq!(
            source_home(Path::new("/home/alice"), ImportSource::ClaudeCode),
            PathBuf::from("/home/alice/.claude")
        );
        assert_eq!(
            source_home(Path::new("/home/alice"), ImportSource::Codex),
            PathBuf::from("/home/alice/.codex")
        );
        assert_eq!(ImportSource::ClaudeCode.name(), "claude-code");
        assert_eq!(ImportSource::Codex.name(), "codex");
    }

    #[test]
    fn top_level_entries_each_name_what_is_left_behind() {
        let fixture = Fixture::new();
        for name in [
            "agents",
            "hooks",
            "projects",
            "sessions",
            "todos",
            "permissions",
        ] {
            fs::create_dir_all(fixture.source().join(name)).unwrap();
        }
        fs::write(fixture.source().join("auth.json"), "{}\n").unwrap();
        fs::write(fixture.source().join("mystery"), "x\n").unwrap();
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert_eq!(
            reason_for(&report, "agents"),
            "agents are not configuration"
        );
        assert_eq!(reason_for(&report, "hooks"), "hooks are not imported");
        assert_eq!(
            reason_for(&report, "projects"),
            "sessions, history and todos are not imported"
        );
        assert_eq!(
            reason_for(&report, "sessions"),
            "sessions, history and todos are not imported"
        );
        assert_eq!(
            reason_for(&report, "todos"),
            "sessions, history and todos are not imported"
        );
        assert_eq!(
            reason_for(&report, "permissions"),
            "permissions are not imported"
        );
        assert_eq!(
            reason_for(&report, "auth.json"),
            "credentials are never imported"
        );
        assert_eq!(
            reason_for(&report, "mystery"),
            "is not configuration Fiber imports"
        );
    }

    #[test]
    fn a_source_home_that_is_a_file_is_an_error() {
        let fixture = Fixture::new();
        fs::write(fixture.source(), "not a dir\n").unwrap();
        assert!(fixture.run(ImportSource::ClaudeCode).is_err());
    }

    #[test]
    fn left_behind_names_never_hold_values() {
        let fixture = Fixture::new();
        fixture.tmp.write(
            "source/settings.json",
            &json!({"mcpServers": {
                "s": {"command": "c", "env": {"TOKEN": "sk-canary-3"}},
            }})
            .to_string(),
        );
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        for left in &report.left_behind {
            assert!(!left.item.contains("sk-canary-3"), "{left:?}");
            assert!(!left.reason.contains("sk-canary-3"), "{left:?}");
        }
        assert!(fixture.files_holding("sk-canary-3").is_empty());
    }
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod import_tests;
