//! Handoff resends loaded skills (`docs/handoff.md`, "What the model sees
//! after a handoff"): after a completed handoff each skill the previous
//! context loaded is sent again, once, in first-load order, with the body
//! read from disk at the handoff, logged as `skills_resent`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

use contract::emit::Emit;
use contract::events::{CallStatus, Control, SkillLoad, ToolCallCompleted, TurnOutcome};
use contract::inbox::Delivery;
use contract::provider::{HostedCall, Input, ReplyAction, ToolDefinition};
use contract::shapes::{ContentPart, DeclaredEffects, Effect};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{Envelope, ProviderCallId, SessionId};
use fakes::{Deadline, Scripted, ScriptedProvider};
use serde_json::{Map, Value, json};

use support::{
    DEADLINE, ENDED, OPENING, REPLY, STEP, Session, StartHooks, assert_kinds, calls_reply,
    delivery, handoff, kinds, of_kind, start_added, start_empty,
};

/// One reply calling one skill: streamed arguments, requests, then runs.
const SKILL1: &[&str] = &[
    "assistant_message_started",
    "assistant_message_delta",
    "tool_call_arguments_delta",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_completed",
];
/// One reply calling two skills.
const SKILL2: &[&str] = &[
    "assistant_message_started",
    "assistant_message_delta",
    "tool_call_arguments_delta",
    "tool_call_arguments_delta",
    "tool_call_requested",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_started",
    "tool_call_completed",
    "tool_call_completed",
];
/// One reply calling three skills.
const SKILL3: &[&str] = &[
    "assistant_message_started",
    "assistant_message_delta",
    "tool_call_arguments_delta",
    "tool_call_arguments_delta",
    "tool_call_arguments_delta",
    "tool_call_requested",
    "tool_call_requested",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_started",
    "tool_call_started",
    "tool_call_completed",
    "tool_call_completed",
    "tool_call_completed",
];
/// A hosted provider-run record carrying a skill load: the provider ran
/// the call, so the request and completion come without streamed arguments.
const HOSTED_SKILL: &[&str] = &[
    "assistant_message_started",
    "tool_call_requested",
    "tool_call_started",
    "tool_call_completed",
    "usage_recorded",
    "assistant_message_completed",
];
/// A handoff whose streamed note the model wrote, through the completion.
const HANDOFF_NOTE: &[&str] = &[
    "handoff_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "handoff_completed",
];
/// Three durable skill calls as the log keeps them: no streamed deltas.
const DURABLE_SKILL3: &[&str] = &[
    "assistant_message_started",
    "tool_call_requested",
    "tool_call_requested",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_started",
    "tool_call_started",
    "tool_call_completed",
    "tool_call_completed",
    "tool_call_completed",
];
/// A handoff note as the log keeps it: no streamed deltas.
const DURABLE_NOTE: &[&str] = &[
    "handoff_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "handoff_completed",
];
/// A text reply as the log keeps it: no streamed deltas.
const DURABLE_TEXT: &[&str] = &[
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
];

/// A tool named `skill`: it reads `workspace/.agents/skills/{name}/SKILL.md`,
/// answers the header's body under the file's path, and records the load.
/// `after` runs after each read, so a test can change or remove the file
/// before the handoff reads it again.
/// Runs after the `skill` tool reads a file: `hook(name, file)`.
type AfterRead = Box<dyn Fn(&str, &Path) + Send>;

struct Skill {
    workspace: Mutex<PathBuf>,
    after: Mutex<Option<AfterRead>>,
}

impl Skill {
    fn named() -> Arc<Self> {
        Arc::new(Self {
            workspace: Mutex::new(PathBuf::new()),
            after: Mutex::new(None),
        })
    }
}

