//! Home's subscriptions and the open gate: this connection's level per
//! session, the session opening and the lowering lines reconciliation
//! queued.

use serde_json::Value;

use super::super::outbox::Outbox;
use super::super::{App, Effect, Phase, Waiting, session_line};
use super::{refusal, refusal_code};
use crate::home::Subs;
use crate::keyset::Context;
use contract::ErrorCode;
use contract::SessionId;
use contract::commands::{Command, SubscribeArgs, SubscribeLevel};

/// This connection's subscriptions: the level per session, the session
/// opening and the lowering lines reconciliation queued.
#[derive(Default)]
pub(super) struct Subscriptions {
    /// This connection's accepted level per session. A subscribe waiting
    /// for its answer lives only as its outbox `Subscribe` tag.
    levels: Subs,
    /// The session being opened: lines for it fold into its row and are
    /// dropped until the last subscribe's acknowledgement, which is the
    /// first line at the new level.
    opening: Option<Opening>,
    /// Lowering lines reconciliation queued, sent from `on_line`'s tail.
    outbox: Vec<String>,
}

/// The last `Subscribe` tag out for `session`, in send order: its id and
/// level.
fn last_subscribe<'a>(
    outbox: &'a Outbox,
    session: &SessionId,
) -> Option<(&'a str, SubscribeLevel)> {
    outbox
        .tags()
        .filter_map(|(id, waiting)| {
            if let Waiting::Subscribe {
                session: asked,
                level,
            } = waiting
                && asked == session
            {
                Some((id, *level))
            } else {
                None
            }
        })
        .last()
}

impl Subscriptions {
    /// The level a new subscribe for `session` must differ from: the
    /// last subscribe out for it, else the accepted one.
    pub(super) fn expected(&self, outbox: &Outbox, session: &SessionId) -> Option<SubscribeLevel> {
        last_subscribe(outbox, session)
            .map(|(_, level)| level)
            .or_else(|| self.levels.accepted_level(session))
    }

    /// Records `level` as accepted for `session`.
    pub(super) fn accept(&mut self, session: SessionId, level: SubscribeLevel) {
        self.levels.accept(session, level);
    }

    /// Whether the accepted level for `session` is `full`.
    pub(super) fn full(&self, session: &SessionId) -> bool {
        self.levels.full(session)
    }

    /// Whether a subscribe for `session` waits for its answer.
    pub(super) fn pending(&self, outbox: &Outbox, session: &SessionId) -> bool {
        last_subscribe(outbox, session).is_some()
    }

    /// The id of the last subscribe out for `session` when it asks for
    /// `summary`.
    pub(super) fn pending_summary(&self, outbox: &Outbox, session: &SessionId) -> Option<String> {
        last_subscribe(outbox, session)
            .filter(|(_, level)| *level == SubscribeLevel::Summary)
            .map(|(id, _)| id.to_owned())
    }

    /// Opens the gate for `session`: the first stage's acknowledgement
    /// is `ack` when a first-stage subscribe went out, and `None` when
    /// the connection already holds accepted `summary`, when the raise
    /// is due at once; the caller passes an in-flight `summary`'s id
    /// then. `held` keeps the screen's rows: the tail read is skipped
    /// and the raise goes out after the first frame draws, once the
    /// preview is up. Until the raise's acknowledgement the gate holds.
    pub(super) fn gate(&mut self, session: SessionId, ack: Option<String>, held: bool) {
        self.opening = Some(match ack {
            Some(ack) => Opening::Summary { session, ack, held },
            None => Opening::Due { session, held },
        });
    }

    /// Records the raise sent with `id`: the first stage is answered,
    /// so the open moves from due to raised; a read preview moves from
    /// reading to raised.
    pub(super) fn raise(&mut self, id: String) {
        let session = match self.opening.take() {
            Some(Opening::Due { session, .. }) | Some(Opening::Reading { session }) => session,
            opening => {
                self.opening = opening;
                return;
            }
        };
        self.opening = Some(Opening::Raised { session, ack: id });
    }

    /// The session opening, if any.
    pub(super) fn opening_session(&self) -> Option<SessionId> {
        self.opening
            .as_ref()
            .map(|opening| opening.session().clone())
    }

