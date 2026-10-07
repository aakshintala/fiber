use contract::Secret;

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

#[test]
fn debug_prints_neither_the_key_nor_a_header_value() {
    let planted = "sk-planted-4c1e9b";
    let endpoint = Endpoint {
        provider: "acme".into(),
        key: Some(Secret::new(planted.into())),
        headers: vec![("x-api-key".into(), planted.into())],
        ..Endpoint::default()
    };
    let printed = format!("{endpoint:?}");
    assert!(printed.contains("acme"), "{printed}");
    assert!(printed.contains("x-api-key"), "{printed}");
    assert!(!printed.contains(planted), "{printed}");
}
