//! Wire shapes for configuration on the hub's machine.
use std::fs::{self, File, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use config::{Imported, LeftBehind};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::shapes::Failure;
use serde_json::{Map, Value, json};
use tui::{
    Configure, ConfigureError, Layer, LoginKind, Revoked, RulesScope, RulesSection, Shown,
    SwitchScope, ToolGroup, ToolLists, WriteScope,
};

use super::Seam;

fn failure(code: ErrorCode, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
        provider: None,
        retry_after_ms: None,
    }
}

fn invalid() -> Failure {
    failure(
        ErrorCode::InvalidArguments,
        "The arguments do not fit this command.",
    )
}

fn from_view(error: ConfigureError) -> Failure {
    failure(error.code, error.message)
}

fn client_key(key: &str) -> bool {
    matches!(
        config::path::parse(key)
            .and_then(|parts| parts.into_iter().next())
            .as_deref(),
        Some("keys" | "tui")
    )
}

struct Args<'a>(&'a Map<String, Value>);

impl<'a> Args<'a> {
    fn shape(&self, required: &[&str], optional: &[&str]) -> Result<(), Failure> {
        for key in self.0.keys() {
            if !required.contains(&key.as_str()) && !optional.contains(&key.as_str()) {
                return Err(invalid());
            }
        }
        for key in required {
            self.value(key)?;
        }
        Ok(())
    }
    fn value(&self, key: &str) -> Result<&'a Value, Failure> {
        self.0.get(key).ok_or_else(invalid)
    }
    fn string(&self, key: &str) -> Result<&'a str, Failure> {
        self.value(key)?.as_str().ok_or_else(invalid)
    }
    fn workspace(&self) -> Result<&'a Path, Failure> {
        let path = Path::new(self.string("workspace")?);
        if !path.is_absolute() {
            return Err(invalid());
        }
        Ok(path)
    }
    fn scope(&self) -> Result<SwitchScope, Failure> {
        match self.string("scope")? {
            "project" => Ok(SwitchScope::Project),
            "everywhere" => Ok(SwitchScope::Everywhere),
            _ => Err(invalid()),
        }
    }
    fn group(&self) -> Result<ToolGroup, Failure> {
        let group = self.value("group")?.as_object().ok_or_else(invalid)?;
        if group.len() != 1 {
            return Err(invalid());
        }
        let args = Args(group);
        if group.contains_key("mcp") {
            return Ok(ToolGroup::Mcp(args.string("mcp")?.to_owned()));
        }
        Ok(ToolGroup::Extension(args.string("extension")?.to_owned()))
    }
}

/// One sent notes layer from the request: absent reads as not sent, and
/// a present layer names exactly its `version` with its `text` plus its
/// optional `read_only` entries. A layer of the wrong shape, with an
/// extra key, or missing its version is not a save at all.
fn layer_write(args: &Map<String, Value>, name: &str) -> Result<Option<tui::NotesWrite>, Failure> {
    match args.get(name) {
        None => Ok(None),
        Some(value) => {
            let layer = value.as_object().ok_or_else(invalid)?;
            Args(layer).shape(&["version", "text"], &["read_only"])?;
            let string = |key: &str| layer.get(key).and_then(Value::as_str).ok_or_else(invalid);
            Ok(Some(tui::NotesWrite {
                version: string("version")?.to_owned(),
                text: string("text")?.to_owned(),
                read_only: read_only_entries(layer)?,
            }))
        }
    }
}

/// One layer's optional read-only entries: absent means no entry changes.
/// A present value maps each command path to its flags array; anything
/// else — the wrong shape, the wrong types or an explicit null — is no
/// save at all. Entry semantics stay with the configuration save.
fn read_only_entries(layer: &Map<String, Value>) -> Result<Vec<tui::ReadOnlyEntry>, Failure> {
    match layer.get("read_only") {
        None => Ok(Vec::new()),
        Some(value) => value
            .as_object()
            .ok_or_else(invalid)?
            .iter()
            .map(|(command, flags)| {
                let flags = flags
                    .as_array()
                    .ok_or_else(invalid)?
                    .iter()
                    .map(|flag| flag.as_str().ok_or_else(invalid).map(str::to_owned))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(tui::ReadOnlyEntry {
                    command: command.clone(),
                    flags,
                })
            })
            .collect(),
    }
}

