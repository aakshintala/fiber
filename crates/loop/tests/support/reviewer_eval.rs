//! The reviewer-eval set's schema, scrubber, sampler and importer
//! (issue #477): what a labelled line carries, how raw calls are dropped
//! and scrubbed, how the first 400 inputs are kept, and how native Claude
//! Code project JSONL and pi session JSONL lines map to the same schema.
//!
//! The importer covers the pinned native subset the set was built from: a
//! `user` line's text is the person's message, an `assistant` line's text
//! is the pending reply, a `tool_use`/`toolcall` block is the effectful
//! call, a result block is stripped, and a `summary`/`compaction` line acts
//! as a handoff. A line carrying conversation the pinned shapes do not
//! cover is a hard error naming the file and line, so native-shape drift
//! surfaces at lane-build time instead of silently skipping calls. Lines
//! carrying no conversation skip.

use std::path::{Path, PathBuf};

use contract::shapes::{DeclaredEffects, Effect};
use serde_json::{Map, Value};

/// How many label inputs the set keeps: the first entries in emission order.
pub(crate) const MAX_SAMPLED: usize = 400;

/// Where a set line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    /// A real call sampled from a session.
    Sampled,
    /// A hand-written bad call.
    Seeded,
}

impl SourceKind {
    /// The wire word.
    /// The wire word.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SourceKind::Sampled => "sampled",
            SourceKind::Seeded => "seeded",
        }
    }

    /// Parses the wire word.
    fn parse(raw: &str) -> Option<SourceKind> {
        match raw {
            "sampled" => Some(SourceKind::Sampled),
            "seeded" => Some(SourceKind::Seeded),
            _ => None,
        }
    }
}

/// One agent family's verdict on a set line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Label {
    /// The reviewer should deny the call.
    Block,
    /// The reviewer should allow the call.
    Pass,
}

impl Label {
    /// The wire word.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Label::Block => "block",
            Label::Pass => "pass",
        }
    }

    /// Parses the wire word.
    fn parse(raw: &str) -> Option<Label> {
        match raw {
            "block" => Some(Label::Block),
            "pass" => Some(Label::Pass),
            _ => None,
        }
    }
}

/// Both families' verdicts: the second pass fills `family2` on a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Labels {
    /// The first family's verdict, filled during the build.
    pub family1: Option<Label>,
    /// The second family's verdict, filled on a copy.
    pub family2: Option<Label>,
}

/// What one `reviewed[]` item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewedKind {
    /// A person's message.
    Person,
    /// An earlier tool call.
    Call,
    /// The agent's reply a person message answers.
    Reply,
}

impl ReviewedKind {
    /// The wire word.
    fn as_str(self) -> &'static str {
        match self {
            ReviewedKind::Person => "person",
            ReviewedKind::Call => "call",
            ReviewedKind::Reply => "reply",
        }
    }

    /// Parses the wire word.
    fn parse(raw: &str) -> Option<ReviewedKind> {
        match raw {
            "person" => Some(ReviewedKind::Person),
            "call" => Some(ReviewedKind::Call),
            "reply" => Some(ReviewedKind::Reply),
            _ => None,
        }
    }
}

/// One item of what the reviewer is shown, in set-line form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewedItem {
    /// What the item is.
    pub kind: ReviewedKind,
    /// The synthetic seq: the 1-based native-file line that wrote it.
    pub seq: Option<u64>,
    /// The person's message index within its line, for person items.
    pub item: Option<u64>,
    /// The call's action, for call items: the entry's own id.
    pub action: Option<String>,
    /// The item as the reviewer reads it.
    pub text: String,
}

/// A set line's provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Source {
    /// Where the line came from.
    pub kind: SourceKind,
    /// The session or native file the line was sampled from.
    pub session: String,
    /// The 1-based native-file line, absent on seeded lines.
    pub seq: Option<u64>,
}

/// One labelled line of the reviewer-eval set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetLine {
    /// The line's id.
    pub id: String,
    /// Where the line came from.
    pub source: Source,
    /// The tool the call ran on.
    pub tool: String,
    /// The arguments the reviewer judged, scrubbed.
    pub arguments: Value,
    /// The call's declared effects, scrubbed.
    pub declared: DeclaredEffects,
    /// The network hosts the call contacts, where the table names them.
    pub hosts: Option<Vec<String>>,
    /// The workspace label the entry replays under, scrubbed.
    pub workspace: String,
    /// The added folders in first-event order as of the entry's cut, scrubbed.
    pub added_folders: Vec<String>,
    /// What the reviewer is shown, ending with the entry's own call.
    pub reviewed: Vec<ReviewedItem>,
    /// Both families' verdicts.
    pub labels: Labels,
}

/// One string field of a line object.
fn line_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("review: bad set line: {key} is not a string"))
}

/// One optional string field of a line object.
fn opt_line_str(value: &Value, key: &str) -> Result<Option<String>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("review: bad set line: {key} is not a string")),
    }
}

/// One unsigned field of a line object.
fn opt_line_u64(value: &Value, key: &str) -> Result<Option<u64>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(count)) => count
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("review: bad set line: {key} is not an unsigned number")),
        Some(_) => Err(format!("review: bad set line: {key} is not a count")),
    }
}

/// Parses one declared-effects object.
fn parse_declared(value: &Value) -> Result<DeclaredEffects, String> {
    let effects = value
        .get("effects")
        .and_then(Value::as_array)
        .ok_or_else(|| "review: bad set line: declared has no effects".to_owned())?;
    let mut parsed = Vec::new();
    for effect in effects {
        let name = effect
            .as_str()
            .ok_or_else(|| "review: bad set line: an effect is not a string".to_owned())?;
        match name {
            "reads" => parsed.push(Effect::Reads),
            "writes" => parsed.push(Effect::Writes),
            "executes" => parsed.push(Effect::Executes),
            "network" => parsed.push(Effect::Network),
            _ => return Err(format!("review: bad set line: unknown effect {name:?}")),
        }
    }
    let reversible = value
        .get("reversible")
        .and_then(Value::as_bool)
        .ok_or_else(|| "review: bad set line: declared has no reversible".to_owned())?;
    let paths = match value.get("paths") {
        None | Some(Value::Null) => None,
        Some(Value::Array(paths)) => {
            let mut parsed_paths = Vec::new();
            for path in paths {
                parsed_paths.push(
                    path.as_str()
                        .ok_or_else(|| {
                            "review: bad set line: a declared path is not a string".to_owned()
                        })?
                        .to_owned(),
                );
            }
            Some(parsed_paths)
        }
        Some(_) => return Err("review: bad set line: declared paths is not an array".to_owned()),
    };
    let confined = value
        .get("confined")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(DeclaredEffects {
        effects: parsed,
        reversible,
        paths,
        confined,
    })
}

/// Parses one `reviewed[]` item.
fn parse_reviewed_item(value: &Value) -> Result<ReviewedItem, String> {
    let kind = ReviewedKind::parse(line_str(value, "kind")?)
        .ok_or_else(|| "review: bad set line: a reviewed item names no known kind".to_owned())?;
    Ok(ReviewedItem {
        kind,
        seq: opt_line_u64(value, "seq")?,
        item: opt_line_u64(value, "item")?,
        action: opt_line_str(value, "action")?,
        text: line_str(value, "text")?.to_owned(),
    })
}

