//! A tool group: everything between two pieces of assistant text, by step,
//! with its thinking blocks, calls, failed model calls and the calls still
//! streaming (`docs/tui.md`, "Tool groups and the ledger"). This module
//! draws it: its summary line and ledger.

use contract::events::{CallStatus, FileChange};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use super::target_id;
use crate::app::Target;
use crate::ext::{
    CallState, GroupLine, Input, LedgerResult, LedgerRow, PromptImage, RowError, TurnCall,
    TurnCallResult, TurnFailed, TurnGroup, TurnGroupState, TurnSection, TurnStreaming, TurnThought,
};
use crate::format::{Kind, Kinds, count, dim, heading, kind, label, opened, summary};
use crate::rows::{RowText, Rows};
use crate::theme::{Role, style};

/// Everything between two pieces of assistant text.
#[derive(Debug, Clone, Default)]
pub(crate) struct Group {
    /// The first call's or thought's action, used as a stable target across reloads.
    pub(crate) key: Option<String>,
    /// Whether its ledger is open.
    pub(crate) open: bool,
    pub(crate) first: u64,
    pub(crate) last: u64,
    pub(crate) sections: Vec<Section>,
    /// Calls the model is still emitting.
    pub(crate) streaming: Vec<Streaming>,
    /// Its turn was cut short.
    pub(crate) cut: bool,
}

/// One step's part of a group.
#[derive(Debug, Clone, Default)]
pub(crate) struct Section {
    pub(crate) step: u64,
    pub(crate) thoughts: Vec<Thought>,
    pub(crate) calls: Vec<Call>,
    /// Model calls that failed and were retried: code and attempt.
    pub(crate) failed: Vec<(String, u32)>,
}

/// One thinking block.
#[derive(Debug, Clone, Default)]
pub(crate) struct Thought {
    pub(crate) id: usize,
    pub(crate) action: String,
    pub(crate) text: String,
    pub(crate) started: u64,
    pub(crate) ended: Option<u64>,
    pub(crate) open: bool,
}

/// One tool call.
#[derive(Debug, Clone, Default)]
pub(crate) struct Call {
    pub(crate) id: usize,
    pub(crate) action: String,
    pub(crate) name: String,
    pub(crate) arguments: Value,
    /// The outer call this call ran inside, from `ran_by`'s
    /// `outer_action_id`; `None` on the model's own calls and on
    /// `command_id`-anchored inners, which never nest.
    pub(crate) outer_action_id: Option<contract::ActionId>,
    /// How it ended; `None` while it runs.
    pub(crate) status: Option<CallStatus>,
    pub(crate) changes: Vec<FileChange>,
    /// What only an opened call reads, boxed so the `Call` the paging
    /// and the re-count walk stays one pointer; `None` while it runs.
    pub(crate) result: Option<Box<CallResult>>,
    /// The process's exit code, when it ran one
    /// (`tool_call_completed`'s `process.exit_code`).
    pub(crate) exit: Option<i32>,
    /// How many lines the result's text parts hold, a trailing newline
    /// adding none (`str::lines`).
    pub(crate) lines: u64,
    /// The image parts of its result: each draws as its one line under
    /// its ledger row (`docs/tui.md`, "Images").
    pub(crate) images: Vec<crate::image::Part>,
    pub(crate) open: bool,
    /// A `permission_requested` for it is open.
    pub(crate) asking: bool,
    /// When it started and how long it took: a denied call never starts.
    pub(crate) timing: Timing,
    /// The question form it raised.
    pub(crate) asked: Option<super::answers::Asked>,
}

/// What only an opened call reads: the result's `details` as JSON
/// text, parsed when the call is opened and never on load
/// (`docs/tools.md`, "What a result carries"), the full output's
/// path, the kept byte ranges, the signal's name, the headline and
/// the text parts joined with "\n". A `null` details on the wire
/// is stored as none; a details string that fails to parse falls
/// back to the plain-content view.
#[derive(Debug, Clone, Default)]
pub(crate) struct CallResult {
    pub(crate) details: Option<String>,
    /// The full output's path, when the result was cut or a hook
    /// returned text for it; kept, never drawn inline.
    #[allow(
        dead_code,
        reason = "read by the call's whole view in #2386; pinned by turn_tests"
    )]
    pub(crate) artifact: Option<String>,
    /// On a cut result, ordered byte ranges of the full output held in
    /// `content`; kept, never drawn inline.
    #[allow(
        dead_code,
        reason = "read by the call's whole view in #2386; pinned by turn_tests"
    )]
    pub(crate) kept: Option<Vec<[u64; 2]>>,
    /// The signal's name when a signal ended the call's process.
    #[allow(
        dead_code,
        reason = "read by the call's whole view in #2386; pinned by turn_tests"
    )]
    pub(crate) signal: Option<String>,
    /// A failed call's "<code>: <message>" or a denial's reason, else
    /// empty: the first row an opened call draws.
    pub(crate) headline: String,
    /// The result's text parts joined with "\n".
    pub(crate) content: String,
    /// The protected-path warning lines, one per changed path, folded
    /// from the completion's report. The ledger row shows only the
    /// result's first line, so the warning draws as its own row beneath
    /// the call; the opened view shows the result text, which carries
    /// the same notice unless a hook rewrote it.
    pub(crate) protected: Vec<String>,
}

/// When a call started and how long it took: one field, so no call can
/// hold two timings at once (`docs/code-quality.md`, "Types").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Timing {
    /// No `tool_call_started` and no completion.
    #[default]
    Unstarted,
    /// Started and not completed: the start's timestamp.
    Started(u64),
    /// Completed: the completion's timestamp less the start's, in milliseconds.
    Took(u64),
}

