//! One topic's lines screen: the earlier lines and the proposals one
//! request gave, the focus among them and the person's decisions on them.

use std::collections::BTreeSet;

use contract::notes::{Evidence, EvidenceKind, Proposed, Topic};

use super::template::{NotesLayer, Source, clean_line, default_layer};
use crate::keys::Key;

/// Earlier bullets struck, which later runs and the save keep.
pub(super) type Struck = Vec<(NotesLayer, Topic, String)>;

/// Whether the earlier bullet `text` of `layer` and `topic` is struck.
pub(super) fn is_struck(struck: &Struck, layer: NotesLayer, topic: Topic, text: &str) -> bool {
    struck
        .iter()
        .any(|(l, t, s)| *l == layer && *t == topic && s == text)
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

enum Decision {
    Keep,
    Strike,
    Edited(String),
}

/// What a key leaves to the setup.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Flow {
    /// The screen stays.
    Stay,
    /// Back to the topic's question.
    Back,
    /// Every proposal is decided: the next topic.
    Next,
}

/// One request's lines: the earlier lines, then the proposals.
pub(super) struct Run {
    evidence: Vec<String>,
    items: Vec<Item>,
    focus: usize,
    edit: Option<String>,
}

impl Run {
    /// The `earlier` lines, then the lines `proposed` for the `evidence`,
    /// with the focus on the first proposal. A line citing nothing in range,
    /// or two kinds of evidence, or cleaning to nothing, is dropped.
    pub(super) fn new(
        earlier: Vec<(NotesLayer, String)>,
        evidence: &[Evidence],
        proposed: &[Proposed],
    ) -> Self {
        let mut items: Vec<Item> = earlier
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
        Self {
            evidence,
            items,
            focus,
            edit: None,
        }
    }

    /// The evidence texts the lines cite, in the order given.
    pub(super) fn evidence(&self) -> &[String] {
        &self.evidence
    }

    /// The lines, earlier ones first.
    pub(super) fn rows(&self, topic: Topic, struck: &Struck) -> Vec<LineRow<'_>> {
        self.items
            .iter()
            .enumerate()
            .map(|(at, item)| {
                let focused = at == self.focus;
                let editing = self.edit.as_deref().filter(|_| focused);
                match item {
                    Item::Earlier { layer, text } => LineRow {
                        earlier: true,
                        layer: *layer,
                        flipped_from: None,
                        text,
                        state: if is_struck(struck, *layer, topic, text) {
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

    /// The texts of the proposals kept for `layer`.
    pub(super) fn kept(&self, layer: NotesLayer) -> impl Iterator<Item = &str> {
        self.items.iter().filter_map(move |item| match item {
            Item::Proposal(p) if p.state == LineState::Kept && p.layer == layer => {
                Some(p.text.as_str())
            }
            Item::Proposal(_) | Item::Earlier { .. } => None,
        })
    }

    /// A paste into the field being edited, and nowhere else.
    pub(super) fn paste(&mut self, text: &str) {
        if let Some(field) = self.edit.as_mut() {
            field.push_str(text);
        }
    }

    /// A key: edits the field while one is open, else moves, decides or
    /// flips. `struck` is where a decision on an earlier line is recorded.
    pub(super) fn key(&mut self, key: &Key, topic: Topic, struck: &mut Struck) -> Flow {
        if self.edit.is_some() {
            return self.edit_key(key, topic, struck);
        }
        if *key == Key::Esc {
            return Flow::Back;
        } else if *key == Key::Enter {
            self.decide(Decision::Keep, topic, struck);
            return self.advance();
        } else if *key == Key::Char('x') && !self.items.is_empty() {
            self.decide(Decision::Strike, topic, struck);
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
        Flow::Stay
    }

    fn edit_key(&mut self, key: &Key, topic: Topic, struck: &mut Struck) -> Flow {
        if let Key::Char(c) = key {
            if let Some(field) = self.edit.as_mut() {
                field.push(*c);
            }
        } else if *key == Key::Backspace {
            if let Some(field) = self.edit.as_mut() {
                field.pop();
            }
        } else if *key == Key::Esc {
            self.edit = None;
        } else if *key == Key::Enter {
            let text = clean_line(&self.edit.take().unwrap_or_default());
            if text.is_empty() {
                self.decide(Decision::Strike, topic, struck);
            } else {
                self.decide(Decision::Edited(text), topic, struck);
            }
            return self.advance();
        }
        Flow::Stay
    }

    fn start_edit(&mut self) {
        if let Some(Item::Proposal(p)) = self.items.get(self.focus) {
            self.edit = Some(p.text.clone());
        }
    }

    fn flip(&mut self) {
        if let Some(Item::Proposal(p)) = self.items.get_mut(self.focus) {
            p.layer = match p.layer {
                NotesLayer::Global => NotesLayer::PerProject,
                NotesLayer::PerProject => NotesLayer::Global,
            };
        }
    }

    fn focus_by(&mut self, step: isize) {
        let last = self.items.len().saturating_sub(1);
        self.focus = self.focus.saturating_add_signed(step).min(last);
    }

    /// Applies a decision to the focused line.
    fn decide(&mut self, decision: Decision, topic: Topic, struck: &mut Struck) {
        match self.items.get_mut(self.focus) {
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
                        if !struck.contains(&entry) {
                            struck.push(entry);
                        }
                    }
                    Decision::Keep | Decision::Edited(_) => struck.retain(|e| *e != entry),
                }
            }
            None => {}
        }
    }

    /// Moves to the next undecided proposal below the focus, wrapping to
    /// above; with none left, to the next topic. Every caller has just
    /// decided the focused line, so it is never open and the search can
    /// start at it.
    fn advance(&mut self) -> Flow {
        let open = |item: &Item| matches!(item, Item::Proposal(p) if p.state == LineState::Open);
        let after = self
            .items
            .iter()
            .enumerate()
            .skip(self.focus)
            .find(|(_, item)| open(item));
        let before = self
            .items
            .iter()
            .enumerate()
            .take(self.focus)
            .find(|(_, item)| open(item));
        match after.or(before) {
            Some((at, _)) => {
                self.focus = at;
                Flow::Stay
            }
            None => Flow::Next,
        }
    }
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
