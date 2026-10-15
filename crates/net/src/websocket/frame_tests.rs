use super::*;
use std::io::Cursor;

fn server(first: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![first];
    match payload.len() {
        0..=125 => out.push(u8::try_from(payload.len()).unwrap()),
        126..=65535 => {
            out.push(126);
            out.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
        }
        _ => {
            out.push(127);
            out.extend_from_slice(&u64::try_from(payload.len()).unwrap().to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

#[test]
fn masked_encoder_uses_minimal_lengths_and_a_fresh_draw() {
    let mut draws = 0u8;
    for len in [0, 124, 125, 126, 127, 65534, 65535, 65536, 65537] {
        let payload = vec![b'x'; len];
        let frame = encode(1, &payload, &mut |mask| {
            draws += 1;
            *mask = [draws; 4];
            Ok(())
        })
        .unwrap();
        assert_eq!(frame[0], 0x81);
        let (tag, offset) = match len {
            0..=125 => (u8::try_from(len).unwrap(), 2),
            126..=65535 => {
                assert_eq!(
                    u16::from_be_bytes(frame[2..4].try_into().unwrap()),
                    u16::try_from(len).unwrap()
                );
                (126, 4)
            }
            _ => {
                assert_eq!(
                    u64::from_be_bytes(frame[2..10].try_into().unwrap()),
                    u64::try_from(len).unwrap()
                );
                (127, 10)
            }
        };
        assert_eq!(frame[1], 0x80 | tag);
        assert_eq!(&frame[offset..offset + 4], &[draws; 4]);
        let decoded: Vec<u8> = frame[offset + 4..].iter().map(|b| b ^ draws).collect();
        assert_eq!(decoded.len(), len);
        assert!(decoded == payload, "payload differs at length {len}");
    }
    assert_eq!(draws, 9);
    for (op, bytes) in [
        (9, b"ping".as_slice()),
        (10, b"pong".as_slice()),
        (8, &[3, 232]),
    ] {
        let encoded = encode(op, bytes, &mut |mask| {
            *mask = [0; 4];
            Ok(())
        })
        .unwrap();
        assert_eq!(encoded[0], 0x80 | op);
        assert_eq!(&encoded[6..], bytes);
    }
    assert!(encode(1, b"", &mut |_| Err(std::io::Error::other("rng"))).is_err());
}

#[test]
fn server_text_lengths_round_trip() {
    for len in [0, 125, 126, 65535, 65536] {
        let payload = vec![b'x'; len];
        let Incoming::Text(text) = Reader::default().next(&mut Cursor::new(server(0x81, &payload)))
        else {
            panic!("text at {len}");
        };
        assert_eq!(text.len(), len);
        assert!(text.bytes().all(|b| b == b'x'));
    }
}

#[test]
fn fragmented_utf8_allows_controls_before_completion() {
    let mut wire = server(0x01, &[0xe2]);
    wire.extend(server(0x89, b"ping"));
    wire.extend(server(0x8a, b"ignored"));
    wire.extend(server(0x80, &[0x82, 0xac]));
    let mut read = Cursor::new(wire);
    let mut reader = Reader::default();
    assert_eq!(reader.next(&mut read), Incoming::Ping(b"ping".to_vec()));
    assert_eq!(reader.next(&mut read), Incoming::Pong);
    assert_eq!(reader.next(&mut read), Incoming::Text("€".to_owned()));
    assert_eq!(reader.next(&mut read), Incoming::Gone);
}

#[test]
fn protocol_refusals_have_the_exact_close_code() {
    let mut nested = server(0x01, b"a");
    nested.extend(server(0x81, b"b"));
    for (label, wire, code) in [
        ("masked", vec![0x81, 0x80], 1002),
        ("rsv", vec![0xc1, 0], 1002),
        ("reserved", vec![0x83, 0], 1002),
        ("control long", server(0x89, &[0; 126]), 1002),
        ("control fragment", server(0x09, b""), 1002),
        ("continuation", server(0x80, b""), 1002),
        ("nested", nested, 1002),
        ("binary", server(0x82, b""), 1003),
        ("utf8", server(0x81, &[0xff]), 1007),
        ("lf", server(0x81, b"a\nb"), 1007),
        ("short nonminimal", vec![0x81, 126, 0, 125], 1002),
        (
            "wide nonminimal",
            vec![0x81, 127, 0, 0, 0, 0, 0, 0, 255, 255],
            1002,
        ),
        ("wide sign", vec![0x81, 127, 128, 0, 0, 0, 0, 0, 0, 0], 1002),
        ("one-byte close", server(0x88, &[3]), 1002),
        ("bad close reason", server(0x88, &[3, 232, 0xff]), 1007),
    ] {
        assert_eq!(
            Reader::default().next(&mut Cursor::new(wire)),
            Incoming::Fail(code),
            "{label}"
        );
    }
}

#[test]
fn controls_at_the_payload_cap_are_valid() {
    for len in [124, 125] {
        let payload = vec![b'x'; len];
        assert_eq!(
            Reader::default().next(&mut Cursor::new(server(0x89, &payload))),
            Incoming::Ping(payload)
        );
    }
}

#[test]
fn close_payloads_are_checked_and_echoable() {
    assert_eq!(
        Reader::default().next(&mut Cursor::new(server(0x88, b""))),
        Incoming::Close(Vec::new())
    );
    for code in [
        999u16, 1000, 1003, 1004, 1006, 1007, 1014, 1015, 2999, 3000, 4999, 5000,
    ] {
        let wire = server(0x88, &code.to_be_bytes());
        let expected = if matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999) {
            Incoming::Close(code.to_be_bytes().to_vec())
        } else {
            Incoming::Fail(1002)
        };
        assert_eq!(
            Reader::default().next(&mut Cursor::new(wire)),
            expected,
            "{code}"
        );
    }
    assert_eq!(
        Reader::default().next(&mut Cursor::new(vec![0x81])),
        Incoming::Gone
    );
    assert_eq!(
        Reader::default().next(&mut Cursor::new(vec![0x81, 1])),
        Incoming::Gone
    );
    assert_eq!(
        Reader::default().next(&mut Cursor::new(vec![0x81, 126, 0])),
        Incoming::Gone
    );
    assert_eq!(
        Reader::default().next(&mut Cursor::new(vec![0x81, 127, 0])),
        Incoming::Gone
    );
    assert_eq!(
        Reader::default().next(&mut Cursor::new(vec![0x89, 1])),
        Incoming::Gone
    );
}
