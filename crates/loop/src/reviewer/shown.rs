//! What the reviewer is shown (`docs/permissions.md`, "What it is shown"):
//! each person's message, each tool call, and the reply each person message
//! answers, nothing else. A person's answer to a question counts as a
//! person message, and the question it answers is the reply.

use std::collections::BTreeMap;

use contract::events::{
    Answer, Event, FormAnswer, InputItem, Interaction, InteractionRequested, KeptMessage,
    MessageOutcome, Outcome, ResolvedBy, TurnOutcome, TurnStarted,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, Origin};
use contract::{ActionId, RequestId, Seq};
use serde_json::Value;

/// What one reviewed item is.
///
/// Public for the `review` jig's replay (`docs/testing.md`, "Jigs");
/// the renderer below is the only writer.
#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    /// The agent's tool call, by its action.
    Call(ActionId),
    /// A person's message, by the line that holds it.
    Person(KeptMessage),
    /// The agent's raw, uncapped text that a person message answers.
    /// Held at the tail until a person message takes it or a call, a
    /// handoff or a failed turn removes it.
    Reply(String),
}

/// One item of what the reviewer is shown (`docs/permissions.md`, "What it
/// is shown").
///
/// Public for the `review` jig's replay (`docs/testing.md`, "Jigs");
/// the renderer below is the only writer.
#[derive(Debug, Clone, PartialEq)]
pub struct Reviewed {
    /// What this item is: a call, a person's message, or the reply it answers.
    pub shown: Shown,
    /// The item as the reviewer reads it.
    pub input: Input,
}

/// The eliciting content a resolved interaction answers, keyed by request
/// id. A resolve carries no prompt, so the fold stages each request here
/// and the resolve consumes it; a repeated resolve after consumption
/// renders nothing.
///
/// Public for the integration crates, which thread their own store through
/// the renderer (`docs/testing.md`, "Jigs").
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PendingAsks {
    staged: BTreeMap<RequestId, StagedAsk>,
}

/// One staged eliciting request: what it asked, with the wire marks that
/// choose how its eliciting context renders. The marks choose the context
/// only; they never gate whether a person's answer is shown.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StagedAsk {
    /// What it asked.
    interaction: Interaction,
    /// The extension that raised it with `host.ask`, if any.
    extension: Option<String>,
    /// The tool calls that raised it, if any.
    action_ids: Option<Vec<ActionId>>,
    /// Whether a tool call landed between the request and its resolve: the
    /// answer still renders, with no reply (`docs/permissions.md`, "What it
    /// is shown": "A tool call between that text and the human message
    /// breaks the link, so no reply is shown.").
    link_broken: bool,
}

impl StagedAsk {
    /// The eliciting content `requested` carries: what it asks, with the
    /// wire marks that choose how its eliciting context renders. A fresh
    /// staging links its reply; a tool call afterwards breaks it.
    fn of(requested: &InteractionRequested) -> Self {
        StagedAsk {
            interaction: requested.interaction.clone(),
            extension: requested.extension.clone(),
            action_ids: requested.action_ids.clone(),
            link_broken: false,
        }
    }
}

impl PendingAsks {
    /// Stages `requested` under its id, overwriting any same-id entry: a
    /// re-raise writes no new line, so raising again replaces the first
    /// staging without duplicating it.
    pub(crate) fn stage(&mut self, requested: &InteractionRequested) {
        self.staged
            .insert(requested.request_id.clone(), StagedAsk::of(requested));
    }

    /// Takes the entry `id` names, once: a second resolve for one id finds
    /// nothing after the first consumed it.
    pub(crate) fn take(&mut self, id: &RequestId) -> Option<StagedAsk> {
        self.staged.remove(id)
    }

    /// Drops every staged entry: a completed handoff restarts the
    /// reviewer's input, so no earlier question answers anything after it.
    pub(crate) fn clear(&mut self) {
        self.staged.clear();
    }

    /// Breaks every staged link: a tool call between a request and its
    /// resolve breaks the link, so the answer renders with no reply
    /// (`docs/permissions.md`, "What it is shown": "A tool call between
    /// that text and the human message breaks the link, so no reply is
    /// shown."). A request staged afterwards links fresh.
    fn break_links(&mut self) {
        for staged in self.staged.values_mut() {
            staged.link_broken = true;
        }
    }
}

