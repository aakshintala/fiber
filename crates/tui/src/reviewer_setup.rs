//! The reviewer's notes setup (`docs/permissions.md`, "Setting up the
//! notes"): the person confirms, edits or strikes proposed lines one topic at
//! a time, then approves the notes as a diff.
//!
//! [`Setup`] is the flow's state, pure: keys, pastes and the hub's answers go
//! in, an [`Act`] comes out for the caller to carry out. Nothing here sends a
//! command or reads a file.

use std::cell::Cell;
use std::collections::BTreeSet;

use contract::notes::{Evidence, EvidenceKind, ProposeNotesArgs, Proposed, Topic};
use contract::shapes::Failure;

use crate::configure::{
    ConfigureError, FileOutcome, FileSaved, NotesSaved, NotesWrite, ReviewerNotes,
};
use crate::keys::Key;
use template::{DiffRow, NotesLayer, Source, clean_line, default_layer, diff, merge, owned};

pub(crate) mod template;

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

/// Where a line stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineState {
    /// A proposal the person has not decided.
    Open,
    /// A proposal confirmed or an earlier line kept.
    Kept,
    /// Struck: kept out of the notes.
    Struck,
}

/// One row of a lines screen, as the view draws it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LineRow<'a> {
    /// Whether the line is already in the notes.
    pub(crate) earlier: bool,
    /// The layer it goes to or is in.
    pub(crate) layer: NotesLayer,
    /// The layer the rule gave it, when the person flipped it off that.
    pub(crate) flipped_from: Option<NotesLayer>,
    /// The line.
    pub(crate) text: &'a str,
    /// Where it stands.
    pub(crate) state: LineState,
    /// Whether the focus is on it.
    pub(crate) focused: bool,
    /// The edit field's text while the person edits this line.
    pub(crate) editing: Option<&'a str>,
}

/// A proposed line: its text as shown and saved, the layer the rule gave
/// it, the layer it will go to and where it stands.
struct Proposal {
    text: String,
    default: NotesLayer,
    layer: NotesLayer,
    state: LineState,
}

enum Item {
    /// A bullet the layer's text already holds.
    Earlier {
        layer: NotesLayer,
        text: String,
    },
    Proposal(Proposal),
}

/// One request's lines: the earlier lines, then the proposals.
struct Run {
    evidence: Vec<String>,
    items: Vec<Item>,
    focus: usize,
    edit: Option<String>,
}

/// The answer typed for a topic and the lines its last request gave.
#[derive(Default)]
struct TopicState {
    answer: String,
    run: Option<Run>,
}

/// A [`TopicState`] for each topic, with no index to get wrong.
#[derive(Default)]
struct PerTopic {
    source_control: TopicState,
    internal_hosts: TopicState,
    internal_tools: TopicState,
    routine_actions: TopicState,
    never: TopicState,
}

impl PerTopic {
    fn get(&self, topic: Topic) -> &TopicState {
        match topic {
            Topic::SourceControl => &self.source_control,
            Topic::InternalHosts => &self.internal_hosts,
            Topic::InternalTools => &self.internal_tools,
            Topic::RoutineActions => &self.routine_actions,
            Topic::Never => &self.never,
        }
    }

    fn get_mut(&mut self, topic: Topic) -> &mut TopicState {
        match topic {
            Topic::SourceControl => &mut self.source_control,
            Topic::InternalHosts => &mut self.internal_hosts,
            Topic::InternalTools => &mut self.internal_tools,
            Topic::RoutineActions => &mut self.routine_actions,
            Topic::Never => &mut self.never,
        }
    }
}

/// Both layers as they will be saved, and the rows that show the change.
struct Prepared {
    global: String,
    project: String,
    global_rows: Vec<DiffRow>,
    project_rows: Vec<DiffRow>,
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

/// The setup's state.
pub(crate) struct Setup {
    notes: ReviewerNotes,
    topics: PerTopic,
    topic: Topic,
    screen: Screen,
    /// A dim row under the question or the diff.
    error: Option<String>,
    /// Earlier bullets struck, which later runs and the save keep.
    struck: Vec<(NotesLayer, Topic, String)>,
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
            topics: PerTopic::default(),
            topic: FIRST,
            screen: Screen::Question,
            error: None,
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
        Topic::ALL
            .iter()
            .position(|t| *t == self.topic)
            .map_or(1, |at| at + 1)
    }

