//! Configuration on the hub (`docs/tui.md`, "Swapped views"). Loop-thread
//! calls hold a credit lend while waiting. Workers wait for a link without
//! lending credits. Login state is locked only across register-and-write,
//! never across an answer or link wait; its epoch binds result and cancel
//! commands to the connection that started the login.
//!
//! Lock order: login state, router state, then the link write lock. Router
//! state is released before writing; credits are never locked under login
//! state or router state. No lock is held while waiting for an answer.

mod decode;
mod login;
mod router;

use crate::ThemeSetting;
use crate::configure::{
    BrowserLogin, Configure, ConfigureError, KeyEdit, Layer, LoginShow, LoginTarget,
    ReviewerModels, Revoked, RulesScope, RulesSection, Saved, SettingRow, SkillsDisabled, Stored,
    SwitchScope, ToolGroup, ToolSwitches,
};
use contract::clock::Clock;
use contract::{ErrorCode, Secret};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) use login::RemoteLogin;
pub(crate) use router::{AskError, Caller, Cancel, Pending, Router};

/// The hub's configuration, with client-owned terminal settings kept local.
pub(crate) struct Remote {
    local: Arc<dyn Configure>,
    client_dir: PathBuf,
    router: Arc<Router>,
}
impl Remote {
    pub(crate) fn new(local: Arc<dyn Configure>, client_dir: PathBuf, router: Arc<Router>) -> Self {
        Self {
            local,
            client_dir,
            router,
        }
    }
    fn call<T>(
        &self,
        operation: &str,
        mut args: Value,
        caller: Caller<'_>,
        decode: impl FnOnce(&Value) -> Option<T>,
    ) -> Result<T, ConfigureError> {
        let Some(args) = args.as_object_mut() else {
            return Err(decode::io_error(operation));
        };
        drop(args.insert("operation".into(), json!(operation)));
        let line = self
            .router
            .ask("configure", Value::Object(std::mem::take(args)), caller)
            .map_err(ask_error)?;
        decode(&decode::result(&line, operation)?).ok_or_else(|| decode::io_error(operation))
    }
    fn workspace(&self, workspace: &Path) -> Result<PathBuf, ConfigureError> {
        if !workspace.as_os_str().is_empty() {
            return Ok(workspace.into());
        }
        let file = self.hub_global()?;
        file.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| decode::io_error("global_file"))
    }
    /// Searches on the hub, delivering only ranked rows to the loop.
    pub(crate) fn search(
        &self,
        workspace: &Path,
        text: &str,
        fresh: bool,
        generation: u64,
    ) -> Result<(), AskError> {
        let mut args = Map::from_iter([
            ("workspace".into(), json!(workspace)),
            ("text".into(), json!(text)),
        ]);
        if fresh {
            drop(args.insert("fresh".into(), json!(true)));
        }
        self.router.ask_into(
            "search_files",
            Value::Object(args),
            Box::new(move |line| {
                let result = match decode::result(line, "search_files") {
                    Err(error) if error.code == ErrorCode::StaleRequest => return None,
                    Err(error) => Err(error.message),
                    Ok(value) => {
                        if let Some(error) = value.get("error") {
                            Err(error
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| decode::io_error("search_files").message))
                        } else {
                            value
                                .get("rows")
                                .cloned()
                                .and_then(|rows| serde_json::from_value(rows).ok())
                                .ok_or_else(|| decode::io_error("search_files").message)
                        }
                    }
                };
                Some(crate::Input::Files { generation, result })
            }),
        )
    }

    /// Fetches an editable hub file and its optimistic-write version.
    pub(crate) fn fetch_file(
        &self,
        workspace: &Path,
        file: &Path,
    ) -> Result<(String, String), ConfigureError> {
        let line = self
            .router
            .ask(
                "read_config",
                json!({"workspace":self.workspace(workspace)?,"path":file}),
                Caller::Loop,
            )
            .map_err(ask_error)?;
        decode::read_config(&decode::result(&line, "read_config")?)
            .ok_or_else(|| decode::io_error("read_config"))
    }
    /// Writes only the hub version that the editor originally fetched.
    pub(crate) fn store_file(
        &self,
        workspace: &Path,
        file: &Path,
        text: &str,
        version: &str,
    ) -> Result<(), ConfigureError> {
        let line=self.router.ask("write_config",json!({"workspace":self.workspace(workspace)?,"path":file,"text":text,"version":version}),Caller::Loop).map_err(ask_error)?;
        decode::result(&line, "write_config").map(|_| ())
    }
    /// The client's global file, for client-owned default rows.
    pub(crate) fn local_global_file(&self) -> PathBuf {
        self.local.global_file()
    }

    fn hub_global(&self) -> Result<PathBuf, ConfigureError> {
        if let Some(file) = self.router.cached_global() {
            return Ok(file);
        }
        let _lend = self.router.lend();
        let pending: Pending = self
            .router
            .send("configure", json!({"operation":"global_file"}))
            .map_err(ask_error)?;
        let epoch = pending.epoch();
        let line = pending.wait().map_err(ask_error)?;
        let file = decode::file(&decode::result(&line, "global_file")?)
            .ok_or_else(|| decode::io_error("global_file"))?;
        self.router.cache_global(epoch, file.clone());
        Ok(file)
    }
}
fn ask_error(error: AskError) -> ConfigureError {
    ConfigureError {
        code: if error == AskError::Cancelled {
            ErrorCode::Cancelled
        } else {
            ErrorCode::IoFailed
        },
        message: error.to_string(),
    }
}
fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Global => "global",
        Layer::Project => "project",
        Layer::Repository => "repository",
    }
}
fn scope_name(scope: SwitchScope) -> &'static str {
    match scope {
        SwitchScope::Project => "project",
        SwitchScope::Everywhere => "everywhere",
    }
}
fn unit(_: &Value) -> Option<()> {
    Some(())
}

