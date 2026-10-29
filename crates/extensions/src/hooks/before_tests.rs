//! A session's `before_tool` and `check_tool` chains (`docs/extensions.md`,
//! "The hook points", "When several hooks share a point" and "When a hook
//! fails"): what each hook is shown, and what its answer decides.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;

use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::events::CallStatus;
use contract::hook::{
    AfterToolAnswer, AfterToolCall, AfterToolOutcome, BeforeMessage, BeforeMessageAnswer,
    BeforeMessageOutcome, BeforeToolAnswer, BeforeToolCall, BeforeToolOutcome, CheckAnswer,
    CheckOutcome, Context, Hooks,
};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, PdfPart};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, Value, json};

use super::super::SessionExtensions;
use crate::{Origin, Request, plan};
use fakes::FakeLock;

struct Home {
    root: fakes::TempDir,
}

impl Home {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-before-hooks");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs `fiber.test/<short>`, whose entry script is `init`. No
    /// fixture registers a provider, so the manifest declares hooks alone.
    fn install(&self, short: &str, init: &str) {
        let src = self.root.path().join("src").join(short);
        fs::create_dir_all(&src).unwrap();
        let manifest = json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1});
        fs::write(src.join("extension.json"), manifest.to_string()).unwrap();
        fs::write(src.join("init.lua"), init).unwrap();
        plan(
            &self.home(),
            &Request::Path(src),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    fn config(&self, overrides: &[&str]) -> Config {
        Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap()
    }

    /// The session's extensions, loaded on a thread under [`MUST_SUCCEED_WITHIN`]: the
    /// fake clock never ends a wait the runtime does not end itself.
    #[track_caller]
    fn load(&self, overrides: &[&str]) -> Arc<SessionExtensions> {
        let config = self.config(overrides);
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        Arc::new(bounded(move || {
            SessionExtensions::load(&home, &config, FakeClock::new(), locks, None)
        }))
    }
}

/// A `before_tool` hook of `phase` whose `run` is `body`.
fn tool_hook(on_failure: &str, phase: &str, body: &str) -> String {
    format!(
        "fiber.hook(\"before_tool\", {{ phase = \"{phase}\", timeout = 1000, on_failure = \"{on_failure}\",\n\
           run = function(call) {body} end }})\n"
    )
}

/// An `after_tool` hook that appends `|<tag>` to the content.
fn tagging(tag: &str) -> String {
    format!(
        "fiber.hook(\"after_tool\", {{ timeout = 1000, on_failure = \"non-blocking\",\n\
           run = function(call) return {{ content = call.content .. \"|{tag}\" }} end }})\n"
    )
}

/// Runs `f` on its own thread and waits for it under [`MUST_SUCCEED_WITHIN`], so a runtime
/// that never answers fails the test instead of hanging it.
#[track_caller]
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(f());
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the extensions")
}

fn arguments(pairs: &[(&str, &str)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::String((*value).to_owned())))
        .collect()
}

fn declared(effects: Vec<Effect>, paths: Option<Vec<&str>>, reversible: bool) -> DeclaredEffects {
    DeclaredEffects {
        effects,
        reversible,
        paths: paths.map(|paths| paths.iter().map(|path| (*path).to_owned()).collect()),
        confined: false,
    }
}

#[track_caller]
fn before_tool(
    session: &Arc<SessionExtensions>,
    args: Map<String, Value>,
    declared: DeclaredEffects,
) -> BeforeToolAnswer {
    let (session, declared) = (Arc::clone(session), declared);
    bounded(move || {
        session.before_tool(&BeforeToolCall {
            tool: "write",
            arguments: &args,
            declared: &declared,
        })
    })
}

#[track_caller]
fn check_tool(
    session: &Arc<SessionExtensions>,
    args: Map<String, Value>,
    declared: DeclaredEffects,
) -> CheckAnswer {
    let (session, declared) = (Arc::clone(session), declared);
    bounded(move || {
        session.check_tool(&BeforeToolCall {
            tool: "write",
            arguments: &args,
            declared: &declared,
        })
    })
}

