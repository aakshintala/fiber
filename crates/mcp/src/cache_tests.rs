//! The cache key, file and miss rules, through the public functions.

use std::collections::BTreeMap;

use fakes::TempDir;
use serde_json::{Value, json};

use super::{key, path, read, write};
use crate::server_json::{ListedPrompt, ListedTool};

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn tools() -> Vec<ListedTool> {
    vec![
        serde_json::from_value(json!({
            "name": "echo",
            "description": "Echoes.",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true},
        }))
        .expect("typed tools"),
    ]
}

fn prompts() -> Vec<ListedPrompt> {
    vec![
        serde_json::from_value(json!({
            "name": "greet",
            "description": "Greets someone.",
            "arguments": [{"name": "who", "required": true}, {"name": "tone"}],
        }))
        .expect("typed prompts"),
    ]
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
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    let declaration = key("fx", &[], &env(&[]));
    std::fs::create_dir_all(&cache).expect("cache dir");
    std::fs::write(
        cache.join("fx.json"),
        json!({"version": 1, "key": declaration, "tools": [{"name": "echo"}]}).to_string(),
    )
    .expect("tool-only cache");
    assert_eq!(read(&cache, "fx", &declaration), None);
}

#[test]
fn a_pre_change_file_is_a_miss() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    let declaration = key("fx", &[], &env(&[]));
    std::fs::create_dir_all(&cache).expect("cache dir");
    std::fs::write(
        cache.join("fx.json"),
        json!({"key": declaration, "tools": [{"name": "echo"}], "prompts": []}).to_string(),
    )
    .expect("pre-change cache");
    assert_eq!(read(&cache, "fx", &declaration), None);
}

#[test]
fn another_version_is_a_miss() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    let declaration = key("fx", &[], &env(&[]));
    std::fs::create_dir_all(&cache).expect("cache dir");
    std::fs::write(
        cache.join("fx.json"),
        json!({"version": 2, "key": declaration, "tools": [], "prompts": []}).to_string(),
    )
    .expect("future cache");
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
    std::fs::write(
        cache.join("fx.json"),
        json!({"version": 1, "key": key("fx", &[], &env(&[])), "tools": {}}).to_string(),
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
fn unknown_fields_survive_a_write_and_read() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    let declaration = key("fx", &[], &env(&[]));
    let bare: ListedTool = serde_json::from_value(json!({
        "name": "bare",
        "idempotentHint": true,
    }))
    .expect("bare tool reads");
    assert_eq!(bare.name, "bare");
    assert_eq!(bare.description, String::new());
    assert_eq!(bare.schema, json!({"type": "object"}));
    assert_eq!(bare.hints(), crate::effects::Hints::default());
    let lists = super::Cached {
        tools: vec![bare],
        prompts: Vec::new(),
    };
    write(&cache, "fx", &declaration, &lists);
    let back = read(&cache, "fx", &declaration).expect("a current write is a hit");
    assert_eq!(back, lists);
    let file: Value =
        serde_json::from_slice(&std::fs::read(cache.join("fx.json")).expect("cache file"))
            .expect("cache parses");
    assert_eq!(file.get("version").and_then(Value::as_u64), Some(1));
    assert!(
        file.to_string().contains("idempotentHint"),
        "the cache holds the raw entry"
    );
}
