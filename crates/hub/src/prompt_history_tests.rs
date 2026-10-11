//! `prompt_history`: its arguments, the project key, and paging a fixture
//! `history.jsonl` back from its tail across read chunks.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use contract::ErrorCode;
use serde_json::{Map, Value, json};

use super::{CHUNK, PAGE, answer, read_back};

const PROJECT: &str = "-Users-alice-work-fiber-.git";

struct Home {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Home {
    fn new() -> Self {
        let (held, dir) = crate::testkit::home("hp");
        Self { dir, held }
    }

    /// Writes `bytes` as `PROJECT`'s history file.
    fn history(&self, bytes: &[u8]) -> PathBuf {
        let project = self.dir.join("projects").join(PROJECT);
        fs::create_dir_all(&project).unwrap();
        let file = project.join("history.jsonl");
        fs::write(&file, bytes).unwrap();
        file
    }

    fn ask(&self, args: Value) -> Result<Value, (ErrorCode, &'static str)> {
        let Value::Object(args) = args else {
            panic!("args are an object");
        };
        answer(&self.dir, &args)
    }
}

/// A history line whose `content` text is `text`.
fn line(text: &str) -> String {
    format!(
        "{}\n",
        json!({"ts": 1, "session_id": "s_0123456789abcdef", "content": [{"type": "text", "text": text}]})
    )
}

fn lines(texts: &[&str]) -> String {
    texts.iter().map(|text| line(text)).collect()
}

/// The `content` texts of a page's prompts, in the page's order.
fn texts(prompts: &[Value]) -> Vec<String> {
    prompts
        .iter()
        .map(|prompt| prompt["content"][0]["text"].as_str().unwrap().to_owned())
        .collect()
}

fn page_of(result: &Value) -> (Vec<String>, Option<u64>) {
    let object = result.as_object().unwrap();
    assert!(
        object.keys().all(|key| key == "prompts" || key == "before"),
        "a page holds only `prompts` and `before`: {result}"
    );
    (
        texts(result["prompts"].as_array().unwrap()),
        object.get("before").map(|before| before.as_u64().unwrap()),
    )
}

fn ok_page(home: &Home, args: Value) -> (Vec<String>, Option<u64>) {
    page_of(&home.ask(args).unwrap())
}

fn read_with(bytes: &[u8], end: u64, chunk: usize) -> (Vec<String>, Option<u64>) {
    let (prompts, before) = read_back(&mut Cursor::new(bytes), end, chunk).unwrap();
    (texts(&prompts), before)
}

fn rev(texts: &[&str]) -> Vec<String> {
    texts.iter().rev().map(|text| (*text).to_owned()).collect()
}

#[test]
fn a_page_is_newest_first() {
    let home = Home::new();
    home.history(lines(&["a", "b", "c"]).as_bytes());
    assert_eq!(
        ok_page(&home, json!({"project": PROJECT})),
        (rev(&["a", "b", "c"]), None)
    );
}

#[test]
fn a_missing_project_or_file_is_an_empty_page() {
    let home = Home::new();
    let empty = json!({"prompts": []});
    assert_eq!(home.ask(json!({"project": PROJECT})).unwrap(), empty);
    fs::create_dir_all(home.dir.join("projects").join(PROJECT)).unwrap();
    assert_eq!(home.ask(json!({"project": PROJECT})).unwrap(), empty);
    home.history(b"");
    assert_eq!(home.ask(json!({"project": PROJECT})).unwrap(), empty);
}

#[test]
fn a_history_that_cannot_be_read_is_io_failed() {
    let home = Home::new();
    let file = home
        .dir
        .join("projects")
        .join(PROJECT)
        .join("history.jsonl");
    fs::create_dir_all(&file).unwrap();
    let (code, _) = home.ask(json!({"project": PROJECT})).unwrap_err();
    assert_eq!(code, ErrorCode::IoFailed);
}

#[test]
fn a_history_that_cannot_be_opened_is_io_failed() {
    // The project path is a file, so opening the history under it fails
    // with something other than `NotFound`.
    let home = Home::new();
    let projects = home.dir.join("projects");
    fs::create_dir_all(&projects).unwrap();
    fs::write(projects.join(PROJECT), b"").unwrap();
    let (code, _) = home.ask(json!({"project": PROJECT})).unwrap_err();
    assert_eq!(code, ErrorCode::IoFailed);
}

#[test]
fn a_full_page_returns_before_and_paging_reaches_the_oldest_line() {
    let home = Home::new();
    let names: Vec<String> = (0..PAGE * 2 + 3).map(|n| format!("p{n}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let text = lines(&refs);
    home.history(text.as_bytes());
    let mut seen = Vec::new();
    let mut before = None;
    let mut pages = 0;
    loop {
        let mut args = Map::new();
        args.insert("project".into(), json!(PROJECT));
        if let Some(before) = before {
            args.insert("before".into(), json!(before));
        }
        let (page, next) = ok_page(&home, Value::Object(args));
        pages += 1;
        assert!(page.len() <= PAGE);
        if let Some(next) = next {
            assert_eq!(page.len(), PAGE, "a page with older lines left is full");
            let offset = usize::try_from(next).unwrap();
            assert_eq!(
                &text[offset..],
                lines(&refs[refs.len() - seen.len() - PAGE..])
            );
        }
        seen.extend(page);
        before = next;
        if before.is_none() {
            break;
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, rev(&refs));
}

#[test]
fn exactly_one_page_of_lines_returns_no_before() {
    let names: Vec<String> = (0..PAGE).map(|n| format!("p{n}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let text = lines(&refs);
    let end = text.len() as u64;
    assert_eq!(read_with(text.as_bytes(), end, CHUNK), (rev(&refs), None));
    let more = format!("{}{text}", line("older"));
    let (page, before) = read_with(more.as_bytes(), more.len() as u64, CHUNK);
    assert_eq!(page, rev(&refs));
    assert_eq!(before, Some(line("older").len() as u64));
}

#[test]
fn a_malformed_line_and_a_trailing_fragment_are_skipped() {
    let home = Home::new();
    let text = format!(
        "{}not json\n[1,2]\n\n{}{{\"ts\":1,\"session",
        line("a"),
        line("b")
    );
    home.history(text.as_bytes());
    assert_eq!(
        ok_page(&home, json!({"project": PROJECT})),
        (rev(&["a", "b"]), None)
    );
}

#[test]
fn a_file_with_no_newline_at_all_is_an_empty_page() {
    let text = line("a");
    let fragment = text.trim_end();
    assert_eq!(
        read_with(fragment.as_bytes(), fragment.len() as u64, CHUNK),
        (Vec::new(), None)
    );
}

#[test]
fn before_at_a_line_start_inside_a_line_past_the_end_and_at_zero() {
    let home = Home::new();
    let text = lines(&["a", "b", "c"]);
    home.history(text.as_bytes());
    let second = line("a").len() as u64;
    let third = second * 2;
    assert_eq!(
        ok_page(&home, json!({"project": PROJECT, "before": third})),
        (rev(&["a", "b"]), None)
    );
    assert_eq!(
        ok_page(&home, json!({"project": PROJECT, "before": third - 1})),
        (rev(&["a"]), None),
        "a `before` inside a line drops that line"
    );
    assert_eq!(
        ok_page(&home, json!({"project": PROJECT, "before": third + 1})),
        (rev(&["a", "b"]), None)
    );
    assert_eq!(
        ok_page(
            &home,
            json!({"project": PROJECT, "before": text.len() + 100})
        ),
        (rev(&["a", "b", "c"]), None)
    );
    assert_eq!(
        ok_page(&home, json!({"project": PROJECT, "before": text.len()})),
        (rev(&["a", "b", "c"]), None)
    );
    assert_eq!(
        ok_page(&home, json!({"project": PROJECT, "before": 0})),
        (Vec::new(), None)
    );
}

#[test]
fn every_chunk_size_reads_the_same_page() {
    let text = format!(
        "{}{}garbage\n{}{}",
        line("first"),
        line(&"x".repeat(40)),
        line("y"),
        line("last")
    );
    let bytes = text.as_bytes();
    let expected = rev(&["first", &"x".repeat(40), "y", "last"]);
    for chunk in 1..=bytes.len() + 1 {
        for end in [bytes.len() as u64, bytes.len() as u64 - 1] {
            let want = if end == bytes.len() as u64 {
                expected.clone()
            } else {
                expected[1..].to_vec()
            };
            assert_eq!(read_with(bytes, end, chunk), (want, None), "chunk {chunk}");
        }
    }
}

#[test]
fn a_full_page_across_chunks_gives_the_same_before_for_every_chunk_size() {
    let names: Vec<String> = (0..PAGE + 2).map(|n| format!("p{n}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let text = lines(&refs);
    let before = (line("p0").len() + line("p1").len()) as u64;
    for chunk in [1, 7, 64, 100, 4096, CHUNK] {
        assert_eq!(
            read_with(text.as_bytes(), text.len() as u64, chunk),
            (rev(&refs[2..]), Some(before)),
            "chunk {chunk}"
        );
    }
}

#[test]
fn a_file_larger_than_one_chunk_pages_through_the_command() {
    let home = Home::new();
    let long = "z".repeat(400);
    let names: Vec<String> = (0..PAGE + 10).map(|n| format!("{long}{n}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let text = lines(&refs);
    assert!(text.len() > CHUNK);
    home.history(text.as_bytes());
    let (newest, before) = ok_page(&home, json!({"project": PROJECT}));
    assert_eq!(newest, rev(&refs[10..]));
    let before = before.expect("older lines remain");
    assert_eq!(usize::try_from(before).unwrap(), lines(&refs[..10]).len());
    let (older, rest) = ok_page(&home, json!({"project": PROJECT, "before": before}));
    assert_eq!(older, rev(&refs[..10]));
    assert_eq!(rest, None);
}

#[test]
fn a_bad_project_key_touches_no_file() {
    let home = Home::new();
    // A file that `..` or an absolute path would reach.
    fs::write(home.dir.join("history.jsonl"), line("outside")).unwrap();
    let projects = home.dir.join("projects");
    fs::create_dir_all(projects.join("a")).unwrap();
    fs::write(projects.join("a/history.jsonl"), line("nested")).unwrap();
    for key in ["", ".", "..", "a/b", "/a", "a\0b"] {
        assert_eq!(
            home.ask(json!({"project": key})),
            Err((ErrorCode::InvalidArguments, super::UNFIT)),
            "{key:?}"
        );
    }
}

#[test]
fn a_key_with_dots_inside_is_a_key() {
    let home = Home::new();
    for key in ["...", ".a", "a..b", "-Users-a-.git"] {
        assert_eq!(
            home.ask(json!({"project": key})).unwrap(),
            json!({"prompts": []}),
            "{key:?}"
        );
    }
}

#[test]
fn bad_arguments_are_invalid_arguments() {
    let home = Home::new();
    home.history(line("a").as_bytes());
    for args in [
        json!({}),
        json!({"before": 1}),
        json!({"project": 1}),
        json!({"project": null}),
        json!({"project": PROJECT, "before": -1}),
        json!({"project": PROJECT, "before": 1.5}),
        json!({"project": PROJECT, "before": "3"}),
        json!({"project": PROJECT, "before": null}),
        json!({"project": PROJECT, "extra": 1}),
    ] {
        assert_eq!(
            home.ask(args.clone()),
            Err((ErrorCode::InvalidArguments, super::UNFIT)),
            "{args}"
        );
    }
}