#[track_caller]
fn after_tool(session: &Arc<SessionExtensions>, content: &str) -> AfterToolAnswer {
    let (session, content) = (Arc::clone(session), content.to_owned());
    bounded(move || {
        let arguments = Map::new();
        session.after_tool(&AfterToolCall {
            tool: "read",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: &content,
            details: None,
            process: None,
            spilled: None,

            not_regular: false,
            delivery: None,
        })
    })
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| format!("fiber.test/{n}")).collect()
}

/// A `before_message` hook whose `run` is `body`.
fn message_hook(on_failure: &str, phase: &str, body: &str) -> String {
    format!(
        "fiber.hook(\"before_message\", {{ phase = \"{phase}\", timeout = 1000, on_failure = \"{on_failure}\",\n\
           run = function(call) {body} end }})\n"
    )
}

/// The parts a message holds: text, an image, a pdf, and one part this
/// build does not know.
fn message_parts() -> Vec<ContentPart> {
    vec![
        ContentPart::Text { text: "hi".into() },
        ContentPart::Image {
            path: "artifacts/i.png".into(),
            mime_type: "image/png".into(),
            width: 4,
            height: 4,
        },
        ContentPart::Pdf(PdfPart::new("artifacts/p.pdf".into(), 2, None).unwrap()),
        ContentPart::Unknown,
    ]
}

fn image_part() -> ContentPart {
    ContentPart::Image {
        path: "artifacts/i.png".into(),
        mime_type: "image/png".into(),
        width: 4,
        height: 4,
    }
}

#[track_caller]
fn before_message(
    session: &Arc<SessionExtensions>,
    content: Vec<ContentPart>,
) -> BeforeMessageAnswer {
    let session = Arc::clone(session);
    bounded(move || {
        session.before_message(&BeforeMessage {
            content: &content,
            sender: None,
        })
    })
}

fn passed(answer: &BeforeMessageAnswer) -> (Option<Vec<ContentPart>>, Vec<Context>) {
    let BeforeMessageOutcome::Passed { replaced, context } = &answer.outcome else {
        panic!("the message passes: {:?}", answer.outcome);
    };
    (
        replaced.as_ref().map(|replaced| replaced.content.clone()),
        context.clone(),
    )
}

#[test]
fn hooks_order_before_message_reorders_only_that_point() {
    fn appending(tag: &str) -> String {
        message_hook(
            "non-blocking",
            "transform",
            &format!(
                "local out = {{}} for _, part in ipairs(call.content) do out[#out + 1] = part end \
                 out[#out + 1] = {{ type = \"text\", text = \"|{tag}\" }} return {{ content = out }}"
            ),
        )
    }
    fn rewriting(tag: &str) -> String {
        tool_hook(
            "non-blocking",
            "transform",
            &format!("return {{ arguments = {{ path = call.arguments.path .. \"|{tag}\" }} }}"),
        )
    }
    let home = Home::new();
    home.install("a", &(appending("a") + &rewriting("a")));
    home.install("b", &(appending("b") + &rewriting("b")));
    let session = home.load(&[r#"hooks.order.before_message=["fiber.test/b"]"#]);
    let answer = before_message(&session, vec![ContentPart::Text { text: "hi".into() }]);
    let (replaced, _) = passed(&answer);
    assert_eq!(
        replaced,
        Some(vec![
            ContentPart::Text { text: "hi".into() },
            ContentPart::Text { text: "|b".into() },
            ContentPart::Text { text: "|a".into() },
        ])
    );
    // The `before_tool` order is untouched by the `before_message` order.
    let answer = before_tool(&session, arguments(&[("path", "x")]), reads());
    let BeforeToolOutcome::Rewritten { changed_by, .. } = answer.outcome else {
        panic!("the hooks rewrite: {:?}", answer.outcome);
    };
    assert_eq!(changed_by, names(&["a", "b"]));
}

#[test]
fn a_replacement_is_seen_by_the_next_hook_and_a_context_only_hook_is_unnamed() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook(
            "non-blocking",
            "transform",
            "return { content = {{ type = \"text\", text = \"hi\" }} }",
        ),
    );
    home.install(
        "b",
        &message_hook(
            "non-blocking",
            "transform",
            "if call.content[1].text ~= \"hi\" then error(\"stale\") end \
             return { context = \"ctx\" }",
        ),
    );
    let answer = before_message(&home.load(&[]), message_parts());
    let (replaced, context) = passed(&answer);
    assert_eq!(
        replaced,
        Some(vec![ContentPart::Text { text: "hi".into() }])
    );
    assert_eq!(
        context,
        vec![Context {
            state: Vec::new(),
            extension: "fiber.test/b".into(),
            text: "ctx".into(),
        }]
    );
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
    let BeforeMessageOutcome::Passed { replaced, .. } = &answer.outcome else {
        panic!("the message passes: {:?}", answer.outcome);
    };
    assert_eq!(
        replaced.as_ref().unwrap().changed_by,
        names(&["a"]),
        "a context-only hook is not named"
    );
}

