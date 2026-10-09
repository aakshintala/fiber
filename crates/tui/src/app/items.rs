//! A running delegate's swapped view: the item fold, the screen swap and
//! the entries into its view (`docs/tui.md`, "Swapped views": "A view swaps
//! into the conversation area and takes all of it. Esc returns to the
//! conversation.").

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use contract::{ActionId, Envelope, JobId, SessionId};

use super::{App, Effect, Kind, Link, Phase, mint, session_command};
use crate::home::Level;
use crate::tty_screen::Output;

pub(crate) mod output;
pub(crate) mod retry;

/// A click target in the item view (`docs/tui.md`, "Swapped views": every
/// action has a mouse target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    /// The header's ✕: closes the view.
    Close,
    /// The status row's stop target: stops the item's job.
    Stop,
}

/// One delegate run behind a job.
#[derive(Debug, Clone)]
pub(super) struct DelegateRun {
    /// The delegate's session.
    session: SessionId,
    /// The harness, such as `fiber`.
    harness: String,
    /// The model reference.
    model: String,
    /// The `delegate_started` envelope's `ts`.
    started_ts: u64,
    /// The delegate's `tool_call_completed` lines folded into the view.
    calls: u64,
}

/// One job of the attached session, in start order.
#[derive(Debug, Clone)]
pub(super) struct JobRecord {
    /// The stable serial naming its rows, never reused within an
    /// attachment.
    serial: u64,
    /// The `job_started` description.
    description: String,
    /// The `job_started` output path.
    output_path: String,
    /// The `job_started` envelope's `ts`.
    started_ts: u64,
    /// The delegate run, when a `delegate_started` named this job.
    delegate: Option<DelegateRun>,
    /// How the job ended and the `job_completed` envelope's `ts`.
    outcome: Option<(String, u64)>,
    /// The job's live output since the terminal attached; dropped at
    /// `job_completed` unless the job's view is open.
    output: Option<Output>,
}

/// The open item: the job and the attached session's screen while a fiber
/// delegate's transcript is swapped in.
pub(super) struct Open {
    /// The open job.
    pub(super) job_id: JobId,
    /// The attached session's screen, kept while a transcript is swapped
    /// in; `None` for a view with no transcript.
    pub(super) stashed: Option<super::screen::Screen>,
    /// What opening or steering the item last refused with.
    pub(super) message: Option<String>,
}

/// What the item view draws: the breadcrumb, the status row and the body.
#[derive(Debug, Clone)]
pub(crate) struct ItemView {
    /// The attached session's name, or `"main"` with none.
    pub(crate) parent: String,
    /// `◆` for a delegate, `▸` for a job.
    pub(crate) kind_glyph: &'static str,
    /// The job's description.
    pub(crate) description: String,
    /// The status glyph and word.
    pub(crate) glyph: String,
    /// The status word.
    pub(crate) word: String,
    /// The harness, or `"—"` when unavailable.
    pub(crate) harness: String,
    /// The model, or `"—"` when unavailable.
    pub(crate) model: String,
    /// The delegate's folded call count; `None` draws `"—"`.
    pub(crate) calls: Option<u64>,
    /// The run's start: the `delegate_started` envelope `ts`, else the
    /// job's `job_started` one.
    pub(crate) started_ts: u64,
    /// The `job_completed` envelope's `ts`, frozen at completion.
    pub(crate) completed_ts: Option<u64>,
    /// The delegate's session, or `"—"` for a job.
    pub(crate) session_label: Option<String>,
    /// Whether the item still runs.
    pub(crate) running: bool,
    /// What opening or steering last refused with.
    pub(crate) message: Option<String>,
    /// Whether a transcript is swapped in: a running or completed fiber
    /// delegate.
    pub(crate) has_transcript: bool,
    /// The job's output path.
    pub(crate) output_path: String,
}

/// The attached session's items: the per-job fold, the open item and the
/// delegate subscription wishes.
pub(crate) struct Items {
    /// The per-job fold, by job.
    pub(super) jobs: HashMap<JobId, JobRecord>,
    /// The job holding each serial.
    pub(super) by_serial: HashMap<u64, JobId>,
    /// The next serial: counting up from 1 on each attachment, never
    /// reused.
    pub(super) next_serial: u64,
    /// The open item, if any.
    pub(super) open: Option<Open>,
    /// The delegate subscription wishes, by delegate session.
    pub(super) wants: BTreeMap<SessionId, retry::Want>,
    /// The tool calls waiting for their `job_started`: their action ids,
    /// marked by a `shell` call with `tty`.
    pub(super) tty_marks: HashSet<ActionId>,
    /// The last `items_due` call's `now`: a wake never re-arms on a passed
    /// deadline.
    pub(super) last_due: Option<Instant>,
}

