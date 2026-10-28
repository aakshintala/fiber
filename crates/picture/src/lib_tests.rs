//! Tests beside [`super::run`]: the child's arguments, output and exit codes.

use std::ffi::OsString;

use super::run;

fn args(values: &[&std::path::Path]) -> Vec<OsString> {
    values
        .iter()
        .map(|value| value.as_os_str().to_owned())
        .collect()
}

/// Runs the child and returns the exit code, standard output and standard
/// error.
fn child(arguments: &[OsString]) -> (i32, String, String) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = run(arguments, &mut stdout, &mut stderr);
    (
        code,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

/// Runs the child and returns the exit code, raw standard output and
/// standard error: a conversion's PNG is binary, never UTF-8.
fn child_bytes(arguments: &[OsString]) -> (i32, Vec<u8>, String) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = run(arguments, &mut stdout, &mut stderr);
    (code, stdout, String::from_utf8(stderr).unwrap())
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([1, 2, 3]));
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([4, 5, 6]));
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

/// A writer whose every write fails, so a stdout failure exits 3.
struct FailWrite;

impl std::io::Write for FailWrite {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("no room"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("no room"))
    }
}

/// The PNG signature every `--png` conversion writes.
const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

#[test]
fn the_wrong_number_of_arguments_is_a_usage_error() {
    for count in [0, 1, 2, 4] {
        let arguments: Vec<OsString> = (0..count).map(|n| OsString::from(n.to_string())).collect();
        let (code, stdout, stderr) = child(&arguments);
        assert_eq!(code, 2, "{count}");
        assert!(stdout.is_empty());
        assert!(stderr.starts_with("usage:"), "{stderr}");
    }
}

#[test]
fn a_stem_with_a_separator_is_a_usage_error() {
    let dir = fakes::TempDir::new("fiber-picture");
    let input = dir.path().join("a.png");
    std::fs::write(&input, png(4, 4)).unwrap();
    for stem in ["", "../x", "a/b", "a.b"] {
        let arguments = [
            input.clone().into_os_string(),
            dir.path().as_os_str().to_owned(),
            OsString::from(stem),
        ];
        assert_eq!(child(&arguments).0, 2, "{stem:?}");
    }
}

#[test]
fn a_stem_may_hold_letters_digits_underscores_and_hyphens() {
    let dir = fakes::TempDir::new("fiber-picture");
    let input = dir.path().join("in.png");
    std::fs::write(&input, png(4, 4)).unwrap();
    let (code, stdout, _) = child(&args(&[&input, dir.path(), "Az_09-x".as_ref()]));
    assert_eq!(code, 0);
    assert!(stdout.contains("Az_09-x.png"), "{stdout}");
}

#[test]
fn an_image_is_written_and_named_on_one_json_line() {
    let dir = fakes::TempDir::new("fiber-picture");
    let input = dir.path().join("in.png");
    let bytes = png(800, 600);
    std::fs::write(&input, &bytes).unwrap();
    let out = dir.path().join("artifacts");
    std::fs::create_dir(&out).unwrap();
    let (code, stdout, stderr) = child(&args(&[&input, &out, "i_00112233445566ff".as_ref()]));
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert_eq!(
        stdout,
        "{\"file\":\"i_00112233445566ff.png\",\"height\":600,\"mime_type\":\"image/png\",\"width\":800}\n"
    );
    assert_eq!(
        std::fs::read(out.join("i_00112233445566ff.png")).unwrap(),
        bytes
    );
}

#[test]
fn an_over_limit_image_exits_1_with_the_count_and_writes_nothing() {
    let dir = fakes::TempDir::new("fiber-picture");
    let input = dir.path().join("big.png");
    // Header only: 8000 x 7000 and no pixels.
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = 8000_u32.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&7000_u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    for (kind, data) in [(b"IHDR", ihdr), (b"IDAT", vec![0x78, 0x9C, 0x00])] {
        let mut body = kind.to_vec();
        body.extend_from_slice(&data);
        bytes.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&body);
        let mut crc = 0xFFFF_FFFF_u32;
        for byte in &body {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        bytes.extend_from_slice(&(!crc).to_be_bytes());
    }
    std::fs::write(&input, bytes).unwrap();
    let (code, stdout, stderr) = child(&args(&[&input, dir.path(), "i_1".as_ref()]));
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        "8000x7000 is 56000000 pixels; the limit is 50000000\n"
    );
    assert!(!dir.path().join("i_1.png").exists());
}

#[test]
fn bytes_that_are_not_an_image_exit_1() {
    let dir = fakes::TempDir::new("fiber-picture");
    let input = dir.path().join("x.png");
    std::fs::write(&input, b"nope").unwrap();
    let (code, stdout, stderr) = child(&args(&[&input, dir.path(), "i_2".as_ref()]));
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty());
}

#[test]
fn a_missing_input_exits_1() {
    let dir = fakes::TempDir::new("fiber-picture");
    let missing = dir.path().join("missing.png");
    let (code, _, stderr) = child(&args(&[&missing, dir.path(), "i_3".as_ref()]));
    assert_eq!(code, 1);
    assert!(stderr.contains("No such file"), "{stderr}");
}

#[test]
fn an_unwritable_directory_exits_3() {
    let dir = fakes::TempDir::new("fiber-picture");
    let input = dir.path().join("in.png");
    std::fs::write(&input, png(4, 4)).unwrap();
    let missing = dir.path().join("no-such-dir");
    let (code, stdout, stderr) = child(&args(&[&input, &missing, "i_4".as_ref()]));
    assert_eq!(code, 3);
    assert!(stdout.is_empty());
    assert!(stderr.starts_with("i_4.png: "), "{stderr}");
}

