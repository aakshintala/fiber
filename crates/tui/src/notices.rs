//! Notices as rows above the working line (`docs/tui.md`, "Notices").
//!
//! A notice is this terminal's own memory: the terminal's own messages and
//! the attached session's `notice` lines, never written anywhere. Oldest
//! first, at most three show, then "+N more". A child of `app`, so the calls
//! the click layer makes sit here beside the stack.

use std::time::{Duration, Instant};

use super::App;

/// How many notices show before "+N more".
const SHOWN: usize = 3;

/// How long an event notice shows before it fades: gone once it is this
/// old, so the docs say "about 10 s" while the code uses 10 s exactly.
pub(crate) const FADE: Duration = Duration::from_secs(10);

/// Whether a notice row fades or stays until its cause ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A one-shot report: gone [`FADE`] after it shows.
    Event,
    /// A standing state: gone when its cause ends or with ✕.
    Condition,
}

/// Every notice code's class: conditions stay until their cause ends,
/// events fade after [`FADE`]. A code the table does not name — hub
/// refusals and session notices carrying any code — is an event.
const KINDS: &[(&str, Kind)] = &[
    ("cache_rebuild", Kind::Event),
    ("closing", Kind::Event),
    ("command_conflict", Kind::Event),
    ("config_invalid", Kind::Event),
    ("config_key_ignored", Kind::Event),
    ("connection_failed", Kind::Condition),
    ("credential_failed", Kind::Event),
    ("credential_missing", Kind::Event),
    ("extension_failed", Kind::Event),
    ("extension_incompatible", Kind::Event),
    ("extension_missing", Kind::Event),
    ("extension_shadowed", Kind::Event),
    ("history_loading", Kind::Condition),
    ("hook_failed", Kind::Event),
    ("hub_version", Kind::Event),
    ("instructions_large", Kind::Event),
    ("invalid_arguments", Kind::Event),
    ("io_failed", Kind::Event),
    ("login_url", Kind::Condition),
    ("malformed", Kind::Event),
    ("model_invalid", Kind::Event),
    ("model_unconfigured", Kind::Event),
    ("models_loading", Kind::Condition),
    ("no_jobs_running", Kind::Event),
    ("no_model", Kind::Event),
    ("notes_unchanged", Kind::Event),
    ("repository_code_skipped", Kind::Event),
    ("reviewer_selection_failed", Kind::Event),
    ("session_held", Kind::Event),
    ("session_not_found", Kind::Event),
    ("skill_invalid", Kind::Event),
    ("skill_shadowed", Kind::Event),
    ("skills_large", Kind::Event),
    ("stale_request", Kind::Event),
    ("stream_closed", Kind::Condition),
    ("switch_pending", Kind::Condition),
    ("tool_definitions_large", Kind::Event),
    ("web_search_unavailable", Kind::Event),
    ("workspace", Kind::Event),
];

/// The code's class from [`KINDS`]: unlisted codes are events.
fn kind(code: &str) -> Kind {
    KINDS
        .iter()
        .find(|(name, _)| *name == code)
        .map(|(_, kind)| *kind)
        .unwrap_or(Kind::Event)
}

/// One notice.
#[derive(Debug)]
struct Notice {
    id: usize,
    code: String,
    /// What dedupes repeats while stacked: the session for `stream_closed`,
    /// one share per sent command for `switch_pending`, else the text.
    identity: String,
    text: String,
    /// When the first sweep showing it stamped it: expiry counts from
    /// showing, so a held notice or one past the shown rows fades 10 s
    /// after it shows, never before.
    born: Option<Instant>,
}

/// What the overlay shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    /// One notice's whole text.
    One(usize),
    /// Every notice.
    All,
}

/// What the overlay shows: one notice's whole text, or every notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoticeOverlay {
    /// True for the list form.
    pub(crate) all: bool,
    /// `(code, text)`, newest first.
    pub(crate) entries: Vec<(String, String)>,
}

