//! The cache key, file and miss rules, through the public functions.

use std::collections::BTreeMap;

use fakes::TempDir;
use serde_json::json;

use super::{key, path, read, write};
use crate::server::ListedTool;

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn tools() -> Vec<ListedTool> {
    vec![ListedTool {
        name: "echo".to_owned(),
        description: "Echoes.".to_owned(),
        schema: json!({"type": "object"}),
        hints: crate::effects::Hints {
            read_only: Some(true),
            destructive: None,
            open_world: None,
        },
    }]
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
fn a_write_then_a_read_returns_the_same_tools() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    let declaration = key("fx", &[], &env(&[]));
    write(&cache, "fx", &declaration, &tools());
    assert_eq!(read(&cache, "fx", &declaration), Some(tools()));
}

#[test]
fn a_different_key_is_a_miss() {
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    write(&cache, "fx", &key("fx", &[], &env(&[])), &tools());
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
        write(&cache, name, &declaration, &tools());
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
    write(&cache, "fx", &declaration, &tools());
    assert_eq!(read(&cache, "fx", &declaration), Some(tools()));
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
fn entries_round_trip_through_read() {
    // A bare entry takes the defaults the tool declares, and survives a
    // write and a read unchanged.
    let dir = TempDir::new("fiber-mcp-cache");
    let cache = dir.path().join("mcp");
    let declaration = key("fx", &[], &env(&[]));
    std::fs::create_dir_all(&cache).expect("cache dir");
    std::fs::write(
        cache.join("fx.json"),
        json!({"key": declaration, "tools": [{"name": "bare"}]}).to_string(),
    )
    .expect("bare cache");
    let bare = ListedTool {
        name: "bare".to_owned(),
        description: String::new(),
        schema: json!({"type": "object"}),
        hints: crate::effects::Hints::default(),
    };
    assert_eq!(read(&cache, "fx", &declaration), Some(vec![bare.clone()]));
    write(&cache, "fx", &declaration, std::slice::from_ref(&bare));
    assert_eq!(read(&cache, "fx", &declaration), Some(vec![bare]));
}
