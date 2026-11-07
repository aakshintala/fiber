//! The `reviewer_notes_save` tool: the model saves reviewer notes the
//! person confirms, both layers at once (`docs/permissions.md`, "Setting
//! up the notes").

use std::path::PathBuf;

use contract::emit::Emit;
use contract::events::{Answer, FileChange, Interaction};
use contract::notes::{Topic, parse_read_only_entry, render_read_only_entry};
use contract::provider::ToolDefinition;
use contract::shapes::ContentPart;
use contract::tool::{Answered, Ask, Asking, Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use crate::error::ReviewerNotesError;
use crate::reviewer_notes::{DiffRow, diff, merge, owned};
use crate::tool_util::{errored, no_effects, text_output};

/// The result when the save changes nothing.
const UNCHANGED: &str = "unchanged";

/// The result when the person declines the confirm.
const DECLINED: &str = "declined";

/// The result when the notes changed under the save.
const STALE: &str = "The reviewer notes changed while the person answered; nothing was written.";

/// One draft line: a bullet or a read-only entry for one layer.
struct DraftLine {
    topic: Topic,
    layer: config::EntryLayer,
    text: String,
}

/// Saves confirmed reviewer notes to both layers' files. It declares no
/// effects of its own: every write goes through the person's confirm,
/// which the session suspends on never (`docs/tools.md`, "Asking the
/// person"), so the tool is sent with `strict: false`.
pub struct ReviewerNotesSave {
    home: PathBuf,
    key: config::ProjectKey,
}

impl ReviewerNotesSave {
    /// Saves to the layers under `home` for `key`.
    pub fn new(home: PathBuf, key: config::ProjectKey) -> Self {
        Self { home, key }
    }
}

impl Tool for ReviewerNotesSave {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "reviewer_notes_save".to_owned(),
            description: "Saves reviewer notes the person confirms: draft the notes as lines \
                with a topic, a layer and text, and read-only command lines as entries. The \
                call shows the person the diff and the entries it would write; on yes it \
                writes both layers' files. It needs the person's confirm, so it refuses in \
                a program-driven session."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "lines": {
                        "type": "array",
                        "description": "The draft notes lines: one item per bullet or read-only entry.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "topic": {
                                    "type": "string",
                                    "enum": [
                                        "source_control",
                                        "internal_hosts",
                                        "internal_tools",
                                        "routine_actions",
                                        "never",
                                        "read_only_commands"
                                    ],
                                    "description": "The heading the line belongs under."
                                },
                                "layer": {
                                    "type": "string",
                                    "enum": ["global", "project"],
                                    "description": "The layer the line belongs in."
                                },
                                "text": {
                                    "type": "string",
                                    "description": "The bullet text, or the entry line for read_only_commands."
                                }
                            },
                            "required": ["topic", "layer", "text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["lines"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(no_effects())
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        errored(ReviewerNotesError::ProgramDriven)
    }

    /// Reads both layers, stages the notes and entry additions, and raises
    /// one confirm carrying the diff and the entries; on yes it saves
    /// through the config writer, which re-validates under its locks.
    fn run_asking(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        if !ask.answerable() {
            return self.run(arguments, cancel, emit);
        }
        let lines = match draft_lines(arguments) {
            Ok(lines) => lines,
            Err(error) => return errored(error),
        };
        let read = match config::reviewer_notes(&self.home, &self.key) {
            Ok(read) => read,
            Err(error) => return errored(ReviewerNotesError::Config(error)),
        };
        let global = match stage_layer(&read.global.text, &lines, config::EntryLayer::Global) {
            Ok(staged) => (config::EntryLayer::Global, &read.global, staged),
            Err(error) => return errored(error),
        };
        let project = match stage_layer(&read.project.text, &lines, config::EntryLayer::Project) {
            Ok(staged) => (config::EntryLayer::Project, &read.project, staged),
            Err(error) => return errored(error),
        };
        let staged = [global, project];
        let entries = staged_entries(&staged);
        let preview = match config::preview_read_only_changes(
            &self.home, &self.key, &entries.0, &entries.1,
        ) {
            Ok(preview) => preview,
            Err(error) => return errored(ReviewerNotesError::Config(error)),
        };
        let sections = confirm_sections(&staged, &preview);
        if sections.iter().all(|section| section.rows.is_empty()) {
            return text_output(UNCHANGED.to_owned());
        }
        let prompt = sections
            .iter()
            .flat_map(|section| {
                let mut out = vec![section.file.clone()];
                out.extend(section.rows.iter().cloned());
                out
            })
            .collect::<Vec<_>>()
            .join("\n");
        let answer = ask.ask(Asking {
            interaction: Interaction::Confirm {
                prompt: format!("Save these reviewer notes?\n{prompt}"),
            },
            action_ids: Vec::new(),
            until: None,
            check: None,
            // The person is already deciding: parking the turn would hold
            // the save past the answer for no review.
            suspends: false,
        });
        match answer {
            Answered::Reply(Answer::Confirmed { confirmed, .. }) => {
                if !confirmed {
                    return text_output(UNCHANGED.to_owned());
                }
                self.save(staged, &sections)
            }
            Answered::Reply(Answer::Declined { .. }) => text_output(DECLINED.to_owned()),
            Answered::NoAnswer => errored(ReviewerNotesError::ProgramDriven),
            Answered::Reply(_) => errored(ReviewerNotesError::AnswerMismatch),
        }
    }
}

