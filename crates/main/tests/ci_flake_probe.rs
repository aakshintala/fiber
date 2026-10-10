//! Temporary probe for #218: fails on the first attempt, passes on the retry.
#[test]
fn passes_only_on_a_retry() {
    let attempt = std::env::var("NEXTEST_ATTEMPT").unwrap_or_default();
    assert_ne!(attempt, "1", "first attempt fails on purpose");
}
