//! The driver shell `builtin` returns runs a command.

use std::sync::{Arc, Weak};

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::Event;
use contract::shapes::ContentPart;
use contract::tool::Cancel;

struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

struct Quiet;

impl Emit for Quiet {
    fn emit(&self, _event: &Event) {}
}

#[test]
fn the_driver_shell_runs_echo() {
    let root = fakes::TempDir::new("fiber-driver-shell");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let (_tools, _infos, driver) =
        super::builtin(root.path(), &root.path().join("artifacts"), &clock).unwrap();
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "command".to_owned(),
        serde_json::Value::String("echo hi".to_owned()),
    );
    let output = driver.run(&arguments, &Never, &Quiet);
    let text = output
        .content
        .iter()
        .find_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Unknown => None,
        })
        .expect("echo wrote text");
    assert!(text.contains("hi"), "{text}");
    assert_eq!(output.process.expect("echo ran").exit_code, Some(0));
}

#[test]
fn read_is_wired_to_the_image_child() {
    let root = fakes::TempDir::new("fiber-read-wired");
    std::fs::write(root.path().join("a.png"), b"\x89PNG\r\n\x1a\nrest").unwrap();
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let (tools, _infos, _driver) =
        super::builtin(root.path(), &root.path().join("artifacts"), &clock).unwrap();
    let (_, read) = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "read")
        .expect("read is registered");
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "path".to_owned(),
        serde_json::Value::String("a.png".to_owned()),
    );
    let output = read.run(&arguments, &Never, &Quiet);
    // The running binary here is the test harness, not `fiber`, so the child
    // fails; what matters is that it was started at all.
    let failure = output.error.expect("the harness is not an image child");
    assert!(
        !failure.message.contains("not configured"),
        "{}",
        failure.message
    );
}
