//! Tests beside [`super::prepare`]: the exact shapes, what a file that
//! cannot be read does, and what a model that cannot take images gets.

use contract::events::CacheLifetime;
use contract::provider::{ImageRef, Input, InputSize, ModelRequest, PdfRef};
use serde_json::json;

use super::{PdfForm, anthropic_content, data_url, input_size, media_path, prepare};

fn image(path: &str) -> ImageRef {
    ImageRef {
        path: path.to_owned(),
        mime_type: "image/png".to_owned(),
        width: 2,
        height: 1,
    }
}

/// The `anthropic-messages` content for a model that takes images.
fn content(text: &str, images: &[ImageRef], session_dir: &std::path::Path) -> serde_json::Value {
    anthropic_content(prepare(
        text,
        images,
        &[],
        session_dir,
        false,
        PdfForm::Native,
    ))
}

#[test]
fn no_image_leaves_the_content_a_plain_string() {
    assert_eq!(
        content("done\n", &[], std::path::Path::new("/nowhere")),
        json!("done\n")
    );
    // ... for either value of `text_only`.
    let prepared = prepare(
        "done\n",
        &[],
        &[],
        std::path::Path::new("/nowhere"),
        true,
        PdfForm::Native,
    );
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
        &[],
        dir.path(),
        true,
        PdfForm::Native,
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
        &[],
        dir.path(),
        true,
        PdfForm::Native,
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
        &[],
        dir.path(),
        true,
        PdfForm::Native,
    );
    assert!(prepared.images.is_empty());
    assert_eq!(
        prepared.text,
        "t\n[Image gone.png left out: this model does not take images.]\n[Image ok.png left out: this model does not take images.]"
    );
}

#[test]
fn media_path_resolves_relative_paths_under_the_session_dir() {
    let root = fakes::TempDir::new("fiber-media-path");
    let sessions = root.path().join("sessions");
    let parent = sessions.join("s_parent");
    let child = sessions.join("s_child");
    let resolved = media_path("artifacts/i.png", &child).unwrap();
    assert_eq!(resolved, child.join("artifacts/i.png"));
    assert_eq!(media_path("../x", &child), None);
    assert_eq!(media_path("./x", &child), None);
    let nested = parent.join("artifacts/sub/i.png").display().to_string();
    assert_eq!(
        media_path(&nested, &child),
        Some(parent.join("artifacts/sub/i.png"))
    );
    let stored = parent.join("artifacts/i.png").display().to_string();
    assert_eq!(
        media_path(&stored, &child),
        Some(parent.join("artifacts/i.png"))
    );
    let log = parent.join("events.jsonl").display().to_string();
    assert_eq!(media_path(&log, &child), None);
    let bare = parent.join("artifacts").display().to_string();
    assert_eq!(media_path(&bare, &child), None);
    let escaped = parent
        .join("artifacts/../events.jsonl")
        .display()
        .to_string();
    assert_eq!(media_path(&escaped, &child), None);
    let outside = root.path().join("elsewhere/i.png").display().to_string();
    assert_eq!(media_path(&outside, &child), None);
    let no_session = sessions.join("artifacts/i.png").display().to_string();
    assert_eq!(media_path(&no_session, &child), None);
}

#[test]
fn prepare_sends_a_parent_image_named_by_its_absolute_path() {
    let root = fakes::TempDir::new("fiber-media-path-parent");
    let sessions = root.path().join("sessions");
    let parent = sessions.join("s_parent");
    let child = sessions.join("s_child");
    std::fs::create_dir_all(parent.join("artifacts")).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    // "abcd" is `YWJjZA==`.
    std::fs::write(parent.join("artifacts/i.png"), b"abcd").unwrap();
    let absolute = parent.join("artifacts/i.png").display().to_string();
    assert_eq!(
        content("t\n", &[image(&absolute)], &child),
        json!([
            {"type": "text", "text": "t\n"},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "YWJjZA=="}},
        ])
    );
}

