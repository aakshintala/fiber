//! Tests beside [`super::content`]: the exact blocks, and what a file that
//! cannot be read does.

use contract::provider::ImageRef;
use serde_json::json;

use super::content;

fn image(path: &str) -> ImageRef {
    ImageRef {
        path: path.to_owned(),
        mime_type: "image/png".to_owned(),
        width: 2,
        height: 1,
    }
}

#[test]
fn no_image_leaves_the_content_a_plain_string() {
    assert_eq!(
        content("done\n", &[], std::path::Path::new("/nowhere")),
        json!("done\n")
    );
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
