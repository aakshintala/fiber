use super::read;

fn events(stream: &str) -> Vec<String> {
    let mut out = Vec::new();
    read(stream.as_bytes(), |data| {
        out.push(data.to_owned());
        Ok(false)
    })
    .unwrap();
    out
}

#[test]
fn each_blank_line_dispatches_the_data_before_it() {
    let stream = "event: a\ndata: {\"n\":1}\n\nevent: b\r\ndata:{\"n\":2}\r\n\r\n";
    assert_eq!(events(stream), ["{\"n\":1}", "{\"n\":2}"]);
}

#[test]
fn data_lines_join_with_newlines_and_comments_are_skipped() {
    let stream = ": keep-alive\ndata: one\ndata: two\nid: 7\n\n";
    assert_eq!(events(stream), ["one\ntwo"]);
}

#[test]
fn an_event_the_stream_ends_inside_is_dropped() {
    assert_eq!(events("data: done\n\ndata: half"), ["done"]);
}

#[test]
fn a_field_that_only_starts_with_data_is_not_data() {
    assert_eq!(events("database: x\ndata: y\n\n"), ["y"]);
}

#[test]
fn on_data_returning_true_stops_the_read() {
    let mut seen = 0;
    read("data: 1\n\ndata: 2\n\n".as_bytes(), |_| {
        seen += 1;
        Ok(true)
    })
    .unwrap();
    assert_eq!(seen, 1);
}
