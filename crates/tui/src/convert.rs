//! Converting a stored JPEG or WebP to PNG in the image child: one
//! worker runs `<fiber> image --png /dev/stdin` with the decoded file
//! bytes on stdin and answers with the PNG in base64 (`docs/tui.md`,
//! "Images").

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::Input;
use crate::image::Convert;

/// The most standard-output bytes kept: past it the child is killed and
/// the image fails (`docs/tui.md`, "Images").
pub(crate) const OUTPUT_CAP: usize = 32 * 1024 * 1024;

/// The most standard-error bytes kept for the notice: past it the rest
/// is drained and dropped, so the child cannot block on a full pipe.
const STDERR_CAP: u64 = 16 * 1024;

/// The PNG signature a conversion must start with.
const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Runs the converter `argv` for `convert` on a worker thread, answering
/// once with [`Input::Converted`]: the PNG in base64, already encoded on
/// the worker, or why the image failed. Standard output past `cap` kills
/// the child and fails the image; only exit 0 with the PNG signature
/// converts, and the loop never waits on the child itself.
pub(crate) fn spawn(argv: &[OsString], convert: Convert, out: &Sender<Input>, cap: usize) {
    let argv = argv.to_owned();
    let worker_out = out.clone();
    let fallback_out = out.clone();
    let generation = convert.generation;
    let id = convert.id;
    if let Err(error) = crate::sources::builder("tui-convert").spawn(move || {
        let result = run(&argv, &convert, cap);
        answer(&worker_out, convert.generation, convert.id, result);
    }) {
        answer(
            &fallback_out,
            generation,
            id,
            Err(format!("the converter could not start: {error}")),
        );
    }
}

/// Posts one conversion's outcome as `Input::Converted`: a closed loop
/// drops it (`docs/tui.md`, "Images").
fn answer(out: &Sender<Input>, generation: u64, id: u32, result: Result<String, String>) {
    drop(out.send(Input::Converted {
        generation,
        id,
        result,
    }));
}

/// Converts `convert`'s bytes through the child at `argv`.
fn run(argv: &[OsString], convert: &Convert, cap: usize) -> Result<String, String> {
    let bytes = STANDARD
        .decode(&convert.data)
        .map_err(|error| format!("the image is not base64: {error}"))?;
    let Some((program, rest)) = argv.split_first() else {
        return Err("no program converts images".to_owned());
    };
    let mut child = Command::new(program)
        .args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("the converter could not start: {error}"))?;
    let (stdin, mut stdout, mut stderr) =
        match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
            (Some(stdin), Some(stdout), Some(stderr)) => (stdin, stdout, stderr),
            _ => {
                drop(child.kill());
                return Err("the converter kept its stdio".to_owned());
            }
        };
    std::thread::scope(|scope| {
        scope.spawn(|| {
            // A broken pipe means the child died or refused early: its
            // exit and stderr say why.
            let mut stdin = stdin;
            drop(stdin.write_all(&bytes).and_then(|()| stdin.flush()));
        });
        // Standard error drains on its own thread while standard output
        // reads here: a child that fills its error pipe still reaches
        // its exit, which closes standard output.
        let drained = scope.spawn(|| {
            // The retained stderr is capped, and the rest is drained and
            // dropped, so the child cannot block on a full pipe.
            let mut retained = Vec::new();
            let read = (&mut stderr)
                .take(STDERR_CAP)
                .read_to_end(&mut retained)
                .and_then(|_| std::io::copy(&mut stderr, &mut std::io::sink()).map(|_| ()));
            (retained, read)
        });
        let mut out = Vec::new();
        let limit = u64::try_from(cap.saturating_add(1)).unwrap_or(u64::MAX);
        let read = (&mut stdout).take(limit).read_to_end(&mut out);
        if read.is_err() {
            drop(child.kill());
            drop(child.wait());
            return Err("the converter could not be read".to_owned());
        }
        if out.len() > cap {
            drop(child.kill());
            drop(child.wait());
            return Err(format!("the converted image is over {cap} bytes"));
        }
        let (retained, draining) = match drained.join() {
            Ok(done) => done,
            Err(_) => return Err("the converter could not be read".to_owned()),
        };
        if draining.is_err() {
            drop(child.kill());
            drop(child.wait());
            return Err("the converter could not be read".to_owned());
        }
        if !child.wait().map(|status| status.success()).unwrap_or(false) {
            let reason = String::from_utf8_lossy(&retained);
            let reason = reason.trim_end();
            if reason.is_empty() {
                return Err("image conversion failed".to_owned());
            }
            return Err(reason.to_owned());
        }
        if !out.starts_with(PNG_SIGNATURE) {
            return Err("the conversion was not a PNG image".to_owned());
        }
        Ok(STANDARD.encode(&out))
    })
}

#[cfg(test)]
#[path = "convert_tests.rs"]
pub(crate) mod tests;
