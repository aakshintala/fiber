//! `host.fs` and `host.data_dir` (`docs/extensions.md`, "Host calls").

use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use contract::files::PathLock;
use mlua::{Lua, MultiValue, Table, Value as LuaValue};

use super::{Session, failure};

/// Installs `host.fs` and `host.data_dir` on `host`. `session` is the
/// extension's session: its project names `projects/<key>/` in Fiber home
/// and its lock is the session's per-path lock. Without one
/// (`LuaExtension::with_session`), relative paths resolve against the
/// process's current directory, `lock = true` takes no lock, and
/// `host.data_dir` raises. `memory_cap` bounds `read`: a file larger
/// than the cap is refused, never read into Lua past its limit.
/// What `install` needs: the workspace relative `host.fs` paths resolve
/// against, the Fiber home holding the data directories, the extension's
/// name, its memory cap bounding `read`, and its session, if it has one.
pub(crate) struct Ctx<'a> {
    /// The session's workspace, resolving relative paths.
    pub workspace: PathBuf,
    /// Fiber home, anchoring the data directories.
    pub home: PathBuf,
    /// The extension's name, slugged for data directories.
    pub extension: &'a str,
    /// The extension's memory cap in bytes, bounding `read`.
    pub memory_cap: usize,
    /// The extension's session, if it has one.
    pub session: Option<&'a Session>,
}

pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    ctx: Ctx<'_>,
    failure: &mlua::Function,
) -> mlua::Result<()> {
    let Ctx {
        workspace,
        home,
        extension,
        memory_cap,
        session,
    } = ctx;
    let slug = config::dir_name(extension);
    let project = session
        .as_ref()
        .map(|session| session.config.extensions().project());
    let (machine_dir, project_dir) = match project {
        Some(key) => (
            Some(config::extension_data_dir(&home, &slug)),
            Some(config::project_data_dir(&home, key, &slug)),
        ),
        None => (None, None),
    };
    let fs = Fs {
        workspace,
        locks: session.as_ref().map(|session| Arc::clone(&session.locks)),
        machine_dir,
        project_dir,
        memory_cap,
    };
    let table = lua.create_table()?;
    // Each method returns its value on success and a `Raise` on failure,
    // raised as the table or the string at the call site, so
    // `coroutine.resume` sees the value at its source.
    {
        let fs = fs.clone();
        failure::register(
            lua,
            &table,
            "read",
            failure,
            None,
            move |lua, path: LuaValue| {
                let path = path_bytes(&path, "host.fs.read")?;
                let data = fs
                    .read(&path)
                    .map_err(|(code, message)| failure::Raise::Failed(code, message))?;
                Ok(MultiValue::from_vec(vec![LuaValue::String(
                    lua.create_string(data)
                        .map_err(|e| failure::Raise::Arg(e.to_string()))?,
                )]))
            },
        )?;
    }
    {
        let fs = fs.clone();
        failure::register(
            lua,
            &table,
            "write",
            failure,
            None,
            move |_lua, (path, data, opts): (LuaValue, LuaValue, LuaValue)| {
                let path = path_bytes(&path, "host.fs.write")?;
                let LuaValue::String(data) = &data else {
                    return Err(failure::Raise::Arg(
                        "host.fs.write: data must be a string".to_owned(),
                    ));
                };
                let data = data.as_bytes();
                let with_lock = lock(&opts)?;
                raw_unit(fs.write(&path, &data, with_lock))
            },
        )?;
    }
    {
        let fs = fs.clone();
        failure::register(
            lua,
            &table,
            "list",
            failure,
            None,
            move |lua, path: LuaValue| {
                let path = path_bytes(&path, "host.fs.list")?;
                let names = fs
                    .list(&path)
                    .map_err(|(code, message)| failure::Raise::Failed(code, message))?;
                let out = lua
                    .create_table()
                    .map_err(|e| failure::Raise::Arg(e.to_string()))?;
                for (i, name) in names.iter().enumerate() {
                    out.raw_set(
                        i + 1,
                        lua.create_string(name)
                            .map_err(|e| failure::Raise::Arg(e.to_string()))?,
                    )
                    .map_err(|e| failure::Raise::Arg(e.to_string()))?;
                }
                Ok(MultiValue::from_vec(vec![LuaValue::Table(out)]))
            },
        )?;
    }
    {
        let fs = fs.clone();
        failure::register(
            lua,
            &table,
            "stat",
            failure,
            None,
            move |lua, path: LuaValue| {
                let path = path_bytes(&path, "host.fs.stat")?;
                let found = fs
                    .stat(&path)
                    .map_err(|(code, message)| failure::Raise::Failed(code, message))?;
                let Some(found) = found else {
                    return Ok(MultiValue::from_vec(vec![LuaValue::Nil]));
                };
                let out = lua
                    .create_table()
                    .map_err(|e| failure::Raise::Arg(e.to_string()))?;
                out.raw_set("kind", found.kind)
                    .map_err(|e| failure::Raise::Arg(e.to_string()))?;
                out.raw_set("size", found.size)
                    .map_err(|e| failure::Raise::Arg(e.to_string()))?;
                out.raw_set("modified_ms", found.modified_ms)
                    .map_err(|e| failure::Raise::Arg(e.to_string()))?;
                Ok(MultiValue::from_vec(vec![LuaValue::Table(out)]))
            },
        )?;
    }
    {
        let fs = fs.clone();
        failure::register(
            lua,
            &table,
            "mkdir",
            failure,
            None,
            move |_lua, (path, opts): (LuaValue, LuaValue)| {
                let path = path_bytes(&path, "host.fs.mkdir")?;
                let with_lock = lock(&opts)?;
                raw_unit(fs.mkdir(&path, with_lock))
            },
        )?;
    }
    {
        let fs = fs.clone();
        failure::register(
            lua,
            &table,
            "remove",
            failure,
            None,
            move |_lua, (path, opts): (LuaValue, LuaValue)| {
                let path = path_bytes(&path, "host.fs.remove")?;
                let with_lock = lock(&opts)?;
                raw_unit(fs.remove(&path, with_lock))
            },
        )?;
    }
    {
        let fs = fs.clone();
        failure::register(
            lua,
            &table,
            "rename",
            failure,
            None,
            move |_lua, (from, to, opts): (LuaValue, LuaValue, LuaValue)| {
                let from = path_bytes(&from, "host.fs.rename")?;
                let to = path_bytes(&to, "host.fs.rename")?;
                let with_lock = lock(&opts)?;
                raw_unit(fs.rename(&from, &to, with_lock))
            },
        )?;
    }
    host.set("fs", table)?;
    {
        let fs = fs.clone();
        failure::register(
            lua,
            host,
            "data_dir",
            failure,
            None,
            move |lua, scope: LuaValue| {
                let LuaValue::String(scope) = &scope else {
                    return Err(failure::Raise::Arg(
                        "host.data_dir: scope must be \"machine\" or \"project\"".to_owned(),
                    ));
                };
                let dir = fs
                    .data_dir(&scope.as_bytes())
                    .map_err(failure::Raise::Arg)?;
                Ok(MultiValue::from_vec(vec![LuaValue::String(
                    lua.create_string(dir.as_os_str().as_bytes())
                        .map_err(|e| failure::Raise::Arg(e.to_string()))?,
                )]))
            },
        )?;
    }
    Ok(())
}

