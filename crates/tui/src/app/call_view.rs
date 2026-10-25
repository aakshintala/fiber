//! A tool call's swapped view (`docs/tui.md`, "Swapped views"): the whole
//! call as drawn inline with no 20-line cap, its full output paged with
//! `artifact_page`, the model's-view markers from `kept`, `y` copying the
//! whole call, and search scoped to the view.
//!
//! The view never holds the whole file for drawing: it keeps at most
//! three pages around the visible page (the visible page plus one on
//! each side) and refetches a dropped page by its byte `offset` when
//! scrolled back to. A running `y` copy holds every fetched page; when
//! it lands whole or aborts, the bounded window applies again. A failed
//! page keeps the held pages with a notice; scrolling to it asks again,
//! and each marker starts a line at or past its `kept` edge.
//!
//! The state lives here; `call_view_pages` owns the fetched pages and
//! fetch queue, and `call_view_rows` builds the drawn rows from them.

use std::collections::BTreeSet;

use serde_json::json;

use super::call_view_pages::{Page, Pager, Seg};
use super::call_view_rows::{
    Row, RowsInput, Source, build_frame, build_rows, highlight_rows, row_text, shown_height, window,
};
use super::{App, Effect, Target, mint, session_command};
use crate::keys::Key;
use crate::mouse::TargetId;
use crate::swapped::{Frame, List, Spot};
use crate::turn::opened::{self, CallSnapshot};
use contract::SessionId;
use contract::events::{CommandAccepted, CommandRejected, CommandResult};

/// A `y` copy waiting on its pages, in file order: while it runs the
/// draw window holds every fetched page; when it lands whole or
/// aborts, the bounded window applies again.
#[derive(Debug)]
struct CopyReq;

/// A call's swapped view: the call's identity and snapshot, its held
/// pages, and its scroll, copy and search state.
#[derive(Debug)]
pub(crate) struct CallView {
    /// The call's id: Esc returns with its row still focused.
    call: usize,
    /// The capitalised kind word, drawn bold on the top row.
    title: String,
    /// The call's target, drawn dim on the top row.
    command: String,
    /// The headline and shell command rows, drawn before the output.
    prefix: Vec<opened::Body>,
    /// The shell or search closing row, drawn after the output.
    closing: Vec<opened::Body>,
    /// The first output line's number, for numbered output.
    numbering: Option<u64>,
    /// The whole call's uncapped rows, drawn when no artifact pages it.
    plain: Vec<opened::Body>,
    /// The call's output text, as the tool returned it: what `y`
    /// copies for a result with no artifact.
    copy_text: String,
    /// Whether the result carries an artifact to page.
    artifact: Option<String>,
    /// The model's-view edges, when the result carries them.
    kept: Option<Vec<[u64; 2]>>,
    /// The session the view opened on: answers from another go nowhere.
    session: SessionId,
    /// The fetched pages and fetch queue.
    pager: Pager,
    /// The failure notice, naming the error code.
    notice: Option<String>,
    /// The scrolled-to row and the first row shown.
    list: List,
    /// Whether new pages pull the view to the bottom, as the
    /// conversation follows new output until the person scrolls up.
    follow: bool,
    /// A `y` copy waiting on its pages, if one runs: while it is
    /// present the draw window evicts nothing.
    copy: Option<CopyReq>,
    /// The query the matches below were built for.
    query: String,
    /// The body rows holding the query, in order.
    matches: Vec<usize>,
    /// The current match's index into `matches`.
    current: Option<usize>,
}

impl CallView {
    /// Snapshots `call` for the view: its kind, target, prefix, closing
    /// and numbering from the per-kind renderer, the uncapped rows for
    /// a result with no artifact, and the paging inputs from its
    /// result. A call with no result yet shows its empty rows.
    fn open(call: CallSnapshot, session: SessionId) -> Self {
        let pager = Pager::new(call.artifact.is_some());
        Self {
            call: call.id,
            title: call.title,
            command: call.command,
            prefix: call.prefix,
            closing: call.closing,
            numbering: call.numbering,
            plain: call.plain,
            copy_text: call.copy,
            artifact: call.artifact,
            kept: call.kept,
            session,
            pager,
            notice: None,
            list: List::default(),
            follow: true,
            copy: None,
            query: String::new(),
            matches: Vec::new(),
            current: None,
        }
    }

