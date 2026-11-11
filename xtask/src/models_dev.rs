//! `cargo xtask models-dev`: regenerates the `models` lists of the
//! first-party provider files from models.dev, keeping every Fiber-only
//! field in the per-package table (`docs/model-routing.md`, "What a
//! provider extension declares").

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use crate::models_dev_table::{PACKAGES, Package, ProtocolRule};

/// The models.dev catalog the generator reads when `--from` is absent.
const MODELS_DEV_URL: &str = "https://models.dev/api.json";
/// The checked-in, trimmed models.dev catalog: the drift test's input.
const SNAPSHOT: &str = "xtask/tests/models-dev.json";

/// One file the generator writes: its path from the repository root, its
/// full text, and how many models it lists.
pub(crate) struct Output {
    /// The file's path from the repository root.
    pub(crate) path: &'static str,
    /// The file's full text, pretty JSON plus a trailing newline.
    pub(crate) text: String,
    /// How many models the file lists.
    pub(crate) models: usize,
}

/// Every package's file from a catalog, with one line per left-out model.
pub(crate) type Generated = (Vec<Output>, Vec<String>);

/// The model keys `generate` reads, kept verbatim.
const MODEL_KEYS: [&str; 7] = [
    "cost",
    "limit",
    "modalities",
    "name",
    "provider",
    "status",
    "tool_call",
];

/// `value` with only `keys` kept, verbatim.
fn keep_keys(value: &Value, keys: &[&str]) -> Map<String, Value> {
    let mut kept = Map::new();
    for key in keys {
        if let Some(field) = value.get(*key) {
            kept.insert((*key).to_owned(), field.clone());
        }
    }
    kept
}

/// A cost tier with only the fields `generate` reads.
fn trim_tier(tier: &Value) -> Value {
    let mut kept = keep_keys(tier, &["cache_read", "cache_write", "input", "output"]);
    if let Some(detail) = tier.get("tier") {
        kept.insert(
            "tier".to_owned(),
            Value::Object(keep_keys(detail, &["size", "type"])),
        );
    }
    Value::Object(kept)
}

/// A `cost` object with only the fields `generate` reads.
fn trim_cost(cost: &Value) -> Value {
    let mut kept = keep_keys(cost, &["cache_read", "cache_write", "input", "output"]);
    if let Some(tiers) = cost.get("tiers").and_then(Value::as_array) {
        kept.insert(
            "tiers".to_owned(),
            Value::Array(tiers.iter().map(trim_tier).collect()),
        );
    }
    Value::Object(kept)
}

/// A source model with only the fields `generate` reads, kept verbatim.
fn trim_model(model: &Value) -> Value {
    let mut kept = Map::new();
    for key in MODEL_KEYS {
        if let Some(field) = model.get(key) {
            let trimmed = match key {
                "cost" => trim_cost(field),
                "limit" => Value::Object(keep_keys(field, &["context", "output"])),
                "modalities" => Value::Object(keep_keys(field, &["input", "output"])),
                "provider" => Value::Object(keep_keys(field, &["npm"])),
                _ => field.clone(),
            };
            kept.insert(key.to_owned(), trimmed);
        }
    }
    Value::Object(kept)
}

/// Each package's source key and, per model, only the fields `generate`
/// reads.
/// Err when a source key is missing or not an object.
pub(crate) fn trim(catalog: &Value) -> Result<Value, String> {
    let mut trimmed = Map::new();
    for package in &PACKAGES {
        let source = package.source;
        let provider = catalog
            .get(source)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("models.dev has no `{source}` provider"))?;
        let models = match provider.get("models") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(models)) => models.clone(),
            Some(_) => return Err(format!("models.dev has no `{source}` provider")),
        };
        let mut kept = Map::new();
        for (id, model) in &models {
            kept.insert(id.clone(), trim_model(model));
        }
        let mut provider = Map::new();
        provider.insert("models".to_owned(), Value::Object(kept));
        trimmed.insert(source.to_owned(), Value::Object(provider));
    }
    Ok(Value::Object(trimmed))
}

/// Whether a source model calls tools and outputs text, as pi keeps them:
/// `tool_call` must be exactly true, and `modalities.output` must list
/// `"text"`, with an absent `modalities` counting as no text.
fn callable(model: &Value) -> bool {
    model.get("tool_call").and_then(Value::as_bool) == Some(true)
        && model
            .get("modalities")
            .and_then(|modalities| modalities.get("output"))
            .and_then(Value::as_array)
            .is_some_and(|output| output.iter().any(|kind| kind.as_str() == Some("text")))
}

/// Whether a deprecated source model is left out of this package.
fn deprecated(package: &Package, model: &Value) -> bool {
    package.drop_deprecated && model.get("status").and_then(Value::as_str) == Some("deprecated")
}

/// The protocol models.dev's `provider.npm` field maps to, as pi maps it.
fn npm_protocol(model: &Value) -> &'static str {
    match model
        .get("provider")
        .and_then(|provider| provider.get("npm"))
        .and_then(Value::as_str)
    {
        Some("@ai-sdk/openai") => "openai-responses",
        Some("@ai-sdk/anthropic") => "anthropic-messages",
        Some("@ai-sdk/google") => "google-generative-ai",
        Some(_) | None => "openai-completions",
    }
}

/// The major version after `gemini-`, if the id names one.
fn gemini_major(id: &str) -> Option<u64> {
    id.strip_prefix("gemini-").and_then(|rest| {
        rest.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u64>()
            .ok()
    })
}

/// Whether a model is left out by the Gemini rule: on
/// `google-generative-ai`, a `gemini-<N>...` id with N below 3 stays out,
/// while an id with no major version, such as `gemini-flash-latest`, stays.
fn pre3_gemini(protocol: &str, id: &str) -> bool {
    protocol == "google-generative-ai" && gemini_major(id).is_some_and(|major| major < 3)
}

/// The model's protocol: the package rule, replaced by its override.
fn protocol_of(package: &Package, model: &Value, id: &str) -> &'static str {
    let ruled = match package.protocol {
        ProtocolRule::Fixed(protocol) => protocol,
        ProtocolRule::ByNpm => npm_protocol(model),
    };
    package
        .protocol_overrides
        .iter()
        .find(|entry| entry.0 == id)
        .map_or(ruled, |entry| entry.1)
}

/// Whether the model's input lists an image.
fn has_image(model: &Value) -> bool {
    model
        .get("modalities")
        .and_then(|modalities| modalities.get("input"))
        .and_then(Value::as_array)
        .is_some_and(|input| input.iter().any(|kind| kind.as_str() == Some("image")))
}

