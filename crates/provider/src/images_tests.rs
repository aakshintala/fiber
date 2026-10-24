//! Tests beside [`super::prepare`]: the exact shapes, what a file that
//! cannot be read does, and what a model that cannot take images gets.

use contract::events::CacheLifetime;
use contract::provider::{ImageRef, Input, InputSize, Media, ModelRequest, PdfRef};
use serde_json::json;

use super::{PdfForm, anthropic_content, data_url, input_size, prepare};

fn image(path: &str) -> ImageRef {
    ImageRef {
        path: path.to_owned(),
        mime_type: "image/png".to_owned(),
        width: 2,
        height: 1,
    }
}

/// The loop's map for files holding these bytes: each path to the bytes as
/// standard base64.
fn media(files: &[(&str, &[u8])]) -> Media {
    use base64::Engine as _;
    files
        .iter()
        .map(|(path, bytes)| {
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            ((*path).to_owned(), std::sync::Arc::from(data))
        })
        .collect()
}

/// The `anthropic-messages` content for a model that takes images.
fn content(text: &str, images: &[ImageRef], media: &Media) -> serde_json::Value {
    anthropic_content(prepare(text, images, &[], media, false, PdfForm::Native))
}

#[test]
fn image_and_pdf_text_files_pin_every_byte() {
    assert_eq!(
        include_bytes!("../prompt/image-left-out.md").as_slice(),
        b"[Image {path} left out: this model does not take images.]\n"
    );
    assert_eq!(
        include_bytes!("../prompt/image-unreadable.md").as_slice(),
        b"[Image {path} could not be read.]\n"
    );
    assert_eq!(
        include_bytes!("../prompt/pdf-left-out.md").as_slice(),
        b"[PDF {path} left out: this model does not take images.]\n"
    );
    assert_eq!(
        include_bytes!("../prompt/pdf-unreadable.md").as_slice(),
        b"[PDF {path} could not be read.]\n"
    );
}

#[test]
fn no_image_leaves_the_content_a_plain_string() {
    assert_eq!(content("done\n", &[], &Media::new()), json!("done\n"));
    // ... for either value of `text_only`.
    let prepared = prepare("done\n", &[], &[], &Media::new(), true, PdfForm::Native);
    assert_eq!(prepared.text, "done\n");
    assert!(prepared.images.is_empty());
    assert_eq!(anthropic_content(prepared), json!("done\n"));
}

#[test]
fn a_stored_image_follows_the_text_as_a_base64_block() {
    // "abcd" is `YWJjZA==`.
    let media = media(&[("artifacts/i_1.png", b"abcd")]);
    assert_eq!(
        content(
            "Image: 2x1 image/png.\n",
            &[image("artifacts/i_1.png")],
            &media
        ),
        json!([
            {"type": "text", "text": "Image: 2x1 image/png.\n"},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "YWJjZA=="}},
        ])
    );
}

#[test]
fn a_readable_image_is_sent_beside_one_that_is_missing() {
    let media = media(&[("ok.png", b"abcd")]);
    let value = content("t\n", &[image("gone.png"), image("ok.png")], &media);
    assert_eq!(
        value[0],
        json!({"type": "text", "text": "t\n[Image gone.png could not be read.]"})
    );
    assert_eq!(value[1]["source"]["data"], "YWJjZA==");
    assert_eq!(value.as_array().map(Vec::len), Some(2));
}