    /// Whether the result pages an artifact.
    fn paged(&self) -> bool {
        self.artifact.is_some()
    }
}

impl CallView {
    /// What [`build_rows`](super::call_view_rows::build_rows) draws:
    /// the snapshot rows, the paging inputs, and the held pages.
    fn rows_input(&self) -> RowsInput<'_> {
        RowsInput {
            prefix: &self.prefix,
            plain: &self.plain,
            closing: &self.closing,
            numbering: self.numbering,
            paged: self.paged(),
            kept: self.kept.as_ref(),
            size: self.pager.size,
            complete: self.pager.complete(),
            pages: &self.pager.pages,
            segs: &self.pager.segs,
        }
    }

    /// The view's body rows: prefix rows, held output lines with marker
    /// rows at their anchors, then closing rows. Rows from a result
    /// with no artifact are its uncapped rows whole.
    fn rows(&self) -> Vec<Row> {
        build_rows(&self.rows_input())
    }

    /// The view's frame: the capitalised kind bold and its target dim
    /// on the top row, the body rows, and "y copies the whole call" dim
    /// on the last row, with the failure notice above it when a page
    /// failed to load.
    pub(in crate::app) fn frame(&self) -> Frame {
        let mut rows = self.rows();
        highlight_rows(&mut rows, &self.query, self.current, &self.matches);
        build_frame(
            &self.title,
            &self.command,
            rows,
            self.list,
            self.notice.iter().cloned().collect(),
        )
    }
}

impl CallView {
    /// Asks for `offset` next, unless its page is held, already queued,
    /// or on the wire.
    fn queue(&mut self, offset: u64) {
        self.pager.queue(offset);
    }

    /// Queues what the draw window wants: the visible pages, one page
    /// on each side, and the next fill page while following, then drops
    /// pages past the held window.
    fn queue_wanted(&mut self, height: usize) {
        if !self.paged() {
            return;
        }
        let rows = self.rows();
        let (top, shown) = window(&rows, &self.list, self.below_len(), height);
        let mut wanted = BTreeSet::new();
        for row in rows.iter().skip(top).take(shown) {
            if let Source::Page { offset, .. } = row.source {
                wanted.insert(offset);
            }
        }
        for offset in wanted.clone() {
            let (previous, next) = self.pager.neighbors(offset);
            wanted.extend(previous.into_iter().chain(next));
        }
        if self.follow
            && let Some(next) = self.pager.fill_next
        {
            wanted.insert(next);
        }
        if self.copy.is_some() {
            wanted.extend(self.pager.chain_prefix());
            if let Some(next) = self.pager.fill_next {
                wanted.insert(next);
            }
        }
        let mut wanted: Vec<u64> = wanted.into_iter().collect();
        wanted.sort();
        for offset in wanted {
            self.queue(offset);
        }
        self.evict(&rows, top, shown);
    }

    /// How many body rows the notice leaves out of `height`.
    fn below_len(&self) -> usize {
        self.notice.iter().len()
    }

    /// Drops held pages past the window around the visible rows: the
    /// visible pages plus one on each side stay, everything else
    /// pages out and refetches by byte offset when scrolled back to.
    /// Segments stay, so markers and the fill chain never lose their
    /// extents. While a `y` copy runs nothing evicts, so the copy's
    /// pages stay whatever the window shows.
    fn evict(&mut self, rows: &[Row], top: usize, shown: usize) {
        if self.copy.is_some() {
            return;
        }
        let top_source = rows.get(top).map(|row| row.source);
        let selected_source = rows.get(self.list.selected()).map(|row| row.source);
        let order: Vec<u64> = self.pager.segs.keys().copied().collect();
        if order.is_empty() {
            return;
        }
        let mut visible: Vec<usize> = rows
            .iter()
            .skip(top)
            .take(shown)
            .filter_map(|row| match row.source {
                Source::Page { offset, .. } => order.iter().position(|at| *at == offset),
                Source::Prefix(_) | Source::Closing(_) | Source::Marker { .. } => None,
            })
            .collect();
        if visible.is_empty() {
            return;
        }
        visible.sort();
        let low = visible.first().copied().unwrap_or(0).saturating_sub(1);
        let high = visible.last().copied().unwrap_or(0).saturating_add(1);
        let keep: BTreeSet<u64> = order
            .into_iter()
            .enumerate()
            .filter(|(index, _)| *index >= low && *index <= high)
            .map(|(_, offset)| offset)
            .collect();
        self.pager.pages.retain(|offset, _| keep.contains(offset));
        let rows = self.rows();
        let position = |source: Option<Source>| {
            source.and_then(|source| rows.iter().position(|row| row.source == source))
        };
        self.list.rebase(
            position(top_source).unwrap_or(top.min(rows.len().saturating_sub(1))),
            position(selected_source)
                .unwrap_or_else(|| self.list.selected().min(rows.len().saturating_sub(1))),
            rows.len(),
            shown,
        );
    }