/// One cost rate: the tier's, falling back to the base rate, then to 0.
fn rate(tier: &Value, base: &Map<String, Value>, key: &str) -> Value {
    tier.get(key)
        .or_else(|| base.get(key))
        .cloned()
        .unwrap_or(Value::from(0))
}

/// A context tier as written: the tier's rates over the base ones, with
/// the size as `input_tokens_above`.
fn write_tier(tier: &Value, base: &Map<String, Value>, size: Value) -> Value {
    let mut written = Map::new();
    for key in ["cache_read", "cache_write", "input", "output"] {
        written.insert(key.to_owned(), rate(tier, base, key));
    }
    written.insert("input_tokens_above".to_owned(), size);
    Value::Object(written)
}

/// A model's derived `cost`, copying models.dev's numbers as they are
/// spelled. None when neither `cost.input` nor `cost.output` is present;
/// a missing one of the two is 0, cache prices are written only when
/// listed, and only `context` tiers with a size are kept.
fn derived_cost(model: &Value) -> Option<Value> {
    let cost = model.get("cost")?;
    let input = cost.get("input").cloned();
    let output = cost.get("output").cloned();
    if input.is_none() && output.is_none() {
        return None;
    }
    let mut derived = Map::new();
    for key in ["cache_read", "cache_write"] {
        if let Some(price) = cost.get(key) {
            derived.insert(key.to_owned(), price.clone());
        }
    }
    derived.insert("input".to_owned(), input.unwrap_or(Value::from(0)));
    derived.insert("output".to_owned(), output.unwrap_or(Value::from(0)));
    if let Some(tiers) = cost.get("tiers").and_then(Value::as_array) {
        let mut written = Vec::new();
        for tier in tiers {
            let context = tier.get("tier");
            let is_context = context
                .and_then(|tier| tier.get("type"))
                .and_then(Value::as_str)
                == Some("context");
            if !is_context || context.and_then(|tier| tier.get("size")).is_none() {
                continue;
            }
            let size = context
                .and_then(|tier| tier.get("size"))
                .cloned()
                .unwrap_or(Value::from(0));
            written.push(write_tier(tier, &derived, size));
        }
        if !written.is_empty() {
            derived.insert("tiers".to_owned(), Value::Array(written));
        }
    }
    Some(Value::Object(derived))
}

/// A table layer parsed as an object; Err naming the package when it is
/// not one.
fn layer(package: &Package, text: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(layer)) => Ok(layer),
        _ => Err(format!(
            "{}: a table layer is not a JSON object",
            stem(package)
        )),
    }
}

/// The file stem of the package's path: `opencode-zen` of
/// `providers/opencode/providers/opencode-zen.json`.
fn stem(package: &Package) -> &str {
    let file = package.path.rsplit('/').next().unwrap_or(package.path);
    file.strip_suffix(".json").unwrap_or(file)
}

/// Why a source model is left out, if it is: the filter, the skip list,
/// the Gemini rule or a missing context window, in that order.
fn leave_out_line(package: &Package, protocol: &str, id: &str, model: &Value) -> Option<String> {
    if !callable(model) {
        return Some(format!(
            "models-dev: {}/{id} does not call tools with text output in models.dev; left out",
            stem(package)
        ));
    }
    if deprecated(package, model) {
        return Some(format!(
            "models-dev: {}/{id} is deprecated in models.dev; left out",
            stem(package)
        ));
    }
    if package.skip.contains(&id) {
        return Some(format!(
            "models-dev: {}/{id} is skipped by its package table; left out",
            stem(package)
        ));
    }
    if pre3_gemini(protocol, id) {
        return Some(format!(
            "models-dev: {}/{id} is a pre-3 Gemini model; left out",
            stem(package)
        ));
    }
    let context = model
        .get("limit")
        .and_then(|limit| limit.get("context"))
        .and_then(Value::as_u64);
    match context {
        None | Some(0) => Some(format!(
            "models-dev: {}/{id} has no context window in models.dev; left out",
            stem(package)
        )),
        Some(_) => None,
    }
}

/// Whether the package is a gateway (`opencode-go`, `opencode-zen`):
/// its models inherit a vendor package's thinking row by exact model id.
fn is_gateway(package: &Package) -> bool {
    package.path.ends_with("opencode-go.json") || package.path.ends_with("opencode-zen.json")
}

/// Whether the package is a direct vendor (`anthropic`, `openai`,
/// `gemini`, `muse`) whose thinking rows a gateway inherits: the
/// subscription-specific `codex` rows carry Codex-backend defaults that do
/// not hold on a gateway route, so they never apply there.
fn is_thinking_vendor(package: &Package) -> bool {
    ["anthropic.json", "gemini.json", "openai.json", "muse.json"]
        .iter()
        .any(|file| package.path.ends_with(file))
}

/// The thinking row for `id`: the package's own row, else a direct-vendor
/// package's row naming the same id exactly, on a gateway package only. A
/// gateway's own row beats the vendor's row for the same id.
fn thinking_row<'a>(
    packages: &'a [Package],
    package: &'a Package,
    id: &str,
) -> Option<(&'a [&'a str], Option<&'a str>)> {
    if let Some(entry) = package.thinking.iter().find(|entry| entry.0 == id) {
        return Some((entry.1, entry.2));
    }
    if !is_gateway(package) {
        return None;
    }
    packages
        .iter()
        .filter(|vendor| is_thinking_vendor(vendor))
        .find_map(|vendor| {
            vendor
                .thinking
                .iter()
                .find(|entry| entry.0 == id)
                .map(|entry| (entry.1, entry.2))
        })
}

/// The generated Databricks thinking module, from the repository root:
/// `cargo xtask models-dev` writes it beside the provider files.
pub(crate) const DATABRICKS_THINKING: &str = "providers/databricks/thinking.lua";

/// The match key of a Databricks service or catalog name, mirroring Lua
/// `model_key` in `providers/databricks/providers/databricks.lua`:
/// lower-cased, each vendor prefix stripped once in order, the part after
/// the last `/`, runs outside `a-z0-9` as one `-`, one trailing `-maas`,
/// `-preview` or `-it` dropped, and one leading and trailing `-` trimmed.
pub(crate) fn databricks_model_key(raw: &str) -> String {
    let mut value = raw.to_ascii_lowercase();
    for prefix in [
        "model-services/system.ai.",
        "system.ai.",
        "databricks-",
        "fw-",
        "meta-",
    ] {
        if let Some(rest) = value.strip_prefix(prefix) {
            value = rest.to_owned();
        }
    }
    for group in [
        ["global.", "us.", "eu.", "apac."].as_slice(),
        ["xai.", "anthropic.", "openai.", "google."].as_slice(),
    ] {
        for prefix in group {
            if let Some(rest) = value.strip_prefix(prefix) {
                value = rest.to_owned();
                break;
            }
        }
    }
    if let Some(tail) = value.rsplit('/').next() {
        value = tail.to_owned();
    }
    let mut squashed = String::with_capacity(value.len());
    let mut gap = false;
    for byte in value.bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            squashed.push(byte as char);
            gap = false;
        } else if !gap {
            squashed.push('-');
            gap = true;
        }
    }
    if let Some(cut) = squashed.rfind('-') {
        let stem = &squashed[cut + 1..];
        if !stem.is_empty()
            && stem.bytes().all(|byte| byte.is_ascii_alphabetic())
            && matches!(stem, "maas" | "preview" | "it")
        {
            squashed.truncate(cut);
        }
    }
    let trimmed = squashed.strip_prefix('-').unwrap_or(&squashed);
    trimmed.strip_suffix('-').unwrap_or(trimmed).to_owned()
}

