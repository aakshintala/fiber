use super::is_minted_session_id;

#[test]
fn accepts_only_hub_minted_lowercase_session_ids() {
    assert!(is_minted_session_id("s_0123456789abcdef"));
    assert!(is_minted_session_id("s_aaaaaaaaaaaaaaaa"));

    for id in [
        "",
        "s_0123456789abcde",
        "s_0123456789abcdef0",
        "x_0123456789abcdef",
        "s_0123456789abcdeG",
        "s_0123456789abcdeF",
        "s_0123456789abcde-",
        "s_0123456789abcdeé",
    ] {
        assert!(!is_minted_session_id(id), "accepted {id:?}");
    }
}
