//! Tests for the fake session's `hold`: a torn write the fake already read
//! is recorded the way the live path records it, so a cut command's bytes
//! count however the session ended.

use super::*;

/// `hold` keeps the EOF fragment: a torn write the fake already read
/// is recorded the way the live path records it, not discarded, so a
/// cut command's bytes count however the session ended.
#[test]
fn hold_records_the_fragment_it_reads_before_eof() {
    let received = Received::default();
    let (mut writer, reader) = UnixStream::pair().expect("a fake connection pair");
    let mut read = BufReader::new(reader);
    writer
        .write_all(b"{\"id\":\"c_2\",\"command\":\"prompt\"}")
        .expect("the torn bytes arrive");
    writer.shutdown(Shutdown::Write).expect("the session ends");
    hold(Some(&received), 0, &mut read);
    assert_eq!(
        lock(&received.lines).clone(),
        vec!["{\"id\":\"c_2\",\"command\":\"prompt\"}\n".to_owned()],
        "the torn fragment is recorded with its line feed, as live reads are"
    );
}
