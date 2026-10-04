//! Shared state for the file tools (`docs/tools.md`, "File tools"): the
//! workspace, the per-path lock, what this session has seen, and the path
//! permission judged.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use contract::ErrorCode;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Effects, EffectsError, Output};
use serde_json::{Map, Value};

pub(crate) mod land;
mod locks;

pub use locks::{PathGuard, PathLocks};

/// Linux stops at 40 symbolic links. Past that the path is a loop, or a chain
/// no call needs.
const MAX_SYMLINKS: u32 = 40;

const IMAGE_HINT: &str = "Images and PDFs are not read yet.";
const DIRECTORY_HINT: &str = "List it through the shell.";

/// Why a path could not be resolved.
#[derive(Debug)]
pub(crate) enum ResolveError {
    /// `..` in the part of the path that does not exist yet.
    Arguments(String),
    /// Permission, a symlink loop, or a parent that is a file.
    Tool(String),
}

/// What a resolved path is, without opening a fifo, socket or device.
#[derive(Debug)]
pub(crate) enum Inspected {
    /// UTF-8 text, byte order mark included.
    Text {
        /// The file's text.
        text: String,
    },
    /// A directory, device or file the tool will not read.
    Unsupported {
        /// Detected type, such as `a directory` or `binary data`.
        kind: String,
        /// Size in bytes.
        size: u64,
        /// Extra sentence, or empty.
        hint: &'static str,
    },
}

/// Why inspection failed.
#[derive(Debug)]
pub(crate) enum InspectError {
    /// The path does not exist.
    NotFound,
    /// A filesystem failure. The message names the path.
    Tool(String),
}