    /// Lands a fetched page: holds its text, records its extent, pulls
    /// a following view to the bottom, and drops pages past the window
    /// unless a copy runs, which holds them all.
    fn land(
        &mut self,
        offset: u64,
        text: String,
        lines: Vec<u64>,
        next: Option<u64>,
        size: u64,
        height: usize,
    ) {
        let ends_newline = text.ends_with('\n');
        self.pager.size = self.pager.size.max(size);
        self.pager.segs.insert(
            offset,
            Seg {
                lines,
                next,
                ends_newline,
            },
        );
        self.pager.pages.insert(offset, Page { text });
        self.pager.missing.remove(&offset);
        if offset == self.pager.fill_next.unwrap_or(u64::MAX) {
            self.pager.fill_next = next;
        }
        self.notice = None;
        if self.follow {
            let rows = self.rows();
            let len = rows.len();
            self.list.select(
                len.saturating_sub(1),
                len,
                shown_height(height, self.below_len()),
            );
        }
        let rows = self.rows();
        let (top, shown) = window(&rows, &self.list, self.below_len(), height);
        self.evict(&rows, top, shown);
    }

    /// Fails a fetch: the offset stays missing until scrolled to, the
    /// view keeps the pages it holds with a notice naming the error
    /// code, and a running copy aborts without writing partial text.
    /// True when a running copy aborted, so the caller reapplies the
    /// bounded window it had held open.
    fn fail(&mut self, offset: u64, code: &str) -> bool {
        self.pager.missing.insert(offset);
        let aborted = self.copy.take().is_some();
        self.notice = Some(self.failure_notice(offset, code));
        aborted
    }

    /// The failure notice: the offset that failed and the error code.
    fn failure_notice(&self, offset: u64, code: &str) -> String {
        format!("The call's output page at byte {offset} could not load: {code}.")
    }

    /// The whole call's text for a result with no artifact: the
    /// output as the tool returned it, with no gutter, line numbers,
    /// headline, shell command or closing row.
    fn plain_text(&self) -> String {
        self.copy_text.clone()
    }
}

impl App {
    /// Whether the call's view is open.
    pub(crate) fn call_view_open(&self) -> bool {
        self.call_view().is_some()
    }

    /// Opens the call's view: Enter on its opened row or its more row,
    /// or a click on the more row. The call stays open behind the view;
    /// Esc returns with its row still focused.
    pub(in crate::app) fn open_call_view(&mut self, id: usize) -> Effect {
        let Some(session) = self.session().cloned() else {
            return Effect::None;
        };
        let Some(call) = self.find_call(id) else {
            return Effect::None;
        };
        let mut view = CallView::open(call, session);
        if !self.connected() {
            view.notice = Some("The call's output could not load: not connected.".to_owned());
        }
        self.set_call_view(view);
        self.settle();
        self.call_view_pump_effect()
    }

    /// The call `id` names on the attached screen, snapshotted for its
    /// view, if it drew.
    fn find_call(&self, id: usize) -> Option<CallSnapshot> {
        let calls = self
            .screen
            .pages()
            .newest_first()
            .flatten()
            .flat_map(|turns| turns.iter())
            .flat_map(|turn| turn.groups())
            .flat_map(|group| group.calls());
        opened::snapshot(calls, id)
    }

