use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::*;
use crate::{CommandId, SessionId, TurnId};

const DOC: &str = include_str!("../../../docs/events.md");

#[test]
fn a_tool_call_completed_and_a_permission_resolved_omit_their_new_keys_when_empty() {
    for (kind, payload, key) in [
        (
            "tool_call_completed",
            json!({"status":"completed", "content":[]}),
            "hosts_surfaced",
        ),
        (
            "permission_resolved",
            json!({"decision":"allow", "decided_by":"person"}),
            "hosts",
        ),
    ] {
        let event = Event::from_envelope(&line(kind, payload.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(event.payload().unwrap()).unwrap(),
            payload
        );
        let mut populated = payload;
        populated[key] = json!(["https://example.com/"]);
        let event = Event::from_envelope(&line(kind, populated.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(event.payload().unwrap()).unwrap(),
            populated
        );
    }
    let event = Event::from_envelope(&line(
        "tool_call_completed",
        json!({"status":"completed", "content":[], "hosts_surfaced":[]}),
    ))
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::to_value(event.payload().unwrap()).unwrap(),
        json!({"status":"completed", "content":[]})
    );
}

#[test]
fn an_extensions_loaded_entry_omits_replaces_when_empty() {
    let empty = LoadedExtension {
        name: "e".into(),
        version: "0.1.0".into(),
        replaces: Vec::new(),
    };
    let written = serde_json::to_value(&empty).unwrap();
    // docs/events.md:1121: `replaces` is absent when empty.
    assert_eq!(written, json!({"name": "e", "version": "0.1.0"}));
    let read: LoadedExtension = serde_json::from_value(written).unwrap();
    assert_eq!(read.replaces, Vec::<String>::new());
    let full = LoadedExtension {
        replaces: vec!["read".into(), "model".into()],
        ..empty
    };
    assert_eq!(
        serde_json::to_value(&full).unwrap(),
        json!({"name": "e", "version": "0.1.0", "replaces": ["read", "model"]})
    );
}

/// Each kind under `docs/events.md`, "Kinds", with the class its text gives
/// and the keys its payload table lists.
fn doc_kinds() -> BTreeMap<&'static str, (Class, BTreeSet<&'static str>)> {
    let kinds = DOC.split("\n## Kinds\n").nth(1).unwrap();
    let kinds = kinds.split("\n## ").next().unwrap();
    let mut found = BTreeMap::new();
    for section in kinds.split("\n### ").skip(1) {
        let mut blocks = section.split("\n#### ");
        let intro = blocks.next().unwrap();
        for block in blocks {
            let name = block.split('`').nth(1).unwrap();
            let class = block
                .lines()
                .find_map(|line| {
                    if line.starts_with("Durable") {
                        Some(Class::Durable)
                    } else if line.starts_with("Ephemeral") {
                        Some(Class::Ephemeral)
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    intro
                        .contains("Both are ephemeral")
                        .then_some(Class::Ephemeral)
                })
                .unwrap_or_else(|| panic!("{name} has no class"));
            found.insert(name, (class, table_keys(block)));
        }
    }
    found
}

/// The keys of the first `| Key |` table in a block.
fn table_keys(block: &'static str) -> BTreeSet<&'static str> {
    let Some(table) = block.split("| Key |").nth(1) else {
        return BTreeSet::new();
    };
    table
        .lines()
        .skip(2)
        .take_while(|line| line.starts_with('|'))
        .map(|line| line.split('`').nth(1).unwrap())
        .collect()
}

fn line(kind: &str, payload: Value) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s".into()),
        ts: 1,
        schema_version: crate::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::from_value(payload).unwrap(),
    }
}

/// The lines under `docs/events.md`, "The envelope".
fn doc_examples() -> Vec<&'static str> {
    let section = DOC.split("## The envelope").nth(1).unwrap();
    let block = section.split("```json\n").nth(1).unwrap();
    block.split("```").next().unwrap().lines().collect()
}

#[test]
fn the_doc_examples_round_trip_through_their_kinds_byte_for_byte() {
    for text in doc_examples() {
        let mut envelope: Envelope = serde_json::from_str(text).unwrap();
        let event = Event::from_envelope(&envelope).unwrap().unwrap();
        assert_eq!(event.kind(), envelope.kind);
        envelope.payload = event.payload().unwrap();
        assert_eq!(serde_json::to_string(&envelope).unwrap(), text);
    }
}

#[test]
fn the_doc_example_turn_reads_as_its_type() {
    let envelope: Envelope = serde_json::from_str(doc_examples()[0]).unwrap();
    let Some(Event::TurnStarted(turn)) = Event::from_envelope(&envelope).unwrap() else {
        panic!("not a turn_started");
    };
    let [
        InputItem::Message {
            content, sender, ..
        },
    ] = turn.input.as_slice()
    else {
        panic!("not one message");
    };
    assert_eq!(sender.origin, crate::shapes::Origin::Driver);
    assert_eq!(sender.command_id, Some(crate::CommandId("c_7f3a".into())));
    assert_eq!(
        content,
        &[crate::shapes::ContentPart::Text {
            text: "fix the failing test".into()
        }]
    );
    assert_eq!(envelope.turn_id, Some(TurnId("t_9a02".into())));
}

#[test]
fn every_kind_in_the_doc_is_known_with_its_class() {
    let doc = doc_kinds();
    let code: BTreeMap<_, _> = KINDS.iter().copied().collect();
    assert_eq!(code.len(), KINDS.len(), "a kind is declared twice");
    let doc_classes: BTreeMap<_, _> = doc.iter().map(|(k, (c, _))| (*k, *c)).collect();
    assert_eq!(code, doc_classes);
}

#[test]
fn a_reader_skips_an_unknown_kind() {
    let envelope = line("kind_from_the_future", json!({"anything": 1}));
    assert_eq!(Event::from_envelope(&envelope).unwrap(), None);
}

#[test]
fn a_reader_ignores_an_unknown_field() {
    let envelope = line(
        "clients",
        json!({"count": 2, "field_from_the_future": true}),
    );
    let event = Event::from_envelope(&envelope).unwrap().unwrap();
    assert_eq!(event, Event::Clients(Clients { count: 2 }));
    assert_eq!(Value::Object(event.payload().unwrap()), json!({"count": 2}));
}

#[test]
fn a_known_kind_with_a_payload_that_does_not_fit_is_an_error() {
    let envelope = line("clients", json!({"count": "two"}));
    assert!(Event::from_envelope(&envelope).is_err());
}

#[test]
fn an_unknown_content_part_or_input_item_reads_as_unknown() {
    let payload = json!({"input": [
        {"type": "hologram"},
        {"type": "message", "source": "driver", "command_id": "c",
         "content": [{"type": "smell", "notes": "citrus"}]},
    ]});
    let event = Event::from_envelope(&line("turn_started", payload)).unwrap();
    let Some(Event::TurnStarted(turn)) = event else {
        panic!("not a turn_started");
    };
    assert_eq!(turn.input[0], InputItem::Unknown);
    let InputItem::Message { content, .. } = &turn.input[1] else {
        panic!("not a message");
    };
    assert_eq!(content, &[crate::shapes::ContentPart::Unknown]);
}

#[test]
fn a_pdf_part_with_and_without_pages_round_trips_as_its_type() {
    use crate::shapes::{ContentPart, ImagePart, PdfPart};
    let with_pages = json!([{"type":"text","text":"PDF: pages 2-3 of 30.\n"},{"type":"pdf","path":"artifacts/p_3f2a9c0d1e4b5a67.pdf","page_count":2,"pages":[{"type":"image","path":"artifacts/i_0a1b2c3d4e5f6071.png","mime_type":"image/png","width":1545,"height":2000},{"type":"image","path":"artifacts/i_8090a0b0c0d0e0f0.png","mime_type":"image/png","width":1545,"height":2000}]}]);
    let parts: Vec<ContentPart> = serde_json::from_value(with_pages.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parts).unwrap(), with_pages);
    let [ContentPart::Text { .. }, ContentPart::Pdf(part)] = parts.as_slice() else {
        panic!("not text plus pdf: {parts:?}");
    };
    assert_eq!(part.path(), "artifacts/p_3f2a9c0d1e4b5a67.pdf");
    assert_eq!(part.page_count(), 2);
    assert_eq!(part.pages().map(<[_]>::len), Some(2));
    let without_pages = json!([{"type":"text","text":"PDF: 2 pages.\nThe pages could not be rendered as images: pdftoppm is not installed. It comes with poppler (poppler-utils on Debian and Ubuntu, brew install poppler on macOS).\n"},{"type":"pdf","path":"artifacts/p_3f2a9c0d1e4b5a67.pdf","page_count":2}]);
    let parts: Vec<ContentPart> = serde_json::from_value(without_pages.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parts).unwrap(), without_pages);
    let [ContentPart::Text { .. }, ContentPart::Pdf(part)] = parts.as_slice() else {
        panic!("not text plus pdf: {parts:?}");
    };
    assert_eq!(part.pages(), None);
    // A boundary part with matching pages is accepted, down to one page.
    assert!(PdfPart::new("artifacts/p.pdf".into(), 1, None).is_ok());
    let ok = PdfPart::new(
        "artifacts/p.pdf".into(),
        2,
        Some(vec![
            ImagePart {
                path: "artifacts/i_1.png".into(),
                mime_type: "image/png".into(),
                width: 1,
                height: 1,
            },
            ImagePart {
                path: "artifacts/i_2.png".into(),
                mime_type: "image/png".into(),
                width: 1,
                height: 1,
            },
        ]),
    );
    assert!(ok.is_ok());
}