#[test]
fn a_data_url_holds_the_mime_type_and_the_base64() {
    let dir = fakes::TempDir::new("fiber-anthropic-image");
    std::fs::write(dir.path().join("i.png"), b"abcd").unwrap();
    let prepared = prepare(
        "t\n",
        &[image("i.png")],
        &[],
        dir.path(),
        false,
        PdfForm::Native,
    );
    assert_eq!(prepared.images.len(), 1);
    assert_eq!(
        data_url(&prepared.images[0]),
        "data:image/png;base64,YWJjZA=="
    );
}

fn pdf(path: &str, pages: Option<Vec<ImageRef>>) -> PdfRef {
    PdfRef {
        path: path.to_owned(),
        page_count: pages
            .as_ref()
            .map_or(2, |pages| u32::try_from(pages.len()).unwrap_or(u32::MAX)),
        pages,
    }
}

#[test]
fn a_native_pdf_is_sent_as_a_document_with_its_bytes_unchanged() {
    let dir = fakes::TempDir::new("fiber-pdf-document");
    std::fs::create_dir(dir.path().join("artifacts")).unwrap();
    // "abcd" is `YWJjZA==`.
    std::fs::write(dir.path().join("artifacts/p_1.pdf"), b"abcd").unwrap();
    let prepared = prepare(
        "PDF: 2 pages.\n",
        &[],
        &[pdf("artifacts/p_1.pdf", None)],
        dir.path(),
        false,
        PdfForm::Native,
    );
    assert_eq!(prepared.text, "PDF: 2 pages.\n");
    assert!(prepared.images.is_empty());
    assert_eq!(prepared.documents.len(), 1);
    assert_eq!(prepared.documents[0].filename, "p_1.pdf");
    assert_eq!(prepared.documents[0].data, "YWJjZA==");
    assert_eq!(
        anthropic_content(prepared),
        json!([
            {"type": "text", "text": "PDF: 2 pages.\n"},
            {"type": "document", "source": {
                "type": "base64", "media_type": "application/pdf", "data": "YWJjZA=="}},
        ])
    );
}

#[test]
fn a_native_pdf_whose_file_is_missing_is_named_in_the_text() {
    let dir = fakes::TempDir::new("fiber-pdf-missing");
    let prepared = prepare(
        "PDF: 2 pages.\n",
        &[],
        &[pdf("artifacts/gone.pdf", None)],
        dir.path(),
        false,
        PdfForm::Native,
    );
    assert!(prepared.documents.is_empty());
    assert_eq!(
        anthropic_content(prepared),
        json!("PDF: 2 pages.\n[PDF artifacts/gone.pdf could not be read.]")
    );
}

#[test]
fn a_native_pdf_that_leaves_the_session_directory_is_not_read() {
    let dir = fakes::TempDir::new("fiber-pdf-escape");
    let prepared = prepare(
        "t\n",
        &[],
        &[pdf("../outside.pdf", None)],
        &dir.path().join("session"),
        false,
        PdfForm::Native,
    );
    assert!(prepared.documents.is_empty());
    assert_eq!(prepared.text, "t\n[PDF ../outside.pdf could not be read.]");
}