/// One row of the Databricks thinking table: the match key with its
/// vendor levels and default.
struct ThinkingRow<'a> {
    /// The vendor model id the row comes from, naming the source.
    id: &'a str,
    /// The vendor row's levels, in vocabulary order.
    levels: &'a [&'a str],
    /// The vendor row's default, if it names one.
    default: Option<&'a str>,
}

/// Every thinking level name a table may use, in vocabulary order
/// (`xtask/src/models_dev_table.rs:51`).
const THINKING_VOCABULARY: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The hand-maintained Databricks thinking overlay, from the repository
/// root: compiled in, never read at run time.
const DATABRICKS_THINKING_OVERLAY: &str = "providers/databricks/thinking-overlay.json";

/// One validated overlay row: the file key with its levels and default.
pub(crate) struct OverlayRow {
    /// The file key, naming the row in errors.
    key: String,
    /// The row's levels, in vocabulary order.
    levels: Vec<&'static str>,
    /// The row's default, when it names one.
    default: Option<&'static str>,
}

/// Parses overlay `text` from `source`: each row holds `levels` and an
/// optional `default`. Every level is a vocabulary name, a default sits
/// inside its levels, levels are never empty, and each key normalizes
/// with `databricks_model_key`. A normalized-key collision keeps the
/// first row when both rows match and fails naming both file keys when
/// they differ; a `gpt-5` or `gpt-5-*` key fails, as those rows keep the
/// openai package's. A syntax error has no key to name, so it reports
/// the file and the parse location instead.
fn parse_thinking_overlay(source: &str, text: &str) -> Result<Vec<OverlayRow>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("databricks thinking overlay {source}: {error}"))?;
    let Some(rows) = value.as_object() else {
        return Err(format!(
            "databricks thinking overlay {source}: top level must be an object"
        ));
    };
    let mut overlay = Vec::new();
    let mut seen: std::collections::BTreeMap<String, (String, Vec<&str>, Option<&str>)> =
        std::collections::BTreeMap::new();
    for (raw, row) in rows {
        let key = databricks_model_key(raw);
        if key == "gpt-5" || key.starts_with("gpt-5-") {
            return Err(format!(
                "databricks thinking overlay: `{raw}` names a gpt-5.x model, which keeps the openai package's row"
            ));
        }
        let Some(levels) = row.get("levels").and_then(Value::as_array) else {
            return Err(format!(
                "databricks thinking overlay: `{raw}` names no levels array"
            ));
        };
        let mut kept: Vec<&'static str> = Vec::with_capacity(levels.len());
        for level in levels {
            let Some(level) = level.as_str() else {
                return Err(format!(
                    "databricks thinking overlay: `{raw}` names a level that is no string"
                ));
            };
            let Some(canonical) = THINKING_VOCABULARY.iter().find(|name| **name == level) else {
                return Err(format!(
                    "databricks thinking overlay: `{raw}` names unknown level `{level}`"
                ));
            };
            kept.push(canonical);
        }
        if kept.is_empty() {
            return Err(format!(
                "databricks thinking overlay: `{raw}` names no levels"
            ));
        }
        let default = match row.get("default") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let Some(default) = value.as_str() else {
                    return Err(format!(
                        "databricks thinking overlay: `{raw}` names a default that is no string"
                    ));
                };
                let Some(canonical) = kept.iter().find(|name| **name == default) else {
                    return Err(format!(
                        "databricks thinking overlay: `{raw}` names default `{default}` outside its levels"
                    ));
                };
                Some(*canonical)
            }
        };
        if let Some((first, first_levels, first_default)) = seen.get(&key) {
            if *first_levels != kept || *first_default != default {
                return Err(format!(
                    "databricks thinking overlay: `{first}` and `{raw}` share a match key with differing thinking rows"
                ));
            }
            continue;
        }
        seen.insert(key, (raw.clone(), kept.clone(), default));
        overlay.push(OverlayRow {
            key: raw.clone(),
            levels: kept,
            default,
        });
    }
    Ok(overlay)
}

/// The `thinking.lua` module for the databricks package from `packages`'
/// thinking rows: one row per match key (`databricks_model_key`) of the
/// four thinking-vendor packages (`is_thinking_vendor`), so a discovered
/// model takes the row of the vendor model it serves. `codex` rows never
/// feed it, as their defaults do not hold on a gateway route. Keys are
/// sorted for stable bytes. A key two ids share with differing rows is
/// an error naming both ids, as silent wrong levels would be worse.
pub(crate) fn databricks_thinking(packages: &[Package]) -> Result<Output, String> {
    let overlay = parse_thinking_overlay(
        DATABRICKS_THINKING_OVERLAY,
        include_str!("../../providers/databricks/thinking-overlay.json"),
    )?;
    merge_databricks_thinking(packages, &overlay)
}

