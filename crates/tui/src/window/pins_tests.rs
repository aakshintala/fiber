use super::*;

#[test]
fn two_pins_on_a_page_need_two_unpins() {
    let mut pins = Pins::default();
    pins.pin(3);
    pins.pin(3);
    pins.unpin(3);
    assert!(pins.contains(3), "one pin is still held");
    assert_eq!(pins.pages().collect::<Vec<_>>(), [3]);
    pins.unpin(3);
    assert!(!pins.contains(3));
    assert_eq!(pins.pages().count(), 0);
}

#[test]
fn unpin_of_an_unpinned_page_does_nothing() {
    let mut pins = Pins::default();
    pins.unpin(2);
    assert!(!pins.contains(2));
    assert_eq!(pins.total(), 0);
    pins.pin(1);
    pins.unpin(2);
    assert!(pins.contains(1));
    assert_eq!(pins.total(), 1);
}

#[test]
fn total_sums_the_counts() {
    let mut pins = Pins::default();
    pins.pin(0);
    pins.pin(4);
    pins.pin(4);
    assert_eq!(pins.total(), 3);
    assert_eq!(pins.pages().collect::<Vec<_>>(), [0, 4]);
}
