//! The reviewer's notes setup (`docs/permissions.md`, "Setting up the
//! notes"): the person confirms, edits or strikes proposed lines one topic at
//! a time, then approves the notes as a diff.
//!
//! [`Setup`] is the flow's state, pure: keys, pastes and the hub's answers go
//! in, an [`Act`] comes out for the caller to carry out. Nothing here sends a
//! command or reads a file.

use std::cell::Cell;
use std::path::Path;

use contract::notes::{AllowsTally, Evidence, ProposeNotesArgs, Proposed, Topic};
use contract::shapes::Failure;

use crate::configure::{
    ConfigureError, FileOutcome, FileSaved, NotesSaved, NotesWrite, ReadOnlyEntry, ReviewerNotes,
};
use crate::keys::Key;
use lines::{Flow, Run, Struck, is_struck};
use template::{DiffRow, NotesLayer, diff, merge, owned};

mod lines;
pub(crate) mod template;

pub(crate) use lines::{LineRow, LineState};

/// The screen the setup is on: one value, so the flags cannot contradict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Screen {
    /// A topic's question and the person's answer field.
    Question,
    /// The request for a topic's lines is out.
    Waiting,
    /// A topic's earlier and proposed lines.
    Lines,
    /// The notes as they will be saved, as a diff.
    Diff,
    /// After the save: each file and whether it was saved.
    Saved,
    /// The one-time notice that the reviewer blocked a call with no notes.
    Notice,
}

/// What the notice says: the first time in a session the reviewer blocks a
/// call while both layers of the notes are empty.
pub(crate) const NOTICE: &str = "The reviewer blocked a call while both layers of the notes are empty. Run /reviewer-setup to set them up.";

/// What the caller does after a key or an answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Act {
    /// Nothing.
    None,
    /// Send one `propose_notes` request.
    Propose(ProposeNotesArgs),
    /// Forget the waiting request: its answer, when it comes, is dropped.
    Abandon,
    /// Save these layers, global first. At least one is `Some`.
    Save {
        /// The global layer, when its text changed.
        global: Option<NotesWrite>,
        /// The per-project layer, when its text changed.
        project: Option<NotesWrite>,
    },
    /// Read both layers again.
    Reread,
    /// Reload the session, then close.
    Reload,
    /// Close the setup.
    Close,
    /// Close with a line saying nothing changed: no layer's text differs.
    Unchanged,
}

/// The answer typed for a topic and the lines its last request gave.
#[derive(Default)]
struct TopicState {
    answer: String,
    run: Option<Run>,
}

/// A topic's place in [`Topic::ALL`], which is also its place in
/// [`Setup`]'s states.
fn place(topic: Topic) -> usize {
    Topic::ALL
        .iter()
        .position(|t| *t == topic)
        .unwrap_or_default()
}

/// Both layers as they will be saved, and the rows that show the change.
struct Prepared {
    global: String,
    project: String,
    global_rows: Vec<DiffRow>,
    project_rows: Vec<DiffRow>,
    global_entries: Vec<ReadOnlyEntry>,
    project_entries: Vec<ReadOnlyEntry>,
}

/// One layer of the final screen.
pub(crate) struct LayerDiff<'a> {
    /// The layer's file.
    pub(crate) file: &'a std::path::Path,
    /// The layer's name on the screen.
    pub(crate) layer: NotesLayer,
    /// The changed topics' rows; none when the layer is unchanged.
    pub(crate) rows: &'a [DiffRow],
}

/// One layer's proposed read-only entries on the final screen.
pub(crate) struct ConfigSection<'a> {
    /// The layer's file, which holds the entries.
    pub(crate) file: &'a Path,
    /// The kept entries, in row order; empty when none is proposed.
    pub(crate) entries: &'a [ReadOnlyEntry],
}