impl Call {
    /// Whether the call started: a cut turn's started call may have run.
    pub(crate) fn has_started(&self) -> bool {
        !matches!(self.timing, Timing::Unstarted)
    }

    /// How long the call took, in milliseconds: some only once completed.
    pub(crate) fn duration_ms(&self) -> Option<u64> {
        match self.timing {
            Timing::Took(took) => Some(took),
            Timing::Unstarted | Timing::Started(_) => None,
        }
    }

    /// The result's JSON text, when the wire carried details.
    pub(crate) fn details_text(&self) -> Option<&str> {
        self.result
            .as_deref()
            .and_then(|result| result.details.as_deref())
    }

    /// A failed call's headline, else empty.
    pub(crate) fn headline(&self) -> &str {
        self.result
            .as_deref()
            .map(|result| result.headline.as_str())
            .unwrap_or("")
    }

    /// The result's text parts joined with "\n", else empty.
    pub(crate) fn content(&self) -> &str {
        self.result
            .as_deref()
            .map(|result| result.content.as_str())
            .unwrap_or("")
    }

    /// The protected-path warning lines, else none.
    pub(crate) fn protected(&self) -> &[String] {
        self.result
            .as_deref()
            .map(|result| result.protected.as_slice())
            .unwrap_or_default()
    }
}

/// A call still streaming: its message, position, name and raw text.
#[derive(Debug, Clone)]
pub(crate) struct Streaming {
    pub(crate) message: String,
    pub(crate) index: u32,
    pub(crate) name: Option<String>,
    pub(crate) text: String,
}

impl Group {
    /// Widens the group's span to `ts`.
    pub(in crate::turn) fn touch(&mut self, ts: u64) {
        self.first = self.first.min(ts);
        self.last = self.last.max(ts);
    }

    /// [`Self::touch`], as a condition that holds.
    pub(in crate::turn) fn touched(&mut self, ts: u64) -> bool {
        self.touch(ts);
        true
    }

    /// The section for `step`, the last one when it is that step's.
    pub(in crate::turn) fn section(&mut self, step: u64) -> &mut Section {
        if self.sections.last().is_none_or(|last| last.step != step) {
            self.sections.push(Section {
                step,
                ..Section::default()
            });
        }
        let at = self.sections.len().saturating_sub(1);
        #[expect(clippy::indexing_slicing, reason = "a section was pushed above")]
        &mut self.sections[at]
    }

    pub(in crate::turn) fn thought(&mut self, action: &str) -> Option<&mut Thought> {
        self.sections
            .iter_mut()
            .flat_map(|section| section.thoughts.iter_mut())
            .find(|thought| thought.action == action)
    }

    pub(in crate::turn) fn call(&mut self, action: &str) -> Option<&mut Call> {
        self.sections
            .iter_mut()
            .flat_map(|section| section.calls.iter_mut())
            .find(|call| call.action == action)
    }

    pub(crate) fn calls(&self) -> impl Iterator<Item = &Call> {
        self.sections
            .iter()
            .flat_map(|section| section.calls.iter())
    }

    pub(crate) fn thoughts(&self) -> impl Iterator<Item = &Thought> {
        self.sections
            .iter()
            .flat_map(|section| section.thoughts.iter())
    }

    pub(in crate::turn) fn set_open(&mut self, target: &Target, open: bool) -> bool {
        let flag = match target {
            Target::Group(id) => self
                .key
                .as_deref()
                .is_some_and(|key| target_id(key) == *id)
                .then_some(&mut self.open),
            Target::Thought(id) => self
                .sections
                .iter_mut()
                .flat_map(|section| section.thoughts.iter_mut())
                .find(|thought| thought.id == *id)
                .map(|thought| &mut thought.open),
            Target::Call(id) => self
                .sections
                .iter_mut()
                .flat_map(|section| section.calls.iter_mut())
                .find(|call| call.id == *id)
                .map(|call| &mut call.open),
            Target::Login
            | Target::Image(_)
            | Target::Note(_)
            | Target::Orphans(_)
            | Target::CallView(_)
            | Target::Copy { .. } => None,
        };
        flag.map(|flag| *flag = open).is_some()
    }

    /// Whether the group holds calls or failed model calls, and so a
    /// ledger.
    pub(crate) fn has_ledger(&self) -> bool {
        self.calls().next().is_some() || self.failed() > 0
    }

    /// How many model calls failed in the group.
    pub(crate) fn failed(&self) -> usize {
        self.sections
            .iter()
            .map(|section| section.failed.len())
            .sum()
    }
}

