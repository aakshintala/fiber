//! The `provider_request` line's `data` shape (`docs/state.md`, "Diagnostic
//! logs"): fields in declared order, absent options left out.

#![allow(clippy::unwrap_used, reason = "test code")]

use super::*;

fn full() -> ProviderRequest {
    ProviderRequest {
        provider: "fake".into(),
        model: Some("m".into()),
        purpose: Purpose::ModelCall,
        host: Some("127.0.0.1:50731".into()),
        path: Some("/v1/responses".into()),
        status: Some(200),
        attempt: 1,
        request_bytes: 2214,
        response_bytes: 913,
        headers_ms: Some(41),
        first_token_ms: Some(57),
        total_ms: 180,
    }
}

#[test]
fn a_provider_request_writes_present_fields_in_order_and_leaves_absent_ones_out() {
    assert_eq!(
        serde_json::to_string(&full()).unwrap(),
        "{\"provider\":\"fake\",\"model\":\"m\",\"purpose\":\"model_call\",\
         \"host\":\"127.0.0.1:50731\",\"path\":\"/v1/responses\",\"status\":200,\
         \"attempt\":1,\"request_bytes\":2214,\"response_bytes\":913,\
         \"headers_ms\":41,\"first_token_ms\":57,\"total_ms\":180}"
    );
    let request = ProviderRequest {
        model: None,
        host: None,
        path: None,
        status: None,
        headers_ms: None,
        first_token_ms: None,
        purpose: Purpose::ModelList,
        response_bytes: 0,
        ..full()
    };
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        "{\"provider\":\"fake\",\"purpose\":\"model_list\",\"attempt\":1,\
         \"request_bytes\":2214,\"response_bytes\":0,\"total_ms\":180}"
    );
}