/// A relative path joins `workspace`. The longest existing prefix is
/// canonicalised and the rest is appended. A `..` in that rest cannot be
/// resolved honestly.
pub(crate) fn resolve(workspace: &Path, raw: &str) -> Result<PathBuf, ResolveError> {
    let given = Path::new(raw);
    let joined = if given.is_absolute() {
        given.to_path_buf()
    } else {
        workspace.join(given)
    };
    let mut pending: VecDeque<Piece> = joined.components().map(Piece::from).collect();
    let mut out = PathBuf::new();
    let mut missing = false;
    let mut follows = 0u32;
    while let Some(piece) = pending.pop_front() {
        match piece {
            Piece::Root(root) => out = root,
            Piece::CurDir => {}
            Piece::ParentDir => {
                if missing {
                    return Err(ResolveError::Arguments(format!(
                        "`{raw}` cannot be resolved: `..` points through a directory that does not exist."
                    )));
                }
                // Popping `/` fails and leaves it in place, which is what `/..` means.
                if !out.pop() && out.as_os_str().is_empty() {
                    out.push("..");
                }
            }
            Piece::Normal(name) => {
                if missing {
                    out.push(&name);
                    continue;
                }
                let candidate = out.join(&name);
                match fs::symlink_metadata(&candidate) {
                    Ok(meta) => {
                        let file_type = meta.file_type();
                        if file_type.is_symlink() {
                            follows += 1;
                            if follows > MAX_SYMLINKS {
                                return Err(ResolveError::Tool(format!(
                                    "`{raw}` could not be resolved: too many levels of symbolic links."
                                )));
                            }
                            let target = fs::read_link(&candidate).map_err(|err| {
                                ResolveError::Tool(format!(
                                    "`{}` could not be resolved: {err}.",
                                    candidate.display()
                                ))
                            })?;
                            if target.as_os_str().is_empty() {
                                return Err(ResolveError::Tool(format!(
                                    "`{}` could not be resolved: the symbolic link is empty.",
                                    candidate.display()
                                )));
                            }
                            for piece in target.components().map(Piece::from).rev() {
                                pending.push_front(piece);
                            }
                        } else if !file_type.is_dir() && !pending.is_empty() {
                            // A later `..` would discard this file and keep resolving.
                            return Err(ResolveError::Tool(format!(
                                "`{raw}` could not be resolved: `{}` is not a directory.",
                                candidate.display()
                            )));
                        } else {
                            out.push(&name);
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        missing = true;
                        out.push(&name);
                    }
                    Err(err) => {
                        return Err(ResolveError::Tool(format!(
                            "`{}` could not be resolved: {err}.",
                            candidate.display()
                        )));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// One path component that owns its bytes, so a symlink target can be queued.
enum Piece {
    Root(PathBuf),
    CurDir,
    ParentDir,
    Normal(OsString),
}

impl From<Component<'_>> for Piece {
    fn from(component: Component<'_>) -> Self {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                Self::Root(PathBuf::from(component.as_os_str()))
            }
            Component::CurDir => Self::CurDir,
            Component::ParentDir => Self::ParentDir,
            Component::Normal(name) => Self::Normal(name.to_os_string()),
        }
    }
}

/// Classifies `path` from its metadata, then its bytes. A fifo, socket or
/// device is never opened.
pub(crate) fn inspect(path: &Path) -> Result<Inspected, InspectError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(InspectError::NotFound),
        Err(err) => {
            return Err(InspectError::Tool(format!(
                "`{}` could not be read: {err}.",
                path.display()
            )));
        }
    };
    let size = meta.len();
    if let Some((kind, hint)) = kind_of(meta.file_type()) {
        return Ok(unsupported(kind, size, hint));
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(InspectError::NotFound),
        Err(err) => {
            return Err(InspectError::Tool(format!(
                "`{}` could not be read: {err}.",
                path.display()
            )));
        }
    };
    let size = u64::try_from(bytes.len()).map_err(|_| {
        InspectError::Tool(format!(
            "`{}` could not be read: its size does not fit in 64 bits.",
            path.display()
        ))
    })?;
    if let Some(kind) = magic(&bytes) {
        return Ok(unsupported(kind, size, IMAGE_HINT));
    }
    if bytes.contains(&0) {
        return Ok(unsupported("binary data", size, ""));
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(Inspected::Text { text }),
        Err(_) => Ok(unsupported("not UTF-8 text", size, "")),
    }
}

/// Kind and hint of anything that is not a regular file. A regular file is
/// `None`; the caller reads its bytes.
pub(crate) fn kind_of(file_type: fs::FileType) -> Option<(&'static str, &'static str)> {
    if file_type.is_file() {
        None
    } else if file_type.is_dir() {
        Some(("a directory", DIRECTORY_HINT))
    } else if file_type.is_fifo() {
        Some(("a fifo", ""))
    } else if file_type.is_socket() {
        Some(("a socket", ""))
    } else if file_type.is_block_device() || file_type.is_char_device() {
        Some(("a device", ""))
    } else if file_type.is_symlink() {
        Some(("a symbolic link", ""))
    } else {
        Some(("a special file", ""))
    }
}

/// The sentence a file tool returns for [`Inspected::Unsupported`].
pub(crate) fn unsupported_message(path: &Path, kind: &str, size: u64, hint: &str) -> String {
    if hint.is_empty() {
        format!("`{}` is {kind} ({size} bytes).", path.display())
    } else {
        format!("`{}` is {kind} ({size} bytes). {hint}", path.display())
    }
}

fn unsupported(kind: &str, size: u64, hint: &'static str) -> Inspected {
    Inspected::Unsupported {
        kind: kind.to_owned(),
        size,
        hint,
    }
}

/// One session's file tools. `read` and `write` share the lock, what the
/// session has seen, and the path permission judged.
pub struct Files {
    shared: Arc<Shared>,
}

/// State shared by the file tools of one session.
pub(crate) struct Shared {
    workspace: PathBuf,
    locks: Arc<PathLocks>,
    /// Seen hashes and judged paths. One map, so a replace observes the hash
    /// a read in this session recorded.
    state: Mutex<Session>,
}

struct Session {
    seen: BTreeMap<PathBuf, u64>,
    /// debt: one entry per distinct raw path for the session, a measured session where the map's size matters
    judged: BTreeMap<String, PathBuf>,
}

impl Files {
    /// File tools for `workspace`. Nothing has been seen.
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            shared: Arc::new(Shared {
                workspace,
                locks: Arc::new(PathLocks::new()),
                state: Mutex::new(Session {
                    seen: BTreeMap::new(),
                    judged: BTreeMap::new(),
                }),
            }),
        }
    }

    /// A `read` tool sharing this session's state.
    pub fn read(&self) -> crate::read::Read {
        crate::read::Read::new(Arc::clone(&self.shared))
    }

    /// A `write` tool sharing this session's state.
    pub fn write(&self) -> crate::write::Write {
        crate::write::Write::new(Arc::clone(&self.shared))
    }

    /// The per-path lock file-mutating tools take.
    pub fn locks(&self) -> Arc<PathLocks> {
        Arc::clone(&self.shared.locks)
    }

    /// Clears what the session has seen. A handoff does this; nothing calls
    /// it from here yet.
    pub fn forget(&self) {
        session(&self.shared.state).seen.clear();
    }
}