#[test]
fn a_pdf_part_that_breaks_an_invariant_fails_to_read() {
    use crate::shapes::{ContentPart, ImagePart, PdfPart};
    fn page(n: u32) -> ImagePart {
        ImagePart {
            path: format!("artifacts/i_{n}.png"),
            mime_type: "image/png".into(),
            width: 1,
            height: 1,
        }
    }
    // `page_count: 0` is refused.
    assert!(PdfPart::new("artifacts/p.pdf".into(), 0, None).is_err());
    assert!(
        serde_json::from_value::<ContentPart>(
            json!({"type":"pdf","path":"artifacts/p.pdf","page_count":0})
        )
        .is_err()
    );
    // `pages: []` with `page_count: 2` is refused, like any length mismatch.
    assert!(PdfPart::new("artifacts/p.pdf".into(), 2, Some(vec![])).is_err());
    assert!(PdfPart::new("artifacts/p.pdf".into(), 2, Some(vec![page(1)])).is_err());
    assert!(
        serde_json::from_value::<ContentPart>(
            json!({"type":"pdf","path":"artifacts/p.pdf","page_count":2,"pages":[]})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<ContentPart>(json!({"type":"pdf","path":"artifacts/p.pdf","page_count":2,"pages":[{"type":"image","path":"artifacts/i_1.png","mime_type":"image/png","width":1,"height":1}]})).is_err());
    // One page too many is refused too.
    assert!(PdfPart::new("artifacts/p.pdf".into(), 1, Some(vec![page(1), page(2)])).is_err());
    // A text part inside `pages` is refused.
    assert!(serde_json::from_value::<ContentPart>(json!({"type":"pdf","path":"artifacts/p.pdf","page_count":1,"pages":[{"type":"text","text":"t"}]})).is_err());
}

#[test]
fn durable_and_ephemeral_events_say_so() {
    let delta = Event::AssistantMessageDelta(TextDelta { text: "Hel".into() });
    assert_eq!(delta.class(), Class::Ephemeral);
    assert_eq!(Event::StepStarted(Empty {}).class(), Class::Durable);
}

#[test]
fn directory_added_round_trips_with_its_resolved_path() {
    let event = read("directory_added", json!({"path": "/shared"}))
        .unwrap()
        .unwrap();
    assert_eq!(
        event,
        Event::DirectoryAdded(DirectoryAdded {
            path: "/shared".into()
        })
    );
    assert_eq!(event.kind(), "directory_added");
    assert_eq!(event.class(), Class::Durable);
    assert_eq!(
        serde_json::to_value(event.payload().unwrap()).unwrap(),
        json!({"path": "/shared"})
    );
}

/// One payload per kind, or more where the doc makes keys exclusive, with
/// every key the doc lists for it.
fn samples() -> Vec<(&'static str, Value)> {
    let error = json!({"code": "timeout", "message": "m", "retry_after_ms": 1500,
        "provider": {"name": "p", "status": 429, "message": "slow down"}});
    let process = json!({"exit_code": 1, "signal": "SIGKILL", "timed_out": false});
    let content = json!([{"type": "text", "text": "t"},
        {"type": "image", "path": "artifacts/a.png", "mime_type": "image/png", "width": 2, "height": 3}]);
    let questions = json!([{"header": "h", "question": "q", "multiSelect": true,
        "options": [{"label": "a", "description": "d"}, {"label": "b"}]}]);
    let asked = json!([{"header": "h", "question": "q",
        "options": [{"label": "a"}, {"label": "b", "description": "d"}]},
        {"header": "n", "question": "free text"}]);
    let tokens =
        json!({"input": 1, "cache_read": 2, "cache_write": {"5m": 3, "1h": 4}, "output": 5});
    let usage = json!({"tokens": tokens, "cost": 0.5, "subscription_cost": 0.1});
    let settings = json!({"model": "p/m", "thinking": "high", "cache_lifetime": "5m",
            "credential": "work"});
    vec![
        (
            "fiber_started",
            json!({"version": "0.0.1", "resumed": false}),
        ),
        (
            "fiber_exited",
            json!({"exit_code": 1, "usage": usage, "final_action_id": "a", "text": "t",
            "error": error, "suspended_on": "r", "questions": questions}),
        ),
        (
            "session_started",
            json!({"workspace": "/w",
            "variables": {"path": "/usr/bin:/bin", "names": ["HOME"], "source": "login_shell"},
            "parent": {"session_id": "s", "delegate_id": "j"},
            "forked_from": {"session_id": "s", "seq": 3},
            "rewind": {"summary": "s", "note": "n"},
            "worktree": {"path": "/w", "branch": "fiber/s", "repository": "/r/.git"},
            "supervised": true}),
        ),
        (
            "rewound",
            json!({"new_session_id": "s2", "seq": 4, "from_session_id": "s1", "summary": "s"}),
        ),
        (
            "turn_started",
            json!({"input": [
            {"type": "message", "content": content, "source": "session",
             "from_session_id": "s0", "command_id": "c", "changed_by": ["e"]},
            {"type": "message", "content": [], "source": "extension", "extension": "e",
             "command_id": "c"},
            {"type": "shell_command", "seq": 2},
            {"type": "jobs", "job_ids": ["j"]},
            {"type": "handoff", "command_id": "c"},
            {"type": "rewind", "command_id": "c"}]}),
        ),
        ("step_started", json!({})),
        (
            "turn_completed",
            json!({"outcome": "failed", "error": error, "questions": questions}),
        ),
        (
            "turn_completed",
            json!({"outcome": "completed", "questions": asked}),
        ),
        (
            "steering_applied",
            json!({"content": content, "source": "extension", "extension": "e",
            "command_id": "c", "changed_by": ["e"]}),
        ),
        (
            "steering_applied",
            json!({"content": content, "source": "session",
            "from_session_id": "s0", "command_id": "c"}),
        ),
        (
            "steering_queue",
            json!({"messages": [{"content": content, "source": "driver",
            "command_id": "c"}]}),
        ),
        (
            "shell_command",
            json!({"command": "ls", "output": "o", "artifact": "artifacts/o.log",
            "process": process}),
        ),
        ("session_named", json!({"name": null, "by": "person"})),
        ("directory_added", json!({"path": "/shared"})),
        ("clients", json!({"count": 1})),
        (
            "session_status",
            json!({"name": "n", "workspace": "/w", "parent": "s0", "supervised": true, "model": "p/m", "thinking": "high",
            "state": "idle", "started": 0, "calls": 0, "turns": 0, "since": 1, "spend": usage, "delegates": 0, "jobs": 0, "project": "-w", "clients": 0}),
        ),
        (
            "session_status",
            json!({"name": "n", "workspace": "/w", "model": "p/m",
            "state": "idle", "started": 0, "calls": 0, "since": 1, "git": {"branch": "main"},
            "context": {"tokens": 3, "window": 4}, "spend": usage,
            "delegates": 0, "jobs": 0, "project": "-w", "clients": 0}),
        ),
        (
            "session_status",
            json!({"name": "n", "workspace": "/w", "model": "p/m",
            "state": "idle", "started": 0, "calls": 0, "since": 1, "spend": usage, "delegates": 0, "jobs": 0, "project": "-w", "clients": 0}),
        ),
        (
            "session_status",
            json!({"name": "n", "workspace": "/w", "model": "p/m",
            "state": "streaming", "started": 0, "calls": 0, "since": 1, "spend": usage, "delegates": 0, "jobs": 0, "project": "-w", "clients": 0}),
        ),
        (
            "session_status",
            json!({"name": "n", "workspace": "/w", "model": "p/m",
            "state": "tool", "tool": "shell", "started": 0, "calls": 0, "step": "shell ls", "since": 1, "spend": usage,
            "delegates": 1, "jobs": 0, "project": "-w", "clients": 0}),
        ),
        (
            "session_status",
            json!({"name": "n", "workspace": "/w", "model": "p/m",
            "state": "retrying", "closing": true, "started": 0, "calls": 0, "since": 1, "spend": usage, "delegates": 0, "jobs": 1, "project": "-w", "clients": 0}),
        ),
        (
            "session_status",
            json!({"name": "n", "workspace": "/w", "model": "p/m",
            "state": "waiting",
            "waiting": {"request_id": "r", "kind": "approval", "summary": "run npm"},
            "started": 0, "calls": 0, "since": 1, "spend": usage, "delegates": 0, "jobs": 0, "project": "-w", "clients": 0}),
        ),
        (
            "session_status",
            json!({"name": "n", "workspace": "/w", "model": "p/m",
            "state": "waiting",
            "waiting": {"request_id": "r", "kind": "question", "summary": "which file?"},
            "started": 0, "calls": 0, "since": 1, "spend": usage, "delegates": 0, "jobs": 0, "project": "-w", "clients": 0}),
        ),
        (
            "session_status",
            json!({"name": "n", "workspace": "/w", "model": "p/m",
            "state": "waiting",
            "waiting": {"request_id": "r", "kind": "offer", "summary": "3 items from the repository"},
            "started": 0, "calls": 0, "since": 1, "spend": usage, "delegates": 0, "jobs": 0, "project": "-w", "clients": 0}),
        ),
        (
            "context_added",
            json!({"text": "t", "extension": "e", "hook": "turn_start"}),
        ),
        ("assistant_message_started", json!({})),
        ("assistant_message_delta", json!({"text": "Hel"})),
        (
            "assistant_message_completed",
            json!({"outcome": "failed", "error": error}),
        ),
        (
            "text_completed",
            json!({"text": "Yo", "provider_item": {"text": "Yo", "thoughtSignature": "c2ln"}}),
        ),
        ("text_completed", json!({"text": "Checking the file."})),
        (
            "tool_call_arguments_delta",
            json!({"index": 0, "name": "read", "text": "{\"pa"}),
        ),
        ("reasoning_started", json!({})),
        ("reasoning_delta", json!({"text": "hmm"})),
        (
            "reasoning_completed",
            json!({"text": "", "provider_item": {"id": "rs_1"}}),
        ),
        (
            "tool_call_requested",
            json!({"name": "read",
            "arguments": {"path": "/a", "limit": "5", "all": "true", "edits": "[1]", "x": null},
            "provider_id": "call_1",
            "repaired": {"path": "/a", "limit": 5, "all": true, "edits": [1]},
            "repairs": [
                {"path": "/limit", "fix": "string_to_number"},
                {"path": "/all", "fix": "string_to_boolean"},
                {"path": "/edits", "fix": "string_parsed"},
                {"path": "/x", "fix": "null_dropped"}]}),
        ),
        (
            "tool_call_requested",
            json!({"name": "read", "arguments": "{not json"}),
        ),
        (
            "tool_call_requested",
            json!({"name": "read", "arguments": {"path": "/a"},
            "ran_by": {"extension": "codemode", "outer_action_id": "a_1"}}),
        ),
        (
            "tool_call_requested",
            json!({"name": "read", "arguments": {"path": "/a"},
            "ran_by": {"extension": "checks", "command_id": "c_1"}}),
        ),
        (
            "tool_call_requested",
            json!({"name": "web_search", "arguments": {"query": "rust"},
            "provider_id": "srvtoolu_01",
            "provider_item": {"type": "server_tool_use", "id": "srvtoolu_01",
            "name": "web_search", "input": {"query": "rust"}}}),
        ),
        (
            "tool_call_started",
            json!({"effects": ["reads", "writes", "executes", "network"],
            "reversible": false, "paths": ["/a"], "arguments": {"path": "/a"},
            "changed_by": ["e"]}),
        ),
        (
            "tool_call_started",
            json!({"effects": ["executes"], "reversible": false,
            "always_reviewed": true}),
        ),
        (
            "tool_call_delta",
            json!({"text": "t", "details": {"pct": 5}}),
        ),
        (
            "tool_call_completed",
            json!({"status": "failed", "reason": "r", "error": error,
            "process": process, "content": content, "details": [1], "artifact": "artifacts/x",
            "kept": [[0, 8192], [1040384, 1048576]],
            "changes": [{"path": "/a", "added": 1, "removed": 2}],
            "control": {"handoff": "note"}, "changed_by": ["e"],
            "hosts_surfaced": ["https://example.com/"]}),
        ),
        (
            "tool_call_completed",
            json!({"status": "completed", "content": content,
            "control": {"questions": asked}}),
        ),
        (
            "tool_call_completed",
            json!({"status": "completed", "content": content,
            "control": {"handoff": "note", "questions": asked}}),
        ),
        (
            "tool_call_completed",
            json!({"status": "completed", "content": content,
            "control": {"skill": {"name": "tdd", "path": "/w/.agents/skills/tdd/SKILL.md"}}}),
        ),
        (
            "tool_call_completed",
            json!({"status": "completed", "content": content,
            "control": {"name": "Fix the parser"}}),
        ),
        (
            "tool_call_completed",
            json!({"status": "completed", "content": content,
            "provider_item": {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01",
            "content": []}}),
        ),
        (
            "tool_call_completed",
            json!({"status": "completed", "content": content, "loaded": ["b", "a"]}),
        ),
        (
            "permission_requested",
            json!({"request_id": "r", "effects": [], "reversible": true,
            "paths": ["/a"], "step": "review",
            "escalation": {"cause": "reviewer_failed", "error": error},
            "rule": {"subject": "npm test", "prefix": "npm"}}),
        ),
        (
            "permission_requested",
            json!({"request_id": "r", "effects": ["writes"], "reversible": true,
            "step": "review", "escalation": {"cause": "consecutive_blocks", "reason": "r"}}),
        ),
        (
            "permission_requested",
            json!({"request_id": "r", "effects": ["executes"], "reversible": true,
            "step": "review"}),
        ),
        (
            "permission_requested",
            json!({"request_id": "r", "effects": ["reads"],
            "reversible": true, "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "npm"}}),
        ),
        (
            "permission_requested",
            json!({"request_id": "r", "effects": ["executes"],
            "reversible": false, "always_reviewed": true, "step": "review"}),
        ),
        (
            "permission_resolved",
            json!({"request_id": "r", "decision": "allow",
            "decided_by": "reviewer", "reason": "r", "feedback": "f",
            "grant": {"tool": "shell", "prefix": "npm"},
            "rule": {"tool": "shell", "prefix": "npm"},
            "reviewer": {"model": "p/m", "stage": 2}}),
        ),
        (
            "permission_resolved",
            json!({"decision": "deny", "decided_by": "budget",
            "reason": "The session reached its spending budget."}),
        ),
        (
            "permission_resolved",
            json!({"request_id": "r", "decision": "deny", "decided_by": "no_reviewer",
            "reason": "No reviewer model is set, so every reviewed call goes to a person. Set reviewer.model."}),
        ),
        (
            "permission_resolved",
            json!({"request_id": "r", "decision": "deny", "decided_by": "hook",
            "reason": "Extension e refused this call: no. It did not run."}),
        ),
        (
            "permission_resolved",
            json!({"decision": "deny", "decided_by": "reviewer",
            "reason": "no", "reviewer": {"model": "p/m", "stage": 1},
            "effects": ["executes"], "reversible": false, "paths": ["/a"],
            "hosts": ["https://example.com/"],
            "always_reviewed": true, "arguments": {"path": "b.txt"},
            "changed_by": ["ext"]}),
        ),
        (
            "interaction_requested",
            json!({"request_id": "r", "kind": "multi_select",
            "action_ids": ["a"], "extension": "e", "prompt": "p",
            "options": [{"label": "a"}]}),
        ),
        (
            "interaction_requested",
            json!({"request_id": "r", "kind": "select", "prompt": "p",
            "options": [{"label": "a", "description": "d"}]}),
        ),
        (
            "interaction_requested",
            json!({"request_id": "r", "kind": "confirm", "prompt": "p"}),
        ),
        (
            "interaction_requested",
            json!({"request_id": "r", "kind": "text_input", "prompt": "p"}),
        ),
        (
            "interaction_requested",
            json!({"request_id": "r", "kind": "form", "fields": questions, "resumes": true}),
        ),
        (
            "interaction_resolved",
            json!({"request_id": "r", "by": "fiber", "declined": true}),
        ),
        (
            "interaction_resolved",
            json!({"request_id": "r", "by": "person", "confirmed": false}),
        ),
        (
            "interaction_resolved",
            json!({"request_id": "r", "by": "person", "labels": []}),
        ),
        (
            "interaction_resolved",
            json!({"request_id": "r", "by": "person", "text": "t"}),
        ),
        (
            "interaction_resolved",
            json!({"request_id": "r", "by": "person", "note": "n",
            "answers": [{"skipped": true}, {"labels": ["a"], "text": "t"}, {"labels": []}]}),
        ),
        (
            "repository_code_offered",
            json!({"request_id": "r", "items": [
                {"kind": "extension", "name": "n", "hash": "h", "required": true,
                 "summary": "s", "version": "1.0.0", "diff": "d", "files": ["tools/init.lua"]},
                {"kind": "mcp_server", "name": "m", "hash": "h", "required": false, "summary": "s", "files": []}]}),
        ),
        (
            "repository_code_resolved",
            json!({"request_id": "r", "decisions": ["approve", "skip", "never"]}),
        ),
        (
            "usage_recorded",
            json!({"generation_id": "g", "model": "p/m", "tokens": tokens,
            "input_bytes": 48213, "input_media": true,
            "web_searches": 1, "cost": 0.25, "subscription": true, "extension": "e",
            "origin_session_id": "s0"}),
        ),
        (
            "usage_recorded",
            json!({"generation_id": "g", "model": "p/m", "tokens": tokens,
            "input_bytes": 48213, "cost": 0.25,
            "reviewer": {"purpose": "stage_1", "action_id": "a_1"}}),
        ),
        (
            "quota_noticed",
            json!({"provider": "p", "credential": "work", "window": "5h", "percent_used": 80.5,
            "resets_at": 17, "notice_at": 80.0}),
        ),
        (
            "quota_rearmed",
            json!({"provider": "p", "credential": "work", "window": "5h", "percent_used": 79.5, "notice_at": 80.0}),
        ),
        (
            "retry_scheduled",
            json!({"code": "rate_limited", "attempt": 2, "delay_ms": 500, "last_attempt": 4}),
        ),
        (
            "notice",
            json!({"code": "extension_failed", "message": "m", "extension": "e"}),
        ),
        (
            "preamble_built",
            json!({"reason": "switch", "model": "p/m", "context_window": 200000,
            "trigger_at": 140000, "budget": 2.5, "thinking": "high", "tool_choice": "auto",
            "cache_lifetime": "1h", "credential": "work", "system_prompt": "s",
            "tools": [{"name": "read", "registered_by": "e", "deferred": false,
            "definition": {"type": "object"}}],
            "replaced": [{"name": "read", "from": "builtin", "to": "e"}]}),
        ),
        (
            "model_changed",
            json!({"before": settings, "after": settings, "source": "extension",
            "extension": "e"}),
        ),
        (
            "opening_message",
            json!({"environment": {"date": "2026-09-29", "os": "linux",
            "arch": "x86_64", "shell": "bash", "workspace": "/w", "git": {"branch": null},
            "session_log": "/l"}, "instruction_files": [{"path": "/a", "content": "c"}],
            "extension_sections": [{"extension": "fiber.test/notes",
            "files": [{"path": "/h/data/fiber.test-notes/index.md", "content": "- [[x]]"}],
            "budget_bytes": 25000}],
            "skills": [{"name": "s", "description": "d", "path": "/s/SKILL.md",
            "source": "repository"}],
            "added_directories": ["/shared"]}),
        ),
        (
            "instruction_file",
            json!({"path": "/a", "reason": "own_edit", "content": "c",
            "sent": "none"}),
        ),
        (
            "instruction_file",
            json!({"path": "/h/data/fiber.test-notes/index.md", "reason": "changed",
            "extension": "fiber.test/notes", "content": "- [[x]]", "sent": "full"}),
        ),
        ("date_changed", json!({"date": "2026-09-30"})),
        (
            "skills_changed",
            json!({"added": [{"name": "s", "description": "d", "path": "/s/SKILL.md",
            "source": "builtin"}], "removed": ["r"]}),
        ),
        ("handoff_started", json!({"trigger": "overflow"})),
        (
            "handoff_completed",
            json!({"outcome": "completed", "note": ["a"], "tokens_before": 9,
            "instructions": "i"}),
        ),
        (
            "handoff_completed",
            json!({"outcome": "completed", "note_text": "n", "extension": "e",
            "tokens_before": 9}),
        ),
        (
            "handoff_completed",
            json!({"outcome": "failed", "error": error, "tokens_before": 9}),
        ),
        (
            "skills_resent",
            json!({"skills": [{"name": "s", "path": "/s/SKILL.md", "content": "c"}]}),
        ),
        ("context_nudged", json!({"tokens": 100, "trigger_at": 140})),
        (
            "reviewer_kept",
            json!({"kept":[{"seq":12,"item":0},{"seq":31,"item":2}]}),
        ),
        ("reviewer_kept", json!({"kept":[],"failed":true})),
        (
            "mcp_server_failed",
            json!({"server": "m", "reason": "not_logged_in",
            "will_restart": true, "error": error}),
        ),
        ("mcp_server_ready", json!({"server": "m"})),
        (
            "reloaded",
            json!({"servers": {"kept": ["a"], "restarted": [], "started": [],
            "stopped": []}, "extensions": ["e"],
            "failed": [{"server": "m", "reason": "died", "error": error}]}),
        ),
        (
            "extensions_loaded",
            json!({"extensions": [{"name": "e", "version": "0.1.0"}]}),
        ),
        (
            "extensions_loaded",
            json!({"extensions": [{"name": "e", "version": "0.1.0", "replaces": ["read", "model"]}]}),
        ),
        (
            "extension_state_set",
            json!({"extension": "e", "key": "k", "value": [1],
            "on_fork": "at_point"}),
        ),
        (
            "extension_state_unset",
            json!({"extension": "e", "key": "k"}),
        ),
        ("extension_ui", json!({"extension": "e", "status": ""})),
        (
            "extension_ui",
            json!({"extension": "e", "widget": "w", "lines": ["l"]}),
        ),
        ("extension_message", json!({"extension": "e", "data": "d"})),
        ("extension_log", json!({"extension": "e", "message": "m"})),
        (
            "extension_exec",
            json!({"extension": "e", "program": "git", "args": ["status"],
            "cwd": "/w", "process": process}),
        ),
        (
            "job_started",
            json!({"job_id": "j", "tool": "shell", "extension": "e",
            "description": "d", "output_path": "/o"}),
        ),
        (
            "delegate_started",
            json!({"job_id": "j", "delegate_session_id": "s2",
            "harness": "fiber", "model": "p/m", "workspace": "/w",
            "worktree": {"path": "/t", "branch": "b"},
            "forked_from": {"session_id": "s", "seq": 3}}),
        ),
        (
            "job_delta",
            json!({"job_id": "j", "text": "t", "details": null}),
        ),
        (
            "job_line",
            json!({"job_id": "j", "lines": "a\nb", "suppressed": 3,
            "changed_by": ["e"]}),
        ),
        (
            "delegate_finished",
            json!({"job_id": "j", "text": "t", "artifact": "artifacts/f",
            "questions": questions, "changed_by": ["e"],
            "usage": {"tokens": tokens, "cost": null, "subscription_cost": 0.0},
            "worktree": {"path": "/t", "branch": "b", "dirty": true}}),
        ),
        (
            "job_completed",
            json!({"job_id": "j", "status": "cancelled", "error": error,
            "process": process, "output_tail": "tail",
            "notice": "n", "changed_by": ["e"]}),
        ),
        (
            "jobs_pending_notified",
            json!({"job_ids": ["j"], "reason": "ending"}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"session_id": "s2"}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"clients": 2, "fiber_version": "0.0.0", "running": true, "variables": {"path": "/usr/bin:/bin", "names": ["HOME"], "source": "inherited"}}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"new_session_id": "s2"}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"tools": [
            {"name": "read", "source": "builtin", "state": "full", "bytes": 10, "tokens": 3},
            {"name": "x", "source": "mcp", "server": "m", "tool": "x", "state": "deferred", "bytes": 1},
            {"name": "y", "source": "extension", "extension": "e", "state": "loaded",
             "bytes": 1}]}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"output": "o",
            "artifact": "artifacts/o", "process": process}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"lines": [
            {"kind": "step_started", "session_id": "s", "ts": 1, "schema_version": 1,
             "seq": 0, "payload": {}}]}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"commands": [
            {"name": "review", "description": "Review a diff.", "argument_hint": "[base]",
             "tag": "template"},
            {"name": "tdd", "description": "Test first.", "tag": "skill"}]}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"text": "a\nb\n", "lines": [0, 2], "next": 4, "size": 9000}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"text": "z\n", "lines": [0], "size": 9000}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"last_seq": 41}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"quota": [{"provider": "codex", "credential": "work", "windows": [{"name": "weekly", "percent_used": 35.0, "resets_at": 1760000000000_u64}, {"name": "daily", "reset": true}]}], "notice_at": 80.0}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"quota": [{"provider": "openrouter", "credential": "work", "credit": {"remaining": 4.2, "limit": 10.0}}], "notice_at": 80.0}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"quota": [{"provider": "openrouter", "credential": "work", "credit": {"remaining": 4.2}}], "notice_at": 80.0}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"quota": [{"provider": "codex", "credential": "work", "windows": [{"name": "weekly", "percent_used": 35.0}], "unreachable": true}], "notice_at": 80.0}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"quota": [{"provider": "codex", "credential": "broken", "unreachable": true}], "notice_at": 80.0}}),
        ),
        (
            "command_accepted",
            json!({"command_id": "c", "result": {"quota": [], "notice_at": 80.0}}),
        ),
        ("command_accepted", json!({"command_id": "c"})),
        (
            "command_rejected",
            json!({"command_id": "c", "code": "busy", "message": "m"}),
        ),
        (
            "command_rejected",
            json!({"code": "malformed", "message": "m"}),
        ),
        (
            "extension_hello",
            json!({"session_id": "s", "reason": "new", "workspace": "/w",
            "data_dirs": {"machine": "/m", "project": "/p"}, "state": {}}),
        ),
        (
            "extension_request",
            json!({"request_id": "r_A", "registration": "c_reg", "key": "run",
            "args": [{"object": {}, "list": []}]}),
        ),
    ]
}