    /// Moves a due open to the tail read for the attached session: the
    /// read before the raise. A held open reads nothing and stays due;
    /// an open whose session is no longer attached drops. Some with the
    /// session to read when the move happened.
    pub(super) fn tail_due(&mut self, attached: Option<&SessionId>) -> Option<SessionId> {
        let (session, held) = match self.opening.as_ref() {
            Some(Opening::Due { session, held }) => (session.clone(), *held),
            _ => return None,
        };
        if Some(&session) != attached {
            self.opening = None;
            return None;
        }
        if held {
            return None;
        }
        self.opening = Some(Opening::Reading {
            session: session.clone(),
        });
        Some(session)
    }

    /// Clears the old connection's levels and open gate, keeping the
    /// queued lowering lines.
    pub(super) fn reset(&mut self) {
        self.levels = Subs::default();
        self.opening = None;
    }

    /// Takes the lowering lines reconciliation queued.
    pub(super) fn take_outbox(&mut self) -> Vec<String> {
        std::mem::take(&mut self.outbox)
    }

    /// Queues one line for the next outgoing pass, which drains it.
    pub(super) fn queue(&mut self, line: String) {
        self.outbox.push(line);
    }
}

/// A session opening from home: the first stage in flight, the raise
/// due after the first frame, the tail reading before the raise, or
/// the raise in flight. Either acknowledgement's rejection fails the
/// open; the raise's acceptance ends the gate.
enum Opening {
    /// The first-stage `summary` subscribe in flight.
    Summary {
        session: SessionId,
        ack: String,
        held: bool,
    },
    /// The first stage answered: the `full` raise goes out after the
    /// first frame draws, once the tail read installs the preview.
    Due { session: SessionId, held: bool },
    /// The tail read in flight: the raise goes once it installs.
    Reading { session: SessionId },
    /// The `full` raise in flight.
    Raised { session: SessionId, ack: String },
}

impl Opening {
    /// The session opening.
    fn session(&self) -> &SessionId {
        match self {
            Self::Summary { session, .. }
            | Self::Due { session, .. }
            | Self::Reading { session }
            | Self::Raised { session, .. } => session,
        }
    }
}

/// A `subscribe` line for `session` at `level`, with its id. With
/// `credential`, the label rides on this subscribe only, never as
/// `null`: an absent label leaves the key out.
pub(super) fn subscribe_line(
    session: &SessionId,
    level: SubscribeLevel,
    credential: Option<&str>,
) -> (String, String) {
    let id = support::mint("c_");
    let line = session_line(
        &id,
        session,
        Command::Subscribe(SubscribeArgs {
            level,
            credential: credential.map(str::to_owned),
        }),
    );
    (id, line)
}

impl App {
    /// A `subscribe` line for `session` at `level`, tagged as sent: the
    /// expected level is the last one tagged, so no path sends a
    /// subscribe at the level already held or asked for.
    pub(in crate::app) fn subscribe(
        &mut self,
        session: &SessionId,
        level: SubscribeLevel,
    ) -> String {
        let (id, line) = subscribe_line(session, level, None);
        self.outbox.wait(
            &id,
            Waiting::Subscribe {
                session: session.clone(),
                level,
            },
        );
        line
    }

    /// The level this connection holds or asks for `session` at, if any:
    /// a delegate is subscribed once per attachment (`docs/invocation.md`,
    /// `subscribe`).
    pub(in crate::app) fn subscribed_level(&self, session: &SessionId) -> Option<SubscribeLevel> {
        self.home.subs.expected(&self.outbox, session)
    }

    /// Whether a subscribe for `session` waits for its answer.
    pub(in crate::app) fn subscribe_pending(&self, session: &SessionId) -> bool {
        self.home.subs.pending(&self.outbox, session)
    }

    /// Leaves the session on screen for home: the conversation cleared
    /// with the session left running, then lowered to `summary` when this
    /// connection holds it at `full` and its row is live. `/close` keeps
    /// `go_home` and lowers nothing: a command for an exited session
    /// would resume it. The session left is remembered as the one home
    /// came from, so the back key and its hint stay live; with none
    /// attached, home keeps the session it came from.
    pub(in crate::app) fn leave(&mut self) -> Effect {
        let attached = self.session().cloned();
        // `go_home` resets before the session clears, so the value is
        // saved first and put back when nothing was attached.
        let came_from = self.home.came_from.clone();
        self.go_home();
        // Home's list focus is requested per entry: a plain leave
        // lands on the entry bar, and the callers that focus a row ask
        // for it after. Without this a leave re-pins the last row focus
        // requested, so `/new` would land back on the left row.
        self.home.focus_list = None;
        let Some(session) = attached else {
            self.home.came_from = came_from;
            return Effect::None;
        };
        self.home.came_from = Some(session.clone());
        let lowers = self.outbox.is_up()
            && self.home.subs.expected(&self.outbox, &session) == Some(SubscribeLevel::Full)
            && self
                .home
                .sessions
                .row(&session)
                .is_some_and(|row| row.left.is_none());
        if lowers {
            Effect::Send(vec![self.subscribe(&session, SubscribeLevel::Summary)])
        } else {
            Effect::None
        }
    }