/// Whether this turn was requested solely to summarise a rewind.
pub(crate) fn summary_turn(started: &TurnStarted) -> bool {
    started
        .input
        .iter()
        .any(|item| matches!(item, InputItem::Rewind { .. }))
}

/// Skips a rewind summary turn, including its start and completion. A process
/// start closes an unfinished summary window after a crash. The history fold
/// and review jig share this gate with the live conversation's summary window.
pub fn summary_skips(inside: &mut bool, event: &Event) -> bool {
    if let Event::TurnStarted(started) = event
        && summary_turn(started)
    {
        *inside = true;
    }
    let skipped = *inside;
    if matches!(event, Event::TurnCompleted(_) | Event::FiberStarted(_)) {
        *inside = false;
    }
    skipped
}

/// Adds what `event` puts in what the reviewer is shown: each person's
/// message, each tool call, and the reply each person message answers,
/// nothing else. `seq` is the durable line's
/// own `seq`: a person message with none is skipped, so every person item
/// carries the identity a later `reviewer_kept` names. The reply a person
/// message answers is the agent's text directly before it, marked as the
/// agent's; a tool call between that text and the message breaks the link,
/// so no reply is shown. A `reviewer_kept` keeps only the named person
/// items, in their current order, each with the reply it answered, and
/// drops every tool call. A completed handoff drops every tool call and
/// keeps every person item with the replies they answered, releasing the
/// calls' allocation: the reviewer's
/// input follows the context window, not the transcript.
///
/// Public for the `review` jig's replay, which folds a source log through
/// the same renderer (`docs/testing.md`, "Jigs"). `asks` stages the
/// eliciting content each request carries, so a later resolve renders the
/// person's answer with what it answers.
pub fn render_reviewed(
    reviewed: &mut Vec<Reviewed>,
    event: &Event,
    action: Option<&ActionId>,
    seq: Option<Seq>,
    asks: &mut PendingAsks,
) {
    match event {
        Event::TurnStarted(started) => {
            let pushed = reviewed.len();
            for (item, entry) in started.input.iter().enumerate() {
                if let InputItem::Message {
                    content, sender, ..
                } = entry
                    && sender.origin == Origin::Driver
                    && let Some(seq) = seq
                {
                    reviewed.push(person(
                        content,
                        Shown::Person(KeptMessage {
                            seq,
                            item: item as u64,
                        }),
                    ));
                }
            }
            // The held reply waits for the person's next message: a line
            // that pushes none leaves it with nothing to answer, so it
            // goes.
            if reviewed.len() == pushed {
                clear_held_reply(reviewed);
            }
        }
        Event::SteeringApplied(steering) => {
            let pushed = reviewed.len();
            if steering.sender.origin == Origin::Driver
                && let Some(seq) = seq
            {
                reviewed.push(person(
                    &steering.content,
                    Shown::Person(KeptMessage { seq, item: 0 }),
                ));
            }
            if reviewed.len() == pushed {
                clear_held_reply(reviewed);
            }
        }
        Event::TextCompleted(completed) => {
            push_text(reviewed, &completed.text);
        }
        Event::AssistantMessageCompleted(done) => {
            // A failed reply never reached the person, so it answers
            // nothing.
            if done.outcome == MessageOutcome::Failed {
                clear_held_reply(reviewed);
            }
        }
        Event::TurnCompleted(done) => {
            // A turn that never completed leaves text the person never
            // answered, so it answers nothing. A completed turn keeps
            // the held reply for the person's next message.
            if done.outcome != TurnOutcome::Completed {
                clear_held_reply(reviewed);
            }
        }
        Event::ToolCallRequested(call) => {
            // Any tool call between the agent's text and the person's
            // message breaks the link, shown or not: an inner or hosted
            // call the reviewer never sees still breaks it. A call between
            // a staged request and its resolve breaks that link too, so the
            // answer renders with no reply.
            clear_held_reply(reviewed);
            asks.break_links();
            if let Some(action) = action {
                let arguments = match &call.repair {
                    Some(repair) => Value::Object(repair.repaired.clone()),
                    None => call.arguments.clone(),
                };
                let rendered = format!(
                    "{{\"tool\":{},\"arguments\":{}}}",
                    serde_json::to_string(&call.name).unwrap_or_default(),
                    serde_json::to_string(&arguments).unwrap_or_default(),
                );
                reviewed.push(Reviewed {
                    shown: Shown::Call(action.clone()),
                    input: Input::User {
                        text: format!("Tool call: {rendered}"),
                        images: Vec::new(),
                    },
                });
            }
        }
        Event::HandoffCompleted(done) => {
            // At a completed handoff the reviewer's input restarts from
            // the person's messages that still bind
            // (`docs/permissions.md`, "At a handoff"), each with the
            // reply it answered: earlier tool calls
            // can never be reviewed again, so they leave the resident
            // input while every person item stays for the selection
            // that follows, with the replies they answered. A held reply
            // with no person message after it answers nothing, so it
            // goes with the calls. Shrinking the allocation keeps a busy turn's
            // peak from becoming permanent kept state. A failed or
            // cancelled handoff leaves the context as it was
            // (`docs/handoff.md`, "Recording").
            if done.outcome == Outcome::Completed {
                // A completed handoff restarts the reviewer's input: no
                // earlier staged question answers anything after it.
                asks.clear();
                let mut keep = kept_flags(reviewed, |_| true).into_iter();
                reviewed.retain(|_| keep.next().is_some_and(|stays| stays));
                reviewed.shrink_to_fit();
            }
        }
        Event::ReviewerKept(kept) => {
            // The selection keeps messages, not prose: a named person
            // item stays with the reply it answered, and nothing the
            // reviewer wrote enters its own prompt.
            let named = |message: &KeptMessage| kept.kept.contains(message);
            let mut keep = kept_flags(reviewed, named).into_iter();
            reviewed.retain(|_| keep.next().is_some_and(|stays| stays));
        }
        // Every other kind adds nothing the reviewer reads. Each is
        // listed, so a new kind does not compile until it is placed.
        Event::FiberStarted(_) | Event::FiberExited(_) | Event::SessionStarted(_) => {}
        Event::Rewound(_) | Event::Moved(_) | Event::StepStarted(_) => {}
        Event::SteeringQueue(_) | Event::ShellCommand(_) | Event::SessionNamed(_) => {}
        Event::ShellCommandStarted(_) | Event::ShellCommandDelta(_) => {}
        // A folder addition shows no call for the reviewer to judge.
        Event::DirectoryAdded(_) => {}
        Event::Clients(_) | Event::SessionStatus(_) | Event::ContextAdded(_) => {}
        Event::AssistantMessageStarted(_) | Event::AssistantMessageDelta(_) => {}
        Event::ToolCallArgumentsDelta(_) | Event::ReasoningStarted(_) => {}
        Event::ReasoningDelta(_) | Event::ReasoningCompleted(_) | Event::ToolCallStarted(_) => {}
        Event::ToolCallDelta(_) | Event::ToolCallCompleted(_) | Event::PermissionRequested(_) => {}
        Event::InteractionRequested(requested) => {
            // Every request stages, whoever raised it and whatever it
            // asks: the resolve carries no prompt, so the staged entry is
            // the only record of what it asked. The eliciting context is
            // emitted beside the resolved answer, never staged in the
            // list, so model text is never marked and a prior model reply
            // answers nothing, while an earlier pending question keeps its
            // own staged entry for its own resolve.
            asks.stage(requested);
            clear_held_reply(reviewed);
        }
        Event::InteractionResolved(resolved) => {
            // The one rule over the person's input: a person's resolve
            // renders as a person item holding the answer, with the staged
            // eliciting content as its reply, for every interaction kind
            // and every raiser. Nothing else about the request gates the
            // showing. A fiber resolve is no person's input: it consumes
            // the staged entry and drops it, as does a resolve with no
            // line of its own or one no request staged.
            let staged = asks.take(&resolved.request_id);
            match (&resolved.by, seq, staged) {
                (ResolvedBy::Person, Some(seq), Some(staged)) => {
                    // The resolve emits its question reply immediately
                    // before its person item, so interleaved resolves pair
                    // exact. When a call broke the link first, the answer
                    // still renders, with no reply.
                    if !staged.link_broken {
                        reviewed.push(eliciting_reply(&staged));
                    }
                    reviewed.push(person_answer(&resolved.answer, &staged.interaction, seq));
                }
                (_, _, Some(_)) => {}
                (_, _, None) => {}
            }
        }
        Event::PermissionResolved(_) => {}
        Event::RepositoryCodeOffered(_) | Event::RepositoryCodeResolved(_) => {}
        Event::UsageRecorded(_)
        | Event::QuotaNoticed(_)
        | Event::QuotaRearmed(_)
        | Event::RetryScheduled(_) => {}
        Event::Notice(_) | Event::PreambleBuilt(_) | Event::ModelChanged(_) => {}
        Event::OpeningMessage(_) | Event::InstructionFile(_) | Event::DateChanged(_) => {}
        Event::SkillsChanged(_) | Event::SkillsResent(_) => {}
        Event::HandoffStarted(_) | Event::ContextNudged(_) => {}
        Event::McpServerFailed(_) | Event::McpServerReady(_) | Event::Reloaded(_) => {}
        Event::ExtensionsLoaded(_)
        | Event::ExtensionStateSet(_)
        | Event::ExtensionStateUnset(_) => {}
        Event::ExtensionUi(_)
        | Event::ExtensionMessage(_)
        | Event::ExtensionLog(_)
        | Event::ExtensionHello(_)
        | Event::ExtensionRequest(_)
        | Event::ExtensionExec(_) => {}
        Event::JobStarted(_) | Event::DelegateStarted(_) | Event::JobDelta(_) => {}
        Event::JobLine(_) | Event::DelegateFinished(_) | Event::JobCompleted(_) => {}
        Event::JobsPendingNotified(_) | Event::CommandAccepted(_) | Event::CommandRejected(_) => {}
    }
}

