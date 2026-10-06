//! Tests beside [`super::decode`]: precedence first, then each source.
//!
//! `#![allow(..., reason = ...)]` header shared by the test modules in this
//! crate: tests unwrap and index freely.
#![allow(clippy::unwrap_used, reason = "tests unwrap")]
#![allow(clippy::indexing_slicing, reason = "tests index")]

use super::decode;

// Shift-JIS for "あ" and windows-1252 for "é".
const SHIFT_JIS_A: &[u8] = &[0x82, 0xa0];
const LATIN1_E_ACUTE: &[u8] = &[0xe9];

#[test]
fn a_bom_wins_over_the_header() {
    let mut bytes = b"\xef\xbb\xbf".to_vec();
    bytes.extend_from_slice(&[0xe9]);
    let text = decode(Some("text/html; charset=shift_jis"), &bytes);
    assert_eq!(text.as_ref(), "�");
}

#[test]
fn a_utf16_bom_decodes_as_utf16() {
    // "Hi" in UTF-16LE with its BOM.
    let bytes = [0xff, 0xfe, 0x48, 0x00, 0x69, 0x00];
    assert_eq!(decode(None, &bytes).as_ref(), "Hi");
    // "Hi" in UTF-16BE with its BOM.
    let bytes = [0xfe, 0xff, 0x00, 0x48, 0x00, 0x69];
    assert_eq!(decode(None, &bytes).as_ref(), "Hi");
}

#[test]
fn the_header_charset_decodes_shift_jis() {
    let text = decode(Some("text/html; charset=shift_jis"), SHIFT_JIS_A);
    assert_eq!(text.as_ref(), "あ");
}

#[test]
fn the_header_charset_name_is_trimmed_case_insensitive_and_may_be_quoted() {
    for content_type in [
        "text/html; charset=Shift_JIS",
        "text/html; CHARSET = \"shift_jis\"",
        "text/html ; charset =  shift_jis  ",
        "text/html; foo=bar; charset=shift_jis",
    ] {
        assert_eq!(
            decode(Some(content_type), SHIFT_JIS_A).as_ref(),
            "あ",
            "{content_type}"
        );
    }
}

#[test]
fn the_headers_utf16_is_honoured() {
    // "Hi" in UTF-16LE without a BOM, named by the header.
    let bytes = [0x48, 0x00, 0x69, 0x00];
    assert_eq!(
        decode(Some("text/html; charset=utf-16le"), &bytes).as_ref(),
        "Hi"
    );
}

#[test]
fn an_unknown_header_label_falls_through_to_the_meta() {
    let mut page = b"<meta charset=\"shift_jis\">".to_vec();
    page.extend_from_slice(SHIFT_JIS_A);
    let text = decode(Some("text/html; charset=unknown-9"), &page);
    assert!(text.as_ref().contains("あ"));
}

#[test]
fn a_meta_charset_decodes_the_page() {
    let mut page = b"<meta charset=\"shift_jis\">".to_vec();
    page.extend_from_slice(SHIFT_JIS_A);
    assert!(decode(None, &page).as_ref().contains("あ"));
}

#[test]
fn a_meta_http_equiv_content_decodes_the_page() {
    let mut page =
        b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=shift_jis\">".to_vec();
    page.extend_from_slice(SHIFT_JIS_A);
    assert!(decode(None, &page).as_ref().contains("あ"));
}

#[test]
fn a_meta_with_a_quoted_charset_in_content_decodes() {
    let mut page =
        b"<meta http-equiv=\"content-type\" content='text/html; charset=\"shift_jis\"'>".to_vec();
    page.extend_from_slice(SHIFT_JIS_A);
    assert!(decode(None, &page).as_ref().contains("あ"));
}

#[test]
fn a_meta_past_1024_bytes_is_ignored() {
    let mut page = vec![b' '; 1024];
    page.extend_from_slice(b"<meta charset=\"shift_jis\">");
    page.extend_from_slice(SHIFT_JIS_A);
    // Falls back to UTF-8: the Shift-JIS bytes are invalid there.
    assert!(!decode(None, &page).as_ref().contains("あ"));
}

#[test]
fn a_meta_just_inside_1024_bytes_counts() {
    let mut page = vec![b' '; 900];
    page.extend_from_slice(b"<meta charset=\"shift_jis\">");
    page.extend_from_slice(SHIFT_JIS_A);
    assert!(decode(None, &page).as_ref().contains("あ"));
}

#[test]
fn a_meta_naming_utf16_means_utf8() {
    let page = b"<meta charset=\"utf-16le\">Hi".to_vec();
    assert_eq!(
        decode(None, &page).as_ref(),
        "<meta charset=\"utf-16le\">Hi"
    );
}

#[test]
fn a_meta_naming_x_user_defined_means_windows_1252() {
    let mut page = b"<meta charset=\"x-user-defined\">".to_vec();
    page.extend_from_slice(LATIN1_E_ACUTE);
    assert!(decode(None, &page).as_ref().contains("é"));
}

#[test]
fn an_unknown_meta_label_falls_through_to_utf8() {
    let page = b"<meta charset=\"unknown-9\">Hi".to_vec();
    assert_eq!(
        decode(None, &page).as_ref(),
        "<meta charset=\"unknown-9\">Hi"
    );
    // ... but a later known meta is still found.
    let page = b"<meta charset=\"unknown-9\"><meta charset=\"shift_jis\">\x82\xa0".to_vec();
    assert!(decode(None, &page).as_ref().contains("あ"));
}

#[test]
fn a_meta_inside_a_comment_is_not_a_charset() {
    let mut page = b"<!-- <meta charset=\"shift_jis\"> -->".to_vec();
    page.extend_from_slice(SHIFT_JIS_A);
    assert!(!decode(None, &page).as_ref().contains("あ"));
}

#[test]
fn the_header_wins_over_the_meta() {
    let mut page = b"<meta charset=\"shift_jis\">".to_vec();
    page.extend_from_slice(LATIN1_E_ACUTE);
    // windows-1252 reads 0xe9 as "é"; Shift-JIS would fail it.
    let text = decode(Some("text/html; charset=windows-1252"), &page);
    assert!(text.as_ref().contains("é"));
}

#[test]
fn nothing_known_is_utf8_lossy() {
    assert_eq!(decode(None, b"hello").as_ref(), "hello");
    assert_eq!(decode(Some("text/html"), b"a\xffb").as_ref(), "a�b");
}

#[test]
fn utf8_borrows_when_valid() {
    let bytes = b"hello".to_vec();
    let text = decode(None, &bytes);
    assert!(matches!(text, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn a_header_charset_after_a_bare_parameter_still_decodes() {
    let text = decode(Some("text/html; foo; charset=shift_jis"), SHIFT_JIS_A);
    assert_eq!(text.as_ref(), "あ");
}

#[test]
fn a_meta_content_without_an_equals_falls_back_to_utf8() {
    let mut page =
        b"<meta http-equiv=\"content-type\" content=\"text/html; charset shift_jis\">".to_vec();
    page.extend_from_slice(SHIFT_JIS_A);
    assert!(!decode(None, &page).as_ref().contains("あ"));
}

#[test]
fn windows_1252_bytes_decode_to_their_text() {
    let mut page = b"<meta charset=\"windows-1252\">".to_vec();
    page.extend_from_slice(b"\x93quoted\x94");
    assert!(decode(None, &page).as_ref().contains("“quoted”"));
}
