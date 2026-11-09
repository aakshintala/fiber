use std::any::TypeId;

#[test]
fn shared_helpers_keep_the_free_half_type_identity() {
    assert_eq!(
        TypeId::of::<crate::Deadline>(),
        TypeId::of::<fakes::Deadline>()
    );
    assert_eq!(
        TypeId::of::<crate::TempDir>(),
        TypeId::of::<fakes::TempDir>()
    );
    assert_eq!(
        TypeId::of::<crate::CancelToken>(),
        TypeId::of::<fakes::CancelToken>()
    );
    assert_eq!(
        TypeId::of::<crate::clock::FakeClock>(),
        TypeId::of::<fakes::clock::FakeClock>()
    );
    assert_eq!(
        TypeId::of::<crate::clock::SystemClock>(),
        TypeId::of::<fakes::clock::SystemClock>()
    );
}

#[test]
fn no_note_has_no_note_or_notices() {
    let answer = crate::no_note();
    assert!(matches!(
        answer.outcome,
        contract::hook::BeforeCompactionOutcome::NoNote
    ));
    assert!(answer.notices.is_empty());
}