/// One notes layer's file, text and version for the wire.
fn notes_file(file: tui::NotesFile) -> Value {
    json!({"file":file.file,"text":file.text,"version":file.version})
}

/// A notes save for the wire: `stale` writes nothing, `written` carries
/// one entry per sent layer, global first. An unsynced file is written
/// with its error; any other failure is not written, with why.
fn notes_saved(saved: tui::NotesSaved) -> Value {
    match saved {
        tui::NotesSaved::Stale => json!({"saved":"stale"}),
        tui::NotesSaved::Written(files) => {
            json!({"saved":"written","files":files.into_iter().map(|saved| {
            let mut entry = Map::from_iter([("file".to_owned(), json!(saved.file))]);
            let (written, error) = match saved.outcome {
                tui::FileOutcome::Saved => (true, None),
                tui::FileOutcome::SavedUnsynced(error) => (true, Some(error)),
                tui::FileOutcome::NotSaved(error) => (false, Some(error)),
            };
            drop(entry.insert("saved".to_owned(), json!(written)));
            if let Some(error) = error {
                drop(entry.insert(
                    "error".to_owned(),
                    json!({"code":error.code,"message":error.message}),
                ));
            }
            Value::Object(entry)
        }).collect::<Vec<_>>()})
        }
    }
}

fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Global => "global",
        Layer::Project => "project",
        Layer::Repository => "repository",
    }
}

fn setting(row: tui::SettingRow) -> Value {
    let mut value = Map::from_iter([
        ("key".to_owned(), json!(row.key)),
        ("layer".to_owned(), json!(row.layer)),
    ]);
    if let Some(file) = row.file {
        drop(value.insert("file".to_owned(), json!(file)));
    }
    match row.value {
        Shown::Unset => drop(value.insert("shown".to_owned(), json!("unset"))),
        Shown::Value(text) => {
            drop(value.insert("shown".to_owned(), json!("value")));
            drop(value.insert("value".to_owned(), json!(text)));
        }
        Shown::Redacted(text) => {
            drop(value.insert("shown".to_owned(), json!("redacted")));
            drop(value.insert("value".to_owned(), json!(text)));
        }
        Shown::Union { names, own } => {
            drop(value.insert("shown".to_owned(), json!("union")));
            drop(value.insert("names".to_owned(), json!(names)));
            drop(value.insert(
                "own".to_owned(),
                json!(
                own.into_iter()
                    .map(|(layer, text)| (layer_name(layer), text))
                    .collect::<Vec<_>>()
            ),
            ));
        }
    }
    match row.scope {
        WriteScope::Any { repo } => {
            drop(value.insert("scope".to_owned(), json!("any")));
            drop(value.insert("repo".to_owned(), json!(repo)));
        }
        WriteScope::GlobalOnly => drop(value.insert("scope".to_owned(), json!("global_only"))),
        WriteScope::RepoOnly => drop(value.insert("scope".to_owned(), json!("repo_only"))),
        WriteScope::PersonFiles => drop(value.insert("scope".to_owned(), json!("person_files"))),
    }
    Value::Object(value)
}

fn section(section: RulesSection) -> Value {
    let mut result = Map::from_iter([("file".to_owned(), json!(section.file))]);
    match section.rows {
        Ok(rows) => drop(result.insert(
            "rows".to_owned(),
            json!(
                rows.into_iter()
                    .map(|row| json!({"line":row.line,"text":row.text,"rule":row.rule}))
                    .collect::<Vec<_>>()
            ),
        )),
        Err(error) => drop(result.insert("error".to_owned(), json!(error))),
    }
    Value::Object(result)
}

fn lists(lists: ToolLists) -> Value {
    let mut value = Map::from_iter([("disabled".to_owned(), json!(lists.disabled))]);
    if let Some(enabled) = lists.enabled {
        drop(value.insert("enabled".to_owned(), json!(enabled)));
    }
    Value::Object(value)
}