impl Group {
    /// What its calls did, by kind, and how often the model thought.
    /// Read, edited and written files count distinct `path` argument
    /// strings per kind; a call with no string `path` counts as one
    /// file of its own. The added and removed totals sum the `changes`
    /// of the `edit` calls.
    fn kinds(&self) -> Kinds {
        let mut kinds = Kinds::default();
        let mut read_paths: Vec<&str> = Vec::new();
        let mut read_missing = 0u64;
        let mut edit_paths: Vec<&str> = Vec::new();
        let mut edit_missing = 0u64;
        let mut write_paths: Vec<&str> = Vec::new();
        let mut write_missing = 0u64;
        // Every call with an inner pointing at it never counts as a
        // kind of its own, at any depth: the summary reads what the
        // inner calls did.
        let mut outers: Vec<&str> = Vec::new();
        for call in self.calls() {
            if let Some(outer) = call.outer_action_id.as_ref() {
                if !outers.contains(&outer.0.as_str()) {
                    outers.push(&outer.0);
                }
            }
        }
        for call in self.calls() {
            if outers.contains(&call.action.as_str()) {
                continue;
            }
            match kind(&call.name, &call.arguments) {
                Kind::Read => match call.arguments.get("path").and_then(Value::as_str) {
                    Some(path) => {
                        if !read_paths.contains(&path) {
                            read_paths.push(path);
                        }
                    }
                    None => read_missing = read_missing.saturating_add(1),
                },
                Kind::Search => kinds.searched = kinds.searched.saturating_add(1),
                Kind::List => kinds.listed = kinds.listed.saturating_add(1),
                Kind::Shell => kinds.ran = kinds.ran.saturating_add(1),
                Kind::Delegate => kinds.delegates = kinds.delegates.saturating_add(1),
                Kind::Jobs => kinds.jobs = kinds.jobs.saturating_add(1),
                Kind::Ask(questions) => kinds.asked = kinds.asked.saturating_add(questions),
                Kind::Other => kinds.called = kinds.called.saturating_add(1),
                Kind::Edit => {
                    match call.arguments.get("path").and_then(Value::as_str) {
                        Some(path) => {
                            if !edit_paths.contains(&path) {
                                edit_paths.push(path);
                            }
                        }
                        None => edit_missing = edit_missing.saturating_add(1),
                    }
                    for change in &call.changes {
                        kinds.added = kinds.added.saturating_add(change.added);
                        kinds.removed = kinds.removed.saturating_add(change.removed);
                    }
                }
                Kind::Write => match call.arguments.get("path").and_then(Value::as_str) {
                    Some(path) => {
                        if !write_paths.contains(&path) {
                            write_paths.push(path);
                        }
                    }
                    None => write_missing = write_missing.saturating_add(1),
                },
            }
        }
        kinds.read = read_paths.len() as u64 + read_missing;
        kinds.edited = edit_paths.len() as u64 + edit_missing;
        kinds.wrote = write_paths.len() as u64 + write_missing;
        kinds.thoughts = self.thoughts().count() as u64;
        kinds
    }

