//! How many copies of a large tool result the loop makes on its own thread
//! while it cuts the result, writes its artifact and logs it
//! (`docs/tools.md`, "Bounded results"; `docs/extensions.md`, "The hook
//! points"). This binary installs the counting allocator and holds only
//! these tests, so no other test binary changes allocator.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::hint::black_box;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::Envelope;
use contract::events::TurnOutcome;
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks};
use contract::inbox::Delivery;
use contract::shapes::ContentPart;
use contract::tool::Tool;
use fakes::alloc::{Counting, large_blocks_during};
use fakes::{Scripted, within};
use serde_json::json;

use support::{Session, TestTool, calls_reply, delivery};

#[global_allocator]
static ALLOC: Counting = Counting;

/// The size of each large result: four large blocks' worth.
const RESULT: usize = 4 << 20;

/// How long a turn may take.
const TURN: Duration = Duration::from_secs(60);

/// `len` bytes of text, the same every time.
fn result(len: usize, seed: u8) -> String {
    (0..len)
        .map(|i| char::from(b'a' + u8::try_from((i + usize::from(seed)) % 26).unwrap()))
        .collect()
}

/// A session whose first reply calls `tool` once and whose second says
/// "Done.", with `hooks` installed when given. Runs the turn on its own
/// thread inside a counting scope and returns the session, the turn's
/// lines and the most large blocks alive at once on the loop's thread.
fn turn(tool: TestTool, hooks: Option<Arc<dyn Hooks>>) -> (Session, Vec<Envelope>, usize) {
    let ((), calibration) =
        large_blocks_during(|| drop(black_box(Vec::<u8>::with_capacity(2 << 20))));
    assert_eq!(calibration, 1, "the counting allocator is installed");
    let name = tool.name;
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[(name, json!({"city": "Paris"}))]),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::new(tool) as Arc<dyn Tool>],
    );
    let mut looped = session.looped.take().unwrap();
    if let Some(hooks) = hooks {
        looped = looped.hooks(hooks);
    }
    session.inbox.send(delivery("go")).unwrap();
    let (looped, outcome, peak) = within("the turn", TURN, move || {
        let (outcome, peak) = large_blocks_during(|| looped.turn().unwrap());
        (looped, outcome, peak)
    });
    session.looped = Some(looped);
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let lines = session.lines();
    (session, lines, peak)
}

fn completed(lines: &[Envelope]) -> &Envelope {
    lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap()
}

/// Checks the completion carries the 16 KiB cut of `full` with its notice,
/// and the artifact holds `full` byte for byte.
fn assert_cut(session: &Session, lines: &[Envelope], full: &str) {
    let done = completed(lines);
    let id = done.action_id.clone().unwrap().0;
    let artifact = format!("artifacts/{id}.txt");
    assert_eq!(done.payload["artifact"], artifact.as_str());
    let path = session.dir.join(&artifact);
    assert!(
        std::fs::read_to_string(&path).unwrap() == full,
        "the artifact holds the whole result"
    );
    let kept = done.payload["content"][0]["text"].as_str().unwrap();
    let expected = format!(
        "{}\n[{} bytes cut. The full output is in {}; read it with `read`.]",
        &full[..16 * 1024],
        full.len() - 16 * 1024,
        path.display()
    );
    assert!(
        kept == expected,
        "the completion carries the cut and its notice"
    );
}

#[test]
fn a_lone_large_text_part_is_not_copied_on_the_loops_thread() {
    let full = result(RESULT, 0);
    let tool = TestTool::reads("cat", &full);
    let (session, lines, peak) = turn(tool, None);
    assert_cut(&session, &lines, &full);
    assert_eq!(peak, 0, "the result is moved, never copied");
}

/// Answers every call with `content`, taken once.
struct Replaces {
    content: Mutex<Option<String>>,
}

impl Hooks for Replaces {
    fn after_tool(&self, _: &AfterToolCall<'_>) -> AfterToolAnswer {
        AfterToolAnswer {
            outcome: AfterToolOutcome::Changed {
                content: self.content.lock().unwrap().take(),
                details: None,
                artifact: None,
            },
            changed_by: vec!["replaces".to_owned()],
            notices: Vec::new(),
        }
    }

    fn deliver_to(&self, inbox: mpsc::Sender<Delivery>) {
        drop(inbox);
    }
}

#[test]
fn a_hooks_large_replacement_is_not_copied_on_the_loops_thread() {
    let full = result(RESULT, 1);
    let hooks = Arc::new(Replaces {
        content: Mutex::new(Some(full.clone())),
    });
    let (session, lines, peak) = turn(TestTool::reads("cat", "Sunny."), Some(hooks));
    assert_cut(&session, &lines, &full);
    assert_eq!(peak, 0, "the replacement is moved, never copied");
}

#[test]
fn several_text_parts_are_still_joined_into_one_artifact() {
    let (first, second) = (result(10 * 1024, 2), result(10 * 1024, 3));
    let mut tool = TestTool::reads("cat", &first);
    tool.output.content.push(ContentPart::Text {
        text: second.clone(),
    });
    let (session, lines, _) = turn(tool, None);
    assert_cut(&session, &lines, &format!("{first}\n{second}"));
}
