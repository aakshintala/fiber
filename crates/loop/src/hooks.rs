//! The `after_tool` call site (`docs/extensions.md`, "The hook points" and
//! "When a hook fails"): a call that ran has returned, and the session's
//! hooks see its output before it is cut, written as an artifact or logged.
//! The loop treats the tool's name and arguments as opaque data, and an
//! extension's name reaches it only on the answer (`docs/architecture.md`,
//! "The call rules").

use std::sync::Arc;

use contract::events::{CallStatus, Event};
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks, StateLine};
use contract::shapes::{ContentPart, Process};
use contract::tool::{Bound, SpillFile, Spilled};
use contract::{ActionId, TurnId};
use log::{Log, cut_joined, cut_notice};
use serde_json::{Map, Value};

use crate::{Error, Loop};

/// A call that ran, as its completion is about to be built: the text parts
/// of its output, and what the hook is shown beside them.
pub(crate) struct Ran<'a> {
    /// The tool's name, as the model called it.
    pub(crate) tool: &'a str,
    /// The arguments it ran with.
    pub(crate) arguments: &'a Map<String, Value>,
    /// The status the completion carries.
    pub(crate) status: CallStatus,
    /// The output's text parts.
    pub(crate) text: Vec<ContentPart>,
    /// Its other parts, such as images, which no hook sees.
    pub(crate) images: Vec<ContentPart>,
    /// The tool's `details`.
    pub(crate) details: Option<Value>,
    /// How its process ended.
    pub(crate) process: Option<&'a Process>,
    /// The extensions that rewrote the arguments, when a `before_tool`
    /// hook did: the completion's text begins with the line naming them.
    pub(crate) rewritten: Option<&'a [String]>,
    /// Output the tool wrote to the call's artifact file as it ran, when
    /// it did (`docs/tools.md`, "Result and output"): the loop takes its
    /// text from the spill alone and ignores any text parts in `content`.
    pub(crate) spilled: Option<Spilled>,
}

/// What the completion carries of the output, once the hooks have answered
/// and the cap is applied.
pub(crate) struct Shaped {
    pub(crate) state: Vec<StateLine>,
    pub(crate) content: Vec<ContentPart>,
    pub(crate) details: Option<Value>,
    pub(crate) artifact: Option<String>,
    pub(crate) kept: Option<Vec<[u64; 2]>>,
    pub(crate) changed_by: Option<Vec<String>>,
    pub(crate) error: Option<contract::shapes::Failure>,
    /// True exactly when the completion carries none of the tool's own
    /// output: under a shutdown, a `Withheld` answer or a failed hook.
    pub(crate) withheld: bool,
}

impl Loop {
    pub(crate) fn drain_state(
        &mut self,
        hooks: &Arc<dyn Hooks>,
        turn: Option<&TurnId>,
    ) -> Result<(), Error> {
        for event in hooks.take_state_lines() {
            self.write(&event, turn, None)?;
        }
        Ok(())
    }

    pub(crate) fn write_carried(
        &mut self,
        state: Vec<StateLine>,
        turn: Option<&TurnId>,
    ) -> Result<(), Error> {
        if let Some(hooks) = self.hooks.clone() {
            for event in hooks.admit_state_lines(state) {
                match turn {
                    Some(turn) => self.append(&event, turn, None)?,
                    None => self.write(&event, None, None)?,
                }
            }
        }
        Ok(())
    }

