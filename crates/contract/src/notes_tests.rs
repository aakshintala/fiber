use super::*;
use serde_json::{from_value, json, to_value};

#[test]
fn topics_follow_the_notes_order_and_wire_contract() {
    let expected = [
        (
            "source_control",
            "### Source control",
            "Which accounts, organisations and repositories are your own or your organisation's?",
            "which accounts, organisations and repositories are the person's own or their organisation's",
        ),
        (
            "internal_hosts",
            "### Internal hosts",
            "Which internal hosts, registries and services count as inside?",
            "internal hosts, registries and services that count as inside",
        ),
        (
            "internal_tools",
            "### Internal tools",
            "Which internal tools do you run, and what does running each one do?",
            "internal tools and what running them does",
        ),
        (
            "routine_actions",
            "### Routine actions",
            "What routine actions, such as squash-merging your own pull requests once CI passes, should the reviewer allow?",
            "routine actions, such as squash-merging their own pull requests once CI passes",
        ),
        (
            "never",
            "### What must never happen",
            "What must never happen, such as touching infra/prod?",
            "what must never happen, such as touching infra/prod",
        ),
        (
            "read_only_commands",
            "### Read-only commands",
            "Which commands only read, so the reviewer may skip reviewing them?",
            "commands that only read",
        ),
    ];
    for (topic, (id, heading, question, covers)) in Topic::ALL.into_iter().zip(expected) {
        assert_eq!(to_value(topic).unwrap(), id);
        assert_eq!(from_value::<Topic>(json!(id)).unwrap(), topic);
        assert_eq!(topic.heading(), heading);
        assert_eq!(topic.question(), question);
        assert_eq!(topic.covers(), covers);
    }
    assert!(from_value::<Topic>(json!("other")).is_err());
    assert!(from_value::<Topic>(json!(null)).is_err());
}

#[test]
fn arguments_omit_optional_fields_and_refuse_unknown_keys() {
    let wire = json!({"topic":"never"});
    let args: ProposeNotesArgs = from_value(wire.clone()).unwrap();
    assert_eq!(args.answer, None);
    assert!(args.earlier.is_empty());
    assert_eq!(to_value(args).unwrap(), wire);
    assert!(from_value::<ProposeNotesArgs>(json!({"topic":"never","extra":1})).is_err());
    let wire = json!({"topic":"never","answer":"no production","earlier":["no deletion"]});
    assert_eq!(
        to_value(from_value::<ProposeNotesArgs>(wire.clone()).unwrap()).unwrap(),
        wire
    );
}

#[test]
fn tallies_validate_each_boundary_and_describe_the_count() {
    assert!(SessionTally::new("p".into(), 0, 0).is_none());
    let one = SessionTally::new("p".into(), 1, 0).unwrap();
    assert_eq!((one.prefix(), one.count(), one.here()), ("p", 1, 0));
    assert!(!one.all_here());
    assert_eq!(one.text(), "1 escalation answered allow for `p`");
    let mixed = SessionTally::new("p".into(), 14, 3).unwrap();
    assert_eq!(mixed.count(), 14);
    assert_eq!(mixed.here(), 3);
    let all = SessionTally::new("p".into(), 2, 2).unwrap();
    assert_eq!(all.count(), 2);
    assert!(all.all_here());
    assert_eq!(all.text(), "2 escalations answered allow for `p`");
    assert!(SessionTally::new("p".into(), 2, 1).is_some());
    assert!(SessionTally::new("p".into(), 2, 3).is_none());
    for (count, here) in [(0, 0), (2, 3)] {
        assert!(
            from_value::<SessionTally>(
                json!({"prefix":"p","count":count,"here":here,"text":"ignored"})
            )
            .is_err()
        );
    }
}