/// `databricks_thinking` over `packages` with `overlay` applied after the
/// vendor-vs-vendor check: the overlay wins on a shared key, with no
/// error. Unit tests call this directly with an explicit overlay, so no
/// vendor-behavior test pins the compiled-in rows.
pub(crate) fn merge_databricks_thinking(
    packages: &[Package],
    overlay: &[OverlayRow],
) -> Result<Output, String> {
    let mut rows: std::collections::BTreeMap<String, ThinkingRow<'_>> =
        std::collections::BTreeMap::new();
    for package in packages
        .iter()
        .filter(|package| is_thinking_vendor(package))
    {
        for (id, levels, default) in package.thinking {
            let key = databricks_model_key(id);
            if let Some(kept) = rows.get(&key) {
                if kept.levels != *levels || kept.default != *default {
                    return Err(format!(
                        "databricks thinking: `{}` and `{id}` share a match key with differing thinking rows",
                        kept.id
                    ));
                }
                continue;
            }
            rows.insert(
                key,
                ThinkingRow {
                    id,
                    levels,
                    default: *default,
                },
            );
        }
    }
    for row in overlay {
        rows.insert(
            databricks_model_key(&row.key),
            ThinkingRow {
                id: row.key.as_str(),
                levels: &row.levels,
                default: row.default,
            },
        );
    }
    let mut text = String::from(
        "-- Never hand-edit: `cargo xtask models-dev` regenerates this module\n\
         -- from the thinking rows of the `anthropic`, `gemini`, `openai`\n\
         -- and `muse` packages (`docs/model-routing.md`, \"What a provider\n\
         -- extension declares\"), overlaid by\n\
         -- `providers/databricks/thinking-overlay.json`, which wins on a\n\
         -- shared key. A discovered `databricks` model takes the row its\n\
         -- bare service name's match key names; a service with no row keeps\n\
         -- no levels.\n\
         return {\n",
    );
    for (key, row) in &rows {
        let mut levels = String::new();
        for level in row.levels {
            if !levels.is_empty() {
                levels.push_str(", ");
            }
            levels.push_str(&format!("\"{level}\""));
        }
        text.push_str(&format!("  [\"{key}\"] = {{ levels = {{ {levels} }}"));
        if let Some(default) = row.default {
            text.push_str(&format!(", default = \"{default}\""));
        }
        text.push_str(" },\n");
    }
    text.push_str("}\n");
    Ok(Output {
        path: DATABRICKS_THINKING,
        text,
        models: rows.len(),
    })
}
/// The cited corrections file, from the repository root: each entry
/// names a package, a model and the derived fields it overrides, with
/// the vendor page that says so (`source`) and the models.dev pull
/// request fixing it upstream (`upstream`). The generator applies the
/// entries after the per-package table and deletes an entry once
/// models.dev agrees with it (`docs/model-routing.md`, "What a provider
/// extension declares").
pub(crate) const CORRECTIONS: &str = "providers/models-corrections.toml";

/// One entry of the corrections file.
#[derive(Clone, Debug)]
pub(crate) struct Correction {
    /// The package file's stem, such as `openai`, or its full path.
    pub(crate) package: String,
    /// The model id the entry overrides.
    pub(crate) model: String,
    /// The derived fields the entry overrides.
    pub(crate) fields: Map<String, Value>,
    /// The vendor page that says so.
    pub(crate) source: String,
    /// The models.dev pull request fixing it upstream.
    pub(crate) upstream: String,
}

/// The derived model-level fields a correction may override: every
/// other model key comes from the per-package table alone.
const OVERRIDABLE_FIELDS: [&str; 6] = [
    "name",
    "protocol",
    "context_window",
    "max_output_tokens",
    "input",
    "cost",
];

/// The model keys a correction must never name: the table owns them,
/// with `id` as the entry's own key.
const TABLE_ONLY_FIELDS: [&str; 6] = [
    "id",
    "thinking_levels",
    "thinking_default",
    "compat",
    "base_url",
    "web_search",
];

/// One corrections entry before its required keys are checked.
#[derive(Default)]
struct RawEntry {
    /// The `package` value, if the entry names one.
    package: Option<Value>,
    /// The `model` value, if the entry names one.
    model: Option<Value>,
    /// The `fields` table, if the entry names one.
    fields: Option<Map<String, Value>>,
    /// Whether `fields` held a non-table: kept apart so the error names it.
    fields_bad: bool,
    /// The `source` value, if the entry names one.
    source: Option<Value>,
    /// The `upstream` value, if the entry names one.
    upstream: Option<Value>,
}

/// A cursor over corrections text: every read is a checked `get`, so a
/// rough edge is an error, never a panic.
struct Parser<'a> {
    /// The file path, naming every error.
    source: &'a str,
    /// The whole text, for substring reads.
    text: &'a str,
    /// Its bytes, for checked single-byte reads.
    bytes: &'a [u8],
    /// The next byte to read: always a string boundary.
    pos: usize,
    /// The 1-based entry being read: 0 before the first `[[correction]]`.
    entry: usize,
    /// Whether a `[correction.fields]` header opened the current section.
    fields: bool,
}

