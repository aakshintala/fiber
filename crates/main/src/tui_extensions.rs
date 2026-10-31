//! Terminal-home TUI loading and compatible-first slot selection (`docs/tui.md`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use contract::clock::Clock;
use extensions::{Span, TuiVm};
use serde_json::{Value, json};
use tui::ext::{Decision, FailureKind, Load, Renderer, SessionCtx, SessionVersion, SlotSelection};
use tui::{CallState, Failure, RawSpan, RendererInput as Input};

pub(crate) struct Loader {
    sources: config::Sources,
    clock: Arc<dyn Clock>,
}

impl Loader {
    pub(crate) fn new(sources: config::Sources, clock: Arc<dyn Clock>) -> Self {
        Self { sources, clock }
    }
}

/// Moves one loaded extension's key bindings into the composite: each
/// malformed registration becomes one `extension_failed` notice and is
/// skipped, while a person-owned binding stays dormant with its notice.
fn register_keys(
    composite: &mut Composite,
    candidate: &Candidate,
    person_owned: &BTreeSet<String>,
    notices: &mut Vec<(String, String)>,
) {
    let name = &candidate.manifest.name;
    let vm = candidate.vm.borrow();
    for rejection in vm.key_rejections() {
        notices.push((
            "extension_failed".into(),
            format!(
                "extension {name}: {}; loading skips that registration.",
                rejection.reason
            ),
        ));
    }
    let mut seen = BTreeSet::new();
    for binding in vm.key_bindings() {
        let raw = binding.raw;
        let Some(canonical) = tui::canonical_key(&raw) else {
            notices.push((
                "extension_failed".into(),
                format!(
                    "extension {name} binds {raw:?}, which is not a key name; loading skips that registration."
                ),
            ));
            continue;
        };
        if canonical == "ctrl+c" {
            notices.push((
                "extension_failed".into(),
                format!(
                    "extension {name} binds ctrl+c, which always clears, then quits; loading skips that registration."
                ),
            ));
            continue;
        }
        let action = match &binding.form {
            extensions::KeyForm::Run { .. } => None,
            extensions::KeyForm::Action { id, variant } => {
                if let Some(reason) = tui::check_action(id, variant.as_deref()) {
                    notices.push((
                        "extension_failed".into(),
                        format!(
                            "extension {name} binds {canonical}: {reason}; loading skips that registration."
                        ),
                    ));
                    continue;
                }
                Some((id.clone(), variant.clone()))
            }
        };
        if !seen.insert(canonical.clone()) {
            notices.push((
                "extension_failed".into(),
                format!(
                    "extension {name} binds {canonical} twice; loading keeps the first registration."
                ),
            ));
            continue;
        }
        if person_owned.contains(&canonical) {
            notices.push((
                "extension_failed".into(),
                format!(
                    "extension {name} binds {canonical}, which is bound in keys; the person keeps it."
                ),
            ));
            composite.person_seen.borrow_mut().insert(KeyNoticeKey {
                slot: format!("key:{canonical}"),
                extension: name.clone(),
            });
        }
        composite
            .keys
            .entry(format!("key:{canonical}"))
            .or_default()
            .push(KeyContender {
                extension: name.clone(),
                raw,
                action,
            });
    }
}

/// Reports an incompatible extension once per extension, TUI version and
/// half, with the same text the drawable path gives.
fn notice_incompatible(
    composite: &Composite,
    extension: &str,
    tui_version: &str,
    half: SessionVersion,
) {
    if half == SessionVersion::Unseen {
        return;
    }
    let key = VersionNoticeKey {
        extension: extension.to_owned(),
        tui_version: tui_version.to_owned(),
        half: half.clone(),
    };
    if composite.versions.borrow_mut().insert(key) {
        let half = match half {
            SessionVersion::NotLoaded => "not loaded".to_owned(),
            SessionVersion::Version(version) => version,
            SessionVersion::Unseen => return,
        };
        composite.pending.borrow_mut().push((
            "extension_incompatible".into(),
            format!("extension {extension} TUI {tui_version} does not support session half {half}; the built-in is drawn"),
        ));
    }
}

/// The compatible contenders for a canonical key, in load order: a
/// switched-off or incompatible extension contends for nothing, with the
/// same version notice the drawable path gives.
fn key_contenders(composite: &Composite, ctx: &SessionCtx, canonical: &str) -> Vec<KeyContender> {
    let slot = format!("key:{canonical}");
    let Some(contenders) = composite.keys.get(&slot) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for contender in contenders {
        let Some(candidate) = composite.candidates.get(&contender.extension) else {
            continue;
        };
        if candidate.off.get() {
            continue;
        }
        if candidate.compatible(ctx) {
            out.push(contender.clone());
            continue;
        }
        notice_incompatible(
            composite,
            &contender.extension,
            &candidate.manifest.version,
            candidate.half(ctx),
        );
    }
    out
}

