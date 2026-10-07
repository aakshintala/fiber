//! What a call that ended without a reply carries (`docs/events.md`,
//! "Usage and notices"): the decoder's partial when it failed after the
//! provider named its generation, or the completed reply's usage when the
//! cancel landed after the decode.

use contract::provider::{CallUsage, InputSize, Reply};

use crate::Error;

/// What a `run` puts in its `CallError`: the decode's partial when it
/// failed, or the completed reply's usage when the cancel landed after it.
/// Either way the call's `input_size` is set. A decode that failed before
/// any generation id, and a completed reply with an empty id, carry none.
pub(crate) fn carried(
    decoded: &Result<Reply, (Error, Option<CallUsage>)>,
    input_size: InputSize,
) -> Option<Box<CallUsage>> {
    match decoded {
        Err((_, partial)) => partial.clone().map(|mut usage| {
            usage.input_size = input_size;
            Box::new(usage)
        }),
        Ok(reply) => {
            if reply.generation_id.0.is_empty() {
                None
            } else {
                let mut usage = reply.usage();
                usage.input_size = input_size;
                Some(Box::new(usage))
            }
        }
    }
}

#[cfg(test)]
#[path = "unfinished_tests.rs"]
mod tests;
