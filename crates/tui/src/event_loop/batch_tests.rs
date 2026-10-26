//! Tests for taking one batch of ready inputs: hub lines and ticks
//! share a frame in arrival order, anything else keeps its own.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use super::{HOLD, HUB_BATCH, batch, batchable, joins};
use crate::Input;
use crate::link::{Credits, Line};

/// One hub line named `hub-{seq}`.
fn hub(seq: u64) -> Input {
    Input::Hub(Line::Session(contract::Envelope {
        kind: format!("hub-{seq}"),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(contract::Seq(seq)),
        payload: serde_json::Map::new(),
    }))
}

/// A keystroke: not batchable, so it keeps its own frame.
fn key() -> Input {
    Input::Bytes(b"a".to_vec())
}

/// Unbounded credits: `batch` never waits on capacity in these tests.
fn credits() -> Arc<Credits> {
    Credits::new(usize::MAX)
}

/// What each input is, in order.
fn described(inputs: &[Input]) -> Vec<String> {
    inputs
        .iter()
        .map(|input| match input {
            Input::Hub(Line::Session(line)) => line.kind.clone(),
            Input::Hub(Line::Hub(line)) => line.kind.clone(),
            Input::Bytes(_) => "bytes".to_owned(),
            Input::Tick => "tick".to_owned(),
            Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::Resize
            | Input::Signal(_)
            | Input::Files { .. }
            | Input::FindDue(_)
            | Input::Image { .. }
            | Input::Feedback(_)
            | Input::Viewed { .. }
            | Input::Install { .. }
            | Input::HooksOfferNames { .. }
            | Input::HooksInstalled { .. }
            | Input::Login { .. }
            | Input::Models(_) => "other".to_owned(),
        })
        .collect()
}

/// The channel holding `inputs`, still open.
fn channel(inputs: Vec<Input>) -> (mpsc::Sender<Input>, Receiver<Input>) {
    let (tx, rx) = mpsc::channel();
    for input in inputs {
        tx.send(input).unwrap_or_else(|err| panic!("send: {err}"));
    }
    (tx, rx)
}

#[test]
fn ready_lines_batch_in_order() {
    let (_tx, rx) = channel(vec![hub(1), hub(2)]);
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["hub-0", "hub-1", "hub-2"]);
    assert!(stash.is_empty());
}

#[test]
fn ticks_batch_with_the_lines() {
    let (_tx, rx) = channel(vec![Input::Tick, hub(1)]);
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["hub-0", "tick", "hub-1"]);
    assert!(stash.is_empty());
}

#[test]
fn a_key_ends_the_batch_and_waits_ahead() {
    let (_tx, rx) = channel(vec![hub(1), hub(2), key(), hub(3)]);
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["hub-0", "hub-1", "hub-2"]);
    // The key runs next, ahead of the line still in the channel.
    assert_eq!(described(stash.make_contiguous()), ["bytes"]);
    let rest = rx.try_recv().unwrap_or_else(|err| panic!("recv: {err}"));
    assert_eq!(described(std::slice::from_ref(&rest)), ["hub-3"]);
}

#[test]
fn the_stash_comes_before_the_channel() {
    let (_tx, rx) = channel(vec![hub(12)]);
    let mut stash = VecDeque::from([hub(10), hub(11)]);
    let got = batch(hub(9), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["hub-9", "hub-10", "hub-11", "hub-12"]);
    assert!(stash.is_empty());
}

#[test]
fn exactly_the_chunk_makes_one_chunk() {
    let (_tx, rx) = channel((1..HOLD as u64).map(hub).collect());
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(got.len(), HOLD);
    let kinds = described(&got);
    assert_eq!(
        kinds.iter().take(3).cloned().collect::<Vec<_>>(),
        ["hub-0", "hub-1", "hub-2"]
    );
    assert!(rx.try_recv().is_err());
    assert!(stash.is_empty());
}