    /// The summary row's spans at `width`: the kinds with the failed and
    /// flight detail, the running dot, and the right-aligned duration
    /// and toggle; none when the body is empty. The seam's built-in and
    /// the row below share them.
    fn summary_spans(&self, running: bool, width: u16) -> Option<Vec<Span<'static>>> {
        let mut body: Vec<Span<'static>> = self.kinds().summary();
        let join = |body: &mut Vec<Span<'static>>| {
            if !body.is_empty() {
                body.push(dim_span(" · ".to_owned()));
            }
        };
        let failed = self.failed() as u64;
        if failed > 0 {
            join(&mut body);
            body.push(dim_span(count(
                failed,
                "failed model call",
                "failed model calls",
            )));
        }
        if running {
            let flight: Vec<String> = self
                .calls()
                .filter(|call| call.status.is_none())
                .map(|call| label(&call.name, &summary(&call.name, &call.arguments)))
                .chain(
                    self.streaming
                        .iter()
                        .map(|call| label(call.name.as_deref().unwrap_or("…"), &call.text)),
                )
                .collect();
            if !flight.is_empty() {
                join(&mut body);
                body.push(dim_span(flight.join(", ")));
            }
            if let Some(thought) = self.thoughts().filter(|t| t.ended.is_none()).last() {
                join(&mut body);
                let text = match heading(&thought.text, true) {
                    Some(heading) => format!("Thinking: {heading}"),
                    None => "Thinking".to_owned(),
                };
                body.push(Span::styled(
                    text,
                    Style::default()
                        .add_modifier(Modifier::DIM)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
        }
        // A running group's dot spins on the working line's tick in the
        // attention colour; a finished group's dot is dim, as is the
        // whole line (`docs/tui.md`, "The working line", "Turns").
        let mut left = Vec::new();
        if running {
            left.push(Span::styled("●", style(Role::Attention)));
        } else {
            left.push(dim_span("●".to_owned()));
        }
        left.push(dim_span(" ".to_owned()));
        // An empty body draws no row, as before: check before `left` takes
        // the body, since the body does not copy.
        let empty = body.is_empty();
        left.extend(body);
        // The duration and the toggle, right-aligned and dim. The toggle
        // follows the ledger's draw, not a search draw's (`docs/tui.md`,
        // "Search").
        let draws = self.open || self.calls().any(|call| call.asking);
        let right = vec![dim_span(format!(
            "{} {}",
            crate::format::duration(self.last.saturating_sub(self.first)),
            if draws { "▾" } else { "▸" },
        ))];
        if empty {
            return None;
        }
        // The row fills `width` exactly; what does not fit is cut from
        // the left part with "…", so the right part keeps its columns.
        let max = usize::from(width);
        let right_width = spans_width(&right);
        let left_width = spans_width(&left);
        let row = if left_width + 1 + right_width <= max {
            let mut row = left;
            row.push(dim_span(" ".repeat(max - left_width - right_width)));
            row.extend(right);
            row
        } else if max >= right_width + 2 {
            // One truncation: `fit` emits the row's single "…", so the
            // right part keeps its columns.
            let mut row = fit(left, max - right_width - 1);
            row.push(dim_span(" ".to_owned()));
            row.extend(right);
            row
        } else {
            let mut row = left;
            row.push(dim_span(" ".to_owned()));
            row.extend(right);
            fit(row, max)
        };
        Some(row)
    }

    /// Its lines. A group with no call is its thinking, one line
    /// a block; otherwise a summary line, and the ledger when open. The
    /// summary line is one row in `width` cells, whatever runs: the dot,
    /// the kinds in their fixed order, and, right-aligned, the group's
    /// duration and ▸ while closed or ▾ while open. What does not fit is
    /// cut with "…", so the duration and the toggle stay put. A call's
    /// images draw as their one line each under its ledger row
    /// (`docs/tui.md`, "Images"). A bound `group_line` renderer replaces
    /// the summary line; the ledger still draws below it.
    pub(crate) fn rows(
        &self,
        running: bool,
        width: u16,
        layout: &crate::image::Layout,
        out: &mut Rows,
    ) {
        // A group with no call is its thinking, streaming or ended: a
        // thinking-only group never draws a summary row, even while the
        // model thinks (`docs/tui.md`, "Thinking").
        if !self.has_ledger() && self.streaming.is_empty() {
            for thought in self.thoughts() {
                thought_rows(thought, "", width, out);
            }
            return;
        }
        // An empty summary draws no row and consults no renderer, as
        // before.
        let Some(spans) = self.summary_spans(running, width) else {
            return;
        };
        let flat = out.seam.as_ref().is_some_and(|seam| seam.flat);
        let input = self.group_line_input(running, flat);
        let wrapped = Input::GroupLine(input);
        let builtin = |input: &Input<'_>, w: u16| {
            let Input::GroupLine(_) = input else {
                return Vec::new();
            };
            vec![crate::ext::spans::from_ratatui(
                &self.summary_spans(running, w).unwrap_or_default(),
            )]
        };
        let target = self.key.as_deref().map(|key| Target::Group(target_id(key)));
        let rendered = match out.fills_for("group_line") {
            crate::ext::SlotSelection::Draw { extension } => {
                out.ext_lines("group_line", &extension, &wrapped, width, &builtin)
            }
            crate::ext::SlotSelection::Absent | crate::ext::SlotSelection::Suppressed => None,
        };
        // The spin a running line carries, so the draw site spins its
        // first cell on the working line's tick.
        let spin = running.then_some(0);
        match rendered {
            Some(fitted) => {
                // The running replacement spins like the built-in line.
                // Its animated tag rides along, so the paint pass marks
                // the item visible.
                for line in fitted.iter() {
                    out.push_text(
                        (line.line.clone(), target),
                        RowText {
                            tail: line.pad,
                            links: line.links.clone(),
                            spin,
                            anim: line.anim.clone(),
                            ..RowText::plain()
                        },
                    );
                }
            }
            None => {
                out.push_text(
                    (Line::from(spans), target),
                    RowText {
                        spin,
                        ..RowText::plain()
                    },
                );
            }
        }
        let draws = self.open || self.calls().any(|call| call.asking);
        // An open approval shows its call whatever the group's own state.
        // A group with no key has no target, and draws as it is on a
        // search draw too: a match there is one nothing could reveal
        // (`docs/tui.md`, "Search").
        match self.key.as_deref().map(|key| Target::Group(target_id(key))) {
            Some(target) => {
                if out.open_scope(target, draws) {
                    self.ledger(width, layout, out);
                }
                out.end_scope();
            }
            None => {
                if draws {
                    self.ledger(width, layout, out);
                }
            }
        }
    }

    /// One row per call, split by step: the step's number right-aligned in
    /// a five-column dim gutter on its first row, then a glyph, the call's
    /// kind in `info` padded to nine columns, its target, and its result
    /// right-aligned to end at `width` (`docs/tui.md`, "Tool groups and
    /// the ledger"). The model calls that failed come first, then the
    /// step's thinking. Each call's images draw under its row, before its
    /// opened detail. A renderer bound to the rows draws what follows the
    /// gutter with the width less the gutter drawn; without one, or when
    /// it falls back, the built-in row draws.
    fn ledger(&self, width: u16, layout: &crate::image::Layout, out: &mut Rows) {
        for section in &self.sections {
            let mut gutter = format!("{:>5} ", section.step);
            // A step number longer than five digits widens the gutter
            // past six columns; its later rows stay blank but keep the
            // grown width, so every column stays put.
            let gutter_cells = crate::format::width(&gutter);
            let blank = " ".repeat(gutter_cells);
            for (code, attempt) in &section.failed {
                let gutter = std::mem::replace(&mut gutter, blank.clone());
                let row = format!("{gutter}model call failed · {code} · attempt {attempt}");
                out.push((dim(row), None));
            }
            for thought in &section.thoughts {
                thought_rows(
                    thought,
                    &std::mem::replace(&mut gutter, blank.clone()),
                    width,
                    out,
                );
            }
            // The section's calls as outer groups: a call whose outer
            // names a call of this section nests under it, at any depth;
            // every other call draws top-level, in push order, so an inner
            // arriving before its outer still lands under it.
            let mut children: Vec<Vec<usize>> = vec![Vec::new(); section.calls.len()];
            let mut roots: Vec<usize> = Vec::new();
            for (index, call) in section.calls.iter().enumerate() {
                let parent = call.outer_action_id.as_ref().and_then(|outer| {
                    section
                        .calls
                        .iter()
                        .position(|other| other.action == outer.0)
                        .filter(|at| *at != index)
                });
                match parent {
                    Some(at) => {
                        if let Some(siblings) = children.get_mut(at) {
                            siblings.push(index);
                        } else {
                            roots.push(index);
                        }
                    }
                    None => roots.push(index),
                }
            }
            // Each call draws once: a contradictory anchor cannot nest
            // forever, and calls a cycle strands outside every tree still
            // draw, flat and top-level in push order.
            let mut drawn = vec![false; section.calls.len()];
            for root in roots {
                self.call_tree(
                    section,
                    &children,
                    root,
                    0,
                    &mut gutter,
                    &blank,
                    width,
                    layout,
                    &mut drawn,
                    out,
                );
            }
            for index in 0..section.calls.len() {
                let stranded = drawn.get(index).is_some_and(|drawn| !drawn);
                if stranded {
                    self.call_tree(
                        section,
                        &children,
                        index,
                        0,
                        &mut gutter,
                        &blank,
                        width,
                        layout,
                        &mut drawn,
                        out,
                    );
                }
            }
        }
    }

    /// One call's ledger rows at `depth`: its row two columns further in
    /// per level, its warning and image rows, then its opened body when
    /// open — the arguments block with each inner call's rows under it
    /// when it has inners, else its own result. Each call draws once, so
    /// no anchor cycle nests forever.
    fn call_tree(
        &self,
        section: &Section,
        children: &[Vec<usize>],
        index: usize,
        depth: usize,
        gutter: &mut String,
        blank: &str,
        width: u16,
        layout: &crate::image::Layout,
        drawn: &mut [bool],
        out: &mut Rows,
    ) {
        let Some(call) = section.calls.get(index) else {
            return;
        };
        if drawn.get(index).is_some_and(|drawn| *drawn) {
            return;
        }
        if let Some(done) = drawn.get_mut(index) {
            *done = true;
        }
        let inners: &[usize] = match children.get(index) {
            Some(inners) => inners,
            None => &[],
        };
        let max = usize::from(width);
        let mark = std::mem::replace(gutter, blank.to_owned());
        // The indent composes with the fit: the content keeps the width
        // past the gutter less two columns per level.
        let pad = "  ".repeat(depth);
        let indent = u16::try_from(pad.len()).unwrap_or(u16::MAX);
        let shift = u16::try_from(crate::format::width(blank)).unwrap_or(u16::MAX);
        let output = first_result_line(call.content());
        let flat = out.seam.as_ref().is_some_and(|seam| seam.flat);
        let input = self.ledger_input(call, &output, flat);
        // The gutter stays with the terminal: the renderer draws what
        // follows it with the width less the gutter and indent drawn.
        let after_gutter = width.saturating_sub(shift).saturating_sub(indent);
        let builtin = |input: &Input<'_>, w: u16| {
            let Input::LedgerRow(built) = input else {
                return Vec::new();
            };
            vec![crate::ext::spans::from_ratatui(&self.builtin_row(built, w))]
        };
        let wrapped = Input::LedgerRow(input);
        let specific = format!("ledger_row:{}", call.name);
        let (slot, selection) = match out.fills_for(&specific) {
            crate::ext::SlotSelection::Absent => ("ledger_row", out.fills_for("ledger_row")),
            selection @ (crate::ext::SlotSelection::Draw { .. }
            | crate::ext::SlotSelection::Suppressed) => (specific.as_str(), selection),
        };
        let rendered = match selection {
            crate::ext::SlotSelection::Draw { extension } => {
                out.ext_lines(slot, &extension, &wrapped, after_gutter, &builtin)
            }
            crate::ext::SlotSelection::Absent | crate::ext::SlotSelection::Suppressed => None,
        };
        match rendered {
            Some(fitted) => {
                for (index, line) in fitted.iter().enumerate() {
                    let head = if index == 0 {
                        format!("{mark}{pad}")
                    } else {
                        format!("{blank}{pad}")
                    };
                    let shift = shift.saturating_add(indent);
                    let (row, text) = gutter_row(head, shift, line, max);
                    out.push_text((row, Some(Target::Call(call.id))), text);
                }
            }
            None => {
                let row = fit(
                    {
                        let mut row = vec![dim_span(format!("{mark}{pad}"))];
                        row.extend(self.builtin_row(&input, after_gutter));
                        row
                    },
                    max,
                );
                out.push((Line::from(row), Some(Target::Call(call.id))));
            }
        }
        for warning in call.protected() {
            let row = fit(crate::format::warning_line(warning), max);
            out.push((Line::from(row), Some(Target::Call(call.id))));
        }
        crate::turn::images::call_rows(&call.images, width, layout, out);
        if out.open_scope(Target::Call(call.id), call.open) {
            if inners.is_empty() {
                super::opened::rows(call, width, out);
            } else {
                self.arguments_block(call, width, out);
                for inner in inners {
                    self.call_tree(
                        section,
                        children,
                        *inner,
                        depth.saturating_add(1),
                        gutter,
                        blank,
                        width,
                        layout,
                        drawn,
                        out,
                    );
                }
            }
        }
        out.end_scope();
    }

    /// An opened outer's arguments block: the script source for
    /// codemode, dim text under the eight-column indent, at most
    /// `super::opened::OPENED_LINES` lines; the inner rows follow it,
    /// never capped.
    fn arguments_block(&self, call: &Call, width: u16, out: &mut Rows) {
        let Some(text) = arguments_text(call) else {
            return;
        };
        let max = usize::from(width);
        if max == 0 {
            return;
        }
        let room = width.saturating_sub(8);
        let mut scratch = Rows::default();
        for line in text.lines().take(super::opened::OPENED_LINES) {
            scratch.push((crate::format::dim(line.to_owned()), None));
        }
        scratch.pad(0, 8, room);
        let (rows, texts) = scratch.into_parts();
        for ((line, _), text) in rows.into_iter().zip(texts) {
            out.push_text((line, None), text);
        }
    }

    /// One call inlined in a card or group-line entry: everything the
    /// ledger row reads, with the opened detail an open call reads.
    /// Statuses, errors and answers come from [`Self::ledger_input`],
    /// so the entries and the ledger never disagree.
    fn turn_call<'a>(&self, call: &'a Call) -> TurnCall<'a> {
        // The first non-empty line, else "done": the borrowed twin of
        // [`first_result_line`].
        let first = call
            .content()
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("done");
        let row = self.ledger_input(call, first, false);
        let images = call
            .images
            .iter()
            .map(|part| PromptImage {
                path: &part.path,
                width: part.width,
                height: part.height,
            })
            .collect::<Vec<_>>();
        let content = call.content();
        let state = match row.state {
            CallState::Pending => CallState::Pending,
            CallState::Running => CallState::Running,
            CallState::Unknown => CallState::Unknown,
            CallState::Completed(result) => CallState::Completed(TurnCallResult {
                exit: result.exit,
                duration_ms: result.duration_ms,
                lines: result.lines,
                changes: result.changes,
                output: result.output,
                content,
                images,
            }),
            CallState::Failed { result, error } => CallState::Failed {
                result: TurnCallResult {
                    exit: result.exit,
                    duration_ms: result.duration_ms,
                    lines: result.lines,
                    changes: result.changes,
                    output: result.output,
                    content,
                    images,
                },
                error,
            },
            CallState::Denied { result, .. } => CallState::Denied {
                result: TurnCallResult {
                    exit: result.exit,
                    duration_ms: result.duration_ms,
                    lines: result.lines,
                    changes: result.changes,
                    output: result.output,
                    content,
                    images,
                },
                detail: call.headline(),
            },
            CallState::Cancelled(result) => CallState::Cancelled(TurnCallResult {
                exit: result.exit,
                duration_ms: result.duration_ms,
                lines: result.lines,
                changes: result.changes,
                output: result.output,
                content,
                images,
            }),
        };
        TurnCall {
            name: row.name,
            arguments: row.arguments,
            state,
            answer: row.answer,
            open: call.open,
            asking: call.asking,
        }
    }

    /// Its card entry: the summary line's fields with the ledger's
    /// sections. `running` is the turn running with this as its open
    /// group. A cut group maps to [`TurnGroupState::Cut`] even when the
    /// caller still names it running.
    pub(crate) fn turn_entry(&self, running: bool) -> TurnGroup<'_> {
        let state = if self.cut {
            TurnGroupState::Cut
        } else if running {
            TurnGroupState::Running
        } else {
            TurnGroupState::Finished
        };
        TurnGroup {
            first: self.first,
            last: self.last,
            state,
            open: self.open,
            sections: self
                .sections
                .iter()
                .map(|section| TurnSection {
                    step: section.step,
                    thoughts: section
                        .thoughts
                        .iter()
                        .map(|thought| TurnThought {
                            text: &thought.text,
                            started: thought.started,
                            ended: thought.ended,
                            open: thought.open,
                        })
                        .collect(),
                    calls: section
                        .calls
                        .iter()
                        .map(|call| self.turn_call(call))
                        .collect(),
                    failed: section
                        .failed
                        .iter()
                        .map(|(code, attempt)| TurnFailed {
                            code,
                            attempt: *attempt,
                        })
                        .collect(),
                })
                .collect(),
            streaming: self
                .streaming
                .iter()
                .map(|streaming| TurnStreaming {
                    message: &streaming.message,
                    index: streaming.index,
                    name: streaming.name.as_deref(),
                    text: &streaming.text,
                })
                .collect(),
        }
    }

    /// Its group-line entry: the summary line's aggregates, flattened
    /// across the sections in order. `running` is the turn running with
    /// this as its open group.
    pub(crate) fn group_line_input(&self, running: bool, flat: bool) -> GroupLine<'_> {
        GroupLine {
            running,
            calls: self
                .sections
                .iter()
                .flat_map(|section| section.calls.iter())
                .map(|call| self.turn_call(call))
                .collect(),
            streaming: self
                .streaming
                .iter()
                .map(|streaming| TurnStreaming {
                    message: &streaming.message,
                    index: streaming.index,
                    name: streaming.name.as_deref(),
                    text: &streaming.text,
                })
                .collect(),
            thoughts: self
                .thoughts()
                .map(|thought| TurnThought {
                    text: &thought.text,
                    started: thought.started,
                    ended: thought.ended,
                    open: thought.open,
                })
                .collect(),
            failed: self
                .sections
                .iter()
                .flat_map(|section| section.failed.iter())
                .map(|(code, attempt)| TurnFailed {
                    code,
                    attempt: *attempt,
                })
                .collect(),
            first: self.first,
            last: self.last,
            open: self.open,
            flat,
        }
    }

    /// A call's renderer input: everything the built-in row reads, folded
    /// from its events, without the step's gutter. A started call of a cut
    /// turn has status unknown; a failed call carries its headline's code
    /// and message.
    fn ledger_input<'a>(&self, call: &'a Call, output: &'a str, flat: bool) -> LedgerRow<'a> {
        let result = LedgerResult {
            exit: call.exit,
            duration_ms: call.duration_ms(),
            lines: call.lines,
            changes: &call.changes,
            output,
        };
        let state = match call.status {
            Some(CallStatus::Completed) => CallState::Completed(result),
            Some(CallStatus::Failed) => CallState::Failed {
                result,
                error: match call.headline().split_once(": ") {
                    Some((code, message)) => RowError { code, message },
                    None => RowError {
                        code: "",
                        message: call.headline(),
                    },
                },
            },
            Some(CallStatus::Denied) => CallState::Denied { result, detail: () },
            Some(CallStatus::Cancelled) => CallState::Cancelled(result),
            None if self.cut && call.has_started() => CallState::Unknown,
            None if call.has_started() => CallState::Running,
            None => CallState::Pending,
        };
        let answer = call
            .asked
            .as_ref()
            .and_then(|asked| asked.suffix())
            .map(|suffix| suffix.strip_prefix(" · ").unwrap_or(suffix));
        LedgerRow {
            name: &call.name,
            arguments: &call.arguments,
            state,
            answer,
            flat,
        }
    }

    /// The row without its gutter and without the final fit: the glyph,
    /// the call's kind, its target, and its result, drawn in the width
    /// past the gutter. The built-in path prepends the gutter and fits
    /// the whole row to `width`, exactly as it drew before.
    fn builtin_row(&self, row: &LedgerRow<'_>, after_gutter: u16) -> Vec<Span<'static>> {
        let kind = kind(row.name, row.arguments);
        // The glyph: what the call is doing, or how it ended. A
        // call that started and never completed may have run.
        let (glyph, glyph_style) = match row.state {
            CallState::Completed(_) => ("✓", style(Role::Success)),
            CallState::Cancelled(_) => ("⊘", Style::default().add_modifier(Modifier::DIM)),
            CallState::Failed { .. } | CallState::Denied { .. } => {
                ("✗", Style::default().add_modifier(Modifier::DIM))
            }
            CallState::Unknown => ("?", style(Role::Attention)),
            CallState::Pending | CallState::Running => ("○", style(Role::Attention)),
        };
        let result = self.builtin_result(row);
        // Past the gutter, the glyph, one space and nine of
        // kind leave the target and the result `after_gutter - 13`
        // cells. The target is cut with "…" to what the result leaves;
        // with no room for even one cell the target is left out
        // and the result is cut instead.
        let room = usize::from(after_gutter).saturating_sub(13 + spans_width(&result));
        let mut out = vec![
            Span::styled(glyph, glyph_style),
            Span::raw(" "),
            Span::styled(format!("{:<9}", kind.word()), style(Role::Info)),
        ];
        if room >= 1 {
            out.push(Span::raw(pad_target(
                &summary(row.name, row.arguments),
                room,
            )));
            out.push(Span::raw("  "));
            out.extend(result);
        } else {
            out.push(Span::raw("  "));
            out.extend(fit(result, usize::from(after_gutter).saturating_sub(13)));
        }
        out
    }

    /// A call's result: what the ledger row ends with (`docs/tui.md`,
    /// "Tool groups and the ledger"). An edit's counts are in the added
    /// and removed colours; every other result is dim, except a running
    /// call and an ask waiting on the person, which are in `attention`.
    fn builtin_result(&self, row: &LedgerRow<'_>) -> Vec<Span<'static>> {
        // The person's answer to a form replaces the status.
        if let Some(answer) = row.answer {
            return vec![dim_span(answer.to_owned())];
        }
        let completed = match row.state {
            CallState::Unknown => {
                return vec![dim_span("? may have run; not run again".to_owned())];
            }
            CallState::Pending | CallState::Running => {
                if matches!(kind(row.name, row.arguments), Kind::Ask(_)) {
                    return vec![Span::styled("waiting on you", style(Role::Attention))];
                }
                return vec![Span::styled("running", style(Role::Attention))];
            }
            CallState::Cancelled(_) => return vec![dim_span("cancelled".to_owned())],
            CallState::Denied { .. } => return vec![dim_span("denied".to_owned())],
            CallState::Failed { result, error } => {
                let headline = if !error.message.is_empty() {
                    if error.code.is_empty() {
                        error.message.to_owned()
                    } else {
                        format!("{}: {}", error.code, error.message)
                    }
                } else {
                    result.output.to_owned()
                };
                return vec![dim_span(headline)];
            }
            CallState::Completed(result) => result,
        };
        if matches!(kind(row.name, row.arguments), Kind::Edit | Kind::Write)
            && !completed.changes.is_empty()
        {
            let (added, removed) = completed.changes.iter().fold((0u64, 0u64), |(a, r), c| {
                (a.saturating_add(c.added), r.saturating_add(c.removed))
            });
            return vec![
                Span::styled(format!("+{added}"), style(Role::Added)),
                Span::raw(" "),
                Span::styled(format!("−{removed}"), style(Role::Removed)),
            ];
        }
        match kind(row.name, row.arguments) {
            Kind::Read => vec![dim_span(count(completed.lines, "line", "lines"))],
            Kind::Search => vec![dim_span(count(completed.lines, "match", "matches"))],
            Kind::List => vec![dim_span(count(completed.lines, "path", "paths"))],
            Kind::Shell if completed.exit.is_some() => {
                let exit = completed.exit.unwrap_or(0);
                vec![dim_span(format!(
                    "exit {exit} · {}",
                    count(completed.lines, "line", "lines")
                ))]
            }
            Kind::Edit
            | Kind::Write
            | Kind::Shell
            | Kind::Delegate
            | Kind::Jobs
            | Kind::Ask(_)
            | Kind::Other => vec![dim_span(completed.output.to_owned())],
        }
    }
}

/// An opened call or thought's indent: eight columns (`docs/tui.md`,
/// "Tool groups and the ledger").
const INDENT: &str = "        ";

/// One rendered ledger line with its gutter: the gutter cut to the
/// width when the width holds no more, else the gutter before the
/// fitted line, with every link range moved right by the shift, the
/// gutter's own cells.
fn gutter_row(
    mark: String,
    shift: u16,
    line: &crate::ext::spans::FittedLine,
    max: usize,
) -> (Line<'static>, RowText) {
    // The gutter precedes the line, so every link range moves right by
    // the shift; past five digits the step's whole number makes the
    // gutter wider than six columns.
    let gutter = crate::format::width(&mark);
    if max < gutter {
        (
            Line::from(vec![dim_span(crate::format::cut(&mark, max))]),
            RowText::plain(),
        )
    } else {
        let mut spans = vec![dim_span(mark)];
        spans.extend(line.line.spans.clone());
        let links = line
            .links
            .iter()
            .map(|(range, to)| {
                (
                    range.start.saturating_add(shift)..range.end.saturating_add(shift),
                    to.clone(),
                )
            })
            .collect();
        (
            Line::from(spans),
            RowText {
                tail: line.pad,
                links,
                // The gutter precedes the line, so its item's tag rides
                // along: a split row still marks its item visible.
                anim: line.anim.clone(),
                ..RowText::plain()
            },
        )
    }
}

/// A dim span.
pub(super) fn dim_span(text: String) -> Span<'static> {
    Span::styled(text, Style::default().add_modifier(Modifier::DIM))
}

