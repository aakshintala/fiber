use std::sync::Arc;

use super::Settle;

/// `Settle` prints opaque and compares by identity, so the loop's logs
/// never carry a queue's contents and two calls' settles never compare
/// equal.
#[test]
fn settle_debug_and_equality_are_by_identity() {
    let first = Settle(Arc::new(Vec::<crate::jobs::Taken>::new));
    let same = first.clone();
    assert_eq!(format!("{:?}", first), "Settle(..)");
    assert_eq!(first, same);
    let other = Settle(Arc::new(Vec::<crate::jobs::Taken>::new));
    assert_ne!(first, other);
}
