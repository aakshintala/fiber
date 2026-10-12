//! Pairing records, secrets, consumption and revocation under an injected clock.
use super::*;
use crate::testkit::args;
use serde_json::json;

#[test]
fn a_code_is_ten_characters_of_the_alphabet() {
    assert_eq!(code_from([0, 31, 32, 255, 1, 2, 3, 4, 5, 6]).0, "0z0z123456");
}

#[test]
fn a_token_is_43_base64url_characters() {
    let bytes = std::array::from_fn(|i| u8::try_from(i).unwrap());
    assert_eq!(token_from(bytes).0, "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8");
    assert_eq!(format!("{:?}", token_from(bytes)), "DeviceToken(..)");
    assert_eq!(format!("{:?}", code_from([0; 10])), "PairingCode(..)");
}

#[test]
fn device_names_hold_to_the_rule() {
    for name in ["a", "A0_-.", "x.", &"a".repeat(63), &"a".repeat(64)] {
        assert!(DeviceName::parse(name).is_ok(), "{name}");
    }
    for name in ["", ".x", "..", "a/b", "local", "é", &"a".repeat(65)] {
        let (code, message) = DeviceName::parse(name).err().unwrap();
        assert_eq!(code, ErrorCode::InvalidArguments);
        assert_eq!(message, NAME_RULE);
    }
}

#[test]
fn pairing_code_devices_and_revoke_reject_wrong_args() {
    for value in [json!({}), json!({"device":1}), json!({"device":"a", "extra":1})] {
        assert!(name(&args(value)).is_err());
    }
}
