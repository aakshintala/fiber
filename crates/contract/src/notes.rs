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
    /// Shell commands that only read, proposed as configuration entries.
    /// The setup asks no question for this topic and owns no notes
    /// heading for it (`docs/permissions.md`, "Setting up the notes").
    ReadOnlyCommands,
}

impl Topic {
    // debt: retain only ALL/heading/question, Part 4 task 4.1 uses them for the planned setup frames.
    /// The setup's topic order.
    pub const ALL: [Self; 6] = [
        Self::SourceControl,
        Self::InternalHosts,
        Self::InternalTools,
        Self::RoutineActions,
        Self::Never,
        Self::ReadOnlyCommands,
    ];

    /// The heading whose bullets the setup owns. The read-only topic owns
    /// none: its display heading names the screen, never a notes section.
    pub fn heading(self) -> &'static str {
        match self {
            Self::SourceControl => "### Source control",
            Self::InternalHosts => "### Internal hosts",
            Self::InternalTools => "### Internal tools",
            Self::RoutineActions => "### Routine actions",
            Self::Never => "### What must never happen",
            Self::ReadOnlyCommands => "### Read-only commands",
        }
    }

    /// The one question asked for this topic. The read-only topic asks
    /// none: the setup shows its risk line instead of a question.
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
            Self::ReadOnlyCommands => {
                "Which commands only read, so the reviewer may skip reviewing them?"
            }
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
            Self::ReadOnlyCommands => "commands that only read",
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
    /// Reviewer allows of one shell subject.
    Allows,
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
    /// A validated reviewer-allow count for one shell subject.
    Allows(AllowsTally),
}

impl Evidence {
    /// The evidence's source kind.
    pub fn kind(&self) -> EvidenceKind {
        match self {
            Self::Files { .. } => EvidenceKind::Files,
            Self::Answer { .. } => EvidenceKind::Answer,
            Self::Session(_) => EvidenceKind::Session,
            Self::Allows(_) => EvidenceKind::Allows,
        }
    }

