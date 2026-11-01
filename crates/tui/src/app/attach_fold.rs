//! The attach fold (`docs/tui.md`, "History and paging"): the tail
//! preview the open rebuilds while reading, the post-frame count, the
//! install that swaps the preview into the screen, the fold-pass state
//! transitions with their landing, and the pass boundaries that open
//! and close them.

use contract::events::{CommandAccepted, CommandResult};
use contract::{Envelope, Seq, SessionId};
use serde_json::Value;

use super::notices::Notices;
use super::overlay::Overlay;
use super::{App, Pass, overlay, read};
use crate::link::Line;
use crate::window::Pages;

impl App {
    /// Whether any full stream is still folding its opening pass.
    pub(crate) fn pass_open(&self) -> bool {
        !self.passes.is_empty()
    }

    /// Whether the session's pass still waits for its boundary.
    pub(super) fn until_waited(&self, session: &SessionId) -> bool {
        self.passes
            .iter()
            .any(|(held, pass)| held == session && matches!(pass, Pass::Until(_)))
    }

    /// Rebuilds the pre-install candidate from fresh pages and replaces
    /// the prior candidate. A rebuild never starts from the prior
    /// candidate; only the newest candidate is installed. It runs only
    /// while the tail is reading; the post-frame count phase never
    /// rebuilds the displayed preview.
    fn fold_preview(&mut self, bounded: bool) {
        let lines = if let Overlay::Reading(tail) = &self.overlay {
            tail.turn_lines()
        } else {
            return;
        };
        let (fresh, busy) = if let Some(lines) = lines {
            let height = self.conversation_height();
            let own_session = bounded
                && lines.first().is_some_and(|first| {
                    lines.iter().all(|line| line.session_id == first.session_id)
                });
            // Whole turns fold from their start; older own-session turns
            // are added only when the newest turns do not fill the view.
            let mut start = if own_session {
                lines
                    .iter()
                    .rposition(|line| line.kind == "turn_started")
                    .unwrap_or(0)
            } else {
                0
            };
            loop {
                let mut fresh = self.attached_screen().pages().fresh();
                fresh.set_image_ids(self.images.id_allocator.clone());
                let mut busy = None;
                // The candidate is not drawn until installation, so its
                // open-page recount waits for the batch's end and counts once.
                fresh.hold();
                for line in lines.get(start..).unwrap_or(&[]) {
                    let applied = fresh.apply(line);
                    if applied.busy.is_some() {
                        busy = applied.busy;
                    }
                }
                fresh.flush();
                if !own_session || fresh.index().total() >= height || start == 0 {
                    break (fresh, busy);
                }
                start = lines
                    .get(..start)
                    .unwrap_or(&[])
                    .iter()
                    .rposition(|line| line.kind == "turn_started")
                    .unwrap_or(0);
            }
        } else {
            let mut fresh = self.attached_screen().pages().fresh();
            fresh.set_image_ids(self.images.id_allocator.clone());
            (fresh, None)
        };
        self.preview = Some(overlay::Preview { pages: fresh, busy });
    }

    /// The args of the open's next tail read: none when the walk
    /// reached the chain root or ended, when no read runs, or when the
    /// display filled on the held run's own segment. Past a crossed seam
    /// the read keeps going for the joint total: a `seq 0` with an
    /// unfollowed parent still asks once an ancestor is held. The pass
    /// counts the lines above a filled display, so the open holds only
    /// the filled run plus each seam crossing.
    pub(crate) fn tail_next(&mut self) -> Option<Value> {
        if self.preview.is_none() {
            self.fold_preview(true);
        }
        let Overlay::Reading(tail) = &self.overlay else {
            return None;
        };
        if tail.chain_done() {
            return None;
        }
        let preview = self.preview.as_ref()?;
        let height = self.conversation_height();
        // Rows past the display stop pre-frame reads; merely reaching
        // the segment's start does not. A run that reaches `seq 0`
        // short of the display crosses the seam before the first frame.
        let full = tail
            .turn_lines()
            .is_some_and(|_| preview.pages.index().total() >= height);
        // A fill on the own segment stops pre-frame reads even with a
        // parent still to read: older windows and ancestors belong to
        // the post-frame count. A fill past a crossed seam keeps going
        // to the chain root, as the seam needs the joint total to draw.
        let past_seam = tail.seam_pending() && tail.crossed_the_seam();
        if full && !past_seam {
            return None;
        }
        Some(tail.next_args())
    }

