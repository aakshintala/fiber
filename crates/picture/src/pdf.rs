//! The child's PDF mode (`docs/tools.md`, "read"): `fiber image pdf <input>
//! <artifacts_dir> <stem> <what>` counts a PDF's pages and cuts a page range
//! (`docs/tools.md`, "read"). `<what>` is `whole=<max>` or
//! `pages=<first>-<last>`, with `1 <= first <= last`.

use std::io::Write;
use std::path::Path;

/// Exit code: the PDF was unreadable, encrypted or has no pages.
const REFUSED: i32 = 1;
/// Exit code: the arguments are not `pdf <input> <artifacts_dir> <stem> <what>`.
const USAGE: i32 = 2;
/// Exit code: the cut file could not be written.
const WRITE_FAILED: i32 = 3;
/// Exit code: the PDF has more pages than asked for.
pub(crate) const TOO_MANY: i32 = 4;

/// The most bytes one object or cross-reference stream may decompress to
/// while the PDF loads (`docs/tools.md`, "read"). This is defense in depth
/// per stream, separate from the 100 MiB file-size cap: only object and
/// cross-reference streams are decoded while loading, and each is bounded
/// on its own, with no aggregate bound.
const MAX_DECOMPRESSED_BYTES: usize = 67_108_864;

/// Options for loading an untrusted PDF: each object and cross-reference
/// stream's decompressed size is bounded, and anything else parses
/// leniently. `strict` stays off on purpose: it would also reject readable
/// but non-conforming files, such as 19-byte xref entries or a header line
/// with trailing bytes, which today load. A dropped object stream is refused
/// by [`dropped_packed_object`] instead: the lenient load skips a stream it
/// cannot decode where `strict` would fail it, so `run` compares the packed
/// objects the cross-reference table promises against the ones that loaded
/// and refuses the file when any is missing. An over-limit cross-reference
/// stream already fails the load itself.
fn load_options() -> lopdf::LoadOptions {
    lopdf::LoadOptions {
        max_decompressed_size: Some(MAX_DECOMPRESSED_BYTES),
        ..Default::default()
    }
}

/// Whether any object the cross-reference table packs into an object stream
/// is missing from the loaded document. The lenient load drops a stream it
/// cannot decode, such as one past [`MAX_DECOMPRESSED_BYTES`], instead of
/// failing, which would serve the file with objects silently missing, so
/// any load error, including one such stream, fails the whole file.
fn dropped_packed_object(document: &lopdf::Document) -> bool {
    document.reference_table.entries.iter().any(|(id, entry)| {
        matches!(entry, lopdf::xref::XrefEntry::Compressed { .. })
            && !document.objects.contains_key(&(*id, 0))
    })
}

/// What the caller asked for.
#[derive(Debug, PartialEq, Eq)]
enum What {
    /// At most `max` pages; the input's bytes are stored unchanged.
    Whole { max: u32 },
    /// Pages `first` through `last`, counted from 1.
    Pages { first: u32, last: u32 },
}

