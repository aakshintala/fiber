//! Tests beside [`super::prepare`]: the exact shapes, what a file that
//! cannot be read does, and what a model that cannot take images gets.

use contract::provider::ImageRef;
use serde_json::json;

use super::{anthropic_content, data_url, prepare};

fn image(path: &str) -> ImageRef {
    ImageRef {
        path: path.to_owned(),
        mime_type: "image/png".to_owned(),
        width: 2,
        height: 1,
    }
}

/// Today's `content` output through the new API, for a model that takes
/// images.
fn content(text: &str, images: &[ImageRef], session_dir: &std::path::Path) -> serde_json::Value {
    anthropic_content(prepare(text, images, session_dir, false))
}

#[test]
fn no_image_leaves_the_content_a_plain_string() {
    assert_eq!(
        content("done\n", &[], std::path::Path::new("/nowhere")),
        json!("done\n")
    );
    // ... for either value of `text_only`.
    let prepared = prepare("done\n", &[], std::path::Path::new("/nowhere"), true);
    assert_eq!(prepared.text, "done\n");
    assert!(prepared.images.is_empty());
    assert_eq!(anthropic_content(prepared), json!("done\n"));
}

#[test]
fn a_stored_image_follows_the_text_as_a_base64_block() {
    let dir = fakes::TempDir::new("fiber-anthropic-image");
    std::fs::create_dir(dir.path().join("artifacts")).unwrap();
    // "abcd" is `YWJjZA==`.
    std::fs::write(dir.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    assert_eq!(
        content(
            "Image: 2x1 image/png.\n",
            &[image("artifacts/i_1.png")],
            dir.path()
        ),
        json!([
            {"type": "text", "text": "Image: 2x1 image/png.\n"},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "YWJjZA=="}},
        ])
    );
}

#[test]
fn empty_text_sends_no_text_block() {
    let dir = fakes::TempDir::new("fiber-anthropic-image");
    std::fs::write(dir.path().join("i.png"), b"abcd").unwrap();
    let value = content("", &[image("i.png")], dir.path());
    assert_eq!(value.as_array().map(Vec::len), Some(1));
    assert_eq!(value[0]["type"], "image");
}

#[test]
fn a_missing_file_is_named_in_the_text_and_not_sent() {
    let dir = fakes::TempDir::new("fiber-anthropic-image");
    assert_eq!(
        content(
            "Image: 2x1 image/png.\n",
            &[image("artifacts/gone.png")],
            dir.path()
        ),
        json!("Image: 2x1 image/png.\n[Image artifacts/gone.png could not be read.]")
    );
    assert_eq!(
        content("no newline", &[image("artifacts/gone.png")], dir.path()),
        json!("no newline\n[Image artifacts/gone.png could not be read.]")
    );
}

#[test]
fn a_readable_image_is_sent_beside_one_that_is_missing() {
    let dir = fakes::TempDir::new("fiber-anthropic-image");
    std::fs::write(dir.path().join("ok.png"), b"abcd").unwrap();
    let value = content("t\n", &[image("gone.png"), image("ok.png")], dir.path());
    assert_eq!(
        value[0],
        json!({"type": "text", "text": "t\n[Image gone.png could not be read.]"})
    );
    assert_eq!(value[1]["source"]["data"], "YWJjZA==");
    assert_eq!(value.as_array().map(Vec::len), Some(2));
}

#[test]
fn a_path_that_leaves_the_session_directory_is_not_read() {
    let dir = fakes::TempDir::new("fiber-anthropic-image");
    let outside = dir.path().join("outside.png");
    std::fs::write(&outside, b"abcd").unwrap();
    let inner = dir.path().join("session");
    std::fs::create_dir(&inner).unwrap();
    let escaping = content("t\n", &[image("../outside.png")], &inner);
    assert_eq!(
        escaping,
        json!("t\n[Image ../outside.png could not be read.]")
    );
    let absolute = content("t\n", &[image(outside.to_str().unwrap())], &inner);
    assert!(absolute.is_string());
}

#[test]
fn text_only_sends_no_image_and_says_the_image_was_left_out() {
    let dir = fakes::TempDir::new("fiber-anthropic-image");
    std::fs::create_dir(dir.path().join("artifacts")).unwrap();
    std::fs::write(dir.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let prepared = prepare(
        "Image: 2x1 image/png.\n",
        &[image("artifacts/i_1.png")],
        dir.path(),
        true,
    );
    assert!(prepared.images.is_empty());
    assert_eq!(
        anthropic_content(prepared),
        json!(
            "Image: 2x1 image/png.\n[Image artifacts/i_1.png left out: this model does not take images.]"
        )
    );
}

#[test]
fn text_only_never_reads_the_filesystem() {
    // The file is missing, yet the line says it was left out, not that it
    // could not be read: the file was never opened.
    let dir = fakes::TempDir::new("fiber-anthropic-image");
    let prepared = prepare(
        "no newline",
        &[image("artifacts/gone.png")],
        dir.path(),
        true,
    );
    assert!(prepared.images.is_empty());
    assert_eq!(
        prepared.text,
        "no newline\n[Image artifacts/gone.png left out: this model does not take images.]"
    );
}

#[test]
fn text_only_leaves_out_each_image_in_order() {
    let dir = fakes::TempDir::new("fiber-anthropic-image");
    std::fs::write(dir.path().join("ok.png"), b"abcd").unwrap();
    let prepared = prepare(
        "t\n",
        &[image("gone.png"), image("ok.png")],
        dir.path(),
        true,
    );
    assert!(prepared.images.is_empty());
    assert_eq!(
        prepared.text,
        "t\n[Image gone.png left out: this model does not take images.]\n[Image ok.png left out: this model does not take images.]"
    );
}

#[test]
fn a_data_url_holds_the_mime_type_and_the_base64() {
    let dir = fakes::TempDir::new("fiber-anthropic-image");
    std::fs::write(dir.path().join("i.png"), b"abcd").unwrap();
    let prepared = prepare("t\n", &[image("i.png")], dir.path(), false);
    assert_eq!(prepared.images.len(), 1);
    assert_eq!(
        data_url(&prepared.images[0]),
        "data:image/png;base64,YWJjZA=="
    );
}
