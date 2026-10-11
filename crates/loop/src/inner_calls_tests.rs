//! The inner-call seam, slots and cancellation.

use std::sync::Arc;

use contract::tool::Ask;
use contract::ActionId;

use crate::progress::{SharedWake, Stream};

#[test]
fn a_stream_without_an_inner_slot_has_no_runner() {
    let stream = Stream::new(Arc::new(SharedWake::default()), ActionId("a_outer".into()), true);
    assert!(stream.inner_calls().is_none());
}
