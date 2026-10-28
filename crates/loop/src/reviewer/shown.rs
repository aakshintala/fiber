//! What the reviewer is shown (`docs/permissions.md`, "What it is shown"):
//! each person's message, each tool call, and the reply each person message
//! answers, nothing else.

use contract::events::{
    Event, InputItem, KeptMessage, MessageOutcome, Outcome, TurnOutcome, TurnStarted,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, Origin};
use contract::{ActionId, Seq};
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
/// the same renderer (`docs/testing.md`, "Jigs").
pub fn render_reviewed(
    reviewed: &mut Vec<Reviewed>,
    event: &Event,
    action: Option<&ActionId>,
    seq: Option<Seq>,
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
            // call the reviewer never sees still breaks it.
            clear_held_reply(reviewed);
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
        Event::Clients(_) | Event::SessionStatus(_) | Event::ContextAdded(_) => {}
        Event::AssistantMessageStarted(_) | Event::AssistantMessageDelta(_) => {}
        Event::ToolCallArgumentsDelta(_) | Event::ReasoningStarted(_) => {}
        Event::ReasoningDelta(_) | Event::ReasoningCompleted(_) | Event::ToolCallStarted(_) => {}
        Event::ToolCallDelta(_) | Event::ToolCallCompleted(_) | Event::PermissionRequested(_) => {}
        Event::PermissionResolved(_)
        | Event::InteractionRequested(_)
        | Event::InteractionResolved(_) => {}
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
    Reviewed {
        shown: Shown::Reply(raw.to_owned()),
        input: Input::User {
            text: format!("The agent: {}", reply(raw)),
            images: Vec::new(),
        },
    }
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
