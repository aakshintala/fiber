//! Configuration on the hub (`docs/tui.md`, "Swapped views"). Loop-thread
//! calls hold a credit lend while waiting. Workers wait for a link without
//! lending credits. Login state is locked only across register-and-write,
//! never across an answer or link wait; its epoch binds result and cancel
//! commands to the connection that started the login.
//!
//! Lock order: login state, router state, then the link write lock. Router
//! state is released before writing; credits are never locked under login
//! state or router state. No lock is held while waiting for an answer.
//! The catalogue workspace (`Remote.reading`) is its own leaf lock (L5):
//! taken only to copy the path, never across a router call, a wait or a send.

mod decode;
mod login;
mod router;

use crate::ThemeSetting;
use crate::catalogue::{
    Catalogue, LeftOutGroup, LeftOutModel, ListAge, LoggedOutProvider, ModelEntry, Price,
    PriceTier, Refresh,
};
use crate::configure::{
    BrowserLogin, Configure, ConfigureError, HostGap, HostSaved, KeyEdit, Layer, LoginShow,
    LoginTarget, LogoutRow, NotesSaved, NotesWrite, ReviewerModels, ReviewerNotes, Revoked,
    RulesScope, RulesSection, Saved, SettingRow, Shown, SkillsDisabled, Stored, SwitchScope,
    ToolGroup, ToolSwitches,
};
use contract::clock::Clock;
use contract::{ErrorCode, Secret};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

pub(crate) use login::RemoteLogin;
pub(crate) use router::{AskError, Caller, Cancel, Pending, Router};

/// The home chips' configuration: the hub model and thinking level on
/// screen, and the scoped models the checklist marks. Each is absent
/// when the hub leaves it unset, redacts it, or writes what is not
/// JSON: the fold keeps the chip's text for those.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Chip {
    pub(crate) model: Option<String>,
    pub(crate) thinking: Option<String>,
    pub(crate) scoped: Vec<String>,
}

