//! Tests beside [`super::spawn`]: the worker streams stdin to a scripted
//! child and answers with the PNG in base64, or why the image failed.

use std::ffi::OsString;
use std::sync::mpsc::{self, Receiver};

use base64::Engine as _;

use super::{Convert, OUTPUT_CAP, spawn};
use crate::Input;
use fakes::{Deadline, MUST_SUCCEED_WITHIN};

/// A 1 by 1 red PNG: the scripted child's fixed output.
pub(crate) const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// A conversion holding `data` in base64.
fn convert(data: &[u8]) -> Convert {
    Convert {
        generation: 7,
        id: 3,
        data: base64::engine::general_purpose::STANDARD.encode(data),
    }
}

/// The worker's completion within the deadline, naming its generation
/// and id.
#[track_caller]
fn completion(rx: &Receiver<Input>, wait: &Deadline) -> Result<String, String> {
    match wait.recv(rx) {
        Ok(Input::Converted {
            generation,
            id,
            result,
        }) => {
            assert_eq!((generation, id), (7, 3));
            result
        }
        Ok(_) => panic!("the worker answered something else"),
        Err(err) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the worker: {err}"),
    }
}

/// The worker's argv running `script` through `/bin/sh` with `args`.
fn argv(script: &std::path::Path, args: &[&std::path::Path]) -> Vec<OsString> {
    let mut argv = vec![OsString::from("/bin/sh"), script.as_os_str().to_owned()];
    argv.extend(args.iter().map(|arg| arg.as_os_str().to_owned()));
    argv
}

#[test]
fn output_cap_is_32_mib() {
    assert_eq!(OUTPUT_CAP, 32 * 1024 * 1024);
}

#[test]
fn a_converted_png_comes_back_as_base64() {
    let dir = fakes::TempDir::new("fiber-convert-png");
    let png = dir.path().join("out.png");
    std::fs::write(&png, PNG).unwrap();
    let script = fakes::script(dir.path(), "convert", "cat \"$1\"\n");
    let (out, rx) = mpsc::channel();
    spawn(
        &argv(&script, &[&png]),
        convert(b"jpeg-bytes"),
        &out,
        OUTPUT_CAP,
    );
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    let result = completion(&rx, &wait);
    assert_eq!(
        result,
        Ok(base64::engine::general_purpose::STANDARD.encode(PNG))
    );
}

#[test]
fn bytes_reach_the_childs_stdin() {
    let dir = fakes::TempDir::new("fiber-convert-stdin");
    let marker = dir.path().join("stdin");
    let png = dir.path().join("out.png");
    std::fs::write(&png, PNG).unwrap();
    let script = fakes::script(dir.path(), "convert", "cat > \"$1\"; cat \"$2\"\n");
    let (out, rx) = mpsc::channel();
    spawn(
        &argv(&script, &[&marker, &png]),
        convert(b"jpeg-bytes"),
        &out,
        OUTPUT_CAP,
    );
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    let result = completion(&rx, &wait);
    assert!(result.is_ok());
    assert_eq!(
        std::fs::read(&marker).unwrap_or_else(|err| panic!("read: {err}")),
        b"jpeg-bytes"
    );
}

#[test]
fn a_failing_child_is_its_stderr() {
    let dir = fakes::TempDir::new("fiber-convert-fail");
    let script = fakes::script(
        dir.path(),
        "convert",
        "echo -n 'no such thing' >&2; exit 1\n",
    );
    let (out, rx) = mpsc::channel();
    spawn(
        &argv(&script, &[]),
        convert(b"jpeg-bytes"),
        &out,
        OUTPUT_CAP,
    );
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(completion(&rx, &wait), Err("no such thing".to_owned()));
}

#[test]
fn stderr_is_trimmed_and_capped() {
    let dir = fakes::TempDir::new("fiber-convert-stderr");
    // Trailing whitespace never reaches the notice.
    let script = fakes::script(dir.path(), "trim", "printf 'bad   \\n' >&2; exit 1\n");
    let (out, rx) = mpsc::channel();
    spawn(&argv(&script, &[]), convert(b"x"), &out, OUTPUT_CAP);
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(completion(&rx, &wait), Err("bad".to_owned()));
    // Past the cap the message still arrives, and the child never
    // blocks on its full pipe: 1 MiB of stderr completes with the
    // first 16 KiB retained.
    let script = fakes::script(
        dir.path(),
        "cap",
        "head -c 1048576 /dev/zero | tr '\\0' 'e' >&2; exit 1\n",
    );
    let (out, rx) = mpsc::channel();
    spawn(&argv(&script, &[]), convert(b"x"), &out, OUTPUT_CAP);
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    let result = completion(&rx, &wait).unwrap_err();
    assert_eq!(result.len(), 16 * 1024, "stderr is capped");
    assert!(result.bytes().all(|byte| byte == b'e'));
}

#[test]
fn empty_stderr_is_the_default_failure() {
    let dir = fakes::TempDir::new("fiber-convert-empty");
    let script = fakes::script(dir.path(), "convert", "exit 1\n");
    let (out, rx) = mpsc::channel();
    spawn(
        &argv(&script, &[]),
        convert(b"jpeg-bytes"),
        &out,
        OUTPUT_CAP,
    );
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(
        completion(&rx, &wait),
        Err("image conversion failed".to_owned())
    );
}

#[test]
fn a_missing_program_fails() {
    let (out, rx) = mpsc::channel();
    spawn(
        &[OsString::from("/nonexistent-fiber-convert-test")],
        convert(b"jpeg-bytes"),
        &out,
        OUTPUT_CAP,
    );
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    let result = completion(&rx, &wait).unwrap_err();
    assert!(result.contains("could not start"), "{result:?}");
}

#[test]
fn an_empty_argv_fails() {
    let argv: Vec<OsString> = Vec::new();
    let (out, rx) = mpsc::channel();
    spawn(&argv, convert(b"jpeg-bytes"), &out, OUTPUT_CAP);
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(
        completion(&rx, &wait),
        Err("no program converts images".to_owned())
    );
}

#[test]
fn output_over_the_cap_kills_and_fails() {
    let dir = fakes::TempDir::new("fiber-convert-cap");
    // `exec` keeps the sleeper the direct child: killing the child
    // closes every pipe, never orphaning a grandchild that holds one.
    let script = fakes::script(
        dir.path(),
        "convert",
        "printf '12345678901234567'; exec sleep 3600\n",
    );
    let _watchdog = fakes::Watchdog::matching(&script.display().to_string());
    let (out, rx) = mpsc::channel();
    spawn(&argv(&script, &[]), convert(b"x"), &out, 16);
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(
        completion(&rx, &wait),
        Err("the converted image is over 16 bytes".to_owned())
    );
}

#[test]
fn output_at_the_cap_is_kept() {
    // Sixteen bytes are not a PNG, but the cap did not trip: the
    // failure names the signature, never the cap.
    let dir = fakes::TempDir::new("fiber-convert-cap-keep");
    let script = fakes::script(dir.path(), "convert", "printf '1234567890123456'\n");
    let (out, rx) = mpsc::channel();
    spawn(&argv(&script, &[]), convert(b"x"), &out, 16);
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(
        completion(&rx, &wait),
        Err("the conversion was not a PNG image".to_owned())
    );
}
