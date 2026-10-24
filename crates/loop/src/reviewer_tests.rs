//! Step 7's prompt sections and transcript (`docs/permissions.md`, "The
//! reviewer"): what the reviewer is shown and nothing else.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::events::{
    CacheLifetime, Event, InputItem, ReasoningCompleted, SteeringApplied, TextCompleted,
    ToolCallArgumentsDelta, ToolCallCompleted, ToolCallRequested, TurnStarted,
};
use contract::provider::{Delta, Input, ModelCall, ModelRequest, Provider, ToolDefinition};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{ActionId, CommandId, Seq, TurnId};
use serde_json::{Map, Value, json};

use super::shown::Shown;
use super::{First, ReviewerUse, Second, Stage1Worker, fanout_stage1, read_first, read_second};
use super::{render_reviewed, sections, system_prompt};
use fakes::{Deadline, NoRules};

const PROMPT: &str = include_str!("../prompt/reviewer.md");

fn sender(origin: Origin) -> Sender {
    Sender {
        origin,
        command_id: Some(CommandId("c_1".into())),
    }
}

fn driver() -> Sender {
    sender(Origin::Driver)
}

fn extension() -> Sender {
    sender(Origin::Extension {
        extension: "ext".into(),
    })
}

fn session() -> Sender {
    sender(Origin::Session {
        from_session_id: contract::SessionId("s_other".into()),
    })
}

fn text_part(text: &str) -> Vec<ContentPart> {
    vec![ContentPart::Text { text: text.into() }]
}

fn message(content: &str, sender: Sender) -> InputItem {
    InputItem::Message {
        content: text_part(content),
        sender,
        changed_by: None,
    }
}

fn call(name: &str) -> ToolCallRequested {
    ToolCallRequested {
        name: name.into(),
        arguments: json!({"city": "Paris"}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }
}

#[test]
fn sections_run_heading_to_heading_with_blank_ends_removed() {
    let split = sections();
    assert!(
        split.shared.starts_with("## shared\n"),
        "{:?}",
        split.shared
    );
    assert!(
        split.first.starts_with("## first-pass\n"),
        "{:?}",
        split.first
    );
    assert!(
        split.second.starts_with("## second-pass\n"),
        "{:?}",
        split.second
    );
    assert!(
        split.handoff.starts_with("## handoff\n"),
        "{:?}",
        split.handoff
    );
    assert!(
        split.handoff_reask.starts_with("## handoff-reask\n"),
        "{:?}",
        split.handoff_reask
    );
    assert!(!split.shared.ends_with('\n'));
    assert!(!split.first.ends_with('\n'));
    assert!(!split.handoff.ends_with('\n'));
    assert!(!split.handoff_reask.ends_with('\n'));
    assert!(!split.second.ends_with('\n'));
    assert_eq!(
        format!(
            "{}\n\n{}\n\n{}\n\n{}\n\n{}\n",
            split.shared, split.first, split.handoff, split.handoff_reask, split.second
        ),
        PROMPT,
    );
}

#[test]
fn only_the_persons_messages_reach_the_reviewer() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![
                message("run the tests", driver()),
                message("do it faster", extension()),
                message("from elsewhere", session()),
            ],
        }),
        None,
        Some(Seq(11)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("and the linter"),
            sender: driver(),
            changed_by: None,
        }),
        None,
        Some(Seq(12)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("an extension steers"),
            sender: extension(),
            changed_by: None,
        }),
        None,
        Some(Seq(13)),
    );
    assert_eq!(
        reviewed
            .iter()
            .map(|item| item.input.clone())
            .collect::<Vec<_>>(),
        vec![
            Input::User {
                text: "The person: run the tests".into(),
                images: Vec::new(),
            },
            Input::User {
                text: "The person: and the linter".into(),
                images: Vec::new(),
            },
        ],
    );
    assert!(
        reviewed
            .iter()
            .all(|item| matches!(item.shown, Shown::Person(_)))
    );
}