    /// Whether home's back key goes somewhere: home came from a session
    /// whose row is still live. A session that exited or closed, or a
    /// row that vanished, hides the hint and leaves the key untaken.
    pub(in crate::app) fn came_from_live(&self) -> bool {
        self.home.came_from.as_ref().is_some_and(|came_from| {
            self.home
                .sessions
                .row(came_from)
                .is_some_and(|row| row.left.is_none())
        })
    }

    /// Goes home on the attached session's row: `leave`, then focus
    /// that row once home draws, expanding its project when the row is
    /// an exited one hidden behind more. With no row for the session,
    /// or no session on screen, home keeps its focus.
    pub(in crate::app) fn leave_on_row(&mut self) -> Effect {
        let key = self
            .session()
            .and_then(|session| self.home.sessions.row(session))
            .map(|row| row.key);
        let effect = self.leave();
        if let Some(key) = key
            && self.on_home()
        {
            self.focus_row(key);
        }
        effect
    }

    /// Goes back to the session home came from: `None` unless home
    /// draws with an empty box, the key context is the input box or the
    /// conversation, no completion panel or steering selection is open,
    /// no notice overlay is up, and the came-from session is live. No
    /// chip test: the `→` caller checks focus, and Esc goes back with
    /// a chip focused. Otherwise the came-from row opens as one from
    /// home does; with the link down or a `start` in flight that open
    /// is a no-op, and the key stays taken.
    pub(in crate::app) fn go_back(&mut self) -> Option<Effect> {
        if !self.on_home()
            || !self.draft.is_empty()
            || self.completions().is_some()
            || !matches!(self.key_context(), Context::Input | Context::Conversation)
            || self.notice_overlay().is_some()
            || !self.came_from_live()
        {
            return None;
        }
        let key = self
            .home
            .came_from
            .clone()
            .and_then(|came_from| self.home.sessions.row(&came_from))
            .map(|row| row.key)?;
        Some(self.open_entry(key))
    }

    /// Opens the row with `key` while home draws.
    pub(super) fn open_row(&mut self, key: u64) -> Effect {
        if !self.on_home() {
            return Effect::None;
        }
        self.open_entry(key)
    }

    /// Opens the row with `key`: the subscribes its level needs, then the
    /// session's commands. The conversation clears as going home does,
    /// and the gate holds until the last subscribe is answered.
    pub(in crate::app) fn open_entry(&mut self, key: u64) -> Effect {
        if !self.outbox.is_up() || matches!(self.phase, Phase::Pending { .. }) {
            return Effect::None;
        }
        let Some(row) = self.home.sessions.by_key(key) else {
            return Effect::None;
        };
        if row.state == crate::home::State::Unreadable {
            let text = format!("{}.", self.unreadable_reason());
            self.notices.push(ErrorCode::SessionHeld, text);
            return Effect::None;
        }
        let session = row.id.clone();
        let expected = self.subscribed_level(&session);
        Effect::Send(self.open_session(session, expected, None, false))
    }

    /// Whether the open's first-stage summary subscribe is in flight:
    /// the attach summary span runs while this holds, through the
    /// acceptance that queues the session's commands line.
    pub(crate) fn summary_in_flight(&self) -> bool {
        matches!(self.home.subs.opening, Some(Opening::Summary { .. }))
    }

    /// Whether `id` is the open's subscribe in flight: the first
    /// stage's or the raise's. Only its rejection fails the open, and
    /// only the raise's acceptance ends the gate.
    fn is_ack(&self, id: &str) -> bool {
        self.home
            .subs
            .opening
            .as_ref()
            .is_some_and(|opening| match opening {
                Opening::Summary { ack, .. } | Opening::Raised { ack, .. } => ack == id,
                Opening::Due { .. } | Opening::Reading { .. } => false,
            })
    }