/// How many cells `spans` take on screen.
pub(super) fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| crate::format::width(&span.content))
        .sum()
}

/// `spans` cut to at most `max` cells, styles kept: when anything is
/// left out the row ends in exactly one "…", so a cut row still fills
/// its width and what follows keeps its columns. Content keeps at most
/// `max − 1` cells, and a wide character's unused column is filled in
/// the cut span's style (`docs/tui.md`, "Tool groups and the ledger").
pub(super) fn fit(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= max {
        return spans;
    }
    let mut out = Vec::new();
    let mut taken = 0usize;
    let room = max.saturating_sub(1);
    let mut style = Style::default();
    for span in spans {
        style = span.style;
        let cells = crate::format::width(&span.content);
        if taken + cells <= room {
            taken += cells;
            out.push(span);
            continue;
        }
        let kept = crate::format::cut(&span.content, room.saturating_sub(taken));
        taken += crate::format::width(&kept);
        if !kept.is_empty() {
            out.push(Span::styled(kept, style));
        }
        break;
    }
    out.push(Span::styled("…", style));
    out.push(Span::styled(
        " ".repeat(max.saturating_sub(taken + 1)),
        style,
    ));
    out
}

/// A thought's row spans after `gutter`, cut to `width` with "…": the
/// seam's built-in and the row below share them.
fn thought_row(thought: &Thought, gutter: &str, width: u16) -> Vec<Span<'static>> {
    let max = usize::from(width);
    let row = match thought.ended {
        None => {
            let heading = heading(&thought.text, true)
                .map(|found| format!(": {found}"))
                .unwrap_or_default();
            let mark = if gutter.is_empty() {
                "Thinking"
            } else {
                "○ Thinking"
            };
            vec![
                dim_span(gutter.to_owned()),
                Span::styled(
                    format!("{mark}{heading}"),
                    Style::default()
                        .add_modifier(Modifier::DIM)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]
        }
        Some(_) => {
            let span = thought
                .ended
                .map(|ended| ended.saturating_sub(thought.started));
            let mut line = crate::format::thought("", &thought.text, span);
            // The line's dim lives on its spans from here, italic too.
            let base = line.style;
            let mut text = std::mem::take(&mut line.spans);
            for span in &mut text {
                span.style = base.patch(span.style).add_modifier(Modifier::ITALIC);
            }
            let mut spans = vec![dim_span(gutter.to_owned())];
            spans.extend(text);
            spans
        }
    };
    fit(row, max)
}