#[cfg(test)]
impl Files {
    pub(crate) fn seen_hash(&self, path: &Path) -> Option<u64> {
        self.shared.seen(path)
    }
}

impl Shared {
    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn locks(&self) -> Arc<PathLocks> {
        Arc::clone(&self.locks)
    }

    pub(crate) fn note_judged(&self, raw: &str, resolved: &Path) {
        session(&self.state)
            .judged
            .insert(raw.to_owned(), resolved.to_path_buf());
    }

    pub(crate) fn judged(&self, raw: &str) -> Option<PathBuf> {
        session(&self.state).judged.get(raw).cloned()
    }

    pub(crate) fn seen(&self, path: &Path) -> Option<u64> {
        session(&self.state).seen.get(path).copied()
    }

    pub(crate) fn set_seen(&self, path: &Path, hash: u64) {
        session(&self.state).seen.insert(path.to_path_buf(), hash);
    }
}

fn session(mutex: &Mutex<Session>) -> MutexGuard<'_, Session> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A hash of the file's bytes. In-process only: nothing persists or logs it.
///
/// debt: a 64-bit hash collision lets a changed file look seen, a measured collision in a session
pub(crate) fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// The path as text. A non-UTF-8 byte is escaped rather than rejected.
pub(crate) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The directory a rule would offer, ending in `/`.
pub(crate) fn dir_prefix(path: &Path) -> String {
    match path.parent() {
        Some(parent) => {
            let mut text = path_text(parent);
            if !text.ends_with('/') {
                text.push('/');
            }
            text
        }
        None => "/".to_owned(),
    }
}

pub(crate) fn declare(effect: Effect, reversible: bool, resolved: &Path) -> Effects {
    let path = path_text(resolved);
    Effects {
        declared: DeclaredEffects {
            effects: vec![effect],
            reversible,
            paths: Some(vec![path.clone()]),
        },
        subject: Some(path),
        prefix: Some(dir_prefix(resolved)),
    }
}

pub(crate) fn effects_error(error: ResolveError) -> EffectsError {
    match error {
        ResolveError::Arguments(message) | ResolveError::Tool(message) => {
            EffectsError::Arguments(message)
        }
    }
}

pub(crate) fn string_argument(
    arguments: &Map<String, Value>,
    key: &str,
    missing: &str,
) -> Result<String, String> {
    match arguments.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(format!("`{key}` must be a string.")),
        None => Err(missing.to_owned()),
    }
}

pub(crate) fn text_output(text: String) -> Output {
    Output {
        content: vec![ContentPart::Text { text }],
        ..Output::default()
    }
}

pub(crate) fn failed(code: ErrorCode, message: String) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: format!("{message}\n"),
        }],
        error: Some(Failure {
            code,
            message,
            retry_after: None,
            provider: None,
        }),
        ..Output::default()
    }
}

fn magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("a PNG image");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("a JPEG image");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("a GIF image");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        return Some("a WebP image");
    }
    if bytes.starts_with(b"%PDF") {
        return Some("a PDF");
    }
    None
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
