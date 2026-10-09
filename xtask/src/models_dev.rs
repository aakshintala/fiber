//! `cargo xtask models-dev`: regenerates the `models` lists of the
//! first-party provider files from models.dev, keeping every Fiber-only
//! field in the per-package table (`docs/model-routing.md`, "What a
//! provider extension declares").

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use crate::models_dev_table::{PACKAGES, Package, ProtocolRule};

/// The models.dev catalog the generator reads when `--from` is absent.
pub(crate) const MODELS_DEV_URL: &str = "https://models.dev/api.json";
/// The checked-in, trimmed models.dev catalog: the drift test's input.
pub(crate) const SNAPSHOT: &str = "xtask/tests/models-dev.json";

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
const MODEL_KEYS: [&str; 6] = [
    "cost",
    "limit",
    "modalities",
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

/// The six source keys and, per model, only the fields `generate` reads.
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
    if package.drop_protocols.contains(&protocol) {
        return Some(format!(
            "models-dev: {}/{id} is on {protocol}, left out until probed; left out",
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

/// One generated model: its derived fields, then the table layers, with a
/// later key replacing an earlier one.
fn generate_model(
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
    Ok(generated)
}

/// Every package's file from `catalog` (full or trimmed), plus one line
/// per left-out model. Models are sorted by id, keys are sorted, and
/// numbers keep models.dev's spelling.
pub(crate) fn generate(catalog: &Value, packages: &[Package]) -> Result<Generated, String> {
    let mut outputs = Vec::new();
    let mut left_out = Vec::new();
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
            if let Some(line) = leave_out_line(package, protocol, id, model) {
                left_out.push(line);
                continue;
            }
            generated.push((id.clone(), generate_model(package, protocol, id, model)?));
        }
        for (id, _) in package
            .protocol_overrides
            .iter()
            .chain(package.by_model.iter())
        {
            if !generated.iter().any(|model| model.0 == *id) {
                return Err(format!(
                    "{}: table entry for `{id}` matches no generated model",
                    stem(package)
                ));
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
    Ok((outputs, left_out))
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
/// `MODELS_DEV_URL`, then trims and generates. Only when both succeed does
/// it write `SNAPSHOT` and every `Output` under `root` (creating parent
/// directories). Prints `<path>: <n> models` per file to stdout and each
/// left-out line to stderr.
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
    let (outputs, left_out) = generate(&trimmed, &PACKAGES)?;
    let snapshot =
        serde_json::to_string_pretty(&trimmed).map_err(|error| format!("{source}: {error}"))?;
    let mut writes = vec![(root.join(SNAPSHOT), snapshot + "\n")];
    for output in &outputs {
        writes.push((root.join(output.path), output.text.clone()));
    }
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
    for line in &left_out {
        eprintln!("{line}");
    }
    Ok(())
}

#[cfg(test)]
#[path = "models_dev_tests.rs"]
mod tests;
