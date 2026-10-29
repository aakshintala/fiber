//! Tests for the logo's Sixel encoder: exact bytes from the format
//! the encoder implements (P2 set, six-row bands, RLE over 3, 3-3-2),
//! and the palette and blend properties review checks.

use super::encode;
use crate::graphics::test_support::bounded_sixel_test;

/// Two red columns over six rows: one band, one register, no RLE.
#[test]
fn one_band_one_register_encodes_exactly() {
    bounded_sixel_test(
        "graphics::sixel::tests::one_band_one_register_encodes_exactly",
        one_band_one_register_encodes_exactly_body,
    );
}

fn one_band_one_register_encodes_exactly_body() {
    let rgba = [[255, 0, 0, 255]].repeat(2 * 6).concat();
    assert_eq!(
        encode(2, 6, &rgba, [0, 0, 0]),
        b"\x1bP0;1q\"1;1;2;6#0;2;100;0;0#0~~$-\x1b\\".as_slice()
    );
}

/// A run over 3 packs into `!count char`; exactly 3 stays literal.
#[test]
fn runs_over_three_pack_the_rest_stays_literal() {
    bounded_sixel_test(
        "graphics::sixel::tests::runs_over_three_pack_the_rest_stays_literal",
        runs_over_three_pack_the_rest_stays_literal_body,
    );
}

fn runs_over_three_pack_the_rest_stays_literal_body() {
    let rgba = [[255, 0, 0, 255]].repeat(8 * 6).concat();
    assert_eq!(
        encode(8, 6, &rgba, [0, 0, 0]),
        b"\x1bP0;1q\"1;1;8;6#0;2;100;0;0#0!8~$-\x1b\\".as_slice()
    );
    let rgba = [[255, 0, 0, 255]].repeat(3 * 6).concat();
    assert_eq!(
        encode(3, 6, &rgba, [0, 0, 0]),
        b"\x1bP0;1q\"1;1;3;6#0;2;100;0;0#0~~~$-\x1b\\".as_slice()
    );
    let rgba = [[255, 0, 0, 255]].repeat(4 * 6).concat();
    assert_eq!(
        encode(4, 6, &rgba, [0, 0, 0]),
        b"\x1bP0;1q\"1;1;4;6#0;2;100;0;0#0!4~$-\x1b\\".as_slice()
    );
}

/// Two registers: each color's raster follows its selector, `$`
/// returns the carriage between them, `-` ends the band.
#[test]
fn two_registers_switch_selectors_inside_the_band() {
    bounded_sixel_test(
        "graphics::sixel::tests::two_registers_switch_selectors_inside_the_band",
        two_registers_switch_selectors_inside_the_band_body,
    );
}

fn two_registers_switch_selectors_inside_the_band_body() {
    let mut rgba = Vec::new();
    for _ in 0..6 {
        rgba.extend_from_slice(&[255, 0, 0, 255].repeat(3));
        rgba.extend_from_slice(&[0, 0, 255, 255].repeat(3));
    }
    assert_eq!(
        encode(6, 6, &rgba, [0, 0, 0]),
        b"\x1bP0;1q\"1;1;6;6#0;2;100;0;0#1;2;0;0;100#0~~~???$#1???~~~$-\x1b\\".as_slice()
    );
}

/// Rows past six open a second band after `-`.
#[test]
fn seven_rows_open_a_second_band() {
    bounded_sixel_test(
        "graphics::sixel::tests::seven_rows_open_a_second_band",
        seven_rows_open_a_second_band_body,
    );
}

fn seven_rows_open_a_second_band_body() {
    let rgba = [[255, 0, 0, 255]].repeat(7).concat();
    assert_eq!(
        encode(1, 7, &rgba, [0, 0, 0]),
        b"\x1bP0;1q\"1;1;1;7#0;2;100;0;0#0~$-#0@$-\x1b\\".as_slice()
    );
}

/// Alpha under 32 reads as the background; 32 and up blends.
#[test]
fn alpha_under_32_is_unset_at_32_it_blends() {
    bounded_sixel_test(
        "graphics::sixel::tests::alpha_under_32_is_unset_at_32_it_blends",
        alpha_under_32_is_unset_at_32_it_blends_body,
    );
}

fn alpha_under_32_is_unset_at_32_it_blends_body() {
    assert_eq!(
        encode(1, 1, &[255, 0, 0, 31], [0, 0, 0]),
        b"\x1bP0;1q\"1;1;1;1#0;2;0;0;0#0@$-\x1b\\".as_slice()
    );
    assert_eq!(
        encode(1, 1, &[255, 0, 0, 32], [0, 0, 0]),
        b"\x1bP0;1q\"1;1;1;1#0;2;14;0;0#0@$-\x1b\\".as_slice()
    );
}

/// A transparent pixel takes the background exactly, which the
/// palette then names.
#[test]
fn the_background_fills_transparent_pixels() {
    bounded_sixel_test(
        "graphics::sixel::tests::the_background_fills_transparent_pixels",
        the_background_fills_transparent_pixels_body,
    );
}