/// Runs the PDF mode on the child's own streams and returns its exit code.
/// `args` are the arguments after `image`, starting with `pdf`.
pub(crate) fn run(
    args: &[std::ffi::OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let [_, input, directory, stem, what] = args else {
        writeln!(
            stderr,
            "usage: fiber image pdf <input> <artifacts_dir> <stem> <what>"
        )
        .unwrap_or(());
        return USAGE;
    };
    let Some(stem) = stem.to_str().filter(|stem| super::valid_stem(stem)) else {
        writeln!(stderr, "the stem must be letters, digits, `_` or `-`").unwrap_or(());
        return USAGE;
    };
    let Some(what) = what.to_str() else {
        writeln!(
            stderr,
            "the request must be `whole=<max>` or `pages=<first>-<last>`"
        )
        .unwrap_or(());
        return USAGE;
    };
    let what = match parse_what(what) {
        Some(what) => what,
        None => {
            writeln!(
                stderr,
                "the request must be `whole=<max>` or `pages=<first>-<last>`"
            )
            .unwrap_or(());
            return USAGE;
        }
    };
    let bytes = match std::fs::read(input) {
        Ok(bytes) => bytes,
        Err(error) => {
            writeln!(stderr, "{error}").unwrap_or(());
            return REFUSED;
        }
    };
    let mut document = match lopdf::Document::load_mem_with_options(&bytes, load_options()) {
        Ok(document) => document,
        Err(error) => {
            writeln!(stderr, "{error}").unwrap_or(());
            return REFUSED;
        }
    };
    if dropped_packed_object(&document) {
        writeln!(
            stderr,
            "an object stream could not be decoded, so the file would run with objects missing"
        )
        .unwrap_or(());
        return REFUSED;
    };
    let total = document.get_pages().len();
    let Ok(total_u32) = u32::try_from(total) else {
        writeln!(stderr, "the PDF has more pages than can be counted").unwrap_or(());
        return REFUSED;
    };
    if total == 0 {
        writeln!(stderr, "the PDF has no pages").unwrap_or(());
        return REFUSED;
    }
    match what {
        What::Whole { max } => {
            if total_u32 > max {
                if writeln!(stdout, "{{\"total\":{total_u32}}}").is_err() {
                    return WRITE_FAILED;
                }
                if stdout.flush().is_err() {
                    return WRITE_FAILED;
                }
                return TOO_MANY;
            }
            let file = format!("{stem}.pdf");
            if let Err(error) = super::write_new(&Path::new(directory).join(&file), &bytes) {
                writeln!(stderr, "{file}: {error}").unwrap_or(());
                return WRITE_FAILED;
            }
            let line = serde_json::json!({
                "file": file,
                "page_count": total_u32,
                "total": total_u32,
            });
            if writeln!(stdout, "{line}").is_err() || stdout.flush().is_err() {
                return WRITE_FAILED;
            }
            0
        }
        What::Pages { first, last } => {
            if last > total_u32 {
                if writeln!(stdout, "{{\"total\":{total_u32}}}").is_err() {
                    return WRITE_FAILED;
                }
                if stdout.flush().is_err() {
                    return WRITE_FAILED;
                }
                return TOO_MANY;
            }
            let remove: Vec<u32> = (1..=total_u32)
                .filter(|page| *page < first || *page > last)
                .collect();
            document.delete_pages(&remove);
            document.prune_objects();
            let mut cut = Vec::new();
            if let Err(error) = document.save_to(&mut cut) {
                writeln!(stderr, "{error}").unwrap_or(());
                return REFUSED;
            }
            let file = format!("{stem}.pdf");
            if let Err(error) = super::write_new(&Path::new(directory).join(&file), &cut) {
                writeln!(stderr, "{file}: {error}").unwrap_or(());
                return WRITE_FAILED;
            }
            let page_count = last - first + 1;
            let line = serde_json::json!({
                "file": file,
                "page_count": page_count,
                "total": total_u32,
            });
            if writeln!(stdout, "{line}").is_err() || stdout.flush().is_err() {
                return WRITE_FAILED;
            }
            0
        }
    }
}

/// Parses `<what>`: `whole=<max>` or `pages=<first>-<last>`, ASCII digits
/// only, `1 <= first <= last`. Anything else is a usage error.
fn parse_what(what: &str) -> Option<What> {
    if let Some(rest) = what.strip_prefix("whole=") {
        // An empty `rest` fails the parse below.
        if !rest.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let max: u32 = rest.parse().ok()?;
        return Some(What::Whole { max });
    }
    if let Some(rest) = what.strip_prefix("pages=") {
        let (first_text, last_text) = rest.split_once('-')?;
        // An empty half fails the parse below.
        if !first_text.bytes().all(|byte| byte.is_ascii_digit())
            || !last_text.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let first: u32 = first_text.parse().ok()?;
        let last: u32 = last_text.parse().ok()?;
        if first < 1 || first > last {
            return None;
        }
        return Some(What::Pages { first, last });
    }
    None
}

#[cfg(test)]
#[path = "pdf_tests.rs"]
mod tests;
