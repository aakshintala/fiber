//! One server's last tool and prompt lists (`docs/mcp.md`, "Starting
//! servers" and "Prompts and resources"): the cache file at
//! `cache/mcp/<server>.json` (`docs/state.md`, "Cache"), keyed by a hash
//! of the server's declaration. A missing, unreadable or unparsable file,
//! one whose key or version differs, or one with no prompt list is a miss;
//! a failed write is ignored, because the cache is always safe to lose.
//! Writes go through a temporary file and a rename, so two sessions
//! refreshing one list leave one whole file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::server_json::{ListedPrompt, ListedTool};

/// The cache file version. A file with no `version`, or with any other
/// version, is a miss.
const CACHE_VERSION: u32 = 1;

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

/// What one cache file holds: the server's `tools/list` and `prompts/list`
/// entries, in the order listed.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Cached {
    /// The `tools/list` entries.
    pub tools: Vec<ListedTool>,
    /// The `prompts/list` entries.
    pub prompts: Vec<ListedPrompt>,
}

/// The file on disk: compact JSON with a version, the declaration key and
/// both lists.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    key: String,
    tools: Vec<ListedTool>,
    prompts: Vec<ListedPrompt>,
}

/// Reads the cached lists for `server` whose key is `key`: `None` on every
/// miss (no file, an unreadable or unparsable file, a version that differs,
/// a key that differs, or an unsafe name).
pub(crate) fn read(cache: &Path, server: &str, key: &str) -> Option<Cached> {
    let path = path(cache, server)?;
    let bytes = std::fs::read(path).ok()?;
    let file: CacheFile = serde_json::from_slice(&bytes).ok()?;
    if file.version != CACHE_VERSION {
        return None;
    }
    if file.key != key {
        return None;
    }
    Some(Cached {
        tools: file.tools,
        prompts: file.prompts,
    })
}

/// Writes `lists` as `server`'s cached lists under `key`: `create_dir_all`
/// first, then a temporary sibling file renamed over the cache file, so
/// the last rename wins. A failure changes nothing the session depends on:
/// it is ignored, and an unsafe name writes nothing at all.
pub(crate) fn write(cache: &Path, server: &str, key: &str, lists: &Cached) {
    let Some(path) = path(cache, server) else {
        return;
    };
    if std::fs::create_dir_all(cache).is_err() {
        return;
    }
    let file = CacheFile {
        version: CACHE_VERSION,
        key: key.to_owned(),
        tools: lists.tools.clone(),
        prompts: lists.prompts.clone(),
    };
    let bytes = serde_json::to_vec(&file).unwrap_or_default();
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id(),));
    if std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .is_err()
    {
        match std::fs::remove_file(&tmp) {
            Ok(()) | Err(_) => {}
        }
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