    /// Runs the session's `after_tool` hooks on every call that ran
    /// (`docs/extensions.md`, "The hook points"). Without hooks the tool's
    /// output is the output.
    pub fn hooks(mut self, hooks: Arc<dyn Hooks>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Asks the hooks about `ran`, writes each notice they raised under
    /// `id`, and applies the answer and the size cap `bound`
    /// (`docs/tools.md`, "Bounded results"). What a hook replaced is never
    /// written: the cap and the artifact see only what it returned.
    pub(crate) fn shape(
        &mut self,
        ran: Ran<'_>,
        bound: Bound,
        id: &ActionId,
        turn: Option<&TurnId>,
    ) -> Result<Shaped, Error> {
        // Under a shutdown no hook runs, so nothing is left to redact the
        // output: the completion carries none of it (`docs/invocation.md`,
        // "Shutdown"). A spilled file is removed, so no raw output stays.
        if self.shutting_down() {
            if let Some(spilled) = ran.spilled {
                remove_spilled(&self.log, &spilled)?;
            }
            return Ok(Shaped {
                state: Vec::new(),
                content: Vec::new(),
                details: None,
                artifact: None,
                kept: None,
                changed_by: None,
                error: None,
                withheld: true,
            });
        }
        if ran.spilled.is_some() {
            return self.shape_spilled(ran, bound, id, turn);
        }
        let (parts, full) = joined(ran.text);
        let line = rewritten_line(ran.rewritten, ran.arguments);
        let Some(hooks) = self.hooks.clone() else {
            let (parts, full) = with_line(parts, full, &line);
            return Ok(self
                .capped(parts, full, ran.details, None, bound, id)?
                .with(ran.images));
        };
        let answer = {
            // The one actual `after_tool` dispatch on ordinary output:
            // the guard closes before the notices, so the cap and the
            // artifact work stay outside.
            let _dispatch = self
                .diag
                .timed_tool(contract::diag::Op::ToolHooks, ran.tool);
            hooks.after_tool(&AfterToolCall {
                tool: ran.tool,
                arguments: ran.arguments,
                status: ran.status,
                content: &full,
                details: ran.details.as_ref(),
                process: ran.process,
                spilled: None,

                not_regular: false,
                delivery: None,
            })
        };
        let state = hooks.take_change_state();
        self.drain_state(&hooks, turn)?;
        // Test-only boundary past the closed timer: later work the timer
        // must already have left out.
        #[cfg(test)]
        crate::tool_timing_tests::after_dispatch_exit(self.log.clock().as_ref());
        for notice in answer.notices {
            self.write(&Event::Notice(notice), turn, None)?;
        }
        let changed_by = (!answer.changed_by.is_empty()).then_some(answer.changed_by);
        let changed = matches!(&answer.outcome, AfterToolOutcome::Changed { .. });
        let mut shaped = match answer.outcome {
            AfterToolOutcome::Unchanged => {
                let (parts, full) = with_line(parts, full, &line);
                self.capped(parts, full, ran.details, None, bound, id)?
                    .with(ran.images)
            }
            AfterToolOutcome::Failed(failure) => Shaped::failed(failure),
            // Its only content is the line: images go too.
            AfterToolOutcome::Withheld { extension } => Shaped {
                state: Vec::new(),
                content: vec![ContentPart::Text {
                    text: withheld_line(&extension),
                }],
                details: None,
                artifact: None,
                kept: None,
                changed_by: None,
                error: None,
                withheld: true,
            },
            AfterToolOutcome::Changed {
                content,
                details,
                artifact,
            } => {
                let details = details.or_else(|| {
                    if content.is_some() {
                        without_matches(ran.details)
                    } else {
                        ran.details
                    }
                });
                let (parts, full) = match content {
                    Some(text) if text.is_empty() => (Some(Vec::new()), text),
                    Some(text) => (None, text),
                    None => (parts, full),
                };
                let (parts, full) = with_line(parts, full, &line);
                self.capped(parts, full, details, artifact, bound, id)?
                    .with(ran.images)
            }
        };
        shaped.changed_by = changed_by;
        if changed {
            shaped.state = state;
        }
        Ok(shaped)
    }

    /// The text parts whose joined text is `full`, cut to `bound`: `parts`,
    /// or `full` as the one part when `parts` is `None`. The artifact
    /// holds `artifact` when a hook returned it, written even when `full`
    /// fits, or else `full` when it was cut. A hook's artifact that cannot
    /// be written fails the turn, as a log write does.
    fn capped(
        &self,
        parts: Option<Vec<ContentPart>>,
        full: String,
        details: Option<Value>,
        artifact: Option<String>,
        bound: Bound,
        id: &ActionId,
    ) -> Result<Shaped, Error> {
        let name = format!("{}.txt", id.0);
        let cap = bound.start.saturating_add(bound.end);
        let (content, cut, kept) = if full.len() > cap {
            let (head, tail) = Log::cut_points(&full, bound);
            let ranges = kept_ranges(head as u64, tail as u64, full.len() as u64);
            let (text, artifact) = self.log.cut_output(&full, bound, &name);
            (vec![ContentPart::Text { text }], artifact, Some(ranges))
        } else {
            (
                parts.unwrap_or_else(|| vec![ContentPart::Text { text: full }]),
                None,
                None,
            )
        };
        let artifact = match artifact {
            // The hook's text replaces what the cut wrote under the same
            // name, so the cut's notice points at it.
            Some(artifact) => Some(self.log.write_artifact(&name, artifact.as_bytes())?.0),
            None => cut,
        };
        Ok(Shaped {
            state: Vec::new(),
            content,
            details,
            artifact,
            kept,
            changed_by: None,
            error: None,
            withheld: false,
        })
    }

    /// A call the tool already spilled to its artifact file: the loop
    /// writes no second artifact. The hook sees the cut output with the
    /// file's `path` and `size`, and its artifact text replaces the file
    /// (`docs/extensions.md`, "Hooks"). The output's own text parts are
    /// ignored; its other parts, such as images, are kept.
    fn shape_spilled(
        &mut self,
        ran: Ran<'_>,
        bound: Bound,
        id: &ActionId,
        turn: Option<&TurnId>,
    ) -> Result<Shaped, Error> {
        let Ran {
            tool,
            arguments,
            status,
            text: _,
            images,
            details,
            process,
            rewritten,
            spilled,
        } = ran;
        let Some(spilled) = spilled else {
            unreachable!("shape routes here only with a spill");
        };
        let line = rewritten_line(rewritten, arguments);
        let lined = |text: &str| match &line {
            Some(line) => format!("{line}\n{text}"),
            None => text.to_owned(),
        };
        let tail = spilled.head_bytes.saturating_add(spilled.removed);
        let len = tail.saturating_add(spilled.tail_bytes);
        let kept = (spilled.removed > 0).then(|| kept_ranges(spilled.head_bytes, tail, len));
        let persisted = match spilled.file {
            Ok(file) => file,
            Err(failed) => {
                return self.shape_unsaved(
                    tool,
                    arguments,
                    status,
                    details,
                    process,
                    images,
                    bound,
                    id,
                    turn,
                    &lined,
                    spilled.head,
                    spilled.tail,
                    spilled.removed,
                    kept,
                    failed.message,
                    failed.left,
                );
            }
        };
        let SpillFile { path, size } = persisted;
        let artifact = spill_artifact(&self.log, &path);
        let notice = cut_notice(spilled.removed, Ok(&path));
        let cut = if spilled.removed == 0 {
            format!("{}{}", spilled.head, spilled.tail)
        } else {
            cut_joined(&spilled.head, &notice, &spilled.tail)
        };
        let shown = lined(&cut);
        let mut state = Vec::new();
        let answer = match self.hooks.clone() {
            Some(hooks) => {
                // The one actual `after_tool` dispatch on spilled output.
                let answer = {
                    let _dispatch = self.diag.timed_tool(contract::diag::Op::ToolHooks, tool);
                    hooks.after_tool(&AfterToolCall {
                        tool,
                        arguments,
                        status,
                        content: &shown,
                        details: details.as_ref(),
                        process,
                        spilled: Some(&SpillFile {
                            path: path.clone(),
                            size,
                        }),

                        not_regular: false,
                        delivery: None,
                    })
                };
                state = hooks.take_change_state();
                self.drain_state(&hooks, turn)?;
                #[cfg(test)]
                crate::tool_timing_tests::after_dispatch_exit(self.log.clock().as_ref());
                answer
            }
            None => AfterToolAnswer {
                outcome: AfterToolOutcome::Unchanged,
                changed_by: Vec::new(),
                notices: Vec::new(),
            },
        };
        for notice in answer.notices {
            self.write(&Event::Notice(notice), turn, None)?;
        }
        let changed_by = (!answer.changed_by.is_empty()).then_some(answer.changed_by);
        let changed = matches!(&answer.outcome, AfterToolOutcome::Changed { .. });
        let mut shaped = match answer.outcome {
            AfterToolOutcome::Unchanged => Shaped {
                state: Vec::new(),
                content: vec![ContentPart::Text { text: shown }],
                details,
                artifact: Some(artifact),
                kept,
                changed_by: None,
                error: None,
                withheld: false,
            },
            AfterToolOutcome::Failed(failure) => {
                let shaped = Shaped::failed(failure);
                let message = crate::conversation::text(&shaped.content);
                replace_spilled(&self.log, &path, format!("{message}\n").as_bytes())?;
                return Ok(shaped);
            }
            // Its only content is the line: images go too. The file holds
            // the same line, as an `output_file` does.
            AfterToolOutcome::Withheld { extension } => {
                let withheld = withheld_line(&extension);
                replace_spilled(&self.log, &path, format!("{withheld}\n").as_bytes())?;
                Shaped {
                    state: Vec::new(),
                    content: vec![ContentPart::Text { text: withheld }],
                    details: None,
                    artifact: None,
                    kept: None,
                    changed_by: None,
                    error: None,
                    withheld: true,
                }
            }
            AfterToolOutcome::Changed {
                content,
                details: hook_details,
                artifact: hook_artifact,
            } => {
                let details = hook_details.or_else(|| {
                    if content.is_some() {
                        without_matches(details)
                    } else {
                        details
                    }
                });
                let separates = hook_artifact.is_some();
                let replaces = content.is_some();
                let full = match content {
                    Some(text) => lined(&text),
                    None => shown,
                };
                let cap = bound.start.saturating_add(bound.end);
                // Only a hook's replacement `content` is cut here, with a
                // notice naming the spilled file, and nothing is written
                // for it: the file stays the artifact. Without replacement
                // content the spilled outcome is already shaped, so its cut
                // text keeps its notice: cutting it again would report the
                // notice's bytes as the removed count instead of the
                // command's missing bytes.
                let kept = if replaces || separates { None } else { kept };
                let (content, kept) = if full.is_empty() {
                    (Vec::new(), None)
                } else if replaces && full.len() > cap {
                    let (text, ranges) = cut_hook_content(&full, bound, &path);
                    let kept = if separates { None } else { Some(ranges) };
                    (vec![ContentPart::Text { text }], kept)
                } else {
                    (vec![ContentPart::Text { text: full }], kept)
                };
                if let Some(hook_artifact) = hook_artifact {
                    replace_spilled(&self.log, &path, hook_artifact.as_bytes())?;
                }
                Shaped {
                    state: Vec::new(),
                    content,
                    details,
                    artifact: Some(artifact),
                    kept,
                    changed_by: None,
                    error: None,
                    withheld: false,
                }
            }
        };
        shaped.changed_by = changed_by;
        if changed {
            shaped.state = state;
        }
        Ok(shaped.with(images))
    }

    /// A spilled call whose file was not kept: the notice says the full
    /// output could not be saved, no artifact is named, and the hook sees
    /// no file. Its artifact text goes to `<action_id>.txt`, as without a
    /// spill. A partial file the tool could not remove is removed here,
    /// in every outcome; a removal that fails fails the turn.
    #[allow(
        clippy::too_many_arguments,
        reason = "the spill's pieces and the call's context are one shaping"
    )]
    fn shape_unsaved(
        &mut self,
        tool: &str,
        arguments: &Map<String, Value>,
        status: CallStatus,
        details: Option<Value>,
        process: Option<&Process>,
        images: Vec<ContentPart>,
        bound: Bound,
        id: &ActionId,
        turn: Option<&TurnId>,
        lined: &dyn Fn(&str) -> String,
        head: String,
        tail: String,
        removed: u64,
        kept: Option<Vec<[u64; 2]>>,
        message: String,
        left: Option<std::path::PathBuf>,
    ) -> Result<Shaped, Error> {
        if let Some(left) = left {
            remove_spilled_at(&self.log, &left)?;
        }
        let notice = cut_notice(removed, Err(&message));
        let shown = lined(&cut_joined(&head, &notice, &tail));
        let mut state = Vec::new();
        let answer = match self.hooks.clone() {
            Some(hooks) => {
                // The one actual `after_tool` dispatch on unsaved output.
                let answer = {
                    let _dispatch = self.diag.timed_tool(contract::diag::Op::ToolHooks, tool);
                    hooks.after_tool(&AfterToolCall {
                        tool,
                        arguments,
                        status,
                        content: &shown,
                        details: details.as_ref(),
                        process,
                        spilled: None,

                        not_regular: false,
                        delivery: None,
                    })
                };
                state = hooks.take_change_state();
                self.drain_state(&hooks, turn)?;
                #[cfg(test)]
                crate::tool_timing_tests::after_dispatch_exit(self.log.clock().as_ref());
                answer
            }
            None => AfterToolAnswer {
                outcome: AfterToolOutcome::Unchanged,
                changed_by: Vec::new(),
                notices: Vec::new(),
            },
        };
        for notice in answer.notices {
            self.write(&Event::Notice(notice), turn, None)?;
        }
        let changed_by = (!answer.changed_by.is_empty()).then_some(answer.changed_by);
        let changed = matches!(&answer.outcome, AfterToolOutcome::Changed { .. });
        let mut shaped = match answer.outcome {
            AfterToolOutcome::Unchanged => Shaped {
                state: Vec::new(),
                content: vec![ContentPart::Text { text: shown }],
                details,
                artifact: None,
                kept,
                changed_by: None,
                error: None,
                withheld: false,
            },
            AfterToolOutcome::Failed(failure) => return Ok(Shaped::failed(failure)),
            // Its only content is the line: images go too.
            AfterToolOutcome::Withheld { extension } => Shaped {
                state: Vec::new(),
                content: vec![ContentPart::Text {
                    text: withheld_line(&extension),
                }],
                details: None,
                artifact: None,
                kept: None,
                changed_by: None,
                error: None,
                withheld: true,
            },
            AfterToolOutcome::Changed {
                content,
                details: hook_details,
                artifact,
            } => {
                let details = hook_details.or_else(|| {
                    if content.is_some() {
                        without_matches(details)
                    } else {
                        details
                    }
                });
                match content {
                    // Replacement `content` is capped as without a spill:
                    // the hook's artifact text goes to `<action_id>.txt`.
                    Some(text) => {
                        let (parts, full) = if text.is_empty() {
                            (Some(Vec::new()), text)
                        } else {
                            (None, lined(&text))
                        };
                        self.capped(parts, full, details, artifact, bound, id)?
                    }
                    // Without replacement `content` the unsaved outcome
                    // stays as shaped: its notice names no artifact and
                    // nothing is saved for it, so the failure summary is
                    // never claimed as the full output. Only a hook's
                    // artifact text is written, to `<action_id>.txt` as
                    // without a spill.
                    None => {
                        let artifact = artifact
                            .map(|text| {
                                self.log
                                    .write_artifact(&format!("{}.txt", id.0), text.as_bytes())
                                    .map(|named| named.0)
                            })
                            .transpose()?;
                        Shaped {
                            state: Vec::new(),
                            content: vec![ContentPart::Text { text: shown }],
                            details,
                            artifact,
                            kept,
                            changed_by: None,
                            error: None,
                            withheld: false,
                        }
                    }
                }
            }
        };
        shaped.changed_by = changed_by;
        if changed {
            shaped.state = state;
        }
        Ok(shaped.with(images))
    }
}

