use std::cell::RefCell;

#[test]
fn shutdown_publishes_before_releasing_callbacks() {
    let cancel = r#loop::TurnCancel::default();
    let order = RefCell::new(Vec::new());
    let observed = RefCell::new(None);
    super::shutdown_steps(
        143,
        |code| {
            cancel.shutdown(code);
            order.borrow_mut().push("publish");
        },
        || {
            *observed.borrow_mut() = cancel.shutdown_code();
            order.borrow_mut().push("callbacks");
        },
        || order.borrow_mut().push("rest"),
    );
    assert_eq!(*observed.borrow(), Some(143));
    assert_eq!(*order.borrow(), ["publish", "callbacks", "rest"]);
}
