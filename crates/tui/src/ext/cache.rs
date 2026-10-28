//! Page-qualified renderer entries, shared failure memo and pending notices.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::anim::{AnimKey, effective_interval};
use super::spans::{self, FittedLine};
use super::{Decision, Failure, FailureKind, Input};

/// The drawn page, never a caller's current session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PageKey {
    pub(crate) session: contract::SessionId,
    pub(crate) index: usize,
}

impl PageKey {
    /// The home notice key: the empty session with an index no page
    /// takes, so it never equals a real page key.
    pub(crate) fn home() -> Self {
        Self {
            session: contract::SessionId(String::new()),
            index: usize::MAX,
        }
    }
}

type FailKey = (PageKey, String, u64, String, Option<String>);
type EntryKey = (FailKey, u16);

#[derive(Debug, Default)]
struct Pending {
    failures: Vec<(PageKey, String, Failure)>,
    notices: Vec<(String, String)>,
}

/// Successful entries are local; speculative draws share only failure state.
#[derive(Debug)]
pub(crate) struct Cache {
    entries: Mutex<HashMap<EntryKey, Arc<Vec<FittedLine>>>>,
    failed: Arc<Mutex<HashSet<FailKey>>>,
    pending: Arc<Mutex<Pending>>,
    owner: AtomicBool,
    anim: Mutex<AnimData>,
}

/// One animated item's schedule state: its drawn frame, when that frame
/// was drawn, its effective interval, and why it holds still, if it does.
/// A still item always has a stored entry to hit: stilling stores.
#[derive(Debug, Clone)]
struct AnimState {
    frame: u64,
    last: Instant,
    interval: Duration,
    kind: AnimKind,
}

/// Why an animated item holds still: the renderer stopped it until its
/// input changes, or reduced motion stilled it at frame 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimKind {
    /// Ticking: advances when due with paint in the previous frame.
    Active,
    /// `{ lines, animate = false }` drew: hits stored lines, never
    /// recalls, until the input changes.
    Stopped,
    /// Reduced motion (or flat) stilled it at frame 0: reactivates at
    /// frame 1 when reduced motion turns off.
    Reduced,
}

/// The animation frame's bookkeeping: its time and reduced flag, the
/// items painted in this frame and the last, and their schedule states.
#[derive(Debug, Default)]
struct AnimData {
    at: Option<(Instant, bool)>,
    painted_this: HashSet<AnimKey>,
    painted_last: HashSet<AnimKey>,
    states: HashMap<AnimKey, AnimState>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
            failed: Arc::default(),
            pending: Arc::default(),
            owner: AtomicBool::new(true),
            anim: Mutex::default(),
        }
    }
}

impl Cache {
    /// A non-owning draw cache whose failures survive its disposal.
    pub(crate) fn scratch_sharing_pending(&self) -> Self {
        Self {
            entries: Mutex::default(),
            failed: Arc::clone(&self.failed),
            pending: Arc::clone(&self.pending),
            owner: AtomicBool::new(false),
            // A scratch draw never opens the animation frame, so its
            // schedule stays shut: closed draws render still.
            anim: Mutex::default(),
        }
    }

    /// A landed copy gains authority to clear shared state on reload.
    pub(crate) fn promote(&self) {
        self.owner.store(true, Ordering::Relaxed);
    }

