//! The terminal's operation timers (`docs/state.md`, "Diagnostic logs"): the
//! diagnostic writer the loop's timed scopes share. Every timed place goes
//! through `log::diag::timed`, so the terminal's lines match every other
//! process's.

use std::sync::Arc;

use contract::diag::Op;

/// The loop's timing state: the diagnostic writer its scopes share, and
/// the attach latches. Each latched operation writes at most once per
/// process: a reconnect, a delegate swap or a second attach never writes
/// a second line.
pub(crate) struct Timing {
    diag: Arc<log::diag::Diag>,
    log_read: bool,
    transcript: bool,
    attach_summary: bool,
    tail_wait: bool,
    preview_fold: bool,
    tail_install: bool,
    attach_setup: Option<log::diag::Timer>,
    attach_wait: Option<log::diag::Timer>,
    attach_tail: Option<log::diag::Timer>,
    attach_prepare: Option<log::diag::Timer>,
    attach_draw: Option<log::diag::Timer>,
    attach_done: bool,
}

impl Timing {
    /// Shares `diag` with the loop's scopes; all latches start unset.
    pub(crate) fn new(diag: Arc<log::diag::Diag>) -> Self {
        Self {
            diag,
            log_read: false,
            transcript: false,
            attach_summary: false,
            tail_wait: false,
            preview_fold: false,
            tail_install: false,
            attach_setup: None,
            attach_wait: None,
            attach_tail: None,
            attach_prepare: None,
            attach_draw: None,
            attach_done: false,
        }
    }

    /// A timing state whose writer records nothing: at `info` every timed
    /// call is one level check that returns `None`, so tests that assert no
    /// timing keep their signatures.
    #[cfg(test)]
    pub(crate) fn info() -> Self {
        Self::new(info_diag())
    }

    /// Starts the first render's guard, from entry to the first draw. The
    /// caller discards it on every pre-draw return and drops it after the
    /// draw: a line there times work that happened.
    pub(crate) fn first_render(&self) -> Option<log::diag::Timer> {
        log::diag::timed(&self.diag, Op::TuiFirstRender, None, None)
    }

    /// Starts alongside the first render's guard. The caller holds it
    /// until the first draw with attached-session history and discards it
    /// if that draw never happens.
    pub(crate) fn attached_frame(&self) -> Option<log::diag::Timer> {
        log::diag::timed(&self.diag, Op::TuiAttachedFrame, None, None)
    }