#[test]
fn text_only_sends_no_image_and_says_the_image_was_left_out() {
    let media = media(&[("artifacts/i_1.png", b"abcd")]);
    let prepared = prepare(
        "Image: 2x1 image/png.\n",
        &[image("artifacts/i_1.png")],
        &[],
        &media,
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
fn a_text_only_endpoint_ignores_the_map() {
    // The map holds one image and lacks the other, yet both lines say the
    // image was left out, not that it could not be read.
    let media = media(&[("artifacts/here.png", b"abcd")]);
    let prepared = prepare(
        "no newline",
        &[image("artifacts/here.png"), image("artifacts/gone.png")],
        &[],
        &media,
        true,
        PdfForm::Native,
    );
    assert!(prepared.images.is_empty());
    assert_eq!(
        prepared.text,
        "no newline\n[Image artifacts/here.png left out: this model does not take images.]\n[Image artifacts/gone.png left out: this model does not take images.]"
    );
}

#[test]
fn text_only_leaves_out_each_image_in_order() {
    let media = media(&[("ok.png", b"abcd")]);
    let prepared = prepare(
        "t\n",
        &[image("gone.png"), image("ok.png")],
        &[],
        &media,
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
fn a_path_is_looked_up_by_its_string_whatever_its_form() {
    // The map is the only authority: an absolute key is sent, and the same
    // file under another spelling is not.
    let media = media(&[("/h/sessions/s_parent/artifacts/i.png", b"abcd")]);
    assert_eq!(
        content(
            "t\n",
            &[image("/h/sessions/s_parent/artifacts/i.png")],
            &media
        ),
        json!([
            {"type": "text", "text": "t\n"},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "YWJjZA=="}},
        ])
    );
    assert_eq!(
        content(
            "t\n",
            &[image("/h/sessions/s_parent/artifacts/./i.png")],
            &media
        ),
        json!("t\n[Image /h/sessions/s_parent/artifacts/./i.png could not be read.]")
    );
}

#[test]
fn a_data_url_holds_the_mime_type_and_the_base64() {
    let media = media(&[("i.png", b"abcd")]);
    let prepared = prepare(
        "t\n",
        &[image("i.png")],
        &[],
        &media,
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
    // "abcd" is `YWJjZA==`.
    let media = media(&[("artifacts/p_1.pdf", b"abcd")]);
    let prepared = prepare(
        "PDF: 2 pages.\n",
        &[],
        &[pdf("artifacts/p_1.pdf", None)],
        &media,
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
    let prepared = prepare(
        "PDF: 2 pages.\n",
        &[],
        &[pdf("artifacts/gone.pdf", None)],
        &media(&[("artifacts/other.pdf", b"abcd")]),
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
fn a_native_pdf_named_by_an_absolute_path_is_sent_under_its_base_name() {
    let path = "/h/sessions/s_parent/artifacts/p_1.pdf";
    let prepared = prepare(
        "t\n",
        &[],
        &[pdf(path, None)],
        &media(&[(path, b"abcd")]),
        false,
        PdfForm::Native,
    );
    assert_eq!(prepared.documents[0].filename, "p_1.pdf");
    assert_eq!(prepared.documents[0].data, "YWJjZA==");
}

#[test]
fn pages_send_each_page_as_an_image() {
    let media = media(&[
        ("artifacts/i_1.png", b"abcd"),
        ("artifacts/i_2.png", b"efgh"),
    ]);
    let prepared = prepare(
        "PDF: 2 pages.\n",
        &[],
        &[pdf(
            "artifacts/p_1.pdf",
            Some(vec![image("artifacts/i_1.png"), image("artifacts/i_2.png")]),
        )],
        &media,
        false,
        PdfForm::Pages,
    );
    assert_eq!(prepared.text, "PDF: 2 pages.\n");
    assert!(prepared.documents.is_empty());
    // No doc promises the pages' order, so they are compared as a set.
    let urls: std::collections::BTreeSet<String> = prepared.images.iter().map(data_url).collect();
    assert_eq!(
        urls,
        [
            "data:image/png;base64,YWJjZA==".to_owned(),
            "data:image/png;base64,ZWZnaA==".to_owned(),
        ]
        .into()
    );
}

#[test]
fn pages_without_rendered_pages_add_nothing() {
    let prepared = prepare(
        "PDF: 2 pages.\n",
        &[],
        &[pdf("artifacts/p_1.pdf", None)],
        &Media::new(),
        false,
        PdfForm::Pages,
    );
    assert_eq!(prepared.text, "PDF: 2 pages.\n");
    assert!(prepared.images.is_empty());
    assert!(prepared.documents.is_empty());
    assert_eq!(anthropic_content(prepared), json!("PDF: 2 pages.\n"));
}

#[test]
fn text_only_leaves_out_each_pdf_whatever_the_map_holds() {
    let media = media(&[("artifacts/gone.pdf", b"abcd")]);
    let prepared = prepare(
        "t\n",
        &[],
        &[pdf(
            "artifacts/gone.pdf",
            Some(vec![image("artifacts/i_1.png")]),
        )],
        &media,
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

#[test]
fn input_size_counts_tool_search_results_as_text_not_media() {
    let request = sized_request(vec![Input::ToolSearchResult {
        action_id: contract::ActionId("a_1".into()),
        loaded: vec!["a".into(), "b".into()],
    }]);
    assert_eq!(
        input_size(b"abc", &request, false),
        InputSize {
            bytes: 3,
            media: false
        }
    );
}

#[test]
fn input_size_counts_an_image_the_map_lacks() {
    // `sized_request` carries an empty map: the image is unreadable, and
    // still counts.
    assert_eq!(
        input_size(b"abc", &sized_request(vec![user_with_image()]), false),
        InputSize {
            bytes: 3,
            media: true,
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
        conversation: conversation.into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        media: Default::default(),
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
