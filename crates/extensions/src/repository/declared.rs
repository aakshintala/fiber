//! Turns what a repository declares into items: each with the files its
//! content hash covers (`docs/extensions.md`, "Pinning").

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use config::RepositoryExtension;
use contract::clock::Clock;
use contract::events::OfferedKind;
use serde_json::{Value, json};

use crate::Error;
use crate::git::GIT_DEADLINE;
use crate::host::exec;
use crate::install::io as io_error;

/// One file an approval pins.
#[derive(Debug, Clone)]
pub(crate) struct PinnedFile {
    /// Its path under the item's base directory, `/`-separated: the package
    /// directory for an extension, the repository root for the rest.
    pub(crate) rel: String,
    /// The file the content was read from: symbolic links resolved, and
    /// inside the repository.
    pub(crate) path: PathBuf,
}

/// An extension, hook or MCP server a repository declares, with what its
/// hash covers.
#[derive(Debug, Clone)]
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
    /// An extension's package directory, resolved.
    pub(crate) dir: PathBuf,
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
            Found::Outside => outside.push(rel),
            Found::Nothing => {}
        }
    }
    Ok(RepoItem {
        kind: OfferedKind::Extension,
        name,
        path: ext.path.clone(),
        required: ext.required,
        declaration: None,
        files: sorted(files),
        outside,
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
    for named in named_paths(entry) {
        match resolve(root, name, &root.join(named))? {
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
                files.insert(rel, path);
            }
            Found::Outside => outside.push(named.to_owned()),
            Found::Nothing => {}
        }
    }
    Ok(RepoItem {
        kind,
        name: name.to_owned(),
        path: path.to_owned(),
        required,
        declaration: Some(json!({"name": name, "entry": entry})),
        files: sorted(files),
        outside,
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

fn sorted(files: BTreeMap<String, PathBuf>) -> Vec<PinnedFile> {
    files
        .into_iter()
        .map(|(rel, path)| PinnedFile { rel, path })
        .collect()
}

/// Where a named path leads.
enum Found {
    /// A regular file inside the repository, resolved.
    Inside(PathBuf),
    /// A regular file outside it.
    Outside,
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
        Found::Outside
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
/// paths under `dir`. `git` stays in Fiber's process group, so terminal
/// prompts still work, and is stopped at the git deadline on `clock`.
fn git_files(name: &str, dir: &Path, clock: &dyn Clock) -> Result<Vec<String>, Error> {
    let fail = |why: String| Error::Pin {
        item: name.to_owned(),
        why,
    };
    let req = exec::ExecRequest {
        program: "git".into(),
        args: vec![
            "-c".to_owned(),
            "core.fsmonitor=false".to_owned(),
            "ls-files".to_owned(),
            "-z".to_owned(),
            "--cached".to_owned(),
            "--others".to_owned(),
            "--exclude-standard".to_owned(),
        ],
        // The directory itself, already canonicalized above: `Command`
        // hands it to the kernel without turning it into text, so a
        // workspace path that is not UTF-8 still reaches `git` intact.
        cwd: dir.to_path_buf(),
        // The file list is not capped today.
        cap: usize::MAX,
        // In Fiber's process group, so terminal prompts still work as they
        // do today.
        own_group: false,
    };
    let deadline = clock.now().checked_add(GIT_DEADLINE).unwrap_or(clock.now());
    // Never cancelled except by the call's own end: the sender drops when
    // this returns.
    let (_cancel, cancel) = mpsc::channel::<()>();
    let out = match exec::run(&req, clock, Some(deadline), cancel) {
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
