use super::*;
use contract::ErrorCode;
use contract::provider::ToolDefinition;
use contract::sessions::Running;
use contract::shapes::{ContentPart, Effect};
use contract::tool::Bound;
use fakes::{CancelToken, Recorder};
use std::path::PathBuf;
use std::sync::Mutex;

struct Call {
    to: SessionId,
    from: SessionId,
    parent: Option<SessionId>,
    text: String,
    cancelled: bool,
}

struct FakeSessions {
    result: Result<(), MessageError>,
    calls: Mutex<Vec<Call>>,
}

impl Sessions for FakeSessions {
    fn scope(&self) -> PathBuf {
        "/h/run/".into()
    }
    fn list(&self, _cancel: &dyn Cancel) -> Result<Vec<Running>, contract::sessions::ListError> {
        panic!("session_message tests never list");
    }
    fn message(
        &self,
        to: &SessionId,
        from: &SessionId,
        parent: Option<&SessionId>,
        text: &str,
        cancel: &dyn Cancel,
    ) -> Result<(), MessageError> {
        self.calls.lock().unwrap().push(Call {
            to: to.clone(),
            from: from.clone(),
            parent: parent.cloned(),
            text: text.to_owned(),
            cancelled: cancel.is_cancelled(),
        });
        self.result.clone()
    }
}

fn fake(result: Result<(), MessageError>) -> Arc<FakeSessions> {
    Arc::new(FakeSessions {
        result,
        calls: Mutex::new(vec![]),
    })
}

fn tool(sessions: Arc<FakeSessions>) -> SessionMessage {
    tool_with_parent(sessions, None)
}

fn tool_with_parent(sessions: Arc<FakeSessions>, parent: Option<SessionId>) -> SessionMessage {
    SessionMessage::new(
        sessions,
        SessionId("s_own".into()),
        Arc::new(move || parent.clone()),
    )
}

fn text(output: &Output) -> &str {
    match output.content.as_slice() {
        [ContentPart::Text { text }] => text,
        other => panic!("one text part: {other:?}"),
    }
}

fn arguments(value: serde_json::Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn definition_and_defaults_are_constant() {
    let tool = tool(fake(Ok(())));
    assert_eq!(tool.definition(), ToolDefinition {
        name: "session_message".into(),
        description: "Sends a plain-text message to one running Fiber session by its full id from session_list. It arrives there as steering during a turn, or starts a turn. Answers \"delivered\" once the target takes it; fails unreachable, closing, message_refused, or hook_failed.".into(),
        input_schema: json!({"type":"object","properties":{"id":{"type":"string","description":"The target session's full id, as session_list gives it."},"text":{"type":"string","description":"The message, plain text."}},"required":["id","text"],"additionalProperties":false}),
        deferred: false, hosted: None,
    });
    assert_eq!(tool.guidelines(), None);
    assert_eq!(tool.bound(), Bound::DEFAULT);
}

#[test]
fn effects_declare_writes_without_paths_so_no_fast_path() {
    let effects = tool(fake(Ok(())))
        .effects(&arguments(
            json!({"id": "s_0000000000000001", "text": "hi"}),
        ))
        .unwrap();
    assert_eq!(effects.declared.effects, [Effect::Writes]);
    assert!(!effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert_eq!(effects.subject.as_deref(), Some(""));
    assert_eq!(effects.prefix, None);
    assert!(!effects.always_reviewed);
    assert_eq!(effects.hosts, None);
}

#[test]
fn effects_reject_what_does_not_fit_the_schema() {
    let tool = tool(fake(Ok(())));
    // An extra key is the loop's schema check to refuse, as in every other
    // tool: `arguments` parses into `Args`, which ignores unknown fields.
    for value in [
        json!({"text": "hi"}),
        json!({"id": "s_0000000000000001"}),
        json!({"id": "s_0000000000000001", "text": 1}),
        json!({"id": 1, "text": "hi"}),
    ] {
        assert!(
            tool.effects(&arguments(value.clone())).is_err(),
            "arguments rejected: {value}"
        );
    }
}

#[test]
fn run_sends_the_readers_parent_as_the_hooks_parent() {
    for parent in [Some(SessionId("s_parent".into())), None] {
        let sessions = fake(Ok(()));
        let output = tool_with_parent(Arc::clone(&sessions), parent.clone()).run(
            &arguments(json!({"id": "s_0000000000000001", "text": "hi"})),
            &CancelToken::new(),
            &Recorder::default(),
        );
        assert_eq!(text(&output), "delivered");
        assert_eq!(output.error, None);
        let calls = sessions.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].parent, parent,
            "the reader's parent reaches the send"
        );
    }
}

