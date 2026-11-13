//! Project settings read by the hub.

use super::keep_exited;
use crate::test_support;

#[test]
fn keep_exited_uses_the_configured_count_and_defaults_to_ten() {
    // `docs/configuration.md`, "Keys": the documented default is 10 and
    // the count accepts zero as a configured value.
    let default = test_support::config("keep-exited-default", Vec::<String>::new());
    assert_eq!(keep_exited(&default), 10);

    let keep_none = test_support::config(
        "keep-exited-zero",
        vec![String::from("session.keep_exited=0")],
    );
    assert_eq!(keep_exited(&keep_none), 0);

    let keep_three = test_support::config(
        "keep-exited-three",
        vec![String::from("session.keep_exited=3")],
    );
    assert_eq!(keep_exited(&keep_three), 3);
}
