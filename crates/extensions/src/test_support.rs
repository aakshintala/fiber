//! Helpers shared by this crate's unit tests.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::path::Path;

/// Writes a healthy extension install record beside its manifest.
pub(crate) fn write_record(dir: &Path) {
    let text = std::fs::read_to_string(dir.join("extension.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    let name = manifest.get("name").and_then(|n| n.as_str()).unwrap();
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("v0.0.0");
    std::fs::write(
        dir.join(".fiber.json"),
        serde_json::json!({"name": name, "version": version, "requested": true, "source": {"path": "/p"}}).to_string(),
    )
    .unwrap();
}
