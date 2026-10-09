//! `read` of a PDF (`docs/tools.md`, "read"): the image child counts and cuts
//! the file once, and `pdftoppm` renders the stored cut's pages, each through
//! the image child, so a later model switch has them. Any render failure
//! leaves `pages` absent and the result says why; only a cancel ends the
//! call without a part.

use std::io;
use std::path::Path;
use std::process::Command;

use contract::ErrorCode;
use contract::images::Images as _;
use contract::shapes::{ContentPart, ImagePart, PdfPart};
use contract::tool::{Cancel, Output};
use serde_json::{Map, Value};

use crate::files::{failed, text_output};
use crate::image::{ImageChild, run_to_end};

/// The most pages a PDF without `pages` may hold (`docs/tools.md`, "read").
const WHOLE_MAX: u32 = 10;
/// The most pages one `read` takes (`docs/tools.md`, "read").
const RANGE_MAX: u32 = 20;
/// The longest side a rendered page is scaled to (`docs/model-routing.md`,
/// "Image limits").
const RENDER_SIDE: u32 = 2000;

/// Why the pages could not be rendered when `pdftoppm` is not installed.
const MISSING_RENDERER: &str = "pdftoppm is not installed. It comes with poppler (poppler-utils on Debian and Ubuntu, brew install poppler on macOS)";

/// A page range counted from 1, a single page `N` as `N-N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageRange {
    /// The first page, counted from 1.
    pub first: u32,
    /// The last page, `>= first`.
    pub last: u32,
}

/// Reads `pages` from the tool arguments: absent is the whole file, a string
/// `N` or `N-M` is that range, anything else is why the call fails.
pub(crate) fn page_range(arguments: &Map<String, Value>) -> Result<Option<PageRange>, String> {
    let Some(value) = arguments.get("pages") else {
        return Ok(None);
    };
    let Value::String(text) = value else {
        return Err("`pages` must be a string.".to_owned());
    };
    match parse_range(text) {
        Some(range) => {
            let count = range.last - range.first + 1;
            if count > RANGE_MAX {
                return Err(format!(
                    "`pages` {text} names {count} pages; a request takes at most {RANGE_MAX}."
                ));
            }
            Ok(Some(range))
        }
        None => Err(
            "`pages` must be a page or a range such as `3` or `1-5`, counted from 1.".to_owned(),
        ),
    }
}

/// Parses `N` or `N-M`: ASCII digits only, `1 <= N <= M`, each fitting `u32`.
fn parse_range(text: &str) -> Option<PageRange> {
    let (first_text, last_text) = match text.split_once('-') {
        Some((first, last)) => (first, last),
        None => (text, text),
    };
    // An empty half fails the parse below.
    if !first_text.bytes().all(|byte| byte.is_ascii_digit())
        || !last_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    // A second `-` leaves a non-digit in one half, refused above.
    let first: u32 = first_text.parse().ok()?;
    let last: u32 = last_text.parse().ok()?;
    if first < 1 || first > last {
        return None;
    }
    Some(PageRange { first, last })
}