#[test]
fn the_png_form_converts_a_jpeg_to_png_on_stdout() {
    let dir = fakes::TempDir::new("fiber-picture-png");
    let input = dir.path().join("in.jpg");
    std::fs::write(&input, jpeg(64, 48)).unwrap();
    let (code, bytes, stderr) = child_bytes(&[OsString::from("--png"), input.into_os_string()]);
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert!(bytes.starts_with(PNG_SIGNATURE));
    let again = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap();
    assert_eq!((again.width(), again.height()), (64, 48));
}

#[test]
fn the_png_form_accepts_a_png() {
    let dir = fakes::TempDir::new("fiber-picture-png-png");
    let input = dir.path().join("in.png");
    std::fs::write(&input, png(30, 20)).unwrap();
    let (code, bytes, stderr) = child_bytes(&[OsString::from("--png"), input.into_os_string()]);
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert!(bytes.starts_with(PNG_SIGNATURE));
}

#[test]
fn the_png_form_refused_is_exit_1_with_the_reason() {
    let dir = fakes::TempDir::new("fiber-picture-png-refused");
    let input = dir.path().join("in.bin");
    std::fs::write(&input, b"not an image at all").unwrap();
    let (code, stdout, stderr) = child(&[OsString::from("--png"), input.into_os_string()]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty(), "a refusal names its reason");
}

#[test]
fn the_png_form_refuses_over_50_megapixels_from_the_header() {
    let dir = fakes::TempDir::new("fiber-picture-png-limit");
    let input = dir.path().join("big.png");
    // Header only: 8000 x 7000 and no pixels.
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = 8000_u32.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&7000_u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    for (kind, data) in [(b"IHDR", ihdr), (b"IDAT", vec![0x78, 0x9C, 0x00])] {
        let mut body = kind.to_vec();
        body.extend_from_slice(&data);
        bytes.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&body);
        let mut crc = 0xFFFF_FFFF_u32;
        for byte in &body {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        bytes.extend_from_slice(&(!crc).to_be_bytes());
    }
    std::fs::write(&input, bytes).unwrap();
    let (code, stdout, stderr) = child(&[OsString::from("--png"), input.into_os_string()]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        "8000x7000 is 56000000 pixels; the limit is 50000000\n"
    );
}

#[test]
fn the_png_form_missing_input_is_exit_1() {
    let dir = fakes::TempDir::new("fiber-picture-png-missing");
    let (code, stdout, stderr) = child(&[
        OsString::from("--png"),
        dir.path().join("missing.jpg").into_os_string(),
    ]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(stderr.contains("No such file"), "{stderr}");
}

#[test]
fn the_png_form_with_the_wrong_number_of_arguments_is_usage_2() {
    let dir = fakes::TempDir::new("fiber-picture-png-usage");
    let input = dir.path().join("in.jpg");
    std::fs::write(&input, jpeg(8, 8)).unwrap();
    for arguments in [
        vec![OsString::from("--png")],
        vec![
            OsString::from("--png"),
            input.as_os_str().to_owned(),
            OsString::from("extra"),
            OsString::from("extra"),
        ],
    ] {
        let (code, stdout, stderr) = child(&arguments);
        assert_eq!(code, 2, "{arguments:?}");
        assert!(stdout.is_empty());
        assert_eq!(
            stderr, "usage: fiber image --png <input>\n",
            "{arguments:?}"
        );
    }
}

#[test]
fn the_png_form_does_not_reinterpret_the_three_operand_form() {
    // Three arguments starting with `--png` stay the stored-file form:
    // a file named `--png` is read, not converted.
    let arguments = [
        OsString::from("--png"),
        OsString::from("artifacts"),
        OsString::from("i_9"),
    ];
    let (code, stdout, _) = child(&arguments);
    assert_eq!(code, 1, "{arguments:?}");
    assert!(stdout.is_empty(), "{arguments:?}");
}

#[test]
fn the_png_flag_off_the_form_is_not_a_conversion() {
    // Two arguments not starting with `--png` stay the stored-file form's
    // usage error, never a conversion.
    let (code, stdout, stderr) = child(&[OsString::from("--pn"), OsString::from("in.jpg")]);
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        "usage: fiber image <input> <artifacts_dir> <stem>\n"
    );
}

#[test]
fn the_png_form_stdout_failure_is_exit_3() {
    let dir = fakes::TempDir::new("fiber-picture-png-write");
    let input = dir.path().join("in.jpg");
    std::fs::write(&input, jpeg(8, 8)).unwrap();
    let arguments = [OsString::from("--png"), input.into_os_string()];
    let mut stderr = Vec::new();
    let code = super::run(&arguments, &mut FailWrite, &mut stderr);
    assert_eq!(code, 3);
}

#[test]
fn a_name_that_exists_is_never_overwritten() {
    let dir = fakes::TempDir::new("fiber-picture");
    let input = dir.path().join("in.png");
    std::fs::write(&input, png(4, 4)).unwrap();
    std::fs::write(dir.path().join("i_5.png"), b"old").unwrap();
    let (code, _, _) = child(&args(&[&input, dir.path(), "i_5".as_ref()]));
    assert_eq!(code, 3);
    assert_eq!(std::fs::read(dir.path().join("i_5.png")).unwrap(), b"old");
}