#[test]
fn contexts_are_collected_in_order_with_their_extensions() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook("non-blocking", "transform", "return { context = \"A\" }"),
    );
    home.install(
        "b",
        &message_hook("non-blocking", "transform", "return { context = \"B\" }"),
    );
    let answer = before_message(&home.load(&[]), message_parts());
    let (replaced, context) = passed(&answer);
    assert_eq!(replaced, None);
    assert_eq!(
        context,
        vec![
            Context {
                state: Vec::new(),
                extension: "fiber.test/a".into(),
                text: "A".into(),
            },
            Context {
                state: Vec::new(),
                extension: "fiber.test/b".into(),
                text: "B".into(),
            },
        ]
    );
}

#[test]
fn a_check_hook_that_returns_context_has_failed() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook("blocking", "check", "return { context = \"x\" }"),
    );
    let answer = before_message(&home.load(&[]), message_parts());
    let BeforeMessageOutcome::Failed(failure) = answer.outcome else {
        panic!("the hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/a");
    let home = Home::new();
    home.install(
        "a",
        &message_hook("non-blocking", "check", "return { context = \"x\" }"),
    );
    let answer = before_message(&home.load(&[]), message_parts());
    let (replaced, _) = passed(&answer);
    assert_eq!(replaced, None);
    assert_eq!(answer.notices.len(), 1);
    assert!(
        answer.notices[0].message.contains("returned a change"),
        "{}",
        answer.notices[0].message
    );
}

#[test]
fn a_refusal_ends_the_chain_and_drops_the_collected_context() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook(
            "non-blocking",
            "transform",
            "return { context = \"early\" }",
        ),
    );
    home.install(
        "b",
        &message_hook("non-blocking", "transform", "return { refuse = \"no\" }"),
    );
    let answer = before_message(&home.load(&[]), message_parts());
    assert_eq!(
        answer.outcome,
        BeforeMessageOutcome::Refused(contract::hook::Refusal {
            extension: "fiber.test/b".into(),
            reason: "no".into(),
        })
    );
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_blocking_error_fails_the_message() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook("blocking", "transform", "error(\"kaput\")"),
    );
    let answer = before_message(&home.load(&[]), message_parts());
    let BeforeMessageOutcome::Failed(failure) = answer.outcome else {
        panic!("the hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/a");
    assert!(failure.message.contains("kaput"), "{}", failure.message);
}

#[test]
fn a_non_blocking_message_error_drops_its_change_and_the_chain_goes_on() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook("non-blocking", "transform", "error(\"kaput\")"),
    );
    home.install(
        "b",
        &message_hook("non-blocking", "transform", "return { context = \"late\" }"),
    );
    let answer = before_message(&home.load(&[]), message_parts());
    let (_, context) = passed(&answer);
    assert_eq!(
        context,
        vec![Context {
            state: Vec::new(),
            extension: "fiber.test/b".into(),
            text: "late".into(),
        }]
    );
    assert_eq!(answer.notices.len(), 1);
    let notice = &answer.notices[0];
    assert_eq!(notice.code, ErrorCode::HookFailed);
    assert_eq!(notice.extension.as_deref(), Some("fiber.test/a"));
    assert!(
        notice
            .message
            .contains("The `before_message` hook failed, so its change was dropped: "),
        "{}",
        notice.message
    );
    assert!(notice.message.contains("kaput"), "{}", notice.message);
}