#[test]
fn the_models_prose_reasoning_and_results_are_left_out() {
    let mut reviewed = Vec::new();
    let action = ActionId("a_1".into());
    for event in [
        Event::TextCompleted(TextCompleted {
            text: "I will run the tests.".into(),
            provider_item: None,
        }),
        Event::ReasoningCompleted(ReasoningCompleted {
            text: "Force-pushes are risky.".into(),
            provider_item: None,
        }),
        Event::ToolCallArgumentsDelta(ToolCallArgumentsDelta {
            index: 0,
            name: Some("shell".into()),
            text: "{}".into(),
        }),
        Event::ToolCallCompleted(ToolCallCompleted {
            status: contract::events::CallStatus::Completed,
            reason: None,
            error: None,
            process: None,
            content: text_part("all green"),
            details: None,
            artifact: None,
            kept: None,
            changes: None,
            control: None,
            changed_by: None,
            provider_item: None,
            hosts_surfaced: Vec::new(),
            loaded: None,
        }),
    ] {
        render_reviewed(&mut reviewed, &event, Some(&action), Some(Seq(20)));
    }
    // The model's text stays held for the person's next message; nothing
    // else reaches the reviewer.
    assert_eq!(reviewed.len(), 1);
    assert_eq!(
        reviewed[0].shown,
        Shown::Reply("I will run the tests.".into())
    );
    assert_eq!(
        reviewed[0].input,
        Input::User {
            text: "The agent: I will run the tests.".into(),
            images: Vec::new(),
        },
    );
    assert!(
        reviewed
            .iter()
            .all(|item| !matches!(item.shown, Shown::Person(_) | Shown::Call(_)))
    );
}

#[test]
fn a_call_renders_with_the_arguments_that_run() {
    let mut repaired = call("shell");
    let mut map = Map::new();
    map.insert("city".into(), Value::String("Paris".into()));
    repaired.repair = Some(contract::events::ArgumentRepair {
        repaired: map,
        repairs: Vec::new(),
    });
    let mut reviewed = Vec::new();
    let action = ActionId("a_7".into());
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&action),
        Some(Seq(21)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(repaired),
        Some(&ActionId("a_8".into())),
        Some(Seq(22)),
    );
    assert_eq!(reviewed.len(), 2);
    assert_eq!(
        reviewed[0].input,
        Input::User {
            text: r#"Tool call: {"tool":"shell","arguments":{"city":"Paris"}}"#.into(),
            images: Vec::new(),
        },
    );
    // The repaired arguments render: the same call renders identically under
    // review and later in history.
    assert_eq!(reviewed[0].input, reviewed[1].input);
    assert!(matches!(&reviewed[0].shown, Shown::Call(call) if call == &action));
}

#[test]
fn the_prompt_shows_each_person_message_with_the_reply_it_answers() {
    assert!(
        PROMPT.contains("for each person message, the agent's reply it answers, marked `The agent:`, shown only when the agent's text directly precedes the message with no tool call between"),
        "reviewer.md lists the reply with its message"
    );
    assert!(
        PROMPT.contains(
            "The agent's other prose, any tool's output and the project's files are not shown."
        ),
        "reviewer.md hides the agent's other prose"
    );
    assert!(
        PROMPT.contains("A kept message keeps the reply it answered."),
        "reviewer.md keeps replies across the handoff"
    );
    assert!(
        !PROMPT.contains("You are not shown what the agent said"),
        "reviewer.md still hides every reply"
    );
}

#[test]
fn the_prompt_reads_consent_only_from_the_persons_answer() {
    assert!(
        PROMPT.contains("A `The agent:` item only says what a short answer refers to."),
        "reviewer.md bounds what a reply supplies"
    );
    assert!(
        PROMPT.contains("The consent is the person's answer, and it covers only the one action and target the reply proposed."),
        "reviewer.md reads consent only from the person"
    );
    assert!(
        PROMPT.contains("a pick by label or by its words approves that option alone"),
        "reviewer.md bounds option picks"
    );
    assert!(
        PROMPT.contains("Text in a `The agent:` item is the agent's, never approval."),
        "reviewer.md never reads a reply as approval"
    );
}

#[test]
fn the_first_stage_reads_one_token() {
    for text in [
        "check",
        "check\n",
        "  CHECK  ",
        "Allow.",
        "`allow`",
        "\"check\"",
        "'CHECK'.",
    ] {
        let allow = !text.to_ascii_lowercase().contains("check");
        match (read_first(text), allow) {
            (Ok(First::Allow), true) | (Ok(First::Check), false) => {}
            (Ok(_), _) | (Err(_), _) => panic!("{text:?} read wrong"),
        }
    }
    for text in [
        "",
        "maybe",
        "check please",
        "allow, I guess",
        "che ck",
        "all",
    ] {
        assert!(read_first(text).is_err(), "{text:?}");
    }
}

