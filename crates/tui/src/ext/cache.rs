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
/// Ordinary entries by input and width: items file under no notice, a
/// notice's successful closed-draw lines under its instance, so pruning
/// that instance never touches item lines.
type EntryKey = (FailKey, Option<usize>, u16);
type AnimEntries = HashMap<AnimKey, HashMap<u16, Arc<Vec<FittedLine>>>>;

/// One closed draw's stored result: the fitted lines at the frame they
/// drew, with whether the item continues. A later paint at the same
/// frame promotes the result instead of calling again.
#[derive(Debug, Clone)]
struct ClosedLines {
    /// The fitted lines the closed draw stored.
    lines: Arc<Vec<FittedLine>>,
    /// The frame the lines drew at: the stored frame, or 0.
    frame: u64,
    /// Whether the draw keeps animating.
    continues: bool,
}

/// Which draw the shared animated path takes: the frame decides, a
/// count forces the closed path, or a paint promotes a stored closed
/// result on the open path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawMode {
    /// The frame decides: open paint advances, a closed frame draws
    /// still.
    Auto,
    /// A count forces the closed path, even in an open frame.
    Count,
    /// A paint promotes a stored closed result at the drawn frame.
    Paint,
}

#[derive(Debug, Default)]
struct Pending {
    failures: Vec<(PageKey, String, Failure)>,
    notices: Vec<(String, String)>,
}