#[test]
fn a_bad_message_return_fails_the_hook() {
    for (body, why) in [
        ("return { content = {} }", "empty list"),
        (
            "return { content = {{ type = \"video\", url = \"x\" }} }",
            "not a text, image or pdf part",
        ),
        (
            "return { content = {{ type = \"image\", path = \"artifacts/evil.png\", \
             mime_type = \"image/png\", width = 4, height = 4 }} }",
            "not one of the message's own parts",
        ),
        (
            "return { content = {{ type = \"pdf\", path = \"artifacts/q.pdf\", page_count = 1 }} }",
            "not one of the message's own parts",
        ),
        (
            "return { refuse = \"no\", content = {{ type = \"text\", text = \"hi\" }} }",
            "`refuse`",
        ),
        ("return { context = 3 }", "`context`"),
        ("return { context = \"\" }", "`context`"),
        ("return { refuse = \"\" }", "`refuse`"),
        ("return {{ type = \"text\", text = \"hi\" }}", "not a table"),
        (
            "return { content = { { type = \"text\", text = \"hi\" }, \
             { type = \"image\", path = \"artifacts/evil.png\", \
             mime_type = \"image/png\", width = 4, height = 4 } } }",
            "`content` part 2",
        ),
    ] {
        let home = Home::new();
        home.install("a", &message_hook("blocking", "transform", body));
        let answer = before_message(&home.load(&[]), message_parts());
        let BeforeMessageOutcome::Failed(failure) = answer.outcome else {
            panic!("{body} fails: {:?}", answer.outcome);
        };
        assert_eq!(failure.extension, "fiber.test/a", "{body}");
        assert!(failure.message.contains(why), "{body}: {}", failure.message);
    }
}

#[test]
fn nil_and_an_empty_table_change_no_message() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook("non-blocking", "transform", "return nil"),
    );
    home.install("b", &message_hook("non-blocking", "transform", "return {}"));
    let answer = before_message(&home.load(&[]), message_parts());
    let (replaced, context) = passed(&answer);
    assert_eq!(replaced, None);
    assert!(context.is_empty(), "{context:?}");
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn the_messages_own_image_and_pdf_parts_are_kept() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook(
            "non-blocking",
            "transform",
            "return { content = { \
             { type = \"image\", path = \"artifacts/i.png\", mime_type = \"image/png\", \
               width = 4, height = 4 }, \
             { type = \"pdf\", path = \"artifacts/p.pdf\", page_count = 2 } } }",
        ),
    );
    let answer = before_message(&home.load(&[]), message_parts());
    let (replaced, _) = passed(&answer);
    assert_eq!(
        replaced,
        Some(vec![
            image_part(),
            ContentPart::Pdf(PdfPart::new("artifacts/p.pdf".into(), 2, None).unwrap()),
        ])
    );
}

#[test]
fn a_replacement_cannot_bring_back_a_part_the_chain_dropped() {
    let without_image = "return { content = {{ type = \"text\", text = \"hi\" }} }";
    let with_image = "return { content = { \
     { type = \"text\", text = \"hi\" }, \
     { type = \"image\", path = \"artifacts/i.png\", mime_type = \"image/png\", \
       width = 4, height = 4 } } }";
    let home = Home::new();
    home.install("a", &message_hook("blocking", "transform", without_image));
    home.install("b", &message_hook("blocking", "transform", with_image));
    let answer = before_message(&home.load(&[]), message_parts());
    let BeforeMessageOutcome::Failed(failure) = answer.outcome else {
        panic!("the second hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/b");
}

#[test]
fn two_hooks_that_keep_the_image_name_both() {
    let echo = "return { content = call.content }";
    let home = Home::new();
    home.install("a", &message_hook("non-blocking", "transform", echo));
    home.install("b", &message_hook("non-blocking", "transform", echo));
    let answer = before_message(&home.load(&[]), message_parts());
    let (replaced, _) = passed(&answer);
    assert_eq!(
        replaced,
        Some(vec![
            ContentPart::Text { text: "hi".into() },
            image_part(),
            ContentPart::Pdf(PdfPart::new("artifacts/p.pdf".into(), 2, None).unwrap()),
        ])
    );
    let BeforeMessageOutcome::Passed { replaced, .. } = &answer.outcome else {
        panic!("the message passes: {:?}", answer.outcome);
    };
    assert_eq!(replaced.as_ref().unwrap().changed_by, names(&["a", "b"]));
}

#[test]
fn a_part_with_other_dimensions_is_not_the_messages_own() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook(
            "non-blocking",
            "transform",
            "return { content = call.content }",
        ),
    );
    home.install(
        "b",
        &message_hook(
            "blocking",
            "transform",
            "return { content = { \
             { type = \"image\", path = \"artifacts/i.png\", mime_type = \"image/png\", \
               width = 5, height = 4 } } }",
        ),
    );
    let answer = before_message(&home.load(&[]), message_parts());
    let BeforeMessageOutcome::Failed(failure) = answer.outcome else {
        panic!("the second hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/b");
}

