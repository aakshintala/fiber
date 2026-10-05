//! The image rules of `docs/model-routing.md`, "Image limits": refuse over 50
//! megapixels from the header alone, store a PNG, JPEG or WebP within the
//! caps byte for byte, and otherwise fit it.

use std::io::Cursor;

use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{DynamicImage, ImageBuffer, ImageEncoder, ImageFormat, ImageReader};

/// The most pixels an image may have: width times height, inclusive.
pub(crate) const MAX_PIXELS: u64 = 50_000_000;
/// The longest side a stored image may have, in pixels, inclusive.
pub(crate) const MAX_SIDE: u32 = 2000;
/// The most bytes a stored image may take as base64, inclusive.
pub(crate) const MAX_BASE64: u64 = 1_048_576;
/// The JPEG quality every re-encode uses.
const JPEG_QUALITY: u8 = 80;

/// The file the child stores.
#[derive(Debug)]
pub(crate) struct Stored {
    pub(crate) bytes: Vec<u8>,
    /// The file extension: `png`, `jpg` or `webp`.
    pub(crate) extension: &'static str,
    pub(crate) mime_type: &'static str,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Which format a fit re-encodes into first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Primary {
    Png,
    Jpeg,
}

/// The base64 length of `bytes` bytes: four characters per three bytes,
/// rounded up.
pub(crate) fn base64_len(bytes: u64) -> u64 {
    bytes.div_ceil(3).saturating_mul(4)
}

/// The length of `bytes` as a `u64`.
fn length(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

/// Processes one image file's bytes, or says why it is refused.
pub(crate) fn process(input: &[u8]) -> Result<Stored, String> {
    let format = image::guess_format(input).map_err(|error| error.to_string())?;
    let supported = [
        ImageFormat::Jpeg,
        ImageFormat::Png,
        ImageFormat::Gif,
        ImageFormat::WebP,
    ];
    if !supported.contains(&format) {
        return Err(format!("{format:?} is not a supported image format"));
    }
    let primary = if format == ImageFormat::Jpeg {
        Primary::Jpeg
    } else {
        Primary::Png
    };
    let (width, height) = ImageReader::with_format(Cursor::new(input), format)
        .into_dimensions()
        .map_err(|error| error.to_string())?;
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(format!(
            "{width}x{height} is {pixels} pixels; the limit is {MAX_PIXELS}"
        ));
    }
    if format != ImageFormat::Gif && within_caps(width, height, input) {
        return Ok(Stored {
            bytes: input.to_vec(),
            extension: extension(format),
            mime_type: mime_type(format),
            width,
            height,
        });
    }
    let decoded =
        image::load_from_memory_with_format(input, format).map_err(|error| error.to_string())?;
    let decoded = normalised(&decoded);
    if format == ImageFormat::Gif {
        // A GIF always becomes PNG; that PNG is stored as encoded when it is
        // within the caps and fitted otherwise.
        let png = encode_png(&decoded)?;
        if within_caps(decoded.width(), decoded.height(), &png) {
            return Ok(stored(png, Primary::Png, &decoded));
        }
    }
    fit(&decoded, primary)
}

fn within_caps(width: u32, height: u32, bytes: &[u8]) -> bool {
    width.max(height) <= MAX_SIDE && base64_len(length(bytes)) <= MAX_BASE64
}

fn extension(format: ImageFormat) -> &'static str {
    if format == ImageFormat::Jpeg {
        "jpg"
    } else if format == ImageFormat::WebP {
        "webp"
    } else {
        "png"
    }
}

fn mime_type(format: ImageFormat) -> &'static str {
    if format == ImageFormat::Jpeg {
        "image/jpeg"
    } else if format == ImageFormat::WebP {
        "image/webp"
    } else {
        "image/png"
    }
}