    /// Text to show and to send in a proposal request.
    pub fn text(&self) -> &str {
        match self {
            Self::Files { text } | Self::Answer { text } => text,
            Self::Session(tally) => tally.text(),
            Self::Allows(tally) => tally.text(),
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
    // debt: retain only prefix/count/here/all_here, Part 4 task 4.1 uses them for session evidence frames.
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

/// A count of reviewer allows for one shell subject, with the classifier's
/// prefix in memory. The wire carries no prefix and no eligibility state:
/// a deserialized tally restores an empty prefix and stays display-only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AllowsWire", into = "AllowsWire")]
pub struct AllowsTally {
    prefix: String,
    subject: String,
    count: NonZeroU32,
    here: u32,
    text: String,
}

impl AllowsTally {
    /// Refuses zero counts, a project count larger than the total, and an
    /// empty prefix or subject: every rendered entry needs both words.
    pub fn new(prefix: String, subject: String, count: u32, here: u32) -> Option<Self> {
        if prefix.is_empty() || subject.is_empty() {
            return None;
        }
        let count = NonZeroU32::new(count)?;
        if here > count.get() {
            return None;
        }
        let noun = if count.get() == 1 {
            "reviewer allow"
        } else {
            "reviewer allows"
        };
        let text = format!("{count} {noun} for {subject}");
        Some(Self {
            prefix,
            subject,
            count,
            here,
            text,
        })
    }

    /// Restores a tally from the wire: the prefix stays empty, so the
    /// tally is display-only and never renders. Refuses zero counts, a
    /// project count larger than the total and an empty subject, as
    /// [`AllowsTally::new`] does.
    pub fn from_wire(subject: String, count: u32, here: u32, text: String) -> Option<Self> {
        if subject.is_empty() {
            return None;
        }
        let count = NonZeroU32::new(count)?;
        if here > count.get() {
            return None;
        }
        Some(Self {
            prefix: String::new(),
            subject,
            count,
            here,
            text,
        })
    }

    /// The classifier's widening for the subject, empty on wire tallies.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
    /// The counted shell subject.
    pub fn subject(&self) -> &str {
        &self.subject
    }
    /// All matching reviewer allows.
    pub fn count(&self) -> u32 {
        self.count.get()
    }
    /// Matching allows from this project.
    pub fn here(&self) -> u32 {
        self.here
    }
    /// The count as evidence text, with the withheld reason appended.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Whether every allow came from this project.
    pub fn all_here(&self) -> bool {
        self.here == self.count.get()
    }

    /// Appends why the subject gets no entry to the evidence text. The
    /// wire shape is unchanged: the reason travels in the text field.
    pub fn with_reason(mut self, reason: &str) -> Self {
        self.text.push_str(" (");
        self.text.push_str(reason);
        self.text.push(')');
        self
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowsWire {
    subject: String,
    count: u32,
    here: u32,
    text: String,
}

impl TryFrom<AllowsWire> for AllowsTally {
    type Error = &'static str;
    fn try_from(wire: AllowsWire) -> Result<Self, Self::Error> {
        Self::from_wire(wire.subject, wire.count, wire.here, wire.text)
            .ok_or("invalid reviewer-allow count")
    }
}
impl From<AllowsTally> for AllowsWire {
    fn from(tally: AllowsTally) -> Self {
        Self {
            subject: tally.subject,
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

/// Flag names that make a shell subject write-shaped: a subject is never
/// proposed when any of its flags, by name before any "=", is on this
/// list (`docs/permissions.md`, "Setting up the notes").
pub const WRITE_SHAPED_FLAGS: &[&str] = &[
    "-o",
    "--output",
    "--out",
    "--output-file",
    "-w",
    "--write",
    "-i",
    "--in-place",
    "--fix",
    "--apply",
    "--force",
    "--delete",
    "--save",
];

/// Built-in read-only command names and their slash-qualified extensions,
/// data only: the built-in list in `crates/tools/src/shell/read_only.rs`
/// (`EXACT` rows joined word for word, every `COMMANDS` name) plus the
/// five bare names the classifier unwraps itself (`cd`, `env`, `timeout`,
/// `xargs`, `sed`). Consumers hold the matching logic; this table only
/// names what the built-ins own.
pub const BUILTIN_READ_ONLY_NAMES: &[&str] = &[
    "pwd",
    "echo",
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "grep",
    "rg",
    "find",
    "sort",
    "nl",
    "git",
    "gh",
    "true",
    ":",
    "cd",
    "env",
    "timeout",
    "xargs",
    "sed",
    "git status",
    "git branch --show-current",
    "git diff",
    "git log",
    "git show",
    "git rev-parse",
    "git merge-base",
    "git ls-files",
    "git blame",
    "git stash list",
    "gh pr view",
    "gh pr list",
    "gh pr diff",
    "gh pr checks",
    "gh issue view",
    "gh issue list",
    "gh repo view",
    "gh run list",
    "gh run view",
    "/bin/pwd",
    "/bin/echo",
    "/bin/ls",
    "/bin/cat",
    "/bin/head",
    "/bin/tail",
    "/bin/wc",
    "/bin/grep",
    "/bin/rg",
    "/bin/find",
    "/bin/sort",
    "/bin/nl",
    "/bin/git",
    "/bin/gh",
    "/bin/true",
    "/bin/:",
    "/bin/cd",
    "/bin/env",
    "/bin/timeout",
    "/bin/xargs",
    "/bin/sed",
    "/usr/bin/pwd",
    "/usr/bin/echo",
    "/usr/bin/ls",
    "/usr/bin/cat",
    "/usr/bin/head",
    "/usr/bin/tail",
    "/usr/bin/wc",
    "/usr/bin/grep",
    "/usr/bin/rg",
    "/usr/bin/find",
    "/usr/bin/sort",
    "/usr/bin/nl",
    "/usr/bin/git",
    "/usr/bin/gh",
    "/usr/bin/true",
    "/usr/bin/:",
    "/usr/bin/cd",
    "/usr/bin/env",
    "/usr/bin/timeout",
    "/usr/bin/xargs",
    "/usr/bin/sed",
];

/// Redirect tokens that keep a read-only shell subject read-only, data
/// only: output sent to "/dev/null" (descriptor none, "1" or "2"), one
/// stream merged into another ("1" or "2"), and input read from a file
/// ("<" and "0<", whose destination must not start with "/dev/tcp/" or
/// "/dev/udp/"). Any other redirect makes the call executes. Consumers
/// hold the strip-and-guard logic; this table only names the tokens.
pub const ALLOWED_REDIRECT_TOKENS: &[&str] = &[
    ">/dev/null",
    "1>/dev/null",
    "2>/dev/null",
    ">&1",
    ">&2",
    "1>&1",
    "1>&2",
    "2>&1",
    "2>&2",
    "<",
    "0<",
];

/// Renders one read-only configuration entry line from its command path
/// and flag names: `shell.read_only."<command>".flags = ["--json"]`. Only
/// `"` and `\` are escaped, so the parse below inverts this exactly.
pub fn render_read_only_entry(command: &str, flags: &[String]) -> String {
    let mut rendered = String::from("shell.read_only.\"");
    for ch in command.chars() {
        if ch == '"' || ch == '\\' {
            rendered.push('\\');
        }
        rendered.push(ch);
    }
    rendered.push_str("\".flags = ");
    rendered.push('[');
    for (at, flag) in flags.iter().enumerate() {
        if at > 0 {
            rendered.push_str(", ");
        }
        rendered.push('"');
        for ch in flag.chars() {
            if ch == '"' || ch == '\\' {
                rendered.push('\\');
            }
            rendered.push(ch);
        }
        rendered.push('"');
    }
    rendered.push(']');
    rendered
}

/// Parses one entry line rendered by [`render_read_only_entry`]: the
/// command path and its flag names. Strict: anything else, including an
/// empty command, an unknown escape, trailing text or a non-string flag
/// list, is refused.
pub fn parse_read_only_entry(text: &str) -> Option<(String, Vec<String>)> {
    let rest = text.strip_prefix("shell.read_only.\"")?;
    let mut command = String::new();
    let mut chars = rest.chars();
    loop {
        let ch = chars.next()?;
        if ch == '\\' {
            match chars.next()? {
                escaped @ ('"' | '\\') => command.push(escaped),
                _ => return None,
            }
        } else if ch == '"' {
            break;
        } else {
            command.push(ch);
        }
    }
    if command.is_empty() {
        return None;
    }
    let rest: String = chars.collect();
    let flags_text = rest.strip_prefix(".flags = ")?;
    if flags_text.trim_end().len() != flags_text.len() {
        return None;
    }
    let flags: Vec<String> = serde_json::from_str(flags_text).ok()?;
    Some((command, flags))
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