/// A filesystem failure: its code and the message `host.fs` raises.
type Failed = (contract::ErrorCode, String);

/// A mutating call's answer: nothing on success, the coded failure on
/// failure.
fn raw_unit(result: Result<(), Failed>) -> Result<MultiValue, failure::Raise> {
    match result {
        Ok(()) => Ok(MultiValue::from_vec(vec![])),
        Err((code, message)) => Err(failure::Raise::Failed(code, message)),
    }
}

fn path_bytes(value: &LuaValue, call: &str) -> Result<Vec<u8>, failure::Raise> {
    if let LuaValue::String(path) = value {
        Ok(path.as_bytes().to_vec())
    } else {
        Err(failure::Raise::Arg(bad_path(call)))
    }
}

fn bad_path(call: &str) -> String {
    format!("{call}: path must be a string")
}

/// The `lock` option of a mutating call: absent or missing is no lock, and a
/// boolean is itself. Anything else, such as `"yes"`, a table or a number,
/// is the caller's error, as the message to raise as a string.
fn lock(opts: &LuaValue) -> Result<bool, failure::Raise> {
    match opts {
        LuaValue::Nil => Ok(false),
        LuaValue::Table(opts) => {
            let not_boolean =
                || failure::Raise::Arg("host.fs: `lock` must be a boolean".to_owned());
            let value = opts.get::<LuaValue>("lock").map_err(|_| not_boolean())?;
            if value.is_nil() {
                return Ok(false);
            }
            value.as_boolean().ok_or_else(not_boolean)
        }
        LuaValue::Boolean(_)
        | LuaValue::LightUserData(_)
        | LuaValue::Integer(_)
        | LuaValue::Number(_)
        | LuaValue::String(_)
        | LuaValue::Function(_)
        | LuaValue::Thread(_)
        | LuaValue::UserData(_)
        | LuaValue::Error(_)
        | LuaValue::Other(_) => Err(failure::Raise::Arg(
            "host.fs: options must be a table".to_owned(),
        )),
    }
}