#[test]
fn a_hook_is_shown_the_parts_as_logged_and_no_unknown_part() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook(
            "non-blocking",
            "transform",
            "return { content = call.content }",
        ),
    );
    let answer = before_message(&home.load(&[]), message_parts());
    let (replaced, _) = passed(&answer);
    // What the hook was shown and returned is exactly what the log holds,
    // without the part this build does not know.
    assert_eq!(
        replaced,
        Some(vec![
            ContentPart::Text { text: "hi".into() },
            image_part(),
            ContentPart::Pdf(PdfPart::new("artifacts/p.pdf".into(), 2, None).unwrap()),
        ])
    );
    assert_eq!(
        serde_json::to_value(&message_parts()[..3]).unwrap(),
        json!([
            { "type": "text", "text": "hi" },
            { "type": "image", "path": "artifacts/i.png", "mime_type": "image/png", "width": 4, "height": 4 },
            { "type": "pdf", "path": "artifacts/p.pdf", "page_count": 2 },
        ])
    );
}

fn reads() -> DeclaredEffects {
    declared(vec![Effect::Reads], Some(vec!["/r/a"]), false)
}

#[test]
fn two_transform_hooks_each_see_what_the_last_returned() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook(
            "non-blocking",
            "transform",
            "return { arguments = { path = call.arguments.path .. \"|a\" } }",
        ),
    );
    home.install(
        "b",
        &tool_hook(
            "non-blocking",
            "transform",
            "return { arguments = { path = call.arguments.path .. \"|b\" } }",
        ),
    );
    // One extension whose two hooks both rewrite is named once.
    home.install(
        "c",
        &(tool_hook(
            "non-blocking",
            "transform",
            "return { arguments = { path = call.arguments.path .. \"|c1\" } }",
        ) + &tool_hook(
            "non-blocking",
            "transform",
            "return { arguments = { path = call.arguments.path .. \"|c2\" } }",
        )),
    );
    let answer = before_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    let BeforeToolOutcome::Rewritten {
        arguments,
        changed_by,
    } = answer.outcome
    else {
        panic!("the hooks rewrite: {:?}", answer.outcome);
    };
    assert_eq!(arguments.get("path"), Some(&json!("x|a|b|c1|c2")));
    assert_eq!(changed_by, names(&["a", "b", "c"]));
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn hooks_order_before_tool_reorders_only_that_point() {
    let home = Home::new();
    home.install(
        "a",
        &(tool_hook(
            "non-blocking",
            "transform",
            "return { arguments = { path = call.arguments.path .. \"|a\" } }",
        ) + &tagging("a")),
    );
    home.install(
        "b",
        &(tool_hook(
            "non-blocking",
            "transform",
            "return { arguments = { path = call.arguments.path .. \"|b\" } }",
        ) + &tagging("b")),
    );
    let session = home.load(&[r#"hooks.order.before_tool=["fiber.test/b"]"#]);
    let answer = before_tool(&session, arguments(&[("path", "x")]), reads());
    let BeforeToolOutcome::Rewritten { changed_by, .. } = answer.outcome else {
        panic!("the hooks rewrite: {:?}", answer.outcome);
    };
    assert_eq!(changed_by, names(&["b", "a"]));
    // The `after_tool` order is untouched by the `before_tool` order.
    let AfterToolOutcome::Changed { content, .. } = after_tool(&session, "x").outcome else {
        panic!("the hooks change the output");
    };
    assert_eq!(content.as_deref(), Some("x|a|b"));
}

#[test]
fn a_before_tool_hook_is_shown_the_call_it_guards() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook(
            "non-blocking",
            "transform",
            "local keys = {} for k in pairs(call) do keys[#keys + 1] = k end \
             table.sort(keys) error(table.concat(keys, \",\"))",
        ),
    );
    let answer = before_tool(
        &home.load(&[]),
        arguments(&[("path", "a")]),
        declared(vec![Effect::Writes], Some(vec!["/w/a"]), false),
    );
    assert_eq!(answer.notices.len(), 1);
    assert!(
        answer.notices[0]
            .message
            .contains("arguments,effects,paths,reversible,tool"),
        "{}",
        answer.notices[0].message
    );
    // Without declared paths the hook is shown no `paths` key.
    let home = Home::new();
    home.install(
        "a",
        &tool_hook(
            "non-blocking",
            "transform",
            "local keys = {} for k in pairs(call) do keys[#keys + 1] = k end \
             table.sort(keys) error(table.concat(keys, \",\"))",
        ),
    );
    let answer = before_tool(
        &home.load(&[]),
        arguments(&[("path", "a")]),
        declared(vec![], None, true),
    );
    assert_eq!(answer.notices.len(), 1);
    assert!(
        answer.notices[0]
            .message
            .contains("arguments,effects,reversible,tool"),
        "{}",
        answer.notices[0].message
    );
    assert!(
        !answer.notices[0].message.contains("paths"),
        "{}",
        answer.notices[0].message
    );
}