#[test]
fn allows_tallies_validate_each_boundary_and_describe_the_count() {
    assert!(AllowsTally::new("".into(), "acme-cli query".into(), 5, 5).is_none());
    assert!(AllowsTally::new("acme-cli query".into(), "".into(), 5, 5).is_none());
    assert!(
        AllowsTally::new(
            "acme-cli query".into(),
            "acme-cli query --json".into(),
            0,
            0
        )
        .is_none()
    );
    let one = AllowsTally::new(
        "acme-cli query".into(),
        "acme-cli query --json".into(),
        1,
        0,
    )
    .unwrap();
    assert_eq!(
        (one.prefix(), one.subject(), one.count(), one.here()),
        ("acme-cli query", "acme-cli query --json", 1, 0)
    );
    assert!(!one.all_here());
    assert_eq!(one.text(), "1 reviewer allow for acme-cli query --json");
    let all = AllowsTally::new(
        "acme-cli query".into(),
        "acme-cli query --json".into(),
        5,
        5,
    )
    .unwrap();
    assert_eq!(all.count(), 5);
    assert_eq!(all.here(), 5);
    assert!(all.all_here());
    assert_eq!(all.text(), "5 reviewer allows for acme-cli query --json");
    assert!(
        AllowsTally::new(
            "acme-cli query".into(),
            "acme-cli query --json".into(),
            5,
            4
        )
        .is_some()
    );
    assert!(
        AllowsTally::new(
            "acme-cli query".into(),
            "acme-cli query --json".into(),
            5,
            6
        )
        .is_none()
    );
    for (count, here) in [(0, 0), (5, 6)] {
        assert!(
            from_value::<AllowsTally>(
                json!({"subject":"acme-cli query --json","count":count,"here":here,"text":"ignored"})
            )
            .is_err()
        );
    }
    // The wire carries no prefix and no eligibility: a decoded tally is
    // display-only, with an empty prefix.
    let inner = json!({"subject":"acme-cli query --json","count":5,"here":5,"text":"5 reviewer allows for acme-cli query --json"});
    let tally: AllowsTally = from_value(inner.clone()).unwrap();
    assert_eq!(tally.prefix(), "");
    assert_eq!(tally.subject(), "acme-cli query --json");
    assert_eq!((tally.count(), tally.here()), (5, 5));
    // An empty subject never decodes, even with an empty prefix.
    assert!(
        from_value::<AllowsTally>(json!({"subject":"","count":1,"here":1,"text":"t"})).is_err()
    );
    let back = to_value(tally).unwrap();
    assert_eq!(back, inner);
    assert!(back.get("prefix").is_none(), "the wire holds no prefix");
    let wire = json!({"kind":"allows","subject":"acme-cli query --json","count":5,"here":5,"text":"5 reviewer allows for acme-cli query --json"});
    let item: Evidence = from_value(wire.clone()).unwrap();
    assert_eq!(item.kind(), EvidenceKind::Allows);
    assert_eq!(item.text(), "5 reviewer allows for acme-cli query --json");
    assert_eq!(to_value(item).unwrap(), wire);
}

#[test]
fn allows_reasons_append_to_the_text_with_no_wire_key() {
    // Each exact withheld-reason sentence the setup shows, pinned word
    // for word: the later topics reuse these strings.
    for reason in [
        "one word: an entry names a subcommand",
        "not proposed: a call it would cover was denied, blocked or escalated",
        "takes a value: an entry can't limit it",
        "words after the command: an entry would cover other subcommands",
        "a path, not a command name: an entry names a command",
    ] {
        let tally = AllowsTally::new("acme-cli".into(), "acme-cli query --json".into(), 5, 5)
            .unwrap()
            .with_reason(reason);
        assert_eq!(
            tally.text(),
            format!("5 reviewer allows for acme-cli query --json ({reason})")
        );
        let wire = to_value(Evidence::Allows(tally)).unwrap();
        assert_eq!(
            wire.get("text"),
            Some(&json!(format!(
                "5 reviewer allows for acme-cli query --json ({reason})"
            ))),
        );
        assert!(wire.get("prefix").is_none(), "the wire holds no prefix");
        assert!(wire.get("reason").is_none());
    }
}

