use super::*;

#[test]
fn an_ack_debugs_without_its_callback() {
    assert_eq!(format!("{:?}", Ack(Box::new(|_| {}))), "Ack(..)");
}

#[test]
fn a_claim_debugs_without_its_callback() {
    assert_eq!(format!("{:?}", Claim(Box::new(|| true))), "Claim(..)");
}

#[test]
fn a_job_line_delivery_carries_its_batch() {
    let line = crate::events::JobLine {
        job_id: crate::JobId("j_1".into()),
        lines: "ok".into(),
        suppressed: Some(2),
    };
    let Delivery::JobLine(carried) = Delivery::JobLine(line.clone()) else {
        panic!("not a job line");
    };
    assert_eq!(carried, line);
}
