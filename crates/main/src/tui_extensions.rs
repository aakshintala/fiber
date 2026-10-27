//! Terminal-home TUI loading and compatible-first slot selection (`docs/tui.md`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use contract::clock::Clock;
use extensions::{Span, TuiVm};
use serde_json::{Value, json};
use tui::ext::{Decision, FailureKind, Load, Renderer, SessionCtx, SessionVersion, SlotSelection};
use tui::{Failure, RawSpan, RendererInput as Input, Status};

pub(crate) struct Loader {
    sources: config::Sources,
    clock: Arc<dyn Clock>,
}

impl Loader {
    pub(crate) fn new(sources: config::Sources, clock: Arc<dyn Clock>) -> Self {
        Self { sources, clock }
    }
}

impl Load for Loader {
    fn load(&self) -> (Rc<dyn Renderer>, Vec<(String, String)>) {
        let mut composite = Composite::default();
        let mut notices = Vec::new();
        let config = match config::Config::load(self.sources.clone()) {
            Ok(config) => config,
            Err(error) => {
                return (
                    Rc::new(composite),
                    vec![(
                        serde_json::to_value(error.code())
                            .unwrap_or(Value::Null)
                            .as_str()
                            .unwrap_or("config_invalid")
                            .to_owned(),
                        error.to_string(),
                    )],
                );
            }
        };
        let root = config::extensions_dir(&self.sources.home);
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Rc::new(composite), notices);
            }
            Err(error) => {
                return (
                    Rc::new(composite),
                    vec![(
                        "extension_failed".into(),
                        format!("{}: {error}", root.display()),
                    )],
                );
            }
        };
        let mut dirs = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => dirs.push(entry.path()),
                Err(error) => notices.push((
                    "extension_failed".into(),
                    format!("{}: {error}", root.display()),
                )),
            }
        }
        dirs.sort();
        for dir in dirs {
            // No optional directory means no client half, regardless of manifest.
            if !dir.join("tui").exists() {
                continue;
            }
            match load_one(&dir, &config, &self.clock) {
                Ok(Some(candidate)) => {
                    for slot in candidate.vm.borrow().slots().keys() {
                        if let Some(winner) =
                            config.text(&format!("tui.slots.{}", json!(slot)), None)
                        {
                            composite.configured.insert(slot.clone(), winner);
                        }
                    }
                    composite
                        .candidates
                        .insert(candidate.manifest.name.clone(), candidate);
                }
                Ok(None) => {}
                Err(error) => notices.push((
                    "extension_failed".into(),
                    format!("{}: {error}", dir.display()),
                )),
            }
        }
        (Rc::new(composite), notices)
    }
}

fn load_one(
    dir: &Path,
    config: &config::Config,
    clock: &Arc<dyn Clock>,
) -> Result<Option<Candidate>, extensions::Error> {
    let manifest = config::read_manifest(dir)?;
    if config.flag(
        &format!("extensions.{}.enabled", json!(manifest.name)),
        None,
    ) == Some(false)
    {
        return Ok(None);
    }
    let vm = TuiVm::load_with_source(
        &manifest.name,
        || config::read_package_text(dir, "tui/init.lua", "tui/init.lua").map_err(Into::into),
        Arc::clone(clock),
    )?;
    Ok(Some(Candidate {
        manifest,
        vm: RefCell::new(vm),
        off: std::cell::Cell::new(false),
    }))
}