/// The notices, oldest first, and the overlay.
#[derive(Debug, Default)]
pub(crate) struct Notices {
    next: usize,
    stack: Vec<Notice>,
    overlay: Option<Overlay>,
    /// While a drag selects, the first id pushed since it began: that
    /// notice and later ones wait for the release ("Notices": a notice that
    /// arrives during a drag waits for the release).
    held_from: Option<usize>,
    /// While a drag selects, the ids its press found showing: expiry
    /// waits for the release even for rows already visible, so the
    /// noted rows a drag reads cannot fade under it (rule R3).
    held_visible: Vec<usize>,
}

/// One drawn row: a notice, or the "+N more" row for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoticeLine {
    /// A notice, oldest shown first.
    Notice {
        /// The notice it shows.
        id: usize,
        /// Its registry code (`docs/errors.md`, "Registry").
        code: String,
        /// Its message.
        text: String,
    },
    /// How many shown notices have no row.
    More(usize),
}

impl Notices {
    /// Adds `text` under `code` on top, dropping a same-code same-text
    /// repeat while it is stacked. A sent `switch_pending` choice is one
    /// notice worth of state even beside an identical choice, so each
    /// push stacks its own row.
    pub(crate) fn push(&mut self, code: &str, text: String) {
        let identity = if code == "switch_pending" {
            format!("{}#{}", text, self.next)
        } else {
            text.clone()
        };
        self.push_identified(code, identity, text);
    }

    /// Adds `text` under `code` with `identity` on top, dropping a
    /// same-code same-identity repeat while it is stacked: the session id
    /// for `stream_closed`, the full text otherwise, so a second message
    /// with another identity stacks beside the first.
    pub(crate) fn push_identified(&mut self, code: &str, identity: String, text: String) {
        if self
            .stack
            .iter()
            .any(|notice| notice.code == code && notice.identity == identity)
        {
            return;
        }
        let id = self.next;
        self.next = self.next.saturating_add(1);
        self.stack.push(Notice {
            id,
            code: code.to_owned(),
            identity,
            text,
            born: None,
        });
    }

    /// Births the unborn notices this sweep shows, then removes expired
    /// events: an event is gone once it is at least [`FADE`] old. Birth
    /// counts from showing — the unheld notices in the shown rows, plus
    /// what the open overlay lists — so a held notice or one past the
    /// shown cap gains no birth until a sweep shows it, and never
    /// expires before it first shows. A notice a drag holds, or one
    /// visible in the open overlay, never expires until the release or
    /// close; expiry then applies on the next sweep. Expired rows leave
    /// before the draw, so none occupies a shown slot. The single
    /// remover: every other end clears through `dismiss` or
    /// [`Notices::clear_code`].
    pub(crate) fn sweep(&mut self, now: Instant) {
        let visible = self.visible_ids();
        for notice in &mut self.stack {
            if notice.born.is_none() && visible.contains(&notice.id) {
                notice.born = Some(now);
            }
        }
        let overlay = self.overlay;
        let held = self.held_visible.clone();
        // Expiry funnels through `drop_where` like every other end, so
        // a fading row closes a single overlay showing it.
        self.drop_where(|notice| {
            if kind(&notice.code) != Kind::Event {
                return false;
            }
            if held.contains(&notice.id) {
                return false;
            }
            let born = notice.born.unwrap_or(now);
            now.saturating_duration_since(born) >= FADE && !Self::paused(overlay, notice.id)
        });
    }

    /// Whether `id` waits out its expiry in sweep and wake alike: a
    /// notice visible in the open overlay keeps its row and contributes
    /// no wake until the close. A notice pushed during a hold needs no
    /// pause here: birth requires showing, and showing excludes held
    /// rows, so it is always unborn — the age rule already retains it
    /// and it contributes no wake. A row showing when the hold began
    /// pauses through `held_visible` instead. Takes the copied overlay
    /// so the sweep's `retain` can call it while holding the stack.
    fn paused(overlay: Option<Overlay>, id: usize) -> bool {
        overlay == Some(Overlay::All) || overlay == Some(Overlay::One(id))
    }

