//! In-process timings and child-process spawn probes for image workloads.
//! Enable with `IMAGE_TIMING=1` (requires `image` or `image-fir`). Child modes:
//! `IMAGE_CHILD=noop`, `IMAGE_CHILD=fit <path>`. Parent spawn bench: `IMAGE_SPAWN_BENCH=1`.
//! Photograph PNG cost table: `IMAGE_PNG_PHOTOS=1` with `IMAGE_PHOTO_MANIFEST` (requires `image-fir`).

use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use image::{
    DynamicImage, GenericImageView, ImageBuffer, ImageEncoder, ImageFormat, ImageReader,
    codecs::{
        jpeg::JpegEncoder,
        png::{CompressionType, FilterType as PngFilterType, PngEncoder},
    },
    imageops::FilterType,
};

const FIXTURES: &[&str] = &[
    "photo-4000x3000.jpg",
    "shot-4000x3000.png",
    "flat-9000x9000.png",
    "small.gif",
    "small.webp",
];

fn fixture_dir() -> String {
    std::env::var("IMAGE_FIXTURES").unwrap_or_else(|_| "../image-limits/fixtures".into())
}

fn fit_dims(w: u32, h: u32) -> (u32, u32) {
    if w > 2000 || h > 2000 {
        let s = (2000.0 / w as f64).min(2000.0 / h as f64);
        (
            ((w as f64 * s).round() as u32).max(1),
            ((h as f64 * s).round() as u32).max(1),
        )
    } else {
        (w, h)
    }
}

fn empty_like(w: u32, h: u32, src: &DynamicImage) -> DynamicImage {
    match src {
        DynamicImage::ImageLuma8(_) => DynamicImage::ImageLuma8(ImageBuffer::new(w, h)),
        DynamicImage::ImageLumaA8(_) => DynamicImage::ImageLumaA8(ImageBuffer::new(w, h)),
        DynamicImage::ImageRgb8(_) => DynamicImage::ImageRgb8(ImageBuffer::new(w, h)),
        DynamicImage::ImageRgba8(_) => DynamicImage::ImageRgba8(ImageBuffer::new(w, h)),
        DynamicImage::ImageLuma16(_) => DynamicImage::ImageLuma16(ImageBuffer::new(w, h)),
        DynamicImage::ImageLumaA16(_) => DynamicImage::ImageLumaA16(ImageBuffer::new(w, h)),
        DynamicImage::ImageRgb16(_) => DynamicImage::ImageRgb16(ImageBuffer::new(w, h)),
        DynamicImage::ImageRgba16(_) => DynamicImage::ImageRgba16(ImageBuffer::new(w, h)),
        DynamicImage::ImageRgb32F(_) => DynamicImage::ImageRgb32F(ImageBuffer::new(w, h)),
        DynamicImage::ImageRgba32F(_) => DynamicImage::ImageRgba32F(ImageBuffer::new(w, h)),
        _ => panic!("unsupported pixel layout"),
    }
}

fn resize_imageops(img: DynamicImage, nw: u32, nh: u32) -> DynamicImage {
    if (nw, nh) == img.dimensions() {
        img
    } else {
        img.resize(nw, nh, FilterType::Lanczos3)
    }
}

#[cfg(feature = "image-fir")]
fn resize_fir(img: DynamicImage, nw: u32, nh: u32) -> DynamicImage {
    use fast_image_resize::{FilterType as FirFilter, ResizeAlg, ResizeOptions, Resizer};
    if (nw, nh) == img.dimensions() {
        return img;
    }
    let mut dst = empty_like(nw, nh, &img);
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FirFilter::Lanczos3));
    Resizer::new().resize(&img, &mut dst, &opts).unwrap();
    dst
}

fn encode_primary(img: &DynamicImage, input: ImageFormat) -> Vec<u8> {
    let mut out = Vec::new();
    match input {
        ImageFormat::Jpeg => {
            JpegEncoder::new_with_quality(&mut out, 80)
                .encode_image(&DynamicImage::ImageRgb8(img.to_rgb8()))
                .unwrap();
        }
        _ => PngEncoder::new(&mut out)
            .write_image(img.as_bytes(), img.width(), img.height(), img.color().into())
            .unwrap(),
    }
    out
}

