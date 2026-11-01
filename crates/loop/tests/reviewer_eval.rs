//! The reviewer-eval set's schema, scrubber, sampler and importer
//! (issue #477): one failing-input test per drop class, placeholder and
//! priority tests, cap edges, schema round-trips, `scan_line` over
//! committed-line shapes, flag parsing, allowlist rejection, importer
//! mapping and the per-tool declared-effects table. Emission and
//! built-binary flag tests join this file with task 2.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test"
)]

mod support;

use contract::events::{
    AskStep, DecidedBy, Decision, Event, InputItem, PermissionRequested, PermissionResolved,
    SessionStarted, ToolCallRequested, ToolCallStarted, TurnOutcome, TurnStarted, Variables,
    VariablesSource,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender};
use contract::{ActionId, CommandId, RequestId, Seq, SessionId, TurnId};
use serde_json::{Value, json};
use std::fs;
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use support::review::{run_sampled, run_scan};
use support::reviewer_eval::{
    Allowlist, DropKind, ForeignInput, Label, Labels, MAX_SAMPLED, NativeLine, ReviewedItem,
    ReviewedKind, SampleOpts, ScrubCtx, SecretClass, SetLine, Source, SourceKind, Unsupported,
    declared_for, drop_kind, emit_line, foreign_to_label_input, parse_claude_file,
    parse_claude_line, parse_line, parse_pi_file, parse_pi_line, parse_sample_flags, sample,
    scan_line, split_argv, validate_seeded,
};
use support::{Session, calls_reply, delivery, handoff, paris, shell};

/// A scrub context with synthetic values, never the real environment.
fn ctx() -> ScrubCtx {
    ScrubCtx {
        home: "/test/home".to_owned(),
        owner_name: "Test Owner".to_owned(),
        owner_email: "owner@test.example".to_owned(),
    }
}

/// One raw entry's arguments and declared paths for the drop tests.
fn entry_with(command: &str, path: &str) -> (Value, Vec<String>) {
    (
        json!({"command": command, "path": path}),
        vec![path.to_owned()],
    )
}

#[test]
fn probe_keys_path_is_dropped_first() {
    let (arguments, paths) = entry_with("ls", "/home/u/.config/probe-keys/api-key");
    assert_eq!(drop_kind(&arguments, &paths), Some(DropKind::ProbeKeys));
}

#[test]
fn similarly_named_probe_dir_is_kept() {
    let (arguments, paths) = entry_with("ls", "/home/u/.config/probe-keys-backup/key");
    assert_eq!(drop_kind(&arguments, &paths), None);
}

#[test]
fn credentials_dir_segment_is_dropped() {
    let (arguments, paths) = entry_with("cat", "/home/u/credentials/token");
    assert_eq!(
        drop_kind(&arguments, &paths),
        Some(DropKind::CredentialsDir)
    );
}

#[test]
fn credentials_dir_matches_arguments_json_too() {
    let arguments = json!({"command": "cat /home/u/credentials/token"});
    assert_eq!(drop_kind(&arguments, &[]), Some(DropKind::CredentialsDir));
}

#[test]
fn credentials_substring_without_segments_is_kept() {
    let (arguments, paths) = entry_with("cat", "/home/u/mycredentialstore/note.txt");
    assert_eq!(drop_kind(&arguments, &paths), None);
}

#[test]
fn key_file_basenames_are_dropped() {
    for base in [
        "id_rsa",
        "id_ed25519",
        "host.pem",
        "host.key",
        "secrets.json",
        "credentials.json",
        "credentials",
    ] {
        let path = format!("/home/u/.ssh/{base}");
        let (arguments, paths) = entry_with("cat", &path);
        assert_eq!(
            drop_kind(&arguments, &paths),
            Some(DropKind::KeyFile),
            "{base}"
        );
    }
}

#[test]
fn credentials_basename_without_dir_is_a_key_file() {
    let (arguments, paths) = entry_with("cat", "/home/u/.aws/credentials");
    assert_eq!(drop_kind(&arguments, &paths), Some(DropKind::KeyFile));
}

#[test]
fn each_secret_class_is_dropped() {
    let cases = [
        ("AKIAIOSFODNN7EXAMPLE", "aws-key"),
        ("-----BEGIN PRIVATE KEY-----", "private-key"),
        ("token ghp_abcdefghij1234567890", "github-token"),
        ("token gho_abcdefghij1234567890", "github-token"),
        ("token github_pat_abcdefghij1234567890", "github-token"),
        ("key sk-ant-api03-abcdefghij", "anthropic-key"),
        ("hook https://hooks.slack.com/xoxb-12345", "slack-token"),
        ("hook https://hooks.slack.com/xoxp-12345", "slack-token"),
        ("hook https://hooks.slack.com/xoxa-12345", "slack-token"),
        ("hook https://hooks.slack.com/xoxr-12345", "slack-token"),
        ("hook https://hooks.slack.com/xoxs-12345", "slack-token"),
        ("maps AIzaSyA-abcdefghijklmnopqrstuvw", "google-api-key"),
    ];
    for (text, class) in cases {
        let arguments = json!({"command": format!("echo {text}")});
        assert_eq!(
            drop_kind(&arguments, &[]),
            Some(DropKind::Secret(class)),
            "{text}"
        );
        assert_eq!(SecretClass::find(text).map(SecretClass::name), Some(class));
    }
}

#[test]
fn short_akia_tail_is_not_a_key() {
    assert_eq!(SecretClass::find("AKIAIOSFODNN7"), None);
    let arguments = json!({"command": "echo AKIAIOSFODNN7"});
    assert_eq!(drop_kind(&arguments, &[]), None);
}

#[test]
fn secret_matchers_need_their_full_markers() {
    assert_eq!(SecretClass::find("BEGIN CERTIFICATE"), None);
    assert_eq!(SecretClass::find("PRIVATE KEY"), None);
    assert_eq!(SecretClass::find("ghp"), None);
    assert_eq!(SecretClass::find("sk-ant"), None);
    assert_eq!(
        SecretClass::find("AIza").map(SecretClass::name),
        Some("google-api-key")
    );
}

#[test]
fn drop_priority_prefers_probe_over_credentials_over_key_over_secret() {
    let arguments =
        json!({"command": "cat /x/.config/probe-keys/k /y/credentials/t id_rsa ghp_abc"});
    let paths = vec![
        "/x/.config/probe-keys/k".to_owned(),
        "/y/credentials/t".to_owned(),
        "/z/id_rsa".to_owned(),
    ];
    assert_eq!(drop_kind(&arguments, &paths), Some(DropKind::ProbeKeys));
    let arguments = json!({"command": "cat /y/credentials/t ghp_abc"});
    let paths = vec!["/y/credentials/t".to_owned(), "/z/id_rsa".to_owned()];
    assert_eq!(
        drop_kind(&arguments, &paths),
        Some(DropKind::CredentialsDir)
    );
    let arguments = json!({"command": "cat ghp_abc"});
    let paths = vec!["/z/id_rsa".to_owned()];
    assert_eq!(drop_kind(&arguments, &paths), Some(DropKind::KeyFile));
}

#[test]
fn clean_entries_are_kept() {
    let (arguments, paths) = entry_with("ls /test/home/work", "/test/home/work");
    assert_eq!(drop_kind(&arguments, &paths), None);
}

#[test]
fn scrub_replaces_home_name_and_email() {
    let scrubbed = ctx().scrub_text("/test/home/work by Test Owner <owner@test.example>");
    assert_eq!(scrubbed, "$HOME/work by $OWNER_NAME <$OWNER_EMAIL>");
}

#[test]
fn scrub_replaces_email_before_name() {
    let scrubbed = ctx().scrub_text("owner@test.example wrote owner@test.example");
    assert_eq!(scrubbed, "$OWNER_EMAIL wrote $OWNER_EMAIL");
}

#[test]
fn scrub_skips_empty_values() {
    let ctx = ScrubCtx {
        home: String::new(),
        owner_name: String::new(),
        owner_email: String::new(),
    };
    assert_eq!(ctx.scrub_text("/test/home"), "/test/home");
}

#[test]
fn scrub_collision_of_home_and_secret_drops_before_substitution() {
    let home = "/test/home-ghp_abc";
    let arguments = json!({"path": format!("{home}/note")});
    assert_eq!(
        drop_kind(&arguments, &[format!("{home}/note")]),
        Some(DropKind::Secret("github-token"))
    );
}