/// Successful entries are local; speculative draws share only failure state.
#[derive(Debug)]
pub(crate) struct Cache {
    entries: Mutex<HashMap<EntryKey, Arc<Vec<FittedLine>>>>,
    /// Closed draws by input and width, with the frame they drew at and
    /// whether they continue: a count's frame-0 result, promoted by the
    /// paint that follows it at the same width.
    closed: Mutex<HashMap<EntryKey, ClosedLines>>,
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
    /// Reduced motion stilled it at frame 0: reactivates at
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
    /// Animated entries are grouped by item, so replacing a frame only
    /// touches that item's width variants.
    entries: AnimEntries,
    /// Last rendered-line count per item, for page recounts on change.
    line_counts: HashMap<AnimKey, usize>,
    /// Pages whose animated line count changed during paint.
    changed_pages: HashSet<PageKey>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
            closed: Mutex::default(),
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
            closed: Mutex::default(),
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
        let entry = (key.clone(), None, width);
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
    /// reactivation paints frame 1, unless the stilling draw declines to
    /// continue, which stays stopped instead. A stopped item hits its
    /// stored lines.
    /// A closed draw hits stored lines or caches a miss in the ordinary
    /// entries without changing animation state or marking. Failures use
    /// the memoized failure path and stop the item's animation until its
    /// input changes.
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
        interval: Duration,
        call: impl FnOnce(u64) -> (Decision, bool),
    ) -> Option<Arc<Vec<FittedLine>>> {
        let (input_key, input_flat) = key_of(input, slot);
        let key = AnimKey::new(page.clone(), slot, input_key, extension, half.clone());
        let fail_key = (page, slot.to_owned(), input_key, extension.to_owned(), half);
        self.lines_animated_with_key(
            key,
            fail_key,
            None,
            extension,
            input_flat,
            width,
            interval,
            DrawMode::Auto,
            call,
        )
    }

    /// The animated notice's lines for one draw: the item schedule with
    /// the notice instance qualifying the animation key, on the stable
    /// home page that never drops or recounts. The failure memo stays
    /// instance-free and page-stable, so a re-raised failure recalls
    /// nothing until its public input changes; successful closed-draw
    /// lines carry the instance and go with it.
    #[allow(
        clippy::too_many_arguments,
        reason = "the key, draw flags and callback describe one animated row"
    )]
    pub(crate) fn lines_animated_notice(
        &self,
        id: usize,
        extension: &str,
        half: Option<String>,
        input: &Input<'_>,
        width: u16,
        interval: Duration,
        call: impl FnOnce(u64) -> (Decision, bool),
    ) -> Option<Arc<Vec<FittedLine>>> {
        let (input_key, input_flat) = key_of(input, "notice");
        let key = AnimKey::for_notice(
            PageKey::home(),
            "notice",
            input_key,
            extension,
            half.clone(),
            id,
        );
        let fail_key = (
            PageKey::home(),
            "notice".to_owned(),
            input_key,
            extension.to_owned(),
            half,
        );
        self.lines_animated_with_key(
            key,
            fail_key,
            Some(id),
            extension,
            input_flat,
            width,
            interval,
            DrawMode::Auto,
            call,
        )
    }

    /// A chrome count's lines for one draw: the shared closed path even
    /// in an open frame, caching the drawn frame with whether it
    /// continues, without animation state or mark. The paint that
    /// follows promotes the stored result at the same width.
    #[allow(
        clippy::too_many_arguments,
        reason = "the key, draw flags and callback describe one animated row"
    )]
    pub(crate) fn lines_animated_closed(
        &self,
        page: PageKey,
        slot: &str,
        extension: &str,
        half: Option<String>,
        input: &Input<'_>,
        width: u16,
        call: impl FnOnce(u64) -> (Decision, bool),
    ) -> Option<Arc<Vec<FittedLine>>> {
        let (input_key, input_flat) = key_of(input, slot);
        let key = AnimKey::new(page.clone(), slot, input_key, extension, half.clone());
        let fail_key = (page, slot.to_owned(), input_key, extension.to_owned(), half);
        self.lines_animated_with_key(
            key,
            fail_key,
            None,
            extension,
            input_flat,
            width,
            // A count never schedules: the closed path ignores it.
            Duration::ZERO,
            DrawMode::Count,
            call,
        )
    }

    /// A chrome paint's lines for one draw: the open animated path, with
    /// a stored closed result at the drawn frame promoted instead of
    /// calling again. Later paints advance only painted items.
    #[allow(
        clippy::too_many_arguments,
        reason = "the key, draw flags and callback describe one animated row"
    )]
    pub(crate) fn lines_animated_paint(
        &self,
        page: PageKey,
        slot: &str,
        extension: &str,
        half: Option<String>,
        input: &Input<'_>,
        width: u16,
        interval: Duration,
        call: impl FnOnce(u64) -> (Decision, bool),
    ) -> Option<Arc<Vec<FittedLine>>> {
        let (input_key, input_flat) = key_of(input, slot);
        let key = AnimKey::new(page.clone(), slot, input_key, extension, half.clone());
        let fail_key = (page, slot.to_owned(), input_key, extension.to_owned(), half);
        self.lines_animated_with_key(
            key,
            fail_key,
            None,
            extension,
            input_flat,
            width,
            interval,
            DrawMode::Paint,
            call,
        )
    }

    /// The shared animated draw: the separately built animation, failure
    /// and closed-entry keys let notices qualify animation by instance
    /// while failures stay on public input. Item keys carry no notice,
    /// so item behavior is unchanged.
    #[allow(
        clippy::too_many_arguments,
        reason = "the keys, draw flags and callback describe one animated row"
    )]
    fn lines_animated_with_key(
        &self,
        key: AnimKey,
        fail_key: FailKey,
        notice_id: Option<usize>,
        extension: &str,
        flat: bool,
        width: u16,
        interval: Duration,
        mode: DrawMode,
        call: impl FnOnce(u64) -> (Decision, bool),
    ) -> Option<Arc<Vec<FittedLine>>> {
        if support::lock(&self.failed).contains(&fail_key) {
            return None;
        }
        if mode == DrawMode::Count {
            // A count takes the closed path even in an open frame: no
            // animation state and no mark.
            return self.closed_draw(&fail_key, notice_id, &key, extension, flat, width, call);
        }
        let frame = support::lock(&self.anim).at;
        let Some((now, frame_reduced)) = frame else {
            return self.closed_draw(&fail_key, notice_id, &key, extension, flat, width, call);
        };
        let reduced = frame_reduced;
        let promote = mode == DrawMode::Paint;
        let (state, painted) = {
            let anim = support::lock(&self.anim);
            (
                anim.states.get(&key).cloned(),
                anim.painted_last.contains(&key),
            )
        };
        match state {
            None => {
                // A fresh item starts at frame 0, still under reduced. A
                // paint promotes the count's stored frame-0 result at
                // the same width instead of calling again.
                let frame = 0;
                let promoted = promote
                    .then(|| self.promoted(&(fail_key.clone(), notice_id, width), frame))
                    .flatten();
                let drawn = match promoted {
                    Some((lines, continues)) => (lines, continues),
                    None => {
                        self.draw_frame(&fail_key, &key, extension, flat, width, frame, call)?
                    }
                };
                // A fresh draw that declines to continue stays stopped,
                // even under reduced motion, so leaving reduced motion
                // recalls nothing.
                let kind = if drawn.1 {
                    if reduced {
                        AnimKind::Reduced
                    } else {
                        AnimKind::Active
                    }
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
                self.store_frame(&key, width, drawn.0, true)
            }
            Some(state) if state.kind == AnimKind::Active && reduced => {
                // Reduced motion stills at frame 0 at the next paint,
                // unless the draw declines to continue: then it stays
                // stopped, so leaving reduced motion recalls nothing.
                let drawn = self.draw_frame(&fail_key, &key, extension, flat, width, 0, call)?;
                self.set_anim(
                    &key,
                    AnimState {
                        frame: 0,
                        last: now,
                        interval,
                        kind: if drawn.1 {
                            AnimKind::Reduced
                        } else {
                            AnimKind::Stopped
                        },
                    },
                );
                self.store_frame(&key, width, drawn.0, true)
            }
            Some(state) if state.kind == AnimKind::Reduced && !reduced => {
                // Reactivation paints frame 1: the still form at 0
                // established the counter, which resumes.
                let drawn = self.draw_frame(&fail_key, &key, extension, flat, width, 1, call)?;
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
                self.store_frame(&key, width, drawn.0, true)
            }
            Some(state) => {
                let due = now
                    .checked_duration_since(state.last)
                    .is_some_and(|elapsed| elapsed >= effective_interval(state.interval));
                if state.kind == AnimKind::Active && painted && due {
                    let frame = state.frame.saturating_add(1);
                    let drawn =
                        self.draw_frame(&fail_key, &key, extension, flat, width, frame, call)?;
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
                    self.store_frame(&key, width, drawn.0, true)
                } else if let Some(hit) = self.stored_entry(&key, width) {
                    Some(hit)
                } else {
                    // A width change or an early re-render reuses the
                    // stored frame with the counter, last, markers and
                    // due unchanged. A paint promotes the count's stored
                    // result at the same width and frame instead of
                    // calling again.
                    let promoted = promote
                        .then(|| self.promoted(&(fail_key.clone(), notice_id, width), state.frame))
                        .flatten();
                    let drawn = match promoted {
                        Some((lines, continues)) => (lines, continues),
                        None => self.draw_frame(
                            &fail_key,
                            &key,
                            extension,
                            flat,
                            width,
                            state.frame,
                            call,
                        )?,
                    };
                    self.store_frame(&key, width, drawn.0, false)
                }
            }
        }
    }

    /// A closed draw: hits stored animated lines, else calls at the
    /// stored frame or 0 and caches the result in the ordinary
    /// input-and-width entries, with no animation-state write or mark. A
    /// notice's closed lines file under its instance, so pruning the
    /// instance drops them; items file under no notice.
    #[allow(
        clippy::too_many_arguments,
        reason = "the keys, draw flags and callback describe one animated row"
    )]
    fn closed_draw(
        &self,
        fail_key: &FailKey,
        notice_id: Option<usize>,
        key: &AnimKey,
        extension: &str,
        flat: bool,
        width: u16,
        call: impl FnOnce(u64) -> (Decision, bool),
    ) -> Option<Arc<Vec<FittedLine>>> {
        if let Some(hit) = self.stored_entry(key, width) {
            return Some(hit);
        }
        let frame = support::lock(&self.anim)
            .states
            .get(key)
            .map(|state| state.frame);
        let entry = (fail_key.clone(), notice_id, width);
        if frame.is_none()
            && let Some(hit) = support::lock(&self.entries).get(&entry).cloned()
        {
            return Some(hit);
        }
        if let Some((lines, _)) = self.promoted(&entry, frame.unwrap_or(0)) {
            return Some(Arc::new(lines));
        }
        let drawn_at = frame.unwrap_or(0);
        let (lines, continues) =
            self.draw_frame(fail_key, key, extension, flat, width, drawn_at, call)?;
        let lines = Arc::new(lines);
        support::lock(&self.entries).insert(entry.clone(), Arc::clone(&lines));
        support::lock(&self.closed).insert(
            entry,
            ClosedLines {
                lines: Arc::clone(&lines),
                frame: drawn_at,
                continues,
            },
        );
        Some(lines)
    }

    /// A stored closed draw for `entry` at `frame`: its lines with
    /// whether the item continues. A paint promotes the result instead
    /// of calling again; any other frame draws fresh.
    fn promoted(&self, entry: &EntryKey, frame: u64) -> Option<(Vec<FittedLine>, bool)> {
        let closed = support::lock(&self.closed).get(entry).cloned()?;
        (closed.frame == frame).then(|| ((*closed.lines).clone(), closed.continues))
    }

    /// The animation dues of items painted this frame: each active one's
    /// last tick with its interval, for the wake fold. The fold asks each
    /// one's next frame at its last tick plus its interval. Rotates the
    /// painted sets and closes the frame. Production folds through
    /// [`Cache::anim_dues_live`]; this unfiltered seam is test-only.
    #[cfg(test)]
    pub(crate) fn anim_dues(&self) -> Vec<(Instant, Duration)> {
        self.dues_filtered(None)
    }

    /// The animation dues for the live notice set: like [`Cache::anim_dues`],
    /// but a painted notice no longer stacked contributes no due, so a
    /// notice expiring in the post-draw sweep arms no animation timer.
    /// Keys without a notice always count, so item dues never change.
    pub(crate) fn anim_dues_live(&self, live_ids: &[usize]) -> Vec<(Instant, Duration)> {
        self.dues_filtered(Some(live_ids))
    }

    /// Rotates the painted sets, closes the frame, and returns each
    /// active painted key's due, dropping notice keys outside `live`.
    fn dues_filtered(&self, live: Option<&[usize]>) -> Vec<(Instant, Duration)> {
        let mut anim = support::lock(&self.anim);
        let painted = std::mem::take(&mut anim.painted_this);
        anim.painted_last = painted.clone();
        if anim.at.take().is_none() {
            return Vec::new();
        }
        painted
            .into_iter()
            .filter_map(|key| {
                if let Some(live_ids) = live
                    && key.notice_id().is_some_and(|id| !live_ids.contains(&id))
                {
                    return None;
                }
                let state = anim.states.get(&key)?;
                (state.kind == AnimKind::Active).then_some((state.last, state.interval))
            })
            .collect()
    }

    /// Whether a painted animation changed a resident page's rendered
    /// line count since its last recount.
    pub(crate) fn has_changed_pages(&self) -> bool {
        !support::lock(&self.anim).changed_pages.is_empty()
    }

    /// Takes pages whose animated line count changed during paint.
    pub(crate) fn take_changed_pages(&self) -> Vec<PageKey> {
        support::lock(&self.anim).changed_pages.drain().collect()
    }

    /// The fitted lines stored for `key` at `width`, if any.
    fn stored_entry(&self, key: &AnimKey, width: u16) -> Option<Arc<Vec<FittedLine>>> {
        support::lock(&self.anim)
            .entries
            .get(key)?
            .get(&width)
            .cloned()
    }

    /// The stored animated lines for cache assertions. A miss reads as
    /// none; this test-only view never invokes or writes.
    #[cfg(test)]
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
        let key = AnimKey::new(page, slot, input_key, extension, half);
        self.stored_entry(&key, width)
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
        let mut anim = support::lock(&self.anim);
        if new_frame {
            let count = stored.len();
            let changed = anim
                .line_counts
                .insert(key.clone(), count)
                .is_some_and(|previous| previous != count);
            if changed && key.page().index != usize::MAX {
                anim.changed_pages.insert(key.page().clone());
            }
            anim.entries.remove(key);
        }
        anim.entries
            .entry(key.clone())
            .or_default()
            .insert(width, Arc::clone(&stored));
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
        support::lock(&self.closed).retain(|key, _| key.0.0 != page);
        // Animation counters, entries and pending recounts drop with the
        // page: a returning item restarts at frame 0.
        let mut anim = support::lock(&self.anim);
        anim.entries.retain(|key, _| key.page() != &page);
        anim.states.retain(|key, _| key.page() != &page);
        anim.line_counts.retain(|key, _| key.page() != &page);
        anim.painted_this.retain(|key| key.page() != &page);
        anim.painted_last.retain(|key| key.page() != &page);
        anim.changed_pages.remove(&page);
        if self.owner.load(Ordering::Relaxed) {
            support::lock(&self.failed).retain(|key| key.0 != page);
            support::lock(&self.pending)
                .failures
                .retain(|(at, _, _)| *at != page);
        }
    }

    /// Drops every per-notice store for instances outside `live_ids`:
    /// animation state, both painted sets, stored animated lines, line
    /// counts and instance-filed closed-draw lines. Item keys carry no
    /// notice and stay, as do instance-free failure memos. A stale state
    /// waits in an unrendered cache until that cache's next render, which
    /// prunes before painting, so pruned state is never painted or reused.
    pub(crate) fn retain_notices(&self, live_ids: &[usize]) {
        let live = |key: &AnimKey| match key.notice_id() {
            None => true,
            Some(id) => live_ids.contains(&id),
        };
        let mut anim = support::lock(&self.anim);
        anim.states.retain(|key, _| live(key));
        anim.painted_this.retain(|key| live(key));
        anim.painted_last.retain(|key| live(key));
        anim.entries.retain(|key, _| live(key));
        anim.line_counts.retain(|key, _| live(key));
        drop(anim);
        support::lock(&self.entries)
            .retain(|(_, notice_id, _), _| notice_id.is_none_or(|id| live_ids.contains(&id)));
        support::lock(&self.closed)
            .retain(|(_, notice_id, _), _| notice_id.is_none_or(|id| live_ids.contains(&id)));
    }

    /// Whether any per-notice store retains `id`: animation state, either
    /// painted set, stored animated lines, line counts or closed-draw
    /// lines. A miss reads as none; this test-only view never invokes.
    #[cfg(test)]
    pub(crate) fn has_notice_state(&self, id: usize) -> bool {
        let anim = support::lock(&self.anim);
        let keyed = |key: &AnimKey| key.notice_id() == Some(id);
        anim.states.keys().any(keyed)
            || anim.painted_this.iter().any(keyed)
            || anim.painted_last.iter().any(keyed)
            || anim.entries.keys().any(keyed)
            || anim.line_counts.keys().any(keyed)
            || support::lock(&self.entries)
                .keys()
                .any(|(_, notice_id, _)| *notice_id == Some(id))
    }

    pub(crate) fn drop_entries(&self) {
        // A width change keeps the frame counter, the last time, the line
        // count and every still and stopped marker: only rendered entries go.
        support::lock(&self.entries).clear();
        support::lock(&self.closed).clear();
        support::lock(&self.anim).entries.clear();
    }

    pub(crate) fn clear(&self) {
        self.drop_entries();
        let mut anim = support::lock(&self.anim);
        anim.at = None;
        anim.painted_this.clear();
        anim.painted_last.clear();
        anim.states.clear();
        anim.line_counts.clear();
        anim.changed_pages.clear();
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
        Input::WorkingLine(line) => (line.key(slot), line.flat),
        Input::SteeringQueue(queue) => (queue.key(slot), queue.flat),
        Input::StatusRows(rows) => (rows.key(slot), rows.flat),
        Input::Card(card) => (card.key(slot), card.flat()),
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
