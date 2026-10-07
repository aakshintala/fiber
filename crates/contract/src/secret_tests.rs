use super::*;

#[test]
fn expose_returns_the_value() {
    let secret = Secret::new("sk-planted-4c1e9b".into());
    assert_eq!(secret.expose(), "sk-planted-4c1e9b");
}

#[test]
fn debug_names_a_secret_and_prints_no_value() {
    let secret = Secret::new("sk-planted-4c1e9b".into());
    assert_eq!(format!("{secret:?}"), "Secret(redacted)");
}