    /// Starts the first attach's first fetched page guard, once per
    /// process. The caller holds it across the first actual `page_in`
    /// `hub.fetch` attempt to completion, whatever its outcome: an
    /// attempted read is work.
    pub(crate) fn log_read(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.log_read, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiLogRead, None, None)
    }

    /// Starts the first successful page-load guard, once per process.
    /// The caller holds it across the first successful `app.load`; a
    /// failed fetch never takes it, and an attach with no stream returns
    /// before it, so either leaves the latch for the next load.
    pub(crate) fn transcript(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.transcript, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiTranscript, None, None)
    }

    /// Starts a frame's guard. A frame within its 16 ms budget writes no
    /// line; the suppression lives in the timer's drop.
    pub(crate) fn frame(&self) -> Option<log::diag::Timer> {
        log::diag::timed(&self.diag, Op::TuiFrame, None, None)
    }

    /// Starts the extension load's guard. Loading runs once before the
    /// first frame, so no latch: one line per process.
    pub(crate) fn extensions(&self) -> Option<log::diag::Timer> {
        log::diag::timed(&self.diag, Op::TuiExtensions, None, None)
    }

    /// Starts the opening summary subscribe's guard, once per process.
    /// The loop drops it once the open leaves the summary stage, so one
    /// line covers the send through the acceptance that queues the
    /// session's commands line.
    pub(crate) fn attach_summary(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.attach_summary, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiAttachSummary, None, None)
    }

    /// Starts the open's pre-frame tail read guard, once per process.
    /// The caller holds it across the whole tail phase to completion,
    /// whatever its outcome; the post-frame count never takes it.
    pub(crate) fn tail_wait(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.tail_wait, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiTailWait, None, None)
    }

    /// Starts the first answer fold's guard, once per process. The
    /// initial seed is empty, and later folds do not write a second line.
    pub(crate) fn preview_fold(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.preview_fold, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiPreviewFold, None, None)
    }

    /// Starts the open's pre-frame install guard, once per process.
    /// The caller holds it across the preview install to completion,
    /// whatever its outcome. The install puts the newest candidate on
    /// screen without another fold (`docs/tui.md`, "History and
    /// paging"); when no answer folded one, the install folds the
    /// leftover candidate first, under this same guard.
    pub(crate) fn tail_install(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.tail_install, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiTailInstall, None, None)
    }

    /// Starts the attach setup guard at `run_with` entry, once per
    /// process: a start while one runs keeps it, and none starts after
    /// the spans emit or discard.
    pub(crate) fn start_attach_setup(&mut self) {
        start_span(
            &mut self.attach_setup,
            self.attach_done,
            &self.diag,
            Op::TuiAttachSetup,
        );
    }

    /// Stamps the setup end after the initial unattached draw; the
    /// retained guard still writes only at the attached-history draw.
    pub(crate) fn stop_attach_setup(&mut self) {
        stop_span(&mut self.attach_setup);
    }

    /// Starts the attach wait guard after the initial draw, once per
    /// process: a start while one runs keeps it, and none starts after
    /// the spans emit or discard.
    pub(crate) fn start_attach_wait(&mut self) {
        start_span(
            &mut self.attach_wait,
            self.attach_done,
            &self.diag,
            Op::TuiAttachWait,
        );
    }

    /// Stamps the wait end at the first due tail attempt; the retained
    /// guard still writes only at the attached-history draw.
    pub(crate) fn stop_attach_wait(&mut self) {
        stop_span(&mut self.attach_wait);
    }

    /// Starts the attach tail guard at the first due tail attempt, once
    /// per process: a start while one runs keeps the first attempt's
    /// guard across false-returning attempts, and none starts after
    /// the spans emit or discard.
    pub(crate) fn start_attach_tail(&mut self) {
        start_span(
            &mut self.attach_tail,
            self.attach_done,
            &self.diag,
            Op::TuiAttachTail,
        );
    }

    /// Stamps the tail end at the install the successful attempt
    /// reaches; the retained guard still writes only at the
    /// attached-history draw.
    pub(crate) fn stop_attach_tail(&mut self) {
        stop_span(&mut self.attach_tail);
    }

    /// Starts the attach prepare guard at the installed tail return,
    /// once per process: a start while one runs keeps it, and none
    /// starts after the spans emit or discard.
    pub(crate) fn start_attach_prepare(&mut self) {
        start_span(
            &mut self.attach_prepare,
            self.attach_done,
            &self.diag,
            Op::TuiAttachPrepare,
        );
    }

    /// Stamps the prepare end immediately before the attached-history
    /// draw; the retained guard still writes only at that draw.
    pub(crate) fn stop_attach_prepare(&mut self) {
        stop_span(&mut self.attach_prepare);
    }

    /// Starts the attach draw guard immediately before the
    /// attached-history draw, once per process: a start while one runs
    /// keeps it, and none starts after the spans emit or discard.
    pub(crate) fn start_attach_draw(&mut self) {
        start_span(
            &mut self.attach_draw,
            self.attach_done,
            &self.diag,
            Op::TuiAttachDraw,
        );
    }

    /// Stamps the draw end at its completion; the retained guard still
    /// writes only when the draw held attached-session history.
    pub(crate) fn stop_attach_draw(&mut self) {
        stop_span(&mut self.attach_draw);
    }

    /// Writes the five retained spans at the first attached-history
    /// draw, once per process: each line covers its stamped boundary,
    /// in boundary order.
    pub(crate) fn emit_attach_spans(&mut self) {
        if self.attach_done {
            return;
        }
        self.attach_done = true;
        drop(self.attach_setup.take());
        drop(self.attach_wait.take());
        drop(self.attach_tail.take());
        drop(self.attach_prepare.take());
        drop(self.attach_draw.take());
    }

    /// Discards the five spans without writing, when the parent
    /// attached-frame guard is discarded; once per process, so nothing
    /// starts afterwards.
    pub(crate) fn discard_attach_spans(&mut self) {
        if self.attach_done {
            return;
        }
        self.attach_done = true;
        discard(self.attach_setup.take());
        discard(self.attach_wait.take());
        discard(self.attach_tail.take());
        discard(self.attach_prepare.take());
        discard(self.attach_draw.take());
    }
}

/// Starts one attach span guard unless the spans already emitted or
/// discarded, or one already runs: the first start wins, so a later
/// attempt never restarts its span.
fn start_span(
    slot: &mut Option<log::diag::Timer>,
    done: bool,
    diag: &Arc<log::diag::Diag>,
    op: Op,
) {
    if done || slot.is_some() {
        return;
    }
    *slot = log::diag::timed(diag, op, None, None);
}