/// The setup's state.
pub(crate) struct Setup {
    notes: ReviewerNotes,
    /// A state for each topic, in [`Topic::ALL`] order.
    topics: [TopicState; Topic::ALL.len()],
    topic: Topic,
    screen: Screen,
    /// A dim row under the question or the diff.
    error: Option<String>,
    /// Why the read-only proposal never arrived, shown on the diff: the
    /// topic has no question to reopen, so a refusal skips to the diff
    /// without the proposal instead.
    proposal_notice: Option<String>,
    /// Earlier bullets struck, which later runs and the save keep.
    struck: Struck,
    prepared: Option<Prepared>,
    /// The diff's first visible row.
    scroll: usize,
    /// The diff's row count and the rows that fit, noted by each draw so a
    /// key can clamp the scroll.
    viewport: Cell<(usize, usize)>,
    /// The lines screen's first visible item, kept so the focus stays in
    /// view as the draw moves it.
    top: Cell<usize>,
    saved: Vec<FileSaved>,
}

const FIRST: Topic = Topic::SourceControl;

impl Setup {
    /// Opens on topic 1's question with `notes` as read.
    pub(crate) fn open(notes: ReviewerNotes) -> Self {
        Self {
            notes,
            topics: Default::default(),
            topic: FIRST,
            screen: Screen::Question,
            error: None,
            proposal_notice: None,
            struck: Vec::new(),
            prepared: None,
            scroll: 0,
            viewport: Cell::new((0, 0)),
            top: Cell::new(0),
            saved: Vec::new(),
        }
    }

    /// The notice screen over `notes` as read: the same state, on the one
    /// screen that takes any key to close it.
    pub(crate) fn notice(notes: ReviewerNotes) -> Self {
        Self {
            screen: Screen::Notice,
            ..Self::open(notes)
        }
    }

    // ----------------------------------------------------------- reading

    /// The screen on show.
    pub(crate) fn screen(&self) -> Screen {
        self.screen
    }

    /// The topic the question or lines are for.
    pub(crate) fn topic(&self) -> Topic {
        self.topic
    }

    /// The topic's place in the setup's order, from 1.
    pub(crate) fn number(&self) -> usize {
        place(self.topic) + 1
    }

    /// The answer typed for the topic.
    pub(crate) fn answer(&self) -> &str {
        self.here().map_or("", |state| &state.answer)
    }

    /// The dim row under the question or the diff, when there is one.
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Why the read-only proposal never arrived, shown on the lines and
    /// the diff, when there is one.
    pub(crate) fn proposal_notice(&self) -> Option<&str> {
        self.proposal_notice.as_deref()
    }

    /// The withheld reviewer-allow tallies no proposal cites, in evidence
    /// order, for the dim evidence-context rows.
    pub(crate) fn evidence_context(&self) -> Vec<AllowsTally> {
        self.here()
            .and_then(|state| state.run.as_ref())
            .map(|run| run.context().to_vec())
            .unwrap_or_default()
    }

    /// The evidence texts the topic's lines cite, in the order given.
    pub(crate) fn evidence(&self) -> &[String] {
        self.here()
            .and_then(|state| state.run.as_ref())
            .map_or(&[], Run::evidence)
    }

