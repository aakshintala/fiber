use std::sync::{Arc, Weak};

use super::*;
use crate::CommandId;
use crate::clock::Wake;
use crate::tool::Cancel;

/// A call that is never cancelled.
struct NeverCancelled;

impl Cancel for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

#[test]
fn inner_call_request_debug_names_its_command_and_tool() {
    let request = InnerCallRequest {
        extension: "ext-notes".to_owned(),
        command: CommandId("cmd-7f3a".to_owned()),
        name: "read_file".to_owned(),
        arguments: serde_json::json!({ "path": "notes.md" }),
        cancel: Arc::new(NeverCancelled),
        taken: Box::new(|| {}),
        answer: Box::new(|_| {}),
    };

    let shown = format!("{:?}", request);

    assert!(
        shown.contains("cmd-7f3a"),
        "Debug output misses the command id: {shown}"
    );
    assert!(
        shown.contains("read_file"),
        "Debug output misses the tool name: {shown}"
    );
}