#[test]
fn a_before_tool_hook_is_shown_the_declared_values() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook(
            "non-blocking",
            "transform",
            "error(call.tool .. \"|\" .. call.arguments.path .. \"|\" \
             .. table.concat(call.effects, \",\") .. \"|\" \
             .. (call.paths and table.concat(call.paths, \",\") or \"none\") .. \"|\" \
             .. tostring(call.reversible))",
        ),
    );
    let answer = before_tool(
        &home.load(&[]),
        arguments(&[("path", "a")]),
        declared(vec![Effect::Writes], Some(vec!["/w/a"]), false),
    );
    assert_eq!(answer.notices.len(), 1);
    assert!(
        answer.notices[0]
            .message
            .contains("write|a|writes|/w/a|false"),
        "{}",
        answer.notices[0].message
    );
}

#[test]
fn nil_and_an_empty_table_change_nothing() {
    let home = Home::new();
    home.install("a", &tool_hook("non-blocking", "transform", "return nil"));
    home.install("b", &tool_hook("non-blocking", "transform", "return {}"));
    let answer = before_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    assert_eq!(answer.outcome, BeforeToolOutcome::Unchanged);
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_refusal_ends_the_chain_before_a_later_blocking_hook() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook(
            "non-blocking",
            "transform",
            "return { refuse = \"no writes\" }",
        ),
    );
    home.install("b", &tool_hook("blocking", "transform", "error(\"never\")"));
    let answer = before_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    assert_eq!(
        answer.outcome,
        BeforeToolOutcome::Refused(contract::hook::Refusal {
            extension: "fiber.test/a".into(),
            reason: "no writes".into(),
        })
    );
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_blocking_error_fails_the_call() {
    let home = Home::new();
    home.install("a", &tool_hook("blocking", "transform", "error(\"kaput\")"));
    let answer = before_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    let BeforeToolOutcome::Failed(failure) = answer.outcome else {
        panic!("the hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/a");
    assert!(failure.message.contains("kaput"), "{}", failure.message);
}

#[test]
fn a_non_blocking_error_drops_its_change_and_the_chain_goes_on() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook(
            "non-blocking",
            "transform",
            "return { arguments = { path = call.arguments.path .. \"|a\" } }",
        ),
    );
    home.install(
        "b",
        &tool_hook("non-blocking", "transform", "error(\"kaput\")"),
    );
    home.install(
        "c",
        &tool_hook(
            "non-blocking",
            "transform",
            "return { arguments = { path = call.arguments.path .. \"|c\" } }",
        ),
    );
    let answer = before_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    let BeforeToolOutcome::Rewritten {
        arguments,
        changed_by,
    } = answer.outcome
    else {
        panic!("the chain goes on: {:?}", answer.outcome);
    };
    assert_eq!(arguments.get("path"), Some(&json!("x|a|c")));
    assert_eq!(changed_by, names(&["a", "c"]));
    assert_eq!(answer.notices.len(), 1);
    let notice = &answer.notices[0];
    assert_eq!(notice.code, ErrorCode::HookFailed);
    assert_eq!(notice.extension.as_deref(), Some("fiber.test/b"));
    assert!(
        notice
            .message
            .contains("The `before_tool` hook failed, so its change was dropped: "),
        "{}",
        notice.message
    );
    assert!(notice.message.contains("kaput"), "{}", notice.message);
}

