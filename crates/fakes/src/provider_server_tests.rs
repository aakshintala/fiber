use std::io::{Cursor, Read};

use super::*;

#[test]
fn a_chunked_body_is_decoded_and_its_trailers_consumed() {
    let mut reader =
        Cursor::new(b"3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\nx-trailer: t\r\n\r\nNEXT".to_vec());

    let body = read_chunked(&mut reader).unwrap();

    assert_eq!(body, b"abcde");
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "NEXT");
}
