use super::*;

#[test]
fn two_pins_on_a_page_need_two_unpins() {
    let mut pins = Pins::default();
    pins.pin(3);
    pins.pin(3);
    pins.unpin(3);
    assert!(pins.contains(3), "one pin is still held");
    pins.unpin(3);
    assert!(!pins.contains(3), "the second unpin releases the page");
}

#[test]
fn unpin_of_an_unpinned_page_does_nothing() {
    let mut pins = Pins::default();
    pins.unpin(2);
    assert!(!pins.contains(2), "no pin appears from nowhere");
    pins.pin(1);
    pins.unpin(2);
    assert!(pins.contains(1), "the absent-page unpin leaves page 1 held");
}