/// Stamps one attach span guard's end; a guard that never started, or
/// already stopped, is untouched.
fn stop_span(slot: &mut Option<log::diag::Timer>) {
    if let Some(timer) = slot.as_mut() {
        timer.stop();
    }
}

/// Drops `timer` without writing: setup work before a frame that never
/// drew must never land as that frame's line.
pub(crate) fn discard(timer: Option<log::diag::Timer>) {
    if let Some(timer) = timer {
        timer.discard();
    }
}

/// An info-level `tui` writer that records nothing, for callers that pass a
/// writer but assert no timing.
#[cfg(test)]
pub(crate) fn info_diag() -> Arc<log::diag::Diag> {
    Arc::new(log::diag::Diag::new(
        &std::env::temp_dir(),
        log::diag::Process::Tui,
        log::diag::Level::Info,
        Arc::new(support::clock::System),
    ))
}

/// The attach spans' state machine (`docs/state.md`, "Diagnostic logs"):
/// each span starts and stops at its live boundary, the stopped guards
/// are retained until the first attached-history draw writes them all,
/// and a parent discard writes none. At `info` every start holds
/// nothing, so the same calls stay silent.
#[cfg(test)]
mod attach_span_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime};

    use contract::clock::Clock;
    use fakes::clock::FakeClock;

    use super::Timing;

    /// A clock stepping 20 ms on every monotonic read, so each stamped
    /// boundary lands on its own instant with no sleep: a span's whole
    /// milliseconds pin the exact read sequence from its start to its
    /// stop.
    struct StepClock {
        base: Instant,
        reads: AtomicU64,
    }

    impl StepClock {
        fn stepped() -> Arc<Self> {
            Arc::new(Self {
                base: FakeClock::new().now(),
                reads: AtomicU64::new(0),
            })
        }
    }

    impl Clock for StepClock {
        fn now(&self) -> Instant {
            let read = self.reads.fetch_add(1, Ordering::SeqCst);
            self.base + Duration::from_millis(20) * u32::try_from(read).unwrap_or(u32::MAX)
        }

        fn wall(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }

        fn sleep(&self, _d: Duration) {}

        fn wait_until(&self, _until: Option<Instant>, _wait: &mut dyn FnMut(Option<Duration>)) {}

        fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
    }

    /// A debug writer in `home` on the stepping clock, with the timing
    /// state sharing it: every timer start and stop reads the steps.
    fn debug_timing(home: &std::path::Path) -> Timing {
        let clock: Arc<dyn Clock> = StepClock::stepped();
        Timing::new(Arc::new(log::diag::Diag::new(
            home,
            log::diag::Process::Tui,
            log::diag::Level::Debug,
            clock,
        )))
    }

    /// Every `op_timing` line in the process's diagnostic file, in file
    /// order; none when nothing was ever emitted there.
    fn op_lines(home: &std::path::Path) -> Vec<serde_json::Value> {
        let path = home
            .join("logs")
            .join(format!("tui-{}.log", std::process::id()));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        text.lines()
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|err| panic!("a line parses: {err}"))
            })
            .collect()
    }

    /// The wire promise on every attach span: exactly the keys `op` and
    /// `ms` with an unsigned whole `ms`, on the terminal's debug line.
    #[track_caller]
    fn assert_span_wire(lines: &[serde_json::Value], op: &str) {
        assert_eq!(lines.len(), 1, "one {op} line: {lines:?}");
        let line = &lines[0];
        assert_eq!(line["code"], "op_timing", "{line}");
        assert_eq!(line["level"], "debug", "{line}");
        assert_eq!(line["process"], "tui", "{line}");
        assert!(line.get("session_id").is_none(), "{line}");
        assert_eq!(line["message"], "An operation ended.", "{line}");
        let data = line["data"].as_object().expect("data is an object");
        let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["ms", "op"], "{line}");
        assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
        assert_eq!(line["data"]["op"], op, "{line}");
    }

    /// One line per span when the attach reaches its draw, in boundary
    /// order, each covering exactly its own step: a span that starts
    /// twice, stops never, or emits twice fails here.
    #[test]
    fn attach_spans_emit_in_boundary_order_with_their_own_steps() {
        let home = fakes::TempDir::new("tui-attach-spans-order");
        let mut timing = debug_timing(home.path());
        timing.start_attach_setup();
        timing.stop_attach_setup();
        timing.start_attach_wait();
        timing.stop_attach_wait();
        timing.start_attach_tail();
        timing.stop_attach_tail();
        timing.start_attach_prepare();
        timing.stop_attach_prepare();
        timing.start_attach_draw();
        timing.stop_attach_draw();
        // Nothing is written before the attached-history draw: the
        // stopped guards are retained, not emitted.
        assert!(op_lines(home.path()).is_empty());
        timing.emit_attach_spans();
        let lines = op_lines(home.path());
        let ops: Vec<&str> = lines
            .iter()
            .map(|line| line["data"]["op"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            ops,
            [
                "tui.attach_setup",
                "tui.attach_wait",
                "tui.attach_tail",
                "tui.attach_prepare",
                "tui.attach_draw"
            ]
        );
        for (line, op) in lines.iter().zip(ops.clone()) {
            assert_span_wire(std::slice::from_ref(line), op);
            assert_eq!(line["data"]["ms"], 20, "{op} covers one step: {line}");
        }
    }

    /// A second tail start while the first attempt is still held keeps
    /// the first guard: one tail line, still covering the first start.
    #[test]
    fn attach_tail_keeps_its_first_start_across_attempts() {
        let home = fakes::TempDir::new("tui-attach-spans-retain");
        let mut timing = debug_timing(home.path());
        timing.start_attach_setup();
        timing.stop_attach_setup();
        timing.start_attach_wait();
        timing.stop_attach_wait();
        timing.start_attach_tail();
        // A later false-returning attempt must not restart the span.
        timing.start_attach_tail();
        timing.stop_attach_tail();
        timing.start_attach_prepare();
        timing.stop_attach_prepare();
        timing.start_attach_draw();
        timing.stop_attach_draw();
        timing.emit_attach_spans();
        let lines = op_lines(home.path());
        assert_eq!(lines.len(), 5, "one line per span: {lines:?}");
        let tails: Vec<serde_json::Value> = lines
            .iter()
            .filter(|line| line["data"]["op"] == "tui.attach_tail")
            .cloned()
            .collect();
        assert_span_wire(&tails, "tui.attach_tail");
        assert_eq!(tails[0]["data"]["ms"], 20, "{tails:?}");
    }

    /// The emit and the discard each run once per process: a second
    /// emit writes nothing more, and starts after either write nothing.
    #[test]
    fn attach_spans_emit_and_discard_once_per_process() {
        let home = fakes::TempDir::new("tui-attach-spans-once");
        let mut timing = debug_timing(home.path());
        timing.start_attach_setup();
        timing.stop_attach_setup();
        timing.start_attach_wait();
        timing.stop_attach_wait();
        timing.start_attach_tail();
        timing.stop_attach_tail();
        timing.start_attach_prepare();
        timing.stop_attach_prepare();
        timing.start_attach_draw();
        timing.stop_attach_draw();
        timing.emit_attach_spans();
        timing.emit_attach_spans();
        timing.start_attach_tail();
        timing.emit_attach_spans();
        assert_eq!(op_lines(home.path()).len(), 5);
    }

    /// A parent discard writes none of the five, stopped or running,
    /// and nothing starts afterwards: an attach that never draws leaves
    /// no span line.
    #[test]
    fn attach_spans_discarded_write_none_and_block_restarts() {
        let home = fakes::TempDir::new("tui-attach-spans-discard");
        let mut timing = debug_timing(home.path());
        timing.start_attach_setup();
        timing.stop_attach_setup();
        timing.start_attach_wait();
        timing.discard_attach_spans();
        timing.start_attach_tail();
        timing.stop_attach_tail();
        timing.start_attach_prepare();
        timing.stop_attach_prepare();
        timing.start_attach_draw();
        timing.stop_attach_draw();
        timing.emit_attach_spans();
        assert!(op_lines(home.path()).is_empty());
    }

    /// At `info` the same boundary calls hold nothing and write no
    /// file: silence needs no latches.
    #[test]
    fn attach_spans_at_info_write_nothing() {
        let home = fakes::TempDir::new("tui-attach-spans-info");
        let mut timing = Timing::info();
        timing.start_attach_setup();
        timing.stop_attach_setup();
        timing.start_attach_wait();
        timing.stop_attach_wait();
        timing.start_attach_tail();
        timing.stop_attach_tail();
        timing.start_attach_prepare();
        timing.stop_attach_prepare();
        timing.start_attach_draw();
        timing.stop_attach_draw();
        timing.emit_attach_spans();
        assert!(op_lines(home.path()).is_empty());
    }
}