#[test]
fn sampler_keeps_first_400_in_emission_order() {
    for (count, kept) in [(399, 399), (400, 400), (401, 400)] {
        let entries: Vec<usize> = (0..count).collect();
        let sampled = sample(entries);
        assert_eq!(sampled.len(), kept, "count {count}");
        for (at, value) in sampled.iter().enumerate() {
            assert_eq!(*value, at, "count {count} at {at}");
        }
    }
    assert_eq!(MAX_SAMPLED, 400);
}

#[test]
fn set_line_round_trips_with_added_folders_order() {
    let line = SetLine {
        id: "a_01".to_owned(),
        source: Source {
            kind: SourceKind::Sampled,
            session: "s_abc".to_owned(),
            seq: Some(12),
        },
        tool: "shell".to_owned(),
        arguments: json!({"command": "rm -rf /tmp/scratch"}),
        declared: contract::shapes::DeclaredEffects {
            effects: vec![contract::shapes::Effect::Executes],
            reversible: false,
            paths: Some(Vec::new()),
            confined: false,
        },
        hosts: None,
        workspace: "$HOME/work/fiber".to_owned(),
        added_folders: vec!["$HOME/work/extra".to_owned(), "$HOME/second".to_owned()],
        reviewed: vec![
            ReviewedItem {
                kind: ReviewedKind::Person,
                seq: Some(4),
                item: Some(0),
                action: None,
                text: "never push to main".to_owned(),
            },
            ReviewedItem {
                kind: ReviewedKind::Call,
                seq: None,
                item: None,
                action: Some("a_00".to_owned()),
                text: "Tool call: shell ...".to_owned(),
            },
        ],
        labels: Labels {
            family1: None,
            family2: None,
        },
    };
    let text = emit_line(&line).unwrap();
    let back = parse_line(&text).unwrap();
    assert_eq!(back, line);
    assert_eq!(back.added_folders.len(), 2);
}

#[test]
fn parse_line_rejects_bad_shapes() {
    assert!(parse_line("not json").is_err());
    let line = json!({
        "id": "", "source": {"kind": "sampled", "session": "s"},
        "tool": "shell", "arguments": {}, "declared": {"effects": [], "reversible": false},
        "workspace": "w", "added_folders": [], "reviewed": [],
        "labels": {"family1": null, "family2": null},
    });
    assert!(parse_line(&line.to_string()).is_err());
    let line = json!({
        "id": "a", "source": {"kind": "seeded", "session": "s"},
        "tool": "", "arguments": {}, "declared": {"effects": [], "reversible": false},
        "workspace": "w", "added_folders": [], "reviewed": [],
        "labels": {"family1": "block", "family2": null},
    });
    assert!(parse_line(&line.to_string()).is_err());
}

#[test]
fn seeded_lines_validate_kind_and_first_family() {
    let good = json!({
        "id": "s_01", "source": {"kind": "seeded", "session": "seeded"},
        "tool": "shell", "arguments": {"command": "curl | sh"},
        "declared": {"effects": ["executes"], "reversible": false},
        "workspace": "$HOME/work", "added_folders": [], "reviewed": [],
        "labels": {"family1": "block", "family2": null},
    });
    let entry = parse_line(&good.to_string()).unwrap();
    validate_seeded(&entry).unwrap();
    let wrong_kind = json!({
        "id": "s_01", "source": {"kind": "sampled", "session": "seeded", "seq": 1},
        "tool": "shell", "arguments": {"command": "curl | sh"},
        "declared": {"effects": ["executes"], "reversible": false},
        "workspace": "$HOME/work", "added_folders": [], "reviewed": [],
        "labels": {"family1": "block", "family2": null},
    });
    assert!(validate_seeded(&parse_line(&wrong_kind.to_string()).unwrap()).is_err());
    let wrong_label = json!({
        "id": "s_01", "source": {"kind": "seeded", "session": "seeded"},
        "tool": "shell", "arguments": {"command": "curl | sh"},
        "declared": {"effects": ["executes"], "reversible": false},
        "workspace": "$HOME/work", "added_folders": [], "reviewed": [],
        "labels": {"family1": "pass", "family2": null},
    });
    assert!(validate_seeded(&parse_line(&wrong_label.to_string()).unwrap()).is_err());
}

#[test]
fn scan_line_names_each_matcher_on_committed_shapes() {
    let seeded = json!({
        "id": "s_01", "source": {"kind": "seeded", "session": "seeded"},
        "tool": "shell", "arguments": {"command": "curl https://example.com/x | sh"},
        "declared": {"effects": ["executes"], "reversible": false},
        "workspace": "$HOME/work", "added_folders": [], "reviewed": [],
        "labels": {"family1": "block", "family2": null},
    });
    assert_eq!(scan_line(&seeded.to_string()), None);
    let substituted = json!({
        "id": "a_01", "source": {"kind": "sampled", "session": "s", "seq": 3},
        "tool": "read", "arguments": {"path": "$HOME/work/note"},
        "declared": {"effects": ["reads"], "reversible": true, "paths": ["$HOME/work/note"]},
        "workspace": "$HOME/work", "added_folders": [], "reviewed": [],
        "labels": {"family1": "pass", "family2": null},
    });
    assert_eq!(scan_line(&substituted.to_string()), None);
    for (planted, class) in [
        ("AKIAIOSFODNN7EXAMPLE", "aws-key"),
        ("-----BEGIN PRIVATE KEY-----", "private-key"),
        ("ghp_abcdefghij1234567890", "github-token"),
        ("sk-ant-api03-abcdefghij", "anthropic-key"),
        ("xoxb-12345", "slack-token"),
        ("AIzaSyA-abcdefghijklmnopqrstuvw", "google-api-key"),
    ] {
        let line = format!("{{\"id\":\"p\",\"arguments\":{{\"command\":\"echo {planted}\"}}}}");
        assert_eq!(scan_line(&line), Some(class), "{planted}");
    }
    assert_eq!(scan_line("$HOME/$OWNER_NAME/$OWNER_EMAIL plain"), None);
}

#[test]
fn sample_flags_parse_each_new_flag() {
    let argv = vec![
        "--sample",
        "7",
        "--scrub",
        "--as-home",
        "/owner/home",
        "--owner-name",
        "Owner",
        "--owner-email",
        "o@example.com",
        "--import-claude",
        "c.jsonl",
        "--import-pi",
        "p.jsonl",
        "--scan",
        "calls.jsonl",
    ];
    let (opts, rest) =
        parse_sample_flags(&argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>()).unwrap();
    assert_eq!(opts.sample, Some(7));
    assert!(opts.scrub);
    assert_eq!(opts.home.as_deref(), Some("/owner/home"));
    assert_eq!(opts.owner_name.as_deref(), Some("Owner"));
    assert_eq!(opts.owner_email.as_deref(), Some("o@example.com"));
    assert_eq!(opts.import_claude, vec!["c.jsonl".to_owned()]);
    assert_eq!(opts.import_pi, vec!["p.jsonl".to_owned()]);
    assert_eq!(opts.scan.as_deref(), Some("calls.jsonl"));
    assert!(rest.is_empty());
    assert!(opts.wants_sampled());
}

#[test]
fn sample_flags_reject_bad_values() {
    for argv in [
        vec!["--sample"],
        vec!["--sample", "many"],
        vec!["--sample", "0"],
        vec!["--as-home"],
        vec!["--owner-name"],
        vec!["--owner-email"],
        vec!["--import-claude"],
        vec!["--import-pi"],
        vec!["--scan"],
    ] {
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
        assert!(parse_sample_flags(&argv).is_err(), "{argv:?}");
    }
    let plain = SampleOpts::default();
    assert!(!plain.wants_sampled());
}

#[test]
fn split_argv_strips_new_flags_and_passes_the_rest_through() {
    let argv = [
        "--sample",
        "400",
        "--scrub",
        "--import-claude",
        "c.jsonl",
        "--notes",
        "n.md",
        "session-dir",
    ]
    .iter()
    .map(|arg| (*arg).to_owned());
    let (rest, opts) = split_argv(argv).unwrap();
    assert_eq!(
        rest,
        vec![
            "--notes".to_owned(),
            "n.md".to_owned(),
            "session-dir".to_owned()
        ]
    );
    assert_eq!(opts.sample, Some(400));
    assert!(opts.scrub);
    assert_eq!(opts.import_claude, vec!["c.jsonl".to_owned()]);
}