    /// Looks up only the selected winner and its output-relevant half identity.
    #[allow(
        clippy::too_many_arguments,
        reason = "the key and callback describe one bound row"
    )]
    pub(crate) fn lines(
        &self,
        page: PageKey,
        slot: &str,
        extension: &str,
        half: Option<String>,
        input: &Input<'_>,
        width: u16,
        call: impl FnOnce() -> Decision,
    ) -> Option<Arc<Vec<FittedLine>>> {
        let (input_key, flat) = key_of(input, slot);
        let key = (page, slot.to_owned(), input_key, extension.to_owned(), half);
        if support::lock(&self.failed).contains(&key) {
            return None;
        }
        let entry = (key.clone(), width);
        if let Some(hit) = support::lock(&self.entries).get(&entry).cloned() {
            return Some(hit);
        }
        crate::work::add(|work| work.renderer_calls += 1);
        match call() {
            Decision::Draw {
                extension: actual,
                lines,
            } => {
                if actual != extension {
                    return None;
                }
                match spans::fit(&lines, width, flat) {
                    Ok(lines) => {
                        let lines = Arc::new(lines);
                        support::lock(&self.entries).insert(entry, Arc::clone(&lines));
                        Some(lines)
                    }
                    Err(error) => {
                        self.fail(
                            key,
                            Failure {
                                kind: FailureKind::HookFailed,
                                extension: actual,
                                message: error.to_string(),
                            },
                        );
                        None
                    }
                }
            }
            Decision::Fail(failure) => {
                if failure.extension == extension {
                    self.fail(key, failure);
                }
                None
            }
            Decision::NotForSession { .. } => None,
        }
    }

    /// Opens the animation frame at `now`: paint draws advance, closed
    /// draws take the still path. `reduced` stills every painted item at
    /// frame 0. Called once per frame on the resident cache, before its
    /// conversation and notice paints.
    pub(crate) fn begin_anim_frame(&self, now: Option<Instant>, reduced: bool) {
        support::lock(&self.anim).at = now.map(|at| (at, reduced));
    }

    /// Marks `keys` painted this frame: only the paint pass calls this,
    /// once per frame, so dues and advancement follow painted rows.
    pub(crate) fn mark_painted(&self, keys: &[AnimKey]) {
        support::lock(&self.anim)
            .painted_this
            .extend(keys.iter().cloned());
    }

    /// The animated item's lines for one draw. An open frame advances an
    /// active item only with paint in the previous frame and its interval
    /// due; effective reduced motion stores a reduced still at 0, and a
    /// reactivation paints frame 1. A stopped item hits its stored lines.
    /// A closed draw renders at the stored frame or 0 with no entry
    /// write and no mark. Failures use the memoized failure path and stop
    /// the item's animation until its input changes.
    #[allow(
        clippy::too_many_arguments,
        reason = "the key, draw flags and callback describe one animated row"
    )]
    pub(crate) fn lines_animated(
        &self,
        page: PageKey,
        slot: &str,
        extension: &str,
        half: Option<String>,
        input: &Input<'_>,
        width: u16,
        flat: bool,
        interval: Duration,
        call: impl FnOnce(u64) -> (Decision, bool),
    ) -> Option<Arc<Vec<FittedLine>>> {
        let (input_key, input_flat) = key_of(input, slot);
        let key = AnimKey::new(page.clone(), slot, input_key, extension, half.clone());
        let fail_key = (page, slot.to_owned(), input_key, extension.to_owned(), half);
        if support::lock(&self.failed).contains(&fail_key) {
            return None;
        }
        let frame = support::lock(&self.anim).at;
        let Some((now, frame_reduced)) = frame else {
            return self.closed_draw(&fail_key, &key, extension, input_flat, width, call);
        };
        let reduced = frame_reduced || flat;
        let (state, painted) = {
            let anim = support::lock(&self.anim);
            (
                anim.states.get(&key).cloned(),
                anim.painted_last.contains(&key),
            )
        };
        match state {
            None => {
                // A fresh item starts at frame 0, still under reduced.
                let frame = 0;
                let drawn =
                    self.draw_frame(&fail_key, &key, extension, input_flat, width, frame, call)?;
                let kind = if reduced {
                    AnimKind::Reduced
                } else if drawn.1 {
                    AnimKind::Active
                } else {
                    AnimKind::Stopped
                };
                self.set_anim(
                    &key,
                    AnimState {
                        frame,
                        last: now,
                        interval,
                        kind,
                    },
                );
                self.store_frame(&fail_key, &key, width, drawn.0, true)
            }
            Some(state) if state.kind == AnimKind::Reduced && reduced => {
                if let Some(hit) = self.stored_entry(&fail_key, width) {
                    return Some(hit);
                }
                // On demand only: the still form drew once, and a width
                // change re-renders it at 0 without advancing.
                let drawn =
                    self.draw_frame(&fail_key, &key, extension, input_flat, width, 0, call)?;
                self.store_frame(&fail_key, &key, width, drawn.0, false)
            }
            Some(state) if state.kind == AnimKind::Active && reduced => {
                // Reduced motion stills at frame 0 at the next paint.
                let drawn =
                    self.draw_frame(&fail_key, &key, extension, input_flat, width, 0, call)?;
                self.set_anim(
                    &key,
                    AnimState {
                        frame: 0,
                        last: now,
                        interval,
                        kind: AnimKind::Reduced,
                    },
                );
                self.store_frame(&fail_key, &key, width, drawn.0, true)
            }
            Some(state) if state.kind == AnimKind::Reduced && !reduced => {
                // Reactivation paints frame 1: the still form at 0
                // established the counter, which resumes.
                let drawn =
                    self.draw_frame(&fail_key, &key, extension, input_flat, width, 1, call)?;
                self.set_anim(
                    &key,
                    AnimState {
                        frame: 1,
                        last: now,
                        interval,
                        kind: if drawn.1 {
                            AnimKind::Active
                        } else {
                            AnimKind::Stopped
                        },
                    },
                );
                self.store_frame(&fail_key, &key, width, drawn.0, true)
            }
            Some(state) if state.kind == AnimKind::Stopped => {
                if let Some(hit) = self.stored_entry(&fail_key, width) {
                    return Some(hit);
                }
                // On demand only: no recall for a width change alone.
                let drawn = self.draw_frame(
                    &fail_key,
                    &key,
                    extension,
                    input_flat,
                    width,
                    state.frame,
                    call,
                )?;
                self.store_frame(&fail_key, &key, width, drawn.0, false)
            }
            Some(state) => {
                let due = now
                    .checked_duration_since(state.last)
                    .is_some_and(|elapsed| elapsed >= effective_interval(state.interval));
                if state.kind == AnimKind::Active && painted && due {
                    let frame = state.frame.saturating_add(1);
                    let drawn = self
                        .draw_frame(&fail_key, &key, extension, input_flat, width, frame, call)?;
                    self.set_anim(
                        &key,
                        AnimState {
                            frame,
                            last: now,
                            interval,
                            kind: if drawn.1 {
                                AnimKind::Active
                            } else {
                                AnimKind::Stopped
                            },
                        },
                    );
                    self.store_frame(&fail_key, &key, width, drawn.0, true)
                } else if let Some(hit) = self.stored_entry(&fail_key, width) {
                    Some(hit)
                } else {
                    // A width change or an early re-render reuses the
                    // stored frame with the counter, last, markers and
                    // due unchanged.
                    let drawn = self.draw_frame(
                        &fail_key,
                        &key,
                        extension,
                        input_flat,
                        width,
                        state.frame,
                        call,
                    )?;
                    self.store_frame(&fail_key, &key, width, drawn.0, false)
                }
            }
        }
    }

    /// A closed draw: hits stored lines, else calls at the stored frame
    /// or 0, with no entry write, no state change and no mark.
    fn closed_draw(
        &self,
        fail_key: &FailKey,
        key: &AnimKey,
        extension: &str,
        flat: bool,
        width: u16,
        call: impl FnOnce(u64) -> (Decision, bool),
    ) -> Option<Arc<Vec<FittedLine>>> {
        if let Some(hit) = self.stored_entry(fail_key, width) {
            return Some(hit);
        }
        let frame = support::lock(&self.anim)
            .states
            .get(key)
            .map(|state| state.frame)
            .unwrap_or(0);
        let (lines, _) = self.draw_frame(fail_key, key, extension, flat, width, frame, call)?;
        Some(Arc::new(lines))
    }

    /// The animated item's stored lines at `width`, without invoking: a
    /// miss reads as none. Never writes and never marks.
    pub(crate) fn animated_lines(
        &self,
        page: PageKey,
        slot: &str,
        extension: &str,
        half: Option<String>,
        input: &Input<'_>,
        width: u16,
    ) -> Option<Arc<Vec<FittedLine>>> {
        let (input_key, _) = key_of(input, slot);
        let key = (page, slot.to_owned(), input_key, extension.to_owned(), half);
        support::lock(&self.entries).get(&(key, width)).cloned()
    }

    /// The animation dues of items painted this frame: each active one's
    /// last tick with its interval, for the wake fold. The fold asks each
    /// one's next frame at its last tick plus its interval. Rotates the
    /// painted sets and closes the frame.
    pub(crate) fn anim_dues(&self) -> Vec<(Instant, Duration)> {
        let mut anim = support::lock(&self.anim);
        let painted = std::mem::take(&mut anim.painted_this);
        anim.painted_last = painted.clone();
        if anim.at.take().is_none() {
            return Vec::new();
        }
        painted
            .into_iter()
            .filter_map(|key| {
                let state = anim.states.get(&key)?;
                (state.kind == AnimKind::Active).then_some((state.last, state.interval))
            })
            .collect()
    }

    /// The fitted lines stored for `fail_key` at `width`, if any.
    fn stored_entry(&self, fail_key: &FailKey, width: u16) -> Option<Arc<Vec<FittedLine>>> {
        support::lock(&self.entries)
            .get(&(fail_key.clone(), width))
            .cloned()
    }

    /// Calls the renderer at `frame` and fits its lines: the drawn lines
    /// with whether the item continues. A failure memoizes against the
    /// input and stops the item's animation, a miss draws nothing.
    /// Never stores and never marks: storing is the caller's.
    #[allow(
        clippy::too_many_arguments,
        reason = "the key, draw flags and callback describe one animated row"
    )]
    fn draw_frame(
        &self,
        fail_key: &FailKey,
        key: &AnimKey,
        extension: &str,
        flat: bool,
        width: u16,
        frame: u64,
        call: impl FnOnce(u64) -> (Decision, bool),
    ) -> Option<(Vec<FittedLine>, bool)> {
        crate::work::add(|work| work.renderer_calls += 1);
        match call(frame) {
            (
                Decision::Draw {
                    extension: actual,
                    lines,
                },
                continues,
            ) => {
                if actual != extension {
                    return None;
                }
                match spans::fit(&lines, width, flat) {
                    Ok(lines) => Some((lines, continues)),
                    Err(error) => {
                        self.fail(
                            fail_key.clone(),
                            Failure {
                                kind: FailureKind::HookFailed,
                                extension: actual,
                                message: error.to_string(),
                            },
                        );
                        self.drop_anim(key);
                        None
                    }
                }
            }
            (Decision::Fail(failure), _) => {
                if failure.extension == extension {
                    self.fail(fail_key.clone(), failure);
                }
                self.drop_anim(key);
                None
            }
            (Decision::NotForSession { .. }, _) => None,
        }
    }

    /// Tags `lines` with the animated item and stores them: a new frame
    /// evicts every older same-item entry at every width, so only the
    /// latest frame is kept. Returns the stored lines.
    fn store_frame(
        &self,
        fail_key: &FailKey,
        key: &AnimKey,
        width: u16,
        lines: Vec<FittedLine>,
        new_frame: bool,
    ) -> Option<Arc<Vec<FittedLine>>> {
        let tagged = lines
            .into_iter()
            .map(|line| FittedLine {
                anim: Some(key.clone()),
                ..line
            })
            .collect::<Vec<_>>();
        let stored = Arc::new(tagged);
        let mut entries = support::lock(&self.entries);
        if new_frame {
            entries.retain(|(past, _), _| past != fail_key);
        }
        entries.insert((fail_key.clone(), width), Arc::clone(&stored));
        Some(stored)
    }

    /// Records the item's schedule state.
    fn set_anim(&self, key: &AnimKey, state: AnimState) {
        support::lock(&self.anim).states.insert(key.clone(), state);
    }

    /// Drops the item's schedule state: a failure stops its animation
    /// until its input changes.
    fn drop_anim(&self, key: &AnimKey) {
        support::lock(&self.anim).states.remove(key);
    }

    fn fail(&self, key: FailKey, failure: Failure) {
        if support::lock(&self.failed).insert(key.clone()) {
            support::lock(&self.pending)
                .failures
                .push((key.0, key.1, failure));
        }
    }

    /// Selection notices have no page lifetime and are already deduplicated.
    pub(crate) fn queue_notice(&self, notice: (String, String)) {
        support::lock(&self.pending).notices.push(notice);
    }

    /// Only displayed owners may prune shared failures for this exact page.
    pub(crate) fn drop_page(&self, page: PageKey) {
        support::lock(&self.entries).retain(|key, _| key.0.0 != page);
        // Animation counters drop with their page: a returning item
        // restarts at frame 0.
        support::lock(&self.anim)
            .states
            .retain(|key, _| key.page() != &page);
        if self.owner.load(Ordering::Relaxed) {
            support::lock(&self.failed).retain(|key| key.0 != page);
            support::lock(&self.pending)
                .failures
                .retain(|(at, _, _)| *at != page);
        }
    }

    pub(crate) fn drop_entries(&self) {
        // A width change keeps the frame counter, the last time and every
        // still and stopped marker: only the rendered entries go.
        support::lock(&self.entries).clear();
    }

    pub(crate) fn clear(&self) {
        self.drop_entries();
        support::lock(&self.anim).states.clear();
        if self.owner.load(Ordering::Relaxed) {
            support::lock(&self.failed).clear();
            *support::lock(&self.pending) = Pending::default();
        }
    }

    pub(crate) fn take_failures(&self) -> Vec<(String, Failure)> {
        std::mem::take(&mut support::lock(&self.pending).failures)
            .into_iter()
            .map(|(_, slot, failure)| (slot, failure))
            .collect()
    }

    pub(crate) fn take_notices(&self) -> Vec<(String, String)> {
        std::mem::take(&mut support::lock(&self.pending).notices)
    }
}

/// The entry key and flat flag for `input` in `slot`: the same key
/// [`Cache::lines`] files entries under, without the width.
fn key_of(input: &Input<'_>, slot: &str) -> (u64, bool) {
    match input {
        Input::LedgerRow(row) => (row.key(slot), row.flat),
        Input::TurnCard(card) => (card.key(slot), card.flat),
        Input::Prompt(prompt) => (prompt.key(slot), prompt.flat),
        Input::Reply(reply) => (reply.key(slot), reply.flat),
        Input::ThinkingLine(line) => (line.key(slot), line.flat),
        Input::GroupLine(line) => (line.key(slot), line.flat),
        Input::Steer(steer) => (steer.key(slot), steer.flat),
        Input::Answers(answers) => (answers.key(slot), answers.flat),
        Input::HandoffBand(band) => (band.key(slot), band.flat),
        Input::ErrorLine(line) => (line.key(slot), line.flat),
        Input::Notice(notice) => (notice.key(slot), notice.flat),
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