/// The hub's configuration, with client-owned terminal settings kept local.
pub(crate) struct Remote {
    local: Arc<dyn Configure>,
    client_dir: PathBuf,
    router: Arc<Router>,
    /// The workspace the catalogue reads use: `take_reads` stores the
    /// workspace in use just before it returns a read, so every read
    /// after the first goes there. None until the first read, and on a
    /// remote-home launch with no local workspace.
    reading: Mutex<Option<PathBuf>>,
}
impl Remote {
    pub(crate) fn new(local: Arc<dyn Configure>, client_dir: PathBuf, router: Arc<Router>) -> Self {
        Self {
            local,
            client_dir,
            router,
            reading: Mutex::new(None),
        }
    }
    /// Stores the workspace the catalogue reads use, taking no other lock.
    /// None means the next `models()` reads at the hub's global file,
    /// as a cold read does.
    pub(crate) fn set_workspace(&self, workspace: Option<PathBuf>) {
        *support::lock(&self.reading) = workspace;
    }
    /// The workspace the catalogue reads use, for tests.
    #[cfg(test)]
    pub(crate) fn reading(&self) -> Option<PathBuf> {
        support::lock(&self.reading).clone()
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
        self.workspace_as(workspace)
    }
    /// Resolves the workspace a `configure` operation sends: a non-empty
    /// path passes as given when absolute (`docs/invocation.md`,
    /// "What the hub speaks": every workspace is absolute); an empty one
    /// is an error, as a relative one already is. No-workspace callers
    /// use `global_parent`, never an empty path here.
    fn workspace_as(&self, workspace: &Path) -> Result<PathBuf, ConfigureError> {
        if workspace.as_os_str().is_empty() {
            return Err(decode::io_error("workspace"));
        }
        if workspace.is_absolute() {
            return Ok(workspace.into());
        }
        Err(decode::io_error("workspace"))
    }
    /// The hub-global parent a no-workspace call sends: the parent of the
    /// hub's global file, read through the `Result` hub-global read, never
    /// the `global_file` fallback. The loop path asks, so the read rides
    /// the connection the loop holds.
    pub(crate) fn global_parent(&self) -> Result<PathBuf, ConfigureError> {
        parent_of(self.hub_global()?)
    }
    /// The hub's global file for a worker read: the cached copy when the
    /// loop path has set it, else one `global_file` ask, without writing
    /// the cache.
    fn hub_global_worker(&self, cancel: &Cancel) -> Result<PathBuf, ConfigureError> {
        if let Some(file) = self.router.cached_global() {
            return Ok(file);
        }
        let line = self
            .router
            .ask(
                "configure",
                json!({"operation":"global_file"}),
                Caller::Worker(cancel),
            )
            .map_err(ask_error)?;
        decode::file(&decode::result(&line, "global_file")?)
            .ok_or_else(|| decode::io_error("global_file"))
    }
    /// Reads the home chips' configuration at the workspace in use: the
    /// model and thinking rows, and the scoped models list. The loop
    /// path asks, so the read rides the connection the loop holds.
    pub(crate) fn chip(&self, workspace: &Path) -> Result<Chip, ConfigureError> {
        let workspace = self.workspace_as(workspace)?;
        self.chip_at(workspace)
    }
    /// Reads the home chips with no local workspace: the hub-global parent
    /// stands in for the workspace, so the hub model and thinking values
    /// survive a remote-home launch.
    pub(crate) fn chip_global(&self) -> Result<Chip, ConfigureError> {
        let workspace = self.global_parent()?;
        self.chip_at(workspace)
    }
    /// Folds a `settings` answer into the home chips: the hub model and
    /// thinking on screen, and the scoped list the checklist marks.
    fn chip_at(&self, workspace: PathBuf) -> Result<Chip, ConfigureError> {
        let rows = self.call(
            "settings",
            json!({"workspace":workspace}),
            Caller::Loop,
            decode::settings,
        )?;
        let text = |key: &str| {
            rows.iter()
                .find(|row| row.key == key)
                .and_then(|row| match &row.value {
                    Shown::Value(text) => Some(text.clone()),
                    Shown::Unset | Shown::Union { .. } | Shown::Redacted(_) => None,
                })
        };
        let model: Option<String> =
            text("model").and_then(|text| serde_json::from_str::<String>(&text).ok());
        let thinking: Option<String> = model.as_deref().and_then(|model| {
            text(&crate::app::model_picker::canonical_thinking_key(model))
                .and_then(|text| serde_json::from_str::<String>(&text).ok())
        });
        let scoped: Vec<String> = text("scoped_models")
            .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
            .unwrap_or_default();
        Ok(Chip {
            model,
            thinking,
            scoped,
        })
    }
    /// Reads the hub's catalogue at `refresh` for the workspace in use,
    /// copied out before anything is sent: the mutex is never held across
    /// a wait. The picker's refresh rides as a word, one per level.
    fn models(&self, refresh: Refresh) -> Result<Catalogue, ConfigureError> {
        let reading = support::lock(&self.reading).clone();
        // An empty stored workspace reads at the hub's global file, as a
        // cold read does: no empty path crosses into the hub.
        let workspace = match reading.as_deref() {
            Some(reading) if !reading.as_os_str().is_empty() => self.workspace_as(reading)?,
            _ => parent_of(self.hub_global_worker(&Cancel::default())?)?,
        };
        let word = match refresh {
            Refresh::Cached => "cached",
            Refresh::Stale => "stale",
            Refresh::Every => "every",
        };
        let line = self
            .router
            .ask(
                "configure",
                json!({"operation":"models","workspace":workspace,"refresh":word}),
                Caller::Worker(&Cancel::default()),
            )
            .map_err(ask_error)?;
        decode_models(&decode::result(&line, "models")?).ok_or_else(|| decode::io_error("models"))
    }
    /// Reads the hub's catalogue as the picker's `ReadModels`: a hub
    /// rejection keeps its message, so the picker's notice shows it.
    pub(crate) fn catalogue_reader(self: &Arc<Self>) -> crate::ReadModels {
        let remote = Arc::clone(self);
        Arc::new(move |refresh| remote.models(refresh).map_err(|error| error.message))
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
        let workspace = self.workspace(workspace)?;
        self.fetch_file_at(workspace, file)
    }
    /// Fetches an editable hub file with no local workspace: only a
    /// hub-global file has an address. A project-scoped file
    /// (`projects/<key>/...` under Fiber home) is an error on the
    /// caller's notice path, and no line is sent.
    pub(crate) fn fetch_file_global(
        &self,
        file: &Path,
    ) -> Result<(String, String), ConfigureError> {
        let workspace = self.global_parent()?;
        if file.starts_with(workspace.join("projects")) {
            return Err(decode::io_error("read_config"));
        }
        self.fetch_file_at(workspace, file)
    }
    /// Reads `file` at `workspace`: the call both variants share.
    fn fetch_file_at(
        &self,
        workspace: PathBuf,
        file: &Path,
    ) -> Result<(String, String), ConfigureError> {
        let line = self
            .router
            .ask(
                "read_config",
                json!({"workspace":workspace,"path":file}),
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
        let workspace = self.workspace(workspace)?;
        self.store_file_at(workspace, file, text, version)
    }
    /// Writes a hub file with no local workspace: only a hub-global file
    /// has an address. A project-scoped file (`projects/<key>/...` under
    /// Fiber home) is an error on the caller's notice path, and no line
    /// is sent.
    pub(crate) fn store_file_global(
        &self,
        file: &Path,
        text: &str,
        version: &str,
    ) -> Result<(), ConfigureError> {
        let workspace = self.global_parent()?;
        if file.starts_with(workspace.join("projects")) {
            return Err(decode::io_error("write_config"));
        }
        self.store_file_at(workspace, file, text, version)
    }
    /// Writes `file` at `workspace`: the call both variants share.
    fn store_file_at(
        &self,
        workspace: PathBuf,
        file: &Path,
        text: &str,
        version: &str,
    ) -> Result<(), ConfigureError> {
        let line = self
            .router
            .ask(
                "write_config",
                json!({"workspace":workspace,"path":file,"text":text,"version":version}),
                Caller::Loop,
            )
            .map_err(ask_error)?;
        decode::result(&line, "write_config").map(|_| ())
    }
    /// The client's global file, for client-owned default rows.
    pub(crate) fn local_global_file(&self) -> PathBuf {
        self.local.global_file()
    }
    /// Reads the first-run offer's pending state from the hub
    /// machine's home: the receipts live where the hub's `import`
    /// writes them, never under the terminal's own home.
    pub(crate) fn import_pending_hub(&self) -> Result<bool, ConfigureError> {
        self.call("import_pending", json!({}), Caller::Loop, |value| {
            value.get("pending")?.as_bool()
        })
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
        // Every workspace is absolute (`docs/invocation.md`, "What the hub
        // speaks"), so a relative answer is an error, never the cache's
        // content: the cache holds only a path workspaces derive from.
        if !file.is_absolute() {
            return Err(decode::io_error("global_file"));
        }
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
// debt: three write-side enum matches and decode::layer's reverse match;
// consolidate into shared tables when a second enum needs wire decoding.
/// The parent a hub-global file resolves to: an absolute file's
/// directory, never a workspace. A relative answer is an error naming
/// `global_file`, so no relative path is ever sent as a workspace.
fn parent_of(file: PathBuf) -> Result<PathBuf, ConfigureError> {
    if !file.is_absolute() {
        return Err(decode::io_error("global_file"));
    }
    file.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| decode::io_error("global_file"))
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

/// A required field: absent or of the wrong type fails the whole answer.
macro_rules! read {
    ($value:expr, $key:expr) => {
        serde_json::from_value($value.get($key)?.clone()).ok()
    };
}
/// An optional field: absent reads as none, but a present field of the
/// wrong type, null included, fails the whole answer.
macro_rules! optional {
    ($value:expr, $key:expr) => {
        $value
            .get($key)
            .map(|present| serde_json::from_value(present.clone()))
            .transpose()
            .ok()
    };
}
/// One size tier's prices, field for field.
fn decode_tier(value: &Value) -> Option<PriceTier> {
    Some(PriceTier {
        input_tokens_above: read!(value, "input_tokens_above")?,
        micros_per_mtok: read!(value, "micros_per_mtok")?,
        input: read!(value, "input")?,
        output: read!(value, "output")?,
    })
}
/// One model's prices, tiers in the hub's order.
fn decode_price(value: &Value) -> Option<Price> {
    Some(Price {
        micros_per_mtok: read!(value, "micros_per_mtok")?,
        input: read!(value, "input")?,
        output: read!(value, "output")?,
        tiers: value
            .get("tiers")?
            .as_array()?
            .iter()
            .map(decode_tier)
            .collect::<Option<Vec<_>>>()?,
    })
}
/// One model as the picker lists it.
fn decode_entry(value: &Value) -> Option<ModelEntry> {
    Some(ModelEntry {
        reference: read!(value, "reference")?,
        provider: read!(value, "provider")?,
        id: read!(value, "id")?,
        name: optional!(value, "name")?,
        levels: read!(value, "levels")?,
        default_level: optional!(value, "default_level")?,
        configured: optional!(value, "configured")?,
        roles: read!(value, "roles")?,
        price: match optional!(value, "price")? {
            None => None,
            Some(price) => Some(decode_price(&price)?),
        },
    })
}
/// One provider's list time.
fn decode_age(value: &Value) -> Option<ListAge> {
    Some(ListAge {
        provider: read!(value, "provider")?,
        updated_ms: optional!(value, "updated_ms")?,
        stale: read!(value, "stale")?,
    })
}
/// The hub's catalogue, whole or not at all: a missing or mistyped field
/// anywhere fails the answer, so no partial catalogue is ever stored.
/// One left-out model: its id, and its name only when the provider
/// data names one.
fn decode_left_out_model(value: &Value) -> Option<LeftOutModel> {
    Some(LeftOutModel {
        id: read!(value, "id")?,
        name: optional!(value, "name")?,
    })
}

/// One provider-and-setting group with no value. `file` is required
/// on the wire: a missing or mistyped one fails the element, as
/// `read!` does elsewhere in this decoder.
fn decode_left_out(value: &Value) -> Option<LeftOutGroup> {
    Some(LeftOutGroup {
        provider: read!(value, "provider")?,
        extension: read!(value, "extension")?,
        file: read!(value, "file")?,
        setting: read!(value, "setting")?,
        login: read!(value, "login")?,
        models: value
            .get("models")?
            .as_array()?
            .iter()
            .map(decode_left_out_model)
            .collect::<Option<Vec<_>>>()?,
    })
}

/// One provider whose data is present but whose credential is absent.
fn decode_logged_out(value: &Value) -> Option<LoggedOutProvider> {
    Some(LoggedOutProvider {
        provider: read!(value, "provider")?,
        login: read!(value, "login")?,
        hint: read!(value, "hint")?,
    })
}

fn decode_models(value: &Value) -> Option<Catalogue> {
    let logged_out = match value.get("logged_out") {
        None => Vec::new(),
        Some(rows) => rows
            .as_array()?
            .iter()
            .map(decode_logged_out)
            .collect::<Option<Vec<_>>>()?,
    };
    let mut catalogue = Catalogue {
        installable: match value.get("installable") {
            None => Vec::new(),
            Some(names) => serde_json::from_value(names.clone()).ok()?,
        },
        left_out: match value.get("left_out") {
            None => Vec::new(),
            Some(groups) => groups
                .as_array()?
                .iter()
                .map(decode_left_out)
                .collect::<Option<Vec<_>>>()?,
        },
        models: value
            .get("models")?
            .as_array()?
            .iter()
            .map(decode_entry)
            .collect::<Option<Vec<_>>>()?,
        notices: read!(value, "notices")?,
        lists: value
            .get("lists")?
            .as_array()?
            .iter()
            .map(decode_age)
            .collect::<Option<Vec<_>>>()?,
        ..Default::default()
    };
    catalogue.logged_out = logged_out;
    Some(catalogue)
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
    fn blocking_hooks(
        &self,
        workspace: &Path,
        overrides: &[String],
    ) -> Result<Vec<String>, ConfigureError> {
        let cancel = Cancel::default();
        self.call(
            "blocking_hooks",
            json!({"workspace":self.workspace_as(workspace)?,"overrides":overrides}),
            Caller::Worker(&cancel),
            decode::blocking_hooks,
        )
    }

    fn install_hooks(&self, _clock: Arc<dyn Clock>) -> Result<(), ConfigureError> {
        let cancel = Cancel::default();
        self.call(
            "install_hooks",
            json!({}),
            Caller::Worker(&cancel),
            decode::install_hooks,
        )
    }

    fn install_provider(
        &self,
        workspace: &Path,
        provider: &str,
        _clock: Arc<dyn Clock>,
    ) -> Result<crate::ProviderInstall, ConfigureError> {
        let cancel = Cancel::default();
        let workspace = self.workspace_as(workspace)?;
        self.call(
            "install_provider",
            json!({"workspace":workspace,"provider":provider}),
            Caller::Worker(&cancel),
            decode::install,
        )
    }

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
    fn logout_rows(&self) -> Result<Vec<LogoutRow>, ConfigureError> {
        self.call("logout_rows", json!({}), Caller::Loop, decode::logout_rows)
    }
    fn delete_stored(&self, name: &str, label: Option<String>) -> Result<String, ConfigureError> {
        let mut args = Map::from_iter([("name".into(), json!(name))]);
        if let Some(label) = label {
            drop(args.insert("label".into(), json!(label)));
        }
        self.call(
            "delete_stored",
            Value::Object(args),
            Caller::Loop,
            decode::deleted,
        )
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
            |_| Some(()),
        )
    }
    fn gaps(&self, name: &str) -> Result<Vec<HostGap>, ConfigureError> {
        self.call(
            "gaps",
            json!({"name":name}),
            Caller::Worker(&Cancel::default()),
            decode::gaps,
        )
    }
    fn save_host(&self, name: &str, key: &str, value: &str) -> Result<HostSaved, ConfigureError> {
        self.call(
            "save_host",
            json!({"name":name,"key":key,"value":value}),
            Caller::Loop,
            decode::host_saved,
        )
    }
    fn reviewer_notes(&self, workspace: &Path) -> Result<ReviewerNotes, ConfigureError> {
        self.call(
            "reviewer_notes",
            json!({"workspace":self.workspace(workspace)?}),
            Caller::Loop,
            decode::notes,
        )
    }
    fn save_reviewer_notes(
        &self,
        workspace: &Path,
        global: Option<NotesWrite>,
        project: Option<NotesWrite>,
    ) -> Result<NotesSaved, ConfigureError> {
        let mut args = Map::from_iter([("workspace".into(), json!(self.workspace(workspace)?))]);
        // A layer not sent is absent, never null: the hub refuses an
        // explicit null, as it does on every other operation. Entries
        // travel as an optional per-layer object mapping each command to
        // its flags, omitted when the layer confirms none.
        for (name, sent) in [("global", global), ("project", project)] {
            if let Some(write) = sent {
                let mut layer = Map::from_iter([
                    ("version".into(), json!(write.version)),
                    ("text".into(), json!(write.text)),
                ]);
                if !write.read_only.is_empty() {
                    layer.insert(
                        "read_only".into(),
                        Value::Object(
                            write
                                .read_only
                                .iter()
                                .map(|entry| (entry.command.clone(), json!(entry.flags)))
                                .collect(),
                        ),
                    );
                }
                drop(args.insert(name.into(), Value::Object(layer)));
            }
        }
        self.call(
            "save_reviewer_notes",
            Value::Object(args),
            Caller::Loop,
            decode::notes_saved,
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
        self.call("switch_tool",json!({"workspace":self.workspace(workspace)?,"group":group,"tool":tool,"scope":scope_name(scope),"on":on}),Caller::Loop,|_| Some(()))
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
        self.call("switch_skill",json!({"workspace":self.workspace(workspace)?,"name":name,"scope":scope_name(scope),"on":on}),Caller::Loop,|_| Some(()))
    }
    /// The offer asks about the person's own sources: the hub command
    /// it sends imports the same machine's home, so pending reads
    /// there too. Without the link the read fails and the offer stays
    /// down until the link is up and the hub answers.
    fn import_pending(&self) -> bool {
        matches!(self.import_pending_hub(), Ok(true))
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
/// The seam installed on the app, the remote-only operations if applicable,
/// and the model lists the picker reads.
pub(crate) struct Seams {
    pub(crate) seam: Option<Arc<dyn Configure>>,
    pub(crate) remote: Option<Arc<Remote>>,
    pub(crate) models: Option<crate::ReadModels>,
}
/// Wraps only a remote terminal's seam; local launches retain the original.
/// On a remote terminal the picker's reader is the hub's catalogue, and the
/// client's reader is dropped unread, with or without a seam.
pub(crate) fn seams(
    location: &crate::LaunchLocation,
    seam: Option<Arc<dyn Configure>>,
    models: Option<crate::ReadModels>,
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
            let models = Some(remote.catalogue_reader());
            return Seams {
                seam: Some(Arc::clone(&remote) as Arc<dyn Configure>),
                remote: Some(remote),
                models,
            };
        }
        drop(models);
        return Seams {
            seam: None,
            remote: None,
            models: None,
        };
    }
    Seams {
        seam,
        remote: None,
        models,
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
