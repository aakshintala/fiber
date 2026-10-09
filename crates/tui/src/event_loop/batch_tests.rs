//! Tests for taking one batch of ready inputs: hub lines and ticks
//! share a frame in arrival order, anything else keeps its own.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver};

use super::{HUB_BATCH, batch};
use crate::Input;
use crate::link::Line;

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
            | Input::Files { .. }
            | Input::FindDue(_)
            | Input::Image { .. }
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
    let got = batch(hub(0), &mut stash, &rx);
    assert_eq!(described(&got), ["hub-0", "hub-1", "hub-2"]);
    assert!(stash.is_empty());
}

#[test]
fn ticks_batch_with_the_lines() {
    let (_tx, rx) = channel(vec![Input::Tick, hub(1)]);
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx);
    assert_eq!(described(&got), ["hub-0", "tick", "hub-1"]);
    assert!(stash.is_empty());
}

#[test]
fn a_key_ends_the_batch_and_waits_ahead() {
    let (_tx, rx) = channel(vec![hub(1), hub(2), key(), hub(3)]);
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx);
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
    let got = batch(hub(9), &mut stash, &rx);
    assert_eq!(described(&got), ["hub-9", "hub-10", "hub-11", "hub-12"]);
    assert!(stash.is_empty());
}

#[test]
fn exactly_the_cap_makes_one_batch() {
    let (_tx, rx) = channel((1..HUB_BATCH as u64).map(hub).collect());
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx);
    assert_eq!(got.len(), HUB_BATCH);
    let kinds = described(&got);
    assert_eq!(
        kinds.iter().take(3).cloned().collect::<Vec<_>>(),
        ["hub-0", "hub-1", "hub-2"]
    );
    assert!(rx.try_recv().is_err());
    assert!(stash.is_empty());
}

#[test]
fn one_past_the_cap_makes_two_batches() {
    let (_tx, rx) = channel((1..=HUB_BATCH as u64).map(hub).collect());
    let mut stash = VecDeque::new();
    let first = batch(hub(0), &mut stash, &rx);
    assert_eq!(first.len(), HUB_BATCH);
    let next = rx.try_recv().unwrap_or_else(|err| panic!("recv: {err}"));
    let second = batch(next, &mut stash, &rx);
    assert_eq!(described(&second), [format!("hub-{}", HUB_BATCH)]);
    assert!(stash.is_empty());
}

#[test]
fn an_empty_channel_gives_a_batch_of_one() {
    let (_tx, rx) = channel(Vec::new());
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx);
    assert_eq!(described(&got), ["hub-0"]);
    assert!(stash.is_empty());
}

#[test]
fn a_closed_channel_keeps_the_first_input() {
    let (tx, rx) = channel(Vec::new());
    drop(tx);
    let mut stash = VecDeque::new();
    let got = batch(hub(0), &mut stash, &rx);
    assert_eq!(described(&got), ["hub-0"]);
    assert!(stash.is_empty());
}

#[test]
fn a_non_batchable_first_is_alone() {
    let (_tx, rx) = channel(vec![hub(2)]);
    let mut stash = VecDeque::from([hub(1)]);
    let got = batch(key(), &mut stash, &rx);
    assert_eq!(described(&got), ["bytes"]);
    // Nothing taken: the stash and the channel wait as they were.
    assert_eq!(described(stash.make_contiguous()), ["hub-1"]);
    assert!(matches!(rx.try_recv(), Ok(Input::Hub(_))));
}
