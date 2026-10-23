//! The stored images and PDFs a request sends (`docs/model-routing.md`,
//! "Image limits"). The provider does no file I/O, so the loop reads each
//! file the conversation names through `log`, encodes it once per session
//! and hands the provider a map from the conversation's path to the encoded
//! file.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::provider::{Input, Media};
use support::lock;

/// The media of one session: each artifact encoded once, whatever the
/// request that names it. Only a successful encode is kept, so a file that
/// could not be read is looked up again at the next request. An encoding
/// lives while the latest request names its path, so the cache follows the
/// context window, not the files the session has ever held
/// (`docs/performance.md`, "Memory follows the context window").
#[derive(Default)]
pub(crate) struct Cache {
    encoded: Mutex<HashMap<String, Arc<str>>>,
}

impl Cache {
    /// The encoded form of every image, PDF and PDF page `conversation`
    /// names that the session may read and can read, keyed by the path as
    /// the conversation holds it. The lock is held across the read and the
    /// encode, so two calls cannot both miss and encode one path twice.
    /// A path the conversation does not name is never in the map, and an
    /// encoding it no longer names is dropped.
    pub(crate) fn resolve(&self, dir: &Path, conversation: &[Input]) -> Arc<Media> {
        let mut media = Media::new();
        let mut encoded = lock(&self.encoded);
        for path in conversation.iter().flat_map(Input::media_paths) {
            let value = match encoded.get(path) {
                Some(value) => Arc::clone(value),
                None => {
                    let Some(value) = encode(dir, path) else {
                        continue;
                    };
                    encoded.insert(path.to_owned(), Arc::clone(&value));
                    value
                }
            };
            media.insert(path.to_owned(), value);
        }
        encoded.retain(|path, _| media.contains_key(path));
        Arc::new(media)
    }
}

/// The bytes of the file `path` names, as standard base64, or `None` when
/// `log` refuses the path or the file cannot be read.
fn encode(dir: &Path, path: &str) -> Option<Arc<str>> {
    let bytes = log::read_media(dir, path)?;
    Some(Arc::from(STANDARD.encode(bytes)))
}

/// What [`Cache::resolve`] returns for a fresh cache: the media of one send
/// that has no session cache, such as `host.model`'s. It shares the loop's
/// resolve path so the two cannot drift.
pub fn resolve_media(dir: &Path, conversation: &[Input]) -> Arc<Media> {
    Cache::default().resolve(dir, conversation)
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