    /// Folds one tail answer into the open's read: an answer that
    /// leaves a gap, or a line carrying no seq, ends the tail as failed
    /// with the out-of-order notice. A valid answer rebuilds the
    /// pre-install candidate from fresh pages, replacing the old one.
    pub(crate) fn tail_lines(&mut self, lines: Vec<Envelope>) {
        {
            let Overlay::Reading(tail) = &mut self.overlay else {
                return;
            };
            if tail.prepend(lines).is_err() {
                tail.end();
                self.notices.push(
                    "io_failed",
                    "Could not load history: the answer was out of order".to_owned(),
                );
                return;
            }
        }
        self.fold_preview(true);
    }

    /// Ends one tail read over its cursor: failed with the notice,
    /// abandoned or lost without one. Both the pre-frame tail and the
    /// post-frame count share it, so neither duplicates the end.
    fn end_read(tail: Option<&mut overlay::Tail>, notice: Option<&str>, notices: &mut Notices) {
        if let Some(tail) = tail {
            tail.end();
        }
        if let Some(notice) = notice {
            notices.push("io_failed", format!("Could not load history: {notice}"));
        }
    }

    /// Ends the open's tail read: failed with the notice, abandoned or
    /// lost without one.
    pub(crate) fn tail_ended(&mut self, notice: Option<&str>) {
        let tail = if let Overlay::Reading(tail) = &mut self.overlay {
            Some(tail)
        } else {
            None
        };
        Self::end_read(tail, notice, &mut self.notices);
        if notice.is_some() {
            self.preview = None;
            self.fold_preview(false);
        }
    }

    /// Installs the newest candidate built while reading the open's
    /// tail, without folding it again. An empty log installs nothing
    /// and shows no history. The screen follows, the overlay shows the
    /// preview, and the screen freezes for the raise. The tail stays
    /// behind as the post-frame count's cursor: older own-segment
    /// windows and ancestors read through it without refolding the
    /// preview.
    pub(crate) fn install_tail(&mut self) {
        if self.preview.is_none() {
            self.fold_preview(true);
        }
        let Overlay::Reading(tail) = std::mem::replace(&mut self.overlay, Overlay::None) else {
            return;
        };
        let Some(session) = self.opening_session() else {
            return;
        };
        let Some(preview) = self.preview.take() else {
            self.overlay = Overlay::Reading(tail);
            return;
        };
        *self.attached_screen_mut().pages_mut() = preview.pages;
        self.attached_screen_mut().follow();
        // The candidate's oldest folded line bounds the count: lines at
        // or after it already show, so the count folds only older ones.
        // An empty candidate leaves no floor, so the count keeps its
        // old behavior.
        self.installed_floor = tail
            .turn_lines()
            .and_then(|lines| lines.first())
            .and_then(|line| line.seq.map(|seq| (line.session_id.clone(), seq)));
        self.count = Some(tail);
        self.overlay = Overlay::Shown {
            session,
            busy: preview.busy,
            held: false,
        };
        self.attached_screen_mut().set_frozen(true);
    }

    /// Drops a fresh tail read's leftover preview and count cursor:
    /// the next read folds its own. The loop calls it when the tail
    /// read starts, so a new open never installs or counts the old run.
    pub(crate) fn clear_tail_read(&mut self) {
        self.preview = None;
        self.count = None;
        self.installed_floor = None;
    }

    /// The args of the post-frame count's next read: none once the walk
    /// reached the chain root or ended, or when no count runs. Never
    /// folds: the count folds once at its end, above the preview.
    pub(crate) fn tail_count_next(&self) -> Option<Value> {
        let tail = self.count.as_ref()?;
        if tail.chain_done() {
            return None;
        }
        Some(tail.next_args())
    }

    /// Folds one count answer into the retained read: an answer that
    /// leaves a gap, or a line carrying no seq, ends the count as
    /// failed with the out-of-order notice, keeping the preview. The
    /// preview itself never refolds.
    pub(crate) fn tail_count_lines(&mut self, lines: Vec<Envelope>) {
        let Some(tail) = self.count.as_mut() else {
            return;
        };
        if tail.prepend(lines).is_err() {
            tail.end();
            self.notices.push(
                "io_failed",
                "Could not load history: the answer was out of order".to_owned(),
            );
        }
    }