#[test]
fn one_past_the_chunk_makes_two_chunks() {
    let (_tx, rx) = channel((1..=HOLD as u64).map(hub).collect());
    let mut stash = VecDeque::new();
    let first = batch(hub(0), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(first.len(), HOLD);
    let next = rx.try_recv().unwrap_or_else(|err| panic!("recv: {err}"));
    let second = batch(next, &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&second), [format!("hub-{HOLD}")]);
    assert!(stash.is_empty());
}

#[test]
fn an_empty_channel_gives_a_batch_of_one() {
    let (_tx, rx) = channel(Vec::new());
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["hub-0"]);
    assert!(stash.is_empty());
}

#[test]
fn a_closed_channel_keeps_the_first_input() {
    let (tx, rx) = channel(Vec::new());
    drop(tx);
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["hub-0"]);
    assert!(stash.is_empty());
}

#[test]
fn a_stashed_key_ends_the_batch_and_stays_first() {
    let (_tx, rx) = channel(vec![hub(2)]);
    let mut stash = VecDeque::from([hub(1), key()]);
    let got = batch(hub(0), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["hub-0", "hub-1"]);
    // The key runs next, ahead of the line still in the channel.
    assert_eq!(described(stash.make_contiguous()), ["bytes"]);
    let rest = rx.try_recv().unwrap_or_else(|err| panic!("recv: {err}"));
    assert_eq!(described(std::slice::from_ref(&rest)), ["hub-2"]);
}

#[test]
fn a_non_batchable_first_is_alone() {
    let (_tx, rx) = channel(vec![hub(2)]);
    let mut stash = VecDeque::from([hub(1)]);
    let got = batch(key(), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["bytes"]);
    // Nothing taken: the stash and the channel wait as they were.
    assert_eq!(described(stash.make_contiguous()), ["hub-1"]);
    assert!(matches!(rx.try_recv(), Ok(Input::Hub(_))));
}

#[test]
fn hub_lines_and_ticks_hold_and_nothing_else_does() {
    assert!(batchable(&hub(0)));
    assert!(batchable(&Input::Tick));
    assert!(!batchable(&key()));
    assert!(!batchable(&Input::Resize));
    assert!(!batchable(&Input::Disconnected));
}

#[test]
fn joins_takes_two_reads_or_two_batchable_inputs() {
    assert!(joins(&key(), &key()));
    assert!(joins(&hub(0), &hub(1)));
    assert!(joins(&hub(0), &Input::Tick));
    assert!(!joins(&key(), &hub(0)));
    assert!(!joins(&hub(0), &key()));
    assert!(!joins(&key(), &Input::Resize));
    assert!(!joins(&Input::Resize, &Input::Resize));
}

#[test]
fn bytes_batch_with_bytes_in_order() {
    let (_tx, rx) = channel(vec![key(), key()]);
    let mut stash = VecDeque::new();
    let got = batch(key(), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["bytes", "bytes", "bytes"]);
    assert!(stash.is_empty());
}

#[test]
fn a_hub_line_ends_a_bytes_batch_and_stays_first() {
    let (_tx, rx) = channel(vec![key(), hub(1)]);
    let mut stash = VecDeque::new();
    let got = batch(key(), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["bytes", "bytes"]);
    assert_eq!(described(stash.make_contiguous()), ["hub-1"]);
}

