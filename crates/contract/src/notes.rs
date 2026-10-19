//! Reviewer-note proposals and their evidence (`docs/permissions.md`).

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

/// A topic in the reviewer's notes, in the setup's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Accounts, organisations and repositories.
    SourceControl,
    /// Hosts, registries and services inside the organisation.
    InternalHosts,
    /// Internal tools and their effects.
    InternalTools,
    /// Routine actions the reviewer may allow.
    RoutineActions,
    /// Actions that must never happen.
    Never,
}

impl Topic {
    // debt: retain only the planned setup helpers below, Part 4 uses them in the terminal flow.
    /// The setup's topic order.
    pub const ALL: [Self; 5] = [
        Self::SourceControl,
        Self::InternalHosts,
        Self::InternalTools,
        Self::RoutineActions,
        Self::Never,
    ];

    /// The heading whose bullets the setup owns.
    pub fn heading(self) -> &'static str {
        match self {
            Self::SourceControl => "### Source control",
            Self::InternalHosts => "### Internal hosts",
            Self::InternalTools => "### Internal tools",
            Self::RoutineActions => "### Routine actions",
            Self::Never => "### What must never happen",
        }
    }

    /// The one question asked for this topic.
    pub fn question(self) -> &'static str {
        match self {
            Self::SourceControl => {
                "Which accounts, organisations and repositories are your own or your organisation's?"
            }
            Self::InternalHosts => "Which internal hosts, registries and services count as inside?",
            Self::InternalTools => {
                "Which internal tools do you run, and what does running each one do?"
            }
            Self::RoutineActions => {
                "What routine actions, such as squash-merging your own pull requests once CI passes, should the reviewer allow?"
            }
            Self::Never => "What must never happen, such as touching infra/prod?",
        }
    }

    /// The facts or rules this topic covers.
    pub fn covers(self) -> &'static str {
        match self {
            Self::SourceControl => {
                "which accounts, organisations and repositories are the person's own or their organisation's"
            }
            Self::InternalHosts => "internal hosts, registries and services that count as inside",
            Self::InternalTools => "internal tools and what running them does",
            Self::RoutineActions => {
                "routine actions, such as squash-merging their own pull requests once CI passes"
            }
            Self::Never => "what must never happen, such as touching infra/prod",
        }
    }
}

/// One topic's request, without conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposeNotesArgs {
    /// The topic to propose lines for.
    pub topic: Topic,
    /// The person's answer, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Lines already covered by the notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier: Vec<String>,
}

/// Where an evidence item came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceKind {
    /// The project's own files.
    Files,
    /// The person's answer.
    Answer,
    /// Escalations a person answered allow.
    Session,
}

/// Evidence cited by a proposed line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Evidence {
    /// Evidence gathered from the project's files.
    Files {
        /// The sanitised evidence text.
        text: String,
    },
    /// The person's answer to the topic's question.
    Answer {
        /// The trimmed answer.
        text: String,
    },
    /// A validated escalation count for one prefix.
    Session(SessionTally),
}

impl Evidence {
    /// The evidence's source kind.
    pub fn kind(&self) -> EvidenceKind {
        match self {
            Self::Files { .. } => EvidenceKind::Files,
            Self::Answer { .. } => EvidenceKind::Answer,
            Self::Session(_) => EvidenceKind::Session,
        }
    }

    /// Text to show and to send in a proposal request.
    pub fn text(&self) -> &str {
        match self {
            Self::Files { text } | Self::Answer { text } => text,
            Self::Session(tally) => tally.text(),
        }
    }
}

/// A nonzero count of escalations answered allow for a prefix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "TallyWire", into = "TallyWire")]
pub struct SessionTally {
    prefix: String,
    count: NonZeroU32,
    here: u32,
    text: String,
}

impl SessionTally {
    /// Refuses zero counts and a project count larger than the total.
    pub fn new(prefix: String, count: u32, here: u32) -> Option<Self> {
        let count = NonZeroU32::new(count)?;
        if here > count.get() {
            return None;
        }
        let noun = if count.get() == 1 {
            "escalation"
        } else {
            "escalations"
        };
        let text = format!("{count} {noun} answered allow for `{prefix}`");
        Some(Self {
            prefix,
            count,
            here,
            text,
        })
    }
    // debt: retain only the planned tally accessors, Part 4 uses them to render session evidence.
    /// The rule prefix counted.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
    /// All matching escalations.
    pub fn count(&self) -> u32 {
        self.count.get()
    }
    /// Matching escalations from this project.
    pub fn here(&self) -> u32 {
        self.here
    }
    /// The count as evidence text.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Whether every escalation came from this project.
    pub fn all_here(&self) -> bool {
        self.here == self.count.get()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TallyWire {
    prefix: String,
    count: u32,
    here: u32,
    text: String,
}

impl TryFrom<TallyWire> for SessionTally {
    type Error = &'static str;
    fn try_from(wire: TallyWire) -> Result<Self, Self::Error> {
        Self::new(wire.prefix, wire.count, wire.here).ok_or("invalid escalation count")
    }
}
impl From<SessionTally> for TallyWire {
    fn from(tally: SessionTally) -> Self {
        Self {
            prefix: tally.prefix,
            count: tally.count.get(),
            here: tally.here,
            text: tally.text,
        }
    }
}

/// A line proposed from one or more evidence items of the same kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ProposalWire")]
pub struct Proposed {
    text: String,
    cites: Vec<usize>,
}

impl Proposed {
    /// Refuses a line that cites no evidence.
    pub fn new(text: String, cites: Vec<usize>) -> Result<Self, &'static str> {
        if cites.is_empty() {
            return Err("a proposal must cite evidence");
        }
        Ok(Self { text, cites })
    }

    /// One fact or rule.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Nonempty, zero-based indices into the answer's evidence.
    pub fn cites(&self) -> &[usize] {
        &self.cites
    }
}

#[derive(Deserialize)]
struct ProposalWire {
    text: String,
    cites: Vec<usize>,
}
impl TryFrom<ProposalWire> for Proposed {
    type Error = &'static str;
    fn try_from(wire: ProposalWire) -> Result<Self, Self::Error> {
        Self::new(wire.text, wire.cites)
    }
}

/// Gathered evidence and arguments delivered to the session's loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotesAsk {
    /// The driver's topic, answer and earlier lines.
    pub args: ProposeNotesArgs,
    /// Files then session evidence; the loop adds the answer last.
    pub evidence: Vec<Evidence>,
}

/// Removes user names and passwords from every URL authority, leaving paths
/// and scp-form remotes byte for byte.
pub fn strip_userinfo(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("://") {
        output.push_str(before);
        output.push_str("://");
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '\'' | '"' | '>'))
            .unwrap_or(after.len());
        let (authority, tail) = after.split_at(end);
        output.push_str(
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host),
        );
        rest = tail;
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
#[path = "notes_tests.rs"]
mod tests;