    /// The topic's lines, earlier ones first.
    pub(crate) fn line_rows(&self) -> Vec<LineRow<'_>> {
        self.here()
            .and_then(|state| state.run.as_ref())
            .map_or_else(Vec::new, |run| run.rows(self.topic, &self.struck))
    }

    /// The diff screen's proposed config sections, global first.
    pub(crate) fn config_sections(&self) -> Vec<ConfigSection<'_>> {
        let Some(prepared) = self.prepared.as_ref() else {
            return Vec::new();
        };
        vec![
            ConfigSection {
                file: &self.notes.global.file,
                entries: &prepared.global_entries,
            },
            ConfigSection {
                file: &self.notes.project.file,
                entries: &prepared.project_entries,
            },
        ]
    }

    /// The diff screen's layers, global first.
    pub(crate) fn layers(&self) -> Vec<LayerDiff<'_>> {
        let Some(prepared) = self.prepared.as_ref() else {
            return Vec::new();
        };
        vec![
            LayerDiff {
                file: &self.notes.global.file,
                layer: NotesLayer::Global,
                rows: &prepared.global_rows,
            },
            LayerDiff {
                file: &self.notes.project.file,
                layer: NotesLayer::PerProject,
                rows: &prepared.project_rows,
            },
        ]
    }

    /// The diff's first visible row.
    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    /// Notes how many rows the diff has and how many fit, from a draw.
    pub(crate) fn note_viewport(&self, total: usize, capacity: usize) {
        self.viewport.set((total, capacity));
    }

    /// The lines screen's first visible item, kept between draws.
    pub(crate) fn top(&self) -> &Cell<usize> {
        &self.top
    }

    /// The files the save wrote or tried to, global first.
    pub(crate) fn saved_files(&self) -> &[FileSaved] {
        &self.saved
    }

    /// Whether at least one file now holds the new notes, which is when a
    /// reload has something to load.
    pub(crate) fn any_saved(&self) -> bool {
        self.saved.iter().any(|file| {
            matches!(
                file.outcome,
                FileOutcome::Saved | FileOutcome::SavedUnsynced(_)
            )
        })
    }

    fn here(&self) -> Option<&TopicState> {
        self.topics.get(place(self.topic))
    }

    fn here_mut(&mut self) -> Option<&mut TopicState> {
        self.topics.get_mut(place(self.topic))
    }

    fn text_of(&self, layer: NotesLayer) -> &str {
        match layer {
            NotesLayer::Global => &self.notes.global.text,
            NotesLayer::PerProject => &self.notes.project.text,
        }
    }

    // -------------------------------------------------------------- keys

    /// A key: Ctrl+C never arrives here, the caller leaves it to quit.
    pub(crate) fn key(&mut self, key: &Key) -> Act {
        match self.screen {
            Screen::Question => self.question_key(key),
            Screen::Waiting => {
                if *key == Key::Esc {
                    if self.topic == Topic::ReadOnlyCommands {
                        // The model-free topic asks no question: Esc
                        // forgets the wait and reopens the previous
                        // topic's lines.
                        if let Some(previous) = place(self.topic)
                            .checked_sub(1)
                            .and_then(|at| Topic::ALL.get(at))
                        {
                            self.topic = *previous;
                        }
                        self.screen = Screen::Lines;
                    } else {
                        self.screen = Screen::Question;
                    }
                    Act::Abandon
                } else {
                    Act::None
                }
            }
            Screen::Lines => self.lines_key(key),
            Screen::Diff => self.diff_key(key),
            Screen::Saved => self.saved_key(key),
            Screen::Notice => Act::Close,
        }
    }

    /// A paste: its text, newlines as spaces, goes into the field being
    /// edited and nowhere else.
    pub(crate) fn paste(&mut self, text: &str) {
        let text = text.replace(['\n', '\r'], " ");
        match self.screen {
            Screen::Question => {
                if let Some(state) = self.here_mut() {
                    state.answer.push_str(&text);
                }
            }
            Screen::Lines => {
                if let Some(run) = self.run_mut() {
                    run.paste(&text);
                }
            }
            Screen::Waiting | Screen::Diff | Screen::Saved | Screen::Notice => {}
        }
    }

    fn question_key(&mut self, key: &Key) -> Act {
        if let Key::Char(c) = key {
            if let Some(state) = self.here_mut() {
                state.answer.push(*c);
            }
        } else if *key == Key::Backspace {
            if let Some(state) = self.here_mut() {
                state.answer.pop();
            }
        } else if *key == Key::Enter {
            return self.propose();
        } else if *key == Key::Esc {
            return self.back_from_question();
        }
        Act::None
    }

    /// Esc on a question: the previous topic's lines, or close on the
    /// first topic.
    fn back_from_question(&mut self) -> Act {
        self.error = None;
        let previous = place(self.topic)
            .checked_sub(1)
            .and_then(|at| Topic::ALL.get(at));
        match previous {
            None => Act::Close,
            Some(topic) => {
                self.topic = *topic;
                self.screen = Screen::Lines;
                Act::None
            }
        }
    }

    /// The request for the topic: the answer only when it holds more than
    /// whitespace, and the topic's earlier lines. The read-only topic
    /// asks no question, so its request carries no answer and no earlier
    /// lines: it proposes itself from the counted subjects alone.
    fn propose(&mut self) -> Act {
        self.error = None;
        let (answer, earlier) = if self.topic == Topic::ReadOnlyCommands {
            (None, Vec::new())
        } else {
            let answer = self.answer();
            let answer = (!answer.trim().is_empty()).then(|| answer.to_string());
            (answer, self.earlier())
        };
        let args = ProposeNotesArgs {
            topic: self.topic,
            answer,
            earlier,
        };
        self.screen = Screen::Waiting;
        Act::Propose(args)
    }

    /// The topic's bullets from both layers, global first: each once, and
    /// none that every layer holding it has struck. The read-only topic
    /// keeps no notes bullets, so its earlier lines stay empty.
    fn earlier(&self) -> Vec<String> {
        if self.topic == Topic::ReadOnlyCommands {
            return Vec::new();
        }
        let mut lines: Vec<String> = Vec::new();
        for (_, text) in self.earlier_items() {
            let live = [NotesLayer::Global, NotesLayer::PerProject]
                .into_iter()
                .filter(|l| self.holds(*l, self.topic, &text))
                .any(|l| !is_struck(&self.struck, l, self.topic, &text));
            if live && !lines.contains(&text) {
                lines.push(text);
            }
        }
        lines
    }

    fn holds(&self, layer: NotesLayer, topic: Topic, text: &str) -> bool {
        owned(self.text_of(layer))
            .into_iter()
            .any(|(t, bullets)| t == topic && bullets.iter().any(|b| b == text))
    }

    /// The topic's bullets in each layer, global first. The read-only
    /// topic keeps no notes bullets, so its items stay empty.
    fn earlier_items(&self) -> Vec<(NotesLayer, String)> {
        if self.topic == Topic::ReadOnlyCommands {
            return Vec::new();
        }
        let mut items = Vec::new();
        for layer in [NotesLayer::Global, NotesLayer::PerProject] {
            for (topic, bullets) in owned(self.text_of(layer)) {
                if topic == self.topic {
                    items.extend(bullets.into_iter().map(|b| (layer, b)));
                }
            }
        }
        items
    }

    fn run_mut(&mut self) -> Option<&mut Run> {
        self.here_mut().and_then(|state| state.run.as_mut())
    }

    fn lines_key(&mut self, key: &Key) -> Act {
        let state = self.topics.get_mut(place(self.topic));
        let Some(run) = state.and_then(|state| state.run.as_mut()) else {
            return Act::None;
        };
        match run.key(key, self.topic, &mut self.struck) {
            Flow::Stay => {}
            Flow::Back => {
                if self.topic == Topic::ReadOnlyCommands {
                    // The model-free topic asks no question: back goes
                    // to the previous topic's lines, keeping its run.
                    if let Some(previous) = place(self.topic)
                        .checked_sub(1)
                        .and_then(|at| Topic::ALL.get(at))
                    {
                        self.topic = *previous;
                    }
                    self.screen = Screen::Lines;
                } else {
                    if let Some(state) = self.here_mut() {
                        state.run = None;
                    }
                    self.screen = Screen::Question;
                }
            }
            Flow::Next => return self.next_topic(),
        }
        Act::None
    }

    fn next_topic(&mut self) -> Act {
        match Topic::ALL.get(place(self.topic) + 1) {
            Some(topic) => {
                self.topic = *topic;
                self.error = None;
                if *topic == Topic::ReadOnlyCommands {
                    // The model-free topic asks no question: it proposes
                    // at once, and the wait carries no answer or lines.
                    return self.propose();
                }
                self.screen = Screen::Question;
            }
            None => self.show_diff(),
        }
        Act::None
    }

    // ----------------------------------------------------- the diff, save

    /// The text a layer is saved as: what was read with the person's
    /// decisions applied. Read-only rows never enter the notes: their
    /// entries live in configuration, so both loops skip the topic.
    fn after(&self, layer: NotesLayer) -> String {
        let struck: Vec<(Topic, String)> = self
            .struck
            .iter()
            .filter(|(l, _, _)| *l == layer)
            .filter(|(_, topic, _)| *topic != Topic::ReadOnlyCommands)
            .map(|(_, topic, text)| (*topic, text.clone()))
            .collect();
        let mut added: Vec<(Topic, String)> = Vec::new();
        for (state, topic) in self.topics.iter().zip(Topic::ALL) {
            if topic == Topic::ReadOnlyCommands {
                continue;
            }
            if let Some(run) = state.run.as_ref() {
                added.extend(run.kept(layer).map(|text| (topic, text.to_owned())));
            }
        }
        merge(self.text_of(layer), &struck, &added)
    }

    /// The kept entry rows parsed per layer, global first. Unparseable
    /// rows are left out here; `save` refuses them with the diff error.
    fn parsed_entries(&self) -> (Vec<ReadOnlyEntry>, Vec<ReadOnlyEntry>) {
        let mut global = Vec::new();
        let mut project = Vec::new();
        for (state, topic) in self.topics.iter().zip(Topic::ALL) {
            if topic != Topic::ReadOnlyCommands {
                continue;
            }
            let Some(run) = state.run.as_ref() else {
                continue;
            };
            for layer in [NotesLayer::Global, NotesLayer::PerProject] {
                let entries = match layer {
                    NotesLayer::Global => &mut global,
                    NotesLayer::PerProject => &mut project,
                };
                for text in run.kept(layer) {
                    if let Some((command, flags)) = contract::notes::parse_read_only_entry(text) {
                        entries.push(ReadOnlyEntry { command, flags });
                    }
                }
            }
        }
        (global, project)
    }

    fn show_diff(&mut self) {
        let global = self.after(NotesLayer::Global);
        let project = self.after(NotesLayer::PerProject);
        let (global_entries, project_entries) = self.parsed_entries();
        self.prepared = Some(Prepared {
            global_rows: diff(&self.notes.global.text, &global),
            project_rows: diff(&self.notes.project.text, &project),
            global,
            project,
            global_entries,
            project_entries,
        });
        self.screen = Screen::Diff;
        self.scroll = 0;
        // A transient error clears; the proposal notice survives the
        // diff and the reread with it.
        self.error = None;
    }

    fn diff_key(&mut self, key: &Key) -> Act {
        let (total, capacity) = self.viewport.get();
        let farthest = total.saturating_sub(capacity);
        let page = capacity.saturating_sub(1).max(1);
        if *key == Key::Esc {
            return Act::Close;
        } else if *key == Key::Enter {
            return self.save();
        } else if *key == Key::Up {
            self.scroll = self.scroll.saturating_sub(1);
        } else if *key == Key::Down {
            self.scroll = (self.scroll + 1).min(farthest);
        } else if *key == Key::PageUp {
            self.scroll = self.scroll.saturating_sub(page);
        } else if *key == Key::PageDown {
            self.scroll = (self.scroll + page).min(farthest);
        }
        Act::None
    }

    /// The layers whose text changed, each with the version read, plus the
    /// kept read-only entries parsed per layer; nothing to send is a close
    /// with the nothing-changed line. A row that parses to no entry, or a
    /// command confirmed twice in one layer, refuses the save with the
    /// diff error instead; overlap stays the authoritative save's refusal.
    fn save(&mut self) -> Act {
        self.error = None;
        let mut global_entries = Vec::new();
        let mut project_entries = Vec::new();
        for (state, topic) in self.topics.iter().zip(Topic::ALL) {
            if topic != Topic::ReadOnlyCommands {
                continue;
            }
            let Some(run) = state.run.as_ref() else {
                continue;
            };
            for layer in [NotesLayer::Global, NotesLayer::PerProject] {
                let entries = match layer {
                    NotesLayer::Global => &mut global_entries,
                    NotesLayer::PerProject => &mut project_entries,
                };
                for text in run.kept(layer) {
                    let Some((command, flags)) = contract::notes::parse_read_only_entry(text)
                    else {
                        self.error = Some(format!("{text} is not a read-only entry"));
                        return Act::None;
                    };
                    if entries
                        .iter()
                        .any(|entry: &ReadOnlyEntry| entry.command == command)
                    {
                        self.error = Some(format!("{command} is confirmed twice in one layer"));
                        return Act::None;
                    }
                    entries.push(ReadOnlyEntry { command, flags });
                }
            }
        }
        let Some(prepared) = self.prepared.as_ref() else {
            return Act::None;
        };
        let write = |after: &str, before: &str, version: &str, entries: Vec<ReadOnlyEntry>| {
            (after != before || !entries.is_empty()).then(|| NotesWrite {
                version: version.to_string(),
                text: after.to_string(),
                read_only: entries,
            })
        };
        let global = write(
            &prepared.global,
            &self.notes.global.text,
            &self.notes.global.version,
            global_entries,
        );
        let project = write(
            &prepared.project,
            &self.notes.project.text,
            &self.notes.project.version,
            project_entries,
        );
        if global.is_none() && project.is_none() {
            return Act::Unchanged;
        }
        Act::Save { global, project }
    }

    fn saved_key(&mut self, key: &Key) -> Act {
        if *key == Key::Esc || (*key == Key::Char('n') && self.any_saved()) {
            Act::Close
        } else if *key == Key::Char('r') && self.any_saved() {
            Act::Reload
        } else {
            Act::None
        }
    }

    // -------------------------------------------------------- the answers

    /// The hub's answer to the waiting request: the lines it proposed for
    /// the evidence it gathered, or the failure that left none. Ignored
    /// unless a request is waiting.
    pub(crate) fn proposed(
        &mut self,
        evidence: &[Evidence],
        proposed: &[Proposed],
        error: Option<&Failure>,
    ) {
        if self.screen != Screen::Waiting {
            return;
        }
        if let Some(failure) = error {
            self.refused(&failure.message);
            return;
        }
        let run = Run::new(self.earlier_items(), evidence, proposed);
        if let Some(state) = self.here_mut() {
            state.run = Some(run);
        }
        self.screen = Screen::Lines;
        self.top.set(0);
    }

    /// The request failed or was refused: back to the question with the
    /// answer kept and `message` under it. The read-only topic asks no
    /// question, so its refusal keeps earlier decisions, shows why on the
    /// diff, and goes on without the proposal instead.
    pub(crate) fn refused(&mut self, message: &str) {
        if self.topic == Topic::ReadOnlyCommands
            && matches!(self.screen, Screen::Waiting | Screen::Question)
        {
            self.show_diff();
            self.proposal_notice = Some(message.to_string());
            return;
        }
        if matches!(self.screen, Screen::Waiting | Screen::Question) {
            self.screen = Screen::Question;
            self.error = Some(message.to_string());
        }
    }

    /// The waiting request's answer may never come: reopen the question.
    /// The read-only topic asks none, so its loss shows why on the diff.
    pub(crate) fn lost(&mut self) {
        if self.screen == Screen::Waiting {
            if self.topic == Topic::ReadOnlyCommands {
                self.show_diff();
                self.proposal_notice =
                    Some("The session restarted before its proposal arrived.".to_string());
                return;
            }
            self.refused("The session restarted. Enter proposes again.");
        }
    }

    /// The save's answer. `written` shows the final screen; `stale` wrote
    /// nothing, so both layers are read again; a failed save stays on the
    /// diff with its error.
    pub(crate) fn saved(&mut self, result: Result<NotesSaved, ConfigureError>) -> Act {
        match result {
            Ok(NotesSaved::Written(files)) => {
                self.saved = files;
                self.screen = Screen::Saved;
                self.error = None;
                Act::None
            }
            Ok(NotesSaved::Stale) => Act::Reread,
            Err(error) => {
                self.error = Some(error.message);
                Act::None
            }
        }
    }

    /// Both layers read again after `stale`: the diff is drawn again with the
    /// person's decisions applied to the fresh text.
    pub(crate) fn reread(&mut self, notes: Result<ReviewerNotes, ConfigureError>) {
        match notes {
            Ok(notes) => {
                self.notes = notes;
                self.show_diff();
            }
            Err(error) => self.error = Some(error.message),
        }
    }
}

#[cfg(test)]
#[path = "reviewer_setup_tests.rs"]
mod tests;