    /// Ends the retained count read: failed with the notice, abandoned
    /// or lost without one, past the first frame.
    pub(crate) fn tail_count_ended(&mut self, notice: Option<&str>) {
        Self::end_read(self.count.as_mut(), notice, &mut self.notices);
    }

    /// Ends the post-frame count: folds the ancestor prefix once with
    /// the line following it as its trailer and carries its runs above
    /// the installed preview. The preview itself never refolds: the
    /// trailer closes the count fold so the ancestor run carries
    /// instead of staying open behind it.
    pub(crate) fn finish_count(&mut self) {
        let Some(tail) = self.count.take() else {
            return;
        };
        if !matches!(&self.overlay, Overlay::Shown { .. }) {
            self.count = Some(tail);
            return;
        }
        let Some(opening) = self.opening_session() else {
            self.count = Some(tail);
            return;
        };
        let mut grown = self.attached_screen().pages().fresh();
        grown.set_image_ids(self.images.id_allocator.clone());
        let (prefix, trailer) = tail.count_parts(&opening, self.installed_floor.as_ref());
        for line in prefix {
            grown.apply(line);
        }
        if let Some(trailer) = trailer {
            grown.apply(trailer);
        }
        grown.carry_chain(self.attached_screen_mut().pages_mut());
    }

    /// Starts the pass on the raise's ack: the preview stays on the
    /// screen and the session's lines fold into a fresh second `Pages`,
    /// held while a batch is open.
    pub(super) fn begin_fold(&mut self, session: &SessionId, ack: &str) {
        let Overlay::Shown {
            session: shown,
            busy,
            held,
        } = &self.overlay
        else {
            return;
        };
        if shown != session || !self.raise_ack(session, ack) {
            return;
        }
        let busy = *busy;
        let held = *held;
        let fold = self.fresh_fold();
        self.overlay = Overlay::Folding {
            session: session.clone(),
            fold,
            busy,
            held,
            landing: false,
        };
    }

    /// A fresh second `Pages` for the pass, held while a batch is open.
    fn fresh_fold(&self) -> Box<Pages> {
        let mut fold = self.attached_screen().pages().fresh();
        fold.set_image_ids(self.images.id_allocator.clone());
        if self.screen.pages().holding() {
            fold.hold();
        }
        Box::new(fold)
    }

    /// The preview's last folded `seq`, if any line was held.
    fn preview_last(&self) -> Option<Seq> {
        self.attached_screen()
            .pages()
            .index()
            .pages()
            .iter()
            .filter(|page| page.lines > 0)
            .map(|page| page.last_seq)
            .max()
    }

    /// The pages the session's line folds into: the overlay's second
    /// `Pages` while its pass runs, else the attached screen's. The
    /// guard and the reborrow are one shape test split for the borrow
    /// checker: no `folding` check, no `unreachable`.
    pub(super) fn fold_pages(&mut self, session: &SessionId) -> &mut Pages {
        if let Overlay::Folding {
            session: folding,
            fold,
            ..
        } = &mut self.overlay
            && folding == session
        {
            return fold;
        }
        if let Some(open) = self.items.open.as_mut()
            && let Some(stashed) = open.stashed.as_mut()
        {
            return stashed.pages_mut();
        }
        self.screen.pages_mut()
    }

    /// Marks the folding pass to land at its boundary: the next line
    /// ending the pass swaps the fold in at the held anchor.
    fn mark_landing(&mut self) {
        if let Overlay::Folding { landing, .. } = &mut self.overlay {
            *landing = true;
        }
    }

    /// Lands the pass whose boundary the last line reached: when the
    /// overlay still folds for the attached session its fold swaps in,
    /// else the overlay ends and drops it.
    pub(super) fn land_pass(&mut self) {
        let taken = std::mem::replace(&mut self.overlay, Overlay::None);
        let Overlay::Folding {
            session,
            fold,
            busy,
            held,
            ..
        } = taken
        else {
            self.end_overlay();
            return;
        };
        if self.session() != Some(&session) {
            self.end_overlay();
            return;
        }
        let fresh = self.fresh_fold();
        self.overlay = Overlay::Folding {
            session,
            fold: fresh,
            busy,
            held,
            landing: false,
        };
        self.land_inner(*fold, held);
    }