fn encode_jpeg_q80(img: &DynamicImage) -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 80)
        .encode_image(&DynamicImage::ImageRgb8(img.to_rgb8()))
        .unwrap();
    out
}

fn b64_len(bytes: usize) -> usize {
    (bytes + 2) / 3 * 4
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn median_only(samples: &mut [f64]) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    if n % 2 == 0 {
        (samples[n / 2 - 1] + samples[n / 2]) / 2.0
    } else {
        samples[n / 2]
    }
}

fn median_p90(samples: &mut [f64]) -> (f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    let med = if n % 2 == 0 {
        (samples[n / 2 - 1] + samples[n / 2]) / 2.0
    } else {
        samples[n / 2]
    };
    let p90_idx = ((n as f64) * 0.9).ceil() as usize - 1;
    let p90 = samples[p90_idx.min(n - 1)];
    (med, p90)
}

#[derive(Copy, Clone)]
enum Backend {
    #[cfg(feature = "image")]
    Image,
    #[cfg(feature = "image-fir")]
    Fir,
}

fn resize_backend(img: DynamicImage, nw: u32, nh: u32, backend: Backend) -> DynamicImage {
    match backend {
        #[cfg(feature = "image")]
        Backend::Image => resize_imageops(img, nw, nh),
        #[cfg(feature = "image-fir")]
        Backend::Fir => resize_fir(img, nw, nh),
    }
}

pub fn child_fit(path: &Path) {
    let bytes = std::fs::read(path).unwrap();
    let backend = backend_for_build();
    let format = image::guess_format(&bytes).unwrap();
    let img = image::load_from_memory(&bytes).unwrap();
    let (w, h) = img.dimensions();
    let (nw, nh) = fit_dims(w, h);
    let img = resize_backend(img, nw, nh, backend);
    let out = encode_primary(&img, format);
    let tmp = std::env::temp_dir().join(format!("dep-rss-fit-{}.out", std::process::id()));
    std::fs::write(&tmp, &out).unwrap();
    black_box(tmp);
}

pub fn spawn_bench() {
    let exe = std::env::current_exe().unwrap();
    let dir = fixture_dir();
    let fit_path = format!("{dir}/photo-4000x3000.jpg");
    println!("spawn_bench exe={}", exe.display());
    for label in ["noop", "fit"] {
        let mut samples = Vec::with_capacity(50);
        for _ in 0..50 {
            let mut cmd = Command::new(&exe);
            cmd.env("IMAGE_CHILD", label);
            if label == "fit" {
                cmd.arg(&fit_path);
            }
            let t0 = Instant::now();
            let status = cmd.status().unwrap();
            assert!(status.success(), "child {label} failed");
            samples.push(ms(t0.elapsed()));
        }
        let (med, p90) = median_p90(&mut samples);
        println!("spawn_{label} median_ms={med:.3} p90_ms={p90:.3}");
    }
}