#[test]
fn exactly_the_cap_makes_one_bytes_batch() {
    let (_tx, rx) = channel((1..HUB_BATCH).map(|_| key()).collect());
    let mut stash = VecDeque::new();
    let got = batch(key(), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(got.len(), HUB_BATCH);
    assert!(rx.try_recv().is_err());
    assert!(stash.is_empty());
}

#[test]
fn a_closed_channel_ends_a_bytes_batch_without_blocking() {
    let (tx, rx) = channel(Vec::new());
    drop(tx);
    let mut stash = VecDeque::new();
    let got = batch(key(), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(described(&got), ["bytes"]);
    assert!(stash.is_empty());
}

#[test]
fn left_edges_bound_a_hub_chunk() {
    // `left` 0 or 1 returns the first alone, taking nothing.
    for left in [0, 1] {
        let (_tx, rx) = channel(vec![hub(1), hub(2)]);
        let mut stash = VecDeque::new();
        let got = batch(hub(0), &mut stash, &rx, &credits(), left);
        assert_eq!(described(&got), ["hub-0"], "left {left}");
        assert!(matches!(rx.try_recv(), Ok(Input::Hub(_))));
        assert!(stash.is_empty());
    }
    // 63 and 64 fill to `left`; 65 is capped by the chunk.
    for (left, want) in [(63, 63), (64, 64), (65, 64)] {
        let (_tx, rx) = channel((1..130u64).map(hub).collect());
        let mut stash = VecDeque::new();
        let got = batch(hub(0), &mut stash, &rx, &credits(), left);
        assert_eq!(got.len(), want, "left {left}");
        assert!(stash.is_empty());
    }
}

#[test]
fn a_hub_run_of_130_returns_three_chunks() {
    let (_tx, rx) = channel((1..130u64).map(hub).collect());
    let mut stash = VecDeque::new();
    let first = batch(hub(0), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(first.len(), 64);
    let next = rx.try_recv().unwrap_or_else(|err| panic!("recv: {err}"));
    let second = batch(next, &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(second.len(), 64);
    let next = rx.try_recv().unwrap_or_else(|err| panic!("recv: {err}"));
    let third = batch(next, &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(third.len(), 2);
    assert!(rx.try_recv().is_err());
    assert!(stash.is_empty());
    // Ordered identities across chunks: the input order, none dropped
    // or duplicated.
    let kinds: Vec<String> = [first, second, third]
        .iter()
        .flat_map(|chunk| described(chunk))
        .collect();
    let want: Vec<String> = (0..130).map(|seq| format!("hub-{seq}")).collect();
    assert_eq!(kinds, want);
}

#[test]
fn ticks_count_toward_the_chunk() {
    let mut inputs = Vec::new();
    for seq in 1..=32u64 {
        inputs.push(Input::Tick);
        inputs.push(hub(seq));
    }
    // 64 ready behind the first: 64 returned, then one more.
    let (_tx, rx) = channel(inputs);
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx, &credits(), HUB_BATCH);
    assert_eq!(got.len(), 64);
    assert!(rx.try_recv().is_ok());
    assert!(stash.is_empty());
}

#[test]
fn a_bytes_batch_takes_up_to_left() {
    let (_tx, rx) = channel(vec![key(), key(), key()]);
    let mut stash = VecDeque::new();
    let got = batch(key(), &mut stash, &rx, &credits(), 2);
    assert_eq!(described(&got), ["bytes", "bytes"]);
    assert!(matches!(rx.try_recv(), Ok(Input::Bytes(_))));
    assert!(matches!(rx.try_recv(), Ok(Input::Bytes(_))));
    assert!(rx.try_recv().is_err());
    assert!(stash.is_empty());
}

#[test]
fn a_zero_cap_never_reaches_batch() {
    // Capacity never gates `batch`: it only releases receives, which
    // saturate at zero.
    let zero = Credits::new(0);
    let (_tx, rx) = channel(vec![hub(1), hub(2)]);
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx, &zero, HUB_BATCH);
    assert_eq!(described(&got), ["hub-0", "hub-1", "hub-2"]);
    assert!(stash.is_empty());
    // Stash-only inputs drain the same way: no receive, no release.
    let (_tx, rx) = channel(Vec::new());
    let mut stash = VecDeque::from([hub(4), hub(5)]);
    let got = batch(hub(3), &mut stash, &rx, &zero, HUB_BATCH);
    assert_eq!(described(&got), ["hub-3", "hub-4", "hub-5"]);
    assert!(stash.is_empty());
}