/// Prints one `reviewed[]` item.
fn reviewed_item_value(item: &ReviewedItem) -> Value {
    let mut map = Map::new();
    map.insert(
        "kind".to_owned(),
        Value::String(item.kind.as_str().to_owned()),
    );
    if let Some(seq) = item.seq {
        map.insert("seq".to_owned(), Value::Number(seq.into()));
    }
    if let Some(item_at) = item.item {
        map.insert("item".to_owned(), Value::Number(item_at.into()));
    }
    if let Some(action) = &item.action {
        map.insert("action".to_owned(), Value::String(action.clone()));
    }
    map.insert("text".to_owned(), Value::String(item.text.clone()));
    Value::Object(map)
}

/// Prints one declared-effects object, omitting `confined` when false.
fn declared_value(declared: &DeclaredEffects) -> Value {
    let effects: Vec<Value> = declared
        .effects
        .iter()
        .map(|effect| {
            Value::String(
                match effect {
                    Effect::Reads => "reads",
                    Effect::Writes => "writes",
                    Effect::Executes => "executes",
                    Effect::Network => "network",
                }
                .to_owned(),
            )
        })
        .collect();
    let mut map = Map::new();
    map.insert("effects".to_owned(), Value::Array(effects));
    map.insert("reversible".to_owned(), Value::Bool(declared.reversible));
    match &declared.paths {
        Some(paths) => map.insert(
            "paths".to_owned(),
            Value::Array(
                paths
                    .iter()
                    .map(|path| Value::String(path.clone()))
                    .collect(),
            ),
        ),
        None => map.insert("paths".to_owned(), Value::Null),
    };
    if declared.confined {
        map.insert("confined".to_owned(), Value::Bool(true));
    }
    Value::Object(map)
}

/// Prints one set line's object.
fn set_line_value(entry: &SetLine) -> Value {
    let mut source = Map::new();
    source.insert(
        "kind".to_owned(),
        Value::String(entry.source.kind.as_str().to_owned()),
    );
    source.insert(
        "session".to_owned(),
        Value::String(entry.source.session.clone()),
    );
    if let Some(seq) = entry.source.seq {
        source.insert("seq".to_owned(), Value::Number(seq.into()));
    }
    let mut labels = Map::new();
    labels.insert(
        "family1".to_owned(),
        entry.labels.family1.map_or(Value::Null, |label| {
            Value::String(label.as_str().to_owned())
        }),
    );
    labels.insert(
        "family2".to_owned(),
        entry.labels.family2.map_or(Value::Null, |label| {
            Value::String(label.as_str().to_owned())
        }),
    );
    let mut map = Map::new();
    map.insert("id".to_owned(), Value::String(entry.id.clone()));
    map.insert("source".to_owned(), Value::Object(source));
    map.insert("tool".to_owned(), Value::String(entry.tool.clone()));
    map.insert("arguments".to_owned(), entry.arguments.clone());
    map.insert("declared".to_owned(), declared_value(&entry.declared));
    if let Some(hosts) = &entry.hosts {
        map.insert(
            "hosts".to_owned(),
            Value::Array(
                hosts
                    .iter()
                    .map(|host| Value::String(host.clone()))
                    .collect(),
            ),
        );
    }
    map.insert(
        "workspace".to_owned(),
        Value::String(entry.workspace.clone()),
    );
    map.insert(
        "added_folders".to_owned(),
        Value::Array(
            entry
                .added_folders
                .iter()
                .map(|folder| Value::String(folder.clone()))
                .collect(),
        ),
    );
    map.insert(
        "reviewed".to_owned(),
        Value::Array(entry.reviewed.iter().map(reviewed_item_value).collect()),
    );
    map.insert("labels".to_owned(), Value::Object(labels));
    Value::Object(map)
}

/// Parses one committed set line, rejecting empty ids and tools.
pub(crate) fn parse_line(line: &str) -> Result<SetLine, String> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| format!("review: bad set line: {error}"))?;
    let id = line_str(&value, "id")?.to_owned();
    if id.is_empty() {
        return Err("review: bad set line: the id is empty".to_owned());
    }
    let tool = line_str(&value, "tool")?.to_owned();
    if tool.is_empty() {
        return Err(format!("review: bad set line {id}: the tool is empty"));
    }
    let source_value = value
        .get("source")
        .ok_or_else(|| format!("review: bad set line {id}: no source"))?;
    let kind = SourceKind::parse(line_str(source_value, "kind")?)
        .ok_or_else(|| format!("review: bad set line {id}: the source kind is unknown"))?;
    let source = Source {
        kind,
        session: line_str(source_value, "session")?.to_owned(),
        seq: opt_line_u64(source_value, "seq")?,
    };
    let declared_value = value
        .get("declared")
        .ok_or_else(|| format!("review: bad set line {id}: no declared effects"))?;
    let hosts = match value.get("hosts") {
        None | Some(Value::Null) => None,
        Some(Value::Array(hosts)) => {
            let mut parsed = Vec::new();
            for host in hosts {
                parsed.push(
                    host.as_str()
                        .ok_or_else(|| {
                            format!("review: bad set line {id}: a host is not a string")
                        })?
                        .to_owned(),
                );
            }
            Some(parsed)
        }
        Some(_) => {
            return Err(format!("review: bad set line {id}: hosts is not an array"));
        }
    };
    let added_value = value
        .get("added_folders")
        .ok_or_else(|| format!("review: bad set line {id}: no added folders"))?;
    let added_folders = added_value
        .as_array()
        .ok_or_else(|| format!("review: bad set line {id}: added folders is not an array"))?
        .iter()
        .map(|folder| {
            folder
                .as_str()
                .ok_or_else(|| {
                    format!("review: bad set line {id}: an added folder is not a string")
                })
                .map(str::to_owned)
        })
        .collect::<Result<Vec<String>, String>>()?;
    let reviewed_value = value
        .get("reviewed")
        .ok_or_else(|| format!("review: bad set line {id}: no reviewed items"))?;
    let reviewed = reviewed_value
        .as_array()
        .ok_or_else(|| format!("review: bad set line {id}: reviewed is not an array"))?
        .iter()
        .map(parse_reviewed_item)
        .collect::<Result<Vec<ReviewedItem>, String>>()?;
    let labels_value = value
        .get("labels")
        .ok_or_else(|| format!("review: bad set line {id}: no labels"))?;
    let label_id = id.clone();
    let family = |key: &str| match labels_value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => Label::parse(raw)
            .map(Some)
            .ok_or_else(|| format!("review: bad set line {label_id}: unknown label {raw:?}")),
        Some(_) => Err(format!(
            "review: bad set line {label_id}: a label is not a string"
        )),
    };
    Ok(SetLine {
        id,
        source,
        tool,
        arguments: value.get("arguments").cloned().unwrap_or(Value::Null),
        declared: parse_declared(declared_value)?,
        hosts,
        workspace: line_str(&value, "workspace")?.to_owned(),
        added_folders,
        reviewed,
        labels: Labels {
            family1: family("family1")?,
            family2: family("family2")?,
        },
    })
}

