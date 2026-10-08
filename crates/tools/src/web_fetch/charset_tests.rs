//! Tests beside [`super::decode`]: precedence first, then each source.
//!
//! `#![allow(..., reason = ...)]` header shared by the test modules in this
//! crate: tests unwrap and index freely.
#![allow(clippy::unwrap_used, reason = "tests unwrap")]
#![allow(clippy::indexing_slicing, reason = "tests index")]

use super::{Decoding, decode, encoding};
use crate::web_fetch::download::Html;
use crate::web_fetch::markdown::to_markdown;

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
    assert_eq!(decode(None, &page), String::from_utf8_lossy(&page));
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
    assert_eq!(decode(None, &page), String::from_utf8_lossy(&page));
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
    assert_eq!(decode(None, &page), String::from_utf8_lossy(&page));
}

#[test]
fn windows_1252_bytes_decode_to_their_text() {
    let mut page = b"<meta charset=\"windows-1252\">".to_vec();
    page.extend_from_slice(b"\x93quoted\x94");
    assert!(decode(None, &page).as_ref().contains("“quoted”"));
}

#[test]
fn a_header_value_that_only_ends_quoted_is_not_stripped() {
    // Both quotes are required: with `||` the mutant strips the first and
    // last byte, turning this unknown label into shift_jis.
    assert_eq!(
        super::header_encoding("text/html; charset=qshift_jis\""),
        None
    );
}

#[test]
fn a_non_meta_start_tag_is_not_a_charset() {
    // Every non-meta tag is skipped: with `&&` the div's charset below
    // would decode the page as Shift-JIS.
    let mut page = b"<div charset=\"shift_jis\">".to_vec();
    page.extend_from_slice(SHIFT_JIS_A);
    assert_eq!(decode(None, &page), String::from_utf8_lossy(&page));
}

#[test]
fn a_meta_content_with_a_single_quoted_charset_decodes() {
    // The closing `'` ends the label: with `&&` it never ends, the label
    // keeps the quote and is unknown, so the page falls back to UTF-8.
    let mut page =
        b"<meta http-equiv=\"content-type\" content=\"text/html; charset='shift_jis'\">".to_vec();
    page.extend_from_slice(SHIFT_JIS_A);
    assert!(decode(None, &page).as_ref().contains("あ"));
}

/// The piece sizes every streaming test cuts its input into: every small
/// size, either side of the 1024-byte prescan, and the download's piece.
const SPLITS: [usize; 13] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 1023, 1024, 1025, 64 * 1024];

/// The byte where the third input's multibyte sequence must be cut.
const BOUNDARY: usize = 64 * 1024;

/// `bytes` decoded through [`Decoding`] in pieces of `split`, then an empty
/// last piece.
fn streamed(content_type: Option<&str>, bytes: &[u8], split: usize) -> String {
    let mut decoding = Decoding::new(encoding(content_type, bytes));
    let mut text = String::new();
    for piece in bytes.chunks(split) {
        decoding.push(piece, false, &mut |part| {
            assert!(part.len() <= 64 * 1024, "a piece of {} bytes", part.len());
            text.push_str(part);
        });
    }
    decoding.push(&[], true, &mut |part| text.push_str(part));
    text
}

/// `bytes` converted through [`Html`] in pieces of `split`.
fn converted(content_type: Option<&str>, bytes: &[u8], split: usize) -> String {
    let mut html = Html::new(content_type);
    for piece in bytes.chunks(split) {
        html.push(piece);
    }
    html.finish()
}

/// UTF-8 text of over 128 KiB after `prefix`, with a three-byte `あ` cut
/// by byte 65,536.
fn long_utf8(prefix: &str) -> Vec<u8> {
    let mut text = prefix.to_owned();
    while (BOUNDARY - text.len()) % 3 != 1 {
        text.push('x');
    }
    text.push_str(&"あ".repeat(50_000));
    assert!(!text.is_char_boundary(BOUNDARY));
    text.into_bytes()
}

/// Shift-JIS text of over 128 KiB after `prefix`, with a two-byte `あ`
/// cut by byte 65,536.
fn long_shift_jis(prefix: &[u8]) -> Vec<u8> {
    let mut bytes = prefix.to_vec();
    if bytes.len().is_multiple_of(2) {
        bytes.push(b'x');
    }
    for _ in 0..70_000 {
        bytes.extend_from_slice(SHIFT_JIS_A);
    }
    assert_eq!(bytes[BOUNDARY - 1], SHIFT_JIS_A[0]);
    bytes
}

