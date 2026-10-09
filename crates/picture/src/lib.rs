//! The image child (`docs/invocation.md`, "Processes"): `fiber image <input>
//! <artifacts_dir> <stem>` processes one image under the limits of
//! `docs/model-routing.md`, "Image limits", and writes the result to
//! `<artifacts_dir>/<stem>.<ext>`. It prints one JSON line naming the file and
//! exits 0. An image it refuses or cannot decode gets the decoder's message on
//! standard error and exit 1; a usage error exits 2 and a write failure 3.
//! `fiber image pdf <input> <artifacts_dir> <stem> <what>` counts a PDF's
//! pages and cuts a page range (`docs/tools.md`, "read").

mod fit;
mod pdf;

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// Exit code: the image was refused or cannot be decoded.
const REFUSED: i32 = 1;
/// Exit code: the arguments are not `<input> <artifacts_dir> <stem>`.
const USAGE: i32 = 2;
/// Exit code: the processed file could not be written.
const WRITE_FAILED: i32 = 3;

/// Runs the child on the process's own streams and returns its exit code.
/// `args` are the arguments after `image`.
pub fn main(args: Vec<OsString>) -> i32 {
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    run(&args, &mut stdout, &mut stderr)
}

fn run(args: &[OsString], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if args.first().is_some_and(|first| first == "pdf") {
        if args.len() != 5 {
            writeln!(stderr, "usage: fiber image pdf <input> <artifacts_dir> <stem> <what>")
                .unwrap_or(());
            return USAGE;
        }
        return pdf::run(args, stdout, stderr);
    }
    let [input, directory, stem] = args else {
        writeln!(stderr, "usage: fiber image <input> <artifacts_dir> <stem>").unwrap_or(());
        return USAGE;
    };
    let Some(stem) = stem.to_str().filter(|stem| valid_stem(stem)) else {
        writeln!(stderr, "the stem must be letters, digits, `_` or `-`").unwrap_or(());
        return USAGE;
    };
    let bytes = match std::fs::read(input) {
        Ok(bytes) => bytes,
        Err(error) => {
            writeln!(stderr, "{error}").unwrap_or(());
            return REFUSED;
        }
    };
    let stored = match fit::process(&bytes) {
        Ok(stored) => stored,
        Err(message) => {
            writeln!(stderr, "{message}").unwrap_or(());
            return REFUSED;
        }
    };
    let file = format!("{stem}.{}", stored.extension);
    if let Err(error) = write_new(&Path::new(directory).join(&file), &stored.bytes) {
        writeln!(stderr, "{file}: {error}").unwrap_or(());
        return WRITE_FAILED;
    }
    let line = serde_json::json!({
        "file": file,
        "mime_type": stored.mime_type,
        "width": stored.width,
        "height": stored.height,
    });
    if writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return WRITE_FAILED;
    }
    0
}

/// A stem is one path component the parent minted: no separator, no dot.
pub(crate) fn valid_stem(stem: &str) -> bool {
    !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Writes `bytes` to a file that must not exist yet, so a name is never
/// reused for different bytes.
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