#[test]
fn write_shaped_flags_hold_exactly_the_posted_thirteen() {
    assert_eq!(
        WRITE_SHAPED_FLAGS,
        &[
            "-o",
            "--output",
            "--out",
            "--output-file",
            "-w",
            "--write",
            "-i",
            "--in-place",
            "--fix",
            "--apply",
            "--force",
            "--delete",
            "--save",
        ]
    );
}

#[test]
fn builtin_names_hold_builtins_and_their_slash_forms_but_no_sample_data() {
    for name in [
        "pwd", "echo", "ls", "cat", "head", "tail", "wc", "grep", "rg", "find", "sort", "nl",
        "git", "gh", "true", ":", "cd", "env", "timeout", "xargs", "sed",
    ] {
        assert!(
            BUILTIN_READ_ONLY_NAMES.contains(&name),
            "{name} is a built-in name"
        );
        for slashed in [format!("/bin/{name}"), format!("/usr/bin/{name}")] {
            assert!(
                BUILTIN_READ_ONLY_NAMES.contains(&slashed.as_str()),
                "{slashed} is a built-in slash form"
            );
        }
    }
    for name in [
        "git status",
        "git branch --show-current",
        "git diff",
        "git log",
        "git show",
        "git rev-parse",
        "git merge-base",
        "git ls-files",
        "git blame",
        "git stash list",
        "gh pr view",
        "gh pr list",
        "gh pr diff",
        "gh pr checks",
        "gh issue view",
        "gh issue list",
        "gh repo view",
        "gh run list",
        "gh run view",
    ] {
        assert!(
            BUILTIN_READ_ONLY_NAMES.contains(&name),
            "{name} is a built-in command path"
        );
    }
    for foreign in ["acme-cli", "acme-cli query"] {
        assert!(
            !BUILTIN_READ_ONLY_NAMES.contains(&foreign),
            "{foreign} is sample data, not built in"
        );
    }
}

#[test]
fn allowed_redirects_hold_the_quoted_allow_list() {
    assert_eq!(
        ALLOWED_REDIRECT_TOKENS,
        &[
            ">/dev/null",
            "1>/dev/null",
            "2>/dev/null",
            ">&1",
            ">&2",
            "1>&1",
            "1>&2",
            "2>&1",
            "2>&2",
            "<",
            "0<",
        ]
    );
}

#[test]
fn read_only_entries_round_trip_and_reject_strictly() {
    for (command, flags) in [
        ("acme-cli query", vec!["--json"]),
        ("acme-cli query", Vec::<&str>::new()),
        ("acme-cli query", vec!["--json", "-p"]),
        ("my \"quoted\" tool", vec!["--json"]),
        ("back\\slash", vec!["--x"]),
    ] {
        let flags: Vec<String> = flags.into_iter().map(str::to_owned).collect();
        let rendered = render_read_only_entry(command, &flags);
        let (back_command, back_flags) =
            parse_read_only_entry(&rendered).unwrap_or_else(|| panic!("{rendered} parses"));
        assert_eq!(back_command, command);
        assert_eq!(back_flags, flags);
    }
    assert_eq!(
        render_read_only_entry("acme-cli query", &["--json".to_owned()]),
        "shell.read_only.\"acme-cli query\".flags = [\"--json\"]"
    );
    for text in [
        "",
        "shell.read_only.\"acme-cli query\".flags = [] trailing",
        "shell.read_only.\"acme-cli query\".flags = [] ",
        "shell.read_only.\"\".flags = []",
        "shell.read_only.\"acme-cli query\".flags = [1]",
        "shell.read_only.\"acme-cli query\".flags = {}",
        "shell.read_only.\"acme-cli query\".flags = [--json]",
        "shell.read_only.\"acme-cli query\".flags = [\"--json\",]",
        "shell.read_only.acme-cli query.flags = [\"--json\"]",
        "shell.read_only.\"acme-cli query\" = [\"--json\"]",
        "shell.read_only.\"acme-cli query\".flags = \"--json\"",
        "shell.read_only.\"acme-cli \\q\".flags = []",
        "shell.read_only.\"unterminated.flags = []",
        "other.\"acme-cli query\".flags = []",
    ] {
        assert!(parse_read_only_entry(text).is_none(), "{text} is refused");
    }
}