#[derive(Debug)]
struct Candidate {
    manifest: config::Manifest,
    vm: RefCell<TuiVm>,
    off: std::cell::Cell<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct VersionNoticeKey {
    extension: String,
    tui_version: String,
    half: SessionVersion,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ConflictNoticeKey {
    slot: String,
    contenders: Vec<String>,
    configured: Option<String>,
}

#[derive(Debug, Default)]
struct Composite {
    candidates: BTreeMap<String, Candidate>,
    configured: BTreeMap<String, String>,
    versions: RefCell<BTreeSet<VersionNoticeKey>>,
    conflicts: RefCell<BTreeSet<ConflictNoticeKey>>,
    pending: RefCell<Vec<(String, String)>>,
}

impl Candidate {
    fn half(&self, ctx: &SessionCtx) -> SessionVersion {
        match &ctx.halves {
            None => SessionVersion::Unseen,
            Some(halves) => halves
                .get(&self.manifest.name)
                .map_or(SessionVersion::NotLoaded, |v| {
                    SessionVersion::Version(v.clone())
                }),
        }
    }

    fn compatible(&self, ctx: &SessionCtx) -> bool {
        if self.off.get() {
            return false;
        }
        let Some(range) = self
            .manifest
            .tui
            .as_ref()
            .and_then(|tui| tui.session_versions.as_ref())
        else {
            return true;
        };
        let SessionVersion::Version(version) = self.half(ctx) else {
            return false;
        };
        // Installation validates bounds; session event versions remain untrusted.
        let Ok(version) = extensions::version_key(&version) else {
            return false;
        };
        extensions::version_key(&range.min).is_ok_and(|min| version >= min)
            && range.before.as_ref().is_none_or(|before| {
                extensions::version_key(before).is_ok_and(|before| version < before)
            })
    }
}

impl Renderer for Composite {
    fn name(&self) -> &str {
        "TUI extensions"
    }
    fn fills(&self, slot: &str) -> bool {
        self.candidates.values().any(|candidate| {
            !candidate.off.get() && candidate.vm.borrow().slots().contains_key(slot)
        })
    }
    fn fills_for(&self, ctx: &SessionCtx, slot: &str) -> SlotSelection {
        let mut contenders = Vec::new();
        for (name, candidate) in &self.candidates {
            if candidate.off.get() || !candidate.vm.borrow().slots().contains_key(slot) {
                continue;
            }
            if candidate.compatible(ctx) {
                contenders.push(name.clone());
                continue;
            }
            let half = candidate.half(ctx);
            if half == SessionVersion::Unseen {
                continue;
            }
            let key = VersionNoticeKey {
                extension: name.clone(),
                tui_version: candidate.manifest.version.clone(),
                half: half.clone(),
            };
            if self.versions.borrow_mut().insert(key) {
                let half = match half {
                    SessionVersion::NotLoaded => "not loaded".to_owned(),
                    SessionVersion::Version(v) => v,
                    SessionVersion::Unseen => continue,
                };
                self.pending.borrow_mut().push(("extension_incompatible".into(), format!("extension {name} TUI {} does not support session half {half}; the built-in is drawn", candidate.manifest.version)));
            }
        }
        let configured = self.configured.get(slot).cloned();
        if contenders.len() == 1 {
            return SlotSelection::Draw {
                extension: contenders.remove(0),
            };
        }
        if contenders.is_empty() {
            return SlotSelection::Absent;
        }
        if let Some(winner) = configured
            .as_ref()
            .filter(|winner| contenders.contains(winner))
        {
            return SlotSelection::Draw {
                extension: winner.clone(),
            };
        }
        let key = ConflictNoticeKey {
            slot: slot.to_owned(),
            contenders: contenders.clone(),
            configured: configured.clone(),
        };
        if self.conflicts.borrow_mut().insert(key) {
            let mut message = format!(
                "extensions {} replace {slot}; the built-in stays",
                contenders.join(", ")
            );
            if let Some(value) = configured {
                message.push_str(&format!(
                    ". `tui.slots.\"{slot}\"` names `{value}`, which is not one of them."
                ));
            }
            self.pending
                .borrow_mut()
                .push(("command_conflict".into(), message));
        }
        SlotSelection::Suppressed
    }
    fn take_pending(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.pending.borrow_mut())
    }
    fn half(&self, ctx: &SessionCtx, extension: &str) -> Option<String> {
        let candidate = self.candidates.get(extension)?;
        candidate.manifest.tui.as_ref()?.session_versions.as_ref()?;
        ctx.halves.as_ref()?.get(extension).cloned()
    }
    fn render(
        &self,
        ctx: &SessionCtx,
        slot: &str,
        input: &Input<'_>,
        width: u16,
        builtin: &dyn Fn(&Input<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> Decision {
        let SlotSelection::Draw { extension } = self.fills_for(ctx, slot) else {
            return Decision::NotForSession {
                extension: String::new(),
                tui_version: String::new(),
                half: SessionVersion::Unseen,
            };
        };
        let Some(candidate) = self.candidates.get(&extension) else {
            return Decision::NotForSession {
                extension,
                tui_version: String::new(),
                half: SessionVersion::Unseen,
            };
        };
        let value = input_value(input);
        let built = |_: &Value, width| {
            builtin(input, width)
                .into_iter()
                .map(|line| line.into_iter().map(to_vm).collect())
                .collect()
        };
        match candidate
            .vm
            .borrow_mut()
            .call(slot, &ctx.session_id.0, &value, width, &built)
        {
            Ok(lines) => Decision::Draw {
                extension,
                lines: lines
                    .into_iter()
                    .map(|line| line.into_iter().map(from_vm).collect())
                    .collect(),
            },
            Err(error) => {
                let kind = if matches!(
                    error,
                    extensions::Error::TuiTimeout | extensions::Error::TuiOff
                ) {
                    candidate.off.set(true);
                    FailureKind::ExtensionFailed
                } else {
                    FailureKind::HookFailed
                };
                Decision::Fail(Failure {
                    kind,
                    extension,
                    message: error.to_string(),
                })
            }
        }
    }
}

fn input_value(input: &Input<'_>) -> Value {
    match input {
        Input::LedgerRow(row) => ledger_value(row),
        Input::TurnCard(card) => turn_card_value(card),
        Input::Prompt(prompt) => {
            json!({"text": prompt.text, "images": images_value(&prompt.images), "flat": prompt.flat})
        }
        Input::Reply(reply) => json!({"text": reply.text, "flat": reply.flat}),
        Input::ThinkingLine(line) => {
            json!({"text": line.text, "started": line.started, "ended": line.ended, "open": line.open, "flat": line.flat})
        }
    }
}

fn ledger_value(row: &tui::ext::LedgerRow<'_>) -> Value {
    let status = match row.status {
        Status::Pending => "pending",
        Status::Running => "running",
        Status::Completed => "completed",
        Status::Failed => "failed",
        Status::Denied => "denied",
        Status::Cancelled => "cancelled",
        Status::Unknown => "unknown",
    };
    json!({"name": row.name, "arguments": row.arguments, "status": status, "exit": row.exit, "duration_ms": row.duration_ms, "lines": row.lines, "changes": row.changes, "error": row.error.map(|error| json!({"code": error.code, "message": error.message})), "output": row.output, "answer": row.answer, "flat": row.flat})
}

fn images_value(images: &[tui::ext::PromptImage<'_>]) -> Value {
    images
        .iter()
        .map(|image| json!({"path": image.path, "width": image.width, "height": image.height}))
        .collect()
}

fn turn_card_value(card: &tui::ext::TurnCard<'_>) -> Value {
    json!({
        "prompts": card.prompts.iter().map(|prompt| json!({"text": prompt.text, "images": images_value(&prompt.images)})).collect::<Vec<_>>(),
        "entries": card.entries.iter().map(entry_value).collect::<Vec<_>>(),
        "started": card.started,
        "calls": card.calls,
        "usage": &card.usage,
        "ended": card.ended.as_ref().map(|ended| json!({
            "outcome": ended.outcome,
            "ended_ts": ended.ended_ts,
            "error": ended.error.as_ref().map(|error| json!({
                "code": serde_json::to_value(&error.code).unwrap_or(Value::Null),
                "message": error.message,
            })).unwrap_or(Value::Null),
        })).unwrap_or(Value::Null),
        "running": card.running,
        "flat": card.flat,
    })
}

fn entry_value(entry: &tui::ext::TurnEntry<'_>) -> Value {
    match entry {
        tui::ext::TurnEntry::Reply { text } => json!({"kind": "reply", "text": text}),
        tui::ext::TurnEntry::Steer { text, images, ts } => {
            json!({"kind": "steer", "text": text, "images": images_value(images), "ts": ts})
        }
        tui::ext::TurnEntry::Group(group) => json!({
            "kind": "group",
            "first": group.first,
            "last": group.last,
            "running": group.running,
            "open": group.open,
            "cut": group.cut,
            "sections": group.sections.iter().map(|section| json!({
                "step": section.step,
                "thoughts": section.thoughts.iter().map(|thought| json!({"text": thought.text, "started": thought.started, "ended": thought.ended, "open": thought.open})).collect::<Vec<_>>(),
                "calls": section.calls.iter().map(call_value).collect::<Vec<_>>(),
                "failed": section.failed.iter().map(|failed| json!({"code": failed.code, "attempt": failed.attempt})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "streaming": group.streaming.iter().map(|streaming| json!({"message": streaming.message, "index": streaming.index, "name": streaming.name, "text": streaming.text})).collect::<Vec<_>>(),
        }),
        tui::ext::TurnEntry::Aside(aside) => aside_value(aside),
        tui::ext::TurnEntry::Band(band) => {
            let (kind, cancelled) = match band.state {
                tui::ext::TurnBandState::Writing => ("writing", Value::Null),
                tui::ext::TurnBandState::Done => ("done", Value::Null),
                tui::ext::TurnBandState::Unchanged { cancelled } => ("unchanged", json!(cancelled)),
            };
            json!({
                "kind": "band",
                "trigger": band.trigger,
                "ts": band.ts,
                "state": {"kind": kind, "cancelled": cancelled},
                "sizes": band.sizes.map(|sizes| json!({"before": sizes.before, "after": sizes.after})).unwrap_or(Value::Null),
            })
        }
        tui::ext::TurnEntry::Answers(answers) => match answers {
            tui::ext::TurnAnswers::Given { parts, note, ts } => json!({
                "kind": "answers",
                "outcome": "given",
                "parts": parts.iter().map(|part| json!({"header": part.header, "text": part.text, "skipped": part.skipped})).collect::<Vec<_>>(),
                "note": note,
                "ts": ts,
            }),
            tui::ext::TurnAnswers::Declined => json!({
                "kind": "answers",
                "outcome": "declined",
                "parts": Vec::<Value>::new(),
                "note": Value::Null,
                "ts": Value::Null,
            }),
        },
    }
}

fn call_value(call: &tui::ext::TurnCall<'_>) -> Value {
    let status = match call.status {
        Status::Pending => "pending",
        Status::Running => "running",
        Status::Completed => "completed",
        Status::Failed => "failed",
        Status::Denied => "denied",
        Status::Cancelled => "cancelled",
        Status::Unknown => "unknown",
    };
    json!({
        "name": call.name,
        "arguments": call.arguments,
        "status": status,
        "exit": call.exit,
        "duration_ms": call.duration_ms,
        "lines": call.lines,
        "changes": call.changes,
        "error": call.error.map(|error| json!({"code": error.code, "message": error.message})).unwrap_or(Value::Null),
        "output": call.output,
        "headline": call.headline,
        "content": call.content,
        "answer": call.answer,
        "images": images_value(&call.images),
        "open": call.open,
        "asking": call.asking,
    })
}

fn aside_value(aside: &tui::ext::TurnAside<'_>) -> Value {
    match aside {
        tui::ext::TurnAside::Line { text } => {
            json!({"kind": "aside", "aside": {"kind": "line", "text": text}})
        }
        tui::ext::TurnAside::Warning { message } => {
            json!({"kind": "aside", "aside": {"kind": "warning", "message": message}})
        }
        tui::ext::TurnAside::Orphans { jobs, open } => json!({
            "kind": "aside",
            "aside": {
                "kind": "orphans",
                "jobs": jobs.iter().map(|job| json!({"name": job.name, "message": job.message})).collect::<Vec<_>>(),
                "open": open,
            },
        }),
    }
}

fn to_vm(span: RawSpan) -> Span {
    Span {
        text: span.text,
        fg: span.fg,
        bg: span.bg,
        bold: span.bold,
        dim: span.dim,
        italic: span.italic,
        underline: span.underline,
        link: span.link,
        click: span.click,
    }
}
fn from_vm(span: Span) -> RawSpan {
    RawSpan {
        text: span.text,
        fg: span.fg,
        bg: span.bg,
        bold: span.bold,
        dim: span.dim,
        italic: span.italic,
        underline: span.underline,
        link: span.link,
        click: span.click,
    }
}

#[cfg(test)]
#[path = "tui_extensions_tests.rs"]
mod tests;