/// Reads `path`, already resolved and classified as a PDF, through the image
/// child in PDF mode, then renders the stored cut's pages.
pub(crate) fn read(
    child: Option<&ImageChild>,
    path: &Path,
    range: Option<PageRange>,
    cancel: &dyn Cancel,
) -> Output {
    let Some(child) = child else {
        return failed(
            ErrorCode::ToolError,
            "PDF reading is not configured.".to_owned(),
        );
    };
    // A fresh stem per read: an older log line's path never points at new
    // bytes.
    let stem = crate::image::fresh_stem("p");
    let what = match range {
        Some(range) => format!("pages={}-{}", range.first, range.last),
        None => format!("whole={WHOLE_MAX}"),
    };
    let mut command = Command::new(child.fiber());
    command
        .arg("image")
        .arg("pdf")
        .arg(path)
        .arg(child.artifacts())
        .arg(&stem)
        .arg(&what);
    let Some(output) = run_to_end(&mut command, cancel) else {
        return text_output("Cancelled and stopped.\n".to_owned());
    };
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return failed(
                ErrorCode::ToolError,
                format!("the image child could not start: {error}."),
            );
        }
    };
    let message = crate::image::capped(&output.stderr);
    let (page_count, total) = match output.status.code() {
        Some(0) => match parse_child(&output.stdout, &stem) {
            Ok(cut) => cut,
            Err(why) => {
                return failed(ErrorCode::ToolError, format!("the image child {why}."));
            }
        },
        Some(4) => match parse_total(&output.stdout) {
            Some(total) => return past_the_end(path, range, total),
            None => {
                return failed(
                    ErrorCode::ToolError,
                    "the image child named no page count.".to_owned(),
                );
            }
        },
        Some(1) => {
            return failed(
                ErrorCode::UnsupportedFile,
                format!("`{}` cannot be read as a PDF: {message}", path.display()),
            );
        }
        Some(code) => {
            return failed(
                ErrorCode::ToolError,
                format!("the image child exited with status {code}: {message}"),
            );
        }
        None => {
            return failed(
                ErrorCode::ToolError,
                format!("the image child was killed by a signal: {message}"),
            );
        }
    };
    let head = match range {
        Some(range) => format!("PDF: pages {}-{} of {total}.\n", range.first, range.last),
        None => format!("PDF: {page_count} pages.\n"),
    };
    match render_pages(child, &stem, page_count, cancel) {
        Ok(pages) => {
            let part = PdfPart::new(format!("artifacts/{stem}.pdf"), page_count, Some(pages));
            match part {
                Ok(part) => Output {
                    content: vec![
                        ContentPart::Text { text: head.clone() },
                        ContentPart::Pdf(part),
                    ],
                    ..Output::default()
                },
                Err(_) => without_pages(
                    head,
                    "the rendered page count did not match".to_owned(),
                    &stem,
                    page_count,
                ),
            }
        }
        Err(RenderError::Cancelled) => text_output("Cancelled and stopped.\n".to_owned()),
        Err(RenderError::Failed(why)) => without_pages(head, why, &stem, page_count),
    }
}

/// The result when the pages could not be rendered: the PDF without `pages`
/// and the sentence saying why. There are never partial `pages`.
fn without_pages(head: String, why: String, stem: &str, page_count: u32) -> Output {
    // `page_count >= 1`: the child never reports zero pages, so this cannot
    // fail; a failure would hide the PDF itself, so fall back to the text.
    let text = format!("{head}The pages could not be rendered as images: {why}.\n");
    let part = PdfPart::new(format!("artifacts/{stem}.pdf"), page_count, None)
        .map(|part| {
            vec![
                ContentPart::Text { text: text.clone() },
                ContentPart::Pdf(part),
            ]
        })
        .unwrap_or_else(|_| vec![ContentPart::Text { text: text.clone() }]);
    Output {
        content: part,
        ..Output::default()
    }
}

/// The `invalid_arguments` failure when the file holds more pages than asked
/// for: the whole-file cap or a range past the end.
fn past_the_end(path: &Path, range: Option<PageRange>, total: u32) -> Output {
    match range {
        Some(range) => failed(
            ErrorCode::InvalidArguments,
            format!(
                "`pages` {}-{} is past the end: `{}` has {total} pages.",
                range.first,
                range.last,
                path.display()
            ),
        ),
        None => failed(
            ErrorCode::InvalidArguments,
            format!(
                "`{}` has {total} pages. A PDF of more than {WHOLE_MAX} pages needs `pages`, such as `1-5`; a request takes at most {RANGE_MAX} pages.",
                path.display()
            ),
        ),
    }
}

/// What the PDF child printed on success: the stored cut's page count and
/// the file's total. The named file must be exactly `<stem>.pdf`.
fn parse_child(stdout: &[u8], stem: &str) -> Result<(u32, u32), &'static str> {
    let text = std::str::from_utf8(stdout).map_err(|_| "printed text that is not UTF-8")?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.contains('\n') {
        return Err("printed more than one line");
    }
    let value: Value = serde_json::from_str(line).map_err(|_| "printed a line that is not JSON")?;
    let number = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let file = value.get("file").and_then(Value::as_str).map(str::to_owned);
    match (file, number("page_count"), number("total")) {
        (Some(file), Some(page_count), Some(total)) => {
            if !valid_pdf_file(stem, &file) {
                return Err("named a file other than the one asked for");
            }
            if page_count == 0 {
                return Err("printed a page count of zero");
            }
            Ok((page_count, total))
        }
        _ => Err("printed a line without file, page_count and total"),
    }
}

