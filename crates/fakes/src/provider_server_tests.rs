use std::io::{Cursor, Read};

use super::*;

fn decode(bytes: &[u8]) -> (Result<(), Malformed>, Vec<u8>, String) {
    let mut reader = Cursor::new(bytes.to_vec());
    let mut body = Vec::new();
    let result = read_chunked(&mut reader, &mut body);
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();
    (result, body, rest)
}

#[test]
fn a_chunked_body_is_decoded_and_its_trailers_consumed() {
    let (result, body, rest) =
        decode(b"3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\nx-trailer: t\r\n\r\nNEXT");

    assert_eq!(result, Ok(()));
    assert_eq!(body, b"abcde");
    assert_eq!(rest, "NEXT");
}

#[test]
fn malformed_or_truncated_chunked_framing_is_rejected_naming_why() {
    let cases: [(&[u8], Malformed); 7] = [
        (
            b"3\r\nabcX\r\n0\r\n\r\n",
            "a chunk's data is not followed by CRLF",
        ),
        (
            b"zz\r\nabc\r\n0\r\n\r\n",
            "a chunk size is not a hex number",
        ),
        (
            b"3\nabc\r\n0\r\n\r\n",
            "a chunked framing line does not end in CRLF",
        ),
        (b"5\r\nab", TRUNCATED),
        (b"3\r\nabc\r\n", TRUNCATED),
        (b"3\r\nabc\r\n0\r\n", TRUNCATED),
        (b"3\r\nabc\r\n0\r\n\r", TRUNCATED),
    ];
    for (bytes, why) in cases {
        let (result, _, _) = decode(bytes);
        assert_eq!(result, Err(why), "{}", String::from_utf8_lossy(bytes));
    }
}

#[test]
fn a_rejected_body_keeps_what_decoded_before_the_fault() {
    let (_, body, _) = decode(b"3\r\nabc\r\n4\r\nde");

    assert_eq!(body, b"abcde");
}