fn bench_fixture(name: &str, backend: Backend, backend_label: &str) {
    let path = format!("{}/{}", fixture_dir(), name);
    let bytes = std::fs::read(&path).unwrap();
    let input_fmt = image::guess_format(&bytes).unwrap();

    let mut header = Vec::with_capacity(20);
    let mut decode = Vec::with_capacity(20);
    let mut resize = Vec::with_capacity(20);
    let mut encode = Vec::with_capacity(20);
    let mut jpeg_fb = Vec::with_capacity(20);

    for _ in 0..2 {
        let _ = ImageReader::open(&path).unwrap().with_guessed_format().unwrap().into_dimensions();
        let img = image::load_from_memory(&bytes).unwrap();
        let (w, h) = img.dimensions();
        let (nw, nh) = fit_dims(w, h);
        let img = resize_backend(img, nw, nh, backend);
        black_box(encode_primary(&img, input_fmt));
        black_box(encode_jpeg_q80(&img));
    }

    let mut primary_bytes = 0usize;
    let mut jpeg_fb_bytes = 0usize;

    for _ in 0..20 {
        let t0 = Instant::now();
        let _ = ImageReader::open(&path).unwrap().with_guessed_format().unwrap().into_dimensions();
        header.push(ms(t0.elapsed()));

        let t0 = Instant::now();
        let img = image::load_from_memory(&bytes).unwrap();
        decode.push(ms(t0.elapsed()));

        let (w, h) = img.dimensions();
        let (nw, nh) = fit_dims(w, h);
        let t0 = Instant::now();
        let img = resize_backend(img, nw, nh, backend);
        resize.push(ms(t0.elapsed()));

        let t0 = Instant::now();
        let out = encode_primary(&img, input_fmt);
        encode.push(ms(t0.elapsed()));
        primary_bytes = out.len();

        let t0 = Instant::now();
        let jout = encode_jpeg_q80(&img);
        jpeg_fb.push(ms(t0.elapsed()));
        jpeg_fb_bytes = jout.len();
    }

    let (h_med, h_p90) = median_p90(&mut header);
    let (d_med, d_p90) = median_p90(&mut decode);
    let (r_med, r_p90) = median_p90(&mut resize);
    let (e_med, e_p90) = median_p90(&mut encode);
    let (j_med, j_p90) = median_p90(&mut jpeg_fb);

    println!(
        "fixture={name} backend={backend_label} \
header_median_ms={h_med:.3} header_p90_ms={h_p90:.3} \
decode_median_ms={d_med:.3} decode_p90_ms={d_p90:.3} \
resize_median_ms={r_med:.3} resize_p90_ms={r_p90:.3} \
encode_median_ms={e_med:.3} encode_p90_ms={e_p90:.3} \
jpeg_fallback_median_ms={j_med:.3} jpeg_fallback_p90_ms={j_p90:.3} \
primary_bytes={primary_bytes} primary_b64={} \
jpeg_fallback_bytes={jpeg_fb_bytes} jpeg_fallback_b64={}",
        b64_len(primary_bytes),
        b64_len(jpeg_fb_bytes),
    );
}

fn bench_png_compression(name: &str, backend: Backend, backend_label: &str) {
    if name != "shot-4000x3000.png" {
        return;
    }
    let bytes = std::fs::read(format!("{}/{}", fixture_dir(), name)).unwrap();
    let img = image::load_from_memory(&bytes).unwrap();
    let (w, h) = img.dimensions();
    let (nw, nh) = fit_dims(w, h);
    let img = resize_backend(img, nw, nh, backend);

    for (label, compression) in [
        ("fast", CompressionType::Fast),
        ("default", CompressionType::Default),
        ("best", CompressionType::Best),
    ] {
        let mut times = Vec::with_capacity(20);
        let mut out_len = 0usize;
        for _ in 0..2 {
            let mut out = Vec::new();
            PngEncoder::new_with_quality(&mut out, compression, PngFilterType::Adaptive)
                .write_image(img.as_bytes(), img.width(), img.height(), img.color().into())
                .unwrap();
            black_box(out);
        }
        for _ in 0..20 {
            let t0 = Instant::now();
            let mut out = Vec::new();
            PngEncoder::new_with_quality(&mut out, compression, PngFilterType::Adaptive)
                .write_image(img.as_bytes(), img.width(), img.height(), img.color().into())
                .unwrap();
            times.push(ms(t0.elapsed()));
            out_len = out.len();
        }
        let (med, p90) = median_p90(&mut times);
        println!(
            "png_compression fixture={name} backend={backend_label} level={label} \
median_ms={med:.3} p90_ms={p90:.3} bytes={out_len} b64={}",
            b64_len(out_len),
        );
    }
}

fn run(backend: Backend, label: &str) {
    println!("image_timing backend={label}");
    for name in FIXTURES {
        bench_fixture(name, backend, label);
    }
    bench_png_compression("shot-4000x3000.png", backend, label);
}

fn backend_for_build() -> Backend {
    #[cfg(all(feature = "image", feature = "image-fir"))]
    {
        return match std::env::var("IMAGE_RESIZE_BACKEND").as_deref() {
            Ok("image") => Backend::Image,
            _ => Backend::Fir,
        };
    }
    #[cfg(all(feature = "image-fir", not(feature = "image")))]
    return Backend::Fir;
    #[cfg(all(feature = "image", not(feature = "image-fir")))]
    return Backend::Image;
}

#[cfg(feature = "image-fir")]
fn encode_png_default(img: &DynamicImage) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Default, PngFilterType::Adaptive)
        .write_image(img.as_bytes(), img.width(), img.height(), img.color().into())
        .unwrap();
    out
}

