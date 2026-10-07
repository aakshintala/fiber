use contract::events::CacheLifetime;
use contract::provider::ModelRequest;

use super::Responses;
use crate::Endpoint;

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "s_root".into(),
        conversation: Vec::new(),
        previous_end: None,
        max_output_tokens: None,
        session_dir: std::path::PathBuf::new(),
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

#[test]
fn the_cache_key_goes_in_the_declared_header_and_nowhere_else_without_one() {
    let plain = Responses::new(Endpoint::default()).request(&request());
    assert_eq!(header(&plain.headers, "x-opencode-session"), None);

    let keyed = Responses::new(Endpoint::default())
        .cache_key_header("x-opencode-session")
        .request(&request());
    assert_eq!(header(&keyed.headers, "x-opencode-session"), Some("s_root"));
    assert_eq!(
        header(&keyed.headers, "user-agent"),
        Some(concat!("fiber/", env!("CARGO_PKG_VERSION")))
    );
}

fn sent_limit(extra: Option<u64>, requested: Option<u64>) -> Option<u64> {
    let mut endpoint = Endpoint::default();
    if let Some(limit) = extra {
        endpoint
            .extra_body
            .insert("max_output_tokens".into(), limit.into());
    }
    let mut asked = request();
    asked.max_output_tokens = requested;
    let call = Responses::new(endpoint).request(&asked);
    let body: serde_json::Value = serde_json::from_slice(&call.body).unwrap();
    body["max_output_tokens"].as_u64()
}

#[test]
fn an_extra_body_output_limit_and_the_requests_keep_the_smaller() {
    assert_eq!(sent_limit(Some(100), Some(4096)), Some(100));
    assert_eq!(sent_limit(Some(5000), Some(300)), Some(300));
    assert_eq!(sent_limit(Some(100), None), Some(100));
    assert_eq!(sent_limit(None, Some(300)), Some(300));
    assert_eq!(sent_limit(None, None), None);
    assert_eq!(sent_limit(Some(4), Some(4096)), Some(16));
}