/// Ownership follows the first key segment, never the file path.
pub(crate) fn client_owned(key: &str) -> bool {
    let first = if let Some(quoted) = key.strip_prefix('"') {
        let Some((first, tail)) = quoted.split_once('"') else {
            return false;
        };
        if !tail.is_empty() && !tail.starts_with('.') {
            return false;
        }
        first
    } else {
        key.split('.').next().unwrap_or_default()
    };
    matches!(first, "keys" | "tui")
}
impl Configure for Remote {
    fn settings(&self, workspace: &Path) -> Result<Vec<SettingRow>, ConfigureError> {
        let workspace = self.workspace(workspace)?;
        let mut hub = self.call(
            "settings",
            json!({"workspace":workspace}),
            Caller::Loop,
            decode::settings,
        )?;
        hub.retain(|row| !client_owned(&row.key));
        let mut rows = Vec::new();
        for row in self.local.settings(&self.client_dir)? {
            if client_owned(&row.key) {
                rows.push(row);
            } else if let Some(index) = hub.iter().position(|h| h.key == row.key) {
                rows.push(hub.remove(index));
            }
        }
        rows.extend(hub);
        Ok(rows)
    }
    fn set(
        &self,
        workspace: &Path,
        layer: Layer,
        key: &str,
        text: &str,
    ) -> Result<Saved, ConfigureError> {
        if client_owned(key) {
            return self.local.set(&self.client_dir, layer, key, text);
        }
        self.call("set",json!({"workspace":self.workspace(workspace)?,"layer":layer_name(layer),"key":key,"text":text}),Caller::Loop,decode::saved)
    }
    fn global_file(&self) -> PathBuf {
        self.hub_global().unwrap_or_default()
    }
    fn rules(&self, workspace: &Path) -> Result<(RulesSection, RulesSection), ConfigureError> {
        self.call(
            "rules",
            json!({"workspace":self.workspace(workspace)?}),
            Caller::Loop,
            decode::rules,
        )
    }
    fn revoke(
        &self,
        workspace: &Path,
        scope: RulesScope,
        line: usize,
        text: &str,
    ) -> Result<Revoked, ConfigureError> {
        let scope = match scope {
            RulesScope::Global => "global",
            RulesScope::Project => "project",
        };
        self.call(
            "revoke",
            json!({"workspace":self.workspace(workspace)?,"scope":scope,"line":line,"text":text}),
            Caller::Loop,
            decode::revoked,
        )
    }
    fn login_targets(&self) -> Result<Vec<LoginTarget>, ConfigureError> {
        self.call("login_targets", json!({}), Caller::Loop, decode::targets)
    }
    fn store_key(
        &self,
        name: &str,
        label: Option<&str>,
        key: Secret,
    ) -> Result<Stored, ConfigureError> {
        let mut args = Map::from_iter([
            ("name".into(), json!(name)),
            ("key".into(), json!(key.expose())),
        ]);
        if let Some(label) = label {
            drop(args.insert("label".into(), json!(label)));
        }
        self.call(
            "store_key",
            Value::Object(args),
            Caller::Loop,
            decode::stored,
        )
    }
    fn browser_login(
        &self,
        name: &str,
        shown: Arc<dyn LoginShow>,
        _clock: Arc<dyn Clock>,
    ) -> Arc<dyn BrowserLogin> {
        Arc::new(RemoteLogin::new(
            Arc::clone(&self.router),
            name.into(),
            shown,
        ))
    }
    fn reviewer_models(
        &self,
        name: &str,
        _clock: Arc<dyn Clock>,
    ) -> Result<ReviewerModels, ConfigureError> {
        self.call(
            "reviewer_models",
            json!({"name":name}),
            Caller::Worker(&Cancel::default()),
            decode::reviewer,
        )
    }
    fn set_reviewer(&self, name: &str, id: &str) -> Result<(), ConfigureError> {
        self.call(
            "set_reviewer",
            json!({"name":name,"id":id}),
            Caller::Loop,
            unit,
        )
    }
    fn themes(&self, _workspace: &Path) -> Vec<String> {
        self.local.themes(&self.client_dir)
    }
    fn theme(&self, _workspace: &Path, name: &str) -> ThemeSetting {
        self.local.theme(&self.client_dir, name)
    }
    fn tool_switches(&self, workspace: &Path) -> Result<Vec<ToolSwitches>, ConfigureError> {
        self.call(
            "tool_switches",
            json!({"workspace":self.workspace(workspace)?}),
            Caller::Loop,
            decode::tools,
        )
    }
    fn switch_tool(
        &self,
        workspace: &Path,
        group: &ToolGroup,
        tool: &str,
        scope: SwitchScope,
        on: bool,
    ) -> Result<(), ConfigureError> {
        let group = match group {
            ToolGroup::Mcp(name) => json!({"mcp":name}),
            ToolGroup::Extension(name) => json!({"extension":name}),
        };
        self.call("switch_tool",json!({"workspace":self.workspace(workspace)?,"group":group,"tool":tool,"scope":scope_name(scope),"on":on}),Caller::Loop,unit)
    }
    fn skills_disabled(&self, workspace: &Path) -> Result<SkillsDisabled, ConfigureError> {
        self.call(
            "skills_disabled",
            json!({"workspace":self.workspace(workspace)?}),
            Caller::Loop,
            decode::disabled,
        )
    }
    fn switch_skill(
        &self,
        workspace: &Path,
        name: &str,
        scope: SwitchScope,
        on: bool,
    ) -> Result<(), ConfigureError> {
        self.call("switch_skill",json!({"workspace":self.workspace(workspace)?,"name":name,"scope":scope_name(scope),"on":on}),Caller::Loop,unit)
    }
    fn skill_text(&self, path: &Path) -> Result<String, ConfigureError> {
        self.call(
            "skill_text",
            json!({"path":path}),
            Caller::Loop,
            decode::text,
        )
    }
    fn save_keys(&self, edits: &[KeyEdit]) -> Result<(), ConfigureError> {
        self.local.save_keys(edits)
    }
}

