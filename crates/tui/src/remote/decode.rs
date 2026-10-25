//! Validates hub answers before the configuration views consume them.

use crate::configure::{
    ConfigureError, FileOutcome, FileSaved, HostGap, HostSaved, Layer, LoginKind, LoginTarget,
    LogoutRow, NotesFile, NotesSaved, ReviewerModels, ReviewerNotes, Revoked, RuleRow,
    RulesSection, Saved, SettingRow, Shown, SkillsDisabled, Stored, ToolGroup, ToolLists,
    ToolSwitches, WriteScope,
};
use contract::{ErrorCode, HubLine};
use serde_json::Value;
use std::path::PathBuf;

pub(super) fn io_error(operation: &str) -> ConfigureError {
    ConfigureError {
        code: ErrorCode::IoFailed,
        message: format!("The {operation} answer could not be read."),
    }
}
pub(super) fn result(line: &HubLine, operation: &str) -> Result<Value, ConfigureError> {
    if line.kind == "command_rejected" {
        return Err(ConfigureError {
            code: line
                .payload
                .get("code")
                .cloned()
                .and_then(|code| serde_json::from_value(code).ok())
                .unwrap_or(ErrorCode::IoFailed),
            message: line
                .payload
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| io_error(operation))?
                .into(),
        });
    }
    if line.kind != "command_accepted" {
        return Err(io_error(operation));
    }
    Ok(line.payload.get("result").cloned().unwrap_or(Value::Null))
}
macro_rules! read {
    ($value:expr, $key:expr) => {
        serde_json::from_value($value.get($key)?.clone()).ok()
    };
}
// An optional field may be absent, but a present field must decode.
macro_rules! optional {
    ($value:expr, $key:expr) => {
        $value
            .get($key)
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .ok()
    };
}

pub(super) fn text(value: &Value) -> Option<String> {
    read!(value, "text")
}
pub(super) fn read_config(value: &Value) -> Option<(String, String)> {
    Some((read!(value, "text")?, read!(value, "version")?))
}
pub(super) fn file(value: &Value) -> Option<PathBuf> {
    read!(value, "file")
}
pub(super) fn saved(value: &Value) -> Option<Saved> {
    Some(Saved {
        file: file(value)?,
        warnings: read!(value, "warnings")?,
    })
}
fn layer(text: &str) -> Option<Layer> {
    match text {
        "global" => Some(Layer::Global),
        "project" => Some(Layer::Project),
        "repository" => Some(Layer::Repository),
        _ => None,
    }
}
pub(super) fn settings(value: &Value) -> Option<Vec<SettingRow>> {
    value
        .get("rows")?
        .as_array()?
        .iter()
        .map(|row| {
            let value = match row.get("shown")?.as_str()? {
                "unset" => Shown::Unset,
                "value" => Shown::Value(read!(row, "value")?),
                "redacted" => Shown::Redacted(read!(row, "value")?),
                "union" => {
                    let own: Vec<(String, String)> = read!(row, "own")?;
                    Shown::Union {
                        names: read!(row, "names")?,
                        own: own
                            .into_iter()
                            .map(|(name, text)| Some((layer(&name)?, text)))
                            .collect::<Option<_>>()?,
                    }
                }
                _ => return None,
            };
            let scope = match row.get("scope")?.as_str()? {
                "any" => WriteScope::Any {
                    repo: read!(row, "repo")?,
                },
                "global_only" => WriteScope::GlobalOnly,
                "repo_only" => WriteScope::RepoOnly,
                "person_files" => WriteScope::PersonFiles,
                _ => return None,
            };
            Some(SettingRow {
                key: read!(row, "key")?,
                layer: read!(row, "layer")?,
                file: optional!(row, "file")?,
                value,
                scope,
            })
        })
        .collect()
}

fn section(value: &Value) -> Option<RulesSection> {
    let rows = if let Some(error) = value.get("error") {
        Err(error.as_str()?.to_owned())
    } else {
        Ok(value
            .get("rows")?
            .as_array()?
            .iter()
            .map(|row| {
                Some(RuleRow {
                    line: read!(row, "line")?,
                    text: read!(row, "text")?,
                    rule: read!(row, "rule")?,
                })
            })
            .collect::<Option<_>>()?)
    };
    Some(RulesSection {
        file: file(value)?,
        rows,
    })
}
pub(super) fn rules(value: &Value) -> Option<(RulesSection, RulesSection)> {
    Some((
        section(value.get("global")?)?,
        section(value.get("project")?)?,
    ))
}
pub(super) fn revoked(value: &Value) -> Option<Revoked> {
    match value.get("revoked")?.as_str()? {
        "removed" => Some(Revoked::Removed),
        "stale" => Some(Revoked::Stale),
        _ => None,
    }
}
pub(super) fn targets(value: &Value) -> Option<Vec<LoginTarget>> {
    value
        .get("targets")?
        .as_array()?
        .iter()
        .map(|target| {
            Some(LoginTarget {
                name: read!(target, "name")?,
                kind: match target.get("kind")?.as_str()? {
                    "key" => LoginKind::Key,
                    "browser" => LoginKind::Browser,
                    "secret" => LoginKind::Secret,
                    _ => return None,
                },
            })
        })
        .collect()
}
pub(super) fn stored(value: &Value) -> Option<Stored> {
    Some(Stored {
        path: read!(value, "path")?,
        replaced: read!(value, "replaced")?,
    })
}
pub(super) fn logout_rows(value: &Value) -> Option<Vec<LogoutRow>> {
    value
        .get("rows")?
        .as_array()?
        .iter()
        .map(|row| match row.get("row")?.as_str()? {
            "provider" => Some(LogoutRow::Provider {
                name: read!(row, "name")?,
                label: read!(row, "label")?,
                default: read!(row, "default")?,
                deletable: read!(row, "deletable")?,
                origin: optional!(row, "origin")?,
                siblings: read!(row, "siblings")?,
            }),
            "secret" => Some(LogoutRow::Secret {
                name: read!(row, "name")?,
                deletable: read!(row, "deletable")?,
            }),
            _ => None,
        })
        .collect()
}
pub(super) fn deleted(value: &Value) -> Option<String> {
    read!(value, "path")
}
pub(super) fn blocking_hooks(value: &Value) -> Option<Vec<String>> {
    read!(value, "names")
}