fn the_background_fills_transparent_pixels_body() {
    assert_eq!(
        encode(1, 1, &[255, 0, 0, 0], [255, 255, 255]),
        b"\x1bP0;1q\"1;1;1;1#0;2;100;100;100#0@$-\x1b\\".as_slice()
    );
}

/// Half alpha blends each channel toward the background.
#[test]
fn half_alpha_blends_toward_the_background() {
    bounded_sixel_test(
        "graphics::sixel::tests::half_alpha_blends_toward_the_background",
        half_alpha_blends_toward_the_background_body,
    );
}

fn half_alpha_blends_toward_the_background_body() {
    // (200 * 128 + 10 * 127) / 255 = 105, (100 * 128 + 20 * 127) /
    // 255 = 60, (50 * 128 + 30 * 127) / 255 = 40.
    assert_eq!(
        encode(1, 1, &[200, 100, 50, 128], [10, 20, 30]),
        b"\x1bP0;1q\"1;1;1;1#0;2;43;14;0#0@$-\x1b\\".as_slice()
    );
}

/// The background's weight is its own term: (0 * 128 + 255 * 127) /
// 255 = 127, the middle of the red ramp.
#[test]
fn blend_weights_the_background_term() {
    bounded_sixel_test(
        "graphics::sixel::tests::blend_weights_the_background_term",
        blend_weights_the_background_term_body,
    );
}

fn blend_weights_the_background_term_body() {
    assert_eq!(
        encode(1, 1, &[0, 0, 0, 128], [255, 0, 0]),
        b"\x1bP0;1q\"1;1;1;1#0;2;43;0;0#0@$-\x1b\\".as_slice()
    );
}

/// The background's weight is 255 minus alpha: (0 * 32 + 255 * 223)
// 255 = 223.
#[test]
fn blend_uses_255_minus_alpha() {
    bounded_sixel_test(
        "graphics::sixel::tests::blend_uses_255_minus_alpha",
        blend_uses_255_minus_alpha_body,
    );
}

fn blend_uses_255_minus_alpha_body() {
    assert_eq!(
        encode(1, 1, &[0, 0, 0, 32], [255, 0, 0]),
        b"\x1bP0;1q\"1;1;1;1#0;2;86;0;0#0@$-\x1b\\".as_slice()
    );
}

/// Two columns over one band: each register's raster follows its own
/// selector, so a `/` in the row walk reads the wrong colour.
#[test]
fn two_columns_pin_the_band_walk_to_row_times_width() {
    bounded_sixel_test(
        "graphics::sixel::tests::two_columns_pin_the_band_walk_to_row_times_width",
        two_columns_pin_the_band_walk_to_row_times_width_body,
    );
}

fn two_columns_pin_the_band_walk_to_row_times_width_body() {
    let mut rgba = Vec::new();
    for _ in 0..6 {
        rgba.extend_from_slice(&[255, 0, 0, 255]);
        rgba.extend_from_slice(&[0, 0, 255, 255]);
    }
    assert_eq!(
        encode(2, 6, &rgba, [0, 0, 0]),
        b"\x1bP0;1q\"1;1;2;6#0;2;100;0;0#1;2;0;0;100#0~?$#1?~$-\x1b\\".as_slice()
    );
}

/// Thirteen rows open three bands, including a one-row tail band.
#[test]
fn thirteen_rows_open_three_bands() {
    bounded_sixel_test(
        "graphics::sixel::tests::thirteen_rows_open_three_bands",
        thirteen_rows_open_three_bands_body,
    );
}

fn thirteen_rows_open_three_bands_body() {
    let rgba = [[255, 0, 0, 255]].repeat(13).concat();
    assert_eq!(
        encode(1, 13, &rgba, [0, 0, 0]),
        b"\x1bP0;1q\"1;1;1;13#0;2;100;0;0#0~$-#0~$-#0@$-\x1b\\".as_slice()
    );
}

/// 300 distinct colors still fit the 256 registers 3-3-2 allows.
#[test]
fn the_palette_never_passes_256_registers() {
    bounded_sixel_test(
        "graphics::sixel::tests::the_palette_never_passes_256_registers",
        the_palette_never_passes_256_registers_body,
    );
}

fn the_palette_never_passes_256_registers_body() {
    let mut rgba = Vec::new();
    for at in 0..300u16 {
        let bucket = u8::try_from(at % 256).unwrap_or(u8::MAX);
        let red = (bucket >> 5) * 36;
        let green = ((bucket >> 2) & 0x07) * 36;
        let blue = (bucket & 0x03) * 85;
        rgba.extend_from_slice(&[red, green, blue, 255]);
    }
    let sixel = encode(300, 1, &rgba, [0, 0, 0]);
    let text = String::from_utf8(sixel).expect("sixel is ASCII");
    assert_eq!(text.matches(";2;").count(), 256);
}