    /// Swaps the pass's second `Pages` in at its boundary: the preview's
    /// opens carry, focus remaps to the same call or clears, the
    /// selection clears, the kept busy applies, and the overlay ends. A
    /// held landing keeps the preview's place: the top row's segment
    /// session with its own `seq` and its offset among that line's rows
    /// are read before the swap, and after the unfreeze applies any
    /// deferred width the top is the fold's row with that session and
    /// `seq`, at that offset kept within its rows, or the end when the
    /// fold lacks it. A dropped page keeps the old width's rows until
    /// it loads, so the pages hold the anchor and each load finds the
    /// top again.
    #[cfg(test)]
    pub(super) fn land(&mut self, fold: Pages, held: bool) {
        self.land_inner(fold, held);
        self.settle();
    }

    /// Handles the route without settling; the caller owns the layout pass.
    fn land_inner(&mut self, mut fold: Pages, held: bool) {
        fold.ext.cache.promote();
        // The preview's closed ancestor runs land in the fold front
        // beside the carried opens, under their own sessions, before the
        // swap; the fold still replays the own log only.
        self.attached_screen().pages().carry_chain(&mut fold);
        self.attached_screen().pages().carry_opens(&mut fold);
        let focus = self.focus.take();
        self.focus = focus.and_then(|id| self.remap_focus(id, &fold));
        self.clear_selection();
        let anchor = held.then(|| self.top_anchor()).flatten();
        let busy = match &self.overlay {
            Overlay::Folding { busy, .. } => *busy,
            Overlay::None | Overlay::Shown { .. } | Overlay::Kept { .. } | Overlay::Reading(_) => {
                None
            }
        };
        std::mem::swap(self.attached_screen_mut().pages_mut(), &mut fold);
        self.end_overlay();
        if let Some((session, seq, offset)) = anchor {
            let pages = self.attached_screen_mut().pages_mut();
            pages.hold_anchor(&session, seq, offset);
            match pages.anchored_row() {
                Some(row) => {
                    self.screen.jump_kept(row);
                }
                None => self.attached_screen_mut().follow(),
            }
        }
        if let Some(busy) = busy {
            self.set_busy(busy);
        }
    }

    /// The preview's top row segment session with its `seq` and the
    /// row's offset among that line's rows: the anchor a held landing
    /// keeps. None while following or when the page holds no line.
    fn top_anchor(&self) -> Option<(SessionId, Seq, usize)> {
        let top = self.attached_screen().top()?;
        self.attached_screen().pages().row_seq(top)
    }

    /// Remaps focus across the swap: a line target follows its call by
    /// stable identity, a row link clears, and the rest stay.
    fn remap_focus(
        &self,
        id: crate::mouse::TargetId,
        fold: &Pages,
    ) -> Option<crate::mouse::TargetId> {
        use crate::mouse::TargetId;
        if let TargetId::Line(target) = id {
            return self
                .attached_screen()
                .pages()
                .same_target(target, fold)
                .map(TargetId::Line);
        }
        if matches!(id, TargetId::Link { .. }) {
            return None;
        }
        Some(id)
    }

    /// Whether the scroll bar draws its thumb: the counts are exact
    /// while the screen is not frozen, except on the attached screen
    /// while the overlay keeps the preview.
    pub(crate) fn counts_exact(&self) -> bool {
        if self.screen.frozen() {
            return false;
        }
        if matches!(self.overlay, Overlay::Kept { .. })
            && self
                .items
                .open
                .as_ref()
                .is_none_or(|open| open.stashed.is_none())
        {
            return false;
        }
        true
    }

    /// Holds page requests while an earlier stash waits to fold a reopening.
    pub(crate) fn reopen_pending(&mut self, session: SessionId) {
        self.passes.retain(|(held, _)| held != &session);
        self.passes.push((session, Pass::Pending));
    }

    /// Discards the late answer of a history wait ended by a reopening.
    pub(crate) fn abandon_history(&mut self, id: String) {
        self.abandoned_history.insert(id);
    }

