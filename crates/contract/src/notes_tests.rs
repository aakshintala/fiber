use super::*;
use serde_json::{from_value, json, to_value};

#[test]
fn topics_follow_the_notes_order_and_wire_contract() {
    let expected = [
        (
            "source_control",
            "### Source control",
            "Which accounts, organisations and repositories are your own or your organisation's?",
        ),
        (
            "internal_hosts",
            "### Internal hosts",
            "Which internal hosts, registries and services count as inside?",
        ),
        (
            "internal_tools",
            "### Internal tools",
            "Which internal tools do you run, and what does running each one do?",
        ),
        (
            "routine_actions",
            "### Routine actions",
            "What routine actions, such as squash-merging your own pull requests once CI passes, should the reviewer allow?",
        ),
        (
            "never",
            "### What must never happen",
            "What must never happen, such as touching infra/prod?",
        ),
    ];
    for (topic, (id, heading, question)) in Topic::ALL.into_iter().zip(expected) {
        assert_eq!(to_value(topic).unwrap(), id);
        assert_eq!(from_value::<Topic>(json!(id)).unwrap(), topic);
        assert_eq!(topic.heading(), heading);
        assert_eq!(topic.question(), question);
        assert!(!topic.covers().is_empty());
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
    let all = SessionTally::new("p".into(), 2, 2).unwrap();
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