impl Reviewed {
    /// Extracts every person's displayed reply/message pair in occurrence
    /// order. A reply belongs to the following consecutive person run with
    /// the same sequence, matching `kept_flags`; rendered inputs preserve
    /// the reviewer's cap and labels.
    pub(crate) fn person_pairs(reviewed: &[Self]) -> Vec<(KeptMessage, String)> {
        reviewed
            .iter()
            .enumerate()
            .filter_map(|(at, person)| {
                let key = match &person.shown {
                    Shown::Person(key) => *key,
                    Shown::Call(_) | Shown::Reply(_) => return None,
                };
                let Input::User { text, .. } = &person.input else {
                    return None;
                };
                let reply = reviewed
                    .iter()
                    .take(at)
                    .rev()
                    .find_map(|prior| match &prior.shown {
                        Shown::Person(previous) if previous.seq == key.seq => None,
                        Shown::Reply(_) => match &prior.input {
                            Input::User { text, .. } => Some(Some(text.as_str())),
                            Input::Assistant { .. }
                            | Input::Reasoning { .. }
                            | Input::ToolCall { .. }
                            | Input::ToolResult { .. }
                            | Input::ToolSearchResult { .. } => Some(None),
                        },
                        Shown::Call(_) | Shown::Person(_) => Some(None),
                    })
                    .flatten();
                let text = match reply {
                    Some(reply) => format!("{reply}\n{text}"),
                    None => text.clone(),
                };
                Some((key, text))
            })
            .collect()
    }
}

