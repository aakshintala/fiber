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
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use contract::ErrorCode;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Effects, EffectsError, Output};
use serde_json::{Map, Value};

pub(crate) mod land;
mod locks;

use crate::image::ImageChild;

pub use locks::{PathGuard, PathLocks};

/// Linux stops at 40 symbolic links. Past that the path is a loop, or a chain
/// no call needs.
const MAX_SYMLINKS: u32 = 40;

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
    /// A PNG, JPEG, GIF or WebP file, by its first bytes. The image child
    /// reads it (`docs/tools.md`, "read").
    Image {
        /// Detected type, such as `a PNG image`.
        kind: &'static str,
        /// Size in bytes.
        size: u64,
        /// The hash of the bytes read, for the stale-file check.
        hash: u64,
    },
    /// A PDF file, by its first bytes. The image child counts and cuts it
    /// (`docs/tools.md`, "read").
    Pdf {
        /// Size in bytes.
        size: u64,
        /// The hash of the bytes read, for the stale-file check.
        hash: u64,
    },
    /// A PDF over the 64 MiB cap, by its metadata length and first bytes.
    /// Its bytes were never loaded, so there is no hash and no stale-file
    /// check: `read` refuses it before the child loads it
    /// (`docs/tools.md`, "read").
    PdfOverCap {
        /// Size in bytes, from the file's metadata.
        size: u64,
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
    // A PDF over the cap is refused from its metadata length and first
    // bytes, before its whole bytes are read below or loaded by the child.
    if crate::pdf::pdf_over_cap(size) {
        match pdf_magic(path) {
            Ok(true) => return Ok(Inspected::PdfOverCap { size }),
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(InspectError::NotFound);
            }
            Err(error) => {
                return Err(InspectError::Tool(format!(
                    "`{}` could not be read: {error}.",
                    path.display()
                )));
            }
        }
    }
    let bytes = read_regular(path)?;
    let size = u64::try_from(bytes.len()).map_err(|_| {
        InspectError::Tool(format!(
            "`{}` could not be read: its size does not fit in 64 bits.",
            path.display()
        ))
    })?;
    match magic(&bytes) {
        Some(Magic::Image(kind)) => {
            let hash = hash_bytes(&bytes);
            return Ok(Inspected::Image { kind, size, hash });
        }
        Some(Magic::Pdf) => {
            return Ok(Inspected::Pdf {
                size,
                hash: hash_bytes(&bytes),
            });
        }
        None => {}
    }
    if bytes.contains(&0) {
        return Ok(unsupported("binary data", size, ""));
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(Inspected::Text { text }),
        Err(_) => Ok(unsupported("not UTF-8 text", size, "")),
    }
}

/// Whether `path` starts with PDF magic, reading at most its first four
/// bytes. A shorter file is not a PDF. Errors are the caller's to report,
/// so `inspect` and `write` keep their own sentences.
pub(crate) fn pdf_magic(path: &Path) -> io::Result<bool> {
    use std::io::Read as _;
    let mut head = [0u8; 4];
    match fs::File::open(path)?.read_exact(&mut head) {
        Ok(()) => Ok(head == *b"%PDF"),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

// Stat already classified this as a regular file. NotFound means it disappeared
// before the read; any other error is reported.
fn read_regular(path: &Path) -> Result<Vec<u8>, InspectError> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Err(InspectError::NotFound),
        Err(err) => Err(InspectError::Tool(format!(
            "`{}` could not be read: {err}.",
            path.display()
        ))),
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

/// One session's file tools. `read`, `write` and `edit` share the lock, what
/// the session has seen, and the path permission judged.
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
    /// How to run the image child, set once by [`Files::with_images`].
    images: OnceLock<ImageChild>,
    /// The program that renders a PDF's pages, set once by
    /// [`Files::with_renderer`]. Tests point it at a stub; production
    /// never calls it and the child default (`pdftoppm` on `PATH`) applies.
    renderer: OnceLock<PathBuf>,
}

struct Session {
    seen: BTreeMap<PathBuf, u64>,
    /// debt: one entry per distinct raw path for the session, a measured session where the map's size matters
    judged: BTreeMap<String, PathBuf>,
}

impl Files {
    /// File tools for `workspace`. Nothing has been seen.
    pub fn new(workspace: PathBuf) -> Self {
        Self::with_locks(workspace, Arc::new(PathLocks::new()))
    }

    /// File tools for `workspace`, sharing `locks` with outside holders,
    /// so an extension's `host.fs` contends with the file tools.
    pub fn with_locks(workspace: PathBuf, locks: Arc<PathLocks>) -> Self {
        Self {
            shared: Arc::new(Shared {
                workspace,
                locks,
                state: Mutex::new(Session {
                    seen: BTreeMap::new(),
                    judged: BTreeMap::new(),
                }),
                images: OnceLock::new(),
                renderer: OnceLock::new(),
            }),
        }
    }

    /// Lets `read` process images: it runs `fiber image` with `fiber`, the
    /// running binary, and the child writes the processed file into
    /// `artifacts`, the session's `artifacts/` directory.
    #[must_use]
    pub fn with_images(self, fiber: PathBuf, artifacts: PathBuf) -> Self {
        let mut child = ImageChild::new(fiber, artifacts);
        if let Some(renderer) = self.shared.renderer.get() {
            child.renderer = renderer.clone();
        }
        self.shared.images.set(child).unwrap_or(());
        self
    }

    /// Points `read`'s PDF rendering at `program` instead of `pdftoppm`.
    /// Tests point it at a stub; production never calls it. Set before
    /// [`Files::with_images`], which builds the child with this program.
    #[must_use]
    pub fn with_renderer(self, program: PathBuf) -> Self {
        self.shared.renderer.set(program).unwrap_or(());
        self
    }

    /// A `read` tool sharing this session's state.
    pub fn read(&self) -> crate::read::Read {
        crate::read::Read::new(Arc::clone(&self.shared))
    }

    /// A `write` tool sharing this session's state.
    pub fn write(&self) -> crate::write::Write {
        crate::write::Write::new(Arc::clone(&self.shared))
    }

    /// An `edit` tool sharing this session's state.
    pub fn edit(&self) -> crate::edit::Edit {
        crate::edit::Edit::new(Arc::clone(&self.shared))
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

    pub(crate) fn images(&self) -> Option<&ImageChild> {
        self.images.get()
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
        always_reviewed: false,
    }
}

/// Resolves `raw` against `workspace`. A failure is the call's output.
#[allow(
    clippy::result_large_err,
    reason = "the error is the call's Output, returned unchanged"
)]
pub(crate) fn resolved(workspace: &Path, raw: &str) -> Result<PathBuf, Output> {
    match resolve(workspace, raw) {
        Ok(path) => Ok(path),
        Err(ResolveError::Arguments(message)) => Err(failed(ErrorCode::InvalidArguments, message)),
        Err(ResolveError::Tool(message)) => Err(failed(ErrorCode::ToolError, message)),
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
            retry_after_ms: None,
            provider: None,
        }),
        ..Output::default()
    }
}

/// What a file's first bytes say it is.
enum Magic {
    Image(&'static str),
    Pdf,
}

fn magic(bytes: &[u8]) -> Option<Magic> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Magic::Image("a PNG image"));
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Magic::Image("a JPEG image"));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(Magic::Image("a GIF image"));
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        return Some(Magic::Image("a WebP image"));
    }
    if bytes.starts_with(b"%PDF") {
        return Some(Magic::Pdf);
    }
    None
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