#[test]
fn a_retry_scheduled_without_last_attempt_does_not_read() {
    assert!(
        read(
            "retry_scheduled",
            json!({"code": "rate_limited", "attempt": 2, "delay_ms": 500})
        )
        .is_err()
    );
}

#[test]
fn stream_reopened_samples_round_trip_as_hub_lines() {
    // Hub lines have no session envelope and are not session Event kinds.
    let samples = [
        json!({"session_id": "s_0123456789abcdef", "last_seq": 41}),
        json!({"session_id": "s_0123456789abcdef"}),
    ];
    for payload in samples {
        let sample = json!({"kind": "stream_reopened", "ts": 1,
            "schema_version": crate::SCHEMA_VERSION, "payload": payload});
        let read: crate::HubLine = serde_json::from_value(sample.clone()).unwrap();
        assert_eq!(read.kind, "stream_reopened");
        assert_eq!(read.payload, payload.as_object().unwrap().clone());
        assert_eq!(serde_json::to_value(read).unwrap(), sample);
    }
}

#[test]
fn every_sample_reads_as_its_kind_and_writes_back_unchanged() {
    for (kind, payload) in samples() {
        let event = Event::from_envelope(&line(kind, payload.clone()))
            .unwrap_or_else(|e| panic!("{kind}: {e}"))
            .unwrap();
        assert_eq!(event.kind(), kind);
        let written = Value::Object(event.payload().unwrap());
        // `null` details read as absent, which is the one lossy key here.
        let expected = if kind == "job_delta" {
            json!({"job_id": "j", "text": "t"})
        } else {
            payload
        };
        assert_eq!(written, expected, "{kind}");
    }
}