/// A thought's row after `gutter`, dim and italic, cut to `width` with
/// "…", and its text when open: "○ Thinking: <latest heading>" while
/// the thought streams, "+ Thought: <first heading> · <span>" once
/// ended. With no gutter the row is a thinking-only group's line, without
/// the "○ ". An opened thought's text sits eight columns in inside the
/// ledger and at the line's own indent outside it (`docs/tui.md`,
/// "Thinking", "Tool groups and the ledger"). A bound `thinking_line`
/// renderer replaces the row; an empty result draws no row and opens
/// no body.
fn thought_rows(thought: &Thought, gutter: &str, width: u16, out: &mut Rows) {
    let flat = out.seam.as_ref().is_some_and(|seam| seam.flat);
    let input = crate::ext::ThinkingLine {
        text: &thought.text,
        started: thought.started,
        ended: thought.ended,
        open: thought.open,
        flat,
    };
    let wrapped = Input::ThinkingLine(input);
    let builtin = |input: &Input<'_>, w: u16| {
        let Input::ThinkingLine(_) = input else {
            return Vec::new();
        };
        vec![crate::ext::spans::from_ratatui(&thought_row(
            thought, gutter, w,
        ))]
    };
    let rendered = match out.fills_for("thinking_line") {
        crate::ext::SlotSelection::Draw { extension } => {
            out.ext_lines("thinking_line", &extension, &wrapped, width, &builtin)
        }
        crate::ext::SlotSelection::Absent | crate::ext::SlotSelection::Suppressed => None,
    };
    match rendered {
        Some(fitted) => {
            // An empty result draws no row and opens no body.
            if fitted.is_empty() {
                return;
            }
            for line in fitted.iter() {
                out.push_fitted(line, Some(Target::Thought(thought.id)));
            }
        }
        None => {
            out.push((
                Line::from(thought_row(thought, gutter, width)),
                Some(Target::Thought(thought.id)),
            ));
        }
    }
    if out.open_scope(Target::Thought(thought.id), thought.open) {
        let indent = if gutter.is_empty() { "" } else { INDENT };
        opened(&thought.text, indent, out);
    }
    out.end_scope();
}

/// `target` cut with "…" to `room` cells, else padded with spaces to it,
/// so the result ends at the row's last cell. A wide character at the
/// edge can leave one column unused: the cut is padded to `room` from
/// its actual width, not its allowance.
fn pad_target(target: &str, room: usize) -> String {
    let cells = crate::format::width(target);
    if cells > room {
        let kept = crate::format::cut(target, room.saturating_sub(1));
        let fill = room.saturating_sub(crate::format::width(&kept) + 1);
        format!("{kept}…{}", " ".repeat(fill))
    } else {
        format!("{target}{}", " ".repeat(room - cells))
    }
}

/// An outer's arguments as text: the script in its `source` argument
/// for codemode, the string itself when a string, else its JSON; none
/// when it has none.
fn arguments_text(call: &Call) -> Option<String> {
    if let Some(source) = call.arguments.get("source").and_then(Value::as_str) {
        return Some(source.to_owned());
    }
    match &call.arguments {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        arguments => serde_json::to_string_pretty(arguments).ok(),
    }
}

/// The first non-empty line of a completed call's output, or "done" when
/// there is none.
fn first_result_line(detail: &str) -> String {
    detail
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("done")
        .to_owned()
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
