//! Tests for what the terminal does with the hub's `attention` lines: the
//! OSC 9 support check, reading a line, its text, and its bytes.

use super::{Attention, Line, Reason, bytes, parse, supported, text};
use serde_json::{Map, Value, json};

/// An environment reader from `pairs`.
fn var(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let owned: Vec<(String, String)> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    move |name: &str| {
        owned
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

#[test]
fn supported_terminals() {
    let truthy: &[&[(&str, &str)]] = &[
        &[("TERM_PROGRAM", "ghostty")],
        &[("TERM_PROGRAM", "iTerm.app")],
        &[("TERM_PROGRAM", "WezTerm")],
        &[("TERM", "xterm-ghostty")],
        &[("TERM", "xterm-kitty")],
        &[("KITTY_WINDOW_ID", "1")],
    ];
    for env in truthy {
        assert!(supported(var(env)), "supported: {env:?}");
        let mut tmux = env.to_vec();
        tmux.push(("TMUX", "x"));
        assert!(!supported(var(&tmux)), "under tmux: {tmux:?}");
        let mut sty = env.to_vec();
        sty.push(("STY", "x"));
        assert!(!supported(var(&sty)), "under screen: {sty:?}");
    }
    assert!(!supported(var(&[])));
    assert!(!supported(var(&[("TERM_PROGRAM", "Apple_Terminal")])));
    assert!(!supported(var(&[("TERM", "xterm-256color")])));
}

/// An `attention` payload with `entries` overlaid on the waiting base.
fn payload(entries: &[(&str, Value)]) -> Map<String, Value> {
    let mut base = json!({
        "session_id": "s_aaaaaaaaaaaaaaaa",
        "name": "fix tests",
        "workspace": "/w",
        "reason": "waiting",
        "summary": "approval: Run cargo test",
    });
    for (key, value) in entries {
        base[key] = value.clone();
    }
    base.as_object().cloned().unwrap_or_default()
}

#[test]
fn parse_reads_waiting_and_finished() {
    assert_eq!(
        parse(&payload(&[])),
        Some(Line {
            session: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
            name: "fix tests".to_owned(),
            reason: Reason::Waiting {
                summary: "approval: Run cargo test".to_owned(),
            },
        })
    );
    assert_eq!(
        parse(&payload(&[("reason", json!("finished"))])),
        Some(Line {
            session: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
            name: "fix tests".to_owned(),
            reason: Reason::Finished,
        })
    );
}

#[test]
fn unknown_reason_or_missing_id_is_nothing() {
    assert_eq!(parse(&payload(&[("reason", json!("other"))])), None);
    let mut missing = payload(&[]);
    missing.remove("reason");
    assert_eq!(parse(&missing), None);
    let mut missing = payload(&[]);
    missing.remove("session_id");
    assert_eq!(parse(&missing), None);
    assert_eq!(parse(&payload(&[("session_id", json!(7))])), None);
}

#[test]
fn an_empty_or_missing_name_uses_the_session_id() {
    assert_eq!(
        parse(&payload(&[("name", json!(""))])).map(|line| line.name),
        Some("s_aaaaaaaaaaaaaaaa".to_owned())
    );
    let mut missing = payload(&[]);
    missing.remove("name");
    assert_eq!(
        parse(&missing).map(|line| line.name),
        Some("s_aaaaaaaaaaaaaaaa".to_owned())
    );
}

/// One line with `name` and a waiting `summary`.
fn line(name: &str, summary: &str) -> Line {
    Line {
        session: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        name: name.to_owned(),
        reason: Reason::Waiting {
            summary: summary.to_owned(),
        },
    }
}

#[test]
fn the_text_for_waiting_and_finished() {
    assert_eq!(
        text(&line("fix tests", "approval: Run cargo test")),
        "Fiber: fix tests needs you: approval: Run cargo test"
    );
    assert_eq!(
        text(&Line {
            session: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
            name: "fix tests".to_owned(),
            reason: Reason::Finished,
        }),
        "Fiber: fix tests finished"
    );
    assert_eq!(text(&line("fix tests", "")), "Fiber: fix tests needs you");
}

#[test]
fn name_and_summary_are_cut() {
    let name = "é".repeat(60);
    assert_eq!(
        text(&line(&name, "")).chars().count(),
        "Fiber: ".len() + 60 + " needs you".len()
    );
    let cut = "é".repeat(61);
    assert_eq!(text(&line(&cut, "")), format!("Fiber: {name} needs you"));
    let summary = "é".repeat(120);
    assert!(text(&line("fix tests", &summary)).ends_with(&summary));
    let long = "é".repeat(121);
    assert!(text(&line("fix tests", &long)).ends_with(&summary));
    assert!(!text(&line("fix tests", &long)).ends_with(&long));
}

#[test]
fn controls_and_del_are_dropped() {
    let dirty = "a\x1bb\x07c\u{9b}d\u{7f}e\nf";
    assert_eq!(text(&line(dirty, dirty)), "Fiber: abcdef needs you: abcdef");
    // The cut counts only what is left: 60 kept characters with controls
    // between them stay whole.
    let padded = format!("{}\x1b", "é".repeat(60));
    assert_eq!(
        text(&line(&padded, "")),
        format!("Fiber: {} needs you", "é".repeat(60))
    );
}

#[test]
fn the_text_never_starts_with_a_digit() {
    assert_eq!(text(&line("4;50", "x")), "Fiber: 4;50 needs you: x");
}

#[test]
fn bytes_table() {
    let waiting = line("fix tests", "approval: Run cargo test");
    let both = Attention {
        notification: true,
        bell: true,
        title: true,
    };
    let body = "Fiber: fix tests needs you: approval: Run cargo test";
    let mut osc9 = format!("\x1b]9;{body}").into_bytes();
    osc9.push(0x07);
    // Notification on and supported: the OSC 9 bytes only, no extra bell.
    assert_eq!(bytes(&waiting, both, true), osc9);
    // Notification off and supported: nothing, not even a bell.
    assert_eq!(
        bytes(
            &waiting,
            Attention {
                notification: false,
                ..both
            },
            true
        ),
        Vec::<u8>::new()
    );
    // Unsupported and the bell on: one bell.
    assert_eq!(bytes(&waiting, both, false), vec![0x07]);
    // Unsupported and the bell off: nothing.
    assert_eq!(
        bytes(
            &waiting,
            Attention {
                bell: false,
                ..both
            },
            false
        ),
        Vec::<u8>::new()
    );
    // Notification off but unsupported: the bell still rings.
    assert_eq!(
        bytes(
            &waiting,
            Attention {
                notification: false,
                ..both
            },
            false
        ),
        vec![0x07]
    );
}