/// How many characters of a reply the reviewer reads: a longer reply
/// keeps its head, then a line saying how many characters were left out.
/// A proposal usually sits at the end of a reply, so a cut reply can lose
/// its question and the person's yes then approves nothing: cut replies
/// fail closed.
const REPLY_CAP: usize = 6_000;

/// The shown body of the agent's raw reply text: the whole text within
/// the cap, else its first `REPLY_CAP` characters and a line saying how
/// many were left out. The cut counts characters, never bytes, so a
/// multi-byte character never splits.
fn reply(raw: &str) -> String {
    let total = raw.chars().count();
    if total <= REPLY_CAP {
        return raw.to_owned();
    }
    let head: String = raw.chars().take(REPLY_CAP).collect();
    let left_out = total.saturating_sub(REPLY_CAP);
    format!("{head}\n[Reply cut here; characters not shown: {left_out}]")
}

/// The reviewer's item for the agent's raw reply text, marked as the
/// agent's. Rebuilt from the raw string at each extension: the cap
/// applies to the shown body only, never to the stored text, so a later
/// part never extends a cut body.
fn reply_item(raw: &str) -> Reviewed {
    reply_item_as("The agent", raw)
}

/// The reviewer's item for raw eliciting text marked as `who` wrote it:
/// the agent for its own asked questions, fiber for its own channel
/// description. The cap applies the same way as for agent replies. The
/// stored text is the plain eliciting body: the context is emitted beside
/// its answer, never staged in the list, so no mark distinguishes it from
/// model text.
fn reply_item_as(who: &str, raw: &str) -> Reviewed {
    Reviewed {
        shown: Shown::Reply(raw.to_owned()),
        input: Input::User {
            text: format!("{who}: {}", reply(raw)),
            images: Vec::new(),
        },
    }
}