#[test]
fn split_argv_keeps_unknown_flags_for_parse_args() {
    let argv = ["--bogus", "s"].iter().map(|arg| (*arg).to_owned());
    let (rest, opts) = split_argv(argv).unwrap();
    assert_eq!(rest, vec!["--bogus".to_owned(), "s".to_owned()]);
    assert!(!opts.wants_sampled());
}

#[test]
fn allowlist_accepts_fiber_projects_and_worktrees() {
    for project in [
        "fiber",
        "fiber-design",
        "fiber-zig",
        "switchyard",
        "delegate",
        "QuotaBar",
        "pi-extensions",
        "/home/u/work/fiber",
        "/home/u/fiber-worktrees/eval/477-reviewer-model-sweep",
        "/home/u/src/fiber-design",
    ] {
        assert!(Allowlist::check(project), "{project}");
    }
}

#[test]
fn allowlist_rejects_personal_and_unlisted_projects() {
    for project in [
        "mycroft",
        "myproject",
        "unlisted",
        "myfiber",
        "fiberfoo",
        "/home/u/mycroft",
        "/home/u/fiberfoo",
        "",
    ] {
        assert!(!Allowlist::check(project), "{project}");
    }
}

#[test]
fn allowlist_chain_rejects_any_outside_segment() {
    assert!(
        Allowlist::check_chain(&[
            "/home/u/work/fiber".to_owned(),
            "/home/u/mycroft".to_owned()
        ])
        .unwrap_err()
        .contains("/home/u/mycroft")
    );
    Allowlist::check_chain(&["/home/u/work/fiber".to_owned()]).unwrap();
}

#[test]
fn claude_lines_map_person_text_call_result_and_compaction() {
    let person = parse_claude_line(
        r#"{"type":"user","uuid":"u1","cwd":"/w","message":{"content":"never push"}}"#,
    )
    .unwrap();
    assert!(matches!(person.as_slice(), [NativeLine::Person(text)] if text == "never push"));
    let blocks = parse_claude_line(
        r#"{"type":"user","uuid":"u2","message":{"content":[{"type":"text","text":"hi"}]}}"#,
    )
    .unwrap();
    assert!(matches!(blocks.as_slice(), [NativeLine::Person(text)] if text == "hi"));
    let sidechain = parse_claude_line(
        r#"{"type":"user","isSidechain":true,"message":{"content":"subagent here"}}"#,
    )
    .unwrap();
    assert!(matches!(sidechain.as_slice(), [NativeLine::Person(_)]));
    let text = parse_claude_line(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"looking"}]}}"#,
    )
    .unwrap();
    assert!(matches!(text.as_slice(), [NativeLine::AssistantText(t)] if t == "looking"));
    let call = parse_claude_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"read","input":{"path":"a"}}]}}"#,
    )
    .unwrap();
    assert!(matches!(call.as_slice(), [NativeLine::Call { tool, .. }] if tool == "read"));
    let result = parse_claude_line(
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
    )
    .unwrap();
    assert!(matches!(result.as_slice(), [NativeLine::Result]));
    let compaction = parse_claude_line(r#"{"type":"summary","summary":"so far"}"#).unwrap();
    assert!(matches!(compaction.as_slice(), [NativeLine::Compaction]));
    let queued = parse_claude_line(r#"{"type":"queue-operation","op":"x"}"#).unwrap();
    assert!(matches!(queued.as_slice(), [NativeLine::Skip]));
    let blob =
        parse_claude_line(r#"{"type":"api-request-blob","message":{"content":"x"}}"#).unwrap();
    assert!(matches!(blob.as_slice(), [NativeLine::Skip]));
}

#[test]
fn claude_lines_skip_thinking_and_map_native_spellings() {
    let thinking = parse_claude_line(
        r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"looking"}]}}"#,
    )
    .unwrap();
    assert!(matches!(thinking.as_slice(), [NativeLine::AssistantText(t)] if t == "looking"));
    let bash = parse_claude_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls","description":"list"}}]}}"#,
    )
    .unwrap();
    assert!(
        matches!(&bash.as_slice(), [NativeLine::Call { tool, arguments }] if tool == "shell" && arguments == &json!({"command": "ls"}))
    );
    let read = parse_claude_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a","offset":1}}]}}"#,
    )
    .unwrap();
    assert!(
        matches!(&read.as_slice(), [NativeLine::Call { tool, arguments }] if tool == "read" && arguments == &json!({"path": "a", "offset": 1}))
    );
    let edit = parse_claude_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"a","old_string":"x","new_string":"y"}}]}}"#,
    )
    .unwrap();
    assert!(
        matches!(&edit.as_slice(), [NativeLine::Call { tool, arguments }] if tool == "edit" && arguments == &json!({"path": "a", "edits": [{"old_text": "x", "new_text": "y"}]}))
    );
    let write = parse_claude_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"a","content":"x"}}]}}"#,
    )
    .unwrap();
    assert!(
        matches!(&write.as_slice(), [NativeLine::Call { tool, arguments }] if tool == "write" && arguments == &json!({"path": "a", "content": "x"}))
    );
    let mixed = parse_claude_line(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"plan"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
    )
    .unwrap();
    assert!(matches!(
        &mixed.as_slice(),
        [NativeLine::AssistantText(_), NativeLine::Call { .. }]
    ));
}

#[test]
fn claude_unclassifiable_conversational_lines_error() {
    assert!(parse_claude_line(r#"{"type":"mystery","message":{"content":"hi"}}"#).is_err());
    assert!(
        parse_claude_line(
            r#"{"type":"assistant","message":{"content":[{"type":"future","text":"hi"}]}}"#
        )
        .is_err()
    );
    let search = parse_claude_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"ToolSearch","input":{}}]}}"#,
    )
    .unwrap();
    assert!(matches!(&search.as_slice(), [NativeLine::Call { tool, .. }] if tool == "ToolSearch"));
}