#[cfg(feature = "image-fir")]
fn resize_fir_path(bytes: &[u8]) -> (DynamicImage, u32, u32, u32, u32) {
    let img = image::load_from_memory(bytes).unwrap();
    let (w, h) = img.dimensions();
    let (nw, nh) = fit_dims(w, h);
    let img = resize_fir(img, nw, nh);
    (img, w, h, nw, nh)
}

#[cfg(feature = "image-fir")]
fn bench_photo_file(path: &Path, label: &str) {
    let bytes = std::fs::read(path).unwrap();
    let (sw, sh) = ImageReader::open(path)
        .unwrap()
        .with_guessed_format()
        .unwrap()
        .into_dimensions()
        .unwrap();
    let (img, _, _, rw, rh) = resize_fir_path(&bytes);
    let mut png_times = Vec::with_capacity(5);
    let mut jpeg_times = Vec::with_capacity(5);
    let mut png_bytes = 0usize;
    let mut jpeg_bytes = 0usize;
    for _ in 0..5 {
        let t0 = Instant::now();
        let out = encode_png_default(&img);
        png_times.push(ms(t0.elapsed()));
        png_bytes = out.len();
        black_box(out);
    }
    for _ in 0..5 {
        let t0 = Instant::now();
        let out = encode_jpeg_q80(&img);
        jpeg_times.push(ms(t0.elapsed()));
        jpeg_bytes = out.len();
        black_box(out);
    }
    let png_b64 = b64_len(png_bytes);
    let jpeg_b64 = b64_len(jpeg_bytes);
    let png_med = median_only(&mut png_times);
    let jpeg_med = median_only(&mut jpeg_times);
    let cap = 1_048_576usize;
    println!(
        "label={label} path={} source={}x{} source_bytes={} resized={}x{} \
png_bytes={} png_b64={} png_under_1mib_b64={} png_encode_median_ms={:.3} \
jpeg_bytes={} jpeg_b64={} jpeg_under_1mib_b64={} jpeg_encode_median_ms={:.3}",
        path.display(),
        sw,
        sh,
        bytes.len(),
        rw,
        rh,
        png_bytes,
        png_b64,
        png_b64 < cap,
        png_med,
        jpeg_bytes,
        jpeg_b64,
        jpeg_b64 < cap,
        jpeg_med,
    );
}

#[cfg(feature = "image-fir")]
pub fn run_if_png_photos() -> bool {
    if std::env::var("IMAGE_PNG_PHOTOS").as_deref() != Ok("1") {
        return false;
    }
    let manifest = std::env::var("IMAGE_PHOTO_MANIFEST").expect("IMAGE_PHOTO_MANIFEST path");
    println!("image_png_photos backend=image-fir");
    for line in std::fs::read_to_string(&manifest).unwrap().lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (label, path) = line.split_once('\t').expect("manifest line: label<TAB>path");
        bench_photo_file(Path::new(path), label);
    }
    true
}

#[cfg(not(feature = "image-fir"))]
pub fn run_if_png_photos() -> bool {
    if std::env::var("IMAGE_PNG_PHOTOS").as_deref() == Ok("1") {
        eprintln!("IMAGE_PNG_PHOTOS requires --features image-fir");
        std::process::exit(1);
    }
    false
}

pub fn run_if_timing() -> bool {
    if std::env::var("IMAGE_SPAWN_BENCH").as_deref() == Ok("1") {
        spawn_bench();
        return true;
    }
    if std::env::var("IMAGE_TIMING").as_deref() != Ok("1") {
        return false;
    }
    #[cfg(all(feature = "image", feature = "image-fir"))]
    {
        eprintln!("build one of image or image-fir for IMAGE_TIMING");
        std::process::exit(1);
    }
    #[cfg(feature = "image-fir")]
    {
        run(Backend::Fir, "image-fir");
        return true;
    }
    #[cfg(feature = "image")]
    {
        run(Backend::Image, "image");
        return true;
    }
    #[allow(unreachable_code)]
    false
}

pub fn handle_child_args() -> bool {
    match std::env::var("IMAGE_CHILD").ok().as_deref() {
        Some("noop") => true,
        Some("fit") => {
            let path = std::env::args().nth(1).expect("IMAGE_CHILD=fit needs fixture path");
            child_fit(Path::new(&path));
            true
        }
        _ => false,
    }
}
