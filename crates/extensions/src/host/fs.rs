//! `host.fs` and `host.data_dir` (`docs/extensions.md`, "Host calls").

use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
use std::collections::BTreeSet;
#[cfg(test)]
use std::sync::{Condvar, Mutex};

use contract::files::PathLock;
use mlua::{Lua, LuaString, Table, Value as LuaValue};

use super::Session;

/// Installs `host.fs` and `host.data_dir` on `host`. `session` is the
/// extension's session: its project names `projects/<key>/` in Fiber home
/// and its lock is the session's per-path lock. Without one
/// (`LuaExtension::with_session`), relative paths resolve against the
/// process's current directory, `lock = true` takes no lock, and
/// `host.data_dir` raises. `memory_cap` bounds `read`: a file larger
/// than the cap is refused, never read into Lua past its limit.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    workspace: PathBuf,
    home: PathBuf,
    extension: &str,
    memory_cap: usize,
    session: Option<&Session>,
) -> mlua::Result<()> {
    let slug = extension.replace('/', "-");
    let project = session.map(|session| session.config.project().as_str().to_owned());
    let (machine_dir, project_dir) = match project.as_deref() {
        Some(key) => (
            Some(home.join("data").join(&slug)),
            Some(home.join("projects").join(key).join("data").join(&slug)),
        ),
        None => (None, None),
    };
    let fs = Fs {
        workspace,
        locks: session.map(|session| Arc::clone(&session.locks)),
        machine_dir,
        project_dir,
        memory_cap,
    };
    let table = lua.create_table()?;
    {
        let fs = fs.clone();
        table.set(
            "read",
            lua.create_function(move |lua, path: LuaString| {
                let data = fs.read(&path.as_bytes()).map_err(runtime)?;
                lua.create_string(data)
            })?,
        )?;
    }
    {
        let fs = fs.clone();
        table.set(
            "write",
            lua.create_function(
                move |_, (path, data, opts): (LuaString, LuaString, Option<Table>)| {
                    fs.write(&path.as_bytes(), &data.as_bytes(), lock(opts)?)
                        .map_err(runtime)
                },
            )?,
        )?;
    }
    {
        let fs = fs.clone();
        table.set(
            "list",
            lua.create_function(move |lua, path: LuaString| {
                let names = fs.list(&path.as_bytes()).map_err(runtime)?;
                let out = lua.create_table()?;
                for (i, name) in names.iter().enumerate() {
                    out.raw_set(i + 1, lua.create_string(name)?)?;
                }
                Ok(out)
            })?,
        )?;
    }
    {
        let fs = fs.clone();
        table.set(
            "stat",
            lua.create_function(move |lua, path: LuaString| {
                let found = fs.stat(&path.as_bytes()).map_err(runtime)?;
                let Some(found) = found else {
                    return Ok(LuaValue::Nil);
                };
                let out = lua.create_table()?;
                out.raw_set("kind", found.kind)?;
                out.raw_set("size", found.size)?;
                out.raw_set("modified_ms", found.modified_ms)?;
                Ok(LuaValue::Table(out))
            })?,
        )?;
    }
    {
        let fs = fs.clone();
        table.set(
            "mkdir",
            lua.create_function(move |_, (path, opts): (LuaString, Option<Table>)| {
                fs.mkdir(&path.as_bytes(), lock(opts)?).map_err(runtime)
            })?,
        )?;
    }
    {
        let fs = fs.clone();
        table.set(
            "remove",
            lua.create_function(move |_, (path, opts): (LuaString, Option<Table>)| {
                fs.remove(&path.as_bytes(), lock(opts)?).map_err(runtime)
            })?,
        )?;
    }
    {
        let fs = fs.clone();
        table.set(
            "rename",
            lua.create_function(
                move |_, (from, to, opts): (LuaString, LuaString, Option<Table>)| {
                    fs.rename(&from.as_bytes(), &to.as_bytes(), lock(opts)?)
                        .map_err(runtime)
                },
            )?,
        )?;
    }
    host.set("fs", table)?;
    {
        let fs = fs.clone();
        host.set(
            "data_dir",
            lua.create_function(move |lua, scope: LuaValue| {
                let scope = if let LuaValue::String(s) = &scope {
                    s.as_bytes()
                } else {
                    return Err(runtime(
                        "host.data_dir: scope must be \"machine\" or \"project\"".into(),
                    ));
                };
                let dir = fs.data_dir(&scope).map_err(runtime)?;
                lua.create_string(dir.as_os_str().as_bytes())
            })?,
        )?;
    }
    Ok(())
}

fn runtime(message: String) -> mlua::Error {
    mlua::Error::RuntimeError(message)
}