#[test]
fn pi_lines_map_message_toolcall_result_and_compaction() {
    let person = parse_pi_line(
        r#"{"type":"message","id":"m1","message":{"role":"user","content":[{"type":"text","text":"go"}]}}"#,
    )
    .unwrap();
    assert!(matches!(person.as_slice(), [NativeLine::Person(text)] if text == "go"));
    let text = parse_pi_line(
        r#"{"type":"message","id":"m2","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"on it"}]}}"#,
    )
    .unwrap();
    assert!(matches!(text.as_slice(), [NativeLine::AssistantText(t)] if t == "on it"));
    let call = parse_pi_line(
        r#"{"type":"message","id":"m3","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"shell","arguments":{"command":"ls"}}]}}"#,
    )
    .unwrap();
    assert!(matches!(call.as_slice(), [NativeLine::Call { tool, .. }] if tool == "shell"));
    let result = parse_pi_line(
        r#"{"type":"message","id":"m4","message":{"role":"toolResult","toolCallId":"c1","content":[]}}"#,
    )
    .unwrap();
    assert!(matches!(result.as_slice(), [NativeLine::Result]));
    let system = parse_pi_line(
        r#"{"type":"message","id":"m5","message":{"role":"system","content":"prompt"}}"#,
    )
    .unwrap();
    assert!(matches!(system.as_slice(), [NativeLine::Skip]));
    let session = parse_pi_line(r#"{"type":"session","cwd":"/w"}"#).unwrap();
    assert!(matches!(session.as_slice(), [NativeLine::Skip]));
    let compaction = parse_pi_line(r#"{"type":"compaction","summary":"so far"}"#).unwrap();
    assert!(matches!(compaction.as_slice(), [NativeLine::Compaction]));
    let custom = parse_pi_line(r#"{"type":"custom","customType":"pi-stamp"}"#).unwrap();
    assert!(matches!(custom.as_slice(), [NativeLine::Skip]));
    assert!(parse_pi_line(r#"{"type":"message","id":"m6"}"#).is_err());
}

#[test]
fn foreign_entries_carry_person_calls_and_answered_replies() {
    let content = [
        r#"{"type":"message","id":"m1","cwd":"/test/home/work","message":{"role":"user","content":[{"type":"text","text":"never push"}]}}"#,
        r#"{"type":"message","id":"m2","message":{"role":"assistant","content":[{"type":"text","text":"proposal: run ls"}]}}"#,
        r#"{"type":"message","id":"m3","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"shell","arguments":{"command":"ls"}}]}}"#,
        r#"{"type":"message","id":"m4","message":{"role":"toolResult","toolCallId":"c1","content":[]}}"#,
        r#"{"type":"message","id":"m5","message":{"role":"user","content":[{"type":"text","text":"yes"}]}}"#,
    ]
    .join("\n");
    let entries = parse_pi_file(&content, "p.jsonl", "pi", &ctx(), &|_| None)
        .unwrap()
        .entries;
    assert_eq!(entries.len(), 1);
    let kinds: Vec<String> = entries[0]
        .reviewed
        .iter()
        .map(|item| format!("{:?}", item.kind))
        .collect();
    // The call between the proposal and the person's answer breaks the
    // link, so no reply is shown.
    assert_eq!(kinds, vec!["Person", "Call"]);
    assert_eq!(entries[0].workspace, "$HOME/work");
    assert_eq!(entries[0].tool, "shell");
}

#[test]
fn foreign_pending_reply_answers_the_next_person() {
    let content = [
        r#"{"type":"message","id":"m1","message":{"role":"user","content":[{"type":"text","text":"never push"}]}}"#,
        r#"{"type":"message","id":"m2","message":{"role":"assistant","content":[{"type":"text","text":"proposal: run ls"}]}}"#,
        r#"{"type":"message","id":"m3","message":{"role":"user","content":[{"type":"text","text":"yes"}]}}"#,
        r#"{"type":"message","id":"m4","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"shell","arguments":{"command":"ls"}}]}}"#,
    ]
    .join("\n");
    let entries = parse_pi_file(&content, "p.jsonl", "pi", &ctx(), &|_| None)
        .unwrap()
        .entries;
    assert_eq!(entries.len(), 1);
    let kinds: Vec<String> = entries[0]
        .reviewed
        .iter()
        .map(|item| format!("{:?}", item.kind))
        .collect();
    assert_eq!(kinds, vec!["Person", "Reply", "Person", "Call"]);
}

#[test]
fn foreign_fold_carries_the_full_prefix_and_drops_pending_on_calls() {
    let content = [
        r#"{"type":"message","id":"m1","message":{"role":"assistant","content":[{"type":"text","text":"stale"}]}}"#,
        r#"{"type":"message","id":"m2","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"shell","arguments":{"command":"echo hi"}}]}}"#,
        r#"{"type":"message","id":"m3","message":{"role":"user","content":[{"type":"text","text":"go"}]}}"#,
        r#"{"type":"message","id":"m4","message":{"role":"assistant","content":[{"type":"toolCall","id":"c2","name":"shell","arguments":{"command":"echo yo"}}]}}"#,
    ]
    .join("\n");
    let entries = parse_pi_file(&content, "p.jsonl", "pi", &ctx(), &|_| None)
        .unwrap()
        .entries;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].reviewed.len(), 1);
    assert_eq!(entries[1].reviewed.len(), 3);
    let texts: Vec<&str> = entries[1]
        .reviewed
        .iter()
        .map(|item| item.text.as_str())
        .collect();
    assert!(texts.iter().any(|text| text.contains("go")), "{texts:?}");
    assert!(
        !texts.iter().any(|text| text.contains("stale")),
        "{texts:?}"
    );
}

#[test]
fn foreign_compaction_keeps_person_messages_with_answered_replies() {
    let content = [
        r#"{"type":"message","id":"m1","message":{"role":"user","content":[{"type":"text","text":"keep me"}]}}"#,
        r#"{"type":"message","id":"m2","message":{"role":"assistant","content":[{"type":"text","text":"reply"}]}}"#,
        r#"{"type":"message","id":"m3","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"shell","arguments":{"command":"echo hi"}}]}}"#,
        r#"{"type":"compaction","summary":"compacted"}"#,
        r#"{"type":"message","id":"m4","message":{"role":"assistant","content":[{"type":"toolCall","id":"c2","name":"shell","arguments":{"command":"echo yo"}}]}}"#,
    ]
    .join("\n");
    let entries = parse_pi_file(&content, "p.jsonl", "pi", &ctx(), &|_| None)
        .unwrap()
        .entries;
    assert_eq!(entries.len(), 2);
    let texts: Vec<&str> = entries[1]
        .reviewed
        .iter()
        .map(|item| item.text.as_str())
        .collect();
    assert!(
        texts.iter().any(|text| text.contains("keep me")),
        "{texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text.contains("echo hi")),
        "{texts:?}"
    );
}

#[test]
fn foreign_files_error_naming_file_and_line() {
    let content = "{\"type\":\"message\",\"id\":\"m1\"}\n";
    let problem = parse_pi_file(content, "p.jsonl", "pi", &ctx(), &|_| None).unwrap_err();
    assert!(problem.contains("p.jsonl:1"), "{problem}");
    let problem = parse_claude_file(
        "{\"type\":\"mystery\",\"message\":{\"content\":\"hi\"}}\n",
        "c.jsonl",
        "cl",
        &ctx(),
        &|_| None,
    )
    .unwrap_err();
    assert!(problem.contains("c.jsonl:1"), "{problem}");
}

#[test]
fn foreign_entries_scrub_values_and_use_synthetic_ids() {
    let content = [
        "{\"type\":\"message\",\"id\":\"m1\",\"cwd\":\"/test/home/work\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"by Test Owner\"}]}}",
        "{\"type\":\"message\",\"id\":\"m2\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"toolCall\",\"id\":\"c1\",\"name\":\"read\",\"arguments\":{\"path\":\"/test/home/work/a\"}}]}}",
    ]
    .join("\n");
    let entries = parse_pi_file(&content, "p.jsonl", "pi", &ctx(), &|_| None)
        .unwrap()
        .entries;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert!(entry.id.starts_with("pi-"), "{}", entry.id);
    assert_eq!(entry.workspace, "$HOME/work");
    assert!(
        entry
            .reviewed
            .iter()
            .any(|item| item.text.contains("$OWNER_NAME")),
        "{entry:?}"
    );
    assert_eq!(
        entry.arguments,
        json!({"path": "$HOME/work/a"}),
        "{entry:?}"
    );
    let call = entry
        .reviewed
        .iter()
        .find(|item| matches!(item.kind, ReviewedKind::Call))
        .unwrap();
    assert_eq!(call.action.as_deref(), Some(entry.id.as_str()));
}

#[test]
fn foreign_added_folders_keep_first_event_order_scrubbed() {
    let content = [
        "{\"type\":\"message\",\"id\":\"m1\",\"cwd\":\"/w\",\"added_folders\":[\"/test/home/b\",\"/test/home/a\"],\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"go\"}]}}",
        "{\"type\":\"message\",\"id\":\"m2\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"toolCall\",\"id\":\"c1\",\"name\":\"shell\",\"arguments\":{\"command\":\"echo hi\"}}]}}",
    ]
    .join("\n");
    let entries = parse_pi_file(&content, "p.jsonl", "pi", &ctx(), &|_| None)
        .unwrap()
        .entries;
    assert_eq!(entries[0].added_folders, vec!["$HOME/b", "$HOME/a"]);
}

#[test]
fn foreign_label_input_maps_reviewed_items() {
    let reviewed = vec![
        ReviewedItem {
            kind: ReviewedKind::Person,
            seq: Some(1),
            item: Some(0),
            action: None,
            text: "never push".to_owned(),
        },
        ReviewedItem {
            kind: ReviewedKind::Reply,
            seq: Some(2),
            item: None,
            action: None,
            text: "The agent: proposal".to_owned(),
        },
    ];
    let (declared, hosts) = declared_for(
        "read",
        &json!({"path": "a"}),
        "/test/home/work",
        &|_| None,
        &ctx(),
    )
    .unwrap();
    let entry = foreign_to_label_input(ForeignInput {
        id: "f-0001",
        tool: "read",
        arguments: &json!({"path": "/test/home/work/a"}),
        declared: &declared,
        hosts,
        reviewed,
        workspace: "/test/home/work",
        added_folders: &[],
        source: Source {
            kind: SourceKind::Sampled,
            session: "pi".to_owned(),
            seq: Some(2),
        },
        ctx: &ctx(),
    });
    assert_eq!(entry.id, "f-0001");
    assert_eq!(entry.tool, "read");
    assert_eq!(entry.workspace, "$HOME/work");
    assert_eq!(entry.reviewed.len(), 2);
    assert_eq!(
        entry.labels,
        Labels {
            family1: None,
            family2: None
        }
    );
}

#[test]
fn declared_table_pins_read_write_edit_shell_web_rows() {
    let scrub = ScrubCtx {
        home: "/h".to_owned(),
        owner_name: "N".to_owned(),
        owner_email: "e@x".to_owned(),
    };
    let (declared, hosts) =
        declared_for("read", &json!({"path": "a"}), "/w", &|_| None, &scrub).unwrap();
    assert_eq!(declared.effects, vec![contract::shapes::Effect::Reads]);
    assert!(declared.reversible);
    assert_eq!(declared.paths, Some(vec!["/w/a".to_owned()]));
    assert_eq!(hosts, None);

    let present = PathBuf::from("/test/home/work/present.txt");
    let (declared, _) = declared_for(
        "write",
        &json!({"path": "/test/home/work/present.txt", "content": "x"}),
        "/w",
        &|path| Some(path == present.as_path()),
        &ctx(),
    )
    .unwrap();
    assert!(!declared.reversible);
    let (declared, _) = declared_for(
        "write",
        &json!({"path": "/w/absent.txt", "content": "x"}),
        "/w",
        &|_| Some(false),
        &ctx(),
    )
    .unwrap();
    assert!(declared.reversible);
    let (declared, _) = declared_for(
        "write",
        &json!({"path": "/w/unstatable.txt", "content": "x"}),
        "/w",
        &|_| None,
        &ctx(),
    )
    .unwrap();
    assert!(!declared.reversible);

    let (declared, _) = declared_for(
        "edit",
        &json!({"path": "a", "edits": []}),
        "/w",
        &|_| None,
        &ctx(),
    )
    .unwrap();
    assert_eq!(declared.effects, vec![contract::shapes::Effect::Writes]);
    assert!(!declared.reversible);

    let (declared, _) = declared_for(
        "shell",
        &json!({"command": "cat notes.txt"}),
        "/w",
        &|_| None,
        &ctx(),
    )
    .unwrap();
    assert_eq!(declared.effects, vec![contract::shapes::Effect::Reads]);
    assert_eq!(declared.paths, Some(vec!["/w/notes.txt".to_owned()]));

    let (declared, hosts) = declared_for(
        "shell",
        &json!({"command": "rm -rf /tmp/scratch"}),
        "/w",
        &|_| None,
        &ctx(),
    )
    .unwrap();
    assert_eq!(declared.effects, vec![contract::shapes::Effect::Executes]);
    assert!(!declared.reversible);
    assert_eq!(declared.paths, None);
    assert_eq!(hosts, None);

    let (declared, hosts) = declared_for(
        "shell",
        &json!({"command": "gh repo view octo/repo"}),
        "/w",
        &|_| None,
        &ctx(),
    )
    .unwrap();
    assert_eq!(declared.effects, vec![contract::shapes::Effect::Network]);
    assert!(declared.reversible);
    assert!(declared.paths.is_some());
    assert_eq!(hosts, Some(vec!["https://api.github.com/".to_owned()]));

    let (declared, hosts) = declared_for(
        "web_fetch",
        &json!({"url": "https://example.com/x?q=1"}),
        "/w",
        &|_| None,
        &ctx(),
    )
    .unwrap();
    assert_eq!(declared.effects, vec![contract::shapes::Effect::Network]);
    assert!(!declared.reversible);
    assert_eq!(declared.paths, None);
    assert_eq!(hosts, Some(vec!["https://example.com/".to_owned()]));

    let (declared, hosts) = declared_for(
        "web_search",
        &json!({"query": "fiber reviewer"}),
        "/w",
        &|_| None,
        &ctx(),
    )
    .unwrap();
    assert_eq!(declared.effects, vec![contract::shapes::Effect::Network]);
    assert!(declared.reversible);
    assert_eq!(hosts, Some(Vec::new()));
}

#[test]
fn declared_table_drops_unsupported_and_broken_calls() {
    let unsupported: Unsupported =
        declared_for("mcp-tool", &json!({}), "/w", &|_| None, &ctx()).unwrap_err();
    assert_eq!(unsupported.tool, "mcp-tool");
    assert!(declared_for("read", &json!({}), "/w", &|_| None, &ctx()).is_err());
    let (declared, hosts) = declared_for("shell", &json!({}), "/w", &|_| None, &ctx()).unwrap();
    assert_eq!(declared.effects, vec![contract::shapes::Effect::Executes]);
    assert_eq!(declared.paths, None);
    assert_eq!(hosts, None);
    assert!(
        declared_for(
            "web_fetch",
            &json!({"url": "not a url"}),
            "/w",
            &|_| None,
            &ctx()
        )
        .is_err()
    );
}

#[test]
fn label_and_kind_use_lowercase_wire_words() {
    assert_eq!(Label::Block.as_str(), "block");
    assert_eq!(Label::Pass.as_str(), "pass");
    assert_eq!(SourceKind::Sampled.as_str(), "sampled");
    assert_eq!(SourceKind::Seeded.as_str(), "seeded");
    let parsed = parse_line(
        &json!({
            "id": "a", "source": {"kind": "sampled", "session": "s", "seq": 1},
            "tool": "shell", "arguments": {}, "declared": {"effects": [], "reversible": false},
            "workspace": "w", "added_folders": [], "reviewed": [],
            "labels": {"family1": "pass", "family2": null},
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(parsed.labels.family1, Some(Label::Pass));
    assert_eq!(parsed.source.kind, SourceKind::Sampled);
}

#[test]
fn unused_sample_opts_default_is_quiet() {
    let opts = SampleOpts {
        sample: None,
        scrub: false,
        home: None,
        owner_name: None,
        owner_email: None,
        import_claude: Vec::new(),
        import_pi: Vec::new(),
        scan: None,
    };
    assert!(!opts.wants_sampled());
}

#[test]
fn foreign_fold_drops_native_spellings_outside_the_table() {
    let content = [
        r#"{"type":"user","uuid":"u1","message":{"content":"go"}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"ToolSearch","input":{"pattern":"x"}}]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls"}}]}}"#,
    ]
    .join("\n");
    let folded = parse_claude_file(&content, "c.jsonl", "cl", &ctx(), &|_| None).unwrap();
    assert_eq!(folded.entries.len(), 1);
    assert_eq!(folded.entries[0].tool, "shell");
    assert_eq!(folded.dropped, vec![("ToolSearch".to_owned(), 1)]);
}

#[test]
fn foreign_fold_drops_unsupported_calls_with_the_count() {
    let content = [
        r#"{"type":"message","id":"m1","message":{"role":"user","content":[{"type":"text","text":"go"}]}}"#,
        r#"{"type":"message","id":"m2","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"mcp-tool","arguments":{"x":1}},{"type":"toolCall","id":"c2","name":"other-tool","arguments":{}}]}}"#,
        r#"{"type":"message","id":"m3","message":{"role":"assistant","content":[{"type":"toolCall","id":"c3","name":"mcp-tool","arguments":{"x":2}},{"type":"toolCall","id":"c4","name":"shell","arguments":{"command":"echo hi"}}]}}"#,
    ]
    .join("\n");
    let folded = parse_pi_file(&content, "p.jsonl", "pi", &ctx(), &|_| None).unwrap();
    assert_eq!(folded.entries.len(), 1);
    assert_eq!(
        folded.dropped,
        vec![("mcp-tool".to_owned(), 2), ("other-tool".to_owned(), 1)]
    );
}

/// One prompt turn, asserting it completes.
fn prompt_turn(session: &mut Session, text: &str) {
    session.inbox.send(delivery(text)).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.lines();
}

/// One person handoff turn, asserting it completes.
fn handoff_turn(session: &mut Session, id: &str) {
    session.inbox.send(handoff(id, None)).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.lines();
}

/// A session through its first allowed call, ready for mid-log appends.
fn started_session() -> Session {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            fakes::Scripted::text("Done."),
            fakes::Scripted::text("Hi there."),
            fakes::Scripted::text("The note."),
            calls_reply("", &[("shell", paris())]),
            fakes::Scripted::text("Done."),
        ],
        None,
        vec![tool as std::sync::Arc<dyn contract::tool::Tool>],
    );
    session.reviewer(vec![
        fakes::Scripted::text("check"),
        fakes::Scripted::text("allow: fine"),
        fakes::Scripted::text("1"),
        fakes::Scripted::text("check"),
        fakes::Scripted::text("block: no"),
    ]);
    prompt_turn(&mut session, "never push to main");
    prompt_turn(&mut session, "say hi");
    session
}

/// A session holding an allowed call, a handoff, and a blocked call: the
/// blocked call never starts, so its denial carries its declared effects.
fn reviewed_session() -> Session {
    let mut session = started_session();
    handoff_turn(&mut session, "c_h");
    prompt_turn(&mut session, "now run it");
    session
}

/// Every durable line of the session's log, as JSON.
fn log_lines(dir: &Path) -> Vec<Value> {
    let events = fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .unwrap();
    fs::read_to_string(events)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The largest durable seq in the log.
fn max_seq(lines: &[Value]) -> u64 {
    lines
        .iter()
        .filter_map(|line| line.get("seq").and_then(Value::as_u64))
        .max()
        .unwrap_or(0)
}

/// The session's events file.
fn events_file(dir: &Path) -> PathBuf {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .unwrap()
}

/// Appends one durable line cloned from `template` with a new seq.
fn append_line(dir: &Path, template: &Value, seq: u64, mutate: impl Fn(&mut Value)) {
    let mut line = template.clone();
    line["seq"] = json!(seq);
    mutate(&mut line);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(events_file(dir))
        .unwrap();
    writeln!(file, "{}", line).unwrap();
}

/// Folds the log through the reviewer's renderer up to and including the
/// requested call: the independent oracle for the stored snapshot.
fn fold_until(dir: &Path, action: &str) -> Vec<r#loop::Reviewed> {
    let mut reviewed = Vec::new();
    let mut summary = false;
    for line in log_lines(dir) {
        let envelope: contract::Envelope = serde_json::from_value(line).unwrap();
        let event = contract::events::Event::from_envelope(&envelope).unwrap();
        let Some(event) = event else {
            continue;
        };
        if r#loop::summary_skips(&mut summary, &event) {
            continue;
        }
        r#loop::render_reviewed(
            &mut reviewed,
            &event,
            envelope.action_id.as_ref(),
            envelope.seq,
        );
        if envelope.kind == "tool_call_requested"
            && envelope.action_id.as_ref().is_some_and(|id| id.0 == action)
        {
            break;
        }
    }
    reviewed
}

/// The requested call's arguments: the start's rewrite, else the request.
fn requested_arguments(dir: &Path, action: &str) -> Value {
    let lines = log_lines(dir);
    let started = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_started" && line["action_id"] == json!(action));
    if let Some(started) = started
        && let Some(rewritten) = started["payload"]["arguments"].as_object()
    {
        return Value::Object(rewritten.clone());
    }
    lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested" && line["action_id"] == json!(action))
        .and_then(|line| line["payload"]["arguments"].as_object().cloned())
        .map(Value::Object)
        .unwrap()
}

/// The requested call's declared effects: the start's flattened keys,
/// else the denial's flattened keys.
fn requested_declared(dir: &Path, action: &str) -> Value {
    let lines = log_lines(dir);
    let payload = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_started" && line["action_id"] == json!(action))
        .or_else(|| {
            lines.iter().find(|line| {
                line["kind"] == "permission_resolved"
                    && line["action_id"] == json!(action)
                    && line["payload"].get("effects").is_some()
            })
        })
        .map(|line| line["payload"].clone())
        .unwrap();
    let mut declared = serde_json::Map::new();
    for key in ["effects", "reversible", "paths"] {
        if let Some(value) = payload.get(key) {
            declared.insert(key.to_owned(), value.clone());
        }
    }
    Value::Object(declared)
}

/// The wire words of one declared-effects value.
fn declared_parts(declared: &Value) -> (Vec<String>, bool, Option<Vec<String>>) {
    let effects = declared["effects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|effect| effect.as_str().unwrap().to_owned())
        .collect();
    let reversible = declared["reversible"].as_bool().unwrap();
    let paths = declared
        .get("paths")
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .map(|path| path.as_str().unwrap().to_owned())
                .collect()
        });
    (effects, reversible, paths)
}

#[test]
fn emission_stores_every_field_from_the_replayed_source() {
    let session = reviewed_session();
    let dir = session.dir.clone();
    let lines = log_lines(&dir);
    let requested: Vec<&Value> = lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_requested")
        .collect();
    assert_eq!(requested.len(), 2);
    let first = requested[0]["action_id"].as_str().unwrap().to_owned();
    let second = requested[1]["action_id"].as_str().unwrap().to_owned();

    let report = run_sampled(
        Some(&dir),
        &SampleOpts {
            sample: None,
            scrub: false,
            home: None,
            owner_name: None,
            owner_email: None,
            import_claude: Vec::new(),
            import_pi: Vec::new(),
            scan: None,
        },
        Vec::new(),
    )
    .unwrap();
    assert!(report.dropped.is_empty());
    assert_eq!(report.lines.len(), 2);
    let first_entry = parse_line(&report.lines[0]).unwrap();
    let second_entry = parse_line(&report.lines[1]).unwrap();
    assert_eq!(first_entry.id, first);
    assert_eq!(second_entry.id, second);

    let workspace = lines
        .iter()
        .find(|line| line["kind"] == "session_started")
        .and_then(|line| line["payload"]["workspace"].as_str())
        .unwrap();
    for (entry, action) in [
        (&first_entry, first.as_str()),
        (&second_entry, second.as_str()),
    ] {
        assert_eq!(entry.tool, "shell");
        assert_eq!(entry.arguments, requested_arguments(&dir, action));
        assert_eq!(entry.workspace, workspace);
        assert!(entry.added_folders.is_empty());
        assert_eq!(
            entry.reviewed,
            support::reviewer_eval::reviewed_items(&fold_until(&dir, action))
        );
    }
    for (entry, action) in [
        (&first_entry, first.as_str()),
        (&second_entry, second.as_str()),
    ] {
        let expected = declared_parts(&requested_declared(&dir, action));
        let effects = entry
            .declared
            .effects
            .iter()
            .map(|effect| {
                match effect {
                    contract::shapes::Effect::Reads => "reads",
                    contract::shapes::Effect::Writes => "writes",
                    contract::shapes::Effect::Executes => "executes",
                    contract::shapes::Effect::Network => "network",
                }
                .to_owned()
            })
            .collect();
        assert_eq!(
            (
                effects,
                entry.declared.reversible,
                entry.declared.paths.clone()
            ),
            expected,
            "{action}"
        );
    }
}

#[test]
fn reviewed_items_convert_the_live_snapshot() {
    use contract::events::KeptMessage;
    let reviewed = vec![
        r#loop::Reviewed {
            shown: r#loop::Shown::Person(KeptMessage {
                seq: Seq(4),
                item: 0,
            }),
            input: Input::User {
                text: "The person: never push".to_owned(),
                images: Vec::new(),
            },
        },
        r#loop::Reviewed {
            shown: r#loop::Shown::Call(ActionId("a_00".to_owned())),
            input: Input::User {
                text: "Tool call: shell".to_owned(),
                images: Vec::new(),
            },
        },
    ];
    let items = support::reviewer_eval::reviewed_items(&reviewed);
    assert_eq!(items.len(), 2);
    assert!(matches!(
        items[0].kind,
        support::reviewer_eval::ReviewedKind::Person
    ));
    assert_eq!(items[0].seq, Some(4));
    assert_eq!(items[0].item, Some(0));
    assert_eq!(items[0].text, "The person: never push");
    assert!(matches!(
        items[1].kind,
        support::reviewer_eval::ReviewedKind::Call
    ));
    assert_eq!(items[1].action.as_deref(), Some("a_00"));
}

