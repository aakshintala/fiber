//! Writes the fixtures the probes use, deterministically, into argv[1].
use image::{ImageEncoder, RgbImage, codecs::jpeg::JpegEncoder, codecs::png::PngEncoder};
use std::{fs::File, io::BufWriter};

fn lcg(s: &mut u64) -> u8 { *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (*s >> 56) as u8 }

/// Smooth colour field with mild noise: JPEG-photo-like.
fn photo(w: u32, h: u32) -> RgbImage {
    let mut s = 7u64;
    RgbImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let n = (lcg(&mut s) % 24) as f32;
        let r = 128.0 + 100.0 * (fx * 9.0).sin() * (fy * 5.0).cos() + n;
        let g = 128.0 + 100.0 * (fy * 7.0 + fx * 3.0).sin() + n;
        let b = 128.0 + 100.0 * ((fx + fy) * 11.0).cos() + n;
        image::Rgb([r as u8, g as u8, b as u8])
    })
}

/// Flat blocks and stripes: screenshot-like, compresses well as PNG.
fn shot(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        let v = if (x / 40 + y / 20) % 7 == 0 { 20 } else if y % 24 < 2 { 90 } else { 245 };
        image::Rgb([v, v, (v / 2).saturating_add((x / 400) as u8 * 10)])
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let d = args.next().unwrap();
    if d == "try" {
        // gen try <file>...: does the image crate decode it, and to what.
        for f in args {
            let b = std::fs::read(&f).unwrap();
            match image::load_from_memory(&b) {
                Ok(i) => println!("{f}: ok {}x{} {:?}", i.width(), i.height(), i.color()),
                Err(e) => println!("{f}: error: {e}"),
            }
        }
        return;
    }
    let out = |n: &str| BufWriter::new(File::create(format!("{d}/{n}")).unwrap());
    let p = photo(4000, 3000);
    JpegEncoder::new_with_quality(out("photo-4000x3000.jpg"), 90).encode_image(&p).unwrap();
    let s = shot(4000, 3000);
    PngEncoder::new(out("shot-4000x3000.png")).write_image(s.as_raw(), 4000, 3000, image::ExtendedColorType::Rgb8).unwrap();
    // Small gif and webp (lossless) for the decode path.
    let small = photo(400, 300);
    small.save(format!("{d}/small.gif")).unwrap();
    small.save(format!("{d}/small.webp")).unwrap();
    let deep = image::ImageBuffer::<image::Rgb<u16>, _>::from_fn(300, 200, |x, y| image::Rgb([(x * 200) as u16, (y * 300) as u16, 40000]));
    deep.save(format!("{d}/deep16.png")).unwrap();
    // Highly compressible oversized images for the live probes.
    let big = shot(8000, 6000);
    PngEncoder::new(out("flat-8000x6000.png")).write_image(big.as_raw(), 8000, 6000, image::ExtendedColorType::Rgb8).unwrap();
    let huge = RgbImage::from_pixel(9000, 9000, image::Rgb([200, 30, 30]));
    PngEncoder::new(out("flat-9000x9000.png")).write_image(huge.as_raw(), 9000, 9000, image::ExtendedColorType::Rgb8).unwrap();
}
