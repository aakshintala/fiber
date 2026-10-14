//! Tests for the frame reader and writer: RFC 6455's masked "Hello", the
//! length encodings on read and write, fragmentation with interleaved
//! control frames, the UTF-8 and close-code rules, the caps, and EOF.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::Cursor;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;

use super::*;

const MASK: [u8; 4] = [0x37, 0xFA, 0x21, 0x3D];

/// One client frame to send: the length encoding is minimal for `None`,
/// or forced to the 16- or 64-bit form for `Some(126)` and `Some(127)`.
fn send(opcode: u8, fin: bool, rsv: u8, masked: bool, long: Option<u8>, payload: &[u8]) -> Vec<u8> {
    let mut first = opcode;
    if fin {
        first |= 0x80;
    }
    first |= rsv;
    let mut out = vec![first];
    let mut second = if masked { 0x80 } else { 0x00 };
    match long {
        None if payload.len() < 126 => {
            second |= u8::try_from(payload.len()).unwrap();
            out.push(second);
        }
        None if payload.len() <= 65_535 => {
            second |= 126;
            out.push(second);
            out.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
        }
        None => {
            second |= 127;
            out.push(second);
            out.extend_from_slice(&u64::try_from(payload.len()).unwrap().to_be_bytes());
        }
        Some(kind) => {
            second |= kind;
            out.push(second);
            if kind == 126 {
                out.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
            } else {
                out.extend_from_slice(&u64::try_from(payload.len()).unwrap().to_be_bytes());
            }
        }
    }
    if masked {
        out.extend_from_slice(&MASK);
        out.extend(
            payload
                .iter()
                .zip(MASK.iter().cycle())
                .map(|(byte, mask)| byte ^ mask),
        );
    } else {
        out.extend_from_slice(payload);
    }
    out
}

/// A masked FIN text frame, the usual client message.
fn text(payload: &[u8]) -> Vec<u8> {
    send(0x1, true, 0x00, true, None, payload)
}

/// A masked FIN ping.
fn ping(payload: &[u8]) -> Vec<u8> {
    send(0x9, true, 0x00, true, None, payload)
}

/// A masked FIN close carrying `code` and `reason`.
fn close(code: u16, reason: &[u8]) -> Vec<u8> {
    let mut payload = code.to_be_bytes().to_vec();
    payload.extend_from_slice(reason);
    send(0x8, true, 0x00, true, None, &payload)
}

/// Reads one item with the first-message cap.
fn read(bytes: &[u8]) -> Incoming {
    Reader::new(FIRST_MESSAGE_MAX).next(&mut Cursor::new(bytes))
}

/// How long one read may take: a whole or truncated message answers at
/// once, so only a mutant spinning the payload loop runs this out.
const READ_WITHIN: Duration = Duration::from_secs(10);

/// Reads one item on a thread and receives it before [`READ_WITHIN`]:
/// under a mutant that spins the payload loop the wait expires and
/// fails the test in seconds instead of the mutant timeout.
#[track_caller]
fn read_within(bytes: &[u8], what: &str) -> Incoming {
    let bytes = bytes.to_vec();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stream = Cursor::new(bytes);
        let got = Reader::new(FIRST_MESSAGE_MAX).next(&mut stream);
        done_tx.send(got).unwrap_or(());
    });
    Deadline::after(READ_WITHIN).recv_or_fail(&done_rx, what)
}

#[test]
fn rfc_6455_masked_hello_reads() {
    assert_eq!(
        read(&[
            0x81, 0x85, 0x37, 0xFA, 0x21, 0x3D, 0x7F, 0x9F, 0x4D, 0x51, 0x58
        ]),
        Incoming::Text("Hello".to_owned())
    );
}

#[test]
fn the_unmasked_hello_is_written_without_a_mask_and_closes_1002() {
    let unmasked = send(0x1, true, 0x00, false, None, b"Hello");
    assert_eq!(unmasked, vec![0x81, 0x05, b'H', b'e', b'l', b'l', b'o']);
    assert_eq!(read(&unmasked), Incoming::Fail(1002));
}

#[test]
fn written_lengths_use_the_minimal_encoding() {
    let frame = text_frame(&[b'a'; 125]);
    assert!(
        frame.starts_with(&[0x81, 125]),
        "125 takes the 2-byte header"
    );
    assert_eq!(frame.len(), 2 + 125);
    let frame = text_frame(&[b'a'; 126]);
    assert!(
        frame.starts_with(&[0x81, 126, 0, 126]),
        "126 takes the 4-byte header"
    );
    assert_eq!(frame.len(), 4 + 126);
    let frame = text_frame(&[b'a'; 65_535]);
    assert!(
        frame.starts_with(&[0x81, 126, 0xFF, 0xFF]),
        "65535 takes the 4-byte header"
    );
    assert_eq!(frame.len(), 4 + 65_535);
    let frame = text_frame(&[b'a'; 65_536]);
    assert!(
        frame.starts_with(&[0x81, 127, 0, 0, 0, 0, 0, 1, 0, 0]),
        "65536 takes the 10-byte header"
    );
    assert_eq!(frame.len(), 10 + 65_536);
}