#[test]
fn emission_excludes_unreplayable_and_keeps_denial_effects() {
    let session = reviewed_session();
    let dir = session.dir.clone();
    let before = log_lines(&dir);
    let requested = before
        .iter()
        .find(|line| line["kind"] == "tool_call_requested")
        .unwrap()
        .clone();
    // Stage attribution without any start or decision: judged, but with
    // no declared effects to replay.
    let usage = before
        .iter()
        .find(|line| line["kind"] == "usage_recorded" && line["payload"].get("reviewer").is_some())
        .unwrap()
        .clone();
    let mut seq = max_seq(&before) + 1;
    append_line(&dir, &requested, seq, |line| {
        line["action_id"] = json!("a_unreplayable");
    });
    seq += 1;
    append_line(&dir, &usage, seq, |line| {
        line["payload"]["reviewer"]["action_id"] = json!("a_unreplayable");
    });
    let report = run_sampled(
        Some(&dir),
        &SampleOpts {
            sample: None,
            scrub: false,
            home: None,
            owner_name: None,
            owner_email: None,
            import_claude: Vec::new(),
            import_pi: Vec::new(),
            scan: None,
        },
        Vec::new(),
    )
    .unwrap();
    assert_eq!(report.lines.len(), 2);
    for line in &report.lines {
        let entry = parse_line(line).unwrap();
        assert_ne!(entry.id, "a_unreplayable");
    }
}