#[test]
fn a_session_status_round_trips_with_and_without_step() {
    let (_, base) = samples()
        .into_iter()
        .find(|(kind, _)| *kind == "session_status")
        .unwrap();
    for (step, thinking) in [Some("shell ls"), None]
        .into_iter()
        .flat_map(|step| [Some("high"), None].map(|thinking| (step, thinking)))
    {
        let mut payload = base.clone();
        payload.as_object_mut().unwrap().remove("thinking");
        if let Some(thinking) = thinking {
            payload["thinking"] = json!(thinking);
        }
        payload["started"] = json!(123);
        payload["calls"] = json!(2);
        if let Some(step) = step {
            payload["step"] = json!(step);
        }
        let Some(Event::SessionStatus(status)) = read("session_status", payload.clone()).unwrap()
        else {
            panic!("not a session_status");
        };
        assert_eq!(status.started, 123);
        assert_eq!(status.calls, 2);
        assert_eq!(status.step.as_deref(), step);
        let written = serde_json::to_value(status).unwrap();
        assert_eq!(written.get("thinking").and_then(Value::as_str), thinking);
        if thinking.is_none() {
            assert!(!written.as_object().unwrap().contains_key("thinking"));
        }
        assert_eq!(written, payload);
    }
}

#[test]
fn a_session_status_reads_turns_and_reads_none_without_it() {
    let mut payload = samples()
        .into_iter()
        .find(|(kind, _)| *kind == "session_status")
        .unwrap()
        .1;
    payload["turns"] = json!(3);
    let status: SessionStatus = serde_json::from_value(payload.clone()).unwrap();
    assert_eq!(status.turns, Some(3));
    assert_eq!(serde_json::to_value(&status).unwrap(), payload);
    payload.as_object_mut().unwrap().remove("turns");
    let old: SessionStatus = serde_json::from_value(payload.clone()).unwrap();
    assert_eq!(old.turns, None);
    assert_eq!(serde_json::to_value(&old).unwrap(), payload);
}

#[test]
fn a_session_status_writes_closing_only_when_true() {
    let mut payload = samples()
        .into_iter()
        .find(|(kind, _)| *kind == "session_status")
        .unwrap()
        .1;
    payload["closing"] = json!(true);
    let status: SessionStatus = serde_json::from_value(payload.clone()).unwrap();
    assert!(status.closing);
    assert_eq!(serde_json::to_value(&status).unwrap(), payload);
    payload.as_object_mut().unwrap().remove("closing");
    let absent: SessionStatus = serde_json::from_value(payload.clone()).unwrap();
    assert!(!absent.closing);
    assert_eq!(serde_json::to_value(&absent).unwrap(), payload);
    payload["closing"] = json!(false);
    let hidden: SessionStatus = serde_json::from_value(payload.clone()).unwrap();
    assert!(!hidden.closing);
    payload.as_object_mut().unwrap().remove("closing");
    assert_eq!(serde_json::to_value(&hidden).unwrap(), payload);
}

#[test]
fn the_samples_cover_every_key_each_kind_lists() {
    let mut covered: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for (kind, payload) in samples() {
        let keys = covered.entry(kind).or_default();
        keys.extend(payload.as_object().unwrap().keys().cloned());
    }
    for (kind, (_, keys)) in doc_kinds() {
        let listed: BTreeSet<String> = keys.iter().map(|k| (*k).to_owned()).collect();
        assert_eq!(covered.get(kind), Some(&listed), "{kind}");
    }
}

fn read(kind: &str, payload: Value) -> Result<Option<Event>, serde_json::Error> {
    Event::from_envelope(&line(kind, payload))
}

#[test]
fn an_empty_options_list_reads_as_a_free_text_question_and_writes_without_the_key() {
    let asked = json!([{"header": "h", "question": "q", "options": []}]);
    let event = read(
        "turn_completed",
        json!({"outcome": "completed", "questions": asked}),
    )
    .unwrap()
    .unwrap();
    let Event::TurnCompleted(completed) = &event else {
        panic!("{event:?}");
    };
    assert!(completed.questions.as_ref().unwrap()[0].options.is_empty());
    assert_eq!(
        Value::Object(event.payload().unwrap()),
        json!({"outcome": "completed", "questions": [{"header": "h", "question": "q"}]})
    );
}

#[test]
fn a_reviewer_kept_without_failure_must_not_carry_failed_false() {
    assert!(read("reviewer_kept", json!({"kept":[],"failed":false})).is_err());
}

