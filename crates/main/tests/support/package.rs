//! A first-party package copied with its origin rewritten to the fake
//! server, and the OpenRouter listing its tests serve.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Copies the first-party package `providers/<name>` to `to` with every
/// base URL's origin `origin` replaced by `url`, and returns `to`.
pub(crate) fn copy_package(name: &str, to: &Path, origin: &str, url: &str) -> PathBuf {
    let from = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../providers")
        .join(name);
    let mut files = vec!["extension.json".to_owned()];
    for entry in fs::read_dir(from.join("providers")).unwrap() {
        let file = entry.unwrap().file_name();
        files.push(format!("providers/{}", file.to_str().unwrap()));
    }
    // A Lua package's entry script, such as `openrouter`'s `models()`.
    if from.join("init.lua").exists() {
        files.push("init.lua".to_owned());
    }
    for file in files {
        let text = fs::read_to_string(from.join(&file)).unwrap();
        fs::create_dir_all(to.join(&file).parent().unwrap()).unwrap();
        fs::write(to.join(&file), text.replace(origin, url)).unwrap();
    }
    to.to_path_buf()
}

/// The recorded OpenRouter listing `models()` serves in the `ask` tests:
/// the reply body of `models-from-the-recording.json`'s scripted request.
pub(crate) fn openrouter_listing() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../providers/openrouter/tests/models-from-the-recording.json");
    let case: Value =
        serde_json::from_slice(&fs::read(path).unwrap()).expect("the recording case reads");
    case["host"]["http"][0]["reply"]["body"]
        .as_str()
        .expect("the recording case holds one body")
        .as_bytes()
        .to_vec()
}
