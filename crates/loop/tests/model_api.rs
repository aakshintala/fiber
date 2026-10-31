//! `loop::Model::from_resolved` builds a session model from the resolved
//! reference and its metadata, keeping all three fields.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use r#loop::Model;

#[test]
fn from_resolved_keeps_reference_cost_and_subscription() {
    let cost = contract::provider::Cost {
        input: 1.0,
        output: 2.0,
        cache_read: Some(0.5),
        cache_write: None,
        tiers: Vec::new(),
    };
    let model = Model::from_resolved("provider/model".to_owned(), Some(cost.clone()), true);
    assert_eq!(model.reference, "provider/model");
    assert_eq!(model.cost, Some(cost));
    assert!(model.subscription);
}