/// UTF-16LE text of over 128 KiB with its BOM, `text` first, with a
/// surrogate pair cut by byte 65,536.
fn long_utf16le(text: &str) -> Vec<u8> {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    // The BOM is one unit: pad until each pair starts two bytes off a
    // four-byte boundary.
    while (2 + units.len() * 2) % 4 != 2 {
        units.push(u16::from(b'x'));
    }
    units.extend("😀".repeat(40_000).encode_utf16());
    let mut bytes = vec![0xff, 0xfe];
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    // 65,534 starts a pair: its high surrogate's second byte is 0xd8.
    assert_eq!(bytes[BOUNDARY - 1], 0xd8);
    bytes
}

/// Every input the decoder is checked on, with its `Content-Type`.
fn inputs() -> Vec<(&'static str, Option<&'static str>, Vec<u8>)> {
    let mut utf8_bom = b"\xef\xbb\xbf".to_vec();
    utf8_bom.extend_from_slice("café あ".as_bytes());
    let latin1: Vec<u8> = (0x80..=0xffu8).cycle().take(64 * 1024).collect();
    vec![
        (
            "a UTF-8 BOM over the header",
            Some("text/html; charset=shift_jis"),
            utf8_bom,
        ),
        (
            "a UTF-16LE BOM over the header",
            Some("text/html; charset=windows-1252"),
            vec![0xff, 0xfe, 0x48, 0x00, 0x69, 0x00, 0x3d, 0xd8, 0x00, 0xde],
        ),
        (
            "a UTF-16BE BOM over the header",
            Some("text/html; charset=windows-1252"),
            vec![0xfe, 0xff, 0x00, 0x48, 0x00, 0x69, 0xd8, 0x3d, 0xde, 0x00],
        ),
        (
            "shift_jis by the header",
            Some("text/html; charset=shift_jis"),
            [b"a ".as_slice(), SHIFT_JIS_A, b" b", SHIFT_JIS_A].concat(),
        ),
        (
            "windows-1252",
            Some("text/html; charset=windows-1252"),
            b"caf\xe9 \x93quoted\x94".to_vec(),
        ),
        ("invalid UTF-8", None, b"a\xffb\xc3(c".to_vec()),
        ("a cut multibyte tail", None, b"abc\xe3\x81".to_vec()),
        (
            "a windows-1252 piece wider decoded than the room",
            Some("text/plain; charset=windows-1252"),
            latin1,
        ),
        ("an empty page", None, Vec::new()),
        ("long UTF-8", None, long_utf8("")),
        (
            "long shift_jis",
            Some("text/plain; charset=shift_jis"),
            long_shift_jis(b""),
        ),
        ("long UTF-16LE", None, long_utf16le("")),
    ]
}

#[test]
fn decoding_in_pieces_gives_the_whole_decode() {
    for (name, content_type, bytes) in inputs() {
        let whole = decode(content_type, &bytes);
        for split in SPLITS {
            assert!(
                streamed(content_type, &bytes, split) == whole,
                "{name}, split at {split}"
            );
        }
    }
}

#[test]
fn a_page_ending_inside_a_character_ends_in_a_replacement() {
    assert_eq!(streamed(None, b"abc\xe3\x81", 1), "abc�");
}

#[test]
fn a_piece_wider_decoded_than_the_room_is_handed_over_whole() {
    let latin1: Vec<u8> = (0x80..=0xffu8).cycle().take(64 * 1024).collect();
    let text = streamed(Some("text/plain; charset=windows-1252"), &latin1, 64 * 1024);
    assert!(text.len() > 64 * 1024);
    assert_eq!(text.chars().count(), 64 * 1024);
}

/// Every page the HTML converter is checked on, with its `Content-Type`.
fn pages() -> Vec<(&'static str, Option<&'static str>, Vec<u8>)> {
    let meta = b"<meta charset=\"shift_jis\">";
    let early = [meta.as_slice(), b"<p>", SHIFT_JIS_A, b"</p>"].concat();
    let mut straddling = format!("<p>{}</p>", "x".repeat(1010 - 7)).into_bytes();
    assert_eq!(straddling.len(), 1010);
    straddling.extend_from_slice(meta);
    straddling.extend_from_slice(b"<p>");
    straddling.extend_from_slice(SHIFT_JIS_A);
    let mut padded_early = early.clone();
    padded_early.extend_from_slice(format!("<p>{}</p>", "y".repeat(2000)).as_bytes());
    let mut bom = b"\xef\xbb\xbf<p>caf\xc3\xa9</p>".to_vec();
    bom.extend_from_slice(meta);
    vec![
        ("a meta before byte 1024", None, padded_early),
        ("a meta across byte 1024", None, straddling),
        ("a short page with a meta", None, early.clone()),
        (
            "a header over a meta",
            Some("text/html; charset=windows-1252"),
            [early.as_slice(), b"<p>caf\xe9</p>"].concat(),
        ),
        (
            "a BOM over the header",
            Some("text/html; charset=shift_jis"),
            bom,
        ),
        ("long UTF-8", None, long_utf8("<p>")),
        (
            "long shift_jis",
            Some("text/html; charset=shift_jis"),
            long_shift_jis(b"<p>"),
        ),
        ("long UTF-16LE", None, long_utf16le("<p>")),
        ("an empty page", Some("text/html"), Vec::new()),
    ]
}

