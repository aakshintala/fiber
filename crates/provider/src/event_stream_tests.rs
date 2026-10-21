use std::io::Read;

use contract::ErrorCode;

use super::{Message, crc32, read};
use crate::Error;

/// One header value as the framing writes it: its type byte and its bytes.
pub(crate) enum Value<'a> {
    True,
    False,
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Bytes(&'a [u8]),
    Str(&'a str),
    Timestamp(i64),
    Uuid([u8; 16]),
}

/// One message framed as AWS frames it, both checksums computed.
pub(crate) fn frame(headers: &[(&str, Value<'_>)], payload: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for (name, value) in headers {
        encoded.push(u8::try_from(name.len()).unwrap());
        encoded.extend_from_slice(name.as_bytes());
        match value {
            Value::True => encoded.push(0),
            Value::False => encoded.push(1),
            Value::Byte(v) => {
                encoded.push(2);
                encoded.extend_from_slice(&v.to_be_bytes());
            }
            Value::Short(v) => {
                encoded.push(3);
                encoded.extend_from_slice(&v.to_be_bytes());
            }
            Value::Int(v) => {
                encoded.push(4);
                encoded.extend_from_slice(&v.to_be_bytes());
            }
            Value::Long(v) => {
                encoded.push(5);
                encoded.extend_from_slice(&v.to_be_bytes());
            }
            Value::Bytes(v) => {
                encoded.push(6);
                encoded.extend_from_slice(&u16::try_from(v.len()).unwrap().to_be_bytes());
                encoded.extend_from_slice(v);
            }
            Value::Str(v) => {
                encoded.push(7);
                encoded.extend_from_slice(&u16::try_from(v.len()).unwrap().to_be_bytes());
                encoded.extend_from_slice(v.as_bytes());
            }
            Value::Timestamp(v) => {
                encoded.push(8);
                encoded.extend_from_slice(&v.to_be_bytes());
            }
            Value::Uuid(v) => {
                encoded.push(9);
                encoded.extend_from_slice(v);
            }
        }
    }
    let total = u32::try_from(16 + encoded.len() + payload.len()).unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_be_bytes());
    let prelude_crc = crc32(&out);
    out.extend_from_slice(&prelude_crc.to_be_bytes());
    out.extend_from_slice(&encoded);
    out.extend_from_slice(payload);
    let message_crc = crc32(&out);
    out.extend_from_slice(&message_crc.to_be_bytes());
    out
}

fn event(payload: &str) -> Vec<u8> {
    frame(
        &[
            (":event-type", Value::Str("chunk")),
            (":content-type", Value::Str("application/json")),
            (":message-type", Value::Str("event")),
        ],
        payload.as_bytes(),
    )
}

fn messages(stream: impl Read) -> Result<Vec<Message>, Error> {
    let mut out = Vec::new();
    read(stream, |message| {
        out.push(message);
        Ok(false)
    })?;
    Ok(out)
}

/// Hands out its bytes `step` at a time, so a message arrives split across
/// reads.
pub(crate) struct Trickle {
    pub(crate) bytes: Vec<u8>,
    pub(crate) at: usize,
    pub(crate) step: usize,
}

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let end = (self.at + self.step).min(self.bytes.len());
        let n = (end - self.at).min(buf.len());
        buf[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

#[test]
fn the_checksum_is_crc32_iso_hdlc() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn the_empty_message_vector_reads_as_one_message_with_nothing_in_it() {
    // AWS's event-stream test suite, `encoded/positive/empty_message`.
    let bytes = [
        0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x05, 0xc2, 0x48, 0xeb, 0x7d, 0x98, 0xc8,
        0xff,
    ];
    assert_eq!(
        messages(&bytes[..]).unwrap(),
        [Message {
            headers: Vec::new(),
            payload: Vec::new(),
        }]
    );
}

#[test]
fn several_messages_in_one_read_each_arrive_in_order() {
    let mut stream = event(r#"{"n":1}"#);
    stream.extend(event(r#"{"n":2}"#));
    let read = messages(&stream[..]).unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].payload, br#"{"n":1}"#);
    assert_eq!(read[1].payload, br#"{"n":2}"#);
    assert_eq!(read[1].header(":event-type"), Some("chunk"));
    assert_eq!(read[1].header(":message-type"), Some("event"));
    assert_eq!(read[1].header(":missing"), None);
}

#[test]
fn a_message_split_across_reads_arrives_whole() {
    let mut bytes = event(r#"{"n":1}"#);
    bytes.extend(event(r#"{"n":2}"#));
    for step in [1, 3, 7, 13] {
        let read = messages(Trickle {
            bytes: bytes.clone(),
            at: 0,
            step,
        })
        .unwrap();
        assert_eq!(read.len(), 2, "step {step}");
        assert_eq!(read[0].payload, br#"{"n":1}"#, "step {step}");
        assert_eq!(read[1].payload, br#"{"n":2}"#, "step {step}");
    }
}

#[test]
fn a_message_the_stream_ends_inside_is_dropped() {
    let mut bytes = event(r#"{"n":1}"#);
    let second = event(r#"{"n":2}"#);
    // Cut inside the second message's prelude, then inside its payload.
    for cut in [5, second.len() - 3] {
        let mut stream = bytes.clone();
        stream.extend_from_slice(&second[..cut]);
        let read = messages(&stream[..]).unwrap();
        assert_eq!(read.len(), 1, "cut at {cut}");
    }
    bytes.clear();
    assert!(messages(&bytes[..]).unwrap().is_empty());
}

#[test]
fn every_header_type_is_read_past_and_strings_are_kept() {
    let bytes = frame(
        &[
            ("t", Value::True),
            ("f", Value::False),
            ("b", Value::Byte(-1)),
            ("s", Value::Short(-2)),
            ("i", Value::Int(-3)),
            ("l", Value::Long(-4)),
            ("bytes", Value::Bytes(b"\x00\x01\x02")),
            ("ts", Value::Timestamp(1_700_000_000_000)),
            ("id", Value::Uuid([7; 16])),
            (":message-type", Value::Str("event")),
            ("empty", Value::Str("")),
        ],
        b"payload",
    );
    let read = messages(&bytes[..]).unwrap();
    assert_eq!(
        read,
        [Message {
            headers: vec![
                (":message-type".into(), "event".into()),
                ("empty".into(), String::new()),
            ],
            payload: b"payload".to_vec(),
        }]
    );
}

#[test]
fn a_header_of_an_unknown_type_fails_the_stream() {
    let mut bytes = frame(&[("x", Value::True)], b"");
    // The type byte follows the one-byte name length and the name.
    bytes[12 + 2] = 10;
    let fixed = reframe(bytes);
    let error = messages(&fixed[..]).unwrap_err();
    assert_eq!(error.code(), ErrorCode::StreamIncomplete);
    assert!(
        error.to_string().contains("headers are malformed"),
        "{error}"
    );
}

#[test]
fn a_header_cut_short_fails_the_stream() {
    for headers in [
        &[("name", Value::Long(1))][..],
        &[("name", Value::Str("value"))][..],
    ] {
        let bytes = frame(headers, b"");
        let headers_length = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
        // Claim one header byte fewer, so the last value runs off the end.
        let mut cut = bytes.clone();
        cut[4..8].copy_from_slice(&(headers_length - 1).to_be_bytes());
        let fixed = reframe(cut);
        let error = messages(&fixed[..]).unwrap_err();
        assert_eq!(error.code(), ErrorCode::StreamIncomplete);
        assert!(
            error.to_string().contains("headers are malformed"),
            "{error}"
        );
    }
}

/// `bytes` with both checksums recomputed, so a test that edits a frame
/// reaches the check past them.
fn reframe(mut bytes: Vec<u8>) -> Vec<u8> {
    let prelude = crc32(&bytes[..8]);
    bytes[8..12].copy_from_slice(&prelude.to_be_bytes());
    let end = bytes.len() - 4;
    let message = crc32(&bytes[..end]);
    bytes[end..].copy_from_slice(&message.to_be_bytes());
    bytes
}

#[test]
fn a_prelude_that_fails_its_checksum_fails_the_stream() {
    let mut bytes = event(r#"{"n":1}"#);
    bytes[9] ^= 0xff;
    let error = messages(&bytes[..]).unwrap_err();
    assert_eq!(error.code(), ErrorCode::StreamIncomplete);
    assert!(
        error.to_string().contains("prelude failed its checksum"),
        "{error}"
    );
}

#[test]
fn a_message_that_fails_its_checksum_fails_the_stream() {
    for at in [14, 40] {
        let mut bytes = event(r#"{"n":1}"#);
        bytes[at] ^= 0x01;
        let error = messages(&bytes[..]).unwrap_err();
        assert_eq!(error.code(), ErrorCode::StreamIncomplete);
        assert!(
            error.to_string().contains("a message failed its checksum"),
            "{error}"
        );
    }
    let mut bytes = event(r#"{"n":1}"#);
    let last = bytes.len() - 1;
    bytes[last] ^= 0x80;
    assert!(messages(&bytes[..]).is_err());
}

#[test]
fn a_length_out_of_range_fails_the_stream() {
    for (total, headers) in [(15_u32, 0_u32), (16 * 1024 * 1024 + 1, 0), (20, 5)] {
        let mut prelude = Vec::new();
        prelude.extend_from_slice(&total.to_be_bytes());
        prelude.extend_from_slice(&headers.to_be_bytes());
        let crc = crc32(&prelude);
        prelude.extend_from_slice(&crc.to_be_bytes());
        let error = messages(&prelude[..]).unwrap_err();
        assert_eq!(
            error.code(),
            ErrorCode::StreamIncomplete,
            "{total}/{headers}"
        );
        assert!(error.to_string().contains("bytes long"), "{error}");
    }
}

#[test]
fn the_largest_message_lengths_are_accepted() {
    // 16 bytes is a message with nothing in it; 16 MiB is AWS's bound, and
    // a headers section may fill everything between prelude and checksum.
    let mut bytes = frame(&[], b"");
    assert_eq!(messages(&bytes[..]).unwrap().len(), 1);
    let big = frame(&[], &vec![b'x'; 16 * 1024 * 1024 - 16]);
    assert_eq!(
        messages(&big[..]).unwrap()[0].payload.len(),
        16 * 1024 * 1024 - 16
    );
    bytes = frame(&[("h", Value::Str("v"))], b"");
    assert_eq!(messages(&bytes[..]).unwrap()[0].header("h"), Some("v"));
}

#[test]
fn headers_over_their_bound_fail_the_stream() {
    // Two string headers named with one byte cost 10 bytes beside their
    // values, so these fill 128 KiB exactly, then one byte more.
    let first = "v".repeat(65_535);
    for (second, accepted) in [(65_527, true), (65_528, false)] {
        let second = "w".repeat(second);
        let bytes = frame(
            &[("a", Value::Str(&first)), ("b", Value::Str(&second))],
            b"",
        );
        let read = messages(&bytes[..]);
        assert_eq!(
            read.is_ok(),
            accepted,
            "{second_len}",
            second_len = second.len()
        );
        if let Err(error) = read {
            assert!(error.to_string().contains("headers are"), "{error}");
        }
    }
}

#[test]
fn on_message_returning_true_stops_the_read() {
    let mut stream = event("1");
    stream.extend(event("2"));
    let mut seen = 0;
    read(&stream[..], |_| {
        seen += 1;
        Ok(true)
    })
    .unwrap();
    assert_eq!(seen, 1);
}

#[test]
fn a_read_error_is_a_dropped_connection() {
    // The stream goes on to serve a whole event after the error, so a read
    // that retried the error rather than failing would read it.
    struct Broken {
        bytes: Vec<u8>,
        failed: bool,
    }
    impl Read for Broken {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.failed {
                self.failed = true;
                return Err(std::io::Error::other("reset"));
            }
            let n = self.bytes.len().min(buf.len());
            buf[..n].copy_from_slice(&self.bytes[..n]);
            self.bytes.drain(..n);
            Ok(n)
        }
    }
    let error = messages(Broken {
        bytes: event("{}"),
        failed: false,
    })
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConnectionFailed);
}

#[test]
fn an_exception_message_is_a_reply_failure_carrying_its_type() {
    let exception = Message {
        headers: vec![
            (":exception-type".into(), "throttlingException".into()),
            (":content-type".into(), "application/json".into()),
            (":message-type".into(), "exception".into()),
        ],
        payload: br#"{"message":"Too many tokens, please wait before trying again."}"#.to_vec(),
    };
    assert_eq!(
        exception.failure(),
        Some(Error::ReplyFailed {
            code: Some("throttlingException".into()),
            message: "Too many tokens, please wait before trying again.".into(),
        })
    );
    let capital = Message {
        payload: br#"{"Message":"Upper."}"#.to_vec(),
        ..exception.clone()
    };
    assert_eq!(
        capital.failure(),
        Some(Error::ReplyFailed {
            code: Some("throttlingException".into()),
            message: "Upper.".into(),
        })
    );
    let bare = Message {
        payload: b"not json".to_vec(),
        ..exception
    };
    assert_eq!(
        bare.failure(),
        Some(Error::ReplyFailed {
            code: Some("throttlingException".into()),
            message: "not json".into(),
        })
    );
}

#[test]
fn an_error_message_is_a_reply_failure_carrying_its_code() {
    let error = Message {
        headers: vec![
            (":message-type".into(), "error".into()),
            (":error-code".into(), "InternalFailure".into()),
            (":error-message".into(), "Something broke.".into()),
        ],
        payload: Vec::new(),
    };
    assert_eq!(
        error.failure(),
        Some(Error::ReplyFailed {
            code: Some("InternalFailure".into()),
            message: "Something broke.".into(),
        })
    );
}

#[test]
fn an_event_is_no_failure() {
    let read = messages(&event("{}")[..]).unwrap();
    assert_eq!(read[0].failure(), None);
    let untyped = Message {
        headers: Vec::new(),
        payload: Vec::new(),
    };
    assert_eq!(untyped.failure(), None);
}

#[test]
fn an_interrupted_read_is_retried() {
    struct Interrupted {
        bytes: Vec<u8>,
        interrupted: bool,
    }
    impl Read for Interrupted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let n = self.bytes.len().min(buf.len());
            buf[..n].copy_from_slice(&self.bytes[..n]);
            self.bytes.drain(..n);
            Ok(n)
        }
    }
    let read = messages(Interrupted {
        bytes: event("{}"),
        interrupted: false,
    })
    .unwrap();
    assert_eq!(read.len(), 1);
}