#[test]
fn a_check_hook_that_returns_arguments_has_failed() {
    let body = "return { arguments = { path = \"x\" } }";
    let home = Home::new();
    home.install("a", &tool_hook("blocking", "check", body));
    let answer = check_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    let CheckOutcome::Failed(failure) = answer.outcome else {
        panic!("the hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/a");
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
    let home = Home::new();
    home.install("a", &tool_hook("non-blocking", "check", body));
    let answer = check_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    assert_eq!(answer.outcome, CheckOutcome::Passed);
    assert_eq!(answer.notices.len(), 1);
    assert!(
        answer.notices[0].message.contains("returned a change"),
        "{}",
        answer.notices[0].message
    );
}

#[test]
fn nil_and_an_empty_table_pass_a_check() {
    let home = Home::new();
    home.install("a", &tool_hook("non-blocking", "check", "return nil"));
    home.install("b", &tool_hook("non-blocking", "check", "return {}"));
    let answer = check_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    assert_eq!(answer.outcome, CheckOutcome::Passed);
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_bare_list_fails_a_check_hook() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook("blocking", "check", "return {{ refuse = \"no\" }}"),
    );
    let answer = check_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    let CheckOutcome::Failed(failure) = answer.outcome else {
        panic!("the hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/a");
    assert!(
        failure.message.contains("not a table"),
        "{}",
        failure.message
    );
}

#[test]
fn an_empty_check_refusal_fails_the_hook() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook("blocking", "check", "return { refuse = \"\" }"),
    );
    let answer = check_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    let CheckOutcome::Failed(failure) = answer.outcome else {
        panic!("the hook fails: {:?}", answer.outcome);
    };
    assert_eq!(failure.extension, "fiber.test/a");
    assert!(failure.message.contains("`refuse`"), "{}", failure.message);
}

#[test]
fn check_hooks_see_the_effects_they_are_given() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook(
            "blocking",
            "check",
            "if call.paths[1] ~= \"/w/b\" then return { refuse = \"wrong path\" } end",
        ),
    );
    home.install(
        "b",
        &tool_hook(
            "blocking",
            "check",
            "for _, effect in ipairs(call.effects) do \
             if effect == \"reads\" then return { refuse = \"reads\" } end end",
        ),
    );
    let session = home.load(&[]);
    let rewritten = declared(vec![Effect::Writes], Some(vec!["/w/b"]), false);
    let answer = check_tool(&session, arguments(&[("path", "b")]), rewritten);
    assert_eq!(answer.outcome, CheckOutcome::Passed);
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
    // The same hooks called with the original declaration refuse.
    let answer = check_tool(&session, arguments(&[("path", "a")]), reads());
    assert_eq!(
        answer.outcome,
        CheckOutcome::Refused(contract::hook::Refusal {
            extension: "fiber.test/a".into(),
            reason: "wrong path".into(),
        })
    );
}

#[test]
fn a_check_hook_that_refuses_on_the_original_effects_fails_the_check() {
    let home = Home::new();
    home.install(
        "b",
        &tool_hook(
            "blocking",
            "check",
            "for _, effect in ipairs(call.effects) do \
             if effect == \"reads\" then return { refuse = \"reads\" } end end",
        ),
    );
    let session = home.load(&[]);
    let answer = check_tool(&session, arguments(&[("path", "a")]), reads());
    assert_eq!(
        answer.outcome,
        CheckOutcome::Refused(contract::hook::Refusal {
            extension: "fiber.test/b".into(),
            reason: "reads".into(),
        })
    );
}

#[test]
fn before_tool_runs_no_check_hook() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook("blocking", "check", "return { refuse = \"never\" }"),
    );
    home.install(
        "b",
        &tool_hook(
            "non-blocking",
            "transform",
            "return { arguments = { path = call.arguments.path .. \"|b\" } }",
        ),
    );
    let answer = before_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    let BeforeToolOutcome::Rewritten { changed_by, .. } = answer.outcome else {
        panic!("the transform hook rewrites: {:?}", answer.outcome);
    };
    assert_eq!(changed_by, names(&["b"]));
}