/// Prints one set line on one line.
pub(crate) fn emit_line(entry: &SetLine) -> Result<String, String> {
    serde_json::to_string(&set_line_value(entry))
        .map_err(|error| format!("review: cannot print its set: {error}"))
}

/// Checks a seeded line carries a seeded source and a first-family block.
pub(crate) fn validate_seeded(entry: &SetLine) -> Result<(), String> {
    if entry.source.kind != SourceKind::Seeded {
        return Err(format!("review: seeded line {} is not seeded", entry.id));
    }
    if entry.labels.family1 != Some(Label::Block) {
        return Err(format!(
            "review: seeded line {} carries no first-family block",
            entry.id
        ));
    }
    Ok(())
}

/// The placeholder values real paths, names and addresses map to. The
/// values arrive as explicit parameters, never read from the environment,
/// so nothing real can leak into the repository and tests use synthetic
/// values.
pub(crate) struct ScrubCtx {
    /// The home directory, scrubbed to `$HOME`.
    pub home: String,
    /// The owner's name, scrubbed to `$OWNER_NAME`.
    pub owner_name: String,
    /// The owner's email, scrubbed to `$OWNER_EMAIL`.
    pub owner_email: String,
}

impl ScrubCtx {
    /// Scrubs one string: the home directory, then the email, then the
    /// name, so a name that is a substring of the email still leaves the
    /// email fully masked. Empty values replace nothing.
    pub(crate) fn scrub_text(&self, raw: &str) -> String {
        let mut out = raw.to_owned();
        for (needle, placeholder) in [
            (&self.home, "$HOME"),
            (&self.owner_email, "$OWNER_EMAIL"),
            (&self.owner_name, "$OWNER_NAME"),
        ] {
            if !needle.is_empty() {
                out = out.replace(needle.as_str(), placeholder);
            }
        }
        out
    }

    /// Scrubs every string in a JSON value.
    pub(crate) fn scrub_value(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.scrub_text(text)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| self.scrub_value(item)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, item)| (self.scrub_text(key), self.scrub_value(item)))
                    .collect(),
            ),
            Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        }
    }
}

/// One closed secret-matcher class: the matchers are `str` operations only,
/// so no pattern crate joins the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecretClass {
    /// An AWS key: the `AKIA` prefix with a 16-character alnum tail.
    AwsKey,
    /// A private key: the `BEGIN` plus `PRIVATE KEY` block marker.
    PrivateKey,
    /// A GitHub token: a `ghp_`, `gho_` or `github_pat_` prefix.
    GithubToken,
    /// An Anthropic key: the `sk-ant-` prefix.
    AnthropicKey,
    /// A Slack token: an `xox[baprs]-` prefix.
    SlackToken,
    /// A Google API key: the `AIza` prefix.
    GoogleApiKey,
}

/// The reviewer input one set line stores, converted from the loop's own
/// `Reviewed` snapshot so emission stores exactly what the reviewer was
/// sent at the call's request position.
pub(crate) fn reviewed_items(reviewed: &[r#loop::Reviewed]) -> Vec<ReviewedItem> {
    reviewed
        .iter()
        .map(|item| {
            let (kind, seq, at, action) = match &item.shown {
                r#loop::Shown::Call(action) => {
                    (ReviewedKind::Call, None, None, Some(action.0.clone()))
                }
                r#loop::Shown::Person(kept) => (
                    ReviewedKind::Person,
                    Some(kept.seq.0),
                    Some(kept.item),
                    None,
                ),
                r#loop::Shown::Reply(_) => (ReviewedKind::Reply, None, None, None),
            };
            let text = match &item.input {
                contract::provider::Input::User { text, .. } => text.clone(),
                // The renderer only writes person, call and reply items,
                // which all read as the person's text.
                contract::provider::Input::Assistant { .. }
                | contract::provider::Input::Reasoning { .. }
                | contract::provider::Input::ToolCall { .. }
                | contract::provider::Input::ToolResult { .. }
                | contract::provider::Input::ToolSearchResult { .. } => String::new(),
            };
            ReviewedItem {
                kind,
                seq,
                item: at,
                action,
                text,
            }
        })
        .collect()
}

impl SecretClass {
    /// The name the final-set scan reports the drop under.
    pub(crate) fn name(self) -> &'static str {
        match self {
            SecretClass::AwsKey => "aws-key",
            SecretClass::PrivateKey => "private-key",
            SecretClass::GithubToken => "github-token",
            SecretClass::AnthropicKey => "anthropic-key",
            SecretClass::SlackToken => "slack-token",
            SecretClass::GoogleApiKey => "google-api-key",
        }
    }

    /// The first closed class matching `text`, in class order.
    pub(crate) fn find(text: &str) -> Option<SecretClass> {
        if has_aws_key(text) {
            return Some(SecretClass::AwsKey);
        }
        if text.contains("BEGIN") && text.contains("PRIVATE KEY") {
            return Some(SecretClass::PrivateKey);
        }
        if text.contains("ghp_") || text.contains("gho_") || text.contains("github_pat_") {
            return Some(SecretClass::GithubToken);
        }
        if text.contains("sk-ant-") {
            return Some(SecretClass::AnthropicKey);
        }
        if ["xoxb-", "xoxa-", "xoxp-", "xoxr-", "xoxs-"]
            .iter()
            .any(|prefix| text.contains(prefix))
        {
            return Some(SecretClass::SlackToken);
        }
        if text.contains("AIza") {
            return Some(SecretClass::GoogleApiKey);
        }
        None
    }
}

/// Whether `text` holds an AWS key: `AKIA` with 16 alnum tail characters.
fn has_aws_key(text: &str) -> bool {
    for at in text.match_indices("AKIA").map(|(at, _)| at) {
        let tail = text.get(at + 4..).unwrap_or_default();
        if tail.chars().take(16).count() == 16
            && tail
                .chars()
                .take(16)
                .all(|cell| cell.is_ascii_alphanumeric())
        {
            return true;
        }
    }
    false
}

/// Maps one committed line to its matching closed class, if any. The scan
/// runs on committed bytes, so placeholders and seeded command shapes must
/// trip no matcher.
pub(crate) fn scan_line(line: &str) -> Option<&'static str> {
    SecretClass::find(line).map(SecretClass::name)
}

/// Why a raw entry never reaches the set, in drop-priority order: the
/// probe-keys path, then the `credentials/` directory, then any other key
/// file, then a secret-scanner match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropKind {
    /// The entry touches a probe-keys path.
    ProbeKeys,
    /// The entry touches a `credentials/` directory.
    CredentialsDir,
    /// The entry touches a key file.
    KeyFile,
    /// A closed secret matcher fired, named by its class.
    Secret(&'static str),
}

impl DropKind {
    /// The name the loud drop report uses.
    pub(crate) fn name(self) -> &'static str {
        match self {
            DropKind::ProbeKeys => "probe-keys",
            DropKind::CredentialsDir => "credentials-dir",
            DropKind::KeyFile => "key-file",
            DropKind::Secret(class) => class,
        }
    }
}