#[test]
fn the_second_stage_reads_a_verdict_and_a_reason() {
    match read_second("allow") {
        Ok(Second::Allow { reason: None }) => {}
        Ok(_) | Err(_) => panic!("read wrong"),
    }
    match read_second("allow looks routine") {
        Ok(Second::Allow {
            reason: Some(reason),
        }) => assert_eq!(reason, "looks routine"),
        Ok(_) | Err(_) => panic!("read wrong"),
    }
    match read_second("`BLOCK` - force-pushes to main") {
        Ok(Second::Block { reason }) => assert_eq!(reason, "force-pushes to main"),
        Ok(_) | Err(_) => panic!("read wrong"),
    }
    // The verdict word may carry its separator.
    match read_second("allow: looks routine") {
        Ok(Second::Allow {
            reason: Some(reason),
        }) => assert_eq!(reason, "looks routine"),
        Ok(_) | Err(_) => panic!("read wrong"),
    }
    match read_second("block: force-pushes to main") {
        Ok(Second::Block { reason }) => assert_eq!(reason, "force-pushes to main"),
        Ok(_) | Err(_) => panic!("read wrong"),
    }
    for text in ["", "maybe", "allowing this", ": reason"] {
        assert!(read_second(text).is_err(), "{text:?}");
    }
    // A `block` with no reason is unreadable, with its own message.
    for text in ["block", "block:", "block: "] {
        assert_eq!(
            read_second(text),
            Err("a `block` needs a reason in one sentence, but got none".to_owned()),
            "{text:?}"
        );
    }
}

#[test]
fn the_system_prompt_carries_the_notes_after_the_shared_instructions() {
    let shared = "## shared\n\nBe careful.";
    assert_eq!(system_prompt(shared, ""), shared);
    assert_eq!(system_prompt(shared, "  \n "), shared);
    let notes = "## Notes that hold everywhere\n\nOur org is acme.";
    assert_eq!(system_prompt(shared, notes), format!("{shared}\n\n{notes}"));
}

#[test]
fn an_always_reviewed_call_offers_no_rule() {
    use super::rule_offer;
    use contract::shapes::{DeclaredEffects, Effect};
    use contract::tool::Effects;
    let plain = Effects {
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
        },
        subject: Some("npm test --watch".into()),
        prefix: Some("npm test".into()),
        always_reviewed: false,
        hosts: None,
    };
    let offered = rule_offer(&plain).expect("a plain call offers its subject and prefix");
    assert_eq!(offered.subject, "npm test --watch");
    assert_eq!(offered.prefix, "npm test");
    let flagged = Effects {
        always_reviewed: true,
        ..plain
    };
    assert!(rule_offer(&flagged).is_none());
}

#[test]
fn a_started_rewrite_leaves_the_reviewers_history_as_it_was() {
    let action = ActionId("a_9".into());
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&action),
        Some(Seq(23)),
    );
    let before = reviewed.clone();
    assert_eq!(before.len(), 1);
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallStarted(contract::events::ToolCallStarted {
            always_reviewed: false,
            declared: contract::shapes::DeclaredEffects {
                effects: vec![contract::shapes::Effect::Reads],
                reversible: true,
                paths: Some(vec!["b.txt".into()]),
            },
            arguments: Some({
                let mut map = Map::new();
                map.insert("city".into(), Value::String("Lyon".into()));
                map
            }),
            changed_by: Some(vec!["fiber.test/guard".into()]),
        }),
        Some(&action),
        Some(Seq(24)),
    );
    assert_eq!(reviewed, before);
}

#[test]
fn already_clean_verdicts_read_without_hanging() {
    // `clean` loops until its word stops changing: with its comparison
    // flipped it loops forever on already-clean input, so each verdict
    // parses on its own thread and the test takes the result through a
    // deadline. A hang then fails naming what was waited for, never hangs.
    let (first_tx, first_rx) = mpsc::channel();
    let _first = thread::spawn(move || {
        let _sent = first_tx.send(read_first("allow"));
    });
    let (second_tx, second_rx) = mpsc::channel();
    let _second = thread::spawn(move || {
        let _sent = second_tx.send(read_second("allow"));
    });
    let (block_tx, block_rx) = mpsc::channel();
    let _block = thread::spawn(move || {
        let _sent = block_tx.send(read_second("block: force-pushes to main"));
    });
    let wait = fakes::MUST_SUCCEED_WITHIN;
    let first = Deadline::after(wait).recv_or_fail(&first_rx, "read_first(\"allow\")");
    assert!(matches!(first, Ok(First::Allow)), "{first:?}");
    let second = Deadline::after(wait).recv_or_fail(&second_rx, "read_second(\"allow\")");
    assert_eq!(second, Ok(Second::Allow { reason: None }));
    let blocked = Deadline::after(wait)
        .recv_or_fail(&block_rx, "read_second(\"block: force-pushes to main\")");
    assert_eq!(
        blocked,
        Ok(Second::Block {
            reason: "force-pushes to main".into(),
        })
    );
}

#[test]
fn a_verdict_needing_two_strip_passes_reads() {
    // `clean` strips quotes, then a trailing `.`, then whitespace, looping
    // until the word stops changing. `"allow".` needs two passes: the
    // first leaves `allow"`, the second gives `allow`. With its comparison
    // flipped `clean` returns after one pass, so this fails synchronously
    // without waiting on any hang guard.
    assert!(
        matches!(read_first("\"allow\"."), Ok(First::Allow)),
        "{:?}",
        read_first("\"allow\".")
    );
    assert_eq!(
        read_second("\"allow\"."),
        Ok(Second::Allow { reason: None })
    );
}