impl Parser<'_> {
    /// Names `detail` as this file's error.
    fn fail<T>(&self, detail: String) -> Result<T, String> {
        Err(format!("{}: {detail}", self.source))
    }

    /// The next byte, if any.
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    /// The next character, if any.
    fn peek_char(&self) -> Option<char> {
        self.text.get(self.pos..)?.chars().next()
    }

    /// Moves past the next character.
    fn bump(&mut self) {
        if let Some(next) = self.peek_char() {
            self.pos += next.len_utf8();
        }
    }

    /// The text from `start` to the cursor.
    fn since(&self, start: usize) -> Option<&str> {
        self.text.get(start..self.pos)
    }

    /// Moves past one `byte`, or errs naming what was wanted.
    fn eat(&mut self, byte: u8, want: &str) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.bump();
            Ok(())
        } else {
            self.fail(format!("reads no {want}"))
        }
    }

    /// Moves past blank space, newlines and `#` comments.
    fn skip_gap(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => self.bump(),
                Some(b'#') => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    /// Whether the cursor holds `word` exactly.
    fn starts(&self, word: &str) -> bool {
        let end = self.pos + word.len();
        matches!(self.text.get(self.pos..end), Some(head) if head == word)
    }

    /// Whether the cursor holds `word` with a non-key byte after it.
    fn holds(&self, word: &str) -> bool {
        let end = self.pos + word.len();
        match self.text.get(self.pos..end) {
            Some(head) if head == word => match self.text.get(end..) {
                None => true,
                Some(tail) => !tail
                    .chars()
                    .next()
                    .is_some_and(|next| next.is_ascii_alphanumeric() || next == '_' || next == '-'),
            },
            _ => false,
        }
    }

    /// Moves past `word`, which `holds` already confirmed.
    fn take(&mut self, word: &str) {
        self.pos += word.len();
    }

    /// One bare key: ASCII letters, digits, `_` and `-`.
    fn bare_key(&mut self) -> Result<String, String> {
        let start = self.pos;
        while self
            .peek_char()
            .is_some_and(|next| next.is_ascii_alphanumeric() || next == '_' || next == '-')
        {
            self.bump();
        }
        match self.since(start) {
            Some(key) if !key.is_empty() => Ok(key.to_owned()),
            _ => self.fail("reads no key".to_owned()),
        }
    }

    /// One TOML value: a string, integer, float, boolean, array or
    /// inline table. Multiline strings and dates have no reading here.
    fn value(&mut self) -> Result<Value, String> {
        self.skip_gap();
        match self.peek() {
            Some(b'"') => self.basic_string(),
            Some(b'\'') => self.literal_string(),
            Some(b't') if self.holds("true") => {
                self.take("true");
                Ok(Value::Bool(true))
            }
            Some(b'f') if self.holds("false") => {
                self.take("false");
                Ok(Value::Bool(false))
            }
            Some(b'[') => self.array(),
            Some(b'{') => self.inline_table(),
            Some(b'-') | Some(b'+') | Some(b'0'..=b'9') => self.number(),
            Some(_) => self.fail("reads an unexpected byte".to_owned()),
            None => self.fail("ends inside a value".to_owned()),
        }
    }

    /// One `"`-quoted string with its closing quote consumed.
    fn basic_string(&mut self) -> Result<Value, String> {
        self.bump();
        if self.peek() == Some(b'"') {
            self.bump();
            if self.peek() == Some(b'"') {
                return self.fail("reads a multiline string, which has no reading".to_owned());
            }
            return Ok(Value::from(""));
        }
        let mut text = String::new();
        loop {
            match self.peek_char() {
                None => return self.fail("ends inside a string".to_owned()),
                Some('"') => {
                    self.bump();
                    return Ok(Value::from(text));
                }
                Some('\\') => {
                    self.bump();
                    text.push(self.escape()?);
                }
                Some(next) => {
                    self.bump();
                    text.push(next);
                }
            }
        }
    }

    /// The escape after a `\`: its character.
    fn escape(&mut self) -> Result<char, String> {
        match self.peek_char() {
            Some('"') => {
                self.bump();
                Ok('"')
            }
            Some('\\') => {
                self.bump();
                Ok('\\')
            }
            Some('b') => {
                self.bump();
                Ok('\u{08}')
            }
            Some('f') => {
                self.bump();
                Ok('\u{0C}')
            }
            Some('n') => {
                self.bump();
                Ok('\n')
            }
            Some('r') => {
                self.bump();
                Ok('\r')
            }
            Some('t') => {
                self.bump();
                Ok('\t')
            }
            Some('u') => {
                self.bump();
                self.hex(4)
            }
            Some('U') => {
                self.bump();
                self.hex(8)
            }
            _ => self.fail("reads an unknown escape".to_owned()),
        }
    }

    /// `digits` hexadecimal digits as one character.
    fn hex(&mut self, digits: usize) -> Result<char, String> {
        let mut code: u32 = 0;
        for _ in 0..digits {
            match self.peek_char() {
                Some(next) if next.is_ascii_hexdigit() => {
                    self.bump();
                    code = code * 16 + next.to_digit(16).unwrap_or(0);
                }
                _ => return self.fail("reads a short unicode escape".to_owned()),
            }
        }
        char::from_u32(code).ok_or_else(|| format!("{}: reads no character", self.source))
    }

    /// One `'`-quoted string with no escapes.
    fn literal_string(&mut self) -> Result<Value, String> {
        self.bump();
        if self.peek() == Some(b'\'') {
            self.bump();
            if self.peek() == Some(b'\'') {
                return self.fail("reads a multiline string, which has no reading".to_owned());
            }
            return Ok(Value::from(""));
        }
        let mut text = String::new();
        loop {
            match self.peek_char() {
                None => return self.fail("ends inside a string".to_owned()),
                Some('\'') => {
                    self.bump();
                    return Ok(Value::from(text));
                }
                Some(next) => {
                    self.bump();
                    text.push(next);
                }
            }
        }
    }

    /// One integer or float.
    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'-') | Some(b'+')) {
            self.bump();
        }
        while self.peek_char().is_some_and(|next| {
            next.is_ascii_alphanumeric() || next == '_' || next == '.' || next == '+' || next == '-'
        }) {
            self.bump();
        }
        let token = self.since(start).unwrap_or("");
        let digits: String = token.chars().filter(|next| *next != '_').collect();
        if let Ok(int) = digits.parse::<i64>() {
            return Ok(Value::from(int));
        }
        if let Ok(float) = digits.parse::<f64>() {
            return Ok(Value::from(float));
        }
        self.fail(format!("reads a malformed number `{token}`"))
    }

    /// One `[...]` array, spanning lines.
    fn array(&mut self) -> Result<Value, String> {
        self.bump();
        self.skip_gap();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.bump();
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_gap();
            match self.peek() {
                Some(b',') => {
                    self.bump();
                    self.skip_gap();
                    if self.peek() == Some(b']') {
                        self.bump();
                        return Ok(Value::Array(items));
                    }
                }
                Some(b']') => {
                    self.bump();
                    return Ok(Value::Array(items));
                }
                _ => return self.fail("reads no `,` or `]`".to_owned()),
            }
        }
    }

    /// One `{...}` inline table.
    fn inline_table(&mut self) -> Result<Value, String> {
        self.bump();
        self.skip_gap();
        let mut table = Map::new();
        if self.peek() == Some(b'}') {
            self.bump();
            return Ok(Value::Object(table));
        }
        loop {
            let key = self.bare_key()?;
            self.skip_gap();
            self.eat(b'=', "`=`")?;
            let value = self.value()?;
            if table.insert(key.clone(), value).is_some() {
                return self.fail(format!("names `{key}` twice"));
            }
            self.skip_gap();
            match self.peek() {
                Some(b',') => {
                    self.bump();
                    self.skip_gap();
                    if self.peek() == Some(b'}') {
                        self.bump();
                        return Ok(Value::Object(table));
                    }
                }
                Some(b'}') => {
                    self.bump();
                    return Ok(Value::Object(table));
                }
                _ => return self.fail("reads no `,` or `}`".to_owned()),
            }
        }
    }

    /// One `[[correction]]` header: the caller pushes a fresh entry,
    /// which becomes current.
    fn array_header(&mut self) -> Result<(), String> {
        self.take("[[");
        self.skip_gap();
        let name = self.bare_key()?;
        self.skip_gap();
        self.eat(b']', "`]]`")?;
        self.eat(b']', "`]]`")?;
        self.end_header()?;
        if name != "correction" {
            return self.fail(format!("names unknown entry `{name}`"));
        }
        Ok(())
    }

    /// One `[correction.fields]` header: the current entry takes fields.
    fn table_header(&mut self, entries: &mut [RawEntry]) -> Result<(), String> {
        self.bump();
        self.skip_gap();
        let mut parts = vec![self.bare_key()?];
        while self.peek() == Some(b'.') {
            self.bump();
            self.skip_gap();
            parts.push(self.bare_key()?);
            self.skip_gap();
        }
        self.eat(b']', "`]`")?;
        self.end_header()?;
        if parts == ["correction", "fields"] {
            if self.entry == 0 {
                return self.fail("names a `fields` table outside any `[[correction]]`".to_owned());
            }
            if let Some(current) = entries.get_mut(self.entry - 1) {
                if current.fields.is_some() || current.fields_bad {
                    return self.fail(format!("correction {} names `fields` twice", self.entry));
                }
                current.fields = Some(Map::new());
            }
            self.fields = true;
            return Ok(());
        }
        self.fail(format!("names unknown table `{}`", parts.join(".")))
    }

    /// Moves past blank space and `#` comments, stopping at a newline.
    fn skip_line_gap(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') => self.bump(),
                Some(b'#') => {
                    while !matches!(self.peek(), None | Some(b'\n') | Some(b'\r')) {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    /// The end of a top-level line: only blank space or a comment
    /// follows the item; returns whether the file ends here.
    fn end_line(&mut self, what: &str) -> Result<bool, String> {
        self.skip_line_gap();
        match self.peek() {
            None => Ok(true),
            Some(b'\n') | Some(b'\r') => {
                self.bump();
                Ok(false)
            }
            _ => self.fail(format!("reads trailing text after {what}")),
        }
    }

    /// The end of a header line: only blank space or a comment follows.
    /// The newline stays for the file loop to consume.
    fn end_header(&mut self) -> Result<(), String> {
        self.skip_line_gap();
        match self.peek() {
            None | Some(b'\n') | Some(b'\r') => Ok(()),
            _ => self.fail("reads trailing text after the header".to_owned()),
        }
    }

    /// One `key = value` line: in the current entry, or in its fields
    /// after a `[correction.fields]` header. Before the first
    /// `[[correction]]` any key is an error.
    fn key_value(&mut self, entries: &mut [RawEntry]) -> Result<(), String> {
        let key = self.bare_key()?;
        self.skip_gap();
        self.eat(b'=', "`=`")?;
        let number = self.entry;
        let in_fields = self.fields;
        let value = self.value().map_err(|detail| {
            format!(
                "{}: correction {number} gives `{key}` an invalid value: {detail}",
                self.source
            )
        })?;
        if number == 0 {
            return self.fail("names a key outside any `[[correction]]`".to_owned());
        }
        let Some(current) = entries.get_mut(number - 1) else {
            return self.fail(format!("correction {number} is no entry"));
        };
        if in_fields {
            match current.fields.as_mut() {
                None => return self.fail(format!("correction {number} is no entry")),
                Some(fields) => {
                    if fields.insert(key.clone(), value).is_some() {
                        return self.fail(format!("correction {number} names field `{key}` twice"));
                    }
                }
            }
            return Ok(());
        }
        let slot = match key.as_str() {
            "package" => &mut current.package,
            "model" => &mut current.model,
            "source" => &mut current.source,
            "upstream" => &mut current.upstream,
            "fields" => {
                if current.fields.is_some() || current.fields_bad {
                    return self.fail(format!("correction {number} names `fields` twice"));
                }
                match value {
                    Value::Object(fields) => {
                        current.fields = Some(fields);
                        return Ok(());
                    }
                    Value::Null
                    | Value::Bool(_)
                    | Value::Number(_)
                    | Value::String(_)
                    | Value::Array(_) => {
                        current.fields_bad = true;
                        return Ok(());
                    }
                }
            }
            _ => return self.fail(format!("correction {number} names unknown key `{key}`")),
        };
        if slot.is_some() {
            return self.fail(format!("correction {number} names `{key}` twice"));
        }
        *slot = Some(value);
        Ok(())
    }
}

/// Parses corrections `text` from `source`: one `[[correction]]` array
/// whose entries each name `package`, `model`, `fields`, `source` and
/// `upstream`. Anything else fails naming the file.
pub(crate) fn parse_corrections(source: &str, text: &str) -> Result<Vec<Correction>, String> {
    let mut parser = Parser {
        source,
        text,
        bytes: text.as_bytes(),
        pos: 0,
        entry: 0,
        fields: false,
    };
    let mut entries: Vec<RawEntry> = Vec::new();
    loop {
        parser.skip_gap();
        if parser.peek().is_none() {
            break;
        }
        if parser.starts("[[") {
            parser.array_header()?;
            entries.push(RawEntry::default());
            parser.entry = entries.len();
            parser.fields = false;
        } else if parser.peek() == Some(b'[') {
            parser.table_header(&mut entries)?;
        } else {
            parser.key_value(&mut entries)?;
        }
        // A top-level item ends at its newline: anything else on the
        // line is trailing text, never the next item.
        if parser.end_line("the value")? {
            break;
        }
    }
    let mut corrections = Vec::with_capacity(entries.len());
    for (index, raw) in entries.into_iter().enumerate() {
        corrections.push(finish_correction(source, index + 1, raw)?);
    }
    Ok(corrections)
}

/// One required string key of entry `number`: missing, empty and
/// non-string values all fail, so an empty page reads as no page.
fn correction_string(
    source: &str,
    number: usize,
    key: &str,
    value: Option<Value>,
) -> Result<String, String> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Ok(text),
        None | Some(Value::String(_)) => {
            Err(format!("{source}: correction {number} names no `{key}`"))
        }
        Some(_) => Err(format!(
            "{source}: correction {number} names a non-string `{key}`"
        )),
    }
}