/// The key-file basenames, beside the `credentials` AWS shared file.
fn is_key_basename(base: &str) -> bool {
    matches!(
        base,
        "id_rsa" | "id_ed25519" | "secrets.json" | "credentials.json" | "credentials"
    ) || base.ends_with(".pem")
        || base.ends_with(".key")
}

/// Whether `candidate` passes through a `credentials/` directory: a
/// non-final path segment, so the `credentials` file itself stays the
/// key-file basename rule's.
fn in_credentials_dir(candidate: &str) -> bool {
    let mut segments: Vec<&str> = candidate.split('/').collect();
    segments.pop();
    segments.contains(&"credentials")
}

/// Every string value in a JSON value, for path matching.
fn string_values(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                string_values(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                string_values(item, out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Maps a raw entry to its drop class, if any. The checks run on the raw
/// entry before placeholder substitution, and probe-keys wins over the
/// `credentials/` dir, which wins over other key files, which win over a
/// secret-scanner match.
pub(crate) fn drop_kind(arguments: &Value, paths: &[String]) -> Option<DropKind> {
    let mut candidates: Vec<String> = paths.to_vec();
    string_values(arguments, &mut candidates);
    if candidates
        .iter()
        .any(|candidate| candidate.contains("/.config/probe-keys/"))
    {
        return Some(DropKind::ProbeKeys);
    }
    if candidates
        .iter()
        .any(|candidate| in_credentials_dir(candidate))
    {
        return Some(DropKind::CredentialsDir);
    }
    if candidates
        .iter()
        .any(|candidate| candidate.rsplit('/').next().is_some_and(is_key_basename))
    {
        return Some(DropKind::KeyFile);
    }
    let haystack = candidates.join("\n") + "\n" + &arguments.to_string();
    if let Some(class) = SecretClass::find(&haystack) {
        return Some(DropKind::Secret(class.name()));
    }
    None
}

/// Keeps the first `MAX_SAMPLED` entries in emission order. The dropped
/// count is the length past the cap; the kept count is the smaller of the
/// two, so no comparison decides it.
pub(crate) fn sample<T>(mut entries: Vec<T>) -> Vec<T> {
    let total = entries.len();
    let dropped = total.saturating_sub(MAX_SAMPLED);
    entries.truncate(total.saturating_sub(dropped));
    entries
}

/// The jig's new sampling, scrub-context, importer and scan flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SampleOpts {
    /// `--sample N`: keep the first N inputs, at most `MAX_SAMPLED`.
    pub sample: Option<usize>,
    /// `--scrub`: scrub raw values through the scrub context.
    pub scrub: bool,
    /// `--as-home PATH`: the home directory value.
    pub home: Option<String>,
    /// `--owner-name NAME`: the owner name value.
    pub owner_name: Option<String>,
    /// `--owner-email EMAIL`: the owner email value.
    pub owner_email: Option<String>,
    /// `--import-claude PATH`, repeatable: native Claude Code session files.
    pub import_claude: Vec<String>,
    /// `--import-pi PATH`, repeatable: native pi session files.
    pub import_pi: Vec<String>,
    /// `--scan FILE`: the final-set scan over the committed file.
    pub scan: Option<String>,
}

impl SampleOpts {
    /// Whether sampling, scrub-context or importer flags ask for
    /// label-input emission. Scrub-context values alone imply emission, so
    /// raw owner values never reach the default path by accident.
    pub(crate) fn wants_sampled(&self) -> bool {
        self.sample.is_some()
            || self.scrub
            || self.home.is_some()
            || self.owner_name.is_some()
            || self.owner_email.is_some()
            || !self.import_claude.is_empty()
            || !self.import_pi.is_empty()
    }
}

/// Reads one flag value, naming the flag when it is missing.
fn flag_value(args: &[String], at: &mut usize, flag: &str) -> Result<String, String> {
    match args.get(*at) {
        Some(value) => {
            *at += 1;
            Ok(value.clone())
        }
        None => Err(format!("review: {flag} needs a value")),
    }
}

/// Reads `--sample N`, rejecting missing and non-positive counts.
fn sample_count(args: &[String], at: &mut usize) -> Result<usize, String> {
    let raw = flag_value(args, at, "--sample")?;
    match raw.parse::<usize>() {
        Ok(count) if count > 0 => Ok(count),
        Ok(_) => Err("review: --sample needs a positive count".to_owned()),
        Err(_) => Err(format!("review: --sample needs a count, not {raw:?}")),
    }
}

/// Parses the new flags out of `args`, passing every other flag through
/// untouched for `parse_args`.
pub(crate) fn parse_sample_flags(args: &[String]) -> Result<(SampleOpts, Vec<String>), String> {
    let mut opts = SampleOpts::default();
    let mut rest = Vec::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        at += 1;
        if arg == "--sample" {
            opts.sample = Some(sample_count(args, &mut at)?);
        } else if arg == "--scrub" {
            opts.scrub = true;
        } else if arg == "--as-home" {
            opts.home = Some(flag_value(args, &mut at, "--as-home")?);
        } else if arg == "--owner-name" {
            opts.owner_name = Some(flag_value(args, &mut at, "--owner-name")?);
        } else if arg == "--owner-email" {
            opts.owner_email = Some(flag_value(args, &mut at, "--owner-email")?);
        } else if arg == "--import-claude" {
            opts.import_claude
                .push(flag_value(args, &mut at, "--import-claude")?);
        } else if arg == "--import-pi" {
            opts.import_pi
                .push(flag_value(args, &mut at, "--import-pi")?);
        } else if arg == "--scan" {
            opts.scan = Some(flag_value(args, &mut at, "--scan")?);
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((opts, rest))
}

/// Splits the new flags off `argv`, returning the remaining argv for
/// `parse_args` and the parsed new flags.
pub(crate) fn split_argv(
    args: impl Iterator<Item = String>,
) -> Result<(Vec<String>, SampleOpts), String> {
    let args: Vec<String> = args.collect();
    let (opts, rest) = parse_sample_flags(&args)?;
    Ok((rest, opts))
}

/// What one pinned native line carries, in block order: an assistant line
/// mixing pending text with its calls folds exactly as the live renderer
/// would, dropping the text the call breaks.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NativeLine {
    /// The person's message text.
    Person(String),
    /// The agent's pending reply text.
    AssistantText(String),
    /// An effectful tool call.
    Call {
        /// The tool's name.
        tool: String,
        /// The call's arguments.
        arguments: Value,
    },
    /// A tool result: stripped before any prompt is built.
    Result,
    /// A compaction marker: the fold acts as a handoff.
    Compaction,
    /// A line carrying no conversation.
    Skip,
}

/// Maps one native Claude tool spelling to the fiber tool and its judged
/// arguments: `Bash` runs a shell command, `Read` reads `file_path`,
/// `Edit` replaces `old_string` with `new_string`, and `Write` writes
/// `content`. Lowercase names pass through with their arguments untouched.
/// Anything else passes through under its native name, so the declared
/// table drops it with a count instead of failing the file.
fn map_native_call(name: &str, input: &Value) -> (String, Value) {
    let object = input.as_object();
    if name == "Bash"
        && let Some(object) = object
        && let Some(command) = object.get("command").and_then(Value::as_str)
    {
        let mut mapped = serde_json::Map::new();
        mapped.insert("command".to_owned(), Value::String(command.to_owned()));
        return ("shell".to_owned(), Value::Object(mapped));
    }
    if name == "Read"
        && let Some(object) = object
        && let Some(path) = object.get("file_path").and_then(Value::as_str)
    {
        let mut mapped = serde_json::Map::new();
        mapped.insert("path".to_owned(), Value::String(path.to_owned()));
        for key in ["offset", "limit"] {
            if let Some(value) = object.get(key) {
                mapped.insert(key.to_owned(), value.clone());
            }
        }
        return ("read".to_owned(), Value::Object(mapped));
    }
    if name == "Edit"
        && let Some(object) = object
        && let Some(path) = object.get("file_path").and_then(Value::as_str)
        && let Some(old_text) = object.get("old_string").and_then(Value::as_str)
        && let Some(new_text) = object.get("new_string").and_then(Value::as_str)
    {
        return (
            "edit".to_owned(),
            serde_json::json!({
                "path": path,
                "edits": [{"old_text": old_text, "new_text": new_text}],
            }),
        );
    }
    if name == "Write"
        && let Some(object) = object
        && let Some(path) = object.get("file_path").and_then(Value::as_str)
        && let Some(content) = object.get("content")
    {
        return (
            "write".to_owned(),
            serde_json::json!({"path": path, "content": content}),
        );
    }
    (
        name.to_owned(),
        object
            .map(|object| Value::Object(object.clone()))
            .unwrap_or(Value::Null),
    )
}

/// One string field of a JSON object.
fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

/// Joins a message content array's text parts with newlines, mirroring how
/// the fold appends text parts to the held reply.
fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut texts = Vec::new();
            for block in blocks {
                let kind = str_field(block, "type").unwrap_or_default();
                if kind == "text" {
                    if let Some(text) = str_field(block, "text") {
                        texts.push(text.to_owned());
                    }
                } else if kind == "tool_use" || kind == "tool_result" || kind == "tool_use_result" {
                    continue;
                } else {
                    return None;
                }
            }
            Some(texts.join("\n"))
        }
        Value::Null => None,
        Value::Object(_) | Value::Bool(_) | Value::Number(_) => None,
    }
}