#[test]
fn run_sends_from_the_calling_session_and_answers_delivered() {
    let sessions = fake(Ok(()));
    let output = tool(Arc::clone(&sessions)).run(
        &arguments(json!({"id": "s_0000000000000001", "text": "the parser is fixed"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(text(&output), "delivered");
    assert_eq!(output.error, None);
    let calls = sessions.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].to, SessionId("s_0000000000000001".into()));
    assert_eq!(calls[0].from, SessionId("s_own".into()));
    assert_eq!(calls[0].parent, None);
    assert_eq!(calls[0].text, "the parser is fixed");
    assert!(!calls[0].cancelled);
}

#[test]
fn run_passes_its_own_id_through_unchanged() {
    let sessions = fake(Ok(()));
    let output = tool(Arc::clone(&sessions)).run(
        &arguments(json!({"id": "s_own", "text": "to myself"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(text(&output), "delivered");
    assert_eq!(output.error, None);
    let calls = sessions.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].to, SessionId("s_own".into()));
    assert_eq!(calls[0].from, SessionId("s_own".into()));
    assert_eq!(calls[0].text, "to myself");
}

#[test]
fn unreachable_names_the_target_id() {
    let sessions = fake(Err(MessageError::Unreachable));
    let output = tool(Arc::clone(&sessions)).run(
        &arguments(json!({"id": "s_0123", "text": "hi"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let failure = output.error.as_ref().unwrap();
    assert_eq!(failure.code, ErrorCode::Unreachable);
    assert_eq!(failure.message, "No running session has the id s_0123.");
    assert_eq!(text(&output), "No running session has the id s_0123.\n");
    assert_eq!(sessions.calls.lock().unwrap().len(), 1);
}

#[test]
fn a_rejection_passes_through_unchanged() {
    for (code, message) in [
        (
            ErrorCode::Closing,
            "The session is closing and takes no session message.",
        ),
        (ErrorCode::MessageRefused, "not now"),
        (ErrorCode::HookFailed, "boom"),
    ] {
        let sessions = fake(Err(MessageError::Rejected(contract::shapes::Failure {
            code: code.clone(),
            message: message.to_owned(),
            retry_after_ms: None,
            provider: None,
        })));
        let output = tool(Arc::clone(&sessions)).run(
            &arguments(json!({"id": "s_0000000000000001", "text": "hi"})),
            &CancelToken::new(),
            &Recorder::default(),
        );
        let failure = output.error.as_ref().unwrap();
        assert_eq!(failure.code, code, "code passes through");
        assert_eq!(failure.message, message, "message passes through");
    }
}

#[test]
fn an_io_failure_is_a_tool_error_naming_the_session() {
    let sessions = fake(Err(MessageError::Io {
        path: "/h/run/s_0000000000000001".into(),
        message: "denied".into(),
    }));
    let output = tool(Arc::clone(&sessions)).run(
        &arguments(json!({"id": "s_0000000000000001", "text": "hi"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let failure = output.error.as_ref().unwrap();
    assert_eq!(failure.code, ErrorCode::ToolError);
    assert_eq!(
        failure.message,
        "Could not message session s_0000000000000001: denied"
    );
}

#[test]
fn cancelled_calls_say_nothing_about_delivery() {
    // The socket client reports the cancel.
    let sessions = fake(Err(MessageError::Cancelled));
    let output = tool(Arc::clone(&sessions)).run(
        &arguments(json!({"id": "s_0000000000000001", "text": "hi"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(text(&output), "Cancelled before it finished.");
    assert_eq!(output.error, None);
    // The cancel lands after the call returns.
    let sessions = fake(Ok(()));
    let cancel = CancelToken::new();
    cancel.cancel();
    let output = tool(Arc::clone(&sessions)).run(
        &arguments(json!({"id": "s_0000000000000001", "text": "hi"})),
        &cancel,
        &Recorder::default(),
    );
    assert_eq!(text(&output), "Cancelled before it finished.");
    assert_eq!(output.error, None);
    assert_eq!(sessions.calls.lock().unwrap().len(), 1);
    assert!(sessions.calls.lock().unwrap()[0].cancelled);
}

#[test]
fn invalid_arguments_in_run_are_invalid_arguments() {
    let output = tool(fake(Ok(()))).run(
        &arguments(json!({"id": "s_0000000000000001"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let failure = output.error.as_ref().unwrap();
    assert_eq!(failure.code, ErrorCode::InvalidArguments);
}