/// A parsed entry with every required key checked.
fn finish_correction(source: &str, number: usize, raw: RawEntry) -> Result<Correction, String> {
    let package = correction_string(source, number, "package", raw.package)?;
    let model = correction_string(source, number, "model", raw.model)?;
    let source_url = correction_string(source, number, "source", raw.source)?;
    let upstream = correction_string(source, number, "upstream", raw.upstream)?;
    if raw.fields_bad {
        return Err(format!(
            "{source}: correction {number} names a non-table `fields`"
        ));
    }
    match raw.fields {
        None => Err(format!("{source}: correction {number} names no `fields`")),
        Some(fields) if fields.is_empty() => {
            Err(format!("{source}: correction {number} names no fields"))
        }
        Some(fields) => Ok(Correction {
            package,
            model,
            fields,
            source: source_url,
            upstream,
        }),
    }
}

/// One `"`-quoted TOML string.
fn write_toml_string(value: &str, text: &mut String) {
    text.push('"');
    for next in value.chars() {
        match next {
            '"' => text.push_str("\\\""),
            '\\' => text.push_str("\\\\"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            '\u{08}' => text.push_str("\\b"),
            '\u{0C}' => text.push_str("\\f"),
            next if next.is_control() => {
                text.push_str(&format!("\\u{:04X}", u32::from(next)));
            }
            _ => text.push(next),
        }
    }
    text.push('"');
}

/// One TOML value: only strings, integers, floats, booleans, arrays and
/// objects reach here, as the corrections reader parses nothing else.
/// A null writes the TOML string `"null"` so a later read fails loudly
/// instead of applying a silent wrong value.
fn write_toml_value(value: &Value, text: &mut String) {
    match value {
        Value::Null => text.push_str("\"null\""),
        Value::Bool(kept) => text.push_str(if *kept { "true" } else { "false" }),
        Value::Number(number) => text.push_str(&number.to_string()),
        Value::String(kept) => write_toml_string(kept, text),
        Value::Array(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                write_toml_value(item, text);
            }
            text.push(']');
        }
        Value::Object(table) => {
            text.push('{');
            for (index, (key, item)) in table.iter().enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                text.push_str(key);
                text.push_str(" = ");
                write_toml_value(item, text);
            }
            text.push('}');
        }
    }
}