/// Whether a message content array holds a result block, which the fold
/// strips instead of showing.
fn has_result_block(content: &Value) -> bool {
    match content {
        Value::Array(blocks) => blocks.iter().any(|block| {
            matches!(
                str_field(block, "type").unwrap_or_default(),
                "tool_result" | "tool_use_result"
            )
        }),
        Value::String(_) | Value::Null | Value::Object(_) | Value::Bool(_) | Value::Number(_) => {
            false
        }
    }
}

/// Classifies one parsed Claude Code project JSONL value into its line
/// items in block order. Reasoning blocks are skipped: they shape no
/// prompt the reviewer is shown. An `api-request-blob` line is the
/// transport's mirror of the same turns the person and agent lines carry,
/// so it skips instead of doubling every entry.
fn classify_claude(value: &Value) -> Result<Vec<NativeLine>, String> {
    let kind = str_field(value, "type").unwrap_or_default();
    if kind == "api-request-blob" {
        return Ok(vec![NativeLine::Skip]);
    }
    if kind == "user" {
        let message = value.get("message");
        let content = message.and_then(|message| message.get("content"));
        match content {
            None | Some(Value::Null) => Ok(vec![NativeLine::Skip]),
            Some(content) if has_result_block(content) => Ok(vec![NativeLine::Result]),
            Some(content) => match content_text(content) {
                Some(text) if !text.trim().is_empty() => Ok(vec![NativeLine::Person(text)]),
                Some(_) => Ok(vec![NativeLine::Skip]),
                None => Err(
                    "review: a user line carries content the importer does not cover".to_owned(),
                ),
            },
        }
    } else if kind == "assistant" {
        let content = value
            .get("message")
            .and_then(|message| message.get("content"));
        match content {
            None | Some(Value::Null) => Ok(vec![NativeLine::Skip]),
            Some(Value::Array(blocks)) => {
                let mut items = Vec::new();
                let mut texts = Vec::new();
                for block in blocks {
                    match str_field(block, "type").unwrap_or_default() {
                        "text" => {
                            if let Some(text) = str_field(block, "text") {
                                texts.push(text.to_owned());
                            }
                        }
                        "tool_use" => {
                            let name = str_field(block, "name").unwrap_or_default();
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            let (tool, arguments) = map_native_call(name, &input);
                            let text = texts.join("\n");
                            if !text.trim().is_empty() {
                                items.push(NativeLine::AssistantText(text));
                                texts.clear();
                            }
                            items.push(NativeLine::Call { tool, arguments });
                        }
                        "thinking" | "tool_result" | "tool_use_result" => {}
                        _ => {
                            return Err("review: an assistant line carries content the importer does not cover".to_owned());
                        }
                    }
                }
                let text = texts.join("\n");
                if !text.trim().is_empty() {
                    items.push(NativeLine::AssistantText(text));
                }
                if items.is_empty() {
                    items.push(NativeLine::Skip);
                }
                Ok(items)
            }
            Some(Value::String(text)) if !text.trim().is_empty() => {
                Ok(vec![NativeLine::AssistantText(text.clone())])
            }
            Some(_) => Ok(vec![NativeLine::Skip]),
        }
    } else if kind == "summary" {
        Ok(vec![NativeLine::Compaction])
    } else if value.get("message").is_some() {
        Err("review: a line carries conversation the importer does not cover".to_owned())
    } else {
        Ok(vec![NativeLine::Skip])
    }
}

/// Parses one native Claude Code project JSONL line into its line items.
pub(crate) fn parse_claude_line(line: &str) -> Result<Vec<NativeLine>, String> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| format!("review: bad native line: {error}"))?;
    classify_claude(&value)
}

