use super::*;

#[test]
fn an_ack_debugs_without_its_callback() {
    assert_eq!(format!("{:?}", Ack(Box::new(|_| {}))), "Ack(..)");
}