#[test]
fn proposals_require_citations_in_rust_as_well_as_on_the_wire() {
    assert!(Proposed::new("line".into(), vec![]).is_err());
    let proposal = Proposed::new("line".into(), vec![0, 2]).unwrap();
    assert_eq!(proposal.text(), "line");
    assert_eq!(proposal.cites(), [0, 2]);
    assert_eq!(
        to_value(proposal).unwrap(),
        json!({"text":"line","cites":[0,2]})
    );
}

#[test]
fn evidence_and_proposals_validate_their_wire_shapes() {
    for (wire, kind, text) in [
        (
            json!({"kind":"files","text":"remote"}),
            EvidenceKind::Files,
            "remote",
        ),
        (
            json!({"kind":"answer","text":"ours"}),
            EvidenceKind::Answer,
            "ours",
        ),
        (
            json!({"kind":"session","prefix":"p","count":1,"here":1,"text":"1 escalation answered allow for `p`"}),
            EvidenceKind::Session,
            "1 escalation answered allow for `p`",
        ),
        (
            json!({"kind":"allows","subject":"acme-cli query --json","count":5,"here":5,"text":"5 reviewer allows for acme-cli query --json"}),
            EvidenceKind::Allows,
            "5 reviewer allows for acme-cli query --json",
        ),
    ] {
        let item: Evidence = from_value(wire.clone()).unwrap();
        assert_eq!(item.kind(), kind);
        assert_eq!(item.text(), text);
        assert_eq!(to_value(item).unwrap(), wire);
    }
    assert!(from_value::<Evidence>(json!({"kind":"files","text":"remote","prefix":"p"})).is_err());
    assert!(from_value::<Evidence>(json!({"kind":"answer","text":"ours","prefix":"p"})).is_err());
    assert!(
        from_value::<Evidence>(
            json!({"kind":"session","prefix":"p","count":1,"here":2,"text":"bad"})
        )
        .is_err()
    );
    assert!(from_value::<Proposed>(json!({"text":"line","cites":[]})).is_err());
    let wire = json!({"text":"line","cites":[0]});
    assert_eq!(
        to_value(from_value::<Proposed>(wire.clone()).unwrap()).unwrap(),
        wire
    );
    let ask = NotesAsk {
        args: ProposeNotesArgs {
            topic: Topic::Never,
            answer: None,
            earlier: vec![],
        },
        evidence: vec![Evidence::Files {
            text: "remote".into(),
        }],
    };
    assert_eq!(ask.args.topic, Topic::Never);
    assert_eq!(ask.evidence.len(), 1);
}

#[test]
fn userinfo_is_removed_from_every_url_authority_only() {
    for (input, expected) in [
        ("https://user:token@host/x.git", "https://host/x.git"),
        (
            "registry=https://user:token@host/",
            "registry=https://host/",
        ),
        ("git@host:x", "git@host:x"),
        ("https://u:p@host:8443/x", "https://host:8443/x"),
        ("ssh://git@host/x", "ssh://host/x"),
        ("https://host/a@b", "https://host/a@b"),
        ("https://u@a/ https://v@b/", "https://a/ https://b/"),
        ("https://u%40x:p%3Aq@h/", "https://h/"),
        ("plain café @ text", "plain café @ text"),
        ("https://a@b@host", "https://host"),
        ("https://host?q=@keep", "https://host?q=@keep"),
        ("https://host#@keep", "https://host#@keep"),
        ("https://host @keep", "https://host @keep"),
        ("https://host\"@keep", "https://host\"@keep"),
        ("https://host'@keep", "https://host'@keep"),
        ("https://host>@keep", "https://host>@keep"),
    ] {
        assert_eq!(strip_userinfo(input), expected, "{input}");
        assert_eq!(strip_userinfo(&strip_userinfo(input)), expected);
    }
}