    /// The ids this sweep shows: the unheld notices in the shown rows,
    /// plus what the open overlay lists past the cap.
    fn visible_ids(&self) -> Vec<usize> {
        let mut ids: Vec<usize> = self.shown().take(SHOWN).map(|notice| notice.id).collect();
        match self.overlay {
            Some(Overlay::All) => ids.extend(self.shown().skip(SHOWN).map(|notice| notice.id)),
            Some(Overlay::One(id)) if self.shown().any(|notice| notice.id == id) => {
                ids.push(id);
            }
            Some(Overlay::One(_)) | None => {}
        }
        ids
    }

    /// The earliest live unpaused event expiry: what the frame arms the
    /// tick with beside the motion and items wakes. A held notice, and a
    /// notice visible in the open overlay, contributes no wake until its
    /// release or close, so arming never ticks in a loop on a paused
    /// expiry. `None` with no unpaused event stacked. Reads post-sweep
    /// births: the frame sweeps before it arms.
    pub(crate) fn notices_wake(&self) -> Option<Instant> {
        self.stack
            .iter()
            .filter(|notice| kind(&notice.code) == Kind::Event)
            .filter(|notice| !Self::paused(self.overlay, notice.id))
            .filter(|notice| !self.held_visible.contains(&notice.id))
            .filter_map(|notice| notice.born.map(|born| born + FADE))
            .min()
    }

    /// Drops every notice under `code`, closing a single overlay on a
    /// dropped row. The turn-end fold clears `switch_pending` this way:
    /// every share standing applied when the turn ended.
    pub(crate) fn clear_code_all(&mut self, code: &str) {
        self.drop_where(|notice| notice.code == code);
    }

    /// Drops every notice under `code` except `keep_identity`, closing a
    /// single overlay on a dropped row. The reconnect fold clears
    /// `connection_failed` this way — both connection texts go, never the
    /// search-incomplete text — and attaching clears `stream_closed`
    /// this way: only other sessions' stale rows go.
    pub(crate) fn clear_code_except(&mut self, code: &str, keep_identity: &str) {
        self.drop_where(|notice| notice.code == code && notice.identity != keep_identity);
    }

    /// Drops the notice under `code` and `identity`, closing a single
    /// overlay showing it, as [`Notices::dismiss`] does. Clearing an
    /// absent notice changes nothing, and either order with ✕ ends the same.
    pub(crate) fn clear_code(&mut self, code: &str, identity: &str) {
        self.drop_where(|notice| notice.code == code && notice.identity == identity);
    }

    /// Drops every stacked notice `drop` accepts, closing a single
    /// overlay showing a dropped row. Every cause-end clear funnels here,
    /// so held rows a sweep would pause still end with their cause.
    fn drop_where(&mut self, drop: impl Fn(&Notice) -> bool) {
        let mut dropped = Vec::new();
        self.stack.retain(|notice| {
            let stale = drop(notice);
            if stale {
                dropped.push(notice.id);
            }
            !stale
        });
        if let Some(Overlay::One(id)) = self.overlay
            && dropped.contains(&id)
        {
            self.overlay = None;
        }
    }

    /// The newest notice's text.
    #[cfg(test)]
    pub(crate) fn newest(&self) -> Option<&str> {
        self.stack.last().map(|notice| notice.text.as_str())
    }

    /// A drag began: notices pushed from now on wait, kept but not drawn,
    /// listed or clickable, until [`Notices::release`]. Rows already
    /// showing stay showing, but their expiry waits too.
    pub(crate) fn hold(&mut self) {
        if self.held_from.is_none() {
            self.held_visible = self.visible_ids();
        }
        self.held_from.get_or_insert(self.next);
    }

    /// The drag ended: the notices that waited show, oldest first, and
    /// the shown rows resume expiring on the next sweep.
    pub(crate) fn release(&mut self) {
        self.held_from = None;
        self.held_visible.clear();
    }

