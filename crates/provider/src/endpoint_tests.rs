use super::Endpoint;

fn endpoint(max_output_tokens: Option<u64>) -> Endpoint {
    Endpoint {
        max_output_tokens,
        ..Endpoint::default()
    }
}

#[test]
fn output_limit_is_the_smaller_of_the_two_that_are_set() {
    assert_eq!(endpoint(None).output_limit(None), None);
    assert_eq!(endpoint(None).output_limit(Some(1)), Some(1));
    assert_eq!(endpoint(Some(4096)).output_limit(None), Some(4096));
    assert_eq!(endpoint(Some(4096)).output_limit(Some(1)), Some(1));
    assert_eq!(endpoint(Some(4096)).output_limit(Some(9000)), Some(4096));
}