    /// Whether the call `id` names is open inline.
    pub(in crate::app) fn call_open(&self, id: usize) -> bool {
        self.find_call(id).is_some_and(|call| call.open)
    }

    /// Handles a click on the open call view: its top row closes it,
    /// and a body row selects it.
    pub(in crate::app) fn call_view_click(&mut self, spot: &Spot) -> Effect {
        let height = self.conversation_height();
        let Some(view) = self.call_view_mut() else {
            return Effect::None;
        };
        if let Spot::Row(at) = spot {
            let len = view.rows().len();
            view.list.select(*at, len, height.max(1));
        }
        self.settle();
        Effect::None
    }

    /// Hands `key` to the open call view. `None` with no view open, and
    /// for Ctrl+C.
    pub(in crate::app) fn call_view_key(&mut self, key: &Key) -> Option<Effect> {
        if self.call_view_mut().is_none() || *key == Key::CtrlC {
            return None;
        }
        if *key == Key::Esc && self.find_open() {
            return self.find_key(key);
        }
        if self.find_open() && matches!(key, Key::Char(_) | Key::Backspace) {
            return self.find_key(key);
        }
        match key {
            Key::Esc => {
                self.close_call_view();
                Some(Effect::None)
            }
            // With the find bar open the arrows move between the view's
            // matches, wrapping at either end, as Enter does.
            Key::Enter | Key::Up | Key::Down if self.find_open() => {
                self.step_call_view_match(key);
                Some(Effect::None)
            }
            // Ctrl+F reaches the find bar, opening it or typing through
            // it; a second Ctrl+F stays swallowed there, so the results
            // view never leaks conversation matches over the view.
            Key::CtrlF => None,
            Key::Up => Some(self.scroll_call_view(true, false)),
            Key::Down => Some(self.scroll_call_view(false, false)),
            Key::PageUp => Some(self.scroll_call_view(true, true)),
            Key::PageDown => Some(self.scroll_call_view(false, true)),
            Key::Char('y') => Some(self.start_call_copy()),
            // Every other key is swallowed: the conversation behind the
            // view takes no input while it is open.
            Key::Char(_)
            | Key::Backspace
            | Key::Enter
            | Key::CtrlC
            | Key::CtrlO
            | Key::End
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlG
            | Key::CtrlR
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::CtrlV
            | Key::CtrlL => Some(Effect::None),
        }
    }

    /// Closes the view, focusing its more row or, for short calls, the call row.
    pub(in crate::app) fn close_call_view(&mut self) {
        let call = self
            .call_view_mut()
            .map(|view| (view.call, view.plain.len() > opened::OPENED_LINES));
        self.close_session_view();
        if let Some((call, more)) = call {
            self.focus = Some(TargetId::Line(if more {
                Target::CallView(call)
            } else {
                Target::Call(call)
            }));
        }
        self.settle();
    }

    /// Scrolls the open view by one row or one page, retrying visible
    /// missing pages and dropping pages past the held window, then
    /// pumps the fetch queue.
    fn scroll_call_view(&mut self, up: bool, page: bool) -> Effect {
        let height = self.conversation_height();
        let key = if page {
            if up { Key::PageUp } else { Key::PageDown }
        } else if up {
            Key::Up
        } else {
            Key::Down
        };
        {
            let Some(view) = self.call_view_mut() else {
                return Effect::None;
            };
            let rows = view.rows();
            let len = rows.len();
            let shown = shown_height(height, view.below_len());
            let (top, _) = window(&rows, &view.list, view.below_len(), height);
            let before = view.list.selected();
            view.list.key(&key, len, shown);
            if view.list.selected() != before {
                view.follow = !up && view.list.selected() + 1 >= len;
            } else if !up {
                view.follow = true;
            }
            if up {
                view.follow = false;
            }
            // Scrolling a missing page into the window asks again, as
            // does scrolling down to the end it stalls.
            for row in rows.iter().skip(top).take(shown) {
                if let Source::Page { offset, .. } = row.source
                    && view.pager.missing.remove(&offset)
                {
                    view.queue(offset);
                }
            }
            let at_bottom = !rows.is_empty() && view.list.selected() + 1 >= rows.len();
            if !up
                && at_bottom
                && let Some(next) = view.pager.fill_next
                && view.pager.missing.remove(&next)
            {
                view.queue(next);
            }
        }
        self.settle();
        self.call_view_pump_effect()
    }

