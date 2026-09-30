//! What another crate sees of `contract`: every type a payload holds can be
//! named from outside it.

use contract::events::{Event, ExtensionStateSet, OnFork};

#[test]
fn an_extension_state_write_can_be_built_and_its_fork_rule_matched() {
    let event = Event::ExtensionStateSet(ExtensionStateSet {
        extension: "e".into(),
        key: "k".into(),
        value: serde_json::json!(1),
        on_fork: OnFork::Latest,
    });
    let Event::ExtensionStateSet(set) = &event else {
        panic!("not an extension_state_set");
    };
    let gets = match set.on_fork {
        OnFork::AtPoint => "the value at the point",
        OnFork::Latest => "the latest value",
        OnFork::Fresh => "nothing",
    };
    assert_eq!(gets, "the latest value");
    assert_eq!(event.payload().unwrap()["on_fork"], "latest");
}
