//! Wire shapes for configuration on the hub's machine.
use std::fs::{self, File, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
            "set_reviewer" => {
                a.shape(&["name", "id"], &[])?;
                self.set_reviewer(a.string("name")?, a.string("id")?)
                    .map_err(from_view)?;
                return Ok(None);
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
}

const TEXT_LIMIT: usize = 64 * 1024;

impl Seam {
    fn config_path(&self, workspace: &Path, path: &Path) -> Result<(), Failure> {
        let (_, project) = cli::project_of(&self.home, workspace)?;
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