/// The corrections file text for `corrections`: the header comment plus
/// one `[[correction]]` entry each, so a prune rewrite keeps the file's
/// shape. An empty list writes the header alone.
pub(crate) fn render_corrections(corrections: &[Correction]) -> String {
    let mut text = String::from(
        "# Cited corrections to the generated model lists (`docs/model-routing.md`,\n\
         # \"What a provider extension declares\"): `cargo xtask models-dev` applies\n\
         # each entry after the per-package table and deletes an entry once\n\
         # models.dev agrees with it. Each entry names the vendor page that says\n\
         # so (`source`) and the models.dev pull request fixing it upstream\n\
         # (`upstream`). Only `name`, `protocol`, `context_window`,\n\
         # `max_output_tokens`, `input` and `cost` may be overridden; every other\n\
         # field comes from the per-package table alone.\n",
    );
    for correction in corrections {
        text.push('\n');
        text.push_str("[[correction]]\n");
        for (key, value) in [
            ("package", correction.package.as_str()),
            ("model", correction.model.as_str()),
            ("source", correction.source.as_str()),
            ("upstream", correction.upstream.as_str()),
        ] {
            text.push_str(key);
            text.push_str(" = ");
            write_toml_string(value, &mut text);
            text.push('\n');
        }
        text.push_str("[correction.fields]\n");
        for (key, value) in &correction.fields {
            text.push_str(key);
            text.push_str(" = ");
            write_toml_value(value, &mut text);
            text.push('\n');
        }
    }
    text
}

/// Whether `name` is the stem or the full path of `package`.
fn names_package(package: &Package, name: &str) -> bool {
    name == package.path || name == stem(package)
}

/// Every correction names a known package once, overrides only derived
/// fields with values of their shape, and never moves the table-pinned
/// codex window.
fn check_corrections(packages: &[Package], corrections: &[Correction]) -> Result<(), String> {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for correction in corrections {
        let entry = format!(
            "correction for `{}/{}`",
            correction.package, correction.model
        );
        let Some(package) = packages
            .iter()
            .find(|package| names_package(package, &correction.package))
        else {
            return Err(format!(
                "{CORRECTIONS}: {entry} names unknown package `{}`",
                correction.package
            ));
        };
        if seen.contains(&(correction.package.as_str(), correction.model.as_str())) {
            return Err(format!("{CORRECTIONS}: {entry} appears twice"));
        }
        seen.push((correction.package.as_str(), correction.model.as_str()));
        for (field, value) in &correction.fields {
            if TABLE_ONLY_FIELDS.contains(&field.as_str()) {
                return Err(format!(
                    "{CORRECTIONS}: {entry} overrides table-only field `{field}`"
                ));
            }
            if !OVERRIDABLE_FIELDS.contains(&field.as_str()) {
                return Err(format!(
                    "{CORRECTIONS}: {entry} names unknown field `{field}`"
                ));
            }
            let shape = match field.as_str() {
                "name" | "protocol" => value.is_string(),
                "context_window" | "max_output_tokens" => value.as_u64().is_some(),
                "input" => value
                    .as_array()
                    .is_some_and(|input| input.iter().all(|kind| kind.is_string())),
                "cost" => value.is_object(),
                _ => false,
            };
            if !shape {
                let want = match field.as_str() {
                    "name" | "protocol" => "a non-string",
                    "context_window" | "max_output_tokens" => "a non-integer",
                    "input" => "a non-string array",
                    _ => "a non-object",
                };
                return Err(format!("{CORRECTIONS}: {entry} sets `{field}` to {want}"));
            }
        }
        if stem(package) == "codex" && correction.fields.contains_key("context_window") {
            return Err(format!(
                "{CORRECTIONS}: {entry} overrides the table-pinned `context_window`"
            ));
        }
    }
    Ok(())
}

