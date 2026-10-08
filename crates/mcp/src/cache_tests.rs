//! The cache key, file and miss rules, through the public functions.

use std::collections::BTreeMap;

use fakes::TempDir;
use serde_json::json;

use super::{key, path, read, write};

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn tools() -> Vec<serde_json::Value> {
    vec![json!({
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": true},
    })]
}

fn prompts() -> Vec<serde_json::Value> {
    vec![json!({
        "name": "greet",
        "description": "Greets someone.",
        "arguments": [{"name": "who", "required": true}, {"name": "tone"}],
    })]
}

fn cached() -> super::Cached {
    super::Cached {
        tools: tools(),
        prompts: prompts(),
    }
}

#[test]
fn equal_declarations_share_a_key() {
    let left = key("fx", &["a".to_owned()], &env(&[("K", "1")]));
    let right = key("fx", &["a".to_owned()], &env(&[("K", "1")]));
    assert_eq!(left, right);
    assert_eq!(left.len(), 64);
    assert!(left.chars().all(|char| char.is_ascii_hexdigit()));
    assert_eq!(left, left.to_ascii_lowercase(), "the key is lowercase hex",);
}

#[test]
fn the_key_moves_with_command_args_and_env() {
    let base = key("fx", &["a".to_owned()], &env(&[("K", "1")]));
    assert_ne!(base, key("other", &["a".to_owned()], &env(&[("K", "1")])));
    assert_ne!(base, key("fx", &["b".to_owned()], &env(&[("K", "1")])));
    assert_ne!(base, key("fx", &["a".to_owned()], &env(&[("K", "2")])));
    assert_ne!(
        base,
        key("fx", &["a".to_owned()], &env(&[("K", "1"), ("J", "0")])),
    );
}

#[test]
fn a_write_then_a_read_returns_both_lists() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    let declaration = key("fx", &[], &env(&[]));
    write(&cache, "fx", &declaration, &cached());
    assert_eq!(read(&cache, "fx", &declaration), Some(cached()));
}

#[test]
fn a_file_with_no_prompt_list_is_a_miss() {
    // A pre-change cache file holds tools but no prompts: the session
    // starts the server once to list both (`docs/mcp.md`, "Starting
    // servers").
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    let declaration = key("fx", &[], &env(&[]));
    std::fs::create_dir_all(&cache).expect("cache dir");
    std::fs::write(
        cache.join("fx.json"),
        json!({"key": declaration, "tools": tools()}).to_string(),
    )
    .expect("tool-only cache");
    assert_eq!(read(&cache, "fx", &declaration), None);
}

#[test]
fn a_different_key_is_a_miss() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    write(&cache, "fx", &key("fx", &[], &env(&[])), &cached());
    assert_eq!(
        read(&cache, "fx", &key("fx", &["changed".to_owned()], &env(&[]))),
        None
    );
}

#[test]
fn a_missing_file_is_a_miss() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    assert_eq!(read(&cache, "fx", &key("fx", &[], &env(&[]))), None);
}

#[test]
fn a_corrupt_file_is_a_miss() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    std::fs::create_dir_all(&cache).expect("cache dir");
    std::fs::write(cache.join("fx.json"), "not json").expect("corrupt cache");
    assert_eq!(read(&cache, "fx", &key("fx", &[], &env(&[]))), None);
}

#[test]
fn a_key_mismatch_shape_is_a_miss() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    std::fs::create_dir_all(&cache).expect("cache dir");
    // A `tools` that is not an array.
    std::fs::write(
        cache.join("fx.json"),
        json!({"key": key("fx", &[], &env(&[])), "tools": {}}).to_string(),
    )
    .expect("shapeless cache");
    assert_eq!(read(&cache, "fx", &key("fx", &[], &env(&[]))), None);
}

#[test]
fn unsafe_names_have_no_file_and_always_miss() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    let declaration = key("fx", &[], &env(&[]));
    for name in ["", ".hidden", "a/b", "a\\b", "a\0b", ".."] {
        assert_eq!(path(&cache, name), None, "name: {name:?}");
        assert_eq!(read(&cache, name, &declaration), None, "name: {name:?}");
        write(&cache, name, &declaration, &cached());
    }
    assert!(
        !cache.exists(),
        "an unsafe name writes nothing, not even the directory",
    );
}

#[test]
fn safe_names_have_a_file() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    assert_eq!(path(&cache, "fx"), Some(cache.join("fx.json")));
    assert_eq!(
        path(&cache, "my.server"),
        Some(cache.join("my.server.json")),
    );
}

#[test]
fn a_write_leaves_no_tmp_behind_and_creates_the_directory() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("missing").join("mcp");
    assert!(!cache.exists());
    let declaration = key("fx", &[], &env(&[]));
    write(&cache, "fx", &declaration, &cached());
    assert_eq!(read(&cache, "fx", &declaration), Some(cached()));
    let left: Vec<_> = std::fs::read_dir(&cache)
        .expect("cache dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(left, ["fx.json"]);
}

#[test]
fn entries_are_stored_verbatim() {
    // A bare entry round-trips unchanged, and reads back through
    // `ListedTool::read` with the defaults the tool declares.
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    let declaration = key("fx", &[], &env(&[]));
    std::fs::create_dir_all(&cache).expect("cache dir");
    std::fs::write(
        cache.join("fx.json"),
        json!({"key": declaration, "tools": [{"name": "bare"}], "prompts": []}).to_string(),
    )
    .expect("bare cache");
    assert_eq!(
        read(&cache, "fx", &declaration),
        Some(super::Cached {
            tools: vec![json!({"name": "bare"})],
            prompts: Vec::new(),
        })
    );
    let bare = crate::server::ListedTool::read(&json!({"name": "bare"}));
    assert_eq!(bare.name, "bare");
    assert_eq!(bare.description, String::new());
    assert_eq!(bare.schema, json!({"type": "object"}));
    assert_eq!(bare.hints, crate::effects::Hints::default());
    write(
        &cache,
        "fx",
        &declaration,
        &super::Cached {
            tools: vec![json!({"name": "bare"})],
            prompts: Vec::new(),
        },
    );
    assert_eq!(
        read(&cache, "fx", &declaration),
        Some(super::Cached {
            tools: vec![json!({"name": "bare"})],
            prompts: Vec::new(),
        })
    );
}