#[test]
fn html_converted_in_pieces_gives_the_whole_conversion() {
    for (name, content_type, bytes) in pages() {
        let whole = to_markdown(&decode(content_type, &bytes));
        for split in SPLITS {
            assert!(
                converted(content_type, &bytes, split) == whole,
                "{name}, split at {split}"
            );
        }
    }
}

#[test]
fn a_meta_across_byte_1024_is_not_taken() {
    let (_, _, bytes) = pages().swap_remove(1);
    let markdown = converted(None, &bytes, 1);
    assert!(!markdown.contains('あ'), "{markdown}");
    assert!(markdown.contains('�'), "{markdown}");
}

#[test]
fn a_meta_before_byte_1024_is_taken() {
    let (_, _, bytes) = pages().swap_remove(0);
    assert!(converted(None, &bytes, 1).contains('あ'));
}

/// Every `encoding_rs` encoding by its canonical label.
fn encodings() -> Vec<(&'static str, &'static encoding_rs::Encoding)> {
    let labels = [
        "utf-8",
        "ibm866",
        "iso-8859-2",
        "iso-8859-3",
        "iso-8859-4",
        "iso-8859-5",
        "iso-8859-6",
        "iso-8859-7",
        "iso-8859-8",
        "iso-8859-8-i",
        "iso-8859-10",
        "iso-8859-13",
        "iso-8859-14",
        "iso-8859-15",
        "iso-8859-16",
        "koi8-r",
        "koi8-u",
        "macintosh",
        "windows-874",
        "windows-1250",
        "windows-1251",
        "windows-1252",
        "windows-1253",
        "windows-1254",
        "windows-1255",
        "windows-1256",
        "windows-1257",
        "windows-1258",
        "x-mac-cyrillic",
        "gbk",
        "gb18030",
        "big5",
        "euc-jp",
        "iso-2022-jp",
        "shift_jis",
        "euc-kr",
        "utf-16be",
        "utf-16le",
        "x-user-defined",
        "replacement",
    ];
    labels
        .into_iter()
        .map(|label| {
            let encoding = encoding_rs::Encoding::for_label(label.as_bytes())
                .unwrap_or_else(|| panic!("{label} names an encoding"));
            (label, encoding)
        })
        .collect()
}

/// `bytes` decoded through [`Decoding`] in pieces of `split`: the text
/// handed over, joined.
fn decoded_total(encoding: &'static encoding_rs::Encoding, bytes: &[u8], split: usize) -> String {
    let mut decoding = Decoding::new(encoding);
    let mut text = String::new();
    let mut pieces = bytes.chunks(split.max(1)).peekable();
    if pieces.peek().is_none() {
        decoding.push(&[], true, &mut |part| text.push_str(part));
    } else {
        for piece in pieces {
            decoding.push(piece, false, &mut |part| text.push_str(part));
        }
        decoding.push(&[], true, &mut |part| text.push_str(part));
    }
    text
}

#[test]
fn every_encoding_expands_at_most_three_bytes_per_byte() {
    // Fixed patterns first, so every encoding is covered whatever the
    // property test below draws: all byte values, lone high bytes, and a
    // split at every small size and either side of the decoder's room.
    let mut pattern: Vec<u8> = (0..=255u8).cycle().take(8 * 1024).collect();
    pattern.extend_from_slice(&[0xe3, 0x81]);
    for (label, encoding) in encodings() {
        for split in [1, 2, 3, 7, 1023, 1024, 1025, 64 * 1024] {
            let text = decoded_total(encoding, &pattern, split);
            assert!(
                text.len() <= 3 * pattern.len(),
                "{label}, split at {split}: {} bytes for {}",
                text.len(),
                pattern.len()
            );
        }
    }
}

#[test]
fn random_bytes_expand_at_most_three_bytes_per_byte() {
    use proptest::prelude::*;
    let encodings = encodings();
    proptest!(|(choice in 0..encodings.len(), bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..2048), split in 1..70000usize)| {
        let (label, encoding) = encodings[choice];
        let text = decoded_total(encoding, &bytes, split);
        prop_assert!(
            text.len() <= 3 * bytes.len(),
            "{label}: {} bytes for {}",
            text.len(),
            bytes.len()
        );
    });
}
