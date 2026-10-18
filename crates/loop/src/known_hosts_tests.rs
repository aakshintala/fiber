use super::*;
use serde_json::json;

#[test]
fn known_hosts_parse_urls_and_ports_exactly() {
    for (url, host, port) in [
        ("https://Example.COM/a", "example.com", 443),
        ("HTTP://example.com", "example.com", 80),
        ("https://example.com:443/", "example.com", 443),
        ("https://example.com:8080/", "example.com", 8080),
        ("http://example.com:80", "example.com", 80),
        ("https://api.example.com/", "api.example.com", 443),
        ("https://example.com:65535/", "example.com", 65535),
        ("https://example.com:65534/", "example.com", 65534),
        ("https://example.com:0/", "example.com", 0),
        ("https://example.com:1/", "example.com", 1),
        ("https://example.com:/", "example.com", 443),
        ("https://user:pw@example.com/", "example.com", 443),
        ("http://[::1]:8080/", "::1", 8080),
        ("http://[::1]/", "::1", 80),
        ("https://example.com/a?q=1#f", "example.com", 443),
        ("https://example.com/b?other=2", "example.com", 443),
        ("http://a.com\\@evil.com", "evil.com", 80),
    ] {
        assert_eq!(host_of(url), Some((host.into(), port)), "{url}");
    }
    for url in [
        "example.com",
        "ftp://example.com/",
        "https:///x",
        "https://",
        "mailto:a@b.c",
        "https://example.com:65536/",
        "https://example.com:8x/",
        "https://ex%41mple.com/",
        "https://exämple.com/",
        "http://:80/",
        "http://[::1",
        "http://[]/",
        "http://[::1]bad/",
        "http://a:b:c/",
        "http://example.com:-1/",
        "http://example.com:+1/",
        "http://exa mple.com/",
    ] {
        assert_eq!(host_of(url), None, "{url}");
    }
}

#[test]
fn known_hosts_named_in_prose_respects_url_boundaries() {
    for end in [
        " ", "\n", "<", ">", "\"", "'", "`", ")", ".", ",", ";", ":", "!", "?",
    ] {
        let prose = format!("See https://Example.com{end}");
        assert_eq!(
            named_in(&prose).collect::<Vec<_>>(),
            [("example.com".into(), 443)],
            "{end}"
        );
    }
    assert_eq!(
        named_in("[x](https://a.com/b) <https://b.com> HTTP://c.com ```https://d.com```")
            .collect::<Vec<_>>(),
        [
            ("a.com".into(), 443),
            ("b.com".into(), 443),
            ("c.com".into(), 80),
            ("d.com".into(), 443)
        ]
    );
    assert_eq!(
        named_in("README.md main.rs docs.rs/serde ftp://example.com").collect::<Vec<_>>(),
        []
    );
}