    /// The answer typed for the topic.
    pub(crate) fn answer(&self) -> &str {
        &self.topics.get(self.topic).answer
    }

    /// The dim row under the question or the diff, when there is one.
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The evidence texts the topic's lines cite, in the order given.
    pub(crate) fn evidence(&self) -> &[String] {
        self.topics
            .get(self.topic)
            .run
            .as_ref()
            .map_or(&[], |run| run.evidence.as_slice())
    }

    /// The topic's lines, earlier ones first.
    pub(crate) fn line_rows(&self) -> Vec<LineRow<'_>> {
        let topic = self.topic;
        let Some(run) = self.topics.get(topic).run.as_ref() else {
            return Vec::new();
        };
        run.items
            .iter()
            .enumerate()
            .map(|(at, item)| {
                let focused = at == run.focus;
                let editing = run.edit.as_deref().filter(|_| focused);
                match item {
                    Item::Earlier { layer, text } => LineRow {
                        earlier: true,
                        layer: *layer,
                        flipped_from: None,
                        text,
                        state: if self.is_struck(*layer, topic, text) {
                            LineState::Struck
                        } else {
                            LineState::Kept
                        },
                        focused,
                        editing,
                    },
                    Item::Proposal(p) => LineRow {
                        earlier: false,
                        layer: p.layer,
                        flipped_from: (p.layer != p.default).then_some(p.default),
                        text: &p.text,
                        state: p.state,
                        focused,
                        editing,
                    },
                }
            })
            .collect()
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