    /// Fetches the view's queued offsets, one request on the wire, and
    /// the next fill page while following.
    fn call_view_pump_effect(&mut self) -> Effect {
        match self.call_view_pump() {
            Some(line) => Effect::Send(vec![line]),
            None => Effect::None,
        }
    }

    /// Queues what the window wants and sends the next request, if any.
    /// `None` with nothing to send or no connection.
    fn call_view_pump(&mut self) -> Option<String> {
        if !self.connected() {
            return None;
        }
        let height = self.conversation_height();
        let session = self.session().cloned()?;
        let line = {
            let view = self.call_view_mut()?;
            if view.pager.request.is_some() {
                return None;
            }
            view.queue_wanted(height);
            let offset = view.pager.queued.pop_front()?;
            let path = view.artifact.clone()?;
            let id = mint();
            view.pager.request = Some((id.clone(), offset));
            session_command(
                &id,
                "artifact_page",
                &session,
                Some(json!({"path": path, "offset": offset})),
            )
            .to_string()
        };
        Some(line)
    }

    /// A session answer that may answer the view's fetch: it folds into
    /// the view, never into the pages. The follow-up fetch lines go
    /// out; a finished `y` copy drains through `take_call_copy`.
    pub(in crate::app) fn call_view_answered(&mut self, accepted: &CommandAccepted) -> Vec<String> {
        let id = accepted.command_id.0.clone();
        let session = self.session().cloned();
        let height = self.conversation_height();
        let Some(view) = self.call_view_mut() else {
            return Vec::new();
        };
        let Some((request, offset)) = view.pager.request.clone() else {
            return Vec::new();
        };
        if request != id || Some(&view.session) != session.as_ref() {
            return Vec::new();
        }
        view.pager.request = None;
        let Some(CommandResult::ArtifactPage {
            text,
            lines,
            next,
            size,
        }) = accepted.result.clone()
        else {
            let aborted = view.fail(offset, "unexpected answer");
            if aborted {
                self.resume_call_window();
            }
            return Vec::new();
        };
        view.land(offset, text, lines, next, size, height);
        self.settle();
        self.refresh_call_view_matches();
        self.call_view_pump().into_iter().collect()
    }

