//! Tests beside [`super::process`]: the caps, the fit loop and the formats.

use image::codecs::gif::{GifEncoder, Repeat};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{DynamicImage, Frame, ImageBuffer, ImageEncoder, ImageFormat, Rgb, RgbImage, Rgba};

use super::{MAX_BASE64, Stored, base64_len, next_side, process};

/// Deterministic noise: a linear congruential generator, so a fixture never
/// changes between runs.
fn noise(width: u32, height: u32) -> RgbImage {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    ImageBuffer::from_fn(width, height, |_, _| {
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            u8::try_from(state >> 56).unwrap()
        };
        Rgb([next(), next(), next()])
    })
}

fn flat(width: u32, height: u32) -> RgbImage {
    ImageBuffer::from_pixel(width, height, Rgb([10, 120, 230]))
}

fn png_of(image: &DynamicImage) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            image.as_bytes(),
            image.width(),
            image.height(),
            image.color().into(),
        )
        .unwrap();
    out
}

fn jpeg_of(image: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 90)
        .encode_image(image)
        .unwrap();
    out
}

fn webp_of(image: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

fn decoded(stored: &Stored, format: ImageFormat) -> DynamicImage {
    image::load_from_memory_with_format(&stored.bytes, format).unwrap()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
    out
}

/// A PNG that holds a signature, an IHDR for `width` x `height` and the start
/// of an IDAT, and no pixels: only a header read can say anything about it.
fn header_only_png(width: u32, height: u32) -> Vec<u8> {
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend(chunk(b"IHDR", &ihdr));
    out.extend(chunk(b"IDAT", &[0x78, 0x9C, 0x00]));
    out
}

/// `bytes` with zero padding after the image, so the file is `size` bytes.
/// Decoders stop at the end of the image and ignore what follows.
fn padded(mut bytes: Vec<u8>, size: usize) -> Vec<u8> {
    assert!(bytes.len() <= size);
    bytes.resize(size, 0);
    bytes
}

fn fits(stored: &Stored) -> bool {
    base64_len(u64::try_from(stored.bytes.len()).unwrap()) <= MAX_BASE64
}

#[test]
fn base64_len_rounds_up_to_four_per_three() {
    assert_eq!(base64_len(0), 0);
    assert_eq!(base64_len(1), 4);
    assert_eq!(base64_len(3), 4);
    assert_eq!(base64_len(4), 8);
    assert_eq!(base64_len(786_432), MAX_BASE64);
    assert_eq!(base64_len(786_433), MAX_BASE64 + 4);
}

#[test]
fn an_image_over_50_megapixels_is_refused_from_its_header() {
    let error = process(&header_only_png(8000, 7000)).unwrap_err();
    assert_eq!(error, "8000x7000 is 56000000 pixels; the limit is 50000000");
}

#[test]
fn exactly_50_megapixels_is_not_refused() {
    // The header-only file cannot decode, so the error is a decoder's.
    let error = process(&header_only_png(10_000, 5000)).unwrap_err();
    assert!(!error.contains("the limit is"), "{error}");
    let error = process(&header_only_png(10_000, 5001)).unwrap_err();
    assert_eq!(
        error,
        "10000x5001 is 50010000 pixels; the limit is 50000000"
    );
}

#[test]
fn a_within_cap_png_is_stored_byte_for_byte() {
    let input = png_of(&DynamicImage::ImageRgb8(flat(300, 200)));
    let stored = process(&input).unwrap();
    assert_eq!(stored.bytes, input);
    assert_eq!(
        (
            stored.extension,
            stored.mime_type,
            stored.width,
            stored.height
        ),
        ("png", "image/png", 300, 200)
    );
}

#[test]
fn a_within_cap_jpeg_is_stored_byte_for_byte() {
    let input = jpeg_of(&flat(64, 48));
    let stored = process(&input).unwrap();
    assert_eq!(stored.bytes, input);
    assert_eq!(
        (
            stored.extension,
            stored.mime_type,
            stored.width,
            stored.height
        ),
        ("jpg", "image/jpeg", 64, 48)
    );
}

#[test]
fn a_within_cap_webp_is_stored_byte_for_byte() {
    let input = webp_of(&flat(40, 30));
    let stored = process(&input).unwrap();
    assert_eq!(stored.bytes, input);
    assert_eq!(
        (
            stored.extension,
            stored.mime_type,
            stored.width,
            stored.height
        ),
        ("webp", "image/webp", 40, 30)
    );
}

#[test]
fn an_image_over_2000_px_is_fitted_keeping_the_aspect_ratio() {
    let stored = process(&png_of(&DynamicImage::ImageRgb8(flat(4000, 3001)))).unwrap();
    // 3001 * 2000 / 4000 = 1500.5, which rounds up.
    assert_eq!((stored.width, stored.height), (2000, 1501));
    assert_eq!(stored.extension, "png");
    let again = decoded(&stored, ImageFormat::Png);
    assert_eq!((again.width(), again.height()), (2000, 1501));
}

#[test]
fn a_tall_image_is_fitted_by_its_height() {
    let stored = process(&png_of(&DynamicImage::ImageRgb8(flat(1000, 4000)))).unwrap();
    assert_eq!((stored.width, stored.height), (500, 2000));
}

#[test]
fn the_shorter_side_never_rounds_to_zero() {
    let stored = process(&png_of(&DynamicImage::ImageRgb8(flat(4000, 1)))).unwrap();
    assert_eq!((stored.width, stored.height), (2000, 1));
}

#[test]
fn a_longest_side_of_2000_passes_and_2001_is_fitted() {
    let input = png_of(&DynamicImage::ImageRgb8(flat(2000, 2)));
    assert_eq!(process(&input).unwrap().bytes, input);
    let input = png_of(&DynamicImage::ImageRgb8(flat(2001, 2)));
    let stored = process(&input).unwrap();
    assert_eq!((stored.width, stored.height), (2000, 2));
    assert_ne!(stored.bytes, input);
}

#[test]
fn a_height_of_2001_is_fitted_too() {
    let stored = process(&png_of(&DynamicImage::ImageRgb8(flat(2, 2001)))).unwrap();
    assert_eq!((stored.width, stored.height), (2, 2000));
}

#[test]
fn a_file_at_exactly_one_mebibyte_of_base64_passes_and_one_byte_more_does_not() {
    let base = png_of(&DynamicImage::ImageRgb8(flat(50, 50)));
    let at_cap = padded(base.clone(), 786_432);
    assert_eq!(process(&at_cap).unwrap().bytes, at_cap);
    let over = padded(base, 786_433);
    let stored = process(&over).unwrap();
    assert_ne!(stored.bytes, over);
    assert_eq!((stored.width, stored.height), (50, 50));
    assert!(fits(&stored));
}

#[test]
fn a_noisy_png_over_the_size_cap_falls_to_jpeg() {
    let input = png_of(&DynamicImage::ImageRgb8(noise(600, 600)));
    assert!(base64_len(u64::try_from(input.len()).unwrap()) > MAX_BASE64);
    let stored = process(&input).unwrap();
    assert_eq!(stored.extension, "jpg");
    assert_eq!(stored.mime_type, "image/jpeg");
    assert_eq!((stored.width, stored.height), (600, 600));
    assert!(fits(&stored));
    assert_eq!(decoded(&stored, ImageFormat::Jpeg).width(), 600);
}

#[test]
fn an_image_that_fits_neither_format_shrinks_by_three_quarters() {
    let stored = process(&jpeg_of(&noise(1400, 1400))).unwrap();
    assert!(fits(&stored));
    // The side steps 1400, 1050, 787, ...: each is three quarters of the last.
    let steps: Vec<u32> = std::iter::successors(Some(1400), |&side| Some(next_side(side)))
        .take(64)
        .collect();
    assert!(steps.contains(&stored.width), "{}", stored.width);
    // Two cuts: 1400 and 1050 do not fit, 787 does.
    assert_eq!((stored.width, stored.height), (787, 787));
    assert!(stored.width < 1400);
    assert_eq!(stored.width, stored.height);
    assert_eq!(stored.extension, "jpg");
}

#[test]
fn an_image_that_fits_after_one_cut_is_stored_at_three_quarters() {
    // 1000 does not fit; one cut, to 750, does.
    let stored = process(&jpeg_of(&noise(1000, 1000))).unwrap();
    assert!(fits(&stored));
    assert_eq!((stored.width, stored.height), (750, 750));
}

#[test]
fn a_jpeg_over_the_size_cap_in_range_is_reencoded_at_its_own_size() {
    // 800x800 noise at quality 90 is about 1.5 MB: over the cap, in range.
    let input = jpeg_of(&noise(800, 800));
    assert!(base64_len(u64::try_from(input.len()).unwrap()) > MAX_BASE64);
    let stored = process(&input).unwrap();
    assert_eq!(stored.extension, "jpg");
    assert!(fits(&stored));
    assert!(stored.width <= 800);
}

#[test]
fn a_fit_never_enlarges() {
    // 100x100 noise as a padded PNG over the size cap forces a re-encode.
    let input = padded(png_of(&DynamicImage::ImageRgb8(noise(100, 100))), 900_000);
    let stored = process(&input).unwrap();
    assert_eq!((stored.width, stored.height), (100, 100));
    assert_ne!(stored.bytes, input);
}

#[test]
fn a_png_keeps_its_alpha_when_fitted() {
    let rgba: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(2400, 100, Rgba([200, 10, 10, 100]));
    let stored = process(&png_of(&DynamicImage::ImageRgba8(rgba))).unwrap();
    assert_eq!((stored.width, stored.height), (2000, 83));
    let again = decoded(&stored, ImageFormat::Png);
    assert!(again.color().has_alpha());
    let pixel = again.to_rgba8().get_pixel(5, 5).0;
    assert!(pixel[3].abs_diff(100) <= 1, "{pixel:?}");
}

#[test]
fn a_grey_png_stays_grey_when_fitted() {
    let grey: ImageBuffer<image::Luma<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(2500, 10, image::Luma([77]));
    let stored = process(&png_of(&DynamicImage::ImageLuma8(grey))).unwrap();
    assert_eq!(
        decoded(&stored, ImageFormat::Png).color(),
        image::ColorType::L8
    );
}

fn gif_of_two_frames() -> Vec<u8> {
    let first: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(20, 10, Rgba([255, 0, 0, 255]));
    let second: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(20, 10, Rgba([0, 0, 255, 255]));
    let mut out = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut out);
        encoder.set_repeat(Repeat::Infinite).unwrap();
        encoder.encode_frame(Frame::new(first)).unwrap();
        encoder.encode_frame(Frame::new(second)).unwrap();
    }
    out
}