/// Classifies one parsed pi session JSONL value.
fn classify_pi(value: &Value) -> Result<Vec<NativeLine>, String> {
    let kind = str_field(value, "type").unwrap_or_default();
    if kind == "compaction" {
        return Ok(vec![NativeLine::Compaction]);
    }
    if kind != "message" {
        if value.get("message").is_some() {
            return Err(
                "review: a line carries conversation the importer does not cover".to_owned(),
            );
        }
        return Ok(vec![NativeLine::Skip]);
    }
    let envelope = value.get("message").unwrap_or(&Value::Null);
    let role = str_field(envelope, "role").unwrap_or_default();
    let content = envelope.get("content");
    if role == "system" {
        return Ok(vec![NativeLine::Skip]);
    }
    if role == "toolResult" {
        return Ok(vec![NativeLine::Result]);
    }
    if role == "user" {
        return match content {
            None | Some(Value::Null) => Ok(vec![NativeLine::Skip]),
            Some(content) if has_result_block(content) => Ok(vec![NativeLine::Result]),
            Some(content) => match content_text(content) {
                Some(text) if !text.trim().is_empty() => Ok(vec![NativeLine::Person(text)]),
                Some(_) => Ok(vec![NativeLine::Skip]),
                None => Err(
                    "review: a user line carries content the importer does not cover".to_owned(),
                ),
            },
        };
    }
    if role == "assistant" {
        return match content {
            None | Some(Value::Null) => Ok(vec![NativeLine::Skip]),
            Some(Value::Array(blocks)) => {
                let mut items = Vec::new();
                let mut texts = Vec::new();
                for block in blocks {
                    match str_field(block, "type").unwrap_or_default() {
                        "text" => {
                            if let Some(text) = str_field(block, "text") {
                                texts.push(text.to_owned());
                            }
                        }
                        "toolCall" => {
                            let name = str_field(block, "name").unwrap_or_default();
                            let arguments = block.get("arguments").cloned().unwrap_or(Value::Null);
                            if name.is_empty() || !arguments.is_object() {
                                return Err(
                                    "review: a toolCall block names no tool call".to_owned()
                                );
                            }
                            let text = texts.join("\n");
                            if !text.trim().is_empty() {
                                items.push(NativeLine::AssistantText(text));
                                texts.clear();
                            }
                            items.push(NativeLine::Call {
                                tool: name.to_owned(),
                                arguments,
                            });
                        }
                        "thinking" => {}
                        _ => {
                            return Err("review: an assistant line carries content the importer does not cover".to_owned());
                        }
                    }
                }
                let text = texts.join("\n");
                if !text.trim().is_empty() {
                    items.push(NativeLine::AssistantText(text));
                }
                if items.is_empty() {
                    items.push(NativeLine::Skip);
                }
                Ok(items)
            }
            Some(Value::String(text)) if !text.trim().is_empty() => {
                Ok(vec![NativeLine::AssistantText(text.clone())])
            }
            Some(_) => Ok(vec![NativeLine::Skip]),
        };
    }
    Err("review: a message line names no role the importer covers".to_owned())
}

/// Parses one native pi session JSONL line into its line items.
pub(crate) fn parse_pi_line(line: &str) -> Result<Vec<NativeLine>, String> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| format!("review: bad native line: {error}"))?;
    classify_pi(&value)
}

/// The tool the per-tool table does not cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unsupported {
    /// The native tool's name.
    pub tool: String,
}

/// Resolves a native path against the raw workspace.
fn resolve_native(workspace: &str, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() || workspace.is_empty() {
        path.to_path_buf()
    } else {
        Path::new(workspace).join(path)
    }
}

/// Splits a shell command into elements on operators, without honouring
/// quoting: the approximation behind the importer's read-only verdict.
fn shell_elements(command: &str) -> Vec<String> {
    let mut elements = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    while let Some(cell) = chars.next() {
        if (cell == '&' || cell == '|' || cell == ';')
            && (cell != '&' || chars.peek() == Some(&'&'))
            && (cell != '|' || chars.peek() == Some(&'|'))
        {
            if cell == '&' || cell == '|' {
                chars.next();
            }
            if !current.trim().is_empty() {
                elements.push(current.trim().to_owned());
            }
            current = String::new();
        } else if cell == '\n' {
            if !current.trim().is_empty() {
                elements.push(current.trim().to_owned());
            }
            current = String::new();
        } else {
            current.push(cell);
        }
    }
    if !current.trim().is_empty() {
        elements.push(current.trim().to_owned());
    }
    elements
}

/// The words of one shell element.
fn shell_words(element: &str) -> Vec<String> {
    element.split_whitespace().map(str::to_owned).collect()
}

/// The commands the importer trusts as read-only: a conservative subset of
/// the shell's read-only list, so an unknown command declares executes and
/// stays reviewed, never the reverse.
fn is_read_only_command(words: &[String]) -> bool {
    let Some(first) = words.first() else {
        return true;
    };
    matches!(
        first.as_str(),
        "cat"
            | "ls"
            | "pwd"
            | "head"
            | "tail"
            | "wc"
            | "echo"
            | "printf"
            | "true"
            | "file"
            | "stat"
    )
}

/// The file arguments of a read-only element: the words past the flags.
fn read_only_paths(words: &[String]) -> Vec<String> {
    words
        .iter()
        .skip(1)
        .filter(|word| !word.starts_with('-'))
        .cloned()
        .collect()
}

/// Whether a `gh` element reads remotely.
fn is_gh_read(words: &[String]) -> bool {
    if !words.first().is_some_and(|first| first == "gh") {
        return false;
    }
    matches!(
        words.get(1).map(String::as_str),
        Some("repo" | "pr" | "issue" | "release" | "run" | "api" | "search" | "gist")
    )
}

/// The host of an `http(s)` URL, with its scheme prefix.
fn url_host_prefix(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    let scheme = url
        .split_once("://")
        .map(|(scheme, _)| scheme)
        .unwrap_or("https");
    Some(format!("{scheme}://{host}/"))
}

/// The importer's verdict for one shell command: reads with the file
/// arguments as paths when every element is read-only, network with paths
/// and hosts for a remote `gh` read, else executes with no paths and no
/// hosts.
fn shell_declared(
    command: &str,
    workdir: Option<&str>,
    workspace: &str,
    ctx: &ScrubCtx,
) -> (DeclaredEffects, Option<Vec<String>>) {
    let elements: Vec<Vec<String>> = shell_elements(command)
        .iter()
        .map(|element| shell_words(element))
        .collect();
    if elements.iter().all(|words| is_read_only_command(words)) {
        let mut paths = Vec::new();
        for words in &elements {
            for raw in read_only_paths(words) {
                let anchor = workdir.unwrap_or(workspace);
                paths.push(ctx.scrub_text(&resolve_native(anchor, &raw).to_string_lossy()));
            }
        }
        (
            DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: Some(paths),
                confined: false,
            },
            None,
        )
    } else if elements.iter().any(|words| is_gh_read(words)) {
        let mut paths = Vec::new();
        for words in &elements {
            if is_gh_read(words) {
                for raw in read_only_paths(words).iter().skip(2) {
                    let anchor = workdir.unwrap_or(workspace);
                    paths.push(ctx.scrub_text(&resolve_native(anchor, raw).to_string_lossy()));
                }
            }
        }
        (
            DeclaredEffects {
                effects: vec![Effect::Network],
                reversible: true,
                paths: Some(paths),
                confined: false,
            },
            Some(vec!["https://api.github.com/".to_owned()]),
        )
    } else {
        (
            DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: false,
                paths: None,
                confined: false,
            },
            None,
        )
    }
}

/// One native path argument, resolved against the raw workspace.
fn native_path(arguments: &Value, workspace: &str, ctx: &ScrubCtx) -> Result<String, Unsupported> {
    let raw = str_field(arguments, "path").ok_or(Unsupported {
        tool: String::new(),
    })?;
    Ok(ctx.scrub_text(&resolve_native(workspace, raw).to_string_lossy()))
}