/// The owned editor copy; removal on drop is best-effort, including shutdown.
pub(crate) struct RemoteCopy {
    pub(crate) dir: PathBuf,
    pub(crate) temp: PathBuf,
    pub(crate) hub: PathBuf,
    pub(crate) version: String,
}
impl RemoteCopy {
    /// Creates a private directory containing the hub text byte for byte.
    pub(crate) fn create(
        dir_parent: &Path,
        hub: &Path,
        text: &str,
        version: String,
    ) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let dir = dir_parent.join(format!("fiber-{}", crate::app::mint()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let temp = dir.join(
            hub.file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("config.json")),
        );
        let copy = Self {
            dir,
            temp,
            hub: hub.into(),
            version,
        };
        std::fs::write(&copy.temp, text)?;
        Ok(copy)
    }
}
impl Drop for RemoteCopy {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.dir));
    }
}
/// The seam installed on the app, and the remote-only operations if applicable.
pub(crate) struct Seams {
    pub(crate) seam: Option<Arc<dyn Configure>>,
    pub(crate) remote: Option<Arc<Remote>>,
}
/// Wraps only a remote terminal's seam; local launches retain the original.
pub(crate) fn seams(
    location: &crate::LaunchLocation,
    seam: Option<Arc<dyn Configure>>,
    router: &Arc<Router>,
) -> Seams {
    if matches!(location, crate::LaunchLocation::Remote { .. }) {
        if let Some(local) = seam {
            let client_dir = std::env::current_dir().unwrap_or_else(|_| {
                local
                    .global_file()
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default()
            });
            let remote = Arc::new(Remote::new(local, client_dir, Arc::clone(router)));
            return Seams {
                seam: Some(Arc::clone(&remote) as Arc<dyn Configure>),
                remote: Some(remote),
            };
        }
        return Seams {
            seam: None,
            remote: None,
        };
    }
    Seams { seam, remote: None }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