/// What `stat` reports for a path that exists.
pub(crate) struct Stat {
    pub kind: &'static str,
    pub size: i64,
    pub modified_ms: i64,
}

/// The files one extension reaches through `host.fs`: its workspace for
/// relative paths, the session's lock when one is offered, its two data
/// directories, created on first write, and the bound `read` refuses past.
#[derive(Clone)]
pub(crate) struct Fs {
    workspace: PathBuf,
    locks: Option<Arc<dyn PathLock>>,
    machine_dir: Option<PathBuf>,
    project_dir: Option<PathBuf>,
    memory_cap: usize,
}

/// What a process extension may read at once: the same 1 MiB the Lua
/// half allows, so a base64 answer fits the wire framing with room to
/// spare.
pub(crate) const PROCESS_READ_CAP: usize = 1 << 20;

impl Fs {
    /// The `host.fs` handle for a process extension: the workspace its
    /// paths resolve under, the folded data directories and the process
    /// read bound. Without a session there are no path locks to hold.
    pub(crate) fn for_process(
        workspace: PathBuf,
        machine_dir: String,
        project_dir: String,
    ) -> Self {
        Self {
            workspace,
            locks: None,
            machine_dir: Some(PathBuf::from(machine_dir)),
            project_dir: Some(PathBuf::from(project_dir)),
            memory_cap: PROCESS_READ_CAP,
        }
    }

    /// The `host.fs` handle for a process extension with a session: as
    /// above, holding the session's path locks for locked calls.
    pub(crate) fn for_process_with_locks(
        workspace: PathBuf,
        machine_dir: String,
        project_dir: String,
        locks: Arc<dyn PathLock>,
    ) -> Self {
        Self {
            workspace,
            locks: Some(locks),
            machine_dir: Some(PathBuf::from(machine_dir)),
            project_dir: Some(PathBuf::from(project_dir)),
            memory_cap: PROCESS_READ_CAP,
        }
    }

    /// The absolute path `raw` names: a relative path joins the workspace,
    /// an absolute path is used as given, and `~` is not expanded.
    fn absolute(&self, raw: &[u8]) -> PathBuf {
        // `join` replaces the workspace when `raw` is absolute.
        self.workspace.join(Path::new(OsStr::from_bytes(raw)))
    }