#[test]
fn each_length_reads_its_payload() {
    for len in [125, 126, 65_535, 65_536] {
        let payload = vec![b'a'; len];
        let Incoming::Text(text) = read(&text(&payload)) else {
            panic!("a {len}-byte message reads at {len}");
        };
        assert_eq!(text.len(), len, "a {len}-byte message keeps its length");
        assert!(
            text.bytes().all(|byte| byte == b'a'),
            "a {len}-byte message keeps its bytes"
        );
    }
}

#[test]
fn a_64_bit_length_with_the_top_bit_set_closes_1002() {
    let mut bytes = vec![0x81, 0xFF, 0x80, 0, 0, 0, 0, 0, 0, 1];
    bytes.extend_from_slice(&MASK);
    assert_eq!(read(&bytes), Incoming::Fail(1002));
}

#[test]
fn each_rsv_bit_set_closes_1002() {
    for rsv in [0x10, 0x20, 0x40] {
        assert_eq!(
            read(&send(0x1, true, rsv, true, None, b"Hello")),
            Incoming::Fail(1002),
            "RSV bit {rsv:#x} is refused"
        );
    }
}

#[test]
fn an_unknown_opcode_closes_1002() {
    assert_eq!(
        read(&send(0x3, true, 0x00, true, None, b"Hello")),
        Incoming::Fail(1002)
    );
}

#[test]
fn a_ping_with_fin_clear_closes_1002() {
    assert_eq!(
        read(&send(0x9, false, 0x00, true, None, b"x")),
        Incoming::Fail(1002)
    );
}

#[test]
fn ping_payloads_of_125_read_and_126_close_1002() {
    let payload = vec![b'p'; 125];
    assert_eq!(read(&ping(&payload)), Incoming::Ping(payload));
    assert_eq!(
        read(&send(0x9, true, 0x00, true, None, &[b'p'; 126])),
        Incoming::Fail(1002)
    );
}

#[test]
fn a_pong_is_ignored() {
    assert_eq!(pong_frame(b"hi"), vec![0x8A, 2, b'h', b'i']);
    assert_eq!(
        read(&send(0xA, true, 0x00, true, None, b"hi")),
        Incoming::Pong
    );
}

#[test]
fn fragments_assemble_with_a_ping_between_them() {
    let mut reader = Reader::new(FIRST_MESSAGE_MAX);
    let mut stream = Cursor::new(
        [
            send(0x1, false, 0x00, true, None, b"Hel"),
            ping(b"x"),
            send(0x0, true, 0x00, true, None, b"lo"),
        ]
        .concat(),
    );
    assert_eq!(reader.next(&mut stream), Incoming::Ping(b"x".to_vec()));
    assert_eq!(reader.next(&mut stream), Incoming::Text("Hello".to_owned()));
}

#[test]
fn a_continuation_with_no_message_open_closes_1002() {
    assert_eq!(
        read(&send(0x0, true, 0x00, true, None, b"lo")),
        Incoming::Fail(1002)
    );
}

#[test]
fn a_new_text_frame_while_one_is_open_closes_1002() {
    let mut reader = Reader::new(FIRST_MESSAGE_MAX);
    let mut stream = Cursor::new(
        [
            send(0x1, false, 0x00, true, None, b"Hel"),
            send(0x1, true, 0x00, true, None, b"lo"),
        ]
        .concat(),
    );
    assert_eq!(reader.next(&mut stream), Incoming::Fail(1002));
}

#[test]
fn a_binary_message_closes_1003() {
    assert_eq!(
        read(&send(0x2, true, 0x00, true, None, b"x")),
        Incoming::Fail(1003)
    );
}

#[test]
fn a_four_byte_character_split_across_fragments_is_accepted() {
    // U+20AC split after its first byte.
    let mut reader = Reader::new(FIRST_MESSAGE_MAX);
    let mut stream = Cursor::new(
        [
            send(0x1, false, 0x00, true, None, &[0xE2]),
            send(0x0, true, 0x00, true, None, &[0x82, 0xAC]),
        ]
        .concat(),
    );
    assert_eq!(reader.next(&mut stream), Incoming::Text("€".to_owned()));
}

#[test]
fn invalid_utf_8_closes_1007() {
    assert_eq!(read(&text(&[0xFF])), Incoming::Fail(1007));
}

#[test]
fn the_first_message_cap_counts_payload_bytes() {
    for len in [FIRST_MESSAGE_MAX - 1, FIRST_MESSAGE_MAX] {
        let bytes = text(&vec![b'a'; len]);
        let Incoming::Text(text) = Reader::new(FIRST_MESSAGE_MAX).next(&mut Cursor::new(bytes))
        else {
            panic!("{len} bytes read at {len}");
        };
        assert_eq!(text.len(), len, "{len} bytes keep their length");
    }
    assert_eq!(
        read(&text(&[b'a'; FIRST_MESSAGE_MAX + 1])),
        Incoming::Fail(1009)
    );
}