#[test]
fn an_opening_message_without_sections_reads_and_writes_back_without_the_key() {
    let payload = json!({"environment": {"date": "2026-09-29", "os": "linux",
        "arch": "x86_64", "shell": "bash", "workspace": "/w", "git": {"branch": null},
        "session_log": "/l"}, "instruction_files": [],
        "skills": []});
    let Some(Event::OpeningMessage(message)) = read("opening_message", payload.clone()).unwrap()
    else {
        panic!("not an opening_message");
    };
    assert!(message.extension_sections.is_empty());
    let written = Value::Object(Event::OpeningMessage(message).payload().unwrap());
    assert_eq!(written, payload);
}

#[test]
fn an_opening_message_with_added_folders_reads_and_writes_them_back() {
    let payload = json!({"environment": {"date": "2026-09-29", "os": "linux",
        "arch": "x86_64", "shell": "bash", "workspace": "/w", "git": {"branch": null},
        "session_log": "/l"}, "instruction_files": [],
        "skills": [], "added_directories": ["/shared", "/other"]});
    let Some(Event::OpeningMessage(message)) = read("opening_message", payload.clone()).unwrap()
    else {
        panic!("not an opening_message");
    };
    assert_eq!(message.added_directories, ["/shared", "/other"]);
    let written = Value::Object(Event::OpeningMessage(message).payload().unwrap());
    assert_eq!(written, payload);
}

#[test]
fn a_handoff_note_is_the_actions_or_a_hook_text_never_both() {
    let both = json!({"outcome": "completed", "note": ["a"], "note_text": "n",
        "extension": "e", "tokens_before": 9});
    assert!(read("handoff_completed", both).is_err());
    let hook_without_extension = json!({"outcome": "completed", "note_text": "n",
        "tokens_before": 9});
    assert!(read("handoff_completed", hook_without_extension).is_err());
    let extension_without_hook = json!({"outcome": "completed", "extension": "e",
        "tokens_before": 9});
    assert!(read("handoff_completed", extension_without_hook).is_err());
}

#[test]
fn a_permission_request_carries_only_the_keys_its_step_defines() {
    let base = || json!({"request_id": "r", "effects": [], "reversible": true});
    let rule = json!({"subject": "npm test", "prefix": "npm"});
    let standing = json!({"scope": "project", "prefix": "npm"});
    let escalation = json!({"cause": "session_blocks", "reason": "r"});
    let invalid = [
        ("standing_ask", vec![]),
        (
            "standing_ask",
            vec![("standing_rule", &standing), ("escalation", &escalation)],
        ),
        (
            "standing_ask",
            vec![("standing_rule", &standing), ("rule", &rule)],
        ),
        ("readonly", vec![]),
        ("readonly", vec![("standing_rule", &standing)]),
        ("review", vec![("standing_rule", &standing)]),
    ];
    for (step, keys) in invalid {
        let mut payload = base();
        payload["step"] = json!(step);
        for (key, value) in &keys {
            payload[*key] = (*value).clone();
        }
        assert!(
            read("permission_requested", payload.clone()).is_err(),
            "{payload}"
        );
    }
}

#[test]
fn a_permission_resolved_with_no_effect_keys_reads_as_no_reviewed_call() {
    let payload = json!({"decision": "deny", "decided_by": "reviewer",
        "reason": "r", "reviewer": {"model": "p/m", "stage": 2}});
    let Some(Event::PermissionResolved(resolved)) =
        read("permission_resolved", payload.clone()).unwrap()
    else {
        panic!("not a permission_resolved");
    };
    assert_eq!(resolved.call, None);
    let written = Value::Object(Event::PermissionResolved(resolved).payload().unwrap());
    assert_eq!(written, payload);
}

#[test]
fn a_reviewed_call_writes_always_reviewed_only_when_true() {
    let base = || {
        json!({"decision": "deny", "decided_by": "reviewer",
        "reason": "r", "reviewer": {"model": "p/m", "stage": 1},
        "effects": ["executes"], "reversible": true})
    };
    let Some(Event::PermissionResolved(resolved)) = read("permission_resolved", base()).unwrap()
    else {
        panic!("not a permission_resolved");
    };
    let written = Value::Object(Event::PermissionResolved(resolved).payload().unwrap());
    assert_eq!(written.get("always_reviewed"), None);
    let mut with_true = base();
    with_true["always_reviewed"] = json!(true);
    let Some(Event::PermissionResolved(resolved)) =
        read("permission_resolved", with_true.clone()).unwrap()
    else {
        panic!("not a permission_resolved");
    };
    assert!(resolved.call.as_ref().unwrap().always_reviewed);
    let written = Value::Object(Event::PermissionResolved(resolved).payload().unwrap());
    assert_eq!(written, with_true);
    let mut with_false = base();
    with_false["always_reviewed"] = json!(false);
    let Some(Event::PermissionResolved(resolved)) =
        read("permission_resolved", with_false).unwrap()
    else {
        panic!("not a permission_resolved");
    };
    let written = Value::Object(Event::PermissionResolved(resolved).payload().unwrap());
    assert_eq!(written, base());
}

#[test]
fn a_resolution_with_one_effect_key_reads_as_no_reviewed_call() {
    for payload in [
        json!({"decision": "deny", "decided_by": "reviewer",
            "reason": "r", "reviewer": {"model": "p/m", "stage": 1},
            "effects": []}),
        json!({"decision": "deny", "decided_by": "reviewer",
            "reason": "r", "reviewer": {"model": "p/m", "stage": 1},
            "reversible": false}),
    ] {
        let Some(Event::PermissionResolved(resolved)) =
            read("permission_resolved", payload.clone()).unwrap()
        else {
            panic!("not a permission_resolved: {payload}");
        };
        assert_eq!(resolved.call, None, "{payload}");
    }
}

#[test]
fn a_resolution_with_empty_effects_and_reversible_reads_as_a_reviewed_call() {
    let payload = json!({"decision": "deny", "decided_by": "reviewer",
        "reason": "r", "reviewer": {"model": "p/m", "stage": 1},
        "effects": [], "reversible": true});
    let Some(Event::PermissionResolved(resolved)) =
        read("permission_resolved", payload.clone()).unwrap()
    else {
        panic!("not a permission_resolved");
    };
    let call = resolved.call.as_ref().unwrap();
    assert!(call.declared.effects.is_empty());
    let written = Value::Object(Event::PermissionResolved(resolved).payload().unwrap());
    assert_eq!(written, payload);
}

#[test]
fn an_inner_call_names_exactly_one_anchor() {
    for ran_by in [
        json!({"extension": "e"}),
        json!({"extension": "e", "outer_action_id": "a_1", "command_id": "c_1"}),
    ] {
        let payload = json!({"name": "read", "arguments": {}, "ran_by": ran_by});
        assert!(
            read("tool_call_requested", payload.clone()).is_err(),
            "{payload}"
        );
    }
}

#[test]
fn repaired_and_repairs_come_together_or_not_at_all() {
    let base = || json!({"name": "read", "arguments": {"limit": "5"}});
    let repaired = json!({"limit": 5});
    let repairs = json!([{"path": "/limit", "fix": "string_to_number"}]);
    for (key, value) in [("repaired", &repaired), ("repairs", &repairs)] {
        let mut payload = base();
        payload[key] = value.clone();
        assert!(
            read("tool_call_requested", payload.clone()).is_err(),
            "{payload}"
        );
    }
}

#[test]
fn resumes_reads_from_a_line_and_is_written_only_when_true() {
    let fields = json!([{"header": "h", "question": "q"}]);
    let resumed = json!({"request_id": "r", "kind": "form", "fields": fields, "resumes": true});
    let Some(Event::InteractionRequested(requested)) =
        read("interaction_requested", resumed).unwrap()
    else {
        panic!("not an interaction request");
    };
    assert!(requested.resumes);
    let plain = json!({"request_id": "r", "kind": "form", "fields": fields});
    let event = read("interaction_requested", plain.clone())
        .unwrap()
        .unwrap();
    let Event::InteractionRequested(requested) = &event else {
        panic!("{event:?}");
    };
    assert!(!requested.resumes);
    assert_eq!(Value::Object(event.payload().unwrap()), plain);
}

#[test]
fn an_interaction_request_carries_only_the_keys_its_kind_defines() {
    let options = json!([{"label": "a"}]);
    let fields = json!([{"header": "h", "question": "q", "options": []}]);
    let prompt = json!("p");
    let invalid = [
        ("confirm", vec![]),
        ("confirm", vec![("prompt", &prompt), ("options", &options)]),
        ("confirm", vec![("prompt", &prompt), ("fields", &fields)]),
        ("select", vec![("prompt", &prompt)]),
        ("select", vec![("options", &options)]),
        (
            "multi_select",
            vec![
                ("prompt", &prompt),
                ("options", &options),
                ("fields", &fields),
            ],
        ),
        (
            "text_input",
            vec![("prompt", &prompt), ("options", &options)],
        ),
        ("form", vec![]),
        ("form", vec![("fields", &fields), ("prompt", &prompt)]),
        ("form", vec![("fields", &fields), ("options", &options)]),
    ];
    for (kind, keys) in invalid {
        let mut payload = json!({"request_id": "r", "kind": kind});
        for (key, value) in &keys {
            payload[*key] = (*value).clone();
        }
        assert!(
            read("interaction_requested", payload.clone()).is_err(),
            "{payload}"
        );
    }
}

#[test]
fn a_required_key_that_may_be_null_must_be_present() {
    let tokens = json!({"input": 1, "cache_read": 0, "cache_write": {}, "output": 1});
    let cases = [
        ("session_named", json!({"by": "person"}), "name"),
        (
            "usage_recorded",
            json!({"generation_id": "g", "model": "p/m", "tokens": tokens, "input_bytes": 1}),
            "cost",
        ),
    ];
    for (kind, mut payload, key) in cases {
        assert!(read(kind, payload.clone()).is_err(), "{kind} without {key}");
        payload[key] = Value::Null;
        assert!(
            read(kind, payload).unwrap().is_some(),
            "{kind} with null {key}"
        );
    }
    let finished = |usage: Value| json!({"job_id": "j", "text": "t", "usage": usage});
    assert!(
        read(
            "delegate_finished",
            finished(json!({"tokens": tokens, "subscription_cost": 0.0}))
        )
        .is_err()
    );
    let with_null = finished(json!({"tokens": tokens, "cost": null, "subscription_cost": 0.0}));
    assert!(read("delegate_finished", with_null).unwrap().is_some());
    let opening = |git: Value| {
        json!({"environment": {"date": "d", "os": "o", "arch": "a", "shell": "s",
            "workspace": "/w", "git": git, "session_log": "/l"},
            "instruction_files": [], "skills": []})
    };
    assert!(read("opening_message", opening(json!({}))).is_err());
    assert!(
        read("opening_message", opening(json!({"branch": null})))
            .unwrap()
            .is_some()
    );
}

