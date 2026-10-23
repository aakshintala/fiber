//! Turns what a repository declares into items: each with the files its
//! content hash covers (`docs/extensions.md`, "Pinning").

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use config::RepositoryExtension;
use contract::clock::Clock;
use contract::events::OfferedKind;
use serde_json::{Value, json};

use crate::Error;
use crate::git::{GIT_DEADLINE, run_git};
use crate::install::io as io_error;

/// One file an approval pins.
#[derive(Debug, Clone)]
pub(crate) struct PinnedFile {
    /// Its path under the item's base directory, `/`-separated: the package
    /// directory for an extension, the repository root for the rest.
    pub(crate) rel: String,
    /// Its path under the workspace, `/`-separated: what an offer lists.
    pub(crate) listed: String,
    /// The file the content was read from: symbolic links resolved, and
    /// inside the repository.
    pub(crate) path: PathBuf,
}

/// An extension, hook or MCP server a repository declares, with what its
/// hash covers. The declaration and the strings it named can hold
/// argument tokens, so they never print
/// (`docs/code-quality.md`, "What a reviewer checks").
#[derive(Clone)]
pub struct RepoItem {
    /// What it is.
    pub kind: OfferedKind,
    /// Its name: an extension's manifest name, or the declared entry's name.
    pub name: String,
    /// Where the repository declares it: a package's path, or the file that
    /// holds the declaration.
    pub path: String,
    /// Whether the repository marks it `required`.
    pub required: bool,
    /// A hook's or server's `{"name", "entry"}`.
    pub(crate) declaration: Option<Value>,
    /// The files its hash covers, sorted by `rel`.
    pub(crate) files: Vec<PinnedFile>,
    /// Existing files its declaration names that lie outside the repository,
    /// so are not pinned.
    pub(crate) outside: Vec<String>,
    /// What each string the declaration named resolved to when gathered:
    /// inside the repository (its `rel`, whose bytes the hash covers) or
    /// to a file outside it (its canonical target). Empty for extensions.
    /// The start uses these records, never a second resolution, so
    /// retargeting a symbolic link after gathering cannot change what runs.
    pub(crate) named: BTreeMap<String, Named>,
    /// An extension's package directory, resolved.
    pub(crate) dir: PathBuf,
}

impl std::fmt::Debug for RepoItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoItem")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("path", &self.path)
            .field("required", &self.required)
            .field(
                "declaration",
                &self.declaration.as_ref().map(|_| "redacted"),
            )
            .field("files", &self.files)
            .field("outside", &self.outside.len())
            .field("named", &self.named.len())
            .field("dir", &self.dir)
            .finish()
    }
}

/// Where one string a declaration named resolved to when gathered.
#[derive(Debug, Clone)]
pub(crate) enum Named {
    /// A file inside the repository: its `rel`, pinned by the hash.
    Inside(String),
    /// A file outside it: the canonical target path.
    Outside(PathBuf),
}

/// The servers the workspace's `.fiber/` declares, by name, without
/// running `git`: only `git_files` needs a clock, so gathering servers
/// never blocks on it. `declared_items` returns extensions, hooks, then
/// exactly these, so `fiber approve`, the offer and startup hash identically.
pub(crate) fn declared_servers(workspace: &Path) -> Result<Vec<RepoItem>, Error> {
    let declared = config::declared(workspace)?;
    let root = workspace.canonicalize().map_err(io_error(workspace))?;
    let mut items = Vec::new();
    for (name, entry) in &declared.mcp_servers {
        items.push(entry_item(
            &root,
            OfferedKind::McpServer,
            name,
            entry,
            ".fiber/config.json",
            required(entry),
        )?);
    }
    Ok(items)
}

/// One MCP server from a declaration snapshot the caller already holds:
/// the same item `declared_servers` gathers for `name`, without re-reading
/// the declaration file. `record_approval` checks one snapshot against the
/// submitted declaration, then hashes the item built here from that same
/// snapshot, so a replacement between the two never approves the wrong
/// declaration.
pub fn mcp_server_item(workspace: &Path, name: &str, entry: &Value) -> Result<RepoItem, Error> {
    let root = workspace.canonicalize().map_err(io_error(workspace))?;
    entry_item(
        &root,
        OfferedKind::McpServer,
        name,
        entry,
        ".fiber/config.json",
        required(entry),
    )
}