/// The call's draft lines: every item names a topic by its serde name, a
/// layer and the text.
fn draft_lines(arguments: &Map<String, Value>) -> Result<Vec<DraftLine>, ReviewerNotesError> {
    let unreadable = |detail: &str| ReviewerNotesError::LinesUnreadable {
        detail: detail.to_owned(),
    };
    let lines = arguments
        .get("lines")
        .and_then(Value::as_array)
        .ok_or_else(|| unreadable("`lines` is not an array"))?;
    lines
        .iter()
        .enumerate()
        .map(|(at, line)| {
            let item = line
                .as_object()
                .ok_or_else(|| unreadable(&format!("`lines[{at}]` is not an object")))?;
            let topic = item
                .get("topic")
                .and_then(Value::as_str)
                .ok_or_else(|| unreadable(&format!("`lines[{at}].topic` is not a string")))?;
            let topic: Topic = serde_json::from_value(Value::String(topic.to_owned()))
                .map_err(|_| unreadable(&format!("`lines[{at}].topic` names no topic")))?;
            let layer = match item.get("layer").and_then(Value::as_str) {
                Some("global") => config::EntryLayer::Global,
                Some("project") => config::EntryLayer::Project,
                _ => return Err(unreadable(&format!("`lines[{at}].layer` is not a layer"))),
            };
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| unreadable(&format!("`lines[{at}].text` is not a string")))?;
            Ok(DraftLine {
                topic,
                layer,
                text: crate::reviewer_notes::clean_line(text),
            })
        })
        .collect()
}

/// One layer's staged save: its merged text and its read-only entries.
struct StagedLayer {
    text: String,
    entries: Vec<config::ReadOnlyFlags>,
}

/// One layer's own text with the draft applied. Per explicitly selected
/// topic — a topic with at least one sent line — struck is what the text
/// owns less what was sent and added is what was sent, merged for that
/// topic only; read-only lines never own a heading and parse as entries.
fn stage_layer(
    text: &str,
    lines: &[DraftLine],
    layer: config::EntryLayer,
) -> Result<StagedLayer, ReviewerNotesError> {
    let sent: Vec<&DraftLine> = lines.iter().filter(|line| line.layer == layer).collect();
    let mut entries = Vec::new();
    for line in sent
        .iter()
        .filter(|line| line.topic == Topic::ReadOnlyCommands)
    {
        match parse_read_only_entry(&line.text) {
            Some((command, flags)) => entries.push(config::ReadOnlyFlags { command, flags }),
            None => {
                return Err(ReviewerNotesError::Unparseable {
                    line: line.text.clone(),
                });
            }
        }
    }
    let mut struck = Vec::new();
    let mut added = Vec::new();
    for topic in Topic::ALL {
        if topic == Topic::ReadOnlyCommands {
            continue;
        }
        let sent: Vec<&str> = sent
            .iter()
            .filter(|line| line.topic == topic)
            .map(|line| line.text.as_str())
            .collect();
        if sent.is_empty() {
            continue;
        }
        let bullets = owned(text)
            .into_iter()
            .find(|(found, _)| *found == topic)
            .map(|(_, bullets)| bullets)
            .unwrap_or_default();
        for bullet in bullets {
            if !sent.iter().any(|line| line.trim() == bullet.as_str()) {
                struck.push((topic, bullet));
            }
        }
        for line in sent {
            added.push((topic, line.to_owned()));
        }
    }
    Ok(StagedLayer {
        text: merge(text, &struck, &added),
        entries,
    })
}