    /// A session rejection that may refuse the view's fetch: the view
    /// stays open with the pages it holds and a notice naming the
    /// error code. A running copy aborts without writing partial text.
    pub(in crate::app) fn call_view_refused(&mut self, rejected: &CommandRejected) {
        let Some(id) = rejected.command_id.as_ref() else {
            return;
        };
        let code = serde_json::to_value(&rejected.code)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "failed".to_owned());
        let Some(view) = self.call_view_mut() else {
            return;
        };
        let Some((request, offset)) = view.pager.request.clone() else {
            return;
        };
        if request != id.0 {
            return;
        }
        view.pager.request = None;
        let aborted = view.fail(offset, &code);
        if aborted {
            self.resume_call_window();
        }
        self.settle();
    }

    /// Reapplies the bounded draw window once a copy ends: pages a
    /// running copy held evict again. A no-op while a copy still runs.
    fn resume_call_window(&mut self) {
        let height = self.conversation_height();
        let Some(view) = self.call_view_mut() else {
            return;
        };
        if view.copy.is_some() {
            return;
        }
        let rows = view.rows();
        let (top, shown) = window(&rows, &view.list, view.below_len(), height);
        view.evict(&rows, top, shown);
    }

    /// The link went down: the request on the wire is gone, so a
    /// scroll asks again, and a running copy ends with the pages it
    /// holds and no clipboard write.
    pub(in crate::app) fn call_view_lost(&mut self) {
        let aborted = if let Some(view) = self.call_view_mut() {
            view.pager.request = None;
            view.copy.take().is_some()
        } else {
            return;
        };
        if aborted {
            self.resume_call_window();
        }
    }

    /// Starts copying the whole call's output text: pages already held
    /// copy at once, and missing pages fetch one after another first.
    /// A needed page that failed to load aborts with the error-code
    /// notice instead of a partial write.
    fn start_call_copy(&mut self) -> Effect {
        {
            let Some(view) = self.call_view_mut() else {
                return Effect::None;
            };
            if view.copy.is_some() {
                return Effect::None;
            }
            if !view.paged() {
                let text = view.plain_text();
                self.copied = true;
                return Effect::Copy(text);
            }
            // A needed page that failed to load leaves the standing
            // error-code notice; scrolling it back in retries, and then
            // `y` copies whole. The running copy holds the window
            // open, so every fetched page stays until it lands whole
            // or aborts.
            if !view.pager.missing.is_empty() {
                return Effect::None;
            }
            view.copy = Some(CopyReq);
        }
        self.settle();
        match self.take_call_copy() {
            Some(text) => Effect::Copy(text),
            None => self.call_view_pump_effect(),
        }
    }

    /// The text of a `y` copy whose every page is held: the pages in
    /// file order, exactly as returned, with no gutter or line numbers.
    /// Reapplies the bounded window either way. `None` while pages are
    /// missing or no copy runs.
    pub(crate) fn take_call_copy(&mut self) -> Option<String> {
        let text = {
            let view = self.call_view_mut()?;
            if view.copy.is_none() || !view.pager.complete() {
                return None;
            }
            let chain = view.pager.chain_prefix();
            if !chain
                .iter()
                .all(|offset| view.pager.pages.contains_key(offset))
            {
                return None;
            }
            chain
                .iter()
                .filter_map(|offset| view.pager.pages.get(offset))
                .map(|page| page.text.as_str())
                .collect::<String>()
        };
        if let Some(view) = self.call_view_mut() {
            view.copy = None;
        }
        self.copied = true;
        self.resume_call_window();
        Some(text)
    }

    /// Rebuilds the view's matches for the find bar's query: body rows
    /// only, never chrome rows and never the conversation. The current
    /// match stays on its row when it still matches, else moves to the
    /// first match.
    pub(in crate::app) fn refresh_call_view_matches(&mut self) {
        let query = self.find.query().to_owned();
        let Some(view) = self.call_view_mut() else {
            return;
        };
        let rows = view.rows();
        let current_row = view.current.and_then(|at| view.matches.get(at).copied());
        if query != view.query {
            view.query = query.clone();
            view.current = None;
        }
        if query.is_empty() {
            view.matches.clear();
            view.current = None;
            return;
        }
        view.matches = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                matches!(
                    row.source,
                    Source::Prefix(_) | Source::Page { .. } | Source::Closing(_)
                ) && !crate::logical::matches(&row_text(&row.cells), &query).is_empty()
            })
            .map(|(index, _)| index)
            .collect();
        view.current = current_row
            .and_then(|row| view.matches.iter().position(|at| *at == row))
            .or(if view.matches.is_empty() {
                None
            } else {
                Some(0)
            });
    }

    /// Moves the current match between view matches, wrapping at either
    /// end, and scrolls it into view: Enter and the arrows while the
    /// find bar is open.
    pub(in crate::app) fn step_call_view_match(&mut self, key: &Key) {
        let height = self.conversation_height();
        let Some(view) = self.call_view_mut() else {
            return;
        };
        if view.matches.is_empty() {
            return;
        }
        let down = !matches!(key, Key::Up);
        let next = match view.current {
            Some(at) if down => (at + 1) % view.matches.len(),
            Some(at) => (at + view.matches.len() - 1) % view.matches.len(),
            None => 0,
        };
        view.current = Some(next);
        let Some(&row) = view.matches.get(next) else {
            return;
        };
        let len = view.rows().len();
        view.list
            .select(row, len, shown_height(height, view.below_len()));
        self.settle();
    }

    /// The find bar's count while the view is open: the view's matches.
    pub(in crate::app) fn call_view_count(&self) -> Option<String> {
        match self.call_view() {
            Some(view) => {
                if view.query.is_empty() {
                    return None;
                }
                if view.matches.is_empty() {
                    return Some("no matches".to_owned());
                }
                let current = view.current.map(|at| at + 1).unwrap_or(0);
                Some(format!("{current} of {}", view.matches.len()))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "call_view_tests.rs"]
mod tests;