fn stored(bytes: Vec<u8>, kind: Primary, image: &DynamicImage) -> Stored {
    let (extension, mime_type) = match kind {
        Primary::Png => ("png", "image/png"),
        Primary::Jpeg => ("jpg", "image/jpeg"),
    };
    Stored {
        bytes,
        extension,
        mime_type,
        width: image.width(),
        height: image.height(),
    }
}

/// The image in the three layouts a fit handles: grey, RGB and RGBA, 8 bits
/// a channel. Alpha is kept; 16-bit and float images lose precision.
fn normalised(image: &DynamicImage) -> DynamicImage {
    let color = image.color();
    if matches!(color, image::ColorType::L8 | image::ColorType::L16) {
        DynamicImage::ImageLuma8(image.to_luma8())
    } else if color.has_alpha() {
        DynamicImage::ImageRgba8(image.to_rgba8())
    } else {
        DynamicImage::ImageRgb8(image.to_rgb8())
    }
}

/// Fits `image` into the caps: the first attempt is at the longest side's
/// own size or 2000, whichever is smaller; each failure cuts the longest side
/// to three quarters, until an attempt fits or the longest side is 1 px, when
/// the last attempt is stored whatever its size.
fn fit(image: &DynamicImage, primary: Primary) -> Result<Stored, String> {
    let mut side = image.width().max(image.height()).min(MAX_SIDE);
    loop {
        let candidate = resized(image, side)?;
        let first = encode(&candidate, primary)?;
        let fits = |bytes: &[u8]| base64_len(length(bytes)) <= MAX_BASE64;
        if fits(&first) || side == 1 {
            return Ok(stored(first, primary, &candidate));
        }
        if primary == Primary::Png {
            let jpeg = encode_jpeg(&candidate)?;
            if fits(&jpeg) {
                return Ok(stored(jpeg, Primary::Jpeg, &candidate));
            }
        }
        side = (side * 3 / 4).max(1);
    }
}

/// `image` with its longest side at `side` px, keeping the aspect ratio and
/// never enlarging; the shorter side is rounded and at least 1.
fn resized(image: &DynamicImage, side: u32) -> Result<DynamicImage, String> {
    let (width, height) = (image.width(), image.height());
    let longest = width.max(height);
    if side >= longest {
        return Ok(image.clone());
    }
    let scale = |length: u32| {
        let scaled = (u64::from(length) * u64::from(side) * 2 + u64::from(longest))
            / (2 * u64::from(longest));
        u32::try_from(scaled).unwrap_or(u32::MAX).max(1)
    };
    let (new_width, new_height) = if width >= height {
        (side, scale(height))
    } else {
        (scale(width), side)
    };
    let mut target = empty_like(image, new_width, new_height);
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    Resizer::new()
        .resize(image, &mut target, &options)
        .map_err(|error| error.to_string())?;
    Ok(target)
}

/// A blank image of the same layout as `like`.
fn empty_like(like: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    // `normalised` leaves only these three layouts.
    if matches!(like, DynamicImage::ImageLuma8(_)) {
        DynamicImage::ImageLuma8(ImageBuffer::new(width, height))
    } else if matches!(like, DynamicImage::ImageRgba8(_)) {
        DynamicImage::ImageRgba8(ImageBuffer::new(width, height))
    } else {
        DynamicImage::ImageRgb8(ImageBuffer::new(width, height))
    }
}

fn encode(image: &DynamicImage, primary: Primary) -> Result<Vec<u8>, String> {
    match primary {
        Primary::Png => encode_png(image),
        Primary::Jpeg => encode_jpeg(image),
    }
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            image.as_bytes(),
            image.width(),
            image.height(),
            image.color().into(),
        )
        .map_err(|error| error.to_string())?;
    Ok(out)
}

/// JPEG at quality 80; it has no alpha, so the image is flattened to RGB.
fn encode_jpeg(image: &DynamicImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode_image(&DynamicImage::ImageRgb8(image.to_rgb8()))
        .map_err(|error| error.to_string())?;
    Ok(out)
}

#[cfg(test)]
#[path = "fit_tests.rs"]
mod tests;