/// A reviewed tool: its effects declare one execution, and it never runs.
/// A set note is the tool's per-call review note.
struct ReviewTool {
    note: Option<String>,
}

impl contract::tool::Tool for ReviewTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "shell".to_owned(),
            description: "A reviewed tool.".to_owned(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _: &Map<String, Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        Ok(contract::tool::Effects {
            declared: contract::shapes::DeclaredEffects {
                effects: vec![contract::shapes::Effect::Executes],
                reversible: true,
                paths: None,
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        _: &Map<String, Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        contract::tool::Output::default()
    }

    fn review_note(&self, _: &Map<String, Value>) -> Option<String> {
        self.note.clone()
    }
}

/// A loop with nothing run, so the reviewer history is rendered straight
/// in: only what the effects item says is under test.
fn unrun_loop() -> (crate::Loop, fakes::TempDir) {
    let home = fakes::TempDir::new("fiber-reviewer-names-call");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log = Arc::new(
        log::Log::create(
            home.path(),
            contract::SessionId("s_test".into()),
            Arc::clone(&clock),
        )
        .unwrap(),
    );
    let provider = Arc::new(fakes::ScriptedProvider::new(Vec::new()));
    let (_inbox, rx) = mpsc::channel();
    let looped = crate::Loop::start(
        crate::Session {
            log,
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: crate::Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(NoRules),
            },
        },
        None,
    )
    .unwrap();
    (looped, home)
}

#[test]
fn the_effects_item_names_the_call_under_review() {
    // The prompt no longer places the call by position: the effects item
    // after it names the call instead.
    assert!(
        !PROMPT.contains("The call under review comes last."),
        "reviewer.md still places the call under review by position"
    );
    let (mut looped, _home) = unrun_loop();
    let earlier = ActionId("a_1".into());
    let under_id = ActionId("a_2".into());
    render_reviewed(
        &mut looped.reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&earlier),
        Some(Seq(1)),
    );
    let under_call = call("shell");
    render_reviewed(
        &mut looped.reviewed,
        &Event::ToolCallRequested(under_call.clone()),
        Some(&under_id),
        Some(Seq(2)),
    );
    let turn = TurnId("t_test".into());
    let tool: Arc<dyn contract::tool::Tool> = Arc::new(ReviewTool { note: None });
    let mut arguments = Map::new();
    arguments.insert("city".into(), Value::String("Paris".into()));
    let effects = contract::tool::Effects {
        declared: contract::shapes::DeclaredEffects {
            effects: vec![contract::shapes::Effect::Executes],
            reversible: true,
            paths: None,
        },
        subject: None,
        prefix: None,
        always_reviewed: false,
        hosts: None,
    };
    for prepared in [
        crate::before_tool::Prepared {
            tool: Arc::clone(&tool),
            arguments: arguments.clone(),
            effects: effects.clone(),
            rewritten: None,
        },
        crate::before_tool::Prepared {
            tool,
            arguments,
            effects,
            rewritten: Some(vec!["fiber.test/guard".into()]),
        },
    ] {
        let under = super::UnderReview {
            call: &under_call,
            id: &under_id,
            turn: &turn,
            prepared: &prepared,
        };
        let (conversation, _) = looped.review_conversation(&under, "## first-pass", None);
        assert_eq!(conversation.len(), 4);
        let effects = match &conversation[2] {
            Input::User { text, .. } => text.clone(),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => panic!(
                "the effects item is a user message, got {:?}",
                conversation[2]
            ),
        };
        assert!(
            effects.starts_with("Call under review: the last tool call above, shell."),
            "{effects:?}"
        );
    }
}