struct FileSearch {
    listing: tui::files::Listing,
    roots: tui::files::Roots,
}
impl hub::FileSearch for FileSearch {
    fn search(
        &mut self,
        text: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Result<Vec<String>, String>> {
        tui::files::search(&mut self.listing, &self.roots, text, cancelled)
    }
}
use hub::ShowCode;
struct DeviceShow {
    shown: ShowCode,
    opened: Arc<AtomicBool>,
}
impl tui::LoginShow for DeviceShow {
    fn open(&self, _url: &str) {
        self.opened.store(true, Ordering::SeqCst);
    }
    fn show(&self, url: &str, code: &str) {
        (self.shown)(url, code);
    }
}
struct DeviceLogin {
    login: Arc<dyn tui::BrowserLogin>,
    opened: Arc<AtomicBool>,
    name: String,
}
impl hub::DeviceLogin for DeviceLogin {
    fn run(&self) -> Result<String, Failure> {
        let result = self.login.run();
        if self.opened.load(Ordering::SeqCst) {
            return Err(failure(
                ErrorCode::AuthenticationFailed,
                format!("`{}` has no device-code form.", self.name),
            ));
        }
        result.map(|stored| stored.path).map_err(from_view)
    }
    fn cancel(&self) {
        self.login.cancel();
    }
}
impl hub::Machine for Seam {
    fn import(
        &self,
        home: &Path,
        source: Option<&str>,
        clock: Arc<dyn Clock>,
    ) -> Result<Option<Value>, Failure> {
        // The hub already refused anything but the two; this names
        // them again so the seam never misroutes a source.
        let sources: &[config::ImportSource] = match source {
            None => &[
                config::ImportSource::ClaudeCode,
                config::ImportSource::Codex,
            ],
            Some("claude-code") => &[config::ImportSource::ClaudeCode],
            Some("codex") => &[config::ImportSource::Codex],
            Some(_) => {
                return Err(failure(
                    ErrorCode::InvalidArguments,
                    "`source` must be `claude-code` or `codex`.",
                ));
            }
        };
        // The ports the driver would print, minus the printing: each
        // source's report extended into one answer, a failed source
        // refusing the rest rather than answering a partial report. The
        // person's home gives the `~/.agents/skills` place, when it is
        // absolute, as the local import reads it; a relative one leaves
        // that place unread.
        let agents = home.is_absolute().then(|| home.to_path_buf());
        let check = crate::prompt_files::personal_source_check(
            self.home.clone(),
            agents,
            Arc::clone(&clock),
        );
        let mut imported = Imported::default();
        let mut left_behind = Vec::new();
        for source in sources {
            let report = config::import_source(
                &self.home,
                &config::source_home(home, *source),
                *source,
                check.as_ref(),
                clock.as_ref(),
            )
            .map_err(|error| failure(error.code(), error.to_string()))?;
            imported.mcp_servers.extend(report.imported.mcp_servers);
            imported.skills.extend(report.imported.skills);
            imported.commands.extend(report.imported.commands);
            imported.instructions.extend(report.imported.instructions);
            left_behind.extend(report.left_behind.into_iter().map(|left| LeftBehind {
                item: left.item,
                reason: left.reason,
            }));
        }
        Ok(Some(
            json!({"imported": imported, "left_behind": left_behind}),
        ))
    }
    fn file_search(&self, workspace: &Path, home: Option<&Path>) -> Box<dyn hub::FileSearch> {
        let workspace = workspace.to_path_buf();
        let roots = tui::files::Roots::new(
            workspace.clone(),
            home.map(|home| home.as_os_str().to_owned()),
        );
        Box::new(FileSearch {
            listing: tui::files::Listing::new(move || tui::files::list(&workspace)),
            roots,
        })
    }
    fn device_login(
        &self,
        name: &str,
        shown: ShowCode,
        clock: Arc<dyn Clock>,
    ) -> Arc<dyn hub::DeviceLogin> {
        let opened = Arc::new(AtomicBool::new(false));
        let shown = Arc::new(DeviceShow {
            shown,
            opened: opened.clone(),
        });
        let login = super::login::browser_login(
            &self.home,
            name,
            shown,
            clock,
            extensions::LoginMethod::Device,
        );
        Arc::new(DeviceLogin {
            login,
            opened,
            name: name.to_owned(),
        })
    }
    fn configure(
        &self,
        operation: &str,
        args: &Map<String, Value>,
        clock: Arc<dyn Clock>,
    ) -> Result<Option<Value>, Failure> {
        let a = Args(args);
        let result = match operation {
            "settings" => {
                a.shape(&["workspace"], &[])?;
                let rows = self.settings(a.workspace()?).map_err(from_view)?;
                json!({"rows":rows.into_iter().filter(|row|!client_key(&row.key)).map(setting).collect::<Vec<_>>()})
            }
            "set" => {
                a.shape(&["workspace", "layer", "key", "text"], &[])?;
                let layer = match a.string("layer")? {
                    "global" => Layer::Global,
                    "project" => Layer::Project,
                    "repository" => Layer::Repository,
                    _ => return Err(invalid()),
                };
                let key = a.string("key")?;
                if client_key(key) {
                    return Err(invalid());
                }
                let saved = self
                    .set(a.workspace()?, layer, key, a.string("text")?)
                    .map_err(from_view)?;
                json!({"file":saved.file,"warnings":saved.warnings})
            }
            "global_file" => {
                a.shape(&[], &[])?;
                json!({"file":self.global_file()})
            }
            "rules" => {
                a.shape(&["workspace"], &[])?;
                let (global, project) = self.rules(a.workspace()?).map_err(from_view)?;
                json!({"global":section(global),"project":section(project)})
            }
            "revoke" => {
                a.shape(&["workspace", "scope", "line", "text"], &[])?;
                let scope = match a.string("scope")? {
                    "global" => RulesScope::Global,
                    "project" => RulesScope::Project,
                    _ => return Err(invalid()),
                };
                let line = a
                    .value("line")?
                    .as_u64()
                    .and_then(|line| usize::try_from(line).ok())
                    .ok_or_else(invalid)?;
                let revoked = self
                    .revoke(a.workspace()?, scope, line, a.string("text")?)
                    .map_err(from_view)?;
                json!({"revoked":match revoked {Revoked::Removed=>"removed",Revoked::Stale=>"stale"}})
            }
            "login_targets" => {
                a.shape(&[], &[])?;
                json!({"targets":self.login_targets().map_err(from_view)?.into_iter().map(|t|json!({"name":t.name,"kind":match t.kind{LoginKind::Key=>"key",LoginKind::Browser=>"browser",LoginKind::Secret=>"secret"}})).collect::<Vec<_>>()})
            }
            "logout_rows" => {
                a.shape(&[], &[])?;
                json!({"rows":self.logout_rows().map_err(from_view)?.into_iter().map(|row|{
                    match row {
                        tui::LogoutRow::Provider { name, label, default, deletable, origin, siblings } => {
                            let mut value = Map::from_iter([
                                ("row".to_owned(), json!("provider")),
                                ("name".to_owned(), json!(name)),
                                ("label".to_owned(), json!(label)),
                                ("default".to_owned(), json!(default)),
                                ("deletable".to_owned(), json!(deletable)),
                                ("siblings".to_owned(), json!(siblings)),
                            ]);
                            if let Some(origin) = origin {
                                drop(value.insert("origin".to_owned(), json!(origin)));
                            }
                            Value::Object(value)
                        }
                        tui::LogoutRow::Secret { name, deletable } => json!({
                            "row": "secret",
                            "name": name,
                            "deletable": deletable,
                        }),
                    }
                }).collect::<Vec<_>>()})
            }
            "delete_stored" => {
                a.shape(&["name"], &["label"])?;
                let label = if args.contains_key("label") {
                    Some(a.string("label")?.to_owned())
                } else {
                    None
                };
                let path = self
                    .delete_stored(a.string("name")?, label)
                    .map_err(from_view)?;
                json!({"path":path})
            }
            "store_key" => {
                a.shape(&["name", "key"], &["label"])?;
                let label = if args.contains_key("label") {
                    Some(a.string("label")?)
                } else {
                    None
                };
                let stored = self
                    .store_key(
                        a.string("name")?,
                        label,
                        contract::Secret::new(a.string("key")?.to_owned()),
                    )
                    .map_err(from_view)?;
                json!({"path":stored.path,"replaced":stored.replaced})
            }
            "reviewer_models" => {
                a.shape(&["name"], &[])?;
                let models = self
                    .reviewer_models(a.string("name")?, clock)
                    .map_err(from_view)?;
                let mut value = Map::from_iter([
                    ("models".to_owned(), json!(models.models)),
                    ("notices".to_owned(), json!(models.notices)),
                ]);
                if let Some(selected) = models.selected {
                    drop(value.insert("selected".to_owned(), json!(selected)));
                }
                Value::Object(value)
            }
            "blocking_hooks" => {
                a.shape(&["workspace"], &["overrides"])?;
                let workspace = a.workspace()?;
                let overrides = args
                    .get("overrides")
                    .map(|value| {
                        serde_json::from_value::<Vec<String>>(value.clone()).map_err(|_| invalid())
                    })
                    .transpose()?
                    .unwrap_or_default();
                let names = self
                    .blocking_hooks(workspace, &overrides)
                    .map_err(from_view)?;
                json!({"names":names})
            }
            "install_hooks" => {
                a.shape(&[], &[])?;
                self.install_hooks(clock).map_err(from_view)?;
                json!({})
            }
            "install_provider" => {
                a.shape(&["workspace", "provider"], &[])?;
                let installed = self
                    .install_provider(a.workspace()?, a.string("provider")?, clock)
                    .map_err(from_view)?;
                let login: Vec<_> = installed
                    .login
                    .into_iter()
                    .map(|need| {
                        let mut value =
                            Map::from_iter([("provider".to_owned(), json!(need.provider))]);
                        if let Some(error) = need.error {
                            drop(value.insert("error".to_owned(), json!(error)));
                        }
                        Value::Object(value)
                    })
                    .collect();
                json!({"installed":installed.installed,"summary":installed.summary,"login":login})
            }
            "models" => {
                a.shape(&["workspace"], &["refresh"])?;
                // An absent `refresh` reads the cached lists; a present
                // one names its thoroughness, and anything else is not a
                // refresh at all.
                let refresh = match args.get("refresh") {
                    None => tui::Refresh::Cached,
                    Some(value) => match value.as_str() {
                        Some("cached") => tui::Refresh::Cached,
                        Some("stale") => tui::Refresh::Stale,
                        Some("every") => tui::Refresh::Every,
                        _ => return Err(invalid()),
                    },
                };
                self.models(a.workspace()?, refresh, clock)?
            }
            "set_reviewer" => {
                a.shape(&["name", "id"], &[])?;
                self.set_reviewer(a.string("name")?, a.string("id")?)
                    .map_err(from_view)?;
                return Ok(None);
            }
            "gaps" => {
                a.shape(&["name"], &[])?;
                let gaps = self.gaps(a.string("name")?).map_err(from_view)?;
                json!({"gaps":gaps.into_iter().map(|gap|json!({"name":gap.name,"env":gap.env})).collect::<Vec<_>>()})
            }
            "save_host" => {
                a.shape(&["name", "key", "value"], &[])?;
                let saved = self
                    .save_host(a.string("name")?, a.string("key")?, a.string("value")?)
                    .map_err(from_view)?;
                let mut value = Map::new();
                if let Some(masked) = saved.masked {
                    drop(value.insert("masked".to_owned(), json!(masked)));
                }
                Value::Object(value)
            }
            "reviewer_notes" => {
                a.shape(&["workspace"], &[])?;
                let notes = self.reviewer_notes(a.workspace()?).map_err(from_view)?;
                json!({"global":notes_file(notes.global),"project":notes_file(notes.project)})
            }
            "save_reviewer_notes" => {
                a.shape(&["workspace"], &["global", "project"])?;
                let saved = self
                    .save_reviewer_notes(
                        a.workspace()?,
                        layer_write(args, "global")?,
                        layer_write(args, "project")?,
                    )
                    .map_err(from_view)?;
                return Ok(Some(notes_saved(saved)));
            }
            "tool_switches" => {
                a.shape(&["workspace"], &[])?;
                json!({"groups":self.tool_switches(a.workspace()?).map_err(from_view)?.into_iter().map(|s|json!({"group":match s.group{ToolGroup::Mcp(name)=>json!({"mcp":name}),ToolGroup::Extension(name)=>json!({"extension":name})},"project":lists(s.project),"everywhere":lists(s.everywhere)})).collect::<Vec<_>>()})
            }
            "switch_tool" => {
                a.shape(&["workspace", "group", "tool", "scope", "on"], &[])?;
                self.switch_tool(
                    a.workspace()?,
                    &a.group()?,
                    a.string("tool")?,
                    a.scope()?,
                    a.value("on")?.as_bool().ok_or_else(invalid)?,
                )
                .map_err(from_view)?;
                return Ok(None);
            }
            "skills_disabled" => {
                a.shape(&["workspace"], &[])?;
                let disabled = self.skills_disabled(a.workspace()?).map_err(from_view)?;
                json!({"project":disabled.project,"everywhere":disabled.everywhere})
            }
            "switch_skill" => {
                a.shape(&["workspace", "name", "scope", "on"], &[])?;
                self.switch_skill(
                    a.workspace()?,
                    a.string("name")?,
                    a.scope()?,
                    a.value("on")?.as_bool().ok_or_else(invalid)?,
                )
                .map_err(from_view)?;
                return Ok(None);
            }
            "skill_text" => {
                a.shape(&["path"], &[])?;
                json!({"text":self.skill_text(Path::new(a.string("path")?)).map_err(from_view)?})
            }
            // Whether the first-run offer shows on a remote terminal:
            // neither source has a completed receipt under the hub
            // machine's own Fiber home, as the hub's `import` writes
            // them there, never under the terminal's home.
            "import_pending" => {
                a.shape(&[], &[])?;
                json!({"pending":config::import_pending(&self.home)})
            }
            _ => return Err(invalid()),
        };
        Ok(Some(result))
    }
    fn read_config(&self, workspace: &Path, path: &Path) -> Result<(String, String), Failure> {
        self.config_path(workspace, path)?;
        let bytes = read_bytes(path)?;
        let version = version_of(bytes.as_deref());
        let text =
            String::from_utf8(bytes.unwrap_or_default()).map_err(|error| io_failed(path, error))?;
        Ok((text, version))
    }
    fn write_config(
        &self,
        workspace: &Path,
        path: &Path,
        text: &str,
        version: &str,
    ) -> Result<(), Failure> {
        self.config_path(workspace, path)?;
        cap(text.len())?;
        // Config and rules writes serialize the whole read-compare-write.
        // Skill files are editor-owned: no lock file is left in a repository.
        let _lock = if path.file_name() == Some(std::ffi::OsStr::new("SKILL.md")) {
            None
        } else {
            let parent = path.parent().ok_or_else(invalid)?;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|error| io_failed(parent, error))?;
            let mut name = path.as_os_str().to_owned();
            name.push(".lock");
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .mode(0o666)
                .open(name)
                .map_err(|error| io_failed(path, error))?;
            lock.lock().map_err(|error| io_failed(path, error))?;
            Some(lock)
        };
        let bytes = read_bytes(path)?;
        if version_of(bytes.as_deref()) != version {
            return Err(failure(
                ErrorCode::StaleRequest,
                format!("{} changed since it was read.", path.display()),
            ));
        }
        config::write_atomic(path, text.as_bytes(), 0o666)
            .map_err(|error| failure(error.code(), error.to_string()))
    }
    #[allow(
        clippy::too_many_arguments,
        clippy::type_complexity,
        reason = "one seam call carries the whole prune: its flags, the previewed items and the session-delete callback"
    )]
    fn prune(
        &self,
        home: &Path,
        older_than: Option<String>,
        cascade: bool,
        force: bool,
        dry_run: bool,
        expect: Option<contract::commands::hub::PruneExpect>,
        clock: Arc<dyn Clock>,
        delete: &mut (
                 dyn FnMut(
            &contract::SessionId,
            bool,
            Option<&std::collections::BTreeSet<contract::SessionId>>,
        ) -> Result<(), Failure>
                     + Send
             ),
    ) -> Result<hub::PruneOutcome, Failure> {
        let older_than = match older_than {
            None => None,
            Some(text) => match config::parse_duration(&text) {
                Some(duration) => Some(duration),
                // Before anything is read: an unreadable duration never
                // reaches the selection, as locally.
                None => return Err(invalid()),
            },
        };
        if dry_run {
            let selection = cli::select_for_hub(
                home,
                older_than,
                cascade,
                force,
                clock.wall(),
                clock.as_ref(),
            );
            return Ok(hub::PruneOutcome {
                lines: selection.lines,
                items: contract::commands::hub::PruneExpect {
                    sessions: selection
                        .sessions
                        .into_iter()
                        .map(|session| contract::commands::hub::PruneSession {
                            id: session.id,
                            cascade: session.expect.into_iter().collect(),
                        })
                        .collect(),
                    worktrees: selection.worktrees,
                    pinned: selection.pinned,
                    diagnostics: selection.diagnostics,
                },
                freed: selection.would_free,
                total: 0,
                failures: Vec::new(),
            });
        }
        // Without `expect` (`--yes`) there is no preview to confirm:
        // the confirm removes the current selection, as locally.
        let want = expect.map(|expect| cli::HubPruneWant {
            sessions: expect
                .sessions
                .into_iter()
                .map(|session| (session.id, session.cascade.into_iter().collect()))
                .collect(),
            worktrees: expect.worktrees.into_iter().collect(),
            pinned: expect.pinned.into_iter().collect(),
            diagnostics: expect.diagnostics.into_iter().collect(),
        });
        // The hub-supplied callback passes through untouched: no loopback
        // connection, no hub socket path. The confirm preflights the
        // exact previewed selection first: any mismatch is `stale_request`
        // with nothing removed, and the callback runs only after the
        // preflight succeeds.
        let removed = cli::remove_for_hub(
            home,
            want.as_ref(),
            older_than,
            cascade,
            force,
            clock.as_ref(),
            delete,
        )?;
        Ok(hub::PruneOutcome {
            lines: Vec::new(),
            items: contract::commands::hub::PruneExpect {
                sessions: Vec::new(),
                worktrees: Vec::new(),
                pinned: Vec::new(),
                diagnostics: Vec::new(),
            },
            freed: removed.freed,
            total: removed.total,
            failures: removed
                .failures
                .into_iter()
                .map(|(item, code, message)| hub::PruneFailure {
                    item,
                    code,
                    message,
                })
                .collect(),
        })
    }
}

const TEXT_LIMIT: usize = 64 * 1024;

impl Seam {
    fn config_path(&self, workspace: &Path, path: &Path) -> Result<(), Failure> {
        let project = self.project_key(workspace)?;
        if config::is_editable_config_path(&self.home, workspace, &project, path) {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}

fn cap(length: usize) -> Result<(), Failure> {
    if length > TEXT_LIMIT {
        return Err(failure(
            ErrorCode::TooLarge,
            "Configuration text exceeds 64 KiB.",
        ));
    }
    Ok(())
}

fn io_failed(path: &Path, error: impl std::fmt::Display) -> Failure {
    failure(ErrorCode::IoFailed, format!("{}: {error}", path.display()))
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, Failure> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_failed(path, error)),
    };
    let mut bytes = Vec::new();
    file.take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|error| io_failed(path, error))?;
    cap(bytes.len())?;
    Ok(Some(bytes))
}

fn version_of(bytes: Option<&[u8]>) -> String {
    match bytes {
        None => "missing".to_owned(),
        Some(bytes) => {
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        }
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