#[test]
fn pages_send_each_page_as_an_image_in_order() {
    let dir = fakes::TempDir::new("fiber-pdf-pages");
    std::fs::create_dir(dir.path().join("artifacts")).unwrap();
    std::fs::write(dir.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    std::fs::write(dir.path().join("artifacts/i_2.png"), b"efgh").unwrap();
    let prepared = prepare(
        "PDF: 2 pages.\n",
        &[],
        &[pdf(
            "artifacts/p_1.pdf",
            Some(vec![image("artifacts/i_1.png"), image("artifacts/i_2.png")]),
        )],
        dir.path(),
        false,
        PdfForm::Pages,
    );
    assert_eq!(prepared.text, "PDF: 2 pages.\n");
    assert!(prepared.documents.is_empty());
    assert_eq!(prepared.images.len(), 2);
    assert_eq!(
        data_url(&prepared.images[0]),
        "data:image/png;base64,YWJjZA=="
    );
}

#[test]
fn pages_without_rendered_pages_add_nothing() {
    let dir = fakes::TempDir::new("fiber-pdf-no-pages");
    let prepared = prepare(
        "PDF: 2 pages.\n",
        &[],
        &[pdf("artifacts/p_1.pdf", None)],
        dir.path(),
        false,
        PdfForm::Pages,
    );
    assert_eq!(prepared.text, "PDF: 2 pages.\n");
    assert!(prepared.images.is_empty());
    assert!(prepared.documents.is_empty());
    assert_eq!(anthropic_content(prepared), json!("PDF: 2 pages.\n"));
}

#[test]
fn text_only_leaves_out_each_pdf_without_reading_it() {
    let dir = fakes::TempDir::new("fiber-pdf-text-only");
    let prepared = prepare(
        "t\n",
        &[],
        &[pdf(
            "artifacts/gone.pdf",
            Some(vec![image("artifacts/i_1.png")]),
        )],
        dir.path(),
        true,
        PdfForm::Native,
    );
    assert!(prepared.images.is_empty());
    assert!(prepared.documents.is_empty());
    assert_eq!(
        prepared.text,
        "t\n[PDF artifacts/gone.pdf left out: this model does not take images.]"
    );
}

#[test]
fn input_size_counts_a_pdf_as_media_unless_text_only() {
    let pdfs = || {
        sized_request(vec![Input::ToolResult {
            pdfs: vec![pdf("artifacts/p_1.pdf", None)],
            action_id: contract::ActionId("a_1".into()),
            text: "PDF: 2 pages.\n".into(),
            is_error: false,
            images: Vec::new(),
        }])
    };
    assert_eq!(
        input_size(b"abc", &pdfs(), false),
        InputSize {
            bytes: 3,
            media: true,
        }
    );
    assert_eq!(
        input_size(b"abc", &pdfs(), true),
        InputSize {
            bytes: 3,
            media: false,
        }
    );
}

fn sized_request(conversation: Vec<Input>) -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: String::new(),
        conversation,
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        session_dir: std::path::PathBuf::new(),
    }
}

fn user_without_images() -> Input {
    Input::User {
        text: "hi".into(),
        images: Vec::new(),
    }
}

fn user_with_image() -> Input {
    Input::User {
        text: "look".into(),
        images: vec![image("artifacts/i_1.png")],
    }
}

#[test]
fn input_size_counts_the_body_it_is_given() {
    assert_eq!(
        input_size(b"abc", &sized_request(vec![user_without_images()]), false),
        InputSize {
            bytes: 3,
            media: false,
        }
    );
    assert_eq!(
        input_size(b"abc", &sized_request(vec![user_with_image()]), false),
        InputSize {
            bytes: 3,
            media: true,
        }
    );
    assert_eq!(
        input_size(
            b"abc",
            &sized_request(vec![Input::ToolResult {
                pdfs: Vec::new(),
                action_id: contract::ActionId("a_1".into()),
                text: "done".into(),
                is_error: false,
                images: vec![image("artifacts/i_1.png")],
            }]),
            false,
        ),
        InputSize {
            bytes: 3,
            media: true,
        }
    );
    assert_eq!(
        input_size(b"abc", &sized_request(vec![user_with_image()]), true),
        InputSize {
            bytes: 3,
            media: false,
        }
    );
    assert_eq!(
        input_size(b"", &sized_request(Vec::new()), false),
        InputSize {
            bytes: 0,
            media: false,
        }
    );
    assert_eq!(
        input_size(
            b"abc",
            &sized_request(vec![
                Input::Assistant {
                    model: "p/m".into(),
                    text: "hi".into(),
                    provider_item: None,
                },
                Input::ToolCall {
                    action_id: contract::ActionId("a_1".into()),
                    call: contract::events::ToolCallRequested {
                        name: "read".into(),
                        arguments: json!({}),
                        provider_id: None,
                        repair: None,
                        ran_by: None,
                        provider_item: None,
                    },
                    model: "p/m".into(),
                },
            ]),
            false,
        ),
        InputSize {
            bytes: 3,
            media: false,
        }
    );
}