#[test]
fn the_first_message_cap_sums_fragments() {
    let mut reader = Reader::new(FIRST_MESSAGE_MAX);
    let mut stream = Cursor::new(
        [
            send(0x1, false, 0x00, true, None, &[b'a'; FIRST_MESSAGE_MAX]),
            send(0x0, true, 0x00, true, None, b"b"),
        ]
        .concat(),
    );
    assert_eq!(reader.next(&mut stream), Incoming::Fail(1009));
}

#[test]
fn close_frames_parse_and_echo() {
    assert_eq!(
        read(&close(1000, b"bye")),
        Incoming::Close(Some((1000, "bye".to_owned())))
    );
    assert_eq!(
        read(&send(0x8, true, 0x00, true, None, b"")),
        Incoming::Close(None)
    );
    assert_eq!(
        read(&send(0x8, true, 0x00, true, None, b"x")),
        Incoming::Fail(1002)
    );
    let mut reason = vec![0x03, 0xE8];
    reason.push(0xFF);
    assert_eq!(read(&close(1000, &reason)), Incoming::Fail(1007));
    assert_eq!(echo_close(None), vec![0x88, 0x00]);
}

#[test]
fn close_codes_an_endpoint_may_send_are_echoed() {
    for code in [1000, 1003, 1007, 1014, 3000, 4999] {
        assert_eq!(
            read(&close(code, b"")),
            Incoming::Close(Some((code, String::new()))),
            "code {code} is accepted"
        );
        assert_eq!(
            echo_close(Some(code)),
            close_frame(code, ""),
            "code {code} echoes"
        );
    }
}

#[test]
fn close_codes_no_endpoint_may_send_close_1002() {
    for code in [999, 1004, 1005, 1006, 1015, 2999, 5000] {
        assert_eq!(
            read(&close(code, b"")),
            Incoming::Fail(1002),
            "code {code} is refused"
        );
    }
}

#[test]
fn short_lengths_in_long_forms_close_1002() {
    assert_eq!(
        read(&send(0x1, true, 0x00, true, Some(126), &[b'a'; 125])),
        Incoming::Fail(1002)
    );
    let Incoming::Text(text) = read(&send(0x1, true, 0x00, true, Some(126), &[b'a'; 126])) else {
        panic!("126 bytes in the 16-bit form read");
    };
    assert_eq!(text.len(), 126);
    assert_eq!(
        read(&send(0x1, true, 0x00, true, Some(127), &[b'a'; 65_535])),
        Incoming::Fail(1002)
    );
    let Incoming::Text(text) = read(&send(0x1, true, 0x00, true, Some(127), &[b'a'; 65_536]))
    else {
        panic!("65536 bytes in the 64-bit form read");
    };
    assert_eq!(text.len(), 65_536);
}

#[test]
fn the_stopping_close_bytes_are_exact() {
    let mut want = vec![0x88, 0x16, 0x03, 0xE9];
    want.extend_from_slice(b"The hub is stopping.");
    assert_eq!(close_frame(1001, "The hub is stopping."), want);
}

#[test]
fn eof_in_a_header_or_a_payload_reads_as_gone() {
    assert_eq!(read(b""), Incoming::Gone);
    assert_eq!(read(&[0x81]), Incoming::Gone);
    assert_eq!(read(&[0x81, 0x85, 0x37]), Incoming::Gone);
    let partial = text(b"Hello");
    let partial = partial.get(..7).unwrap();
    assert_eq!(read(partial), Incoming::Gone);
}

#[test]
fn the_payload_loop_stops_when_no_bytes_remain() {
    // No bytes remain before the first round, one round reads one byte,
    // and more than one round spans the per-round take: a `>=` in the
    // loop condition spins past zero on every one of these.
    assert_eq!(
        read_within(&text(b""), "an empty message reads"),
        Incoming::Text(String::new())
    );
    assert_eq!(
        read_within(&text(b"a"), "a one-byte message reads"),
        Incoming::Text("a".to_owned())
    );
    let payload = vec![b'a'; 5_000];
    let Incoming::Text(got) = read_within(&text(&payload), "a multi-round message reads") else {
        panic!("a multi-round message reads as text");
    };
    assert_eq!(got.len(), 5_000, "a multi-round message keeps its length");
}

#[test]
fn a_truncated_payload_reads_as_gone_without_hanging() {
    // Fewer bytes than the header claims, and none at all: accepting a
    // short read keeps a loop that never advances, so these hang when
    // the full-read guard is broken.
    let frame = text(b"Hello");
    let partial = frame.get(..7).unwrap();
    assert_eq!(
        read_within(partial, "a short payload reads as gone"),
        Incoming::Gone
    );
    let header = frame.get(..6).unwrap();
    assert_eq!(
        read_within(header, "an absent payload reads as gone"),
        Incoming::Gone
    );
}

#[test]
fn two_frames_back_to_back_read_in_order() {
    let mut reader = Reader::new(FIRST_MESSAGE_MAX);
    let mut stream = Cursor::new([text(b"one"), ping(b"two")].concat());
    assert_eq!(reader.next(&mut stream), Incoming::Text("one".to_owned()));
    assert_eq!(reader.next(&mut stream), Incoming::Ping(b"two".to_vec()));
}
