//! The `provider_request` line's `data` shape (`docs/state.md`, "Diagnostic
//! logs"): fields in declared order, absent options left out.

#![allow(clippy::unwrap_used, reason = "test code")]

use super::*;
use std::collections::BTreeMap;

fn full() -> ProviderRequest {
    ProviderRequest {
        provider: "fake".into(),
        model: Some("m".into()),
        purpose: Purpose::ModelCall,
        host: Some("127.0.0.1:50731".into()),
        path: Some("/v1/responses".into()),
        status: Some(200),
        attempt: 1,
        request_bytes: 2214,
        response_bytes: 913,
        headers_ms: Some(41),
        first_token_ms: Some(57),
        total_ms: 180,
        prefix_hashes: Some(PrefixHashes {
            system: Some("9f2c".into()),
            tools: Some("41ab".into()),
            messages: vec!["c7d0".into(), "e91f".into()],
        }),
        cache_read: Some(9585),
        cache_write: Some(BTreeMap::from([("1h".into(), 57)])),
    }
}

#[test]
fn every_purpose_serializes_to_its_snake_case_name() {
    for (purpose, want) in [
        (Purpose::ModelCall, "model_call"),
        (Purpose::ModelList, "model_list"),
        (Purpose::Quota, "quota"),
        (Purpose::TokenRefresh, "token_refresh"),
        (Purpose::Cost, "cost"),
    ] {
        assert_eq!(
            serde_json::to_string(&purpose).unwrap(),
            format!("\"{want}\""),
            "{purpose:?}"
        );
    }
}

#[test]
fn an_absent_option_is_left_out_never_null() {
    let request = ProviderRequest {
        provider: "fake".into(),
        model: None,
        purpose: Purpose::Quota,
        host: None,
        path: None,
        status: None,
        attempt: 1,
        request_bytes: 0,
        response_bytes: 0,
        headers_ms: None,
        first_token_ms: None,
        total_ms: 3,
        prefix_hashes: None,
        cache_read: None,
        cache_write: None,
    };
    let line = serde_json::to_string(&request).unwrap();
    assert_eq!(
        line,
        "{\"provider\":\"fake\",\"purpose\":\"quota\",\"attempt\":1,\
         \"request_bytes\":0,\"response_bytes\":0,\"total_ms\":3}"
    );
    assert!(!line.contains("null"), "{line}");
    for absent in ["prefix_hashes", "cache_read", "cache_write"] {
        assert!(!line.contains(absent), "{absent} is absent: {line}");
    }
}

#[test]
fn every_operation_serializes_to_its_wire_name() {
    use HookChain::*;
    for (op, want) in [
        (Op::StartupConfig, "startup.config"),
        (Op::StartupExtensions, "startup.extensions"),
        (Op::StartupPreamble, "startup.preamble"),
        (Op::StartupSocket, "startup.socket"),
        (Op::StartupRouting, "startup.routing"),
        (Op::RoutingSwitch, "routing.switch"),
        (Op::TuiLogRead, "tui.log_read"),
        (Op::TuiTranscript, "tui.transcript"),
        (Op::TuiFirstRender, "tui.first_render"),
        (Op::TuiAttachedFrame, "tui.attached_frame"),
        (Op::TuiFrame, "tui.frame"),
        // #3654's attach spans: the launch-to-first-screen path, each
        // timed separately and never summed.
        (Op::TuiExtensions, "tui.extensions"),
        (Op::TuiAttachSummary, "tui.attach_summary"),
        (Op::TuiTailWait, "tui.tail_wait"),
        (Op::TuiPreviewFold, "tui.preview_fold"),
        (Op::TuiTailInstall, "tui.tail_install"),
        (Op::LogAppend, "log.append"),
        (Op::LogFsync, "log.fsync"),
        (Op::ToolPermission, "tool.permission"),
        (Op::ToolReviewer, "tool.reviewer"),
        (Op::ToolHooks, "tool.hooks"),
        (Op::ToolRun, "tool.run"),
        (Op::TurnRequest, "turn.request"),
        (Op::TurnModelCall, "turn.model_call"),
        (Op::TurnHandoff, "turn.handoff"),
        (Op::Hook(SessionStart), "hook.session_start"),
        (Op::Hook(TurnStart), "hook.turn_start"),
        (Op::Hook(BeforeMessage), "hook.before_message"),
        (Op::Hook(BeforeModelCall), "hook.before_model_call"),
        (Op::Hook(TurnEnd), "hook.turn_end"),
        (Op::Hook(BeforeHandoff), "hook.before_handoff"),
        (Op::McpConnect, "mcp.connect"),
        (Op::JobsStart, "jobs.start"),
        (Op::WorktreeCreate, "worktree.create"),
        (Op::WorktreeRemove, "worktree.remove"),
        (Op::PictureRun, "picture.run"),
        (Op::HubSessionStart, "hub.session_start"),
        (Op::HubResume, "hub.resume"),
        (Op::HubSubscribeReplay, "hub.subscribe_replay"),
        (Op::SessionsList, "sessions.list"),
        (Op::NetConnect, "net.connect"),
        (Op::SessionSearchScan, "session_search.scan"),
    ] {
        assert_eq!(op.name(), want, "{op:?}");
        assert_eq!(
            serde_json::to_string(&op).unwrap(),
            format!("\"{want}\""),
            "{op:?}"
        );
    }
}

#[test]
fn a_provider_request_writes_present_fields_in_order_and_leaves_absent_ones_out() {
    assert_eq!(
        serde_json::to_string(&full()).unwrap(),
        "{\"provider\":\"fake\",\"model\":\"m\",\"purpose\":\"model_call\",\
         \"host\":\"127.0.0.1:50731\",\"path\":\"/v1/responses\",\"status\":200,\
         \"attempt\":1,\"request_bytes\":2214,\"response_bytes\":913,\
         \"headers_ms\":41,\"first_token_ms\":57,\"total_ms\":180,\
         \"prefix_hashes\":{\"system\":\"9f2c\",\"tools\":\"41ab\",\
         \"messages\":[\"c7d0\",\"e91f\"]},\"cache_read\":9585,\
         \"cache_write\":{\"1h\":57}}"
    );
    let request = ProviderRequest {
        model: None,
        host: None,
        path: None,
        status: None,
        headers_ms: None,
        first_token_ms: None,
        purpose: Purpose::ModelList,
        response_bytes: 0,
        prefix_hashes: None,
        cache_read: None,
        cache_write: None,
        ..full()
    };
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        "{\"provider\":\"fake\",\"purpose\":\"model_list\",\"attempt\":1,\
         \"request_bytes\":2214,\"response_bytes\":0,\"total_ms\":180}"
    );
}

#[test]
fn a_prefix_hash_without_optional_segments_leaves_them_out() {
    let request = ProviderRequest {
        prefix_hashes: Some(PrefixHashes {
            system: None,
            tools: None,
            messages: Vec::new(),
        }),
        cache_read: None,
        cache_write: None,
        ..full()
    };
    let line = serde_json::to_string(&request).unwrap();
    assert!(
        line.contains("\"prefix_hashes\":{\"messages\":[]}"),
        "no system or tools member: {line}"
    );
    assert!(!line.contains("null"), "{line}");
    for absent in ["cache_read", "cache_write"] {
        assert!(!line.contains(absent), "{absent} is absent: {line}");
    }
}