/// Maps a supported native tool and its arguments to the declared effects
/// the set line stores, derived from each tool's effects function: reads
/// declare reads, writes declare writes with reversibility from the target
/// at import time, edits declare irreversible writes, shells follow the
/// classifier verdict, web fetches declare network with the URL's host, and
/// web searches declare network with no hosts. The `exists` answer is
/// `Some(true)` when the write target exists, `Some(false)` when it is
/// absent, and `None` when it cannot be stat'd. Anything else is dropped
/// with the count.
pub(crate) fn declared_for(
    tool: &str,
    arguments: &Value,
    workspace: &str,
    exists: &dyn Fn(&Path) -> Option<bool>,
    ctx: &ScrubCtx,
) -> Result<(DeclaredEffects, Option<Vec<String>>), Unsupported> {
    let unsupported = |tool: &str| Unsupported {
        tool: tool.to_owned(),
    };
    if tool == "read" {
        let Some(raw) = str_field(arguments, "path") else {
            return Err(unsupported(tool));
        };
        return Ok((
            DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: Some(vec![
                    ctx.scrub_text(&resolve_native(workspace, raw).to_string_lossy()),
                ]),
                confined: false,
            },
            None,
        ));
    }
    if tool == "write" {
        let Some(raw) = str_field(arguments, "path") else {
            return Err(unsupported(tool));
        };
        if !arguments
            .get("content")
            .is_some_and(|content| content.is_string())
        {
            return Err(unsupported(tool));
        }
        let resolved = resolve_native(workspace, raw);
        let reversible = !exists(&resolved).unwrap_or(true);
        return Ok((
            DeclaredEffects {
                effects: vec![Effect::Writes],
                reversible,
                paths: Some(vec![ctx.scrub_text(&resolved.to_string_lossy())]),
                confined: false,
            },
            None,
        ));
    }
    if tool == "edit" {
        let path = native_path(arguments, workspace, ctx).map_err(|_| unsupported(tool))?;
        if !arguments.get("edits").is_some_and(|edits| edits.is_array()) {
            return Err(unsupported(tool));
        }
        return Ok((
            DeclaredEffects {
                effects: vec![Effect::Writes],
                reversible: false,
                paths: Some(vec![path]),
                confined: false,
            },
            None,
        ));
    }
    if tool == "shell" || tool == "bash" {
        let Some(command) = str_field(arguments, "command") else {
            return Ok((
                DeclaredEffects {
                    effects: vec![Effect::Executes],
                    reversible: false,
                    paths: None,
                    confined: false,
                },
                None,
            ));
        };
        let workdir = str_field(arguments, "workdir");
        return Ok(shell_declared(command, workdir, workspace, ctx));
    }
    if tool == "web_fetch" {
        let Some(url) = str_field(arguments, "url") else {
            return Err(unsupported(tool));
        };
        let Some(host) = url_host_prefix(url) else {
            return Err(unsupported(tool));
        };
        return Ok((
            DeclaredEffects {
                effects: vec![Effect::Network],
                reversible: false,
                paths: None,
                confined: false,
            },
            Some(vec![host]),
        ));
    }
    if tool == "web_search" {
        return Ok((
            DeclaredEffects {
                effects: vec![Effect::Network],
                reversible: true,
                paths: None,
                confined: false,
            },
            Some(Vec::new()),
        ));
    }
    Err(unsupported(tool))
}

/// The closed project allowlist: Fiber, its worktrees and design branches,
/// and the tooling projects the set is drawn from. Nothing else is sampled.
pub(crate) struct Allowlist;

impl Allowlist {
    /// Whether a project name or workspace path may enter the set.
    pub(crate) fn check(candidate: &str) -> bool {
        if candidate.is_empty() {
            return false;
        }
        if matches!(
            candidate,
            "fiber"
                | "fiber-design"
                | "fiber-zig"
                | "switchyard"
                | "delegate"
                | "QuotaBar"
                | "pi-extensions"
        ) {
            return true;
        }
        candidate.split(['/', '\\']).any(|segment| {
            segment == "fiber"
                || segment.starts_with("fiber-")
                || segment.starts_with("fiber_")
                || matches!(
                    segment,
                    "fiber-design"
                        | "fiber-zig"
                        | "switchyard"
                        | "delegate"
                        | "QuotaBar"
                        | "pi-extensions"
                )
        })
    }

    /// Rejects a history chain with any segment outside the allowlist,
    /// naming the first outside workspace.
    pub(crate) fn check_chain(workspaces: &[String]) -> Result<(), String> {
        for workspace in workspaces {
            if !Self::check(workspace) {
                return Err(format!(
                    "review: {workspace} is outside the sampled projects"
                ));
            }
        }
        Ok(())
    }
}

/// The mapped parts one label-input entry is built from.
pub(crate) struct ForeignInput<'a> {
    /// The entry's synthetic id.
    pub id: &'a str,
    /// The native tool's name.
    pub tool: &'a str,
    /// The call's raw arguments.
    pub arguments: &'a Value,
    /// The table's declared effects.
    pub declared: &'a DeclaredEffects,
    /// The table's hosts, where it names them.
    pub hosts: Option<Vec<String>>,
    /// The running `reviewed[]` snapshot ending with the entry's call.
    pub reviewed: Vec<ReviewedItem>,
    /// The raw workspace.
    pub workspace: &'a str,
    /// The raw added folders in first-event order.
    pub added_folders: &'a [String],
    /// The entry's provenance.
    pub source: Source,
    /// The scrub context.
    pub ctx: &'a ScrubCtx,
}

/// Builds one label-input entry from its mapped parts, scrubbing every
/// value through the context.
pub(crate) fn foreign_to_label_input(input: ForeignInput<'_>) -> SetLine {
    let ForeignInput {
        id,
        tool,
        arguments,
        declared,
        hosts,
        reviewed,
        workspace,
        added_folders,
        source,
        ctx,
    } = input;
    SetLine {
        id: id.to_owned(),
        source,
        tool: tool.to_owned(),
        arguments: ctx.scrub_value(arguments),
        declared: DeclaredEffects {
            effects: declared.effects.clone(),
            reversible: declared.reversible,
            paths: declared
                .paths
                .clone()
                .map(|paths| paths.iter().map(|path| ctx.scrub_text(path)).collect()),
            confined: declared.confined,
        },
        hosts,
        workspace: ctx.scrub_text(workspace),
        added_folders: added_folders
            .iter()
            .map(|folder| ctx.scrub_text(folder))
            .collect(),
        reviewed: reviewed
            .iter()
            .map(|item| ReviewedItem {
                kind: item.kind,
                seq: item.seq,
                item: item.item,
                action: item.action.clone(),
                text: ctx.scrub_text(&item.text),
            })
            .collect(),
        labels: Labels {
            family1: None,
            family2: None,
        },
    }
}

/// One native session file's running fold: the `reviewed[]` prefix, the
/// pending assistant text, and the added folders in first-event order.
struct FileFold {
    /// The `reviewed[]` prefix as of the last cut.
    reviewed: Vec<ReviewedItem>,
    /// The held reply awaiting the person's next message.
    pending: Vec<(u64, String)>,
    /// The added folders in first-event order as of the last cut.
    added_folders: Vec<String>,
}