    pub(crate) fn read(&self, raw: &[u8]) -> Result<Vec<u8>, Failed> {
        let path = self.absolute(raw);
        let cap = u64::try_from(self.memory_cap).unwrap_or(u64::MAX);
        // Refuse past the cap before allocating: the bytes would land in
        // Lua past its memory limit.
        if fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) > cap {
            return Err(too_big("read", &path, self.memory_cap));
        }
        // Bound the read itself, so a file that grows between stat and
        // read is refused too.
        let mut data = Vec::new();
        fs::File::open(&path)
            .and_then(|file| file.take(cap.saturating_add(1)).read_to_end(&mut data))
            .map_err(|err| fail("read", &path, err))?;
        if u64::try_from(data.len()).unwrap_or(u64::MAX) > cap {
            return Err(too_big("read", &path, self.memory_cap));
        }
        Ok(data)
    }

    pub(crate) fn write(&self, raw: &[u8], data: &[u8], with_lock: bool) -> Result<(), Failed> {
        let path = self.absolute(raw);
        self.ensure_data_dir("write", &path)?;
        let mut op = || fs::write(&path, data).map_err(|err| fail("write", &path, err));
        self.locked(vec![path.clone()], with_lock, &mut op)
    }

    pub(crate) fn list(&self, raw: &[u8]) -> Result<Vec<Vec<u8>>, Failed> {
        let path = self.absolute(raw);
        let entries = fs::read_dir(&path).map_err(|err| fail("list", &path, err))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| fail("list", &path, err))?;
            names.push(entry.file_name().as_bytes().to_vec());
        }
        names.sort();
        Ok(names)
    }

    pub(crate) fn stat(&self, raw: &[u8]) -> Result<Option<Stat>, Failed> {
        let path = self.absolute(raw);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(fail("stat", &path, err)),
        };
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            "symlink"
        } else if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else {
            "other"
        };
        Ok(Some(Stat {
            kind,
            size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
            modified_ms: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .and_then(|ms| i64::try_from(ms).ok())
                .unwrap_or(0),
        }))
    }

    pub(crate) fn mkdir(&self, raw: &[u8], with_lock: bool) -> Result<(), Failed> {
        let path = self.absolute(raw);
        self.ensure_data_dir("mkdir", &path)?;
        let mut op = || fs::create_dir_all(&path).map_err(|err| fail("mkdir", &path, err));
        self.locked(vec![path.clone()], with_lock, &mut op)
    }

    pub(crate) fn remove(&self, raw: &[u8], with_lock: bool) -> Result<(), Failed> {
        let path = self.absolute(raw);
        let mut op = || {
            let is_dir = fs::symlink_metadata(&path)
                .map(|meta| meta.file_type().is_dir() && !meta.file_type().is_symlink())
                .map_err(|err| fail("remove", &path, err))?;
            if is_dir {
                fs::remove_dir(&path).map_err(|err| fail("remove", &path, err))
            } else {
                fs::remove_file(&path).map_err(|err| fail("remove", &path, err))
            }
        };
        self.locked(vec![path.clone()], with_lock, &mut op)
    }

    pub(crate) fn rename(
        &self,
        from_raw: &[u8],
        to_raw: &[u8],
        with_lock: bool,
    ) -> Result<(), Failed> {
        let from = self.absolute(from_raw);
        let to = self.absolute(to_raw);
        let mut op = || {
            fs::rename(&from, &to).map_err(|err| {
                (
                    code_of(&err),
                    format!(
                        "host.fs.rename: {} -> {}: {err}",
                        from.display(),
                        to.display()
                    ),
                )
            })
        };
        self.locked(vec![from.clone(), to.clone()], with_lock, &mut op)
    }

    fn data_dir(&self, scope: &[u8]) -> Result<PathBuf, String> {
        let dir = match scope {
            b"machine" => self.machine_dir.clone(),
            b"project" => self.project_dir.clone(),
            _ => {
                return Err("host.data_dir: scope must be \"machine\" or \"project\"".into());
            }
        };
        dir.ok_or_else(|| "host.data_dir: this extension has no session".into())
    }

    /// Creates this extension's own data directory, and the `data/` above
    /// it, when `target` lies inside it and it is missing. A path anywhere
    /// else gets no directory created.
    fn ensure_data_dir(&self, op: &str, target: &Path) -> Result<(), Failed> {
        let normal = lexical(target);
        for dir in [&self.machine_dir, &self.project_dir].into_iter().flatten() {
            if normal.starts_with(dir) && !dir.exists() {
                fs::create_dir_all(dir).map_err(|err| fail(op, target, err))?;
            }
        }
        Ok(())
    }

    /// Runs `op` while holding the lock on each of `paths` when `want`
    /// and the session offers a lock. The lock sorts the keys and drops
    /// duplicates, so a rename onto itself never waits on itself. Without
    /// either, no lock is taken; reads never ask.
    fn locked(
        &self,
        paths: Vec<PathBuf>,
        want: bool,
        op: &mut dyn FnMut() -> Result<(), Failed>,
    ) -> Result<(), Failed> {
        match (want, self.locks.clone()) {
            (true, Some(locks)) => {
                let mut out = None;
                {
                    let mut nested = || {
                        out = Some(op());
                    };
                    locks.hold_all(&paths, &mut nested);
                }
                out.unwrap_or(Ok(()))
            }
            _ => op(),
        }
    }
}

fn fail(op: &str, path: &Path, err: std::io::Error) -> Failed {
    let code = code_of(&err);
    (code, format!("host.fs.{op}: {}: {err}", path.display()))
}

/// The failure's code: a missing path is `not_found`, a directory, device
/// or other kind the call cannot handle `unsupported_file`, and any other
/// filesystem failure `io_failed`.
fn code_of(err: &std::io::Error) -> contract::ErrorCode {
    use contract::ErrorCode::{IoFailed, NotFound, UnsupportedFile};
    use std::io::ErrorKind::{IsADirectory, NotADirectory, NotFound as Missing};
    let kind = err.kind();
    if kind == Missing {
        NotFound
    } else if kind == IsADirectory || kind == NotADirectory {
        UnsupportedFile
    } else {
        IoFailed
    }
}

/// A file `read` refuses past the extension's memory cap: the call, the
/// path and the cap, without allocating the file's contents.
fn too_big(op: &str, path: &Path, cap: usize) -> Failed {
    (
        contract::ErrorCode::TooLarge,
        format!(
            "host.fs.{op}: {}: larger than the extension's memory cap of {cap} bytes",
            path.display()
        ),
    )
}

/// `path` with `.` dropped and `..` popping lexically, so the data-directory
/// test sees through a `..` without touching the filesystem.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            dir @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                out.push(dir.as_os_str());
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "fs_tests.rs"]
mod tests;