#[test]
fn a_gif_becomes_a_png_of_its_first_frame() {
    let stored = process(&gif_of_two_frames()).unwrap();
    assert_eq!(
        (
            stored.extension,
            stored.mime_type,
            stored.width,
            stored.height
        ),
        ("png", "image/png", 20, 10)
    );
    let pixel = decoded(&stored, ImageFormat::Png)
        .to_rgba8()
        .get_pixel(3, 3)
        .0;
    assert_eq!(pixel, [255, 0, 0, 255]);
}

#[test]
fn a_gif_over_2000_px_is_fitted_as_a_png() {
    let big: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(2400, 1200, Rgba([0, 255, 0, 255]));
    let mut out = Vec::new();
    GifEncoder::new(&mut out)
        .encode_frame(Frame::new(big))
        .unwrap();
    let stored = process(&out).unwrap();
    assert_eq!((stored.width, stored.height), (2000, 1000));
    assert_eq!(stored.extension, "png");
}

#[test]
fn a_cmyk_jpeg_is_accepted_and_stored_within_the_caps() {
    let input = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/cmyk.jpg"
    ))
    .unwrap();
    let stored = process(&input).unwrap();
    assert_eq!(stored.bytes, input);
    assert_eq!(stored.extension, "jpg");
}

#[test]
fn a_cmyk_jpeg_over_the_size_cap_is_decoded_and_fitted() {
    let input = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/cmyk.jpg"
    ))
    .unwrap();
    let forced = padded(input, 900_000);
    let stored = process(&forced).unwrap();
    assert_ne!(stored.bytes, forced);
    assert_eq!(stored.extension, "jpg");
    assert!(decoded(&stored, ImageFormat::Jpeg).width() > 0);
}

