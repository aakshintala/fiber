use super::{MatchFile, MATCHES_ENV, LIMIT};
use fakes::TempDir;
use serde_json::{Value, json};

#[test]
fn records_keep_order_context_lossy_text_and_whole_character_spans() {
    assert_eq!(MATCHES_ENV, "FIBER_GREP_MATCHES");
    assert_eq!(LIMIT, 1_048_576);
    let dir = TempDir::new("fiber-grep-records");
    let path = dir.path().join("matches");
    let mut writer = MatchFile::create(&path, LIMIT).unwrap();
    writer.record(b"a.txt", 1, b"needle needle", &[(0,6), (7,13)], false);
    writer.record(b"a.txt", 2, b"context\r", &[], true);
    writer.record(b"a.txt", 3, "café".as_bytes(), &[(0,4)], false);
    writer.record(b"a.txt", 4, "éé".as_bytes(), &[(1,3)], false);
    writer.record(b"a.txt", 5, b"n\xff", &[(0,1)], false);
    writer.record(b"a.txt", 6, b"", &[(0,0)], false);
    writer.finish();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert_eq!(raw.lines().next().unwrap(),
        r#"{"path":"a.txt","line":1,"text":"needle needle","spans":[[0,6],[7,13]],"context":false}"#);
    let lines: Vec<Value> = raw.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(lines, vec![
        json!({"path":"a.txt","line":1,"text":"needle needle","spans":[[0,6],[7,13]],"context":false}),
        json!({"path":"a.txt","line":2,"text":"context\r","spans":[],"context":true}),
        json!({"path":"a.txt","line":3,"text":"café","spans":[[0,5]],"context":false}),
        json!({"path":"a.txt","line":4,"text":"éé","spans":[[0,4]],"context":false}),
        json!({"path":"a.txt","line":5,"text":"n�","spans":[],"context":false}),
        json!({"path":"a.txt","line":6,"text":"","spans":[],"context":false}),
    ]);
}

#[test]
fn record_bound_keeps_whole_records_and_stops_for_good() {
    let dir = TempDir::new("fiber-grep-record-bound");
    let first = dir.path().join("first");
    let mut writer = MatchFile::create(&first, LIMIT).unwrap();
    writer.record(b"a", 1, b"abc", &[(0,1)], false);
    writer.finish();
    let raw = std::fs::read(&first).unwrap();
    let size = raw.len() as u64;
    for limit in [size-1, size, size+1] {
        let path = dir.path().join(format!("{limit}"));
        let mut writer = MatchFile::create(&path, limit).unwrap();
        writer.record(b"a", 1, b"abc", &[(0,1)], false);
        // This record cannot fit. Even the shorter record after it stays out.
        writer.record(b"a", 2, b"abc", &[(0,1)], false);
        writer.record(b"", 0, b"", &[], true);
        writer.finish();
        let actual = std::fs::read(path).unwrap();
        assert_eq!(actual, if limit >= size { raw.clone() } else { Vec::new() });
    }
}

#[test]
fn create_failure_preserves_existing_files_and_write_failure_removes_partial_data() {
    let dir = TempDir::new("fiber-grep-record-failure");
    assert!(MatchFile::create(&dir.path().join("missing/matches"), LIMIT).is_none());
    let path = dir.path().join("matches");
    std::fs::write(&path, "existing").unwrap();
    assert!(MatchFile::create(&path, LIMIT).is_none());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing");
    let other = dir.path().join("partial");
    let mut writer = MatchFile::create(&other, LIMIT).unwrap();
    writer.file = Some(std::fs::File::open(&other).unwrap());
    writer.record(b"a", 1, b"x", &[(0,1)], false);
    writer.finish();
    assert!(!other.exists());
}
