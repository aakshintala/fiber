//! Tests for MCP stdio line limits and child-pipe reads.

#[test]
fn max_line_is_4_mib() {
    assert_eq!(super::MAX_LINE, 4 * 1024 * 1024);
}

#[test]
fn a_line_ending_in_a_newline_strips_it() {
    // Without the `ends_with` guard the trailing newline would stay in
    // the line: the `false` mutant returns `"hi\n"` here.
    use std::io::Cursor;
    let mut reader = Cursor::new(b"hi\n".to_vec());
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line, "hi"),
        super::ReadLine::Eof => panic!("a newline-terminated line is a line, got the end"),
        super::ReadLine::TooLong => {
            panic!("a newline-terminated line is a line, got too long")
        }
    }
}

#[test]
fn a_final_line_without_a_newline_is_a_line() {
    // Without the `ends_with` guard the last byte would be popped as if
    // it were a newline: the `true` mutant returns `"h"` here.
    use std::io::Cursor;
    let mut reader = Cursor::new(b"hi".to_vec());
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line, "hi"),
        super::ReadLine::Eof => panic!("a final line without a newline is a line, got the end"),
        super::ReadLine::TooLong => {
            panic!("a final line without a newline is a line, got too long")
        }
    }
}

#[test]
fn an_empty_read_is_the_end() {
    use std::io::Cursor;
    let mut reader = Cursor::new(Vec::new());
    match super::read_line(&mut reader) {
        super::ReadLine::Eof => {}
        super::ReadLine::Line(_) => panic!("an empty read is the end, got a line"),
        super::ReadLine::TooLong => panic!("an empty read is the end, got too long"),
    }
}

#[test]
fn exactly_max_line_bytes_is_a_line() {
    // `>=` would end the reader here; only `>` lets exactly `MAX_LINE`
    // bytes through as a line.
    use std::io::Cursor;
    let content = vec![b'x'; super::MAX_LINE];
    let mut reader = Cursor::new(content);
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line.len(), super::MAX_LINE),
        super::ReadLine::Eof => panic!("exactly MAX_LINE bytes is a line, got the end"),
        super::ReadLine::TooLong => panic!("exactly MAX_LINE bytes is a line, got too long"),
    }
}

#[test]
fn max_line_content_plus_a_newline_is_a_line() {
    // The `take(MAX_LINE + 1)` lets a full line plus its newline through:
    // `-` or `*` in place of `+` truncates it and the assertion on the
    // exact length fails.
    use std::io::Cursor;
    let mut content = vec![b'x'; super::MAX_LINE];
    content.push(b'\n');
    let mut reader = Cursor::new(content);
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line.len(), super::MAX_LINE),
        super::ReadLine::Eof => panic!("MAX_LINE bytes plus a newline is a line, got the end"),
        super::ReadLine::TooLong => {
            panic!("MAX_LINE bytes plus a newline is a line, got too long")
        }
    }
}

#[test]
fn max_line_plus_one_bytes_is_too_long() {
    // `==` misses this length and `<` ends short lines instead: only `>`
    // ends exactly the lines past the cap.
    use std::io::Cursor;
    let content = vec![b'x'; super::MAX_LINE + 1];
    let mut reader = Cursor::new(content);
    match super::read_line(&mut reader) {
        super::ReadLine::TooLong => {}
        super::ReadLine::Line(_) => panic!("MAX_LINE + 1 bytes is too long, got a line"),
        super::ReadLine::Eof => panic!("MAX_LINE + 1 bytes is too long, got the end"),
    }
}

#[test]
fn a_read_error_without_bytes_is_the_end() {
    // The `true` mutant would also end a read that did carry bytes, and
    // the `false` mutant would return an empty line here.
    let mut reader = AlwaysErr;
    match super::read_line(&mut reader) {
        super::ReadLine::Eof => {}
        super::ReadLine::Line(_) => panic!("a failed read without bytes is the end, got a line"),
        super::ReadLine::TooLong => {
            panic!("a failed read without bytes is the end, got too long")
        }
    }
}

#[test]
fn a_read_error_after_bytes_keeps_the_line() {
    // The `true` mutant would discard these bytes and report the end.
    let mut reader = DataThenErr {
        data: b"hi".to_vec(),
        done: false,
    };
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line, "hi"),
        super::ReadLine::Eof => panic!("a failed read after bytes keeps them, got the end"),
        super::ReadLine::TooLong => {
            panic!("a failed read after bytes keeps them, got too long")
        }
    }
}

/// A reader whose every read fails, carrying no bytes.
struct AlwaysErr;

impl std::io::Read for AlwaysErr {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("boom"))
    }
}

impl std::io::BufRead for AlwaysErr {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        Err(std::io::Error::other("boom"))
    }

    fn consume(&mut self, _amount: usize) {}
}

/// A reader that hands over `data` once, then fails: the line reader sees
/// a partial line followed by an error.
struct DataThenErr {
    data: Vec<u8>,
    done: bool,
}

impl std::io::Read for DataThenErr {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::BufRead;
        let chunk = self.fill_buf()?;
        let len = chunk.len().min(buf.len());
        buf[..len].copy_from_slice(&chunk[..len]);
        self.consume(len);
        Ok(len)
    }
}

impl std::io::BufRead for DataThenErr {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.done {
            Err(std::io::Error::other("boom"))
        } else {
            Ok(&self.data)
        }
    }

    fn consume(&mut self, _amount: usize) {
        self.data.clear();
        self.done = true;
    }
}

#[test]
fn a_queued_line_leaves_the_server_live() {
    let (writer, incoming) = std::sync::mpsc::sync_channel(1);
    let shared = super::super::wait::Shared::default();
    super::queue(&writer, b"hi".to_vec(), &shared);
    assert_eq!(incoming.try_recv().expect("the line arrives"), b"hi");
    assert!(!crate::registry::lock(&shared.inner).gone, "gone stays false");
}

#[test]
fn a_full_queue_marks_the_server_gone() {
    let (writer, _incoming) = std::sync::mpsc::sync_channel(1);
    let shared = super::super::wait::Shared::default();
    writer.try_send(b"waiting".to_vec()).expect("one line waits");
    super::queue(&writer, b"next".to_vec(), &shared);
    assert!(crate::registry::lock(&shared.inner).gone, "gone turns true");
}

#[test]
fn a_queue_with_no_writer_marks_the_server_gone() {
    let (writer, incoming) = std::sync::mpsc::sync_channel(1);
    drop(incoming);
    let shared = super::super::wait::Shared::default();
    super::queue(&writer, b"hi".to_vec(), &shared);
    assert!(crate::registry::lock(&shared.inner).gone, "gone turns true");
}