#[test]
fn an_empty_notice_is_written_and_absent_keys_stay_absent() {
    let empty = read(
        "job_completed",
        json!({"job_id": "j",
        "status": "failed", "notice": ""}),
    )
    .unwrap()
    .unwrap();
    let written = Value::Object(empty.payload().unwrap());
    assert_eq!(written.get("notice"), Some(&json!("")));
    assert!(!written.as_object().unwrap().contains_key("output_tail"));
    assert!(!written.as_object().unwrap().contains_key("changed_by"));

    let plain = read(
        "job_completed",
        json!({"job_id": "j",
        "status": "failed", "output_tail": "tail"}),
    )
    .unwrap()
    .unwrap();
    let written = Value::Object(plain.payload().unwrap());
    assert!(!written.as_object().unwrap().contains_key("notice"));
    assert!(!written.as_object().unwrap().contains_key("changed_by"));
}

#[test]
fn a_usage_without_input_bytes_does_not_read() {
    let tokens = json!({"input": 1, "cache_read": 0, "cache_write": {}, "output": 1});
    let without = json!({"generation_id": "g", "model": "p/m", "tokens": tokens, "cost": null});
    assert!(read("usage_recorded", without.clone()).is_err());
    let mut with = without;
    with["input_bytes"] = json!(1);
    assert!(read("usage_recorded", with).unwrap().is_some());
}

#[test]
fn a_reviewer_use_round_trips_and_is_absent_otherwise() {
    let tokens = json!({"input": 1, "cache_read": 0, "cache_write": {}, "output": 1});
    let base = || {
        json!({"generation_id": "g", "model": "p/m", "tokens": tokens,
        "input_bytes": 1, "cost": null})
    };
    let recorded = |payload: Value| match read("usage_recorded", payload).unwrap() {
        Some(Event::UsageRecorded(recorded)) => recorded,
        other => panic!("{other:?}"),
    };
    // A stage call names the call it reviewed.
    let mut staged = base();
    staged["reviewer"] = json!({"purpose": "stage_1", "action_id": "a_1"});
    let line = recorded(staged.clone());
    assert_eq!(
        line.reviewer,
        Some(ReviewerUse::Stage1 {
            action_id: crate::ActionId("a_1".into()),
        })
    );
    assert_eq!(
        Value::Object(Event::UsageRecorded(line).payload().unwrap()),
        staged
    );
    // A handoff selection names no call.
    let mut handoff = base();
    handoff["reviewer"] = json!({"purpose": "handoff"});
    let line = recorded(handoff.clone());
    assert_eq!(line.reviewer, Some(ReviewerUse::Handoff));
    assert_eq!(
        Value::Object(Event::UsageRecorded(line).payload().unwrap()),
        handoff
    );
    // Any other call carries none, and writes no key.
    let line = recorded(base());
    assert_eq!(line.reviewer, None);
    let written = Value::Object(Event::UsageRecorded(line).payload().unwrap());
    assert!(written.get("reviewer").is_none(), "{written}");
}

#[test]
fn a_reviewer_use_names_its_call_on_a_stage_and_none_on_a_handoff() {
    let tokens = json!({"input": 1, "cache_read": 0, "cache_write": {}, "output": 1});
    let base = || {
        json!({"generation_id": "g", "model": "p/m", "tokens": tokens,
        "input_bytes": 1, "cost": null})
    };
    // A stage without the call it reviewed does not read: the action id
    // is part of the variant, not an optional side field.
    let mut staged = base();
    staged["reviewer"] = json!({"purpose": "stage_1"});
    assert!(read("usage_recorded", staged).is_err());
    // A handoff selection reviews no call, so an action id on one is not
    // a field of the variant: serde's default ignores it, and the line
    // reads as a plain handoff and writes back without the key.
    let mut handoff = base();
    handoff["reviewer"] = json!({"purpose": "handoff", "action_id": "a_1"});
    let event = read("usage_recorded", handoff).unwrap().unwrap();
    let Event::UsageRecorded(line) = &event else {
        panic!("{event:?}");
    };
    assert_eq!(line.reviewer, Some(ReviewerUse::Handoff));
    assert_eq!(
        Value::Object(event.payload().unwrap())["reviewer"],
        json!({"purpose": "handoff"})
    );
}

#[test]
fn declined_and_skipped_are_only_ever_true() {
    let declined = json!({"request_id": "r", "by": "fiber", "declined": false});
    assert!(read("interaction_resolved", declined).is_err());
    let skipped = json!({"request_id": "r", "by": "person", "answers": [{"skipped": false}]});
    assert!(read("interaction_resolved", skipped).is_err());
}

#[test]
fn fibers_own_message_has_source_fiber_and_no_command_id() {
    let sender = crate::shapes::Sender {
        origin: crate::shapes::Origin::Fiber,
        command_id: None,
    };
    let value = serde_json::to_value(&sender).unwrap();
    assert_eq!(value, json!({"source": "fiber"}));
    assert_eq!(
        serde_json::from_value::<crate::shapes::Sender>(value).unwrap(),
        sender
    );
}

#[test]
fn provider_item_is_read_from_a_hosted_call_and_absent_from_an_ordinary_one() {
    let item = json!({"type": "server_tool_use", "id": "srvtoolu_01"});
    let call = |payload: Value| match read("tool_call_requested", payload).unwrap() {
        Some(Event::ToolCallRequested(call)) => call,
        other => panic!("{other:?}"),
    };
    let hosted = call(json!({"name": "web_search", "arguments": {}, "provider_item": item}));
    assert_eq!(hosted.provider_item, Some(item));
    let ordinary = call(json!({"name": "read", "arguments": {}}));
    assert_eq!(ordinary.provider_item, None);
    let written = serde_json::to_value(&ordinary).unwrap();
    assert!(written.get("provider_item").is_none(), "{written}");

    let result = |payload: Value| match read("tool_call_completed", payload).unwrap() {
        Some(Event::ToolCallCompleted(done)) => done,
        other => panic!("{other:?}"),
    };
    let block = json!({"type": "web_search_tool_result"});
    let hosted = result(json!({"status": "completed", "content": [], "provider_item": block}));
    assert_eq!(hosted.provider_item, Some(block));
    let ordinary = result(json!({"status": "completed", "content": []}));
    assert_eq!(ordinary.provider_item, None);
    let written = serde_json::to_value(&ordinary).unwrap();
    assert!(written.get("provider_item").is_none(), "{written}");
}

/// The tools a search loaded, on `tool_call_completed`: `None` writes no
/// key, `Some` keeps the tool's order, and a line without the key reads
/// `None`.
#[test]
fn loaded_reads_from_a_search_result_and_is_absent_from_any_other() {
    let result = |payload: Value| match read("tool_call_completed", payload).unwrap() {
        Some(Event::ToolCallCompleted(done)) => done,
        other => panic!("{other:?}"),
    };
    let none = result(json!({"status": "completed", "content": []}));
    assert_eq!(none.loaded, None);
    let written = serde_json::to_value(&none).unwrap();
    assert!(written.get("loaded").is_none(), "{written}");
    let empty = result(json!({"status": "completed", "content": [], "loaded": []}));
    assert_eq!(empty.loaded, Some(vec![]));
    assert_eq!(
        serde_json::to_value(&empty).unwrap(),
        json!({"status": "completed", "content": [], "loaded": []})
    );
    let ordered = result(json!({"status": "completed", "content": [], "loaded": ["b", "a"]}));
    assert_eq!(ordered.loaded, Some(vec!["b".to_owned(), "a".to_owned()]));
    assert_eq!(
        serde_json::to_value(&ordered).unwrap(),
        json!({"status": "completed", "content": [], "loaded": ["b", "a"]})
    );
}

#[test]
fn kept_reads_from_a_cut_result_and_is_absent_from_an_uncut_one() {
    let result = |payload: Value| match read("tool_call_completed", payload).unwrap() {
        Some(Event::ToolCallCompleted(done)) => done,
        other => panic!("{other:?}"),
    };
    let cut = result(json!({"status": "completed", "content": [],
        "kept": [[0, 8192], [1040384, 1048576]]}));
    assert_eq!(cut.kept, Some(vec![[0, 8192], [1040384, 1048576]]));
    let uncut = result(json!({"status": "completed", "content": []}));
    assert_eq!(uncut.kept, None);
    let written = serde_json::to_value(&uncut).unwrap();
    assert!(written.get("kept").is_none(), "{written}");
    let empty = result(json!({"status": "completed", "content": [], "kept": []}));
    assert_eq!(empty.kept, Some(vec![]));
    for kept in [json!([[0, 1, 2]]), json!([[-1, 4]]), json!([[0, null]])] {
        assert!(
            read(
                "tool_call_completed",
                json!({"status": "completed",
            "content": [], "kept": kept})
            )
            .is_err(),
            "{kept}"
        );
    }
}

/// The variant a `command_accepted` sample's `result` reads as.
fn result_of(payload: Value) -> CommandResult {
    let Some(Event::CommandAccepted(accepted)) = read("command_accepted", payload).unwrap() else {
        panic!("not a command_accepted");
    };
    accepted.result.expect("a result")
}

