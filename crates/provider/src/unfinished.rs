//! What a call that ended without a reply carries (`docs/events.md`,
//! "Usage and notices"): the decoder's partial when it failed mid-stream,
//! the completed reply's usage when the cancel landed after the decode, or
//! an unnamed usage when no stream was read.

use contract::GenerationId;
use contract::provider::{CallUsage, InputSize, Reply};

use crate::Error;

/// What a `run` puts in its `CallError`: the decode's partial when it
/// failed, the completed reply's usage when the cancel landed after it, and
/// an unnamed usage when no stream was read (`None`). Every call reports
/// what it saw, so each writes its `usage_recorded`. The call's
/// `input_size` is set on all three.
pub(crate) fn carried(
    decoded: &Result<Reply, (Error, Option<CallUsage>)>,
    input_size: InputSize,
) -> Box<CallUsage> {
    let mut usage = match decoded {
        Err((_, Some(partial))) => partial.clone(),
        Err((_, None)) => CallUsage::unnamed(input_size),
        Ok(reply) => reply.usage(),
    };
    usage.input_size = input_size;
    Box::new(usage)
}

/// The generation a decoder saw: `None` until the stream named one, so no
/// usage carries an empty id.
pub(crate) fn named(id: String) -> Option<GenerationId> {
    (!id.is_empty()).then_some(GenerationId(id))
}