impl Default for Items {
    fn default() -> Self {
        Self {
            jobs: HashMap::new(),
            by_serial: HashMap::new(),
            next_serial: 1,
            open: None,
            wants: BTreeMap::new(),
            tty_marks: HashSet::new(),
            last_due: None,
        }
    }
}

impl Items {
    /// The record for `job`, creating it with the next serial when it is
    /// new.
    fn record(&mut self, job: &JobId) -> &mut JobRecord {
        if !self.jobs.contains_key(job) {
            let serial = self.next_serial;
            self.next_serial = self.next_serial.saturating_add(1);
            self.by_serial.insert(serial, job.clone());
            self.jobs.insert(
                job.clone(),
                JobRecord {
                    serial,
                    description: String::new(),
                    output_path: String::new(),
                    started_ts: 0,
                    delegate: None,
                    outcome: None,
                    output: None,
                },
            );
        }
        self.jobs.get_mut(job).unwrap_or_else(|| unreachable!())
    }
}

impl App {
    /// Whether an item view is open.
    pub(crate) fn item_open(&self) -> bool {
        self.items.open.is_some()
    }

    /// The open item's job, if any.
    fn open_job(&self) -> Option<JobId> {
        self.items.open.as_ref().map(|open| open.job_id.clone())
    }

    /// Whether `session` is the open fiber delegate's session.
    pub(super) fn is_item_session(&self, session: &SessionId) -> bool {
        let Some(open) = self.items.open.as_ref() else {
            return false;
        };
        self.items
            .jobs
            .get(&open.job_id)
            .and_then(|record| record.delegate.as_ref())
            .is_some_and(|delegate| delegate.session == *session)
    }

    /// What the item view draws: the breadcrumb, the status row and the
    /// body (`docs/tui.md`, "Swapped views").
    pub(crate) fn item_view(&self) -> Option<ItemView> {
        let open = self.items.open.as_ref()?;
        let record = self.items.jobs.get(&open.job_id)?;
        let parent = {
            let name = self.header();
            if name.is_empty() {
                "main".to_owned()
            } else {
                name
            }
        };
        let running = record.outcome.is_none();
        let (kind_glyph, harness, model, calls, started_ts, session_label, has_transcript) =
            match &record.delegate {
                Some(delegate) if delegate.harness == "fiber" => (
                    "◆",
                    delegate.harness.clone(),
                    delegate.model.clone(),
                    Some(delegate.calls),
                    delegate.started_ts,
                    Some(delegate.session.0.clone()),
                    true,
                ),
                Some(delegate) => (
                    "◆",
                    delegate.harness.clone(),
                    delegate.model.clone(),
                    None,
                    delegate.started_ts,
                    Some(delegate.session.0.clone()),
                    false,
                ),
                None => (
                    "▸",
                    "—".to_owned(),
                    "—".to_owned(),
                    None,
                    record.started_ts,
                    None,
                    false,
                ),
            };
        let (glyph, word) = if running {
            match record.delegate.as_ref() {
                Some(delegate) => match self.delegate_row(&delegate.session) {
                    Some(row) => (
                        self.motion().glyph(row).to_owned(),
                        crate::view::rail::word(row).to_owned(),
                    ),
                    // A delegate with no status held runs by definition:
                    // the spinner and "running".
                    None => (self.motion().spinner().to_owned(), "running".to_owned()),
                },
                None => (self.motion().spinner().to_owned(), "running".to_owned()),
            }
        } else {
            let outcome = record
                .outcome
                .as_ref()
                .map(|(outcome, _)| outcome.as_str())
                .unwrap_or("completed");
            let mark = match outcome {
                "failed" => "✗",
                "cancelled" => "■",
                _ => "▣",
            };
            (mark.to_owned(), outcome.to_owned())
        };
        Some(ItemView {
            parent,
            kind_glyph,
            description: record.description.clone(),
            glyph,
            word,
            harness,
            model,
            calls,
            started_ts,
            completed_ts: record.outcome.as_ref().map(|(_, ts)| *ts),
            session_label,
            running,
            message: open.message.clone(),
            has_transcript,
            output_path: record.output_path.clone(),
        })
    }