impl Tool for Skill {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "skill".into(),
            description: "Loads a skill.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let name = arguments
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let file = self
            .workspace
            .lock()
            .unwrap()
            .join(".agents/skills")
            .join(&name)
            .join("SKILL.md");
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let body = text.splitn(3, "---\n").nth(2).unwrap_or("").to_owned();
        if let Some(after) = self.after.lock().unwrap().as_ref() {
            after(&name, &file);
        }
        let path = file.to_string_lossy().into_owned();
        Output {
            content: vec![ContentPart::Text {
                text: format!("{path}\n\n{body}"),
            }],
            control: Some(Control {
                handoff: None,
                questions: None,
                skill: Some(SkillLoad {
                    name,
                    path: path.clone(),
                }),
                name: None,
            }),
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

fn write_skill(workspace: &Path, name: &str, body: &str) {
    let dir = workspace.join(".agents/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: The {name} skill.\n---\n{body}"),
    )
    .unwrap();
}

fn load_calls(names: &[&str]) -> Scripted {
    let calls: Vec<(&str, Value)> = names
        .iter()
        .map(|name| ("skill", json!({"name": name})))
        .collect();
    calls_reply("", &calls)
}

fn user(text: &str) -> Input {
    Input::User {
        text: text.into(),
        images: Vec::new(),
    }
}

/// A session whose `skill` tool reads `session.workspace`, with alpha and
/// beta on disk.
fn start(script: Vec<Scripted>, during: Vec<Delivery>, skill: &Arc<Skill>) -> Session {
    let session =
        Session::with_tools_injecting(script, during, vec![skill.clone() as Arc<dyn Tool>]);
    *skill.workspace.lock().unwrap() = session.workspace.clone();
    write_skill(&session.workspace, "alpha", "Alpha body.\n");
    write_skill(&session.workspace, "beta", "Beta body.\n");
    session
}

fn run(session: &mut Session, prompt: &str) -> (Option<TurnOutcome>, Vec<Envelope>) {
    session.inbox.send(delivery(prompt)).unwrap();
    let outcome = session.turn();
    (outcome, session.lines())
}

/// The names of a `skills_resent` line's skills, in order.
/// The sent skills of a `skills_resent` line: name, path and content.
fn payload_skills(line: &Envelope) -> Vec<(String, String, String)> {
    line.payload["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| {
            (
                skill["name"].as_str().unwrap().to_owned(),
                skill["path"].as_str().unwrap().to_owned(),
                skill["content"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// The resend notices: `invalid_arguments` lines naming the skill that was
/// not sent again after the handoff.
fn resend_notices(lines: &[Envelope]) -> Vec<&Envelope> {
    of_kind(lines, "notice")
        .into_iter()
        .filter(|line| {
            line.payload["message"]
                .as_str()
                .unwrap_or("")
                .contains(" was not sent again after the handoff: ")
        })
        .collect()
}

fn user_texts(conversation: &[Input]) -> Vec<&str> {
    conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } => Some(text.as_str()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect()
}

fn hook(hooks: Arc<StartHooks>, session: &mut Session) {
    session.looped = session
        .looped
        .take()
        .map(|looped| looped.hooks(hooks as Arc<dyn contract::hook::Hooks>));
}

#[test]
fn loaded_skills_are_sent_again_once_each_in_first_load_order_after_the_note() {
    let skill = Skill::named();
    let mut session = start(
        vec![
            load_calls(&["alpha", "beta", "alpha"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            SKILL3,
            STEP,
            HANDOFF_NOTE,
            &["skills_resent", "opening_message"],
            REPLY,
            ENDED,
        ],
    );
    // One durable line, between the handoff and the new opening.
    let all = kinds(&lines);
    let at = all
        .iter()
        .position(|kind| *kind == "handoff_completed")
        .unwrap();
    assert_eq!(all[at + 1], "skills_resent");
    assert_eq!(all[at + 2], "opening_message");
    let resent = of_kind(&lines, "skills_resent");
    assert_eq!(resent.len(), 1);
    assert!(resent[0].is_durable());
    let sent = payload_skills(resent[0]);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].0, "alpha");
    assert_eq!(sent[1].0, "beta");
    assert!(sent[0].1.ends_with(".agents/skills/alpha/SKILL.md"));
    assert!(sent[1].1.ends_with(".agents/skills/beta/SKILL.md"));
    assert_eq!(sent[0].2, "Alpha body.");
    assert_eq!(sent[1].2, "Beta body.");
    assert!(resend_notices(&lines).is_empty());
    // The next request ends with the note and the two skills.
    let requests = session.requests();
    assert_eq!(requests.len(), 3);
    let next = &requests[2].conversation;
    assert_eq!(next.len(), 5);
    assert_eq!(next[1], user("hi"));
    assert_eq!(next[2], user("The note."));
    assert_eq!(next[3], user(&format!("{}\n\n{}", sent[0].1, sent[0].2)));
    assert_eq!(next[4], user(&format!("{}\n\n{}", sent[1].1, sent[1].2)));
    assert_eq!(requests[2].previous_end, None);
    // The new opening lists both skills.
    let openings = of_kind(&lines, "opening_message");
    assert_eq!(openings.len(), 2);
    let names: Vec<&str> = openings[1].payload["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| skill["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpha", "beta"]);
}

#[test]
fn a_body_edited_since_the_load_is_sent_as_it_is_at_the_handoff() {
    let skill = Skill::named();
    *skill.after.lock().unwrap() = Some(Box::new(|name, file| {
        if name == "alpha" {
            std::fs::write(
                file,
                "---\nname: alpha\ndescription: The alpha skill.\n---\nAlpha revised.\n",
            )
            .unwrap();
        }
    }));
    let mut session = start(
        vec![
            load_calls(&["alpha"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            SKILL1,
            STEP,
            HANDOFF_NOTE,
            &["skills_resent", "opening_message"],
            REPLY,
            ENDED,
        ],
    );
    let resent = of_kind(&lines, "skills_resent");
    assert_eq!(resent.len(), 1);
    let sent = payload_skills(resent[0]);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].2, "Alpha revised.");
    let requests = session.requests();
    let next = &requests[2].conversation;
    assert_eq!(
        *next.last().unwrap(),
        user(&format!("{}\n\n{}", sent[0].1, sent[0].2))
    );
}

#[test]
fn a_context_with_no_loaded_skills_sends_nothing_extra() {
    let skill = Skill::named();
    let mut session = start(
        vec![
            Scripted::text("Hello."),
            Scripted::text("The note."),
            Scripted::text("Done."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            REPLY,
            STEP,
            HANDOFF_NOTE,
            &["opening_message"],
            REPLY,
            ENDED,
        ],
    );
    assert!(of_kind(&lines, "skills_resent").is_empty());
    assert!(resend_notices(&lines).is_empty());
    let requests = session.requests();
    let next = &requests[2].conversation;
    assert_eq!(next.len(), 3);
    assert_eq!(next[1], user("hi"));
    assert_eq!(next[2], user("The note."));
}

#[test]
fn a_skill_whose_file_is_gone_is_not_sent_and_the_new_listing_omits_it() {
    let skill = Skill::named();
    *skill.after.lock().unwrap() = Some(Box::new(|name, file| {
        if name == "beta" {
            std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
        }
    }));
    let mut session = start(
        vec![
            load_calls(&["alpha", "beta"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            SKILL2,
            STEP,
            HANDOFF_NOTE,
            &["skills_resent", "opening_message", "notice"],
            REPLY,
            ENDED,
        ],
    );
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["outcome"], "completed");
    let resent = of_kind(&lines, "skills_resent");
    assert_eq!(resent.len(), 1);
    let sent = payload_skills(resent[0]);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "alpha");
    let notices = resend_notices(&lines);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].payload["code"], "invalid_arguments");
    assert!(
        notices[0].payload["message"]
            .as_str()
            .unwrap()
            .starts_with("Skill beta was not sent again after the handoff: "),
        "{}",
        notices[0].payload["message"]
    );
    let openings = of_kind(&lines, "opening_message");
    let names: Vec<&str> = openings.last().unwrap().payload["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| skill["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpha"]);
    let requests = session.requests();
    let texts = user_texts(&requests[2].conversation);
    assert!(
        !texts.iter().any(|text| text.contains("Beta body")),
        "{texts:?}"
    );

    // Both gone: no line, one grouped notice naming both skills, and the
    // handoff still completes (`docs/errors.md`, "Notices": one notice per
    // code; `docs/handoff.md`, "What the model sees after a handoff": a
    // skill left out at the handoff gets a `notice` naming it and the
    // reason). The notice lands at the opening flush, after the new
    // `opening_message`.
    let skill = Skill::named();
    *skill.after.lock().unwrap() = Some(Box::new(|_, file| {
        std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }));
    let mut session = start(
        vec![
            load_calls(&["alpha", "beta"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            SKILL2,
            STEP,
            HANDOFF_NOTE,
            &["opening_message", "notice"],
            REPLY,
            ENDED,
        ],
    );
    assert!(of_kind(&lines, "skills_resent").is_empty());
    let resend = resend_notices(&lines);
    assert_eq!(resend.len(), 1);
    assert_eq!(resend[0].payload["code"], "invalid_arguments");
    let message = resend[0].payload["message"].as_str().unwrap();
    assert!(
        message.starts_with(
            "2 `invalid_arguments` notices: Skill alpha was not sent again after the handoff: "
        ),
        "{message}"
    );
    assert!(
        message.contains("Skill beta was not sent again after the handoff: "),
        "{message}"
    );
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done[0].payload["outcome"], "completed");
}

#[test]
fn a_skill_whose_header_no_longer_parses_is_not_sent_and_the_handoff_still_completes() {
    let skill = Skill::named();
    *skill.after.lock().unwrap() = Some(Box::new(|name, file| {
        if name == "beta" {
            std::fs::write(file, "no header here\n").unwrap();
        }
    }));
    let mut session = start(
        vec![
            load_calls(&["beta"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            SKILL1,
            STEP,
            HANDOFF_NOTE,
            &["opening_message", "notice", "notice"],
            REPLY,
            ENDED,
        ],
    );
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done[0].payload["outcome"], "completed");
    assert!(of_kind(&lines, "skills_resent").is_empty());
    let notices = resend_notices(&lines);
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0].payload["message"]
            .as_str()
            .unwrap()
            .starts_with("Skill beta was not sent again after the handoff: "),
        "{}",
        notices[0].payload["message"]
    );
    // The opening's own invalid-skill notice is a separate line.
    let invalid: Vec<&Envelope> = of_kind(&lines, "notice")
        .into_iter()
        .filter(|line| line.payload["code"] == "skill_invalid")
        .collect();
    assert_eq!(invalid.len(), 1);
    // The next turn runs with no beta text.
    let requests = session.requests();
    let texts = user_texts(&requests[2].conversation);
    assert!(
        !texts.iter().any(|text| text.contains("Beta body")),
        "{texts:?}"
    );
}

#[test]
fn a_skill_file_that_became_a_link_into_the_credentials_directory_is_not_sent() {
    let skill = Skill::named();
    let mut session = start(
        vec![
            load_calls(&["alpha", "beta"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let target = session.credentials.join("sec").join("SKILL.md");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(
        &target,
        "---\nname: sec\ndescription: Secret.\n---\nMARKER body.\n",
    )
    .unwrap();
    *skill.after.lock().unwrap() = Some(Box::new(move |name, file| {
        if name == "beta" {
            std::fs::remove_file(file).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, file).unwrap();
        }
    }));
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            SKILL2,
            STEP,
            HANDOFF_NOTE,
            &["skills_resent", "opening_message", "notice"],
            REPLY,
            ENDED,
        ],
    );
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done[0].payload["outcome"], "completed");
    for line in of_kind(&lines, "skills_resent") {
        for skill in line.payload["skills"].as_array().unwrap() {
            assert_ne!(skill["name"], "beta");
        }
    }
    assert_eq!(resend_notices(&lines).len(), 1);
    // The credential bytes reach neither the log nor any request.
    let logged = serde_json::to_string(&lines).unwrap();
    assert!(!logged.contains("MARKER"), "{logged}");
    for request in &session.requests() {
        let text = serde_json::to_string(&request.conversation[..]).unwrap();
        assert!(!text.contains("MARKER"), "{text}");
    }
}

#[test]
fn a_skill_the_permission_layer_refuses_is_left_out_with_a_notice() {
    // The link's header still names the loaded skill, so the handoff's
    // fresh listing holds beta and only the resend's permission check
    // leaves it out: without `refuse` the credential bytes would be read
    // and sent again.
    let skill = Skill::named();
    let mut session = start(
        vec![
            load_calls(&["alpha", "beta"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let target = session.credentials.join("sec").join("SKILL.md");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(
        &target,
        "---\nname: beta\ndescription: The beta skill.\n---\nMARKER body.\n",
    )
    .unwrap();
    *skill.after.lock().unwrap() = Some(Box::new(move |name, file| {
        if name == "beta" {
            std::fs::remove_file(file).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, file).unwrap();
        }
    }));
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            SKILL2,
            STEP,
            HANDOFF_NOTE,
            &["skills_resent", "opening_message", "notice"],
            REPLY,
            ENDED,
        ],
    );
    let done = of_kind(&lines, "handoff_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["outcome"], "completed");
    let resent = of_kind(&lines, "skills_resent");
    assert_eq!(resent.len(), 1);
    let sent = payload_skills(resent[0]);
    let names: Vec<&str> = sent.iter().map(|skill| skill.0.as_str()).collect();
    assert_eq!(names, ["alpha"]);
    let notices = resend_notices(&lines);
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0].payload["message"]
            .as_str()
            .unwrap()
            .starts_with("Skill beta was not sent again after the handoff: "),
        "{}",
        notices[0].payload["message"]
    );
    // The credential bytes reach neither the log nor any request.
    let logged = serde_json::to_string(&lines).unwrap();
    assert!(!logged.contains("MARKER"), "{logged}");
    for request in &session.requests() {
        let text = serde_json::to_string(&request.conversation[..]).unwrap();
        assert!(!text.contains("MARKER"), "{text}");
    }
}

#[test]
fn a_resume_renders_the_same_bytes() {
    let skill = Skill::named();
    let mut session = start(
        vec![
            load_calls(&["alpha", "beta", "alpha"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
            Scripted::text("After resume."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let (outcome, _) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let recorded = session.requests()[2].conversation.clone();
    session.resume(Vec::new());
    session.inbox.send(delivery("again")).unwrap();
    let outcome = session.turn();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let requests = session.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(&requests[3].conversation[..recorded.len()], &recorded[..]);
}

#[test]
fn the_session_start_context_stays_between_the_opening_and_the_input() {
    let skill = Skill::named();
    let hooks = StartHooks::new();
    hooks.answer(start_empty());
    hooks.answer(start_added("fiber.test/inbox", "hook context"));
    let mut session = start(
        vec![
            load_calls(&["alpha", "beta"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
            Scripted::text("After resume."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    hook(hooks.clone(), &mut session);
    let (outcome, _) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let live = session.requests()[2].conversation.clone();
    assert_eq!(live.len(), 6);
    assert_eq!(live[1], user("hook context"));
    assert_eq!(live[2], user("hi"));
    assert_eq!(live[3], user("The note."));
    assert!(matches!(&live[4], Input::User { text, .. } if text.contains("Alpha body")));
    assert!(matches!(&live[5], Input::User { text, .. } if text.contains("Beta body")));
    // The resume renders the identical bytes, then the new input.
    hooks.answer(start_empty());
    session.resume(Vec::new());
    session.inbox.send(delivery("again")).unwrap();
    let outcome = session.turn();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let requests = session.requests();
    assert_eq!(&requests[3].conversation[..live.len()], &live[..]);
}

/// A handoff sent while the loop is idle starts its own turn; each turn ends
/// before the test sends the next handoff, so the rounds need no rendezvous.
fn idle_handoff(session: &mut Session, id: &str) -> (Option<TurnOutcome>, Vec<Envelope>) {
    session.inbox.send(handoff(id, None)).unwrap();
    let outcome = session.turn();
    (outcome, session.lines())
}

#[test]
fn a_skill_sent_again_is_sent_at_the_next_handoff_until_its_file_is_gone() {
    let skill = Skill::named();
    let mut session = start(
        vec![
            load_calls(&["alpha", "beta", "alpha"]),
            Scripted::text("End one."),
            Scripted::text("Note 1."),
            load_calls(&["alpha"]),
            Scripted::text("End more."),
            Scripted::text("Note 2."),
            Scripted::text("Note 3."),
            Scripted::text("Note 4."),
        ],
        Vec::new(),
        &skill,
    );
    let (outcome, mut lines) = run(&mut session, "go");
    assert_eq!(outcome, Some(TurnOutcome::Completed));

    let (outcome, second) = idle_handoff(&mut session, "c_h1");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    lines.extend(second);
    // A fresh load of alpha in the new context, between the handoffs.
    let (outcome, third) = run(&mut session, "more");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    lines.extend(third);
    let (outcome, fourth) = idle_handoff(&mut session, "c_h2");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    lines.extend(fourth);
    // The alpha load between the handoffs keeps first-load order.
    let resent = of_kind(&lines, "skills_resent");
    assert_eq!(resent.len(), 2);
    for line in &resent {
        let sent = payload_skills(line);
        let names: Vec<&str> = sent.iter().map(|skill| skill.0.as_str()).collect();
        assert_eq!(names, ["alpha", "beta"]);
    }

    // Beta's file is gone before the third handoff: only alpha is sent,
    // with one notice naming beta.
    std::fs::remove_dir_all(session.workspace.join(".agents/skills/beta")).unwrap();
    let (outcome, fifth) = idle_handoff(&mut session, "c_h3");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    lines.extend(fifth);
    let resent = of_kind(&lines, "skills_resent");
    assert_eq!(resent.len(), 3);
    let sent = payload_skills(resent[2]);
    let names: Vec<&str> = sent.iter().map(|skill| skill.0.as_str()).collect();
    assert_eq!(names, ["alpha"]);
    assert_eq!(resend_notices(&lines).len(), 1);

    // The fourth handoff sends alpha with no new notice.
    let (outcome, sixth) = idle_handoff(&mut session, "c_h4");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    lines.extend(sixth);
    let resent = of_kind(&lines, "skills_resent");
    assert_eq!(resent.len(), 4);
    let sent = payload_skills(resent[3]);
    let names: Vec<&str> = sent.iter().map(|skill| skill.0.as_str()).collect();
    assert_eq!(names, ["alpha"]);
    assert_eq!(resend_notices(&lines).len(), 1);
    // The last request carries alpha and no beta.
    let requests = session.requests();
    let texts = user_texts(&requests.last().unwrap().conversation);
    assert!(texts.iter().any(|text| text.contains("Alpha body")));
    assert!(
        !texts.iter().any(|text| text.contains("Beta body")),
        "{texts:?}"
    );
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            SKILL3,
            STEP,
            REPLY,
            ENDED,
            &["turn_started"],
            STEP,
            HANDOFF_NOTE,
            &["skills_resent", "opening_message"],
            ENDED,
            &["turn_started"],
            STEP,
            SKILL1,
            STEP,
            REPLY,
            ENDED,
            &["turn_started"],
            STEP,
            HANDOFF_NOTE,
            &["skills_resent", "opening_message"],
            ENDED,
            &["skills_changed", "turn_started"],
            STEP,
            HANDOFF_NOTE,
            &["skills_resent", "opening_message", "notice"],
            ENDED,
            &["turn_started"],
            STEP,
            HANDOFF_NOTE,
            &["skills_resent", "opening_message"],
            ENDED,
        ],
    );
}

#[test]
fn a_crash_after_handoff_completed_sends_the_skills_with_the_next_opening() {
    let skill = Skill::named();
    let mut session = start(
        vec![
            load_calls(&["alpha", "beta", "alpha"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
            Scripted::text("After resume."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let (outcome, _) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let recorded = session.requests()[2].conversation.clone();

    // Cut the log after the handoff, then resume from the cut file: the
    // resume writes the resend and the opening, and the request matches the
    // live one except for the resumed turn's own new input, appended last.
    truncate_after(&session, "handoff_completed");
    let (mut looped, inbox, provider) =
        resume_truncated(&mut session, &skill, vec![Scripted::text("After resume.")]);
    inbox.send(delivery("again")).unwrap();
    let outcome = run_loop(&mut looped);
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let lines = log::read(&session.dir).unwrap();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            DURABLE_SKILL3,
            STEP,
            DURABLE_NOTE,
            &["preamble_built", "skills_resent", "opening_message"],
            &["turn_started"],
            STEP,
            DURABLE_TEXT,
            ENDED,
        ],
    );
    let all = kinds(&lines);
    let at = all
        .iter()
        .position(|kind| *kind == "handoff_completed")
        .unwrap();
    // The new process rebuilds its preamble before the opening; the resend
    // still immediately precedes the opening.
    let resent_at = at
        + 1
        + all[at + 1..]
            .iter()
            .position(|kind| *kind == "skills_resent")
            .unwrap();
    assert_eq!(all[resent_at + 1], "opening_message");
    assert_eq!(of_kind(&lines, "skills_resent").len(), 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(&requests[0].conversation[..recorded.len()], &recorded[..]);
    assert_eq!(*requests[0].conversation.last().unwrap(), user("again"));
}

#[test]
fn a_crash_after_skills_resent_writes_only_the_opening() {
    let skill = Skill::named();
    let mut session = start(
        vec![
            load_calls(&["alpha", "beta", "alpha"]),
            Scripted::text("The note."),
            Scripted::text("Done."),
            Scripted::text("After resume."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let (outcome, _) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let recorded = session.requests()[2].conversation.clone();

    // Cut the log after the resend: the resume writes only the opening.
    truncate_after(&session, "skills_resent");
    let (mut looped, inbox, provider) =
        resume_truncated(&mut session, &skill, vec![Scripted::text("After resume.")]);
    inbox.send(delivery("again")).unwrap();
    let outcome = run_loop(&mut looped);
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let lines = log::read(&session.dir).unwrap();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            DURABLE_SKILL3,
            STEP,
            DURABLE_NOTE,
            &["skills_resent", "preamble_built", "opening_message"],
            &["turn_started"],
            STEP,
            DURABLE_TEXT,
            ENDED,
        ],
    );
    assert_eq!(of_kind(&lines, "skills_resent").len(), 1);
    let all = kinds(&lines);
    let at = all
        .iter()
        .position(|kind| *kind == "skills_resent")
        .unwrap();
    // The new process rebuilds its preamble before the opening.
    let opening_at = at
        + 1
        + all[at + 1..]
            .iter()
            .position(|kind| *kind == "opening_message")
            .unwrap();
    assert!(
        all[at + 1..opening_at]
            .iter()
            .all(|kind| *kind == "preamble_built")
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    assert_eq!(conversation.len(), 6);
    assert_eq!(&conversation[..recorded.len()], &recorded[..]);
    assert_eq!(*conversation.last().unwrap(), user("again"));
}

#[test]
fn a_provider_run_record_carrying_a_skill_is_not_a_load() {
    let skill = Skill::named();
    let mut session = start(
        vec![
            hosted_skill(),
            Scripted::text("The note."),
            Scripted::text("Done."),
            Scripted::text("After resume."),
        ],
        vec![handoff("c_h", None)],
        &skill,
    );
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            HOSTED_SKILL,
            STEP,
            HANDOFF_NOTE,
            &["opening_message"],
            REPLY,
            ENDED,
        ],
    );
    // The provider-run record never counts: nothing is sent again.
    assert!(of_kind(&lines, "skills_resent").is_empty());
    let live = session.requests()[2].conversation.clone();
    assert!(
        !user_texts(&live)
            .iter()
            .any(|text| text.contains("Alpha body")),
        "{live:?}"
    );
    // A resume renders the same bytes.
    session.resume(Vec::new());
    session.inbox.send(delivery("again")).unwrap();
    let outcome = session.turn();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let requests = session.requests();
    assert_eq!(&requests[3].conversation[..live.len()], &live[..]);
}

/// A reply carrying a hosted web search whose completion names the alpha
/// skill: the provider ran the call, so the record is raw.
fn hosted_skill() -> Scripted {
    let call = contract::events::ToolCallRequested {
        name: "web_search".into(),
        arguments: json!({"query": "rust"}),
        provider_id: Some(ProviderCallId("srv_1".into())),
        repair: None,
        ran_by: None,
        provider_item: Some(json!({"type": "server_tool_use", "id": "srv_1"})),
    };
    let completed = ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text {
            text: "hosted".into(),
        }],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: Some(Control {
            handoff: None,
            questions: None,
            skill: Some(SkillLoad {
                name: "alpha".into(),
                path: "/w/alpha/SKILL.md".into(),
            }),
            name: None,
        }),
        changed_by: None,
        protected_paths: None,
        provider_item: Some(json!({"type": "server_tool_result"})),
        hosts_surfaced: Vec::new(),
        loaded: None,
    };
    let mut end = fakes::reply("");
    end.actions
        .push(ReplyAction::Hosted(HostedCall { call, completed }));
    Scripted {
        deltas: Vec::new(),
        end: Ok(end),
    }
}

/// Rewrites the session log with its lines up to and including the first
/// `kind` line, as a crash there would leave it.
fn truncate_after(session: &Session, kind: &str) {
    let lines = log::read(&session.dir).unwrap();
    let at = lines.iter().position(|line| line.kind == kind).unwrap();
    let mut text = String::new();
    for line in &lines[..=at] {
        text.push_str(&serde_json::to_string(line).unwrap());
        text.push('\n');
    }
    std::fs::write(session.dir.join("events.jsonl"), text).unwrap();
}

/// Resumes the truncated session with a freshly opened log, as a new
/// process would: same home, workspace, credentials, model and tools, the
/// remaining script on a new provider.
fn resume_truncated(
    session: &mut Session,
    skill: &Arc<Skill>,
    script: Vec<Scripted>,
) -> (r#loop::Loop, mpsc::Sender<Delivery>, Arc<ScriptedProvider>) {
    let provider = Arc::new(ScriptedProvider::new(script));
    let clock: Arc<dyn contract::clock::Clock> = session.clock.clone();
    // Release the session's lock: swap in a log on a throwaway directory,
    // so the last Arc of the session's log drops and a new process can
    // open the truncated session directory.
    // Drop the loop first: it holds the session log's other Arc, and the
    // lock only releases once every Arc is gone.
    drop(session.looped.take());
    let throwaway = fakes::TempDir::new("skills-resent-throwaway");
    std::fs::create_dir_all(throwaway.path().join("other")).unwrap();
    std::fs::write(throwaway.path().join("other").join("events.jsonl"), "").unwrap();
    session.log = Arc::new(
        log::Log::open(throwaway.path(), SessionId("other".into()), clock.clone()).unwrap(),
    );
    let log = Arc::new(
        log::Log::open(
            session.dir.parent().unwrap(),
            SessionId("s_test".into()),
            clock.clone(),
        )
        .unwrap(),
    );
    let (inbox, rx) = mpsc::channel();
    let mut prompt = r#loop::PromptInputs::new(
        session.dir.parent().unwrap().to_path_buf(),
        "/bin/sh".into(),
        session.dir.join("events.jsonl").display().to_string(),
        clock.clone(),
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let looped = r#loop::Loop::resume(
        r#loop::Session {
            log,
            provider: provider.clone() as Arc<dyn contract::provider::Provider>,
            model: r#loop::Model {
                reference: support::MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: vec![("skill".into(), skill.clone() as Arc<dyn Tool>)],
            permissions: r#loop::Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: Vec::new(),
                rules: session.rules.clone(),
            },
        },
        r#loop::resumed(&session.dir).unwrap(),
    )
    .unwrap()
    .answerable(false);
    (looped, inbox, provider)
}

fn run_loop(looped: &mut r#loop::Loop) -> Option<TurnOutcome> {
    let (done, finished) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            done.send(looped.turn().unwrap()).unwrap();
        });
        Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("the resumed turn ended before the deadline")
    })
}