/// Everything the workspace's `.fiber/` declares, in offer order: extensions,
/// then hooks, then MCP servers, each by name. The file list runs through
/// `git`, stopped at the git deadline on `clock`.
pub fn declared_items(workspace: &Path, clock: &dyn Clock) -> Result<Vec<RepoItem>, Error> {
    let declared = config::declared(workspace)?;
    let root = workspace.canonicalize().map_err(io_error(workspace))?;
    let mut items = Vec::new();
    // Where the `hooks` extension's declarations live in a repository.
    let hooks_file = format!(
        ".fiber/config/{}.json",
        config::dir_name(&config::full_name("hooks"))
    );
    for ext in &declared.extensions {
        items.push(extension(&root, ext, clock)?);
    }
    for (name, entry) in &declared.hooks {
        items.push(entry_item(
            &root,
            OfferedKind::Hook,
            name,
            entry,
            &hooks_file,
            required(entry),
        )?);
    }
    // The same servers `declared_servers` gathers, so `fiber approve`,
    // the offer and startup hash identically.
    items.extend(declared_servers(workspace)?);
    Ok(items)
}

/// An entry's `required`: anything but `true` reads as false.
fn required(entry: &Value) -> bool {
    entry
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn extension(root: &Path, ext: &RepositoryExtension, clock: &dyn Clock) -> Result<RepoItem, Error> {
    let bad = |why| Error::BadRepositoryPath {
        path: ext.path.clone(),
        why,
    };
    let given = Path::new(&ext.path);
    if given.is_absolute() {
        return Err(bad("is absolute; it must be relative to the repository"));
    }
    let dir = match root.join(given).canonicalize() {
        Ok(dir) => dir,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(bad("does not exist")),
        Err(e) => return Err(io_error(given)(e)),
    };
    if !dir.starts_with(root) {
        return Err(bad("is outside the repository"));
    }
    if !dir.is_dir() {
        return Err(bad("is not a directory"));
    }
    let manifest = config::read_manifest(&dir)?;
    let name = manifest.name;
    let mut files = BTreeMap::new();
    let mut outside = Vec::new();
    for rel in git_files(&name, &dir, clock)? {
        let candidate = dir.join(&rel);
        match resolve(root, &name, &candidate)? {
            Found::Inside(path) => {
                files.insert(rel, path);
            }
            Found::Outside(_) => outside.push(rel),
            Found::Nothing => {}
        }
    }
    let base = dir
        .strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .ok_or_else(|| Error::Pin {
            item: name.clone(),
            why: format!("{} is not a UTF-8 path", dir.display()),
        })?;
    Ok(RepoItem {
        kind: OfferedKind::Extension,
        name,
        path: ext.path.clone(),
        required: ext.required,
        declaration: None,
        files: sorted(files, base),
        outside,
        named: BTreeMap::new(),
        dir,
    })
}

/// A hook or a server: its declaration, and the files its `command` and
/// `args` name.
fn entry_item(
    root: &Path,
    kind: OfferedKind,
    name: &str,
    entry: &Value,
    path: &str,
    required: bool,
) -> Result<RepoItem, Error> {
    let mut files = BTreeMap::new();
    let mut outside = Vec::new();
    let mut named = BTreeMap::new();
    for given in named_paths(entry) {
        match resolve(root, name, &root.join(given))? {
            Found::Inside(path) => {
                let rel = path
                    .strip_prefix(root)
                    .ok()
                    .and_then(Path::to_str)
                    .ok_or_else(|| Error::Pin {
                        item: name.to_owned(),
                        why: format!("{} is not a UTF-8 path", path.display()),
                    })?
                    .to_owned();
                files.insert(rel.clone(), path);
                named.insert(given.to_owned(), Named::Inside(rel));
            }
            Found::Outside(target) => {
                outside.push(given.to_owned());
                named.insert(given.to_owned(), Named::Outside(target));
            }
            Found::Nothing => {}
        }
    }
    Ok(RepoItem {
        kind,
        name: name.to_owned(),
        path: path.to_owned(),
        required,
        declaration: Some(json!({"name": name, "entry": entry})),
        files: sorted(files, ""),
        outside,
        named,
        dir: PathBuf::new(),
    })
}

/// The strings of a declaration that can name a file: a `command` with a
/// `/` in it (a bare word is looked up on the `PATH`), and every `args`
/// entry.
fn named_paths(entry: &Value) -> Vec<&str> {
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| command.contains('/'));
    let args = entry
        .get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    command.into_iter().chain(args).collect()
}