    /// The job holding `serial`, if any.
    fn job_of_serial(&self, serial: u64) -> Option<JobId> {
        self.items.by_serial.get(&serial).cloned()
    }

    /// The serial of `job`, if folded.
    pub(crate) fn serial_of_job(&self, job: &JobId) -> Option<u64> {
        self.items.jobs.get(job).map(|record| record.serial)
    }

    /// Opens `job`'s view: a running fiber delegate swaps a fresh screen
    /// in and subscribes at `full`; any other item opens without a swap.
    /// Opening closes the other views and search first, so drawing,
    /// scrolling and focus act on the shown transcript with no per-call
    /// routing.
    pub(super) fn open_item(&mut self, job: &JobId) -> Effect {
        let Some(record) = self.items.jobs.get(job).cloned() else {
            return Effect::None;
        };
        if self.open_job().as_ref() == Some(job) {
            return Effect::None;
        }
        self.close_item();
        self.close_keymap();
        self.close_config_view();
        self.close_session_view();
        self.model_picker.close();
        self.close_find();
        self.clear_selection();
        let fiber = record
            .delegate
            .as_ref()
            .is_some_and(|delegate| delegate.harness == "fiber");
        if fiber {
            let mut swapped = super::screen::Screen::new();
            swapped.set_size(self.screen.width(), self.screen.height());
            swapped.pages_mut().zone = self.screen.pages().zone.clone();
            let stashed = std::mem::replace(&mut self.screen, swapped);
            // A fresh screen replays the transcript from the start, so
            // the call count restarts with it: replayed completions
            // would otherwise count twice.
            if let Some(record) = self.items.jobs.get_mut(job)
                && let Some(delegate) = record.delegate.as_mut()
            {
                delegate.calls = 0;
            }
            self.items.open = Some(Open {
                job_id: job.clone(),
                stashed: Some(stashed),
                message: None,
            });
        } else {
            self.items.open = Some(Open {
                job_id: job.clone(),
                stashed: None,
                message: None,
            });
            return Effect::None;
        }
        let Some(delegate) = self
            .items
            .jobs
            .get(job)
            .and_then(|record| record.delegate.clone())
        else {
            return Effect::None;
        };
        if record.outcome.is_some() {
            return Effect::None;
        }
        self.want_full(&delegate.session)
    }

    /// Makes a `full` wish for `session` and sends what goes out now: one
    /// `full`, or `summary` then `full` when `full` is held with nothing
    /// in flight, or nothing while a subscribe is in flight or the link
    /// is down.
    fn want_full(&mut self, session: &SessionId) -> Effect {
        self.items
            .wants
            .insert(session.clone(), retry::Want::full());
        if self.link != Link::Up {
            return Effect::None;
        }
        if self.subscribe_pending(session) {
            return Effect::None;
        }
        match self.subscribed_level(session) {
            None | Some(Level::Summary) => {
                self.items.wants.remove(session);
                Effect::Send(vec![self.subscribe(session, Level::Full)])
            }
            Some(Level::Full) => {
                self.items.wants.remove(session);
                Effect::Send(vec![
                    self.subscribe(session, Level::Summary),
                    self.subscribe(session, Level::Full),
                ])
            }
        }
    }

    /// Closes the open item view, swapping the attached screen back first.
    /// A delegate view leaves a `summary` wish, which `items_due` lowers.
    pub(super) fn close_item(&mut self) -> Option<String> {
        let open = self.items.open.take()?;
        if let Some(stashed) = open.stashed {
            let (width, height) = (self.screen.width(), self.screen.height());
            self.screen = stashed;
            self.screen.set_size(width, height);
        }
        let record = self.items.jobs.get(&open.job_id);
        let delegate = record.and_then(|record| record.delegate.as_ref())?;
        if record.is_some_and(|record| record.outcome.is_some()) {
            return None;
        }
        self.items
            .wants
            .insert(delegate.session.clone(), retry::Want::summary());
        // The lowering goes out through `items_due` on the same step, so
        // a close never sends at a level the connection holds.
        None
    }

