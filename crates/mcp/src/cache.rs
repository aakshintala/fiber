//! One server's last tool list (`docs/mcp.md`, "Starting servers"): the
//! cache file at `cache/mcp/<server>.json` (`docs/state.md`, "Cache"),
//! keyed by a hash of the server's declaration. A missing, unreadable or
//! unparsable file, or one whose key differs, is a miss; a failed write is
//! ignored, because the cache is always safe to lose. Writes go through a
//! temporary file and a rename, so two sessions refreshing one list leave
//! one whole file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::server::ListedTool;

/// The key of a server's declaration: the lowercase hex SHA-256 of the
/// JSON serialisation of its command, arguments and environment. Timeouts,
/// `required`, `enabled`, `disabled` and hint overrides are not part of
/// the declaration, so changing them needs no cache miss.
pub(crate) fn key(command: &str, args: &[String], env: &BTreeMap<String, String>) -> String {
    let declaration = serde_json::json!({
        "command": command,
        "args": args,
        "env": env,
    });
    let bytes = serde_json::to_vec(&declaration).unwrap_or_default();
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The cache file of `server` under `cache`: `None` when the name has no
/// safe file. An empty name, one starting with `.`, or one holding `/`,
/// `\\` or NUL always counts as a miss and nothing is written.
pub(crate) fn path(cache: &Path, server: &str) -> Option<PathBuf> {
    if !safe(server) {
        return None;
    }
    Some(cache.join(format!("{server}.json")))
}

/// Whether `server` names a cache file: a mutant of any arm counts the
/// wrong names as safe or safe names as misses.
fn safe(server: &str) -> bool {
    !server.is_empty()
        && !server.starts_with('.')
        && !server.contains('/')
        && !server.contains('\\')
        && !server.contains('\0')
}

/// Reads the cached list for `server` whose key is `key`: `None` on every
/// miss (no file, an unreadable or unparsable file, a key that differs, a
/// `tools` that is not an array, or an unsafe name). Each entry is read
/// back through [`ListedTool::read`], so the cache holds the server's full
/// list before `enabled`/`disabled`/hint overrides.
pub(crate) fn read(cache: &Path, server: &str, key: &str) -> Option<Vec<ListedTool>> {
    let path = path(cache, server)?;
    let bytes = std::fs::read(path).ok()?;
    let file: Value = serde_json::from_slice(&bytes).ok()?;
    let object = file.as_object()?;
    if object.get("key").and_then(Value::as_str) != Some(key) {
        return None;
    }
    let tools = object.get("tools").and_then(Value::as_array)?;
    Some(tools.iter().map(ListedTool::read).collect())
}

/// Writes `tools` as `server`'s cached list under `key`: `create_dir_all`
/// first, then a temporary sibling file renamed over the cache file, so
/// the last rename wins. A failure changes nothing the session depends
/// on: it is ignored, and an unsafe name writes nothing at all.
pub(crate) fn write(cache: &Path, server: &str, key: &str, tools: &[ListedTool]) {
    let Some(path) = path(cache, server) else {
        return;
    };
    if std::fs::create_dir_all(cache).is_err() {
        return;
    }
    let entries: Vec<Value> = tools.iter().map(entry).collect();
    let file = serde_json::json!({"key": key, "tools": entries});
    let bytes = serde_json::to_vec(&file).unwrap_or_default();
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id(),));
    if std::fs::write(&tmp, bytes).is_err() {
        match std::fs::remove_file(&tmp) {
            Ok(()) | Err(_) => {}
        }
        return;
    }
    if std::fs::rename(&tmp, &path).is_err() {
        match std::fs::remove_file(&tmp) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// One cached entry: the server's raw `tools/list` entry shape, which
/// [`ListedTool::read`] reads back into the same tool.
fn entry(tool: &ListedTool) -> Value {
    serde_json::json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": tool.schema,
        "annotations": tool.hints.to_annotations(),
    })
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