fn sorted(files: BTreeMap<String, PathBuf>, base: &str) -> Vec<PinnedFile> {
    files
        .into_iter()
        .map(|(rel, path)| {
            let listed = if base.is_empty() {
                rel.clone()
            } else {
                format!("{base}/{rel}")
            };
            PinnedFile { rel, listed, path }
        })
        .collect()
}

/// Where a named path leads.
enum Found {
    /// A regular file inside the repository, resolved.
    Inside(PathBuf),
    /// A regular file outside it: its resolved path.
    Outside(PathBuf),
    /// No file: missing, a directory, or a string that cannot be a path.
    Nothing,
}

/// Resolves `candidate`, symbolic links included, and says whether it is a
/// regular file inside `root`. The resolved path is the one to read: a path
/// is opened only after this check (`content.rs` records what that leaves).
fn resolve(root: &Path, item: &str, candidate: &Path) -> Result<Found, Error> {
    let fail = |e: io::Error| Error::Pin {
        item: item.to_owned(),
        why: format!("{}: {e}", candidate.display()),
    };
    let names_no_file = |e: &io::Error| {
        matches!(
            e.kind(),
            ErrorKind::NotFound
                | ErrorKind::NotADirectory
                | ErrorKind::InvalidInput
                | ErrorKind::InvalidFilename
        )
    };
    // Only a path that names nothing, or names something that is not a
    // regular file, is skipped; any other failure is the item's.
    match fs::metadata(candidate) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(Found::Nothing),
        Err(e) if names_no_file(&e) => return Ok(Found::Nothing),
        Err(e) => return Err(fail(e)),
    }
    let resolved = candidate.canonicalize().map_err(fail)?;
    Ok(if resolved.starts_with(root) {
        Found::Inside(resolved)
    } else {
        Found::Outside(resolved)
    })
}

/// A failure to start `git`: it is not installed, or something else.
pub(super) fn spawn_error(dir: &Path, e: io::Error) -> Error {
    if e.kind() == ErrorKind::NotFound {
        Error::GitMissing
    } else {
        io_error(dir)(e)
    }
}

/// The files of a package: tracked, plus untracked and not ignored, as
/// paths under `dir`. `git` stays in Fiber's process group, so SSH
/// prompts and credential-helper prompts on the terminal still work, and
/// is stopped at the git deadline on `clock`.
fn git_files(name: &str, dir: &Path, clock: &dyn Clock) -> Result<Vec<String>, Error> {
    let fail = |why: String| Error::Pin {
        item: name.to_owned(),
        why,
    };
    // The directory itself, already canonicalized above: `Command` hands
    // it to the kernel without turning it into text, so a workspace path
    // that is not UTF-8 still reaches `git` intact.
    let out = match run_git(
        "git",
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
        dir,
        clock,
    ) {
        Err(failed) => match failed.source {
            Some(source) => return Err(spawn_error(dir, source)),
            // After the spawn: a reader thread that could not start.
            None => return Err(fail(failed.message)),
        },
        Ok(ran) if ran.timed_out => {
            return Err(fail(format!(
                "`git ls-files` did not finish within {} s, so it was stopped",
                GIT_DEADLINE.as_secs()
            )));
        }
        Ok(ran) => ran,
    };
    if out.exit_code != Some(0) {
        return Err(fail(format!(
            "{} is not in a git repository, so its files cannot be listed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    out.stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8(path.to_vec())
                .map_err(|_| fail("a file name is not UTF-8".to_owned()))
        })
        .collect()
}