    /// The notices that show, oldest first: all but those a drag holds.
    fn shown(&self) -> impl DoubleEndedIterator<Item = &Notice> {
        let held = self.held_from;
        self.stack
            .iter()
            .filter(move |notice| held.is_none_or(|from| notice.id < from))
    }

    /// How many rows [`Notices::lines`] draws, without building them.
    pub(crate) fn drawn(&self) -> usize {
        let shown = self.shown().count();
        shown.min(SHOWN) + usize::from(shown > SHOWN)
    }

    /// The rows the view draws, oldest first: at most [`SHOWN`] notices,
    /// then one `More` for the rest.
    pub(crate) fn lines(&self) -> Vec<NoticeLine> {
        let mut lines: Vec<NoticeLine> = self
            .shown()
            .take(SHOWN)
            .map(|notice| NoticeLine::Notice {
                id: notice.id,
                code: notice.code.clone(),
                text: notice.text.clone(),
            })
            .collect();
        let rest = self.shown().count().saturating_sub(SHOWN);
        if rest > 0 {
            lines.push(NoticeLine::More(rest));
        }
        lines
    }

    /// Removes notice `id`, closing an overlay that showed it alone.
    pub(crate) fn dismiss(&mut self, id: usize) {
        self.stack.retain(|notice| notice.id != id);
        if self.overlay == Some(Overlay::One(id)) {
            self.overlay = None;
        }
    }

    /// Opens notice `id` whole in the overlay.
    pub(crate) fn open(&mut self, id: usize) {
        if self.stack.iter().any(|notice| notice.id == id) {
            self.overlay = Some(Overlay::One(id));
        }
    }

    /// Opens every notice in the overlay.
    pub(crate) fn open_all(&mut self) {
        if !self.stack.is_empty() {
            self.overlay = Some(Overlay::All);
        }
    }

    /// Closes the overlay; false when none was open.
    pub(crate) fn close(&mut self) -> bool {
        self.overlay.take().is_some()
    }

    /// The overlay's entries, newest first, while it is open.
    pub(crate) fn overlay_entries(&self) -> Option<NoticeOverlay> {
        let overlay = self.overlay?;
        Some(NoticeOverlay {
            all: overlay == Overlay::All,
            entries: self
                .shown()
                .rev()
                .filter(|notice| overlay == Overlay::All || overlay == Overlay::One(notice.id))
                .map(|notice| (notice.code.clone(), notice.text.clone()))
                .collect(),
        })
    }

    /// The overlay's text, newest first, while it is open.
    pub(crate) fn overlay(&self) -> Option<Vec<String>> {
        self.overlay_entries()
            .map(|entries| entries.entries.into_iter().map(|(_, text)| text).collect())
    }

    /// Notice `id`'s whole text, while it is in the stack.
    pub(crate) fn text(&self, id: usize) -> Option<&str> {
        self.stack
            .iter()
            .find(|notice| notice.id == id)
            .map(|notice| notice.text.as_str())
    }
}

impl App {
    /// The notice rows the view draws above the working line.
    pub(crate) fn notices(&self) -> Vec<NoticeLine> {
        self.notices.lines()
    }

    /// The notice overlay's entries while it is open.
    pub(crate) fn notice_overlay_entries(&self) -> Option<NoticeOverlay> {
        self.notices.overlay_entries()
    }

    /// The notice overlay's text while it is open.
    pub(crate) fn notice_overlay(&self) -> Option<Vec<String>> {
        self.notices.overlay()
    }

    /// The newest notice's text.
    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<&str> {
        self.notices.newest()
    }

    /// ✕ on notice `id`.
    pub(crate) fn dismiss_notice(&mut self, id: usize) {
        self.notices.dismiss(id);
    }

    /// A click on notice `id`: its whole text in the overlay.
    pub(crate) fn open_notice(&mut self, id: usize) {
        self.notices.open(id);
    }

    /// A click on "+N more": every notice in the overlay.
    pub(crate) fn open_more_notices(&mut self) {
        self.notices.open_all();
    }
}

#[cfg(test)]
#[path = "notices_tests.rs"]
mod tests;