    /// Esc in an item view closes it, after notices, panels and
    /// completions, before interrupting the turn: it never sends `cancel`.
    pub(super) fn item_esc(&mut self) -> Effect {
        self.close_item();
        self.settle();
        Effect::None
    }

    /// A click on an item target: ✕ closes, stop sends `job_stop` to the
    /// attached session, which owns the job.
    pub(super) fn item_click(&mut self, spot: Spot) -> Effect {
        match spot {
            Spot::Close => {
                self.close_item();
                self.settle();
                Effect::None
            }
            Spot::Stop => self.item_stop(),
        }
    }

    /// The stop target: one `job_stop` naming the open job, sent to the
    /// attached session while the item runs.
    fn item_stop(&mut self) -> Effect {
        let Some(open) = self.items.open.as_ref() else {
            return Effect::None;
        };
        let running = self
            .items
            .jobs
            .get(&open.job_id)
            .is_some_and(|record| record.outcome.is_none());
        if !running {
            return Effect::None;
        }
        let Phase::Attached { session, .. } = &self.phase else {
            return Effect::None;
        };
        if self.link != Link::Up {
            return Effect::None;
        }
        let session = session.clone();
        let job_id = open.job_id.clone();
        let id = mint();
        let line = session_command(
            &id,
            "job_stop",
            &session,
            Some(serde_json::json!({"job_id": job_id.0})),
        )
        .to_string();
        self.pending
            .insert(id, (Kind::Command, crate::input::Draft::default()));
        Effect::Send(vec![line])
    }

    /// Enter in an item view: a `/` built-in runs on the attached session
    /// as today; anything else goes to the open fiber delegate as `steer`.
    /// `None` outside an item view.
    pub(super) fn item_enter(&mut self) -> Option<Effect> {
        let open = self.items.open.as_ref()?;
        let job_id = open.job_id.clone();
        let record = self.items.jobs.get(&job_id)?.clone();
        let delegate = match record.delegate {
            Some(delegate) if delegate.harness == "fiber" => delegate,
            _ => {
                let message = if record.delegate.is_some() {
                    "This delegate takes no steering here."
                } else {
                    "A job takes no input here."
                };
                self.notices.push(message.to_owned());
                return Some(Effect::None);
            }
        };
        if record.outcome.is_some() {
            self.notices.push("This delegate has finished.".to_owned());
            return Some(Effect::None);
        }
        // A `/` built-in already ran in `on_enter` before this call, so
        // everything left, `!` text included, goes to the delegate as
        // `steer` with the draft's content.
        let text = self.draft.expand();
        if !self.draft.has_image() && text.trim().is_empty() {
            return Some(Effect::None);
        }
        if self.link != Link::Up {
            return Some(Effect::None);
        }
        let id = mint();
        let args = super::commands::content_arg(&self.draft);
        let line = session_command(&id, "steer", &delegate.session, Some(args)).to_string();
        let draft = std::mem::take(&mut self.draft);
        self.pending.insert(id, (Kind::Steer, draft));
        Some(Effect::Send(vec![line]))
    }

    /// Folds one attached-session line into the item fold: `job_started`
    /// takes the next serial, `delegate_started` keeps the run's start,
    /// and `job_completed` keeps the outcome frozen at its `ts`.
    pub(super) fn items_line(&mut self, envelope: &Envelope) {
        match envelope.kind.as_str() {
            "job_started" => {
                if let Some(started) = super::read!(envelope, contract::events::JobStarted) {
                    let record = self.items.record(&started.job_id);
                    record.description = started.description;
                    record.output_path = started.output_path;
                    record.started_ts = envelope.ts;
                    // A delegate resumes under the same job id
                    // (`docs/delegates.md`, "Identity and resume"), so a
                    // new run clears the previous outcome: the row is
                    // clickable again and an open view runs again.
                    record.outcome = None;
                }
            }
            "delegate_started" => {
                if let Some(started) = super::read!(envelope, contract::events::DelegateStarted) {
                    let record = self.items.record(&started.job_id);
                    record.delegate = Some(DelegateRun {
                        session: started.delegate_session_id,
                        harness: started.harness,
                        model: started.model,
                        started_ts: envelope.ts,
                        calls: record.delegate.as_ref().map_or(0, |run| run.calls),
                    });
                }
            }
            "job_completed" => {
                if let Some(done) = super::read!(envelope, contract::events::JobCompleted) {
                    let word = match done.status {
                        contract::events::Outcome::Completed => "completed",
                        contract::events::Outcome::Failed => "failed",
                        contract::events::Outcome::Cancelled => "cancelled",
                    };
                    let record = self.items.record(&done.job_id);
                    record.outcome = Some((word.to_owned(), envelope.ts));
                    // A completed delegate drops its wish: a lowering
                    // would answer `session_not_found`.
                    if let Some(delegate) = record.delegate.clone() {
                        self.items.wants.remove(&delegate.session);
                    }
                }
            }
            _ => {}
        }
    }

