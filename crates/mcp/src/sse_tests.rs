//! Tests for the server-sent event reader.

use std::io::Cursor;

use super::{Event, Next};

fn read(body: &[u8]) -> Next {
    super::next(&mut Cursor::new(body.to_vec()))
}

#[test]
fn one_event_reads_its_name_and_data() {
    assert_eq!(
        read(b"event: message\ndata: {\"a\":1}\n\n"),
        Next::Event(Event {
            name: "message".to_owned(),
            data: "{\"a\":1}".to_owned(),
        })
    );
}

#[test]
fn multi_line_data_joins_with_newlines() {
    // Without the join a multi-line payload would arrive as its last line:
    // the `push('\\n')` mutant returns only it here.
    assert_eq!(
        read(b"data: one\ndata: two\ndata: three\n\n"),
        Next::Event(Event {
            name: String::new(),
            data: "one\ntwo\nthree".to_owned(),
        })
    );
}

#[test]
fn data_without_a_space_reads_the_same() {
    assert_eq!(
        read(b"data:{\"a\":1}\n\n"),
        Next::Event(Event {
            name: String::new(),
            data: "{\"a\":1}".to_owned(),
        })
    );
}

#[test]
fn a_field_named_database_is_not_data() {
    // A prefix match would read the database line as data here.
    assert_eq!(
        read(b"database: here\ndata: there\n\n"),
        Next::Event(Event {
            name: String::new(),
            data: "there".to_owned(),
        })
    );
}

#[test]
fn crlf_endings_strip_the_carriage_return() {
    assert_eq!(
        read(b"event: message\r\ndata: hi\r\n\r\n"),
        Next::Event(Event {
            name: "message".to_owned(),
            data: "hi".to_owned(),
        })
    );
}

#[test]
fn comments_and_id_and_retry_are_skipped() {
    assert_eq!(
        read(b": a comment\nid: 7\nretry: 100\ndata: hi\n\n"),
        Next::Event(Event {
            name: String::new(),
            data: "hi".to_owned(),
        })
    );
}

#[test]
fn a_comment_only_stream_ends() {
    assert_eq!(read(b": keep alive\n: again\n"), Next::End);
}

#[test]
fn an_event_the_stream_ends_inside_is_not_returned() {
    // Without the blank-line dispatch an EOF would return the buffered
    // event: this row pins the unreturned half-event.
    assert_eq!(read(b"event: message\ndata: half"), Next::End);
}

#[test]
fn an_empty_stream_ends() {
    assert_eq!(read(b""), Next::End);
}

#[test]
fn a_line_past_max_line_is_too_long() {
    let mut body = vec![b'x'; super::super::pipes::MAX_LINE + 1];
    body.extend_from_slice(b"\n\n");
    assert_eq!(read(&body), Next::TooLong);
}

#[test]
fn a_line_at_exactly_max_line_is_accepted() {
    // The no-newline path checks the cap directly; `>=` rejects this exact
    // length even though only a longer line is too long.
    let mut reader = Cursor::new(vec![b'x'; super::super::pipes::MAX_LINE]);
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line.len(), super::super::pipes::MAX_LINE),
        super::ReadLine::Eof => panic!("the exact-cap line is present"),
        super::ReadLine::TooLong => panic!("the exact-cap line is accepted"),
    }
}

#[test]
fn a_line_at_exactly_max_line_is_an_event() {
    let mut body = b"data: ".to_vec();
    body.extend(std::iter::repeat_n(
        b'x',
        super::super::pipes::MAX_LINE - "data: ".len(),
    ));
    body.extend_from_slice(b"\n\n");
    match read(&body) {
        Next::Event(event) => assert_eq!(
            event.data.len(),
            super::super::pipes::MAX_LINE - "data: ".len()
        ),
        Next::End => panic!("exactly MAX_LINE bytes is an event, got the end"),
        Next::TooLong => panic!("exactly MAX_LINE bytes is an event, got too long"),
    }
}

#[test]
fn a_line_one_past_max_line_is_too_long_without_a_newline() {
    let mut reader = Cursor::new(vec![b'x'; super::super::pipes::MAX_LINE + 1]);
    assert!(matches!(
        super::read_line(&mut reader),
        super::ReadLine::TooLong
    ));
}

#[test]
fn an_events_data_past_max_line_is_too_long() {
    // Two lines, each short, whose joined data runs past the cap.
    let half = super::super::pipes::MAX_LINE / 2 + 1;
    let mut body = Vec::new();
    body.extend_from_slice(b"data: ");
    body.extend(std::iter::repeat_n(b'x', half));
    body.extend_from_slice(b"\ndata: ");
    body.extend(std::iter::repeat_n(b'y', half));
    body.extend_from_slice(b"\n\n");
    assert_eq!(read(&body), Next::TooLong);
}

#[test]
fn an_events_data_at_exactly_max_line_is_an_event() {
    let half = super::super::pipes::MAX_LINE / 2;
    let mut body = Vec::new();
    body.extend_from_slice(b"data: ");
    body.extend(std::iter::repeat_n(b'x', half));
    body.extend_from_slice(b"\ndata: ");
    body.extend(std::iter::repeat_n(b'y', half - 1));
    body.extend_from_slice(b"\n\n");
    match read(&body) {
        Next::Event(event) => assert_eq!(event.data.len(), super::super::pipes::MAX_LINE),
        Next::End => panic!("joined data at MAX_LINE is an event, got the end"),
        Next::TooLong => panic!("joined data at MAX_LINE is an event, got too long"),
    }
}