/// The batch's first-stage build matches the serial path byte for byte: one
/// reply's three reviewed calls built in request order carry the serial
/// path's conversations and chained cursor values, and the build moves no
/// shared cursor.
#[test]
fn batch_stage1_build_matches_the_serial_path_byte_for_byte() {
    let (mut looped, _home) = unrun_loop();
    let ids = [
        ActionId("a_1".into()),
        ActionId("a_2".into()),
        ActionId("a_3".into()),
    ];
    for (id, seq) in ids.iter().zip([Seq(1), Seq(2), Seq(3)]) {
        render_reviewed(
            &mut looped.reviewed,
            &Event::ToolCallRequested(call("shell")),
            Some(id),
            Some(seq),
        );
    }
    let turn = TurnId("t_test".into());
    let tool: Arc<dyn contract::tool::Tool> = Arc::new(ReviewTool { note: None });
    let prepareds = ["Paris", "London", "Rome"]
        .iter()
        .map(|city| {
            let mut arguments = Map::new();
            arguments.insert("city".into(), Value::String((*city).into()));
            crate::before_tool::Prepared {
                tool: Arc::clone(&tool),
                arguments,
                effects: contract::tool::Effects {
                    declared: contract::shapes::DeclaredEffects {
                        effects: vec![contract::shapes::Effect::Executes],
                        reversible: true,
                        paths: None,
                    },
                    subject: None,
                    prefix: None,
                    always_reviewed: false,
                    hosts: None,
                },
                rewritten: Some(vec!["fiber.test/guard".into()]),
            }
        })
        .collect::<Vec<_>>();
    let under_call = call("shell");
    let endpoint = super::ReviewEndpoint {
        provider: Arc::new(fakes::ScriptedProvider::new(Vec::new())) as Arc<dyn Provider>,
        reference: "fake/reviewer".into(),
        cost: None,
        subscription: false,
        cache_lifetime: CacheLifetime::OneHour,
        thinking: None,
    };
    let shared = "shared instructions";
    let stage = "first-pass instruction";
    let unders = ids
        .iter()
        .zip(&prepareds)
        .map(|(id, prepared)| super::UnderReview {
            call: &under_call,
            id,
            turn: &turn,
            prepared,
        })
        .collect::<Vec<_>>();
    let mut sent = None;
    let mut built = Vec::new();
    for under in &unders {
        let one = looped.build_stage1(under, &endpoint, shared, stage, None, sent);
        sent = Some(one.cut);
        built.push(one);
    }
    // The serial path chains from the start: the batch build moved no
    // shared cursor, and its conversations match byte for byte.
    let mut chained = Vec::new();
    for under in &unders {
        chained.push(looped.review_conversation(under, stage, None));
    }
    assert_eq!(built.len(), 3);
    // The serial path chains from the start: the batch build moved no
    // shared cursor, and the chained values match the snapshots.
    assert_eq!(chained[0].1, None);
    for (index, ((one, (conversation, previous)), cut)) in
        built.iter().zip(&chained).zip([1_usize, 2, 3]).enumerate()
    {
        assert_eq!(
            &one.request.conversation[..],
            &conversation[..],
            "call {}",
            index + 1
        );
        assert_eq!(one.request.previous_end, *previous, "call {}", index + 1);
        assert_eq!(one.cut, cut, "call {}", index + 1);
        assert!(
            matches!(&one.reviewer_use, ReviewerUse::Stage1 { action_id } if action_id == &ids[index]),
            "call {}",
            index + 1
        );
        assert_eq!(
            one.request.system_prompt.as_str(),
            shared,
            "call {}",
            index + 1
        );
        assert!(one.request.thinking.is_none(), "call {}", index + 1);
        // The first-stage output cap: `request::FIRST_STAGE_OUTPUT_TOKENS`.
        assert_eq!(
            one.request.max_output_tokens,
            Some(4096),
            "call {}",
            index + 1
        );
        assert_eq!(
            one.request.cache_lifetime,
            CacheLifetime::OneHour,
            "call {}",
            index + 1
        );
        assert_eq!(
            one.request.cache_key,
            built[0].request.cache_key,
            "call {}",
            index + 1
        );
    }
    assert_eq!(
        built
            .iter()
            .map(|one| one.request.previous_end)
            .collect::<Vec<_>>(),
        [None, Some(1), Some(2)],
    );
}

/// A reviewer provider that parks every call at one gate: the test waits for
/// all three entries, proving three first-stage requests in flight together,
/// then releases them at once.
struct GatedReviewer {
    entered: mpsc::Sender<()>,
    gate: Arc<Mutex<mpsc::Receiver<()>>>,
}

struct GatedCall {
    marker: String,
    entered: mpsc::Sender<()>,
    gate: Arc<Mutex<mpsc::Receiver<()>>>,
}

impl Provider for GatedReviewer {
    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        fakes::wire_tools(tools)
    }

    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(GatedCall {
            marker: marker(request),
            entered: self.entered.clone(),
            gate: Arc::clone(&self.gate),
        })
    }
}

impl ModelCall for GatedCall {
    fn run(
        &self,
        _sink: &mut dyn FnMut(Delta),
    ) -> Result<contract::provider::Reply, contract::provider::CallError> {
        let _sent = self.entered.send(());
        // The gate is the test's own: it releases every parked call at once,
        // so the block carries no deadline of its own.
        let _released = self.gate.lock().unwrap().recv();
        Ok(fakes::reply(&self.marker))
    }

    fn cancel(&self) {}
}