    /// Lowers `session` to `summary` when this connection holds it at
    /// `full` without showing it: not the attached one, nothing in
    /// flight, and its row live. Untouched live sessions get no
    /// subscribe: their rows come from the hub's feed, which the hub
    /// reads over its own connections, and subscribing each one would
    /// send a command to every live session.
    pub(super) fn reconcile(&mut self, session: &SessionId) {
        let lowers = self.outbox.is_up()
            && self.session() != Some(session)
            && self.home.subs.full(session)
            && !self.home.subs.pending(&self.outbox, session)
            && self
                .home
                .sessions
                .row(session)
                .is_some_and(|row| row.left.is_none());
        if lowers {
            let line = self.subscribe(session, SubscribeLevel::Summary);
            self.home.subs.outbox.push(line);
        }
    }

    /// The workspace path a missing-workspace resume refusal names, when
    /// `code` and `message` have exactly the hub's unrecoverable-workspace
    /// shape (`docs/invocation.md`, "Lifecycle"): `io_failed` with
    /// `Session <id> could not resume: <path> no longer exists`, with no
    /// trailing period. A refusal carrying the starter-error wrapper's
    /// trailing period is the race the ruling leaves alone, which renders
    /// like any other refusal. Else `None`.
    fn missing_workspace<'message>(code: &str, message: &'message str) -> Option<&'message str> {
        if code != "io_failed" {
            return None;
        }
        let rest = message
            .strip_prefix("Session ")?
            .strip_suffix(" no longer exists")?;
        let (id, path) = rest.split_once(" could not resume: ")?;
        if id.is_empty() || path.is_empty() {
            return None;
        }
        Some(path)
    }

    /// A rejection of the open's subscribe in flight fails the open:
    /// home again when still attached, with the message as the row's
    /// note and a notice. A missing-workspace refusal renders the ruled
    /// sentence instead, naming the path. A rejection of an earlier step
    /// changes nothing by itself.
    fn fail_open(&mut self, code: ErrorCode, message: String) -> Vec<String> {
        let session = self
            .home
            .subs
            .opening
            .take()
            .map(|opening| opening.session().clone());
        let Some(session) = session else {
            return Vec::new();
        };
        let text = match Self::missing_workspace(code.name(), &message) {
            Some(path) => {
                format!("Cannot resume: {path} no longer exists. Delete removes the session.")
            }
            None => message,
        };
        self.home.sessions.note(&session, text.clone());
        let attached = self.session() == Some(&session);
        self.notices.push(code, text);
        if attached {
            self.go_home();
        }
        Vec::new()
    }

    /// Folds one hub subscribe acknowledgement into the levels: an
    /// acceptance makes the tagged level the accepted one, a refusal
    /// becomes the row's note, and a refusal of the open's last
    /// subscribe fails the open. Some with the lines to send when `id`
    /// carried a `Subscribe` tag, else `None`. A session answer names
    /// its source session, which must be the session the tag names: an
    /// answer for another session changes nothing and leaves the tag, as
    /// an unknown id does. A hub subscribe answer has no source session
    /// and matches by tag.
    pub(super) fn subscribe_answer(
        &mut self,
        id: &str,
        accepted: bool,
        payload: &serde_json::Map<String, Value>,
        source: Option<&SessionId>,
    ) -> Option<Vec<String>> {
        let Some(Waiting::Subscribe { session, level }) = self.outbox.waiting(id) else {
            return None;
        };
        if source.is_some_and(|source| *source != session) {
            return None;
        }
        self.outbox.settled(id);
        if accepted {
            self.home.subs.accept(session.clone(), level);
        }
        if !accepted {
            let code = payload
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or_default();
            self.subscribe_refused(session, code, &refusal(payload));
        }
        if !accepted && self.is_ack(id) {
            let code = refusal_code(payload);
            return Some(self.fail_open(code, refusal(payload)));
        }
        Some(Vec::new())
    }

    /// Folds an accepted subscribe on `session`'s stream past the gate:
    /// a session held at `full` reconciles first, then the first stage's
    /// subscribe keeps the gate and makes the raise due, sending the
    /// `commands` line once the subscribe is accepted and never after a
    /// refusal, and the raise's subscribe ends the gate and its line
    /// goes on to `on_line`. Some with the lines to send, or `None`
    /// when the gate ended.
    pub(super) fn subscribe_accepted(
        &mut self,
        session: &SessionId,
        id: &str,
    ) -> Option<Vec<String>> {
        if self.home.subs.full(session) {
            self.reconcile(session);
        }
        if self.is_ack(id) {
            let due = self
                .home
                .subs
                .opening
                .as_ref()
                .is_some_and(|opening| matches!(opening, Opening::Summary { .. }));
            if due {
                if let Some(Opening::Summary { session, held, .. }) = self.home.subs.opening.take()
                {
                    let line = self.ask_commands(&session);
                    self.home.subs.opening = Some(Opening::Due { session, held });
                    return Some(vec![line]);
                }
                return Some(Vec::new());
            }
            // The raise's acceptance ends the gate: the full open
            // succeeded, so a stale close notice for this session ends
            // with it. A log still cut past here re-pushes
            // `stream_closed` when its replay reaches the cut.
            self.notices.clear_code(ErrorCode::StreamClosed, &session.0);
            self.home.subs.opening = None;
            return None;
        }
        Some(Vec::new())
    }

    /// The `full` raise due after the first frame drew: one `full`
    /// subscribe for the opening session, written after `draw` returns
    /// so no `full` line goes out before the first frame. The raise goes
    /// when the open is due with no read started, due for a held screen,
    /// or reading with the tail installed; a read open raises over the
    /// installed preview. When the attached session is not the opening
    /// one the open was left, so it drops and nothing goes out. Else
    /// `None`: no raise is due.
    pub(crate) fn take_raise(&mut self) -> Option<String> {
        let opening = self.home.subs.opening.as_ref();
        let due = opening.as_ref().and_then(|opening| match opening {
            Opening::Due { held, .. } => Some(*held),
            Opening::Summary { .. } | Opening::Reading { .. } | Opening::Raised { .. } => None,
        });
        let reading_done = opening.is_some_and(|opening| {
            matches!(opening, Opening::Reading { .. })
                && matches!(&self.overlay, crate::app::overlay::Overlay::Shown { session: shown, .. } if shown == opening.session())
        });
        let held = due.or(if reading_done { Some(false) } else { None })?;
        let session = self
            .home
            .subs
            .opening
            .as_ref()
            .map(|opening| opening.session().clone());
        let session = session?;
        if self.session() != Some(&session) {
            self.home.subs.opening = None;
            return None;
        }
        let (id, line) = subscribe_line(&session, SubscribeLevel::Full, None);
        self.home.subs.raise(id.clone());
        self.outbox.wait(
            &id,
            Waiting::Subscribe {
                session: session.clone(),
                level: SubscribeLevel::Full,
            },
        );
        if due.is_some() {
            let busy = matches!(&self.phase, Phase::Attached { busy: true, .. });
            self.overlay = crate::app::overlay::Overlay::Shown {
                session,
                busy: Some(busy),
                held,
            };
            self.attached_screen_mut().set_frozen(true);
        }
        Some(line)
    }

    /// Whether `id` answers the open's raise for `session`: the ack
    /// `pass_line` reads while the overlay is shown.
    pub(in crate::app) fn raise_ack(&self, session: &SessionId, id: &str) -> bool {
        self.home.subs.opening.as_ref().is_some_and(|opening| {
            matches!(opening, Opening::Raised { ack, .. } if ack == id)
                && opening.session() == session
        })
    }

    /// Moves a due open to the tail read and starts it: the loop reads
    /// the session's last lines before the raise. Some with the session
    /// to read when the move happened.
    pub(crate) fn tail_due(&mut self) -> Option<SessionId> {
        let attached = self.session().cloned();
        let session = self.home.subs.tail_due(attached.as_ref())?;
        self.overlay = crate::app::overlay::Overlay::Reading(crate::app::overlay::Tail::new());
        Some(session)
    }

    /// The session opening, if any.
    pub(crate) fn opening_session(&self) -> Option<SessionId> {
        self.home.subs.opening_session()
    }

    /// Whether `session` is opening: its lines fold into its row and are
    /// dropped until the raise's acknowledgement.
    pub(super) fn gated(&self, session: &SessionId) -> bool {
        self.home
            .subs
            .opening
            .as_ref()
            .is_some_and(|opening| opening.session() == session)
    }
}
