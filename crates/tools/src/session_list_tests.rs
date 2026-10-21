use super::*;
use contract::ErrorCode;
use contract::events::SessionStatus;
use contract::provider::ToolDefinition;
use contract::sessions::Running;
use contract::shapes::ContentPart;
use contract::tool::Bound;
use fakes::{CancelToken, Recorder};
use std::path::PathBuf;
use std::sync::Mutex;

struct FakeSessions {
    result: Result<Vec<Running>, ListError>,
    cancellations: Mutex<Vec<bool>>,
}
impl Sessions for FakeSessions {
    fn scope(&self) -> PathBuf {
        "/h/run/".into()
    }
    fn list(&self, cancel: &dyn Cancel) -> Result<Vec<Running>, ListError> {
        self.cancellations
            .lock()
            .unwrap()
            .push(cancel.is_cancelled());
        self.result.clone()
    }
    fn message(
        &self,
        _to: &SessionId,
        _from: &SessionId,
        _parent: Option<&SessionId>,
        _text: &str,
        _cancel: &dyn Cancel,
    ) -> Result<(), contract::sessions::MessageError> {
        panic!("session_list tests never message");
    }
}
fn fake(result: Result<Vec<Running>, ListError>) -> Arc<FakeSessions> {
    Arc::new(FakeSessions {
        result,
        cancellations: Mutex::new(vec![]),
    })
}
fn tool(sessions: Arc<FakeSessions>) -> SessionList {
    SessionList::new(sessions, SessionId("s_own".into()))
}
fn text(output: &Output) -> &str {
    match output.content.as_slice() {
        [ContentPart::Text { text }] => text,
        other => panic!("one text part: {other:?}"),
    }
}
fn status(parent: Option<SessionId>) -> SessionStatus {
    serde_json::from_value(json!({
        "name":"an uncut\nname", "workspace":"/w", "parent":parent, "model":"m",
        "state":"idle", "since":0, "started":0, "calls":0, "turns":0,
        "spend":{"tokens":{"input":0,"cache_read":0,"cache_write":{},"output":0},"cost":0.0,"subscription_cost":0.0},
        "delegates":0,"jobs":0,"project":"p","clients":0
    })).unwrap()
}
#[test]
fn definition_effects_and_defaults_are_constant() {
    let tool = tool(fake(Ok(vec![])));
    assert_eq!(tool.definition(), ToolDefinition {
        name: "session_list".into(),
        description: "Lists every running Fiber session of this account: this one, other projects' sessions, delegates and their siblings. Each entry gives the session's id, name and workspace, and its parent's id when it is a delegate; this session's entry has \"self\": true. A running session that did not report in time is listed by id with \"answering\": false.".into(),
        input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        deferred: false, hosted: None,
    });
    assert_eq!(tool.guidelines(), None);
    assert_eq!(tool.bound(), Bound::DEFAULT);
    let effects = tool.effects(&Map::new()).unwrap();
    assert_eq!(effects.declared.effects, [Effect::Reads]);
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, Some(vec!["/h/run/".into()]));
    assert_eq!(effects.subject.as_deref(), Some(""));
    assert_eq!(effects.prefix, None);
    assert!(!effects.always_reviewed);
}
#[test]
fn one_json_array_keeps_order_and_marks_only_itself() {
    let entries = vec![
        Running {
            id: SessionId("s_own".into()),
            status: Some(status(None)),
        },
        Running {
            id: SessionId("s_other".into()),
            status: Some(status(None)),
        },
        Running {
            id: SessionId("s_child".into()),
            status: Some(status(Some(SessionId("s_parent".into())))),
        },
        Running {
            id: SessionId("s_silent".into()),
            status: None,
        },
    ];
    let sessions = fake(Ok(entries));
    let output = tool(sessions.clone()).run(&Map::new(), &CancelToken::new(), &Recorder::default());
    assert!(output.error.is_none());
    assert_eq!(output.details, None);
    assert_eq!(
        serde_json::from_str::<Value>(text(&output)).unwrap(),
        json!([
            {"answering":true,"id":"s_own","name":"an uncut\nname","workspace":"/w","self":true},
            {"answering":true,"id":"s_other","name":"an uncut\nname","workspace":"/w"},
            {"answering":true,"id":"s_child","name":"an uncut\nname","workspace":"/w","parent":"s_parent"},
            {"answering":false,"id":"s_silent"}
        ])
    );
    assert_eq!(*sessions.cancellations.lock().unwrap(), [false]);
    for (entries, expected) in [
        (vec![], json!([])),
        (
            vec![Running {
                id: SessionId("s_own".into()),
                status: None,
            }],
            json!([{"answering":false,"id":"s_own","self":true}]),
        ),
    ] {
        let output =
            tool(fake(Ok(entries))).run(&Map::new(), &CancelToken::new(), &Recorder::default());
        assert_eq!(
            serde_json::from_str::<Value>(text(&output)).unwrap(),
            expected
        );
    }
}
#[test]
fn io_failure_is_a_tool_error_and_names_the_path_and_cause() {
    let output = tool(fake(Err(ListError::Io {
        path: "/h/run".into(),
        message: "denied".into(),
    })))
    .run(&Map::new(), &CancelToken::new(), &Recorder::default());
    let failure = output.error.as_ref().unwrap();
    assert_eq!(failure.code, ErrorCode::ToolError);
    assert!(failure.message.contains("/h/run"));
    assert!(failure.message.contains("denied"));
}
#[test]
fn cancelled_error_and_a_cancelled_success_are_not_lists_or_errors() {
    for result in [Err(ListError::Cancelled), Ok(vec![])] {
        let cancel = CancelToken::new();
        cancel.cancel();
        let sessions = fake(result);
        let output = tool(sessions.clone()).run(&Map::new(), &cancel, &Recorder::default());
        assert_eq!(text(&output), "Cancelled before it finished.");
        assert_eq!(output.error, None);
        assert_eq!(*sessions.cancellations.lock().unwrap(), [true]);
    }
    let output = tool(fake(Err(ListError::Cancelled))).run(
        &Map::new(),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(text(&output), "Cancelled before it finished.");
    assert_eq!(output.error, None);
}