/// The `{"total":N}` the PDF child prints when the file holds more pages
/// than asked for.
fn parse_total(stdout: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(stdout).ok()?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.contains('\n') {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    value
        .get("total")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

/// Whether `file` is exactly the name asked for: `<stem>.pdf`, byte for
/// byte, so any other extension or case is refused.
fn valid_pdf_file(stem: &str, file: &str) -> bool {
    file == format!("{stem}.pdf")
}

/// Why rendering stopped.
enum RenderError {
    /// Cancelled; the call ends without a part.
    Cancelled,
    /// A render failure; the call keeps the PDF without `pages`.
    Failed(String),
}

/// Renders every page of the stored cut through `pdftoppm` and the image
/// child, in page order. Any failure gives `Failed`, never partial pages,
/// and no raw `<stem>-<n>.png` is left behind.
fn render_pages(
    child: &ImageChild,
    stem: &str,
    page_count: u32,
    cancel: &dyn Cancel,
) -> Result<Vec<ImagePart>, RenderError> {
    let pdf = child.artifacts().join(format!("{stem}.pdf"));
    let mut pages = Vec::new();
    for n in 1..=page_count {
        if cancel.is_cancelled() {
            return Err(RenderError::Cancelled);
        }
        let root = child.artifacts().join(format!("{stem}-{n}"));
        let raw = root.with_extension("png");
        let mut command = Command::new(child.renderer());
        command
            .arg("-png")
            .arg("-scale-to")
            .arg(RENDER_SIDE.to_string())
            .arg("-f")
            .arg(n.to_string())
            .arg("-l")
            .arg(n.to_string())
            .arg("-singlefile")
            .arg(&pdf)
            .arg(&root);
        let Some(output) = run_to_end(&mut command, cancel) else {
            drop(std::fs::remove_file(&raw));
            return Err(RenderError::Cancelled);
        };
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                drop(std::fs::remove_file(&raw));
                if error.kind() == io::ErrorKind::NotFound {
                    return Err(RenderError::Failed(MISSING_RENDERER.to_owned()));
                }
                return Err(RenderError::Failed(format!(
                    "pdftoppm failed to start: {error}."
                )));
            }
        };
        if !output.status.success() {
            drop(std::fs::remove_file(&raw));
            let message = crate::image::capped(&output.stderr);
            match output.status.code() {
                Some(code) => {
                    return Err(RenderError::Failed(format!(
                        "pdftoppm exited with status {code}: {message}."
                    )));
                }
                None => {
                    return Err(RenderError::Failed(format!(
                        "pdftoppm was killed by a signal: {message}."
                    )));
                }
            }
        }
        let bytes = match std::fs::read(&raw) {
            Ok(bytes) => bytes,
            Err(_) => {
                drop(std::fs::remove_file(&raw));
                return Err(RenderError::Failed("pdftoppm wrote no file.".to_owned()));
            }
        };
        drop(std::fs::remove_file(&raw));
        match child.process(&bytes, cancel) {
            Ok(reference) => pages.push(ImagePart {
                path: reference.path,
                mime_type: reference.mime_type,
                width: reference.width,
                height: reference.height,
            }),
            Err(contract::images::ImageError::Cancelled) => {
                return Err(RenderError::Cancelled);
            }
            Err(contract::images::ImageError::Unreadable(message))
            | Err(contract::images::ImageError::Failed(message)) => {
                return Err(RenderError::Failed(format!(
                    "the image child refused a page: {message}."
                )));
            }
        }
    }
    Ok(pages)
}

#[cfg(test)]
#[path = "pdf_tests.rs"]
mod tests;