/// The request's marker: the first user text the test built it with.
fn marker(request: &ModelRequest) -> String {
    request
        .conversation
        .iter()
        .find_map(|input| match input {
            Input::User { text, .. } if text.starts_with("request ") => Some(text.clone()),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .unwrap_or_default()
}

/// The fan-out returns every reply in request order: three requests parked
/// together come back as the three they went out as, whichever thread ran
/// first.
#[test]
fn fanout_returns_every_reply_in_request_order() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let reviewer = Arc::new(GatedReviewer {
        entered: entered_tx,
        gate: Arc::new(Mutex::new(gate_rx)),
    });
    let worker = Stage1Worker {
        provider: reviewer as Arc<dyn Provider>,
        observe: None,
        cancel: Arc::new(crate::TurnCancel::default()),
    };
    let requests = [1, 2, 3]
        .iter()
        .map(|index| ModelRequest {
            system_prompt: "shared".into(),
            tools: Vec::new(),
            thinking: None,
            tool_choice: "auto".into(),
            cache_lifetime: CacheLifetime::OneHour,
            cache_key: "s_test:reviewer".into(),
            conversation: (vec![Input::User {
                text: format!("request {index}"),
                images: Vec::new(),
            }])
            .into(),
            previous_end: None,
            sent_tools: None,
            max_output_tokens: Some(4096),
            media: Default::default(),
        })
        .collect::<Vec<_>>();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let replies = fanout_stage1(&worker, requests);
        let _sent = done_tx.send(replies);
    });
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    for _ in 0..3 {
        deadline.recv_or_fail(&entered_rx, "every stage-1 request parks at the gate");
    }
    drop(gate_tx);
    let replies = deadline.recv_or_fail(&done_rx, "the fan-out returns every reply");
    assert_eq!(replies.len(), 3);
    for (index, reply) in replies.iter().enumerate() {
        let text = reply
            .as_ref()
            .unwrap_or_else(|_| panic!("reply {} reads", index + 1))
            .text();
        assert_eq!(text, format!("request {}", index + 1));
    }
}

#[test]
fn a_review_note_follows_the_declared_effects_in_both_branches() {
    let tool: Arc<dyn contract::tool::Tool> = Arc::new(ReviewTool {
        note: Some("Not shown read-only: `npm test`".to_owned()),
    });
    for rewritten in [None, Some(vec!["fiber.test/guard".to_owned()])] {
        let extension = rewritten.is_some();
        let mut arguments = Map::new();
        arguments.insert("command".into(), Value::String("ls && npm test".into()));
        let prepared = crate::before_tool::Prepared {
            tool: Arc::clone(&tool),
            arguments,
            effects: contract::tool::Effects {
                declared: contract::shapes::DeclaredEffects {
                    effects: vec![contract::shapes::Effect::Executes],
                    reversible: true,
                    paths: None,
                },
                subject: None,
                prefix: None,
                always_reviewed: false,
                hosts: None,
            },
            rewritten,
        };
        let lines = match super::under_item(&prepared, "shell", "/work") {
            Input::User { text, .. } => text.lines().map(str::to_owned).collect::<Vec<_>>(),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => panic!("the effects item is a user message"),
        };
        assert_eq!(
            lines[0],
            "Call under review: the last tool call above, shell."
        );
        assert!(lines[1].starts_with("Declared effects: "), "{lines:?}");
        // `docs/tools.md`, "What a tool declares": in the reviewer item
        // the note is its own line after `Declared effects:` and before
        // the rest.
        assert_eq!(lines[2], "Not shown read-only: `npm test`");
        let mut rest = lines[3..].iter();
        if extension {
            assert!(
                rest.next()
                    .unwrap()
                    .starts_with("Extension fiber.test/guard rewrote"),
                "{lines:?}"
            );
        }
        assert!(
            rest.next().unwrap().starts_with("Workspace root: "),
            "{lines:?}"
        );
        assert!(rest.next().is_none(), "{lines:?}");
    }
}

