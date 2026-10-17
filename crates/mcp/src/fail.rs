//! The `Output` one [`McpError`](crate::error::McpError) makes.

use contract::shapes::Failure;
use contract::tool::Output;

use crate::error::McpError;

/// One failed output from a case: its code and its sentence, with no
/// retry and no provider.
pub(crate) fn failed(error: &McpError) -> Output {
    Output {
        error: Some(Failure::from(error)),
        ..Output::default()
    }
}