fn event(kind: &str, payload: serde_json::Value) -> Event {
    let line = contract::Envelope {
        kind: kind.into(),
        session_id: contract::SessionId("s".into()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::from_value(payload).unwrap(),
    };
    Event::from_envelope(&line).unwrap().unwrap()
}

#[test]
fn known_hosts_fold_only_trusted_sources_and_is_idempotent() {
    let url = "https://example.com/a";
    let text = json!([{"type":"text", "text":url}]);
    let mut cases = vec![
        (
            event(
                "turn_started",
                json!({"input":[{"type":"message", "source":"driver", "content":text}]}),
            ),
            true,
        ),
        (
            event(
                "steering_applied",
                json!({"source":"driver", "content":text}),
            ),
            true,
        ),
        (
            event(
                "opening_message",
                json!({"environment":{"workspace":"/", "os":"linux", "arch":"x86_64", "shell":"sh", "session_log":"/log", "date":"2026-10-17"}, "instruction_files":[{"path":"AGENTS.md", "content":url}], "skills":[]}),
            ),
            true,
        ),
        (
            event(
                "opening_message",
                json!({"environment":{"workspace":"/", "os":"linux", "arch":"x86_64", "shell":"sh", "session_log":"/log", "date":"2026-10-17"}, "instruction_files":[], "extension_sections":[{"extension":"test", "files":[{"path":"a", "content":url}]}], "skills":[]}),
            ),
            false,
        ),
        (
            event(
                "instruction_file",
                json!({"path":"AGENTS.md", "reason":"subdirectory", "content":url, "sent":"full"}),
            ),
            true,
        ),
        (
            event(
                "instruction_file",
                json!({"path":"AGENTS.md", "reason":"own_edit", "content":url, "sent":"full"}),
            ),
            false,
        ),
        (
            event(
                "instruction_file",
                json!({"path":"AGENTS.md", "reason":"changed", "content":url, "sent":"full", "extension":"test"}),
            ),
            false,
        ),
        (
            event(
                "instruction_file",
                json!({"path":"AGENTS.md", "reason":"deleted", "sent":"deleted"}),
            ),
            false,
        ),
        (
            event(
                "tool_call_completed",
                json!({"status":"completed", "content":[], "hosts_surfaced":[url, "bare.test"]}),
            ),
            true,
        ),
        (
            event(
                "tool_call_completed",
                json!({"status":"failed", "content":[], "hosts_surfaced":[url]}),
            ),
            true,
        ),
        (
            event(
                "tool_call_completed",
                json!({"status":"completed", "content":text}),
            ),
            false,
        ),
        (
            event(
                "tool_call_requested",
                json!({"name":"lookup", "arguments":{"url":url}}),
            ),
            false,
        ),
        (event("text_completed", json!({"text":url})), false),
    ];
    for origin in [
        json!({"source":"extension", "extension":"test"}),
        json!({"source":"session", "from_session_id":"other"}),
        json!({"source":"fiber"}),
    ] {
        let mut message = origin.clone();
        message["content"] = text.clone();
        message["type"] = json!("message");
        cases.push((event("turn_started", json!({"input":[message]})), false));
        let mut steering = origin;
        steering["content"] = text.clone();
        cases.push((event("steering_applied", steering), false));
    }
    for (decision, by, expected) in [
        ("allow", "person", true),
        ("allow", "reviewer", true),
        ("deny", "person", false),
        ("deny", "reviewer", false),
        ("allow", "session_grant", false),
        ("allow", "standing_rule", false),
    ] {
        cases.push((
            event(
                "permission_resolved",
                json!({"decision":decision, "decided_by":by, "hosts":[url, "not-a-url"]}),
            ),
            expected,
        ));
    }
    for (event, expected) in cases {
        let mut known = KnownHosts::default();
        known.fold(&event);
        assert_eq!(known.all_known(&[url.into()]), expected, "{event:?}");
        assert!(!known.all_known(&["https://not-a-url/".into()]));
        let once = known.clone();
        known.fold(&event);
        assert_eq!(known, once);
    }
}

#[test]
fn own_edit_instruction_file_makes_no_version_known() {
    let mut known = KnownHosts::default();
    known.fold(&event(
        "instruction_file",
        json!({"path":"AGENTS.md", "reason":"own_edit", "content":"https://own.test/", "sent":"full"}),
    ));
    assert!(!known.all_known(&["https://own.test/".into()]));
    known.fold(&event(
        "instruction_file",
        json!({"path":"AGENTS.md", "reason":"changed", "content":"https://later.test/", "sent":"full"}),
    ));
    assert!(!known.all_known(&["https://later.test/".into()]));
    known.fold(&event(
        "tool_call_completed",
        json!({"status":"completed", "content":[], "hosts_surfaced":["https://own.test/"]}),
    ));
    assert!(known.all_known(&["https://own.test/".into()]));
}

#[test]
fn instruction_file_named_in_earlier_changes_adds_nothing() {
    let mut known = KnownHosts::default();
    known.fold(&event(
        "tool_call_completed",
        json!({"status":"failed", "content":[], "changes":[{"path":"AGENTS.md", "added":1, "removed":0}]}),
    ));
    known.fold(&event(
        "instruction_file",
        json!({"path":"AGENTS.md", "reason":"changed", "content":"https://changed.test/", "sent":"full"}),
    ));
    assert!(!known.all_known(&["https://changed.test/".into()]));
    known.fold(&event(
        "instruction_file",
        json!({"path":"other/AGENTS.md", "reason":"subdirectory", "content":"https://other.test/", "sent":"full"}),
    ));
    assert!(known.all_known(&["https://other.test/".into()]));
}

#[test]
fn unchanged_instruction_file_adds_hosts_from_each_version() {
    let mut known = KnownHosts::default();
    for url in ["https://first.test/", "https://second.test/"] {
        known.fold(&event(
            "instruction_file",
            json!({"path":"AGENTS.md", "reason":"changed", "content":url, "sent":"full"}),
        ));
    }
    known.fold(&event(
        "instruction_file",
        json!({"path":"AGENTS.md", "reason":"own_edit", "extension":"test", "content":"https://extension.test/", "sent":"full"}),
    ));
    known.fold(&event(
        "instruction_file",
        json!({"path":"AGENTS.md", "reason":"deleted", "sent":"deleted"}),
    ));
    assert!(known.all_known(&["https://first.test/".into(), "https://second.test/".into()]));
    assert!(!known.all_known(&["https://extension.test/".into()]));
}

#[test]
fn later_changes_remove_all_instruction_versions_but_keep_other_sources() {
    let mut known = KnownHosts::default();
    known.fold(&event(
        "opening_message",
        json!({"environment":{"workspace":"/", "os":"linux", "arch":"x86_64", "shell":"sh", "session_log":"/log", "date":"2026-10-17"}, "instruction_files":[{"path":"AGENTS.md", "content":"https://opening.test/"}], "skills":[]}),
    ));
    for url in ["https://first.test/", "https://second.test/"] {
        known.fold(&event(
            "instruction_file",
            json!({"path":"AGENTS.md", "reason":"changed", "content":url, "sent":"full"}),
        ));
    }
    known.fold(&event(
        "instruction_file",
        json!({"path":"other/AGENTS.md", "reason":"subdirectory", "content":"https://second.test/", "sent":"full"}),
    ));
    known.fold(&event(
        "tool_call_completed",
        json!({"status":"completed", "content":[], "hosts_surfaced":["https://surfaced.test/"]}),
    ));
    assert!(known.all_known(&["https://first.test/".into(), "https://second.test/".into()]));
    known.fold(&event(
        "tool_call_completed",
        json!({"status":"completed", "content":[], "changes":[{"path":"AGENTS.md", "added":1, "removed":0}], "hosts_surfaced":["https://result.test/"]}),
    ));
    assert!(!known.all_known(&["https://first.test/".into()]));
    assert!(known.all_known(&[
        "https://second.test/".into(),
        "https://surfaced.test/".into(),
        "https://result.test/".into(),
    ]));
    known.fold(&event(
        "instruction_file",
        json!({"path":"other/AGENTS.md", "reason":"own_edit", "content":"https://own.test/", "sent":"full"}),
    ));
    assert!(!known.all_known(&["https://second.test/".into()]));
    assert!(!known.all_known(&["https://own.test/".into()]));
    assert!(!known.all_known(&["https://opening.test/".into()]));
}

#[test]
fn opening_instruction_files_exclude_changed_paths_before_and_after_opening() {
    let opening = event(
        "opening_message",
        json!({"environment":{"workspace":"/", "os":"linux", "arch":"x86_64", "shell":"sh", "session_log":"/log", "date":"2026-10-17"}, "instruction_files":[{"path":"AGENTS.md", "content":"https://opening.test/"}, {"path":"other/AGENTS.md", "content":"https://other.test/"}], "skills":[]}),
    );
    for change in [
        event(
            "tool_call_completed",
            json!({"status":"completed", "content":[], "changes":[{"path":"AGENTS.md", "added":1, "removed":0}]}),
        ),
        event(
            "instruction_file",
            json!({"path":"AGENTS.md", "reason":"own_edit", "content":"https://own.test/", "sent":"full"}),
        ),
    ] {
        let mut known = KnownHosts::default();
        known.fold(&opening);
        assert!(known.all_known(&["https://opening.test/".into()]));
        known.fold(&change);
        assert!(!known.all_known(&["https://opening.test/".into()]));
        known.fold(&opening);
        assert!(!known.all_known(&["https://opening.test/".into()]));
        assert!(known.all_known(&["https://other.test/".into()]));
    }
}

#[test]
fn known_hosts_all_known_requires_every_entry_to_parse_and_match() {
    let mut known = KnownHosts::default();
    assert!(known.all_known(&[]));
    known.fold(&event(
        "tool_call_completed",
        json!({"status":"completed", "content":[], "hosts_surfaced":["https://example.com/"]}),
    ));
    assert!(known.all_known(&[
        "https://EXAMPLE.com/other?q=1".into(),
        "http://example.com:443/".into()
    ]));
    for other in [
        "https://unknown.test/",
        "https://api.example.com/",
        "https://example.com:8080/",
        "http://example.com/",
        "example.com",
        "https://ex%41mple.com/",
    ] {
        assert!(
            !known.all_known(&["https://example.com/".into(), other.into()]),
            "{other}"
        );
    }
}
