use super::{MatchDir, read, remove};
use crate::search::matches::LIMIT;
use fakes::TempDir;
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn reader_groups_only_consecutive_paths_and_drops_context_marker() {
    let dir = TempDir::new("fiber-shell-matches-read");
    let path = dir.path().join("matches");
    fs::write(&path, concat!(
        "{\"path\":\"a\",\"line\":1,\"text\":\"café\",\"spans\":[[0,5]],\"context\":false}\n",
        "{\"path\":\"a\",\"line\":2,\"text\":\"context\",\"spans\":[],\"context\":true}\n",
        "{\"path\":\"b\",\"line\":3,\"text\":\"x\",\"spans\":[[0,1]],\"context\":false}\n",
        "{\"path\":\"a\",\"line\":4,\"text\":\"x\",\"spans\":[[0,0],[1,1]],\"context\":false}\n",
    )).unwrap();
    assert_eq!(read(&path, LIMIT), Some(json!([
        {"path":"a","lines":[{"line":1,"text":"café","spans":[[0,5]]}, {"line":2,"text":"context","spans":[]}]},
        {"path":"b","lines":[{"line":3,"text":"x","spans":[[0,1]]}]},
        {"path":"a","lines":[{"line":4,"text":"x","spans":[[0,0],[1,1]]}]},
    ])));
    fs::write(&path, "").unwrap();
    assert_eq!(read(&path, LIMIT), Some(json!([])));
}

#[test]
fn malformed_missing_or_unreadable_side_files_add_nothing() {
    let dir = TempDir::new("fiber-shell-matches-bad");
    let path = dir.path().join("matches");
    assert_eq!(read(&path, LIMIT), None);
    let valid = json!({"path":"a", "line":1, "text":"café", "spans":[[0,5]], "context":false});
    let mut cases = Vec::new();
    let mut extra = valid.clone(); extra["unknown"] = json!(1); cases.push(extra.to_string()+"\n");
    for key in ["path", "line", "text", "spans", "context"] {
        let mut missing = valid.clone(); missing.as_object_mut().unwrap().remove(key);
        cases.push(missing.to_string()+"\n");
    }
    for spans in [json!([[0,6]]), json!([[3,2]]), json!([[0,4]]), json!([[4,5]]), json!([[0,1,2]])] {
        let mut bad = valid.clone(); bad["spans"] = spans; cases.push(bad.to_string()+"\n");
    }
    cases.push(valid.to_string());
    cases.push(valid.to_string()+"\n{\n");
    for bad in cases {
        fs::write(&path, bad).unwrap();
        assert_eq!(read(&path, LIMIT), None);
    }
    fs::write(&path, b"\xff\n").unwrap();
    assert_eq!(read(&path, LIMIT), None);
    fs::set_permissions(&path, fs::Permissions::from_mode(0)).unwrap();
    assert_eq!(read(&path, LIMIT), None);
}

#[test]
fn reader_bound_accepts_exactly_the_limit_but_not_one_byte_more() {
    let dir = TempDir::new("fiber-shell-matches-bound");
    let path = dir.path().join("matches");
    let mut record = json!({"path":"a", "line":1, "text":"", "spans":[], "context":false});
    let overhead = record.to_string().len() + 1;
    for len in [LIMIT-1, LIMIT, LIMIT+1] {
        record["text"] = json!("x".repeat(usize::try_from(len).unwrap()-overhead));
        fs::write(&path, record.to_string()+"\n").unwrap();
        let parsed = read(&path, LIMIT);
        assert_eq!(parsed.is_some(), len <= LIMIT);
        if let Some(groups) = parsed {
            assert_eq!(groups[0]["lines"][0]["text"].as_str().unwrap().len(), usize::try_from(len).unwrap()-overhead);
        }
    }
}

#[test]
fn private_directory_drop_removes_both_names_and_blocks_late_creates() {
    let directory = MatchDir::create().unwrap();
    let file = directory.file();
    let path = file.parent().unwrap().to_path_buf();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o700);
    assert!(path.file_name().unwrap().to_str().unwrap().starts_with("fiber-grep-"));
    fs::write(&file, "metadata").unwrap();
    drop(directory);
    assert!(!path.exists());
    assert!(!path.with_extension("gone").exists());
    assert_eq!(fs::OpenOptions::new().create_new(true).write(true).open(file).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    let dir = TempDir::new("fiber-shell-matches-remove");
    let path = dir.path().join("old");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("file"), "data").unwrap();
    fs::create_dir(path.with_extension("gone")).unwrap();
    fs::write(path.with_extension("gone").join("unrelated"), "preserve").unwrap();
    remove(&path);
    assert!(!path.exists());
    assert_eq!(fs::read_to_string(path.with_extension("gone").join("unrelated")).unwrap(), "preserve");
    remove(&path);
}

#[test]
fn carried_environment_excludes_startup_functions_and_inherited_match_paths() {
    use std::ffi::OsStr;
    for (key, expected) in [("FIBER_GREP_MATCHES",false), ("BASH_ENV",false),
        ("BASH_FUNC_grep%%",false), ("PATH",true), ("FIBER_GREP_MATCHES_OTHER",true)] {
        assert_eq!(super::super::spawn::carried(OsStr::new(key)), expected, "{key}");
    }
}