pub(super) fn install_hooks(value: &Value) -> Option<()> {
    value.as_object()?.is_empty().then_some(())
}

pub(super) fn install(value: &Value) -> Option<crate::ProviderInstall> {
    Some(crate::ProviderInstall {
        installed: read!(value, "installed")?,
        summary: read!(value, "summary")?,
        login: value
            .get("login")?
            .as_array()?
            .iter()
            .map(|need| {
                Some(crate::LoginNeed {
                    provider: read!(need, "provider")?,
                    error: optional!(need, "error")?,
                })
            })
            .collect::<Option<Vec<_>>>()?,
    })
}

pub(super) fn reviewer(value: &Value) -> Option<ReviewerModels> {
    Some(ReviewerModels {
        models: read!(value, "models")?,
        notices: read!(value, "notices")?,
        selected: optional!(value, "selected")?,
    })
}
/// One presence gap per entry: its setting with its declared environment
/// variable, which is `None` when the placeholder declares none.
pub(super) fn gaps(value: &Value) -> Option<Vec<HostGap>> {
    value
        .get("gaps")?
        .as_array()?
        .iter()
        .map(|gap| {
            Some(HostGap {
                name: read!(gap, "name")?,
                env: read!(gap, "env")?,
            })
        })
        .collect()
}
/// What a host save reported: the project settings file hiding the saved
/// value, absent when nothing hides it.
pub(super) fn host_saved(value: &Value) -> Option<HostSaved> {
    Some(HostSaved {
        masked: optional!(value, "masked")?,
    })
}
fn lists(value: &Value) -> Option<ToolLists> {
    Some(ToolLists {
        enabled: optional!(value, "enabled")?,
        disabled: read!(value, "disabled")?,
    })
}
pub(super) fn tools(value: &Value) -> Option<Vec<ToolSwitches>> {
    value
        .get("groups")?
        .as_array()?
        .iter()
        .map(|switch| {
            let group = switch.get("group")?.as_object()?;
            if group.len() != 1 {
                return None;
            }
            let group = if let Some(name) = group.get("mcp") {
                ToolGroup::Mcp(name.as_str()?.into())
            } else {
                ToolGroup::Extension(group.get("extension")?.as_str()?.into())
            };
            Some(ToolSwitches {
                group,
                project: lists(switch.get("project")?)?,
                everywhere: lists(switch.get("everywhere")?)?,
            })
        })
        .collect()
}
/// Both person layers' notes: each layer names its file with its own
/// text and version. A layer missing any of the three is refused.
pub(super) fn notes(value: &Value) -> Option<ReviewerNotes> {
    Some(ReviewerNotes {
        global: notes_file(value.get("global")?)?,
        project: notes_file(value.get("project")?)?,
    })
}

/// One notes layer's file, text and version.
fn notes_file(value: &Value) -> Option<NotesFile> {
    Some(NotesFile {
        file: read!(value, "file")?,
        text: read!(value, "text")?,
        version: read!(value, "version")?,
    })
}

/// A notes save: `stale` writes nothing, `written` carries one entry per
/// sent layer. An entry that was not saved without an error explains
/// nothing, so it is refused.
pub(super) fn notes_saved(value: &Value) -> Option<NotesSaved> {
    match value.get("saved")?.as_str()? {
        "stale" => Some(NotesSaved::Stale),
        "written" => Some(NotesSaved::Written(
            value
                .get("files")?
                .as_array()?
                .iter()
                .map(file_saved)
                .collect::<Option<Vec<_>>>()?,
        )),
        _ => None,
    }
}

/// One sent file's outcome: `saved` true without an error wrote cleanly,
/// `saved` true with an error wrote with the rename already in place, and
/// `saved` false needs its error.
fn file_saved(value: &Value) -> Option<FileSaved> {
    let saved: bool = read!(value, "saved")?;
    let error: Option<ConfigureError> = match value.get("error") {
        None => None,
        Some(error) => Some(ConfigureError {
            code: read!(error, "code")?,
            message: read!(error, "message")?,
        }),
    };
    let outcome = match (saved, error) {
        (true, None) => FileOutcome::Saved,
        (true, Some(error)) => FileOutcome::SavedUnsynced(error),
        (false, Some(error)) => FileOutcome::NotSaved(error),
        (false, None) => return None,
    };
    Some(FileSaved {
        file: read!(value, "file")?,
        outcome,
    })
}

pub(super) fn disabled(value: &Value) -> Option<SkillsDisabled> {
    Some(SkillsDisabled {
        project: read!(value, "project")?,
        everywhere: read!(value, "everywhere")?,
    })
}

#[cfg(test)]
#[path = "decode_tests.rs"]
mod tests;