/// One generated model: its derived fields, then the table layers, with a
/// later key replacing an earlier one.
fn generate_model(
    packages: &[Package],
    package: &Package,
    protocol: &'static str,
    id: &str,
    model: &Value,
) -> Result<Map<String, Value>, String> {
    let mut generated = Map::new();
    generated.insert("base_url".to_owned(), Value::from(package.base_url));
    let context = model
        .get("limit")
        .and_then(|limit| limit.get("context"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    generated.insert("context_window".to_owned(), Value::from(context));
    if let Some(cost) = derived_cost(model) {
        generated.insert("cost".to_owned(), cost);
    }
    generated.insert("id".to_owned(), Value::from(id));
    let input: Vec<Value> = if has_image(model) {
        vec![Value::from("text"), Value::from("image")]
    } else {
        vec![Value::from("text")]
    };
    generated.insert("input".to_owned(), Value::Array(input));
    if let Some(output) = model
        .get("limit")
        .and_then(|limit| limit.get("output"))
        .and_then(Value::as_u64)
        .filter(|output| *output > 0)
    {
        generated.insert("max_output_tokens".to_owned(), Value::from(output));
    }
    if let Some(name) = model
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        generated.insert("name".to_owned(), Value::from(name));
    }
    generated.insert("protocol".to_owned(), Value::from(protocol));
    let by_protocol = package
        .by_protocol
        .iter()
        .find(|entry| entry.0 == protocol)
        .map(|entry| entry.1);
    let by_model = package
        .by_model
        .iter()
        .find(|entry| entry.0 == id)
        .map(|entry| entry.1);
    for text in [Some(package.every_model), by_protocol, by_model]
        .into_iter()
        .flatten()
    {
        for (key, value) in layer(package, text)? {
            generated.insert(key, value);
        }
    }
    let thinking = thinking_row(packages, package, id);
    if is_gateway(package) && thinking.is_none() {
        return Err(format!(
            "{}: `{id}` has neither an inherited nor its own thinking row",
            stem(package)
        ));
    }
    if let Some((levels, default)) = thinking {
        generated.insert(
            "thinking_levels".to_owned(),
            Value::Array(levels.iter().map(|level| Value::from(*level)).collect()),
        );
        if let Some(default) = default {
            generated.insert("thinking_default".to_owned(), Value::from(default));
        }
    }
    Ok(generated)
}

/// Every package's file from `catalog` (full or trimmed), plus one line
/// per left-out model. Models are sorted by id, keys are sorted, and
/// numbers keep models.dev's spelling. `corrections` apply over the
/// derived and table values of the models they name; an entry every
/// field of which already matches is pruned, and the survivors return
/// for the run to rewrite the file with.
pub(crate) fn generate(
    catalog: &Value,
    packages: &[Package],
    corrections: &[Correction],
) -> Result<(Generated, Vec<Correction>), String> {
    check_corrections(packages, corrections)?;
    let mut outputs = Vec::new();
    let mut left_out = Vec::new();
    let mut pruned: Vec<usize> = Vec::new();
    for package in packages {
        let source = package.source;
        let provider = catalog
            .get(source)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("models.dev has no `{source}` provider"))?;
        let models = match provider.get("models") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(models)) => models.clone(),
            Some(_) => return Err(format!("models.dev has no `{source}` provider")),
        };
        let mut ids: Vec<&String> = models.keys().collect();
        ids.sort();
        let mut generated: Vec<(String, Map<String, Value>)> = Vec::new();
        for id in ids {
            let model = models.get(id).unwrap_or(&Value::Null);
            let protocol = protocol_of(package, model, id);
            if !package.only.is_empty() && !package.only.contains(&id.as_str()) {
                continue;
            }
            if let Some(line) = leave_out_line(package, protocol, id, model) {
                left_out.push(line);
                continue;
            }
            generated.push((
                id.clone(),
                generate_model(packages, package, protocol, id, model)?,
            ));
        }
        for id in package.only.iter().copied() {
            if !generated.iter().any(|model| model.0 == id) {
                return Err(format!(
                    "{}: only entry for `{id}` matches no generated model",
                    stem(package)
                ));
            }
        }
        for id in package
            .protocol_overrides
            .iter()
            .map(|entry| entry.0)
            .chain(package.by_model.iter().map(|entry| entry.0))
            .chain(package.thinking.iter().map(|entry| entry.0))
        {
            if !generated.iter().any(|model| model.0 == id) {
                return Err(format!(
                    "{}: table entry for `{id}` matches no generated model",
                    stem(package)
                ));
            }
        }
        for (index, correction) in corrections.iter().enumerate() {
            if !names_package(package, &correction.package) {
                continue;
            }
            let Some((_, built)) = generated
                .iter_mut()
                .find(|built| built.0 == correction.model)
            else {
                return Err(format!(
                    "{}: correction for `{}` matches no generated model",
                    stem(package),
                    correction.model
                ));
            };
            let agreed = correction
                .fields
                .iter()
                .all(|(key, value)| built.get(key) == Some(value));
            if agreed {
                pruned.push(index);
            } else {
                for (key, value) in &correction.fields {
                    built.insert(key.clone(), value.clone());
                }
            }
        }
        let provider_layer = layer(package, package.provider)?;
        if let Some(reviewer) = provider_layer.get("reviewer_model").and_then(Value::as_str)
            && !generated.iter().any(|model| model.0 == reviewer)
        {
            return Err(format!(
                "{}: reviewer_model `{reviewer}` is not a generated model",
                stem(package)
            ));
        }
        let mut file = provider_layer;
        file.insert(
            "models".to_owned(),
            Value::Array(
                generated
                    .iter()
                    .map(|(_, model)| Value::Object(model.clone()))
                    .collect(),
            ),
        );
        let text = serde_json::to_string_pretty(&Value::Object(file))
            .map_err(|error| format!("{}: {error}", package.path))?;
        outputs.push(Output {
            path: package.path,
            text: text + "\n",
            models: generated.len(),
        });
    }
    let mut survivors = Vec::with_capacity(corrections.len());
    for (index, correction) in corrections.iter().enumerate() {
        if !pruned.contains(&index) {
            survivors.push(correction.clone());
        }
    }
    Ok(((outputs, left_out), survivors))
}

/// `curl -fsSL --max-time 120 <url>` writing the body to stdout.
pub(crate) fn curl(url: &str) -> Command {
    let mut command = Command::new("curl");
    command.args(["-fsSL", "--max-time", "120", url]);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command
}

/// Runs `curl(url)`; Err names curl's exit status and stderr.
pub(crate) fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let output = curl(url)
        .output()
        .map_err(|error| format!("curl {url}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "curl {url}: {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

/// `cargo xtask models-dev [--from FILE]`: reads `FILE`, or fetches
/// `MODELS_DEV_URL`, then trims and generates with the cited corrections
/// file read from `root`. Only when trim, generate and the Databricks
/// thinking table all succeed does it write `SNAPSHOT`, every `Output`
/// and the corrections file under `root` (creating parent
/// directories): an unchanged corrections file keeps its bytes, while a
/// prune rewrites it with the survivors. Prints `<path>: <n> models` per
/// file to stdout and each left-out line to stderr.
pub(crate) fn run(root: &Path, from: Option<&Path>) -> Result<(), String> {
    let (source, bytes) = match from {
        Some(path) => (
            path.display().to_string(),
            std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?,
        ),
        None => (MODELS_DEV_URL.to_owned(), fetch(MODELS_DEV_URL)?),
    };
    let catalog: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("{source}: {error}"))?;
    let trimmed = trim(&catalog)?;
    let corrections_path = root.join(CORRECTIONS);
    let corrections_text = std::fs::read_to_string(&corrections_path)
        .map_err(|error| format!("{}: {error}", corrections_path.display()))?;
    let corrections = parse_corrections(CORRECTIONS, &corrections_text)?;
    let ((outputs, left_out), survivors) = generate(&trimmed, &PACKAGES, &corrections)?;
    let thinking = databricks_thinking(&PACKAGES)?;
    let snapshot =
        serde_json::to_string_pretty(&trimmed).map_err(|error| format!("{source}: {error}"))?;
    let corrections_text = if survivors.len() == corrections.len() {
        corrections_text
    } else {
        render_corrections(&survivors)
    };
    let mut writes = vec![(root.join(SNAPSHOT), snapshot + "\n")];
    for output in &outputs {
        writes.push((root.join(output.path), output.text.clone()));
    }
    writes.push((root.join(thinking.path), thinking.text.clone()));
    writes.push((corrections_path, corrections_text));
    for (path, text) in &writes {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", path.display()))?;
        }
        std::fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    for output in &outputs {
        println!("{}: {} models", output.path, output.models);
    }
    println!("{}: {} rows", thinking.path, thinking.models);
    for line in &left_out {
        eprintln!("{line}");
    }
    Ok(())
}

#[cfg(test)]
#[path = "models_dev_tests.rs"]
mod tests;
