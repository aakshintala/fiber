use contract::events::CacheLifetime;
use contract::provider::ModelRequest;

use super::Responses;
use crate::Endpoint;

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        effort: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "s_root".into(),
        conversation: Vec::new(),
        previous_end: None,
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
