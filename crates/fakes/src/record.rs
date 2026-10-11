//! A healthy extension install record beside its manifest: the one helper
//! every test fixture that hand-makes an extension directory shares.

use std::path::Path;

/// Writes a healthy extension install record beside its manifest: `dir`'s
/// `extension.json` gives the name and version (`"v0.0.0"` when it names
/// none), and the record claims a local path source, as installed.
///
/// # Panics
///
/// When the manifest cannot be read or parsed, or the record cannot be
/// written.
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    reason = "a test helper; a failure is the test's"
)]
pub fn write_record(dir: &Path) {
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

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
