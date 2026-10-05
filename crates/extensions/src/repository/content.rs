//! The content hash of an item, and `pinned.json`, the index that lets a
//! session hash a file again only when its size or modification time changed
//! (`docs/state.md`, "What each part holds"; `docs/performance.md`).
//!
//! The hash is the lowercase hex SHA-256 over, in order: the kind's name and
//! a NUL; for a hook or a server, its declaration as compact JSON with keys
//! sorted, behind an 8-byte little-endian length; then, for each pinned file
//! sorted by path bytes, the path (relative to the package directory for an
//! extension, to the repository root otherwise, `/`-separated) behind an
//! 8-byte length, one byte that is 1 when any execute bit is set and 0
//! otherwise, and the file's own SHA-256. A file's own hash covers its
//! content only, which is what the index stores. The format is internal.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use contract::events::OfferedKind;
use ring::digest::{Context, SHA256};
use serde_json::{Map, Value, json};

use super::declared::RepoItem;
use super::kind_name;
use crate::Error;
use crate::install::io as io_error;

// debt: a file opened after its path was resolved and checked inside the
// repository can be swapped for a link to outside in the window between the
// two, by a concurrent local writer; fixed when a measured need arises to
// defend a repository against a concurrent local writer.
// debt: a file changed within the same modification-time tick and byte length
// as when it was hashed is taken as unchanged; fixed when a miss is measured.

/// How much of a file is read at a time.
pub(super) const CHUNK: usize = 64 * 1024;

/// A file's own SHA-256.
pub(crate) type Digest = [u8; 32];

/// What the index records of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    size: u64,
    mtime_ns: u64,
    hash: Digest,
}

/// `pinned.json`: each declared path's size, modification time and hash. An
/// index, not a record: a missing or damaged file costs a re-hash and
/// nothing else.
#[derive(Debug)]
pub struct Index {
    file: Option<PathBuf>,
    entries: BTreeMap<String, Entry>,
    dirty: bool,
}

impl Index {
    /// Reads `pinned.json` in Fiber home. A file that is missing or does not
    /// read is an empty index.
    pub fn load(home: &Path) -> Self {
        let file = home.join("pinned.json");
        let entries = fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .map(|value| parse(&value))
            .unwrap_or_default();
        Self {
            file: Some(file),
            entries,
            dirty: false,
        }
    }

    /// An index that is never saved.
    pub(crate) fn scratch() -> Self {
        Self {
            file: None,
            entries: BTreeMap::new(),
            dirty: false,
        }
    }

