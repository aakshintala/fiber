use super::Endpoint;
use crate::Error;
use base64::{Engine, prelude::BASE64_STANDARD as STANDARD};
use std::io::{self, Read};
fn accept(_key: &str) -> String {
    String::new()
}
pub(super) fn key() -> io::Result<String> {
    Ok(String::new())
}
pub(super) fn request(_endpoint: &Endpoint, _key: &str) -> String {
    String::new()
}
pub(super) fn reply(_read: &mut impl Read, _key: &str) -> Result<Vec<u8>, Error> {
    Ok(Vec::new())
}
#[cfg(test)]
#[path = "handshake_tests.rs"]
mod tests;