#[test]
fn an_mcp_servers_result_reads_its_rows_and_omits_absent_keys() {
    let result = result_of(json!({"command_id": "c_1", "result": {"servers": [
        {"name": "linear", "layer": "repository", "state": "needs-login",
         "reason": "connection refused Run `fiber mcp login linear` to log in.",
         "tools": 3, "approved": false,
         "auth_url": "http://127.0.0.1:9/cb?code=a"},
        {"name": "up", "layer": "global", "state": "ready",
         "reason": "", "tools": 1, "approved": true},
        {"name": "new", "layer": "project", "state": "connecting",
         "reason": "", "approved": true}]} }));
    assert_eq!(
        result,
        CommandResult::McpServers {
            servers: vec![
                McpServerRow {
                    name: "linear".into(),
                    layer: McpServerLayer::Repository,
                    state: McpServerState::NeedsLogin,
                    reason: "connection refused Run `fiber mcp login linear` to log in.".into(),
                    tools: Some(3),
                    approved: false,
                    auth_url: Some("http://127.0.0.1:9/cb?code=a".into()),
                },
                McpServerRow {
                    name: "up".into(),
                    layer: McpServerLayer::Global,
                    state: McpServerState::Ready,
                    reason: String::new(),
                    tools: Some(1),
                    approved: true,
                    auth_url: None,
                },
                McpServerRow {
                    name: "new".into(),
                    layer: McpServerLayer::Project,
                    state: McpServerState::Connecting,
                    reason: String::new(),
                    tools: None,
                    approved: true,
                    auth_url: None,
                },
            ]
        }
    );
    let written = serde_json::to_value(&result).unwrap();
    let rows = written["servers"].as_array().unwrap();
    assert!(rows[1].get("auth_url").is_none(), "{written}");
    assert!(rows[2].get("tools").is_none(), "{written}");
    assert!(rows[2].get("auth_url").is_none(), "{written}");
    assert_eq!(
        result_of(json!({"command_id": "c_1", "result": {"servers": []}})),
        CommandResult::McpServers { servers: vec![] }
    );
    for servers in [
        json!([{"name": "x", "layer": "home", "state": "ready",
            "reason": "", "approved": true}]),
        json!([{"name": "x", "layer": "global", "state": "offline",
            "reason": "", "approved": true}]),
        json!([{"name": "x", "layer": "global", "state": "ready",
            "reason": "", "approved": true, "future": 1}]),
    ] {
        assert!(
            read(
                "command_accepted",
                json!({"command_id": "c_1", "result": {"servers": servers}})
            )
            .is_err(),
            "{servers}"
        );
    }
}

#[test]
fn a_subscribe_result_names_the_last_fold_line() {
    assert_eq!(
        result_of(json!({"command_id": "c", "result": {"last_seq": 41}})),
        CommandResult::Subscribe {
            last_seq: crate::Seq(41)
        }
    );
    let Some(Event::CommandAccepted(accepted)) =
        read("command_accepted", json!({"command_id": "c"})).unwrap()
    else {
        panic!("not an acceptance");
    };
    assert_eq!(accepted.result, None);
}

#[test]
fn an_artifact_page_result_reads_with_and_without_next() {
    assert_eq!(
        result_of(json!({"command_id": "c", "result": {
            "text": "a\nb\n", "lines": [0, 2], "next": 4, "size": 9000}})),
        CommandResult::ArtifactPage {
            text: "a\nb\n".into(),
            lines: vec![0, 2],
            next: Some(4),
            size: 9000
        }
    );
    let last = result_of(
        json!({"command_id": "c", "result": {"text": "z\n", "lines": [4], "size": 9000}}),
    );
    assert_eq!(
        last,
        CommandResult::ArtifactPage {
            text: "z\n".into(),
            lines: vec![4],
            next: None,
            size: 9000
        }
    );
    let written = serde_json::to_value(&last).unwrap();
    assert_eq!(written, json!({"text": "z\n", "lines": [4], "size": 9000}));
    assert!(written.get("next").is_none(), "{written}");
}

#[test]
fn a_commands_result_reads_as_commands_with_and_without_a_hint() {
    let result = result_of(json!({"command_id": "c_1", "result": {"commands": [
        {"name": "review", "description": "Review a diff.", "argument_hint": "[base]",
         "tag": "template"},
        {"name": "tdd", "description": "Test first.", "tag": "skill"}]}}));
    assert_eq!(
        result,
        CommandResult::Commands {
            commands: vec![
                CommandInfo {
                    name: "review".into(),
                    description: "Review a diff.".into(),
                    argument_hint: Some("[base]".into()),
                    tag: "template".into(),
                    extension: false,
                },
                CommandInfo {
                    name: "tdd".into(),
                    description: "Test first.".into(),
                    argument_hint: None,
                    tag: "skill".into(),
                    extension: false,
                },
            ]
        }
    );
    let accepted = CommandAccepted {
        command_id: CommandId("c_1".into()),
        result: Some(result),
    };
    assert_eq!(
        serde_json::to_string(&accepted).unwrap(),
        r#"{"command_id":"c_1","result":{"commands":[{"name":"review","description":"Review a diff.","argument_hint":"[base]","tag":"template"},{"name":"tdd","description":"Test first.","tag":"skill"}]}}"#
    );
}

#[test]
fn an_extension_command_result_marks_extension() {
    let result = result_of(json!({"command_id": "c_1", "result": {"commands": [
        {"name": "deploy", "description": "Deploys.", "tag": "acme", "extension": true}]}}));
    assert_eq!(
        result,
        CommandResult::Commands {
            commands: vec![CommandInfo {
                name: "deploy".into(),
                description: "Deploys.".into(),
                argument_hint: None,
                tag: "acme".into(),
                extension: true,
            }]
        }
    );
    let written = serde_json::to_value(&result).unwrap();
    assert_eq!(written["commands"][0]["extension"], json!(true));
    let plain = result_of(json!({"command_id": "c_1", "result": {"commands": [
        {"name": "tdd", "description": "Test first.", "tag": "skill"}]}}));
    let written = serde_json::to_value(&plain).unwrap();
    assert!(
        written["commands"][0].get("extension").is_none(),
        "{written}"
    );
}

#[test]
fn an_empty_commands_result_reads_as_commands() {
    assert_eq!(
        result_of(json!({"command_id": "c", "result": {"commands": []}})),
        CommandResult::Commands { commands: vec![] }
    );
}

#[test]
fn a_skills_result_reads_and_writes_every_key() {
    let result = result_of(json!({"command_id": "c_1", "result": {"skills": [
        {"name": "review", "description": "Repository review.",
         "path": "/w/.agents/skills/review/SKILL.md", "source": "repository",
         "extension": null, "model_invocable": true, "disabled": false,
         "shadows": ["/h/skills/review/SKILL.md"]},
        {"name": "tidy", "description": "Tidies.",
         "path": "/h/extensions/acme/skills/tidy/SKILL.md", "source": "extension",
         "extension": "acme", "model_invocable": true, "disabled": true,
         "shadows": [], "shadowed_by": "/w/skills/tidy/SKILL.md"}]}}));
    assert_eq!(
        result,
        CommandResult::Skills {
            skills: vec![
                SkillInfo {
                    name: "review".into(),
                    description: "Repository review.".into(),
                    path: "/w/.agents/skills/review/SKILL.md".into(),
                    source: SkillSource::Repository,
                    extension: None,
                    model_invocable: true,
                    disabled: false,
                    shadows: vec!["/h/skills/review/SKILL.md".into()],
                    shadowed_by: None,
                },
                SkillInfo {
                    name: "tidy".into(),
                    description: "Tidies.".into(),
                    path: "/h/extensions/acme/skills/tidy/SKILL.md".into(),
                    source: SkillSource::Extension,
                    extension: Some("acme".into()),
                    model_invocable: true,
                    disabled: true,
                    shadows: vec![],
                    shadowed_by: Some("/w/skills/tidy/SKILL.md".into()),
                },
            ]
        }
    );
    // An optional key is absent, never null: `null` reads as missing.
    let accepted = CommandAccepted {
        command_id: CommandId("c_1".into()),
        result: Some(result),
    };
    assert_eq!(
        serde_json::to_string(&accepted).unwrap(),
        r#"{"command_id":"c_1","result":{"skills":[{"name":"review","description":"Repository review.","path":"/w/.agents/skills/review/SKILL.md","source":"repository","model_invocable":true,"disabled":false,"shadows":["/h/skills/review/SKILL.md"]},{"name":"tidy","description":"Tidies.","path":"/h/extensions/acme/skills/tidy/SKILL.md","source":"extension","extension":"acme","model_invocable":true,"disabled":true,"shadows":[],"shadowed_by":"/w/skills/tidy/SKILL.md"}]}}"#
    );
}

#[test]
fn a_skills_result_leaves_out_absent_keys() {
    let written = serde_json::to_value(&CommandAccepted {
        command_id: CommandId("c".into()),
        result: Some(CommandResult::Skills {
            skills: vec![SkillInfo {
                name: "t".into(),
                description: "Tidies.".into(),
                path: "/p/SKILL.md".into(),
                source: SkillSource::Personal,
                extension: None,
                model_invocable: false,
                disabled: false,
                shadows: vec![],
                shadowed_by: None,
            }],
        }),
    })
    .unwrap();
    assert_eq!(
        written,
        json!({"command_id": "c", "result": {"skills": [
            {"name": "t", "description": "Tidies.", "path": "/p/SKILL.md",
             "source": "personal", "model_invocable": false, "disabled": false,
             "shadows": []}]}})
    );
    assert!(written["result"]["skills"][0].get("extension").is_none());
    assert!(written["result"]["skills"][0].get("shadowed_by").is_none());
}

#[test]
fn a_placeholder_env_result_round_trips_present_names_only() {
    let result = CommandResult::PlaceholderEnv {
        values: [(
            "DATABRICKS_HOST".to_owned(),
            "adb-1.example.test".to_owned(),
        )]
        .into_iter()
        .collect(),
    };
    let wire = json!({"values": {"DATABRICKS_HOST": "adb-1.example.test"}});
    assert_eq!(serde_json::to_value(&result).unwrap(), wire);
    assert_eq!(result_of(json!({"command_id":"c","result":wire})), result);
    // Absent names are omitted, never `null`.
    assert_eq!(
        result_of(json!({"command_id":"c","result":{"values":{}}})),
        CommandResult::PlaceholderEnv {
            values: BTreeMap::new()
        }
    );
}

#[test]
fn an_empty_skills_result_reads_as_skills() {
    assert_eq!(
        result_of(json!({"command_id": "c", "result": {"skills": []}})),
        CommandResult::Skills { skills: vec![] }
    );
}