/// The eliciting context a staged request answers with: the asked
/// questions as the agent wrote them for an agent-raised form, fiber's
/// own description of the channel otherwise. A server's or extension's
/// submitted prompt words never reach the reviewer: repository-produced
/// text stays out of its prompt (`docs/permissions.md`, "What it is
/// shown"), so the reviewer learns which channel asked, never the words
/// it asked with; the extension name renders as data. No arm compares a
/// tool, extension, or server name.
fn eliciting_reply(staged: &StagedAsk) -> Reviewed {
    match agent_questions(staged) {
        Some(questions) => reply_item_as("The agent", &questions),
        None => reply_item_as("Fiber", &channel_sentence(staged)),
    }
}

/// The asked questions as the agent wrote them, for the one raiser with
/// questions of its own on the wire: a form with no extension and a
/// non-empty raising call list. Any other shape takes fiber's channel
/// description instead.
fn agent_questions(staged: &StagedAsk) -> Option<String> {
    if staged.extension.is_some() {
        return None;
    }
    if !staged
        .action_ids
        .as_ref()
        .is_some_and(|ids| !ids.is_empty())
    {
        return None;
    }
    match &staged.interaction {
        Interaction::Form { fields } => Some(
            fields
                .iter()
                .map(form_question)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Interaction::Confirm { .. }
        | Interaction::Select { .. }
        | Interaction::MultiSelect { .. }
        | Interaction::TextInput { .. } => None,
    }
}

/// One asked question with its options, one line each: the question, then
/// one `- <label>` line per option with ` (<description>)` appended when
/// present.
fn form_question(field: &contract::shapes::Question) -> String {
    let mut lines = vec![field.question.clone()];
    lines.extend(
        field
            .options
            .iter()
            .map(|choice| match &choice.description {
                Some(description) => format!("- {} ({description})", choice.label),
                None => format!("- {}", choice.label),
            }),
    );
    lines.join("\n")
}

/// Fiber's own description of the channel that asked: the named extension
/// channel, a server elicitation, or a question with no recorded asker.
/// The submitted prompt words stay out in every case.
fn channel_sentence(staged: &StagedAsk) -> String {
    if let Some(name) = &staged.extension {
        format!("A question from the {name} extension. Its words are not shown.")
    } else if !matches!(staged.interaction, Interaction::Form { .. })
        && staged
            .action_ids
            .as_ref()
            .is_some_and(|ids| !ids.is_empty())
    {
        "A question from a server elicitation. Its words are not shown.".to_owned()
    } else {
        "A question with no recorded asker. Its words are not shown.".to_owned()
    }
}

/// A person's answer as the reviewer reads it, with the resolving line's
/// identity: the `seq` of the resolving line, item zero.
fn person_answer(answer: &Answer, staged: &Interaction, seq: Seq) -> Reviewed {
    Reviewed {
        shown: Shown::Person(KeptMessage { seq, item: 0 }),
        input: Input::User {
            text: format!("The person: {}", answer_body(answer, staged)),
            images: Vec::new(),
        },
    }
}

/// The answer's body, rendered from the answer's own variant: a form reads
/// its headers from the staged questions it answers (`docs/tools.md`,
/// "The result"). A cancelled form is the single line `declined`.
fn answer_body(answer: &Answer, staged: &Interaction) -> String {
    match answer {
        Answer::Declined { .. } => "declined".to_owned(),
        Answer::Confirmed { confirmed } => {
            if *confirmed {
                "yes".to_owned()
            } else {
                "no".to_owned()
            }
        }
        Answer::Labels { labels } => labels
            .iter()
            .map(|label| escaped(label))
            .collect::<Vec<_>>()
            .join(", "),
        Answer::Text { text } => quoted(text),
        Answer::Form { answers, note } => {
            let headers: Vec<String> = match staged {
                Interaction::Form { fields } => {
                    fields.iter().map(|field| escaped(&field.header)).collect()
                }
                Interaction::Confirm { .. }
                | Interaction::Select { .. }
                | Interaction::MultiSelect { .. }
                | Interaction::TextInput { .. } => Vec::new(),
            };
            let mut lines: Vec<String> = answers
                .iter()
                .enumerate()
                .map(|(at, answer)| {
                    let said = match answer {
                        FormAnswer::Skipped { .. } => "skipped".to_owned(),
                        FormAnswer::Answered { labels, text } => {
                            let mut parts: Vec<String> =
                                labels.iter().map(|label| escaped(label)).collect();
                            parts.extend(text.as_deref().map(quoted));
                            parts.join(", ")
                        }
                    };
                    match headers.get(at) {
                        Some(header) => format!("{header}: {said}"),
                        None => said,
                    }
                })
                .collect();
            lines.extend(
                note.as_deref()
                    .map(|note| format!("note: {}", quoted(note))),
            );
            lines.join("\n")
        }
    }
}

/// `text` as a JSON string, quotes included.
fn quoted(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

/// `text` with JSON's escapes but without the quotes around it.
fn escaped(text: &str) -> String {
    let quoted = quoted(text);
    quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(&quoted)
        .to_owned()
}

/// Adds one text part to the held reply at the tail, or starts one. An
/// empty or whitespace-only part adds nothing; parts join with `\n` in
/// the raw string the person's next message answers. Only the tail item
/// is ever extended, so the part already sent stays append-only.
fn push_text(reviewed: &mut Vec<Reviewed>, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    if let Some(held) = reviewed.last_mut()
        && let Shown::Reply(raw) = &mut held.shown
    {
        raw.push('\n');
        raw.push_str(text);
        let body = reply(raw);
        held.input = Input::User {
            text: format!("The agent: {body}"),
            images: Vec::new(),
        };
        return;
    }
    reviewed.push(reply_item(text));
}

/// Removes the held reply at the tail, if any: a tool call, a failed
/// message or an unsettled turn between the agent's text and the person's
/// message breaks the link, so nothing is shown.
fn clear_held_reply(reviewed: &mut Vec<Reviewed>) {
    if matches!(
        reviewed.last(),
        Some(Reviewed {
            shown: Shown::Reply(_),
            ..
        })
    ) {
        reviewed.pop();
    }
}

/// Which items stay when the persons `named` stay: every named person
/// item, and a reply while any person item of the run right after it
/// (the consecutive person items with its line's `seq`) is named. Tool
/// calls never stay, and neither does any other reply, including a held
/// reply with no person message after it.
fn kept_flags(reviewed: &[Reviewed], named: impl Fn(&KeptMessage) -> bool) -> Vec<bool> {
    // Walked back to front: a reply's run follows it, so going backwards
    // names the run before its reply is judged.
    let mut keep = Vec::with_capacity(reviewed.len());
    let mut run_seq: Option<Seq> = None;
    let mut run_named = false;
    for item in reviewed.iter().rev() {
        match &item.shown {
            Shown::Call(_) => {
                keep.push(false);
                run_seq = None;
                run_named = false;
            }
            Shown::Person(message) => {
                let stays = named(message);
                if run_seq != Some(message.seq) {
                    run_seq = Some(message.seq);
                    run_named = false;
                }
                run_named |= stays;
                keep.push(stays);
            }
            Shown::Reply(_) => {
                keep.push(run_named);
                run_seq = None;
                run_named = false;
            }
        }
    }
    keep.reverse();
    keep
}

/// A person's message as the reviewer reads it.
fn person(content: &[ContentPart], shown: Shown) -> Reviewed {
    Reviewed {
        shown,
        input: Input::User {
            text: format!("The person: {}", crate::conversation::text(content)),
            images: Vec::new(),
        },
    }
}

#[cfg(test)]
#[path = "shown_tests.rs"]
mod tests;