    fn is_struck(&self, layer: NotesLayer, topic: Topic, text: &str) -> bool {
        self.struck
            .iter()
            .any(|(l, t, s)| *l == layer && *t == topic && s == text)
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
                    self.screen = Screen::Question;
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
            Screen::Question => self.topics.get_mut(self.topic).answer.push_str(&text),
            Screen::Lines => {
                if let Some(field) = self.run_mut().and_then(|run| run.edit.as_mut()) {
                    field.push_str(&text);
                }
            }
            Screen::Waiting | Screen::Diff | Screen::Saved | Screen::Notice => {}
        }
    }

    fn question_key(&mut self, key: &Key) -> Act {
        if let Key::Char(c) = key {
            self.topics.get_mut(self.topic).answer.push(*c);
        } else if *key == Key::Backspace {
            self.topics.get_mut(self.topic).answer.pop();
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
        let at = Topic::ALL.iter().position(|t| *t == self.topic);
        let previous = at
            .and_then(|at| at.checked_sub(1))
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
    /// whitespace, and the topic's earlier lines.
    fn propose(&mut self) -> Act {
        self.error = None;
        let answer = self.answer();
        let answer = (!answer.trim().is_empty()).then(|| answer.to_string());
        let args = ProposeNotesArgs {
            topic: self.topic,
            answer,
            earlier: self.earlier(),
        };
        self.screen = Screen::Waiting;
        Act::Propose(args)
    }

    /// The topic's bullets from both layers, global first: each once, and
    /// none that every layer holding it has struck.
    fn earlier(&self) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for (_, text) in self.earlier_items() {
            let live = [NotesLayer::Global, NotesLayer::PerProject]
                .into_iter()
                .filter(|l| self.holds(*l, self.topic, &text))
                .any(|l| !self.is_struck(l, self.topic, &text));
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

    /// The topic's bullets in each layer, global first.
    fn earlier_items(&self) -> Vec<(NotesLayer, String)> {
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
        self.topics.get_mut(self.topic).run.as_mut()
    }

    fn lines_key(&mut self, key: &Key) -> Act {
        if self.run_mut().is_some_and(|run| run.edit.is_some()) {
            return self.edit_key(key);
        }
        let empty = self.run_mut().is_none_or(|run| run.items.is_empty());
        if *key == Key::Esc {
            self.topics.get_mut(self.topic).run = None;
            self.screen = Screen::Question;
        } else if *key == Key::Enter {
            self.decide(Decision::Keep);
            return self.advance();
        } else if *key == Key::Char('x') && !empty {
            self.decide(Decision::Strike);
            return self.advance();
        } else if *key == Key::Char('e') {
            self.start_edit();
        } else if *key == Key::Tab {
            self.flip();
        } else if *key == Key::Up {
            self.focus_by(-1);
        } else if *key == Key::Down {
            self.focus_by(1);
        }
        Act::None
    }

    fn edit_key(&mut self, key: &Key) -> Act {
        if let Key::Char(c) = key {
            if let Some(field) = self.run_mut().and_then(|run| run.edit.as_mut()) {
                field.push(*c);
            }
        } else if *key == Key::Backspace {
            if let Some(field) = self.run_mut().and_then(|run| run.edit.as_mut()) {
                field.pop();
            }
        } else if *key == Key::Esc {
            if let Some(run) = self.run_mut() {
                run.edit = None;
            }
        } else if *key == Key::Enter {
            let field = self
                .run_mut()
                .and_then(|run| run.edit.take())
                .unwrap_or_default();
            let text = clean_line(&field);
            if text.is_empty() {
                self.decide(Decision::Strike);
            } else {
                self.decide(Decision::Edited(text));
            }
            return self.advance();
        }
        Act::None
    }

    fn start_edit(&mut self) {
        if let Some(run) = self.run_mut()
            && let Some(Item::Proposal(p)) = run.items.get(run.focus)
        {
            run.edit = Some(p.text.clone());
        }
    }

    fn flip(&mut self) {
        if let Some(run) = self.run_mut()
            && let Some(Item::Proposal(p)) = run.items.get_mut(run.focus)
        {
            p.layer = match p.layer {
                NotesLayer::Global => NotesLayer::PerProject,
                NotesLayer::PerProject => NotesLayer::Global,
            };
        }
    }

    fn focus_by(&mut self, step: isize) {
        if let Some(run) = self.run_mut() {
            let last = run.items.len().saturating_sub(1);
            run.focus = run.focus.saturating_add_signed(step).min(last);
        }
    }

    /// Applies a decision to the focused line.
    fn decide(&mut self, decision: Decision) {
        let topic = self.topic;
        let Some(run) = self.topics.get_mut(topic).run.as_mut() else {
            return;
        };
        match run.items.get_mut(run.focus) {
            Some(Item::Proposal(p)) => match decision {
                Decision::Keep => p.state = LineState::Kept,
                Decision::Strike => p.state = LineState::Struck,
                Decision::Edited(text) => {
                    p.text = text;
                    p.state = LineState::Kept;
                }
            },
            Some(Item::Earlier { layer, text }) => {
                let entry = (*layer, topic, text.clone());
                match decision {
                    Decision::Strike => {
                        if !self.struck.contains(&entry) {
                            self.struck.push(entry);
                        }
                    }
                    Decision::Keep | Decision::Edited(_) => self.struck.retain(|e| *e != entry),
                }
            }
            None => {}
        }
    }

    /// Moves to the next undecided proposal below the focus, wrapping to
    /// above; with none left, to the next topic.
    fn advance(&mut self) -> Act {
        let Some(run) = self.run_mut() else {
            return Act::None;
        };
        let open = |item: &Item| matches!(item, Item::Proposal(p) if p.state == LineState::Open);
        let after = run
            .items
            .iter()
            .enumerate()
            .skip(run.focus + 1)
            .find(|(_, item)| open(item));
        let before = run
            .items
            .iter()
            .enumerate()
            .take(run.focus)
            .find(|(_, item)| open(item));
        match after.or(before) {
            Some((at, _)) => {
                run.focus = at;
                Act::None
            }
            None => self.next_topic(),
        }
    }

    fn next_topic(&mut self) -> Act {
        let next = Topic::ALL
            .iter()
            .position(|t| *t == self.topic)
            .and_then(|at| Topic::ALL.get(at + 1));
        match next {
            Some(topic) => {
                self.topic = *topic;
                self.screen = Screen::Question;
                self.error = None;
            }
            None => self.show_diff(),
        }
        Act::None
    }

    // ----------------------------------------------------- the diff, save

    /// The text a layer is saved as: what was read with the person's
    /// decisions applied.
    fn after(&self, layer: NotesLayer) -> String {
        let struck: Vec<(Topic, String)> = self
            .struck
            .iter()
            .filter(|(l, _, _)| *l == layer)
            .map(|(_, topic, text)| (*topic, text.clone()))
            .collect();
        let mut added: Vec<(Topic, String)> = Vec::new();
        for topic in Topic::ALL {
            let Some(run) = self.topics.get(topic).run.as_ref() else {
                continue;
            };
            for item in &run.items {
                if let Item::Proposal(p) = item
                    && p.state == LineState::Kept
                    && p.layer == layer
                {
                    added.push((topic, p.text.clone()));
                }
            }
        }
        merge(self.text_of(layer), &struck, &added)
    }

    fn show_diff(&mut self) {
        let global = self.after(NotesLayer::Global);
        let project = self.after(NotesLayer::PerProject);
        self.prepared = Some(Prepared {
            global_rows: diff(&self.notes.global.text, &global),
            project_rows: diff(&self.notes.project.text, &project),
            global,
            project,
        });
        self.screen = Screen::Diff;
        self.scroll = 0;
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

    /// The layers whose text changed, each with the version read; nothing to
    /// send is a close with the nothing-changed line.
    fn save(&mut self) -> Act {
        self.error = None;
        let Some(prepared) = self.prepared.as_ref() else {
            return Act::None;
        };
        let write = |after: &str, before: &str, version: &str| {
            (after != before).then(|| NotesWrite {
                version: version.to_string(),
                text: after.to_string(),
            })
        };
        let global = write(
            &prepared.global,
            &self.notes.global.text,
            &self.notes.global.version,
        );
        let project = write(
            &prepared.project,
            &self.notes.project.text,
            &self.notes.project.version,
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
        let mut items: Vec<Item> = self
            .earlier_items()
            .into_iter()
            .map(|(layer, text)| Item::Earlier { layer, text })
            .collect();
        let mut cited: BTreeSet<usize> = BTreeSet::new();
        for line in proposed {
            let text = clean_line(line.text());
            let Some(source) = source_of(line.cites(), evidence) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            cited.extend(line.cites().iter().filter(|at| **at < evidence.len()));
            let default = default_layer(source);
            items.push(Item::Proposal(Proposal {
                text,
                default,
                layer: default,
                state: LineState::Open,
            }));
        }
        let evidence = evidence
            .iter()
            .enumerate()
            .filter(|(at, _)| cited.contains(at))
            .map(|(_, item)| clean_line(item.text()))
            .collect();
        let focus = items
            .iter()
            .position(|item| matches!(item, Item::Proposal(_)))
            .unwrap_or(0);
        self.topics.get_mut(self.topic).run = Some(Run {
            evidence,
            items,
            focus,
            edit: None,
        });
        self.screen = Screen::Lines;
        self.top.set(0);
    }

    /// The request failed or was refused: back to the question with the
    /// answer kept and `message` under it.
    pub(crate) fn refused(&mut self, message: &str) {
        if matches!(self.screen, Screen::Waiting | Screen::Question) {
            self.screen = Screen::Question;
            self.error = Some(message.to_string());
        }
    }

    /// The waiting request's answer may never come: reopen the question.
    pub(crate) fn lost(&mut self) {
        if self.screen == Screen::Waiting {
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

enum Decision {
    Keep,
    Strike,
    Edited(String),
}

/// The one source a proposal's cites name, or `None` when none is in range
/// of the evidence or they name more than one kind.
fn source_of(cites: &[usize], evidence: &[Evidence]) -> Option<Source> {
    let cited: Vec<&Evidence> = cites.iter().filter_map(|at| evidence.get(*at)).collect();
    let kind = cited.first()?.kind();
    if cited.iter().any(|item| item.kind() != kind) {
        return None;
    }
    Some(match kind {
        EvidenceKind::Files => Source::Files,
        EvidenceKind::Answer => Source::Answer,
        EvidenceKind::Session => Source::Session {
            all_here: cited
                .iter()
                .all(|item| matches!(item, Evidence::Session(tally) if tally.all_here())),
        },
    })
}

#[cfg(test)]
#[path = "reviewer_setup_tests.rs"]
mod tests;
