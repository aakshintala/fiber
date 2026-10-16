//! Wire shapes for configuration on the hub's machine.
use std::path::Path;
use std::sync::Arc;

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
    let mut value = json!({"key":row.key, "layer":row.layer});
    if let Some(file) = row.file {
        value["file"] = json!(file);
    }
    match row.value {
        Shown::Unset => value["shown"] = json!("unset"),
        Shown::Value(text) => {
            value["shown"] = json!("value");
            value["value"] = json!(text);
        }
        Shown::Redacted(text) => {
            value["shown"] = json!("redacted");
            value["value"] = json!(text);
        }
        Shown::Union { names, own } => {
            value["shown"] = json!("union");
            value["names"] = json!(names);
            value["own"] = json!(
                own.into_iter()
                    .map(|(layer, text)| (layer_name(layer), text))
                    .collect::<Vec<_>>()
            );
        }
    }
    match row.scope {
        WriteScope::Any { repo } => {
            value["scope"] = json!("any");
            value["repo"] = json!(repo);
        }
        WriteScope::GlobalOnly => value["scope"] = json!("global_only"),
        WriteScope::RepoOnly => value["scope"] = json!("repo_only"),
        WriteScope::PersonFiles => value["scope"] = json!("person_files"),
    }
    value
}

fn section(section: RulesSection) -> Value {
    let mut result = json!({"file":section.file});
    match section.rows {
        Ok(rows) => {
            result["rows"] = json!(
                rows.into_iter()
                    .map(|row| json!({"line":row.line,"text":row.text,"rule":row.rule}))
                    .collect::<Vec<_>>()
            )
        }
        Err(error) => result["error"] = json!(error),
    }
    result
}

fn lists(lists: ToolLists) -> Value {
    let mut value = json!({"disabled":lists.disabled});
    if let Some(enabled) = lists.enabled {
        value["enabled"] = json!(enabled);
    }
    value
}

impl hub::Machine for Seam {
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
                let mut value = json!({"models":models.models,"notices":models.notices});
                if let Some(selected) = models.selected {
                    value["selected"] = json!(selected);
                }
                value
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
    fn read_config(&self, _workspace: &Path, _path: &Path) -> Result<(String, String), Failure> {
        Err(failure(ErrorCode::IoFailed, "not built yet"))
    }
    fn write_config(
        &self,
        _workspace: &Path,
        _path: &Path,
        _text: &str,
        _version: &str,
    ) -> Result<(), Failure> {
        Err(failure(ErrorCode::IoFailed, "not built yet"))
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