#[test]
fn notes_results_write_and_read_evidence_proposals_and_optional_error() {
    use crate::notes::{Evidence, Proposed};
    let result = CommandResult::ProposeNotes {
        evidence: vec![Evidence::Files {
            text: "remote".into(),
        }],
        proposed: vec![Proposed::new("ours".into(), vec![0]).unwrap()],
        error: None,
    };
    let wire = json!({"evidence":[{"kind":"files","text":"remote"}],"proposed":[{"text":"ours","cites":[0]}]});
    assert_eq!(serde_json::to_value(&result).unwrap(), wire);
    assert_eq!(result_of(json!({"command_id":"c","result":wire})), result);
    for evidence in [
        vec![],
        vec![Evidence::Answer {
            text: "answer".into(),
        }],
    ] {
        let result = CommandResult::ProposeNotes {
            evidence: evidence.clone(),
            proposed: Vec::new(),
            error: Some(Box::new(crate::shapes::Failure {
                code: crate::ErrorCode::BudgetExceeded,
                message: "spent".into(),
                retry_after_ms: None,
                provider: None,
            })),
        };
        let wire = json!({"evidence":evidence,"proposed":[],"error":{"code":"budget_exceeded","message":"spent"}});
        assert_eq!(serde_json::to_value(&result).unwrap(), wire);
        assert_eq!(result_of(json!({"command_id":"c","result":wire})), result);
    }
    let empty = json!({"evidence":[],"proposed":[]});
    assert!(matches!(
        result_of(json!({"command_id":"c","result":empty})),
        CommandResult::ProposeNotes { .. }
    ));
}

#[test]
fn every_other_result_still_reads_as_its_own_variant() {
    assert!(matches!(
        result_of(json!({"command_id":"c","result":{"evidence":[],"proposed":[]}})),
        CommandResult::ProposeNotes { .. }
    ));
    assert_eq!(
        result_of(json!({"command_id": "c", "result": {"text": "t"}})),
        CommandResult::OfferedFile { text: "t".into() }
    );
    assert!(matches!(
        result_of(json!({"command_id": "c", "result": {"text": "t", "lines": [0], "size": 1}})),
        CommandResult::ArtifactPage { .. }
    ));
    assert!(matches!(
        result_of(json!({"command_id": "c", "result": {"provider": "fake", "credentials": []}})),
        CommandResult::Credentials(_)
    ));
    assert!(matches!(
        result_of(json!({"command_id": "c", "result": {"provider": "other"}})),
        CommandResult::CredentialSwitch { .. }
    ));
    assert!(matches!(
        result_of(json!({"command_id": "c", "result": {"quota": [], "notice_at": 80.0}})),
        CommandResult::Quota { .. }
    ));
    assert!(matches!(
        result_of(json!({"command_id": "c", "result": {"tools": []}})),
        CommandResult::Tools { .. }
    ));
    assert!(matches!(
        result_of(json!({"command_id": "c", "result": {"commands": []}})),
        CommandResult::Commands { .. }
    ));
    assert!(matches!(
        result_of(json!({"command_id": "c", "result": {"lines": []}})),
        CommandResult::History { .. }
    ));
    assert!(matches!(
        result_of(json!({"command_id": "c", "result": {"new_session_id": "s2"}})),
        CommandResult::Rewind { .. }
    ));
    let process = json!({"exit_code": 0, "signal": null, "timed_out": false});
    assert!(matches!(
        result_of(json!({"command_id": "c", "result": {"output": "o", "process": process}})),
        CommandResult::Shell { .. }
    ));
}

fn credential_labels() -> CredentialLabels {
    CredentialLabels {
        provider: "fake".into(),
        credentials: vec!["default".into(), "home".into(), "work".into()],
        credential: Some("home".into()),
    }
}

#[test]
fn a_credentials_result_writes_provider_labels_and_label() {
    let accepted = CommandAccepted {
        command_id: CommandId("c".into()),
        result: Some(CommandResult::Credentials(credential_labels())),
    };
    assert_eq!(
        serde_json::to_value(&accepted).unwrap(),
        json!({"command_id":"c", "result": {
            "provider":"fake", "credentials":["default","home","work"], "credential":"home"
        }})
    );
}

#[test]
fn a_scripted_credentials_result_leaves_out_the_label() {
    let result = CommandResult::Credentials(CredentialLabels {
        provider: "scripted".into(),
        credentials: vec![],
        credential: None,
    });
    let written = serde_json::to_value(&result).unwrap();
    assert_eq!(written, json!({"provider":"scripted", "credentials":[]}));
    assert!(written.get("credential").is_none());
    assert_eq!(
        serde_json::from_value::<CommandResult>(written).unwrap(),
        result
    );
}

#[test]
fn a_credentials_result_reads_as_credentials() {
    assert_eq!(
        result_of(json!({"command_id":"c", "result": {
            "provider":"fake", "credentials":["default","home","work"], "credential":"home"
        }})),
        CommandResult::Credentials(credential_labels())
    );
}

#[test]
fn a_credential_switch_result_writes_and_reads_only_its_provider() {
    let result = CommandResult::CredentialSwitch {
        provider: "other".into(),
    };
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({"provider":"other"})
    );
    assert_eq!(
        result_of(json!({"command_id":"c", "result":{"provider":"other"}})),
        result
    );
}

#[test]
fn a_sessions_stopped_result_round_trips_as_stop() {
    let result = CommandResult::Stop {
        sessions_stopped: 3,
    };
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({"sessions_stopped": 3})
    );
    assert_eq!(
        result_of(json!({"command_id": "c", "result": {"sessions_stopped": 3}})),
        result
    );
}

#[test]
fn a_session_id_result_reads_as_start_and_a_status_object_as_status() {
    assert_eq!(
        result_of(json!({"command_id": "c", "result": {"session_id": "s2"}})),
        CommandResult::Start {
            session_id: SessionId("s2".into())
        }
    );
    assert_eq!(
        result_of(
            json!({"command_id": "c", "result": {"clients": 2, "fiber_version": "0.0.0", "running": true, "variables": {"path": "/usr/bin:/bin", "names": ["HOME"], "source": "inherited"}}})
        ),
        CommandResult::Status {
            running: true,
            fiber_version: "0.0.0".into(),
            clients: 2,
            variables: crate::events::Variables {
                path: "/usr/bin:/bin".into(),
                names: vec!["HOME".into()],
                source: crate::events::VariablesSource::Inherited,
            },
        }
    );
    assert!(matches!(
        result_of(json!({"command_id": "c", "result": {"new_session_id": "s2"}})),
        CommandResult::Rewind { .. }
    ));
}

#[test]
fn an_mcp_tool_names_its_server_and_its_own_name() {
    let payload = json!({"name": "mcp__m__x", "source": "mcp", "server": "m",
        "tool": "x", "state": "deferred", "bytes": 1});
    let info: ToolInfo = serde_json::from_value(payload.clone()).unwrap();
    assert_eq!(
        info.source,
        ToolSource::Mcp {
            server: "m".into(),
            tool: "x".into()
        }
    );
    assert_eq!(serde_json::to_value(&info).unwrap(), payload);
}

#[test]
fn an_old_offered_item_without_files_reads_as_empty() {
    let item: OfferedItem = serde_json::from_value(
        json!({"kind": "hook", "name": "h", "hash": "h", "required": false, "summary": "s"}),
    )
    .unwrap();
    assert!(item.files.is_empty());
    assert_eq!(serde_json::to_value(item).unwrap()["files"], json!([]));
}

#[test]
fn a_waiting_offer_reads_as_its_kind() {
    let (_, payload) = samples()
        .into_iter()
        .find(|(kind, payload)| *kind == "session_status" && payload["waiting"]["kind"] == "offer")
        .unwrap();
    let Some(Event::SessionStatus(status)) = read("session_status", payload).unwrap() else {
        panic!("not a session_status");
    };
    let SessionState::Waiting { waiting } = status.state else {
        panic!("not waiting");
    };
    assert_eq!(waiting.kind, WaitingKind::Offer);
    assert_eq!(waiting.summary, "3 items from the repository");
}

#[test]
fn an_old_rewound_line_with_jobs_still_reads() {
    let Some(Event::Rewound(rewound)) = read(
        "rewound",
        json!({"new_session_id": "s2", "seq": 4, "jobs": []}),
    )
    .unwrap() else {
        panic!("not a rewound");
    };
    assert_eq!(rewound.new_session_id, SessionId("s2".into()));
    let Some(Event::SessionStarted(_)) = read(
        "session_started",
        json!({"workspace": "/w",
        "variables": {"path": "/usr/bin:/bin", "names": ["HOME"], "source": "login_shell"},
        "rewind": {"note": "n", "jobs": ["j"]}}),
    )
    .unwrap() else {
        panic!("not a session_started");
    };
}

#[test]
fn a_session_started_omits_supervised_when_false_and_round_trips_when_true() {
    let base = json!({"workspace": "/w",
        "variables": {"path": "/usr/bin:/bin", "names": ["HOME"], "source": "login_shell"}});
    let Some(Event::SessionStarted(absent)) = read("session_started", base.clone()).unwrap() else {
        panic!("not a session_started");
    };
    assert!(!absent.supervised);
    assert_eq!(
        serde_json::to_value(&absent).unwrap(),
        base,
        "absent supervised writes back absent"
    );
    let mut explicit_false = base.clone();
    explicit_false["supervised"] = json!(false);
    let Some(Event::SessionStarted(hidden)) = read("session_started", explicit_false).unwrap()
    else {
        panic!("not a session_started");
    };
    assert!(!hidden.supervised);
    assert_eq!(
        serde_json::to_value(&hidden).unwrap(),
        base,
        "explicit false writes back absent"
    );
    let mut shown = base.clone();
    shown["supervised"] = json!(true);
    let Some(Event::SessionStarted(present)) = read("session_started", shown.clone()).unwrap()
    else {
        panic!("not a session_started");
    };
    assert!(present.supervised);
    assert_eq!(serde_json::to_value(&present).unwrap(), shown);
}

#[test]
fn a_session_status_omits_supervised_when_false_and_round_trips_when_true() {
    let (_, base) = samples()
        .into_iter()
        .find(|(kind, payload)| {
            *kind == "session_status" && !payload.as_object().unwrap().contains_key("supervised")
        })
        .unwrap();
    let Some(Event::SessionStatus(absent)) = read("session_status", base.clone()).unwrap() else {
        panic!("not a session_status");
    };
    assert!(!absent.supervised);
    assert_eq!(
        serde_json::to_value(&absent).unwrap(),
        base,
        "absent supervised writes back absent"
    );
    let mut explicit_false = base.clone();
    explicit_false["supervised"] = json!(false);
    let Some(Event::SessionStatus(hidden)) = read("session_status", explicit_false).unwrap() else {
        panic!("not a session_status");
    };
    assert!(!hidden.supervised);
    assert_eq!(
        serde_json::to_value(&hidden).unwrap(),
        base,
        "explicit false writes back absent"
    );
    let mut shown = base.clone();
    shown["supervised"] = json!(true);
    let Some(Event::SessionStatus(present)) = read("session_status", shown.clone()).unwrap() else {
        panic!("not a session_status");
    };
    assert!(present.supervised);
    assert_eq!(serde_json::to_value(&present).unwrap(), shown);
}