    /// Folds one open delegate's line into the swapped screen, counting
    /// its `tool_call_completed` lines. True when the line was the open
    /// delegate's. A delegate line never changes the attached busy flag.
    pub(super) fn item_session_line(&mut self, envelope: &Envelope) -> bool {
        if !self.is_item_session(&envelope.session_id) {
            return false;
        }
        if envelope.kind == "tool_call_completed"
            && let Some(open) = self.items.open.as_ref()
            && let Some(record) = self.items.jobs.get_mut(&open.job_id)
            && let Some(delegate) = record.delegate.as_mut()
        {
            delegate.calls = delegate.calls.saturating_add(1);
        }
        // The hub's answers settle below in `on_session`'s non-attached
        // branch; the transcript folds here without touching the
        // attached busy flag.
        let applied = self.screen.pages_mut().apply(envelope);
        if applied.changed {
            self.screen.changed();
        }
        true
    }

    /// The attached session's screen: the stashed one while a transcript
    /// is swapped in, else the shown one. Every write folding the
    /// attached session's lines goes through it.
    pub(super) fn attached_screen(&self) -> &super::screen::Screen {
        self.items
            .open
            .as_ref()
            .and_then(|open| open.stashed.as_ref())
            .map_or(&self.screen, |stashed| stashed)
    }

    /// The attached session's screen, for folding its lines.
    pub(super) fn attached_screen_mut(&mut self) -> &mut super::screen::Screen {
        if self
            .items
            .open
            .as_ref()
            .is_some_and(|open| open.stashed.is_some())
        {
            self.items
                .open
                .as_mut()
                .and_then(|open| open.stashed.as_mut())
                .unwrap_or_else(|| unreachable!())
        } else {
            &mut self.screen
        }
    }

    /// The session history pages for: the open delegate's, else the
    /// attached one.
    pub(crate) fn paging_session(&self) -> Option<&SessionId> {
        if let Some(open) = self.items.open.as_ref()
            && let Some(record) = self.items.jobs.get(&open.job_id)
            && let Some(delegate) = record.delegate.as_ref()
            && delegate.harness == "fiber"
        {
            return Some(&delegate.session);
        }
        self.session()
    }

    /// Opens the delegate holding `serial`, when it still runs.
    pub(super) fn open_serial(&mut self, serial: u64) -> Effect {
        let Some(job) = self.job_of_serial(serial) else {
            return Effect::None;
        };
        let running = self
            .items
            .jobs
            .get(&job)
            .is_some_and(|record| record.outcome.is_none());
        if !running {
            return Effect::None;
        }
        self.open_item(&job)
    }

    /// Leaves the attached session: closes the item view, drops retry
    /// wishes tied to that attachment and clears its item fold. A pending
    /// `summary` lowering stays until it is sent or the link drops.
    pub(super) fn leave_item(&mut self) {
        self.close_item();
        // Retry wishes belong to this attachment; summary lowerings remain
        // until the hub acknowledges the in-flight `full`.
        let retries: Vec<SessionId> = self
            .items
            .wants
            .iter()
            .filter(|(_, want)| want.retry_at.is_some())
            .map(|(session, _)| session.clone())
            .collect();
        for session in retries {
            self.items.wants.remove(&session);
        }
        self.items.jobs.clear();
        self.items.by_serial.clear();
        self.items.next_serial = 1;
        self.items.tty_marks.clear();
    }

    /// Drops the item state on a lost connection: the hub connection and
    /// its levels are gone.
    pub(super) fn drop_items(&mut self) {
        if let Some(open) = self.items.open.take()
            && let Some(stashed) = open.stashed
        {
            self.screen = stashed;
        }
        self.items.wants.clear();
        self.items.last_due = None;
    }
}

#[cfg(test)]
#[path = "items_tests.rs"]
mod tests;