    /// Updates pass boundaries before any view consumes the line. Returns
    /// true only for an abandoned answer, which must fold nowhere.
    pub(super) fn pass_line(&mut self, line: &Line) -> bool {
        match line {
            Line::Session(line) => {
                if matches!(line.kind.as_str(), "command_accepted" | "command_rejected")
                    && let Some(id) = line.payload.get("command_id").and_then(Value::as_str)
                    && self.abandoned_history.remove(id)
                {
                    return true;
                }
                let waited = line
                    .seq
                    .is_some_and(|_| self.until_waited(&line.session_id));
                if let Some(seq) = line.seq {
                    self.passes.retain(|(session, pass)| match pass {
                        Pass::Pending => true,
                        Pass::Until(last) => session != &line.session_id || seq < *last,
                    });
                }
                if waited && !self.until_waited(&line.session_id) && self.folding(&line.session_id)
                {
                    self.mark_landing();
                }
                if line.kind == "command_accepted"
                    && let Some(accepted) = read!(line, CommandAccepted)
                {
                    if let Some(CommandResult::Subscribe { last_seq }) = accepted.result {
                        self.passes
                            .retain(|(session, _)| session != &line.session_id);
                        self.passes
                            .push((line.session_id.clone(), Pass::Until(last_seq)));
                        if let Some(id) = line.payload.get("command_id").and_then(Value::as_str) {
                            self.begin_fold(&line.session_id, id);
                        }
                    } else if let Some(id) = line.payload.get("command_id").and_then(Value::as_str)
                        && self.raise_ack(&line.session_id, id)
                    {
                        self.end_overlay();
                    }
                }
            }
            Line::Hub(line) => {
                let Some(session) = line.payload.get("session_id").and_then(Value::as_str) else {
                    return false;
                };
                let session = SessionId(session.to_owned());
                if matches!(
                    line.kind.as_str(),
                    "stream_reopened" | "stream_closed" | "session_left"
                ) {
                    self.reviewer_setup_stream_lost(&session);
                }
                match line.kind.as_str() {
                    "stream_reopened" => {
                        // The cause ended for this session: its notice goes.
                        self.notices.clear_code("stream_closed", &session.0);
                        self.passes.retain(|(held, _)| held != &session);
                        if let Some(last) = line.payload.get("last_seq").and_then(Value::as_u64) {
                            self.passes.push((session.clone(), Pass::Until(Seq(last))));
                        }
                        match std::mem::replace(&mut self.overlay, Overlay::None) {
                            Overlay::Folding {
                                session: folding,
                                busy,
                                held,
                                ..
                            } if folding == session => {
                                let fold = self.fresh_fold();
                                self.overlay = Overlay::Folding {
                                    session,
                                    fold,
                                    busy,
                                    held,
                                    landing: false,
                                };
                            }
                            rest @ (Overlay::None
                            | Overlay::Shown { .. }
                            | Overlay::Folding { .. }
                            | Overlay::Kept { .. }
                            | Overlay::Reading(_)) => {
                                self.overlay = rest;
                            }
                        }
                    }
                    "stream_closed" => {
                        self.passes.retain(|(held, pass)| {
                            held != &session || matches!(pass, Pass::Pending)
                        });
                        if matches!(&self.overlay, Overlay::Shown { session: shown, .. } if shown == &session)
                        {
                            self.end_overlay();
                        } else {
                            self.keep_pass(&session);
                        }
                    }
                    "session_left" => {
                        self.passes.retain(|(held, pass)| {
                            held != &session || matches!(pass, Pass::Pending)
                        });
                        if matches!(&self.overlay, Overlay::Shown { session: shown, .. } if shown == &session)
                        {
                            self.end_overlay();
                        } else {
                            self.keep_pass(&session);
                        }
                    }
                    _ => {}
                }
            }
        }
        false
    }

    /// Keeps the preview when the pass's stream ends without its
    /// boundary, through `stream_closed` or `session_left` alike: the
    /// preview stays, unfrozen, and replays skip to its last line. With
    /// no preview the pass's lines are the only copy, so they land as
    /// the screen instead of dropping with the fold.
    fn keep_pass(&mut self, session: &SessionId) {
        let last = self.preview_last();
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Folding {
                session: folding,
                fold,
                busy,
                held,
                ..
            } if &folding == session => {
                if last.is_none() {
                    // The tail read failed and installed no preview, so
                    // the pass's lines are the only copy: land them as
                    // the screen instead of dropping them with the fold.
                    let fresh = self.fresh_fold();
                    self.overlay = Overlay::Folding {
                        session: session.clone(),
                        fold: fresh,
                        busy,
                        held,
                        landing: false,
                    };
                    self.land_inner(*fold, held);
                }
                self.overlay = Overlay::Kept { last };
                self.attached_screen_mut().set_frozen(false);
            }
            rest @ (Overlay::None
            | Overlay::Shown { .. }
            | Overlay::Folding { .. }
            | Overlay::Kept { .. }
            | Overlay::Reading(_)) => {
                self.overlay = rest;
            }
        }
    }
}
