//! The signing seam (`docs/model-routing.md`, "Signing a request"): a
//! provider whose every request carries a signature, such as AWS SigV4, is
//! asked for the headers to add just before each request is sent, a retry
//! included. It adds headers only: the body goes out as it was built.

/// Adds a signature to each request a provider sends.
pub trait Signer: Send + Sync {
    /// The headers to add to `request`, or why it could not be signed. An
    /// extension's `sign()` is handed the body's SHA-256, never the body.
    fn sign(&self, request: &SignRequest<'_>) -> Result<Vec<(String, String)>, String>;
}

/// A request about to be sent.
#[derive(Debug, Clone, Copy)]
pub struct SignRequest<'a> {
    /// The method, such as `POST`.
    pub method: &'a str,
    /// The full URL.
    pub url: &'a str,
    /// The headers it carries so far.
    pub headers: &'a [(String, String)],
    /// The body bytes.
    pub body: &'a [u8],
}
