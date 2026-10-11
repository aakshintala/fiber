//! The admission lock: rows over `admit`, `close` and `close_unless`.

use super::Admission;

#[test]
fn admit_runs_send_while_open() {
    let admission = Admission::default();
    let mut called = 0;
    let out = admission.admit(|| {
        called += 1;
        7
    });
    assert_eq!(out, Some(7));
    assert_eq!(called, 1);
}

#[test]
fn admit_after_close_unless_empty_returns_none_without_calling_send() {
    let admission = Admission::default();
    assert_eq!(admission.close_unless(|| None::<()>), None);
    let mut called = 0;
    let out = admission.admit(|| {
        called += 1;
    });
    assert_eq!(out, None);
    assert_eq!(called, 0);
}

#[test]
fn admit_after_close_returns_none_without_calling_send() {
    let admission = Admission::default();
    admission.close();
    let mut called = 0;
    let out = admission.admit(|| {
        called += 1;
    });
    assert_eq!(out, None);
    assert_eq!(called, 0);
}

#[test]
fn close_unless_returning_some_leaves_it_open() {
    let admission = Admission::default();
    assert_eq!(admission.close_unless(|| Some(1)), Some(1));
    let mut called = 0;
    let out = admission.admit(|| {
        called += 1;
        2
    });
    assert_eq!(out, Some(2));
    assert_eq!(called, 1);
}

#[test]
fn close_unless_once_closed_still_calls_last_and_stays_closed() {
    let admission = Admission::default();
    admission.close();
    let mut calls = 0;
    let out = admission.close_unless(|| {
        calls += 1;
        Some(3)
    });
    assert_eq!(out, Some(3));
    assert_eq!(calls, 1);
    let mut called = 0;
    assert_eq!(
        admission.admit(|| {
            called += 1;
        }),
        None
    );
    assert_eq!(called, 0);
}

#[test]
fn close_and_close_unless_are_idempotent_once_closed() {
    let admission = Admission::default();
    admission.close();
    admission.close();
    assert_eq!(admission.close_unless(|| None::<()>), None);
    let mut called = 0;
    assert_eq!(
        admission.admit(|| {
            called += 1;
        }),
        None
    );
    assert_eq!(called, 0);
}