#[test]
fn check_tool_runs_only_check_hooks() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook("blocking", "transform", "return { refuse = \"never\" }"),
    );
    home.install("b", &tool_hook("blocking", "check", "return nil"));
    let answer = check_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    assert_eq!(answer.outcome, CheckOutcome::Passed);
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_bad_return_fails_the_hook() {
    for (body, why) in [
        ("return { refuse = 3 }", "`refuse`"),
        ("return { refuse = \"\" }", "`refuse`"),
        ("return { refuse = \"x\", arguments = {} }", "`refuse`"),
        ("return { arguments = \"x\" }", "`arguments`"),
        ("return { arguments = { \"x\" } }", "`arguments`"),
        ("return { bogus = 1 }", "`bogus`"),
        ("return 3", "not a table"),
        ("return {{ path = \"x\" }}", "not a table"),
    ] {
        let home = Home::new();
        home.install("a", &tool_hook("blocking", "transform", body));
        let answer = before_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
        let BeforeToolOutcome::Failed(failure) = answer.outcome else {
            panic!("{body} fails: {:?}", answer.outcome);
        };
        assert_eq!(failure.extension, "fiber.test/a", "{body}");
        assert!(failure.message.contains(why), "{body}: {}", failure.message);
    }
}

/// Two non-blocking failures in one chain answer one grouped notice: the
/// count and the first item up front, every item with its extension in
/// the list, and no extension on the group itself.
fn assert_grouped_pair(notices: &[contract::events::Notice]) {
    assert_eq!(notices.len(), 1, "{notices:?}");
    let notice = &notices[0];
    assert_eq!(notice.code, ErrorCode::HookFailed);
    assert_eq!(notice.extension, None);
    assert!(
        notice.message.starts_with("2 hooks failed: "),
        "{}",
        notice.message
    );
    assert!(
        notice.message.contains("(extension `fiber.test/a`)"),
        "{}",
        notice.message
    );
    assert!(
        notice.message.contains("(extension `fiber.test/b`)"),
        "{}",
        notice.message
    );
}

#[test]
fn two_non_blocking_before_tool_failures_group_by_code() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook("non-blocking", "transform", "error(\"boom-a\")"),
    );
    home.install(
        "b",
        &tool_hook("non-blocking", "transform", "error(\"boom-b\")"),
    );
    let answer = before_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    assert!(
        matches!(answer.outcome, BeforeToolOutcome::Unchanged),
        "{:?}",
        answer.outcome
    );
    assert_grouped_pair(&answer.notices);
}

#[test]
fn two_non_blocking_check_tool_failures_group_by_code() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook("non-blocking", "check", "error(\"boom-a\")"),
    );
    home.install(
        "b",
        &tool_hook("non-blocking", "check", "error(\"boom-b\")"),
    );
    let answer = check_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    assert!(
        matches!(answer.outcome, CheckOutcome::Passed),
        "{:?}",
        answer.outcome
    );
    assert_grouped_pair(&answer.notices);
}

#[test]
fn two_non_blocking_before_message_failures_group_by_code() {
    let home = Home::new();
    home.install(
        "a",
        &message_hook("non-blocking", "transform", "error(\"boom-a\")"),
    );
    home.install(
        "b",
        &message_hook("non-blocking", "transform", "error(\"boom-b\")"),
    );
    let answer = before_message(
        &home.load(&[]),
        vec![ContentPart::Text { text: "hi".into() }],
    );
    let (replaced, _) = passed(&answer);
    assert_eq!(replaced, None);
    assert_grouped_pair(&answer.notices);
}

#[test]
fn an_empty_arguments_table_rewrites_to_the_empty_object() {
    let home = Home::new();
    home.install(
        "a",
        &tool_hook("non-blocking", "transform", "return { arguments = {} }"),
    );
    let answer = before_tool(&home.load(&[]), arguments(&[("path", "x")]), reads());
    let BeforeToolOutcome::Rewritten {
        arguments,
        changed_by,
    } = answer.outcome
    else {
        panic!("the hook rewrites: {:?}", answer.outcome);
    };
    assert!(arguments.is_empty(), "{arguments:?}");
    assert_eq!(changed_by, names(&["a"]));
}