/// Both layers' staged read-only entries, global first.
fn staged_entries(
    staged: &[(config::EntryLayer, &config::NotesFile, StagedLayer)],
) -> (Vec<config::ReadOnlyFlags>, Vec<config::ReadOnlyFlags>) {
    let mut global = Vec::new();
    let mut project = Vec::new();
    for (layer, _, staged) in staged {
        match layer {
            config::EntryLayer::Global => global.extend(staged.entries.iter().cloned()),
            config::EntryLayer::Project => project.extend(staged.entries.iter().cloned()),
        }
    }
    (global, project)
}

/// One changed file's confirm section: its path header and its rows.
struct ConfirmSection {
    file: String,
    rows: Vec<String>,
}

/// One section per changed file: the path header plus that layer's diff
/// rows, plus the layer's entry additions rendered as `+` lines from the
/// staged preview. Identical entries never reach the preview, so unchanged
/// layers have no rows.
fn confirm_sections(
    staged: &[(config::EntryLayer, &config::NotesFile, StagedLayer)],
    preview: &[config::EntryPreview],
) -> Vec<ConfirmSection> {
    staged
        .iter()
        .map(|(layer, notes, staged)| {
            let mut rows = Vec::new();
            for row in diff(&notes.text, &staged.text) {
                rows.push(match row {
                    DiffRow::Context(line) => format!("  {line}"),
                    DiffRow::Removed(line) => format!("- {line}"),
                    DiffRow::Added(line) => format!("+ {line}"),
                });
            }
            for entry in preview.iter().filter(|entry| entry.layer == *layer) {
                rows.push(format!(
                    "+ {}",
                    render_read_only_entry(&entry.command, &entry.flags)
                ));
            }
            ConfirmSection {
                file: notes.file.display().to_string(),
                rows,
            }
        })
        .collect()
}

impl ReviewerNotesSave {
    /// Writes the staged save the person confirmed: both layers through
    /// the config writer, which re-validates under its locks, so a
    /// concurrent write refuses stale instead of clobbering.
    fn save(
        &self,
        staged: [(config::EntryLayer, &config::NotesFile, StagedLayer); 2],
        sections: &[ConfirmSection],
    ) -> Output {
        let write = |notes: &config::NotesFile, staged: &StagedLayer| config::NotesWrite {
            version: notes.version.clone(),
            text: staged.text.clone(),
            read_only: staged.entries.clone(),
        };
        let [global, project] = staged;
        match config::save_reviewer_notes(
            &self.home,
            &self.key,
            Some(write(global.1, &global.2)),
            Some(write(project.1, &project.2)),
        ) {
            Err(error) => errored(ReviewerNotesError::Config(error)),
            Ok(config::NotesSaved::Stale) => text_output(STALE.to_owned()),
            Ok(config::NotesSaved::Written(files)) => {
                let mut report = Vec::new();
                let mut changes = Vec::new();
                for saved in files {
                    let path = saved.file.display().to_string();
                    let (added, removed) = sections
                        .iter()
                        .find(|section| section.file == path)
                        .map(|section| {
                            section
                                .rows
                                .iter()
                                .fold((0u64, 0u64), |(added, removed), row| {
                                    if row.starts_with('+') {
                                        (added + 1, removed)
                                    } else if row.starts_with('-') {
                                        (added, removed + 1)
                                    } else {
                                        (added, removed)
                                    }
                                })
                        })
                        .unwrap_or((0, 0));
                    match saved.outcome {
                        config::FileOutcome::Saved => {
                            report.push(format!("{path}: {added} added, {removed} removed"));
                            changes.push(FileChange {
                                path,
                                added,
                                removed,
                            });
                        }
                        config::FileOutcome::SavedUnsynced(error) => {
                            return errored(ReviewerNotesError::Config(error));
                        }
                        config::FileOutcome::NotSaved(_) => {}
                    }
                }
                if report.is_empty() {
                    return text_output(UNCHANGED.to_owned());
                }
                let mut text = String::from("Saved the reviewer notes.\n");
                for line in report {
                    text.push_str(&line);
                    text.push('\n');
                }
                Output {
                    content: vec![ContentPart::Text { text }],
                    changes: Some(changes),
                    ..Output::default()
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "reviewer_notes_save_tests.rs"]
mod tests;