/// The `lock` option of a mutating call: absent or missing is no lock.
fn lock(opts: Option<Table>) -> mlua::Result<bool> {
    match opts {
        None => Ok(false),
        Some(opts) => Ok(opts.get::<Option<bool>>("lock")?.unwrap_or(false)),
    }
}

/// What `stat` reports for a path that exists.
struct Stat {
    kind: &'static str,
    size: i64,
    modified_ms: i64,
}

/// The files one extension reaches through `host.fs`: its workspace for
/// relative paths, the session's lock when one is offered, its two data
/// directories, created on first write, and the bound `read` refuses past.
#[derive(Clone)]
struct Fs {
    workspace: PathBuf,
    locks: Option<Arc<dyn PathLock>>,
    machine_dir: Option<PathBuf>,
    project_dir: Option<PathBuf>,
    memory_cap: usize,
}

impl Fs {
    /// The absolute path `raw` names: a relative path joins the workspace,
    /// an absolute path is used as given, and `~` is not expanded.
    fn absolute(&self, raw: &[u8]) -> PathBuf {
        let path = Path::new(OsStr::from_bytes(raw));
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace.join(path)
        }
    }

    fn read(&self, raw: &[u8]) -> Result<Vec<u8>, String> {
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

    fn write(&self, raw: &[u8], data: &[u8], with_lock: bool) -> Result<(), String> {
        let path = self.absolute(raw);
        self.ensure_data_dir("write", &path)?;
        let mut op = || fs::write(&path, data).map_err(|err| fail("write", &path, err));
        self.locked(vec![path.clone()], with_lock, &mut op)
    }

    fn list(&self, raw: &[u8]) -> Result<Vec<Vec<u8>>, String> {
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

    fn stat(&self, raw: &[u8]) -> Result<Option<Stat>, String> {
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

    fn mkdir(&self, raw: &[u8], with_lock: bool) -> Result<(), String> {
        let path = self.absolute(raw);
        self.ensure_data_dir("mkdir", &path)?;
        let mut op = || fs::create_dir_all(&path).map_err(|err| fail("mkdir", &path, err));
        self.locked(vec![path.clone()], with_lock, &mut op)
    }

    fn remove(&self, raw: &[u8], with_lock: bool) -> Result<(), String> {
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

    fn rename(&self, from_raw: &[u8], to_raw: &[u8], with_lock: bool) -> Result<(), String> {
        let from = self.absolute(from_raw);
        let to = self.absolute(to_raw);
        let mut op = || {
            fs::rename(&from, &to).map_err(|err| {
                format!(
                    "host.fs.rename: {} -> {}: {err}",
                    from.display(),
                    to.display()
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
    fn ensure_data_dir(&self, op: &str, target: &Path) -> Result<(), String> {
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
        op: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
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

fn fail(op: &str, path: &Path, err: impl std::fmt::Display) -> String {
    format!("host.fs.{op}: {}: {err}", path.display())
}

/// A file `read` refuses past the extension's memory cap: the call, the
/// path and the cap, without allocating the file's contents.
fn too_big(op: &str, path: &Path, cap: usize) -> String {
    format!(
        "host.fs.{op}: {}: larger than the extension's memory cap of {cap} bytes",
        path.display()
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

/// A fake per-path lock for tests: real exclusion, so a test observes a
/// locked call wait, and the keys it was asked for, in order.
#[cfg(test)]
pub(crate) struct FakeLock {
    state: Mutex<FakeState>,
    changed: Condvar,
    calls: Mutex<Vec<PathBuf>>,
}

#[cfg(test)]
struct FakeState {
    held: BTreeSet<PathBuf>,
    /// Callers blocked in [`FakeLock::hold`], raised before they wait.
    waiting: usize,
}

#[cfg(test)]
impl FakeLock {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(FakeState {
                held: BTreeSet::new(),
                waiting: 0,
            }),
            changed: Condvar::new(),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// The keys `hold` was asked for, in order.
    pub(crate) fn calls(&self) -> Vec<PathBuf> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How many callers are blocked in [`FakeLock::hold`].
    pub(crate) fn waiting(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .waiting
    }
}

#[cfg(test)]
impl PathLock for FakeLock {
    fn hold(&self, path: &Path, run: &mut dyn FnMut()) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(path.to_path_buf());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.held.contains(path) {
            state.waiting += 1;
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.waiting = state.waiting.saturating_sub(1);
        }
        state.held.insert(path.to_path_buf());
        drop(state);
        run();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.held.remove(path);
        self.changed.notify_all();
    }
}

#[cfg(test)]
#[path = "fs_tests.rs"]
mod tests;