/// The withheld line for a delivery whose `blocking` hook failed: the
/// delivery's only content, naming the extension that failed, with no
/// `changed_by` (`docs/extensions.md`, "When a hook fails").
pub(crate) fn withheld_line(extension: &str) -> String {
    format!("Output withheld: the `after_tool` hook of extension {extension} failed.")
}

/// The withheld line when a hook of the job's own extension could not
/// run, with why.
pub(crate) fn failed_line(failure: &contract::hook::HookFailure) -> String {
    format!(
        "Output withheld: the `after_tool` hook of extension {} could not run: {}.",
        failure.extension, failure.message
    )
}

/// The spilled file as the completion's artifact: its path relative to
/// the session directory (`docs/events.md`, "Conventions"), or absolute
/// when it is not under it.
fn spill_artifact(log: &Log, path: &std::path::Path) -> String {
    match path.strip_prefix(log.dir()) {
        Ok(relative) => relative.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

/// Replaces the spilled file at `path` with `bytes`: through `log`
/// when the file sits inside the session directory, else written to its
/// own path. A path inside the session directory but outside `artifacts/`
/// is refused by `log`, never written by the fallback, so no hook text
/// lands in `events.jsonl` or another session file. A failure fails the
/// turn, as a log write does.
fn replace_spilled(log: &Log, path: &std::path::Path, bytes: &[u8]) -> Result<(), Error> {
    if path.strip_prefix(log.dir()).is_ok() {
        log.replace_artifact_at(path, bytes)?;
    } else {
        std::fs::write(path, bytes).map_err(|source| log::Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

/// `content` a hook returned over the cap, cut with a notice naming the
/// spilled file at `path`. Nothing is written for it: the file stays the
/// artifact.
fn cut_hook_content(
    content: &str,
    bound: Bound,
    path: &std::path::Path,
) -> (String, Vec<[u64; 2]>) {
    let (head, tail) = Log::cut_points(content, bound);
    let removed = u64::try_from(tail.saturating_sub(head)).unwrap_or(u64::MAX);
    let text = cut_joined(
        content.get(..head).unwrap_or_default(),
        &cut_notice(removed, Ok(path)),
        content.get(tail..).unwrap_or_default(),
    );
    (
        text,
        kept_ranges(head as u64, tail as u64, content.len() as u64),
    )
}

fn without_matches(mut details: Option<Value>) -> Option<Value> {
    if let Some(Value::Object(object)) = details.as_mut() {
        object.remove("matches");
    }
    details
}

fn kept_ranges(head: u64, tail: u64, len: u64) -> Vec<[u64; 2]> {
    let mut ranges = Vec::new();
    if head > 0 {
        ranges.push([0, head]);
    }
    if len > tail {
        ranges.push([tail, len]);
    }
    ranges
}

/// Removes a spilled file the loop must not keep: the kept file, or a
/// partial file the tool could not remove. A failed removal fails the turn.
fn remove_spilled(log: &Log, spilled: &Spilled) -> Result<(), Error> {
    match &spilled.file {
        Ok(file) => remove_spilled_at(log, &file.path)?,
        Err(failed) => {
            if let Some(left) = &failed.left {
                remove_spilled_at(log, left)?;
            }
        }
    }
    Ok(())
}

/// Removes the spilled file at `path`: through `log` when it sits inside
/// the session directory, by the fallback otherwise. A path inside the
/// session directory but outside `artifacts/` is refused by `log`, never
/// removed by the fallback.
fn remove_spilled_at(log: &Log, path: &std::path::Path) -> Result<(), Error> {
    if path.strip_prefix(log.dir()).is_ok() {
        log.remove_artifact_at(path)?;
    } else {
        std::fs::remove_file(path).map_err(|source| log::Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

/// The joined text of `parts`, and the parts when they are not that text
/// as one part. A lone text part is moved into the joined text, so a large
/// result is never copied; several are joined with `\n`.
fn joined(parts: Vec<ContentPart>) -> (Option<Vec<ContentPart>>, String) {
    let parts = match <[ContentPart; 1]>::try_from(parts) {
        Ok([ContentPart::Text { text }]) => return (None, text),
        Ok(one) => Vec::from(one),
        Err(parts) => parts,
    };
    let full = crate::conversation::text(&parts);
    (Some(parts), full)
}

impl Shaped {
    fn failed(failure: contract::hook::HookFailure) -> Self {
        let message = failed_line(&failure);
        Self {
            state: Vec::new(),
            content: vec![ContentPart::Text {
                text: message.clone(),
            }],
            details: None,
            artifact: None,
            kept: None,
            changed_by: None,
            error: Some(contract::shapes::Failure {
                code: contract::ErrorCode::HookFailed,
                message,
                provider: None,
                retry_after_ms: None,
            }),
            withheld: true,
        }
    }

    /// The output's other parts, after its text.
    fn with(mut self, images: Vec<ContentPart>) -> Self {
        self.content.extend(images);
        self
    }
}

/// The line heading the completion of a call a `before_tool` hook rewrote:
/// who rewrote it and the arguments that ran. `None` when the call ran as
/// asked.
fn rewritten_line(rewritten: Option<&[String]>, arguments: &Map<String, Value>) -> Option<String> {
    let rewritten = rewritten?;
    let names = rewritten.join(", ");
    let arguments = serde_json::to_string(arguments).unwrap_or_default();
    if rewritten.len() == 1 {
        Some(format!(
            "Extension {names} rewrote this call's arguments; it ran with {arguments}."
        ))
    } else {
        Some(format!(
            "Extensions {names} rewrote this call's arguments; it ran with {arguments}."
        ))
    }
}

/// The text parts whose joined text is `full` headed by `line`: one part,
/// so the call's text parts are joined into one part under it. `None` is
/// no line: the parts and the text as they were.
fn with_line(
    parts: Option<Vec<ContentPart>>,
    full: String,
    line: &Option<String>,
) -> (Option<Vec<ContentPart>>, String) {
    let Some(line) = line else {
        return (parts, full);
    };
    (None, format!("{line}\n{full}"))
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