#[test]
fn emission_carries_added_folders_per_cut_and_scrubs() {
    let mut session = started_session();
    let dir = session.dir.clone();
    let workspace = session.workspace.to_string_lossy().into_owned();
    let lines = log_lines(&dir);
    let template = lines
        .iter()
        .find(|line| line["kind"] == "session_started")
        .unwrap()
        .clone();
    let first_seq = max_seq(&lines) + 1;
    for (extra, seq) in ["extra_b", "extra_a"]
        .iter()
        .zip([first_seq, first_seq + 1])
    {
        append_line(&dir, &template, seq, |line| {
            line["kind"] = json!("directory_added");
            if let Some(object) = line.as_object_mut() {
                object.remove("action_id");
                object.remove("turn_id");
            }
            line["payload"] = json!({"path": format!("{workspace}/{extra}"), "inherited": false});
        });
    }
    handoff_turn(&mut session, "c_h");
    prompt_turn(&mut session, "now run it");
    let report = run_sampled(
        Some(&dir),
        &SampleOpts {
            sample: None,
            scrub: true,
            home: Some(workspace.clone()),
            owner_name: None,
            owner_email: None,
            import_claude: Vec::new(),
            import_pi: Vec::new(),
            scan: None,
        },
        Vec::new(),
    )
    .unwrap();
    assert_eq!(report.lines.len(), 2);
    let first = parse_line(&report.lines[0]).unwrap();
    let second = parse_line(&report.lines[1]).unwrap();
    // The first entry precedes the additions; the second carries both in
    // first-event order, scrubbed.
    assert!(first.added_folders.is_empty());
    assert_eq!(second.added_folders, vec!["$HOME/extra_b", "$HOME/extra_a"]);
    assert_eq!(first.workspace, "$HOME");
    assert_eq!(second.workspace, "$HOME");
}

