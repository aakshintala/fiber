#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::fs;

use serde_json::json;

use super::write_record;

#[test]
fn a_record_names_the_manifest_and_claims_a_path_source() {
    let root = crate::TempDir::new("fiber-record");
    let dir = root.path().join("ext");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": "a", "version": "v2"}).to_string(),
    )
    .unwrap();
    write_record(&dir);
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join(".fiber.json")).unwrap()).unwrap();
    assert_eq!(record.get("name").and_then(|n| n.as_str()), Some("a"));
    assert_eq!(record.get("version").and_then(|v| v.as_str()), Some("v2"));
    assert_eq!(
        record.get("requested").and_then(|r| r.as_bool()),
        Some(true)
    );
    assert_eq!(
        record.get("source"),
        Some(&json!({"path": "/p"})),
        "{record}"
    );
}

#[test]
fn a_manifest_without_a_version_records_zero() {
    let root = crate::TempDir::new("fiber-record-noversion");
    let dir = root.path().join("ext");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("extension.json"), json!({"name": "a"}).to_string()).unwrap();
    write_record(&dir);
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join(".fiber.json")).unwrap()).unwrap();
    assert_eq!(
        record.get("version").and_then(|v| v.as_str()),
        Some("v0.0.0")
    );
}