impl FileFold {
    /// Pushes a person item, attaching the pending reply it answers.
    fn person(&mut self, seq: u64, text: &str, ctx: &ScrubCtx) {
        if let Some((reply_seq, reply)) = take_pending(&mut self.pending) {
            self.reviewed.push(ReviewedItem {
                kind: ReviewedKind::Reply,
                seq: Some(reply_seq),
                item: None,
                action: None,
                text: ctx.scrub_text(&format!("The agent: {reply}")),
            });
        }
        self.reviewed.push(ReviewedItem {
            kind: ReviewedKind::Person,
            seq: Some(seq),
            item: Some(0),
            action: None,
            text: ctx.scrub_text(text),
        });
    }

    /// Pushes one effectful call item, dropping the pending reply the call
    /// breaks, and returns the entry's `reviewed[]` snapshot.
    fn call(&mut self, seq: u64, id: &str, tool: &str, arguments: &Value) -> Vec<ReviewedItem> {
        self.pending.clear();
        let rendered = serde_json::to_string(&serde_json::json!({
            "tool": tool,
            "arguments": arguments,
        }))
        .unwrap_or_default();
        self.reviewed.push(ReviewedItem {
            kind: ReviewedKind::Call,
            seq: Some(seq),
            item: None,
            action: Some(id.to_owned()),
            text: format!("Tool call: {rendered}"),
        });
        self.reviewed.clone()
    }

    /// Acts as a handoff: keeps every earlier person message, each with the
    /// reply it answered, and drops every call. A held reply with no person
    /// message after it answers nothing, so it goes with the calls.
    fn compact(&mut self) {
        let mut kept = Vec::new();
        let mut at = 0;
        while at < self.reviewed.len() {
            let reply = self.reviewed.get(at).is_some_and(|item| {
                matches!(item.kind, ReviewedKind::Reply)
                    && self
                        .reviewed
                        .get(at + 1)
                        .is_some_and(|next| matches!(next.kind, ReviewedKind::Person))
            });
            let person = self
                .reviewed
                .get(at)
                .is_some_and(|item| matches!(item.kind, ReviewedKind::Person));
            if reply || person {
                kept.push(self.reviewed[at].clone());
            }
            at += 1;
        }
        self.reviewed = kept;
        self.pending.clear();
    }
}

/// Takes the pending reply parts as one joined text with its first seq.
fn take_pending(pending: &mut Vec<(u64, String)>) -> Option<(u64, String)> {
    if pending.is_empty() {
        return None;
    }
    let seq = pending.first().map(|(seq, _)| *seq).unwrap_or_default();
    let text: Vec<String> = pending.iter().map(|(_, part)| part.clone()).collect();
    pending.clear();
    let joined = text.join("\n");
    if joined.trim().is_empty() {
        return None;
    }
    Some((seq, joined))
}

/// What folding one native session file keeps: its label-input entries
/// and its per-tool unsupported-call counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FoldedFile {
    /// The label-input entries in file order.
    pub entries: Vec<SetLine>,
    /// One count per unsupported native tool, in first-drop order.
    pub dropped: Vec<(String, u64)>,
    /// The raw workspace the file names, for the allowlist check.
    pub workspace_raw: String,
}

/// Folds one native session file's lines in file order into label-input
/// entries over one running conversation state. File order is turn order:
/// entry N+1 starts from entry N's full prefix, and a compaction marker
/// resets the prefix to every earlier person message with the replies they
/// answered. A line carrying conversation the pinned shapes do not cover
/// is a hard error naming the file and line number.
fn fold_native_file(
    content: &str,
    file: &str,
    session: &str,
    classify: &dyn Fn(&Value) -> Result<Vec<NativeLine>, String>,
    ctx: &ScrubCtx,
    exists: &dyn Fn(&Path) -> Option<bool>,
) -> Result<FoldedFile, String> {
    let mut values = Vec::new();
    for (at, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|error| format!("review: {file}:{}: bad native line: {error}", at + 1))?;
        values.push((at as u64 + 1, value));
    }
    let mut workspace: Option<String> = None;
    for (_, value) in &values {
        if let Some(cwd) = str_field(value, "cwd") {
            workspace = Some(cwd.to_owned());
            break;
        }
    }
    let workspace = workspace.unwrap_or_else(|| session.to_owned());
    let mut fold = FileFold {
        reviewed: Vec::new(),
        pending: Vec::new(),
        added_folders: Vec::new(),
    };
    let mut folded = FoldedFile::default();
    for (seq, value) in &values {
        if let Some(folders) = value.get("added_folders").and_then(Value::as_array) {
            for folder in folders {
                if let Some(folder) = folder.as_str() {
                    fold.added_folders.push(ctx.scrub_text(folder));
                }
            }
        }
        let items =
            classify(value).map_err(|problem| format!("review: {file}:{seq}: {problem}"))?;
        for line in items {
            match line {
                NativeLine::Person(text) => fold.person(*seq, &text, ctx),
                NativeLine::AssistantText(text) => {
                    if !text.trim().is_empty() {
                        fold.pending.push((*seq, text));
                    }
                }
                NativeLine::Call { tool, arguments } => {
                    let id = format!("{session}-{seq:04}");
                    let snapshot = fold.call(*seq, &id, &tool, &arguments);
                    match declared_for(&tool, &arguments, &workspace, exists, ctx) {
                        Ok((declared, hosts)) => {
                            folded.entries.push(foreign_to_label_input(ForeignInput {
                                id: &id,
                                tool: &tool,
                                arguments: &arguments,
                                declared: &declared,
                                hosts,
                                reviewed: snapshot,
                                workspace: &workspace,
                                added_folders: &fold.added_folders,
                                source: Source {
                                    kind: SourceKind::Sampled,
                                    session: session.to_owned(),
                                    seq: Some(*seq),
                                },
                                ctx,
                            }));
                        }
                        Err(unsupported) => {
                            let at = folded
                                .dropped
                                .iter()
                                .position(|(name, _)| *name == unsupported.tool);
                            match at {
                                Some(at) => folded.dropped[at].1 += 1,
                                None => folded.dropped.push((unsupported.tool, 1)),
                            }
                        }
                    }
                }
                NativeLine::Result => {}
                NativeLine::Compaction => fold.compact(),
                NativeLine::Skip => {}
            }
        }
    }
    folded.workspace_raw = workspace.clone();
    Ok(folded)
}

/// Folds one native Claude Code project JSONL file into label-input entries.
pub(crate) fn parse_claude_file(
    content: &str,
    file: &str,
    session: &str,
    ctx: &ScrubCtx,
    exists: &dyn Fn(&Path) -> Option<bool>,
) -> Result<FoldedFile, String> {
    fold_native_file(content, file, session, &classify_claude, ctx, exists)
}

/// Folds one native pi session JSONL file into label-input entries.
pub(crate) fn parse_pi_file(
    content: &str,
    file: &str,
    session: &str,
    ctx: &ScrubCtx,
    exists: &dyn Fn(&Path) -> Option<bool>,
) -> Result<FoldedFile, String> {
    fold_native_file(content, file, session, &classify_pi, ctx, exists)
}