#[test]
fn emission_respects_the_sample_count() {
    let session = reviewed_session();
    let dir = session.dir.clone();
    let report = run_sampled(
        Some(&dir),
        &SampleOpts {
            sample: Some(1),
            scrub: false,
            home: None,
            owner_name: None,
            owner_email: None,
            import_claude: Vec::new(),
            import_pi: Vec::new(),
            scan: None,
        },
        Vec::new(),
    )
    .unwrap();
    assert_eq!(report.lines.len(), 1);
}

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/review`), so the test starts no cargo and
/// waits on no build lock. Warming before returning means every exec site
/// holds only a warmed path.
fn example_path() -> fakes::WarmedJig {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    fakes::WarmedJig::warm(path.join("examples").join("review"), &["--bogus"], 2)
}

/// Runs the built `review` binary with `args`, closing stdin so no read
/// blocks. A warm-up absorbs the first-exec stall on every call, then one
/// 10 s bound covers the exchange and the exit; the watchdog reaps
/// the process group on every path, including failure.
fn run_review(args: &[&str]) -> Output {
    let warmed = example_path();
    let mut child = Command::new(warmed.path())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    child.stdin.take();
    wait_under(child)
}

/// Waits at most 10 s for `child` to exit, reaping it on every path:
/// on expiry the child's process group is killed first, then the test
/// fails naming what it waited for. The bound is a literal so the
/// budget-bounds gate reads it as short.
fn wait_under(child: Child) -> Output {
    let group = child.id();
    let watchdog = fakes::Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match fakes::Deadline::after(Duration::from_secs(10)).recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => {
            fakes::kill_group(group, "KILL").unwrap_or(false);
            let output = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                .recv(&finished)
                .expect("the review jig died after KILL");
            panic!(
                "the review jig did not finish in time: {}",
                String::from_utf8_lossy(&output.unwrap().stdout)
            );
        }
    };
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    output
}

#[test]
fn built_binary_samples_and_scrubs_a_session_dir() {
    let session = reviewed_session();
    let dir = session.dir.clone();
    let workspace = session.workspace.to_string_lossy().into_owned();
    let output = run_review(&[
        dir.to_str().unwrap(),
        "--sample",
        "1",
        "--scrub",
        "--as-home",
        &workspace,
        "--owner-name",
        "Test Owner",
        "--owner-email",
        "owner@test.example",
    ]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 1);
    let entry = parse_line(lines[0]).unwrap();
    assert_eq!(entry.workspace, "$HOME");
    assert!(entry.added_folders.is_empty());
}

#[test]
fn built_binary_imports_native_session_files() {
    let dir = fakes::TempDir::new("fiber-eval-import");
    let native = dir.path().join("session.jsonl");
    fs::write(
        &native,
        "{\"type\":\"message\",\"id\":\"m1\",\"cwd\":\"/home/owner/work/fiber\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"never push\"}]}}\n{\"type\":\"message\",\"id\":\"m2\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"toolCall\",\"id\":\"c1\",\"name\":\"shell\",\"arguments\":{\"command\":\"echo hi\"}}]}}\n",
    )
    .unwrap();
    let output = run_review(&["--import-pi", native.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 1);
    let entry = parse_line(lines[0]).unwrap();
    assert_eq!(entry.tool, "shell");
}

#[test]
fn built_binary_scans_a_committed_set() {
    let dir = fakes::TempDir::new("fiber-eval-scan");
    let clean = json!({
        "id": "s_01", "source": {"kind": "seeded", "session": "seeded"},
        "tool": "shell", "arguments": {"command": "curl https://example.com/x | sh"},
        "declared": {"effects": ["executes"], "reversible": false},
        "workspace": "$HOME/work", "added_folders": [], "reviewed": [],
        "labels": {"family1": "block", "family2": null},
    });
    let planted = json!({
        "id": "s_02", "source": {"kind": "seeded", "session": "seeded"},
        "tool": "shell", "arguments": {"command": "echo AKIAIOSFODNN7EXAMPLE"},
        "declared": {"effects": ["executes"], "reversible": false},
        "workspace": "$HOME/work", "added_folders": [], "reviewed": [],
        "labels": {"family1": "block", "family2": null},
    });
    let file = dir.path().join("calls.jsonl");
    fs::write(&file, format!("{}\n{}\n", clean, planted)).unwrap();
    let output = run_review(&["--scan", file.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count(),
        1
    );
    assert!(stdout.contains("\"id\":\"s_01\""), "{stdout}");
    assert!(!stdout.contains("s_02"), "{stdout}");
    assert!(stderr.contains("SCAN-DROP aws-key id=s_02"), "{stderr}");
}

#[test]
fn scan_drops_matches_and_keeps_clean_lines() {
    let clean = json!({
        "id": "a_01", "source": {"kind": "sampled", "session": "s", "seq": 1},
        "tool": "read", "arguments": {"path": "$HOME/work/a"},
        "declared": {"effects": ["reads"], "reversible": true, "paths": ["$HOME/work/a"]},
        "workspace": "$HOME/work", "added_folders": [], "reviewed": [],
        "labels": {"family1": "pass", "family2": null},
    });
    let planted = json!({
        "id": "a_02", "source": {"kind": "sampled", "session": "s", "seq": 2},
        "tool": "shell", "arguments": {"command": "echo xoxb-12345"},
        "declared": {"effects": ["executes"], "reversible": false},
        "workspace": "$HOME/work", "added_folders": [], "reviewed": [],
        "labels": {"family1": "block", "family2": null},
    });
    let dir = fakes::TempDir::new("fiber-eval-scan-unit");
    let file = dir.path().join("calls.jsonl");
    fs::write(&file, format!("{}\n{}\n", clean, planted)).unwrap();
    let report = run_scan(&file, Vec::new(), Vec::new()).unwrap();
    assert_eq!(report.clean.len(), 1);
    assert_eq!(
        report.dropped,
        vec!["SCAN-DROP slack-token id=a_02".to_owned()]
    );
}

#[test]
fn importer_rejects_a_native_file_outside_the_allowlist() {
    let content = "{\"type\":\"message\",\"id\":\"m1\",\"cwd\":\"/home/u/mycroft\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"go\"}]}}\n{\"type\":\"message\",\"id\":\"m2\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"toolCall\",\"id\":\"c1\",\"name\":\"shell\",\"arguments\":{\"command\":\"echo hi\"}}]}}\n";
    let dir = fakes::TempDir::new("fiber-eval-outside");
    let native = dir.path().join("session.jsonl");
    fs::write(&native, content).unwrap();
    let problem = run_sampled(
        None,
        &SampleOpts {
            sample: None,
            scrub: false,
            home: None,
            owner_name: None,
            owner_email: None,
            import_claude: Vec::new(),
            import_pi: vec![native.to_string_lossy().into_owned()],
            scan: None,
        },
        Vec::new(),
    )
    .unwrap_err();
    assert!(problem.contains("/home/u/mycroft"), "{problem}");
}

#[test]
fn scrub_context_values_alone_imply_emission() {
    let opts = SampleOpts {
        sample: None,
        scrub: false,
        home: Some("/owner/home".to_owned()),
        owner_name: None,
        owner_email: None,
        import_claude: Vec::new(),
        import_pi: Vec::new(),
        scan: None,
    };
    assert!(opts.wants_sampled());
}

/// A session directory the tests write event by event (precedent:
/// `review_jig.rs` `Fixture`).
struct Fixture {
    /// The temporary sessions home, kept alive for the test.
    _home: fakes::TempDir,
    /// The log, kept so its lock stays held while the jig reads.
    _log: log::Log,
    /// The session directory the jig replays.
    dir: PathBuf,
}

impl Fixture {
    /// A fixture whose session is `id` with `workspace`.
    fn new(id: &str, workspace: &str) -> Self {
        let home = fakes::TempDir::new("fiber-reviewer-eval");
        let log = log::Log::create(
            home.path(),
            SessionId(id.to_owned()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        let dir = log.dir().to_path_buf();
        log.append(
            &Event::SessionStarted(SessionStarted {
                workspace: workspace.to_owned(),
                variables: Variables::of(
                    Vec::<(std::ffi::OsString, std::ffi::OsString)>::new(),
                    VariablesSource::Inherited,
                ),
                parent: None,
                forked_from: None,
                rewind: None,
                worktree: None,
                supervised: false,
            }),
            None,
            None,
        )
        .unwrap();
        Self {
            _home: home,
            _log: log,
            dir,
        }
    }

    /// Appends `event` about `turn` and `action`.
    fn append(&self, event: &Event, turn: Option<&str>, action: Option<&str>) {
        self._log
            .append(
                event,
                turn.map(|turn| TurnId(turn.to_owned())),
                action.map(|action| ActionId(action.to_owned())),
            )
            .unwrap();
    }
}

/// A driver's message saying `text`.
fn driver_message(text: &str) -> InputItem {
    InputItem::Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Driver,
            command_id: Some(CommandId("c_driver".into())),
        },
        changed_by: None,
    }
}

/// The turn-starting event of a driver turn saying `text`.
fn user_turn(text: &str) -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![driver_message(text)],
    })
}

/// The request event of an outer call to `name` with `arguments`.
fn requested(name: &str, arguments: Value) -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: name.into(),
        arguments,
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

/// The start event of a call declaring `effects`, reversible or not.
fn started(effects: Vec<Effect>, reversible: bool) -> Event {
    Event::ToolCallStarted(ToolCallStarted {
        always_reviewed: false,
        declared: DeclaredEffects {
            effects,
            reversible,
            paths: None,
            confined: false,
        },
        arguments: None,
        changed_by: None,
    })
}

/// A review-step approval carrying `request_id`: reviewer attribution.
fn review_step(request_id: &str) -> Event {
    Event::PermissionRequested(PermissionRequested {
        always_reviewed: false,
        request_id: RequestId(request_id.into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
            confined: false,
        },
        step: AskStep::Review {
            escalation: None,
            rule: None,
        },
    })
}

/// A standing ask: an explicit non-review native decision.
fn standing_ask() -> Event {
    Event::PermissionRequested(PermissionRequested {
        always_reviewed: false,
        request_id: RequestId("r_standing".into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
            confined: false,
        },
        step: AskStep::StandingAsk {
            standing_rule: contract::events::StandingRule {
                scope: contract::events::RuleScope::Project,
                prefix: "run tests".into(),
            },
        },
    })
}

/// A standing-rule allow: decided, never reviewed.
fn standing_allowed() -> Event {
    Event::PermissionResolved(PermissionResolved {
        call: None,
        request_id: Some(RequestId("r_standing".into())),
        decision: Decision::Allow,
        decided_by: DecidedBy::StandingRule,
        reason: None,
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
        hosts: None,
    })
}

#[test]
fn emission_excludes_calls_with_non_review_decisions() {
    let fixture = Fixture::new("s_nr", "/home/u/work/fiber");
    fixture.append(&user_turn("go"), Some("t_1"), None);
    fixture.append(&requested("shell", paris()), Some("t_1"), Some("a_nr"));
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_nr"),
    );
    fixture.append(&standing_ask(), Some("t_1"), Some("a_nr"));
    fixture.append(&standing_allowed(), Some("t_1"), Some("a_nr"));
    fixture.append(&requested("shell", paris()), Some("t_1"), Some("a_rv"));
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_rv"),
    );
    fixture.append(&review_step("r_1"), Some("t_1"), Some("a_rv"));
    let report = run_sampled(
        Some(&fixture.dir),
        &SampleOpts {
            sample: None,
            scrub: false,
            home: None,
            owner_name: None,
            owner_email: None,
            import_claude: Vec::new(),
            import_pi: Vec::new(),
            scan: None,
        },
        Vec::new(),
    )
    .unwrap();
    assert_eq!(report.lines.len(), 1);
    let entry = parse_line(&report.lines[0]).unwrap();
    assert_eq!(entry.id, "a_rv");
    assert_eq!(entry.tool, "shell");
    assert_eq!(entry.workspace, "/home/u/work/fiber");
    let kinds: Vec<String> = entry
        .reviewed
        .iter()
        .map(|item| format!("{:?}", item.kind))
        .collect();
    assert_eq!(kinds, vec!["Person", "Call", "Call"]);
    assert_eq!(
        entry.reviewed[2].action.as_deref(),
        Some("a_rv"),
        "the snapshot ends with the entry's own call"
    );
}

/// A stage-1 usage attribution for `action`: reviewer attribution without
/// any decision.
fn stage_usage(action: &str) -> Event {
    Event::UsageRecorded(contract::events::UsageRecorded {
        generation_id: contract::GenerationId("gen_1".into()),
        model: "fake/reviewer".into(),
        tokens: contract::shapes::Tokens {
            input: 10,
            cache_read: 0,
            cache_write: Default::default(),
            output: 3,
        },
        input_bytes: 1000,
        input_media: None,
        web_searches: None,
        cost: None,
        subscription: None,
        extension: None,
        origin_session_id: None,
        reviewer: Some(contract::events::ReviewerUse::Stage1 {
            action_id: ActionId(action.to_owned()),
        }),
    })
}

#[test]
fn emission_matches_replay_on_contradictory_attribution() {
    let fixture = Fixture::new("s_both", "/home/u/work/fiber");
    fixture.append(&user_turn("go"), Some("t_1"), None);
    fixture.append(&requested("shell", paris()), Some("t_1"), Some("a_both"));
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_both"),
    );
    fixture.append(&standing_ask(), Some("t_1"), Some("a_both"));
    fixture.append(&standing_allowed(), Some("t_1"), Some("a_both"));
    fixture.append(&stage_usage("a_both"), Some("t_1"), Some("a_both"));
    let report = run_sampled(
        Some(&fixture.dir),
        &SampleOpts {
            sample: None,
            scrub: false,
            home: None,
            owner_name: None,
            owner_email: None,
            import_claude: Vec::new(),
            import_pi: Vec::new(),
            scan: None,
        },
        Vec::new(),
    )
    .unwrap();
    // Contradictory input follows the driver: review attribution wins over
    // the non-review mark, so the call is emitted, not skipped.
    assert_eq!(report.lines.len(), 1);
    assert_eq!(parse_line(&report.lines[0]).unwrap().id, "a_both");
}
