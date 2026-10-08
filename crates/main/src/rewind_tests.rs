//! Tests for the point a rewound session starts from: what its old log's
//! last line must hold.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use serde_json::json;

use super::*;
use contract::shapes::Point;

fn envelope(kind: &str, payload: serde_json::Value) -> Envelope {
    serde_json::from_value(json!({
        "kind": kind, "session_id": "s_aaaaaaaaaaaaaaaa",
        "ts": 2, "schema_version": 1, "seq": 5, "payload": payload,
    }))
    .unwrap()
}

fn rewound(new: &str, from: Option<&str>) -> serde_json::Value {
    let mut payload = json!({"new_session_id": new, "seq": 3, "jobs": []});
    if let Some(from) = from {
        payload["from_session_id"] = from.into();
    }
    payload
}

#[test]
fn point_of_reads_the_old_session_and_its_seq() {
    let old = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let new = SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    assert_eq!(
        point_of(Some(envelope("rewound", rewound(&new.0, None))), &new, &old).unwrap(),
        Point {
            session_id: old.clone(),
            seq: contract::Seq(3),
        }
    );
}

#[test]
fn point_of_reads_the_ancestor_an_ancestor_point_names() {
    let old = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let ancestor = SessionId("s_cccccccccccccccc".to_owned());
    let new = SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    assert_eq!(
        point_of(
            Some(envelope("rewound", rewound(&new.0, Some(&ancestor.0)))),
            &new,
            &old
        )
        .unwrap(),
        Point {
            session_id: ancestor,
            seq: contract::Seq(3),
        }
    );
}

#[test]
fn point_of_refuses_anything_but_a_rewound_naming_this_session() {
    let old = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let new = SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    let other = SessionId("s_dddddddddddddddd".to_owned());
    let cases: Vec<(&str, Option<Envelope>)> = vec![
        ("an empty log", None),
        (
            "a last line that is not rewound",
            Some(envelope("fiber_exited", json!({}))),
        ),
        (
            "a rewound naming another session",
            Some(envelope("rewound", rewound(&other.0, None))),
        ),
        (
            "a rewound whose payload does not parse",
            Some(envelope("rewound", json!({"new_session_id": new.0}))),
        ),
    ];
    for (what, last) in cases {
        match point_of(last, &new, &old) {
            Err(failure) => {
                assert_eq!(failure.code, ErrorCode::InvalidArguments, "{what}");
                assert!(
                    failure.message.contains(&old.0) && failure.message.contains(&new.0),
                    "{what}: {}",
                    failure.message
                );
            }
            Ok(point) => panic!("{what} gives a point: {point:?}"),
        }
    }
}
