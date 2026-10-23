//! The media a request carries (`docs/model-routing.md`, "Image limits"):
//! the loop reads each stored image and PDF through `log`, encodes it once
//! per session and hands the provider a map from the conversation's path to
//! the base64, through the loop's public API.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::fs;
use std::sync::Arc;

use contract::CommandId;
use contract::events::TurnOutcome;
use contract::inbox::{Delivery, Message};
use contract::provider::Media;
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::{ContentPart, ImagePart, Origin, PdfPart, Sender};
use contract::tool::Tool;
use fakes::Scripted;

use support::{Session, TestTool, calls_reply, delivery, ignore, paris, shell};

/// A prompt with `text` and the stored image at `path`.
fn image_prompt(text: &str, path: &str) -> Delivery {
    Delivery::Prompt(
        Message {
            content: vec![
                ContentPart::Text { text: text.into() },
                ContentPart::Image {
                    path: path.into(),
                    mime_type: "image/png".into(),
                    width: 1,
                    height: 1,
                },
            ],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_one".into())),
            },
        },
        ignore(),
    )
}

fn write_artifact(session: &Session, name: &str, bytes: &[u8]) {
    fs::create_dir_all(session.dir.join("artifacts")).unwrap();
    fs::write(session.dir.join(name), bytes).unwrap();
}

/// The base64 the map holds for `path`. No doc promises the map's order or
/// size, so a test looks each path up.
fn data<'a>(media: &'a Media, path: &str) -> Option<&'a str> {
    media.get(path).map(|data| &**data)
}

#[test]
fn a_prompts_image_reaches_the_first_request_as_its_base64() {
    let mut session = Session::with_tools(vec![Scripted::text("seen")], None, Vec::new());
    // "abcd" is `YWJjZA==`.
    write_artifact(&session, "artifacts/i.png", b"abcd");
    session
        .inbox
        .send(image_prompt("look", "artifacts/i.png"))
        .unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));

    // A reply with no tool call ends the turn after its one step, so the
    // turn made one request (`docs/loop.md`, "One step", steps 3 and 4).
    let requests = session.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        data(&requests[0].media, "artifacts/i.png"),
        Some("YWJjZA==")
    );
}

#[test]
fn an_image_is_encoded_once_per_session_across_turns() {
    let mut session = Session::with_tools(
        vec![Scripted::text("seen"), Scripted::text("again")],
        None,
        Vec::new(),
    );
    write_artifact(&session, "artifacts/i.png", b"abcd");
    session
        .inbox
        .send(image_prompt("look", "artifacts/i.png"))
        .unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    // The file changes on disk; the session keeps sending what it first sent.
    write_artifact(&session, "artifacts/i.png", b"zzzz");
    session.inbox.send(delivery("and now")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));

    // Each turn is one step, one request (`docs/loop.md`, "One step"), in the
    // order the turns ran.
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    let first = &requests[0].media["artifacts/i.png"];
    let second = &requests[1].media["artifacts/i.png"];
    assert_eq!(&**second, "YWJjZA==");
    assert!(Arc::ptr_eq(first, second));
}

#[test]
fn a_missing_image_leaves_the_map_empty_and_the_request_goes_out() {
    let mut session = Session::with_tools(vec![Scripted::text("ok")], None, Vec::new());
    session
        .inbox
        .send(image_prompt("look", "artifacts/gone.png"))
        .unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));

    // One step, one request (`docs/loop.md`, "One step").
    let requests = session.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].media.is_empty());
}

fn allow_rule(tool: &str) -> Rule {
    Rule {
        decision: RuleDecision::Allow,
        tool: tool.into(),
        prefix: String::new(),
        added: None,
        session_id: None,
    }
}

#[test]
fn a_tool_results_pdf_and_both_pages_reach_the_request() {
    let mut tool = TestTool::reads("cat", "PDF: 2 pages.\n");
    let page = |path: &str| ImagePart {
        path: path.into(),
        mime_type: "image/png".into(),
        width: 1545,
        height: 2000,
    };
    tool.output.content.push(ContentPart::Pdf(
        PdfPart::new(
            "artifacts/p.pdf".into(),
            2,
            Some(vec![page("artifacts/p1.png"), page("artifacts/p2.png")]),
        )
        .unwrap(),
    ));
    let mut session = Session::with_tools(
        vec![
            calls_reply("working", &[("cat", paris())]),
            Scripted::text("done"),
        ],
        None,
        vec![Arc::new(tool) as Arc<dyn Tool>],
    );
    write_artifact(&session, "artifacts/p.pdf", b"abc");
    write_artifact(&session, "artifacts/p1.png", b"ab");
    write_artifact(&session, "artifacts/p2.png", b"abcd");
    session.rules.set(StandingRules {
        global: vec![allow_rule("cat")],
        project: Vec::new(),
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));

    // The tool call's step is the first request; the step after its result
    // is the second (`docs/loop.md`, "One step", steps 4 to 7). The result is
    // not in the first.
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].media.is_empty());
    let media = &requests[1].media;
    assert_eq!(data(media, "artifacts/p.pdf"), Some("YWJj"));
    assert_eq!(data(media, "artifacts/p1.png"), Some("YWI="));
    assert_eq!(data(media, "artifacts/p2.png"), Some("YWJjZA=="));
}

#[test]
fn a_reviewer_request_carries_no_media() {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer(vec![Scripted::text("allow")]);
    write_artifact(&session, "artifacts/i.png", b"abcd");
    session
        .inbox
        .send(image_prompt("run it", "artifacts/i.png"))
        .unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));

    let requests = reviewer.requests();
    assert!(!requests.is_empty());
    assert!(requests.iter().all(|request| request.media.is_empty()));
    // The session's own request did carry the image.
    assert!(
        session
            .requests()
            .iter()
            .any(|request| data(&request.media, "artifacts/i.png").is_some())
    );
}