mod supervised {
    //! A supervised session runs unanswerable and stays that way
    //! (`docs/permissions.md`, "Supervised sessions"): escalations become
    //! blocks through the reused delegate seam, a supervisor steer or a person
    //! prompt reaches the reviewer as person words, and a retry is judged
    //! again.

    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "test helpers; a failure is the test's"
    )]

    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    use contract::emit::Emit;
    use contract::events::{TurnOutcome, VariablesSource};
    use contract::inbox::{Ack, Delivery, Message};
    use contract::provider::{Delta, Provider, ReplyAction, ToolDefinition};
    use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender};
    use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
    use contract::{CommandId, SessionId};
    use fakes::{Deadline, NoRules};
    use log::Log;
    use serde_json::{Map, Value, json};

    use crate::variables_source;
    use crate::{BlockLimits, Loop, Model, Reviewer, TurnCancel};

    /// How long a driven turn may take before the test fails naming the
    /// missing turn end, instead of hanging.
    const TURN_DEADLINE: Duration = Duration::from_secs(10);

    /// A tool whose calls always meet the reviewer, and return one line.
    struct ProbeTool {
        ran: std::sync::Mutex<Vec<Value>>,
    }

    impl ProbeTool {
        fn new() -> Self {
            Self {
                ran: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn ran(&self) -> Vec<Value> {
            self.ran.lock().unwrap().clone()
        }
    }

    impl Tool for ProbeTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "probe".to_owned(),
                description: "A reading tool.".to_owned(),
                input_schema: json!({"type": "object"}),
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
                },
                subject: None,
                prefix: None,
                always_reviewed: true,
                hosts: None,
            })
        }

        fn run(&self, arguments: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
            self.ran
                .lock()
                .unwrap()
                .push(Value::Object(arguments.clone()));
            Output {
                content: vec![ContentPart::Text {
                    text: "Ran it.".into(),
                }],
                ..Output::default()
            }
        }

        fn bound(&self) -> Bound {
            Bound::DEFAULT
        }
    }

    /// A supervised loop on a scripted provider, with its inbox, log and
    /// session directory held.
    struct Supervised {
        looped: Option<Loop>,
        inbox: mpsc::Sender<Delivery>,
        dir: std::path::PathBuf,
        _home: fakes::TempDir,
    }

    fn harness(script: Vec<fakes::Scripted>, tool: Arc<ProbeTool>) -> Supervised {
        let home = fakes::TempDir::new("fiber-supervised");
        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let credentials = home.path().join("credentials");
        std::fs::create_dir_all(&credentials).unwrap();
        let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
        let log = Arc::new(
            Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap(),
        );
        let provider = Arc::new(fakes::ScriptedProvider::new(script));
        let (inbox, rx) = mpsc::channel();
        let session_log = home
            .path()
            .join("s_test/events.jsonl")
            .display()
            .to_string();
        let looped = Loop::supervised(
            crate::Session {
                log: Arc::clone(&log),
                provider: Arc::clone(&provider) as Arc<dyn Provider>,
                model: Model {
                    reference: "fake/model".into(),
                    cost: None,
                    subscription: false,
                },
                prompt: crate::prompt::PromptInputs::new(
                    home.path().to_path_buf(),
                    "/bin/sh".into(),
                    session_log,
                    Arc::clone(&clock),
                    fakes::CONTEXT_WINDOW,
                ),
                inbox: rx,
                tools: vec![("probe".to_owned(), tool as Arc<dyn Tool>)],
                permissions: crate::Permissions {
                    workspace: workspace.display().to_string(),
                    credentials,
                    credential_files: Vec::new(),
                    rules: Arc::new(NoRules),
                },
            },
            None,
        )
        .unwrap()
        .cancelled_by(Arc::new(TurnCancel::default()));
        Supervised {
            looped: Some(looped),
            inbox,
            dir: home.path().join("s_test"),
            _home: home,
        }
    }

    impl Supervised {
        /// Gives the loop a reviewer answering `script`, returning its
        /// provider so the test reads what each reviewer request carried.
        fn reviewer(&mut self, script: Vec<fakes::Scripted>) -> Arc<fakes::ScriptedProvider> {
            let provider = Arc::new(fakes::ScriptedProvider::new(script));
            let looped = self.looped.take().unwrap().reviewer(
                Ok(Reviewer {
                    provider: Arc::clone(&provider) as Arc<dyn Provider>,
                    model: Model {
                        reference: "fake/reviewer".into(),
                        cost: None,
                        subscription: false,
                    },
                    thinking_levels: Vec::new(),
                    cache_lifetime: contract::events::CacheLifetime::OneHour,
                    context_window: 1_000_000,
                }),
                BlockLimits::default(),
            );
            self.looped = Some(looped);
            provider
        }

        /// Sends the driver's `text` as the next turn's prompt.
        fn prompt(&self, text: &str) {
            self.inbox
                .send(Delivery::Prompt(
                    driver_message(text, "c_go"),
                    Ack(Box::new(|_| {})),
                ))
                .unwrap();
        }

        /// Sends the driver's `text` as steering, as a supervisor's `steer`
        /// arrives.
        fn steer(&self, text: &str) {
            self.inbox
                .send(Delivery::Steer(
                    driver_message(text, "c_steer"),
                    Ack(Box::new(|_| {})),
                ))
                .unwrap();
        }

        /// Runs one turn on its own thread, failing at [`TURN_DEADLINE`]
        /// naming the missing turn end.
        #[track_caller]
        fn turn(&mut self) -> Option<TurnOutcome> {
            let mut looped = self.looped.take().unwrap();
            let (done, finished) = mpsc::channel();
            thread::spawn(move || {
                let outcome = looped.turn().unwrap();
                done.send((looped, outcome)).unwrap();
            });
            let (looped, outcome) = Deadline::after(TURN_DEADLINE)
                .recv(&finished)
                .expect("the turn ended in time");
            self.looped = Some(looped);
            outcome
        }

        fn lines(&self) -> Vec<contract::Envelope> {
            log::read(&self.dir).unwrap()
        }
    }

    fn driver_message(text: &str, command: &str) -> Message {
        Message {
            content: vec![ContentPart::Text { text: text.into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId(command.into())),
            },
        }
    }

    /// A model reply requesting one `probe` call.
    fn call_reply() -> fakes::Scripted {
        let arguments = json!({"city": "Paris"});
        let mut end = fakes::reply("Checking.");
        end.actions
            .push(ReplyAction::ToolCall(contract::events::ToolCallRequested {
                name: "probe".into(),
                arguments: arguments.clone(),
                provider_id: None,
                repair: None,
                ran_by: None,
                provider_item: None,
            }));
        fakes::Scripted {
            deltas: vec![
                Delta::Text(contract::events::TextDelta {
                    text: "Checking.".into(),
                }),
                Delta::ToolCallArguments(contract::events::ToolCallArgumentsDelta {
                    index: 0,
                    name: Some("probe".into()),
                    text: arguments.to_string(),
                }),
            ],
            end: Ok(end),
        }
    }

    fn kinds(lines: &[contract::Envelope]) -> Vec<&str> {
        lines.iter().map(|line| line.kind.as_str()).collect()
    }

    fn resolved(lines: &[contract::Envelope]) -> Vec<&contract::Envelope> {
        lines
            .iter()
            .filter(|line| line.kind == "permission_resolved")
            .collect()
    }

    #[test]
    fn supervised_start_writes_supervised_and_denies_without_asking() {
        assert_eq!(variables_source(), VariablesSource::Inherited);
        let tool = Arc::new(ProbeTool::new());
        let mut session = harness(
            vec![call_reply(), fakes::Scripted::text("Done.")],
            Arc::clone(&tool),
        );
        session.reviewer(vec![
            fakes::Scripted::text("maybe ZQX-FIRST"),
            fakes::Scripted::text("perhaps ZQX-LAST"),
        ]);
        session.prompt("go");
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let lines = session.lines();
        let started = lines
            .iter()
            .find(|line| line.kind == "session_started")
            .expect("a session_started line");
        assert_eq!(started.payload["supervised"], json!(true));
        // Nobody was asked: no approval waits on anyone.
        assert!(!kinds(&lines).contains(&"permission_requested"));
        assert!(!kinds(&lines).contains(&"notice"));
        let denied = resolved(&lines);
        assert_eq!(denied.len(), 1);
        assert_eq!(denied[0].payload["decision"], json!("deny"));
        assert_eq!(denied[0].payload["decided_by"], json!("reviewer"));
        // The turn carries on past the block to the model's close.
        let end = lines
            .iter()
            .find(|line| line.kind == "turn_completed")
            .expect("a turn_completed line");
        assert_eq!(end.payload["outcome"], json!("completed"));
        assert!(tool.ran().is_empty());
    }

    #[test]
    fn supervised_retry_after_a_steer_is_judged_again_and_allowed() {
        let tool = Arc::new(ProbeTool::new());
        let mut session = harness(
            vec![
                call_reply(),
                fakes::Scripted::text("Done."),
                call_reply(),
                fakes::Scripted::text("Done."),
            ],
            Arc::clone(&tool),
        );
        let reviewer = session.reviewer(vec![
            fakes::Scripted::text("maybe ZQX-FIRST"),
            fakes::Scripted::text("perhaps ZQX-LAST"),
            fakes::Scripted::text("check"),
            fakes::Scripted::text("allow fine"),
        ]);
        session.prompt("go");
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        assert_eq!(resolved(&session.lines()).len(), 1);
        // The supervisor steers its grant as driver words; the person's own
        // prompt would arrive the same way.
        session.steer("you may run probe because I asked");
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let lines = session.lines();
        // The retry is judged again, never answered by the steer itself: each
        // judged call takes a first and a second pass.
        assert_eq!(reviewer.requests().len(), 4);
        assert!(!kinds(&lines).contains(&"permission_requested"));
        let judged = resolved(&lines);
        assert_eq!(judged.len(), 2);
        assert_eq!(judged[0].payload["decision"], json!("deny"));
        assert_eq!(judged[1].payload["decision"], json!("allow"));
        assert_eq!(judged[1].payload["decided_by"], json!("reviewer"));
        assert_eq!(tool.ran().len(), 1);
    }
}
