use contract::ErrorCode;
use contract::events::{CallStatus, Control, Event, NamedBy, Notice, SessionNamed};
use contract::shapes::Failure;

use super::{Named, Naming, PINNED, by_person};

fn named(name: Option<&str>, by: NamedBy) -> Event {
    Event::SessionNamed(SessionNamed {
        name: name.map(str::to_owned),
        by,
    })
}

fn other() -> Event {
    Event::Notice(Notice {
        code: ErrorCode::BudgetExceeded,
        message: "over".into(),
        extension: None,
    })
}

#[test]
fn fold_tracks_the_latest_session_named() {
    type Case<'a> = (Vec<(Option<&'a str>, NamedBy)>, bool);
    let rows: Vec<Case<'_>> = vec![
        (vec![], false),
        (vec![(Some("Mine"), NamedBy::Person)], true),
        (vec![(None, NamedBy::Person)], false),
        (vec![(Some("Old"), NamedBy::Model)], false),
        (
            vec![(Some("Mine"), NamedBy::Person), (None, NamedBy::Person)],
            false,
        ),
        (
            vec![
                (Some("Mine"), NamedBy::Person),
                (Some("New"), NamedBy::Model),
            ],
            false,
        ),
        (
            vec![
                (Some("Old"), NamedBy::Model),
                (Some("Mine"), NamedBy::Person),
            ],
            true,
        ),
    ];
    for (events, pinned) in rows {
        let mut naming = Naming::default();
        for (name, by) in events {
            naming.fold(&named(name, by));
        }
        assert_eq!(naming.pinned, pinned);
    }
}

#[test]
fn fold_ignores_other_events() {
    for pinned in [false, true] {
        let mut naming = Naming::default();
        if pinned {
            naming.fold(&named(Some("Mine"), NamedBy::Person));
        }
        naming.fold(&other());
        assert_eq!(naming.pinned, pinned);
    }
}

fn control_with(name: Option<&str>) -> Control {
    Control {
        handoff: None,
        questions: None,
        skill: None,
        name: name.map(str::to_owned),
    }
}

#[test]
fn judge_writes_only_a_completed_call_with_a_name() {
    let with_name = control_with(Some("Fix the parser"));
    let handoff_only = Control {
        handoff: Some("note".into()),
        questions: None,
        skill: None,
        name: None,
    };
    let statuses = [
        CallStatus::Completed,
        CallStatus::Failed,
        CallStatus::Cancelled,
        CallStatus::Denied,
    ];
    for status in statuses {
        for (pinned, control) in [
            (false, None),
            (false, Some(&handoff_only)),
            (true, None),
            (true, Some(&handoff_only)),
        ] {
            let naming = if pinned {
                let mut naming = Naming::default();
                naming.fold(&named(Some("Mine"), NamedBy::Person));
                naming
            } else {
                Naming::default()
            };
            assert!(matches!(naming.judge(status, control), Named::Nothing));
        }
        if status != CallStatus::Completed {
            assert!(matches!(
                Naming::default().judge(status, Some(&with_name)),
                Named::Nothing
            ));
        }
    }

    let Named::Write(wrote) = Naming::default().judge(CallStatus::Completed, Some(&with_name))
    else {
        panic!("expected a write");
    };
    assert_eq!(
        wrote,
        SessionNamed {
            name: Some("Fix the parser".into()),
            by: NamedBy::Model,
        }
    );

    let mut pinned = Naming::default();
    pinned.fold(&named(Some("Mine"), NamedBy::Person));
    let Named::Pinned(failure) = pinned.judge(CallStatus::Completed, Some(&with_name)) else {
        panic!("expected a pin");
    };
    assert_eq!(
        failure,
        Failure {
            code: ErrorCode::NamePinned,
            message: PINNED.to_owned(),
            retry_after_ms: None,
            provider: None,
        }
    );
}

#[test]
fn by_person_clears_only_on_the_exact_empty_string() {
    let cleared = by_person(String::new());
    assert_eq!(cleared.name, None);
    assert_eq!(cleared.by, NamedBy::Person);
    for text in ["Mine", "  Mine  ", " ", &"x".repeat(61)] {
        let kept = by_person(text.to_owned());
        assert_eq!(kept.name.as_deref(), Some(text));
        assert_eq!(kept.by, NamedBy::Person);
    }
}