/// The winning contender for a canonical key: the configured winner when
/// it contends, else the lone contender. A configured setting that names
/// no contender selects no winner, with no further notice: the load
/// already reported the full setting.
fn key_winner(composite: &Composite, ctx: &SessionCtx, canonical: &str) -> Option<KeyContender> {
    let contenders = key_contenders(composite, ctx, canonical);
    if contenders.is_empty() {
        return None;
    }
    let slot = format!("key:{canonical}");
    if let Some(winner) = composite.configured.get(&slot) {
        return contenders
            .into_iter()
            .find(|contender| &contender.extension == winner);
    }
    if contenders.len() == 1 {
        return contenders.into_iter().next();
    }
    let names: Vec<String> = contenders
        .iter()
        .map(|contender| contender.extension.clone())
        .collect();
    let key = ConflictNoticeKey {
        slot: slot.clone(),
        contenders: names.clone(),
        configured: None,
    };
    if composite.conflicts.borrow_mut().insert(key) {
        composite.pending.borrow_mut().push((
            "command_conflict".into(),
            format!(
                "extensions {} replace {slot}; the built-in stays",
                names.join(", ")
            ),
        ));
    }
    None
}

/// Reports every contender holding `canonical` as person-owned, once per
/// key and extension until `/reload`: the binding stays dormant for the
/// stroke.
fn notice_person(composite: &Composite, canonical: &str) {
    let slot = format!("key:{canonical}");
    let Some(contenders) = composite.keys.get(&slot) else {
        return;
    };
    for contender in contenders {
        let key = KeyNoticeKey {
            slot: slot.clone(),
            extension: contender.extension.clone(),
        };
        if composite.person_seen.borrow_mut().insert(key) {
            composite.pending.borrow_mut().push((
                "extension_failed".into(),
                format!(
                    "extension {} binds {canonical}, which is bound in keys; the person keeps it.",
                    contender.extension
                ),
            ));
        }
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
        // The merged `keys` object as written, empty when no file sets it:
        // the same read as the launch's user keys, so a key the person
        // still sets after clash reversion stays theirs in every context.
        let user: serde_json::Map<String, serde_json::Value> = config
            .get("keys", None)
            .and_then(|(value, _)| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let person_owned = tui::person_bound_keys(&user);
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
                    register_keys(&mut composite, &candidate, &person_owned, &mut notices);
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
        // Key-slot winners name an extension the same way drawable slots
        // do, with the suffix canonicalized by the key parser. Neither an
        // unparseable suffix nor a parseable setting naming no contender
        // can be reported through `fills_for`, so each selects no winner
        // with one `command_conflict` notice naming the full setting.
        if let Some((value, _)) = config.get("tui.slots", None)
            && let Ok(slots) =
                serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(value)
        {
            for (key, setting) in &slots {
                let Some(suffix) = key.strip_prefix("key:") else {
                    continue;
                };
                let name = format!("tui.slots.\"{key}\"");
                let Some(canonical) = tui::canonical_key(suffix) else {
                    notices.push((
                        "command_conflict".into(),
                        format!(
                            "Invalid key binding setting: {name}; the key does not parse; no extension wins."
                        ),
                    ));
                    continue;
                };
                let Some(winner) = setting.as_str() else {
                    continue;
                };
                let slot = format!("key:{canonical}");
                let contends = composite.keys.get(&slot).is_some_and(|contenders| {
                    contenders
                        .iter()
                        .any(|contender| contender.extension == winner)
                });
                composite.configured.insert(slot, winner.to_owned());
                if !contends {
                    notices.push((
                        "command_conflict".into(),
                        format!(
                            "Invalid key binding setting: {name}; no registered contender matches the selected extension; no extension wins."
                        ),
                    ));
                }
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
    // The slots table reader walks `animate` at registration: a whole
    // millisecond count above 0, clamped to the tick floor. Absent
    // `animate` omits the slot; clamping at or above the floor is a
    // no-op, and the seam clamps defensively again at its dues and
    // asks (`docs/tui.md`, "Animation").
    let mut anim = BTreeMap::new();
    for slot in vm.slots().keys() {
        if let Some(raw) = vm.animate(slot) {
            anim.insert(slot.clone(), tui::ext::anim::effective_interval(raw));
        }
    }
    Ok(Some(Candidate {
        manifest,
        vm: RefCell::new(vm),
        off: std::cell::Cell::new(false),
        anim,
    }))
}

#[derive(Debug)]
struct Candidate {
    manifest: config::Manifest,
    vm: RefCell<TuiVm>,
    off: std::cell::Cell<bool>,
    /// Each animated slot's clamped interval, by slot: absent `animate`
    /// omits the slot.
    anim: BTreeMap<String, Duration>,
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

/// One extension's contender for a canonical whole-screen key: the
/// extension, its raw spelling for the VM call, and, for an action
/// binding, its action and variant.
#[derive(Debug, Clone)]
struct KeyContender {
    extension: String,
    raw: String,
    action: Option<(String, Option<String>)>,
}

/// A key slot and extension pair already noticed: hook and person notices
/// come once per extension and key slot until `/reload`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct KeyNoticeKey {
    slot: String,
    extension: String,
}

#[derive(Debug, Default)]
struct Composite {
    candidates: BTreeMap<String, Candidate>,
    configured: BTreeMap<String, String>,
    versions: RefCell<BTreeSet<VersionNoticeKey>>,
    conflicts: RefCell<BTreeSet<ConflictNoticeKey>>,
    pending: RefCell<Vec<(String, String)>>,
    /// Whole-screen key contenders by canonical slot name
    /// (`key:<canonical>`), in load order: a dormant person-owned binding
    /// contends on, reactivated at once when the person binding lifts.
    keys: BTreeMap<String, Vec<KeyContender>>,
    /// Person-owned key and extension pairs already noticed: load seeds
    /// the pairs it reports, so load and runtime share the once-until-
    /// reload rule.
    person_seen: RefCell<BTreeSet<KeyNoticeKey>>,
    /// Hook failures already reported per key slot and extension.
    key_hooks: RefCell<BTreeSet<KeyNoticeKey>>,
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
        if let Some(canonical) = slot.strip_prefix("key:") {
            return match key_winner(self, ctx, canonical) {
                Some(winner) => SlotSelection::Draw {
                    extension: winner.extension,
                },
                None if key_contenders(self, ctx, canonical).is_empty() => SlotSelection::Absent,
                None => SlotSelection::Suppressed,
            };
        }
        let mut contenders = Vec::new();
        for (name, candidate) in &self.candidates {
            if candidate.off.get() || !candidate.vm.borrow().slots().contains_key(slot) {
                continue;
            }
            if candidate.compatible(ctx) {
                contenders.push(name.clone());
                continue;
            }
            notice_incompatible(self, name, &candidate.manifest.version, candidate.half(ctx));
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
    fn animate_interval(&self, ctx: &SessionCtx, slot: &str) -> Option<Duration> {
        // The winner's interval, exactly as `render` draws it: a static
        // winner answers none, and an absent slot selects nothing.
        let SlotSelection::Draw { extension } = self.fills_for(ctx, slot) else {
            return None;
        };
        let candidate = self.candidates.get(&extension)?;
        candidate.anim.get(slot).copied()
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
        match candidate.vm.borrow_mut().call(
            slot,
            ctx.session_id.as_ref().map(|id| id.0.as_str()),
            &value,
            width,
            &built,
        ) {
            Ok(lines) => Decision::Draw {
                extension,
                lines: lines
                    .into_iter()
                    .map(|line| line.into_iter().map(from_vm).collect())
                    .collect(),
            },
            Err(error) => Decision::Fail(Self::failure(candidate, extension, error)),
        }
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "the frame and its wall time travel with the draw context"
    )]
    fn render_animated(
        &self,
        ctx: &SessionCtx,
        slot: &str,
        input: &Input<'_>,
        width: u16,
        frame: u64,
        now: u64,
        builtin: &dyn Fn(&Input<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> (Decision, bool) {
        // The same selection as `render`: the frame and its wall time
        // count the animation into the input object, and failures map
        // exactly as a still call's do, stopping on every failure.
        let SlotSelection::Draw { extension } = self.fills_for(ctx, slot) else {
            return (
                Decision::NotForSession {
                    extension: String::new(),
                    tui_version: String::new(),
                    half: SessionVersion::Unseen,
                },
                true,
            );
        };
        let Some(candidate) = self.candidates.get(&extension) else {
            return (
                Decision::NotForSession {
                    extension,
                    tui_version: String::new(),
                    half: SessionVersion::Unseen,
                },
                true,
            );
        };
        let value = input_value(input);
        let built = |_: &Value, width| {
            builtin(input, width)
                .into_iter()
                .map(|line| line.into_iter().map(to_vm).collect())
                .collect()
        };
        match candidate.vm.borrow_mut().call_animated(
            slot,
            ctx.session_id.as_ref().map(|id| id.0.as_str()),
            &value,
            width,
            frame,
            now,
            &built,
        ) {
            Ok(out) => (
                Decision::Draw {
                    extension,
                    lines: out
                        .lines
                        .into_iter()
                        .map(|line| line.into_iter().map(from_vm).collect())
                        .collect(),
                },
                out.continues,
            ),
            Err(error) => (
                Decision::Fail(Self::failure(candidate, extension, error)),
                false,
            ),
        }
    }
    fn key_action(
        &self,
        ctx: &SessionCtx,
        key: &str,
        person_bound: bool,
    ) -> Option<(String, Option<String>)> {
        // With no session on screen neither kind of binding acts.
        ctx.session_id.as_ref()?;
        if person_bound {
            notice_person(self, key);
            return None;
        }
        key_winner(self, ctx, key)?.action
    }
    fn run_key(&self, ctx: &SessionCtx, key: &str, person_bound: bool) -> tui::ext::KeyOutcome {
        let Some(session_id) = ctx.session_id.as_ref() else {
            return tui::ext::KeyOutcome::NotHandled;
        };
        if person_bound {
            notice_person(self, key);
            return tui::ext::KeyOutcome::NotHandled;
        }
        let Some(winner) = key_winner(self, ctx, key) else {
            return tui::ext::KeyOutcome::NotHandled;
        };
        let Some(candidate) = self.candidates.get(&winner.extension) else {
            return tui::ext::KeyOutcome::NotHandled;
        };
        let slot = format!("key:{key}");
        match candidate
            .vm
            .borrow_mut()
            .run_key(&winner.raw, session_id.0.as_str())
        {
            Ok(true) => tui::ext::KeyOutcome::Handled,
            Ok(false) => tui::ext::KeyOutcome::NotHandled,
            Err(extensions::Error::TuiTimeout) => {
                candidate.off.set(true);
                self.pending.borrow_mut().push((
                    "extension_failed".into(),
                    format!(
                        "extension {} passed its key timeout for `{slot}`; it is switched off until `/reload`",
                        winner.extension
                    ),
                ));
                tui::ext::KeyOutcome::NotHandled
            }
            Err(extensions::Error::TuiOff) => tui::ext::KeyOutcome::NotHandled,
            Err(error) => {
                let seen = KeyNoticeKey {
                    slot: slot.clone(),
                    extension: winner.extension.clone(),
                };
                if self.key_hooks.borrow_mut().insert(seen) {
                    self.pending.borrow_mut().push((
                        "hook_failed".into(),
                        format!(
                            "extension {}'s key binding for `{slot}` failed: {error}",
                            winner.extension
                        ),
                    ));
                }
                tui::ext::KeyOutcome::NotHandled
            }
        }
    }
}

impl Composite {
    /// Maps a callback failure exactly as a still call does: a timeout
    /// switches the extension off until `/reload`.
    fn failure(candidate: &Candidate, extension: String, error: extensions::Error) -> Failure {
        let kind = if matches!(
            error,
            extensions::Error::TuiTimeout | extensions::Error::TuiOff
        ) {
            candidate.off.set(true);
            FailureKind::ExtensionFailed
        } else {
            FailureKind::HookFailed
        };
        Failure {
            kind,
            extension,
            message: error.to_string(),
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
        Input::GroupLine(line) => json!({
            "running": line.running,
            "calls": line.calls.iter().map(call_value).collect::<Vec<_>>(),
            "streaming": line.streaming.iter().map(streaming_value).collect::<Vec<_>>(),
            "thoughts": line.thoughts.iter().map(thought_value).collect::<Vec<_>>(),
            "failed": line.failed.iter().map(failed_value).collect::<Vec<_>>(),
            "first": line.first,
            "last": line.last,
            "open": line.open,
            "flat": line.flat,
        }),
        Input::Steer(steer) => {
            json!({"text": steer.text, "images": images_value(&steer.images), "ts": steer.ts, "flat": steer.flat})
        }
        Input::Answers(answers) => match &answers.answers {
            tui::ext::TurnAnswers::Given { parts, note, ts } => json!({
                "outcome": "given",
                "parts": parts.iter().map(answer_part_value).collect::<Vec<_>>(),
                "note": note,
                "ts": ts,
                "flat": answers.flat,
            }),
            tui::ext::TurnAnswers::Declined => json!({
                "outcome": "declined",
                "parts": Vec::<Value>::new(),
                "note": Value::Null,
                "ts": Value::Null,
                "flat": answers.flat,
            }),
        },
        Input::HandoffBand(band) => {
            json!({
                "trigger": band.trigger,
                "ts": band.ts,
                "state": band_state_value(&band.state),
                "flat": band.flat,
            })
        }
        Input::ErrorLine(line) => json!({
            "code": code_value(&line.code),
            "message": line.message,
            "provider": line.provider.map(|provider| json!({"name": provider.name, "status": provider.status, "message": provider.message})).unwrap_or(Value::Null),
            "flat": line.flat,
        }),
        Input::Notice(notice) => {
            json!({"code": notice.code, "text": notice.text, "flat": notice.flat})
        }
        Input::WorkingLine(line) => working_line_value(line),
        Input::SteeringQueue(queue) => steering_queue_value(queue),
        Input::StatusRows(rows) => status_rows_value(rows),
        Input::InputStatus(status) => input_status_value(status),
        Input::Card(card) => card_value(card),
    }
}

/// Inserts `value` under `key` when it holds one: optional input keys
/// are absent when they do not apply, never null.
fn insert_present(table: &mut serde_json::Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        table.insert(key.to_owned(), value);
    }
}

fn working_line_value(line: &tui::ext::WorkingLine) -> Value {
    let mut out = serde_json::Map::new();
    insert_present(
        &mut out,
        "started",
        line.started.map(|started| json!(started)),
    );
    insert_present(
        &mut out,
        "retry",
        line.retry.as_ref().map(|retry| {
            json!({
                "code": code_value(&retry.code),
                "attempt": retry.attempt,
                "last_attempt": retry.last_attempt,
                "delay_ms": retry.delay_ms,
                "ts": retry.ts,
            })
        }),
    );
    out.insert("flat".to_owned(), json!(line.flat));
    Value::Object(out)
}

fn steering_queue_value(queue: &tui::ext::SteeringQueue<'_>) -> Value {
    json!({
        "messages": queue.messages.iter().map(|message| json!({
            "text": message.text,
            "images": images_value(&message.images),
            "droppable": message.droppable,
            "selectable": message.selectable,
            "selected": message.selected,
        })).collect::<Vec<_>>(),
        "flat": queue.flat,
    })
}

/// The session card's spend: the status spend whole, or the attached
/// spend's cost alone when the session reports none.
fn spend_value(spend: &tui::ext::Spend<'_>) -> Value {
    match spend {
        tui::ext::Spend::Status(status) => {
            let mut out = serde_json::Map::new();
            insert_present(&mut out, "cost", status.cost.map(|cost| json!(cost)));
            out.insert(
                "tokens".to_owned(),
                json!({
                    "input": status.tokens.input,
                    "output": status.tokens.output,
                    "cache_read": status.tokens.cache_read,
                    "cache_write": status.tokens.cache_write,
                }),
            );
            out.insert(
                "subscription_cost".to_owned(),
                json!(status.subscription_cost),
            );
            Value::Object(out)
        }
        tui::ext::Spend::Attached { cost } => json!({ "cost": cost }),
    }
}

fn quota_value(quota: &tui::ext::Quota<'_>) -> Value {
    json!({
        "quota": quota.rows,
        "notice_at": quota.notice_at,
    })
}

fn branch_value(branch: &tui::ext::BranchName<'_>) -> Value {
    match branch {
        tui::ext::BranchName::Named(name) => json!(name),
        tui::ext::BranchName::Detached => json!("detached"),
    }
}

fn session_value(card: &tui::ext::SessionCard<'_>) -> Value {
    let mut out = serde_json::Map::new();
    insert_present(
        &mut out,
        "workspace",
        card.workspace.map(|workspace| json!(workspace)),
    );
    insert_present(&mut out, "branch", card.branch.as_ref().map(branch_value));
    insert_present(&mut out, "model", card.model.map(|model| json!(model)));
    insert_present(
        &mut out,
        "thinking",
        card.thinking.map(|thinking| json!(thinking)),
    );
    out.insert("thinking_now".to_owned(), json!(card.thinking_now));
    insert_present(
        &mut out,
        "context",
        card.context.map(|context| {
            let mut filled = serde_json::Map::new();
            filled.insert("tokens".to_owned(), json!(context.tokens));
            filled.insert("window".to_owned(), json!(context.window));
            insert_present(
                &mut filled,
                "trigger_at",
                context.trigger_at.map(|at| json!(at)),
            );
            Value::Object(filled)
        }),
    );
    insert_present(&mut out, "spend", card.spend.as_ref().map(spend_value));
    insert_present(&mut out, "budget", card.budget.map(|budget| json!(budget)));
    insert_present(
        &mut out,
        "cache_expires",
        card.cache_expires.map(|expiry| json!(expiry)),
    );
    insert_present(&mut out, "speed", card.speed.map(|speed| json!(speed)));
    out.insert("turns".to_owned(), json!(card.turns));
    out.insert(
        "tools_down".to_owned(),
        card.tools_down.iter().cloned().collect::<Vec<_>>().into(),
    );
    insert_present(
        &mut out,
        "waiting",
        card.waiting.map(|waiting| json!(waiting)),
    );
    out.insert("flat".to_owned(), json!(card.flat));
    Value::Object(out)
}

fn changed_files_value(card: &tui::ext::ChangedFiles<'_>) -> Value {
    json!({
        "files": card.files.iter().map(|file| json!({
            "path": file.path,
            "added": file.added,
            "removed": file.removed,
        })).collect::<Vec<_>>(),
        "flat": card.flat,
    })
}

fn outcome_value(outcome: contract::events::Outcome) -> Value {
    serde_json::to_value(outcome).unwrap_or(Value::Null)
}

fn delegates_value(card: &tui::ext::DelegatesCard<'_>) -> Value {
    json!({
        "delegates": card.delegates.iter().map(|delegate| {
            let mut out = serde_json::Map::new();
            out.insert("job_id".to_owned(), json!(delegate.job_id));
            out.insert("description".to_owned(), json!(delegate.description));
            out.insert("harness".to_owned(), json!(delegate.harness));
            out.insert("model".to_owned(), json!(delegate.model));
            out.insert("calls".to_owned(), json!(delegate.calls));
            insert_present(&mut out, "step", delegate.step.map(|step| json!(step)));
            insert_present(&mut out, "started", delegate.started.map(|started| json!(started)));
            insert_present(
                &mut out,
                "outcome",
                delegate.outcome.map(outcome_value),
            );
            insert_present(&mut out, "ended", delegate.ended.map(|ended| json!(ended)));
            Value::Object(out)
        }).collect::<Vec<_>>(),
        "flat": card.flat,
    })
}

fn jobs_value(card: &tui::ext::JobsCard<'_>) -> Value {
    json!({
        "jobs": card.jobs.iter().map(|job| json!({
            "job_id": job.job_id,
            "description": job.description,
        })).collect::<Vec<_>>(),
        "open": card.open,
        "flat": card.flat,
    })
}

fn widget_value(card: &tui::ext::WidgetCard<'_>) -> Value {
    json!({
        "extension": card.extension,
        "widget": card.widget,
        "lines": card.lines,
        "open": card.open,
        "flat": card.flat,
    })
}

fn card_value(card: &tui::ext::Card<'_>) -> Value {
    match card {
        tui::ext::Card::Session(session) => session_value(session),
        tui::ext::Card::ChangedFiles(files) => changed_files_value(files),
        tui::ext::Card::Delegates(delegates) => delegates_value(delegates),
        tui::ext::Card::Jobs(jobs) => jobs_value(jobs),
        tui::ext::Card::Widget(widget) => widget_value(widget),
    }
}

fn status_rows_value(rows: &tui::ext::StatusRows<'_>) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("cards".to_owned(), json!(rows.cards));
    out.insert("session".to_owned(), session_value(&rows.session));
    out.insert(
        "changed_files".to_owned(),
        json!({"files": rows.changed_files.files.iter().map(|file| json!({
            "path": file.path,
            "added": file.added,
            "removed": file.removed,
        })).collect::<Vec<_>>()}),
    );
    out.insert(
        "delegates_running".to_owned(),
        json!(rows.delegates_running),
    );
    out.insert("jobs_running".to_owned(), json!(rows.jobs_running));
    out.insert(
        "widgets".to_owned(),
        rows.widgets
            .iter()
            .map(|widget| {
                json!({
                    "extension": widget.extension,
                    "widget": widget.widget,
                    "lines": widget.lines,
                })
            })
            .collect::<Vec<_>>()
            .into(),
    );
    insert_present(
        &mut out,
        "waiting",
        rows.waiting.map(|waiting| json!(waiting)),
    );
    insert_present(&mut out, "quota", rows.quota.as_ref().map(quota_value));
    out.insert("flat".to_owned(), json!(rows.flat));
    Value::Object(out)
}

/// The input box's status row: the working directory, the permission
/// mode, the model and thinking level, and the context's tokens with
/// the window they fill (`docs/tui.md`, "Extension seams").
fn input_status_value(status: &tui::ext::InputStatus<'_>) -> Value {
    let mut out = serde_json::Map::new();
    insert_present(
        &mut out,
        "workspace",
        status.workspace.map(|workspace| json!(workspace)),
    );
    // The mode is always `auto` (`docs/permissions.md`, "One mode"): the
    // input carries no mode field, so the table emits the fixed value.
    out.insert("mode".to_owned(), json!("auto"));
    insert_present(&mut out, "model", status.model.map(|model| json!(model)));
    insert_present(
        &mut out,
        "thinking",
        status.thinking.map(|thinking| json!(thinking)),
    );
    insert_present(
        &mut out,
        "context",
        status
            .context
            .map(|context| json!({"tokens": context.tokens, "window": context.window})),
    );
    out.insert("flat".to_owned(), json!(status.flat));
    Value::Object(out)
}

fn code_value(code: &contract::ErrorCode) -> Value {
    serde_json::to_value(code).unwrap_or(Value::Null)
}

fn streaming_value(streaming: &tui::ext::TurnStreaming<'_>) -> Value {
    json!({"message": streaming.message, "index": streaming.index, "name": streaming.name, "text": streaming.text})
}

fn thought_value(thought: &tui::ext::TurnThought<'_>) -> Value {
    json!({"text": thought.text, "started": thought.started, "ended": thought.ended, "open": thought.open})
}

fn failed_value(failed: &tui::ext::TurnFailed<'_>) -> Value {
    json!({"code": failed.code, "attempt": failed.attempt})
}

fn answer_part_value(part: &tui::ext::TurnAnswerPart<'_>) -> Value {
    match &part.disposition {
        tui::ext::AnswerDisposition::Skipped => {
            json!({"header": part.header, "text": "skipped", "skipped": true})
        }
        tui::ext::AnswerDisposition::Answered { text } => {
            json!({"header": part.header, "text": text, "skipped": false})
        }
    }
}

/// The Lua status word for a call state.
fn call_status_str<R, D>(state: &CallState<'_, R, D>) -> &'static str {
    match state {
        CallState::Pending => "pending",
        CallState::Running => "running",
        CallState::Unknown => "unknown",
        CallState::Completed(_) => "completed",
        CallState::Failed { .. } => "failed",
        CallState::Denied { .. } => "denied",
        CallState::Cancelled(_) => "cancelled",
    }
}

fn ledger_value(row: &tui::ext::LedgerRow<'_>) -> Value {
    let status = call_status_str(&row.state);
    let (exit, duration_ms, lines, changes, error, output) = match &row.state {
        CallState::Pending | CallState::Running | CallState::Unknown => (
            Value::Null,
            Value::Null,
            json!(0),
            json!(Vec::<Value>::new()),
            Value::Null,
            json!("done"),
        ),
        CallState::Completed(result) | CallState::Cancelled(result) => (
            json!(result.exit),
            json!(result.duration_ms),
            json!(result.lines),
            json!(result.changes),
            Value::Null,
            json!(result.output),
        ),
        CallState::Failed { result, error } => (
            json!(result.exit),
            json!(result.duration_ms),
            json!(result.lines),
            json!(result.changes),
            json!({"code": error.code, "message": error.message}),
            json!(result.output),
        ),
        CallState::Denied { result, .. } => (
            json!(result.exit),
            json!(result.duration_ms),
            json!(result.lines),
            json!(result.changes),
            Value::Null,
            json!(result.output),
        ),
    };
    json!({"name": row.name, "arguments": row.arguments, "status": status, "exit": exit, "duration_ms": duration_ms, "lines": lines, "changes": changes, "error": error, "output": output, "answer": row.answer, "flat": row.flat})
}

fn images_value(images: &[tui::ext::PromptImage<'_>]) -> Value {
    images
        .iter()
        .map(|image| json!({"path": image.path, "width": image.width, "height": image.height}))
        .collect()
}

/// A band's state as Lua reads it: `writing`, `done` with its context
/// sizes, or `unchanged` with `cancelled` (`docs/tui.md`, "The slots",
/// `handoff_band`).
fn band_state_value(state: &tui::ext::TurnBandState) -> Value {
    match state {
        tui::ext::TurnBandState::Writing => json!({"kind": "writing"}),
        tui::ext::TurnBandState::Done(sizes) => {
            json!({"kind": "done", "before": sizes.before, "after": sizes.after})
        }
        tui::ext::TurnBandState::Unchanged { cancelled } => {
            json!({"kind": "unchanged", "cancelled": cancelled})
        }
    }
}

/// How a turn ended as Lua reads it: the outcome with its timestamp,
/// and on a failure the error (`docs/tui.md`, "The slots", `turn_card`).
fn ended_value(ended: &tui::ext::TurnEnded<'_>) -> Value {
    let (outcome, ts, error) = match ended {
        tui::ext::TurnEnded::Completed { ended_ts } => ("completed", ended_ts, Value::Null),
        tui::ext::TurnEnded::Interrupted { ended_ts } => ("interrupted", ended_ts, Value::Null),
        tui::ext::TurnEnded::Failed { ended_ts, error } => (
            "failed",
            ended_ts,
            error
                .as_ref()
                .map(|error| {
                    json!({
                        "code": code_value(&error.code),
                        "message": error.message,
                    })
                })
                .unwrap_or(Value::Null),
        ),
        tui::ext::TurnEnded::CutShort { ended_ts } => ("cut short", ended_ts, Value::Null),
    };
    json!({"outcome": outcome, "ended_ts": ts, "error": error})
}

fn turn_card_value(card: &tui::ext::TurnCard<'_>) -> Value {
    let (ended, running) = match &card.state {
        tui::ext::TurnCardState::Running => (Value::Null, true),
        tui::ext::TurnCardState::Ended(ended) => (ended_value(ended), false),
    };
    json!({
        "prompts": card.prompts.iter().map(|prompt| json!({"text": prompt.text, "images": images_value(&prompt.images)})).collect::<Vec<_>>(),
        "entries": card.entries.iter().map(entry_value).collect::<Vec<_>>(),
        "started": card.started,
        "calls": card.calls,
        "usage": &card.usage,
        "ended": ended,
        "running": running,
        "flat": card.flat,
    })
}

fn entry_value(entry: &tui::ext::TurnEntry<'_>) -> Value {
    match entry {
        tui::ext::TurnEntry::Reply { text } => json!({"kind": "reply", "text": text}),
        tui::ext::TurnEntry::Steer { text, images, ts } => {
            json!({"kind": "steer", "text": text, "images": images_value(images), "ts": ts})
        }
        tui::ext::TurnEntry::Message {
            source,
            label,
            text,
            images,
            ts,
        } => {
            json!({"kind": "message", "source": source, "label": label, "text": text, "images": images_value(images), "ts": ts})
        }
        tui::ext::TurnEntry::Group(group) => {
            let (running, cut) = match group.state {
                tui::ext::TurnGroupState::Running => (true, false),
                tui::ext::TurnGroupState::Finished => (false, false),
                tui::ext::TurnGroupState::Cut => (false, true),
            };
            json!({
                "kind": "group",
                "first": group.first,
                "last": group.last,
                "running": running,
                "open": group.open,
                "cut": cut,
                "sections": group.sections.iter().map(|section| json!({
                    "step": section.step,
                    "thoughts": section.thoughts.iter().map(thought_value).collect::<Vec<_>>(),
                    "calls": section.calls.iter().map(call_value).collect::<Vec<_>>(),
                    "failed": section.failed.iter().map(failed_value).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "streaming": group.streaming.iter().map(streaming_value).collect::<Vec<_>>(),
            })
        }
        tui::ext::TurnEntry::Aside(aside) => aside_value(aside),
        tui::ext::TurnEntry::Band(band) => {
            json!({
                "kind": "band",
                "trigger": band.trigger,
                "ts": band.ts,
                "state": band_state_value(&band.state),
            })
        }
        tui::ext::TurnEntry::Answers(answers) => match answers {
            tui::ext::TurnAnswers::Given { parts, note, ts } => json!({
                "kind": "answers",
                "outcome": "given",
                "parts": parts.iter().map(answer_part_value).collect::<Vec<_>>(),
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
    let status = call_status_str(&call.state);
    let headline = call.headline();
    let (exit, duration_ms, lines, changes, error, output, content, images) = match &call.state {
        CallState::Pending | CallState::Running | CallState::Unknown => (
            Value::Null,
            Value::Null,
            json!(0),
            json!(Vec::<Value>::new()),
            Value::Null,
            json!("done"),
            String::new(),
            json!(Vec::<Value>::new()),
        ),
        CallState::Completed(result) | CallState::Cancelled(result) => (
            json!(result.exit),
            json!(result.duration_ms),
            json!(result.lines),
            json!(result.changes),
            Value::Null,
            json!(result.output),
            result.content.to_owned(),
            images_value(&result.images),
        ),
        CallState::Failed { result, error } => (
            json!(result.exit),
            json!(result.duration_ms),
            json!(result.lines),
            json!(result.changes),
            json!({"code": error.code, "message": error.message}),
            json!(result.output),
            result.content.to_owned(),
            images_value(&result.images),
        ),
        CallState::Denied { result, .. } => (
            json!(result.exit),
            json!(result.duration_ms),
            json!(result.lines),
            json!(result.changes),
            Value::Null,
            json!(result.output),
            result.content.to_owned(),
            images_value(&result.images),
        ),
    };
    json!({
        "name": call.name,
        "arguments": call.arguments,
        "status": status,
        "exit": exit,
        "duration_ms": duration_ms,
        "lines": lines,
        "changes": changes,
        "error": error,
        "output": output,
        "headline": headline,
        "content": content,
        "answer": call.answer,
        "images": images,
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