    /// Writes the index when it changed: to a temporary file in Fiber home,
    /// renamed over `pinned.json`. Two sessions racing leave the last
    /// writer's, and a lost entry costs one re-hash.
    pub fn save(&mut self) -> Result<(), Error> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        if !self.dirty {
            return Ok(());
        }
        let mut map = Map::new();
        for (path, entry) in &self.entries {
            map.insert(
                path.clone(),
                json!({
                    "size": entry.size,
                    "mtime_ns": entry.mtime_ns,
                    "hash": hex(&entry.hash),
                }),
            );
        }
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let temp = file.with_extension(format!(
            "json.tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let written =
            fs::write(&temp, Value::Object(map).to_string()).and_then(|()| fs::rename(&temp, file));
        if let Err(e) = written {
            fs::remove_file(&temp).unwrap_or(());
            return Err(io_error(file)(e));
        }
        self.dirty = false;
        Ok(())
    }
}

fn parse(value: &Value) -> BTreeMap<String, Entry> {
    let mut entries = BTreeMap::new();
    for (path, entry) in value.as_object().into_iter().flatten() {
        let parsed = (|| {
            Some(Entry {
                size: entry.get("size")?.as_u64()?,
                mtime_ns: entry.get("mtime_ns")?.as_u64()?,
                hash: from_hex(entry.get("hash")?.as_str()?)?,
            })
        })();
        if let Some(parsed) = parsed {
            entries.insert(path.clone(), parsed);
        }
    }
    entries
}

/// The content hash of an item: see the module's description.
pub fn hash(index: &mut Index, item: &RepoItem) -> Result<String, Error> {
    hash_paths(
        index,
        &item.name,
        item.kind,
        item.declaration.as_ref(),
        item.files
            .iter()
            .map(|f| (f.rel.as_str(), f.path.as_path())),
    )
}

/// The hash of `files`, each a path under the item's base directory and where
/// to read it. The copy's check calls this on the copy, with a scratch index.
pub(crate) fn hash_paths<'a>(
    index: &mut Index,
    item: &str,
    kind: OfferedKind,
    declaration: Option<&Value>,
    files: impl Iterator<Item = (&'a str, &'a Path)>,
) -> Result<String, Error> {
    let mut parts: Vec<(&str, bool, Digest)> = Vec::new();
    for (rel, path) in files {
        let (exec, digest) = file_digest(index, item, path)?;
        parts.push((rel, exec, digest));
    }
    parts.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut ctx = Context::new(&SHA256);
    ctx.update(kind_name(kind).as_bytes());
    ctx.update(&[0]);
    if let Some(declaration) = declaration {
        // `serde_json` without `preserve_order` writes keys sorted.
        let text = declaration.to_string();
        ctx.update(&(text.len() as u64).to_le_bytes());
        ctx.update(text.as_bytes());
    }
    for (rel, exec, digest) in parts {
        ctx.update(&(rel.len() as u64).to_le_bytes());
        ctx.update(rel.as_bytes());
        ctx.update(&[u8::from(exec)]);
        ctx.update(&digest);
    }
    Ok(hex(ctx.finish().as_ref()))
}

/// Whether any execute bit is set, and the file's own hash: from the index
/// when its size and modification time are what was recorded, else read in
/// chunks and recorded.
fn file_digest(index: &mut Index, item: &str, path: &Path) -> Result<(bool, Digest), Error> {
    let fail = |e: io::Error| Error::Pin {
        item: item.to_owned(),
        why: format!("{}: {e}", path.display()),
    };
    let meta = fs::metadata(path).map_err(fail)?;
    let exec = meta.mode() & 0o111 != 0;
    let key = path.to_string_lossy().into_owned();
    if let Some(mtime_ns) = mtime_ns(&meta)
        && let Some(entry) = index.entries.get(&key)
        && entry.size == meta.len()
        && entry.mtime_ns == mtime_ns
    {
        return Ok((exec, entry.hash));
    }
    let mut file = File::open(path).map_err(fail)?;
    let digest = digest_file(&mut file).map_err(fail)?;
    // The recorded size and time are the opened file's, so an entry never
    // pairs one version's stat with another's content.
    let opened = file.metadata().map_err(fail)?;
    if let Some(mtime_ns) = mtime_ns(&opened) {
        index.entries.insert(
            key,
            Entry {
                size: opened.len(),
                mtime_ns,
                hash: digest,
            },
        );
        index.dirty = true;
    }
    Ok((exec, digest))
}

/// Nanoseconds since the epoch; none before it, or past what a `u64` holds,
/// so such a time is never trusted.
fn mtime_ns(meta: &fs::Metadata) -> Option<u64> {
    let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(since.as_nanos()).ok()
}

/// A file's SHA-256, read in chunks.
pub(crate) fn digest_file(file: &mut impl Read) -> io::Result<Digest> {
    let mut ctx = Context::new(&SHA256);
    let mut buf = vec![0_u8; CHUNK];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if let Some(chunk) = buf.get(..n) {
            ctx.update(chunk);
        }
    }
    <Digest>::try_from(ctx.finish().as_ref())
        .map_err(|_| io::Error::other("a digest of the wrong length"))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Digest> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0_u8; 32];
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}