#[test]
fn bytes_that_are_not_an_image_are_refused_with_the_decoders_message() {
    let error = process(b"not an image at all").unwrap_err();
    assert!(!error.is_empty());
    let error = process(b"BM\0\0\0\0\0\0\0\0\0\0\0\0\0\0").unwrap_err();
    assert!(!error.is_empty());
}

#[test]
fn a_truncated_png_over_the_caps_is_refused_with_a_decoder_message() {
    let mut input = png_of(&DynamicImage::ImageRgb8(noise(2400, 20)));
    input.truncate(input.len() / 2);
    assert!(process(&input).is_err());
}

#[test]
fn a_truncated_png_within_the_caps_is_stored_as_it_is() {
    // Within the caps a file is stored byte for byte and never decoded.
    let mut input = png_of(&DynamicImage::ImageRgb8(noise(100, 100)));
    input.truncate(input.len() / 2);
    assert_eq!(process(&input).unwrap().bytes, input);
}

#[test]
fn a_failed_attempt_cuts_the_side_to_three_quarters_rounded_down() {
    assert_eq!(next_side(2000), 1500);
    assert_eq!(next_side(1500), 1125);
    assert_eq!(next_side(1125), 843);
    assert_eq!(next_side(5), 3);
    assert_eq!(next_side(1), 1);
}

#[test]
fn the_sides_a_fit_tries_reach_one_pixel_in_under_forty_attempts() {
    // Bounded, so a `next_side` that never shrinks fails instead of hanging.
    let sides: Vec<u32> = std::iter::successors(Some(2000), |&side| Some(next_side(side)))
        .take(64)
        .collect();
    let attempts = sides.iter().position(|&side| side == 1).map(|at| at + 1);
    assert!(
        attempts.is_some_and(|n| n < super::MAX_ATTEMPTS),
        "{attempts:?}"
    );
}
