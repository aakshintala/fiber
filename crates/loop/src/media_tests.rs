use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use contract::ActionId;
use contract::provider::{ImageRef, Input, PdfRef};

use super::{Cache, resolve_media};

/// A session directory under `<root>/sessions/s_child`.
struct Dir {
    root: fakes::TempDir,
    path: PathBuf,
}

impl Dir {
    fn new() -> Self {
        let root = fakes::TempDir::new("loop-media");
        let path = root.path().join("sessions").join("s_child");
        fs::create_dir_all(path.join("artifacts")).unwrap();
        Self { root, path }
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        fs::write(self.path.join(rel), bytes).unwrap();
    }
}

fn image(path: &str) -> ImageRef {
    ImageRef {
        path: path.into(),
        mime_type: "image/png".into(),
        width: 1,
        height: 1,
    }
}

fn user(paths: &[&str]) -> Vec<Input> {
    vec![Input::User {
        text: "t".into(),
        images: paths.iter().map(|path| image(path)).collect(),
    }]
}

fn tool_result(images: &[&str], pdfs: Vec<PdfRef>) -> Input {
    Input::ToolResult {
        action_id: ActionId("a".into()),
        text: "t".into(),
        is_error: false,
        images: images.iter().map(|path| image(path)).collect(),
        pdfs,
    }
}

#[test]
fn every_readable_image_pdf_and_page_maps_to_its_base64() {
    let dir = Dir::new();
    dir.write("artifacts/i.png", b"abcd");
    dir.write("artifacts/p.pdf", b"abc");
    dir.write("artifacts/p1.png", b"ab");
    let conversation = [tool_result(
        &["artifacts/i.png"],
        vec![PdfRef {
            path: "artifacts/p.pdf".into(),
            page_count: 1,
            pages: Some(vec![image("artifacts/p1.png")]),
        }],
    )];

    let media = Cache::default().resolve(&dir.path, &conversation);

    assert_eq!(&*media["artifacts/i.png"], "YWJjZA==");
    assert_eq!(&*media["artifacts/p.pdf"], "YWJj");
    assert_eq!(&*media["artifacts/p1.png"], "YWI=");
}

#[test]
fn a_second_resolve_returns_the_first_encoding_of_a_changed_file() {
    let dir = Dir::new();
    dir.write("artifacts/i.png", b"abcd");
    let cache = Cache::default();
    let conversation = user(&["artifacts/i.png"]);

    let first = cache.resolve(&dir.path, &conversation);
    dir.write("artifacts/i.png", b"zzzz");
    let second = cache.resolve(&dir.path, &conversation);

    assert_eq!(&*second["artifacts/i.png"], "YWJjZA==");
    assert!(Arc::ptr_eq(
        &first["artifacts/i.png"],
        &second["artifacts/i.png"]
    ));
}

#[test]
fn an_unreadable_path_is_absent_and_read_again_once_the_file_exists() {
    let dir = Dir::new();
    let cache = Cache::default();
    let conversation = user(&["artifacts/late.png"]);

    assert!(cache.resolve(&dir.path, &conversation).is_empty());
    dir.write("artifacts/late.png", b"abcd");
    let media = cache.resolve(&dir.path, &conversation);

    assert_eq!(&*media["artifacts/late.png"], "YWJjZA==");
}

#[test]
fn a_path_the_session_may_not_read_is_absent_although_the_file_exists() {
    let dir = Dir::new();
    fs::write(dir.root.path().join("sessions").join("x"), b"abcd").unwrap();

    let media = Cache::default().resolve(&dir.path, &user(&["../x"]));

    assert!(media.is_empty());
}

#[test]
fn a_conversation_without_media_is_empty_and_reads_nothing() {
    let missing = PathBuf::from("/no/such/session/dir");
    let conversation = [Input::Assistant {
        model: "m".into(),
        text: "t".into(),
        provider_item: None,
    }];

    assert!(Cache::default().resolve(&missing, &conversation).is_empty());
}

#[test]
fn resolve_media_returns_what_a_fresh_cache_resolves() {
    let dir = Dir::new();
    dir.write("artifacts/i.png", b"abcd");
    let conversation = user(&["artifacts/i.png", "artifacts/gone.png"]);

    assert_eq!(
        resolve_media(&dir.path, &conversation),
        Cache::default().resolve(&dir.path, &conversation)
    );
}

#[test]
fn an_unreadable_path_does_not_hide_the_readable_paths_after_it() {
    let dir = Dir::new();
    dir.write("artifacts/i.png", b"abcd");

    let media =
        Cache::default().resolve(&dir.path, &user(&["artifacts/gone.png", "artifacts/i.png"]));

    assert_eq!(&*media["artifacts/i.png"], "YWJjZA==");
    assert!(!media.contains_key("artifacts/gone.png"));
}

#[test]
fn a_path_the_conversation_no_longer_names_is_encoded_afresh_when_it_returns() {
    let dir = Dir::new();
    dir.write("artifacts/a.png", b"abcd");
    dir.write("artifacts/b.png", b"b");
    let cache = Cache::default();

    cache.resolve(&dir.path, &user(&["artifacts/a.png"]));
    // A handoff dropped the image: a request without it evicts its encoding.
    cache.resolve(&dir.path, &user(&["artifacts/b.png"]));
    dir.write("artifacts/a.png", b"zzzz");
    let media = cache.resolve(&dir.path, &user(&["artifacts/a.png"]));

    assert_eq!(&*media["artifacts/a.png"], "enp6eg==");
}
