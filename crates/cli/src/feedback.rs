//! `fiber feedback`: the one code path both doors call. It captures
//! the production `doctor` report into a buffer, builds the Markdown in
//! memory, writes it once under the current directory, and returns the
//! file path with the new-issue link. A failure removes any partial
//! file, and the report never carries session content: only the version,
//! platform, install method, description and doctor output.

use std::io;
use std::path::{Path, PathBuf};

use contract::shapes::Failure;
use contract::{ErrorCode, clock};

/// What the report writer reads: the version text, the directory the
/// report lands in, the clock for the file stamp, and the doctor-output
/// capturer. Tests inject a stub capturer; both doors inject the
/// production one.
pub struct ReportContext<'a> {
    /// The bare version text, as `fiber version` prints after `fiber `.
    pub version: &'a str,
    /// The directory the report file lands in.
    pub cwd: &'a Path,
    /// The clock for the file stamp.
    pub clock: &'a dyn clock::Clock,
    /// The doctor output, or its capture failure.
    pub doctor: &'a dyn Fn() -> Result<String, FeedbackError>,
}

/// Why a feedback write failed: one sentence the door prints.
#[derive(Debug, thiserror::Error)]
pub enum FeedbackError {
    /// No description reached the writer.
    #[error("No description. Run `fiber feedback \"<description>\"` or `fiber feedback < <file>`.")]
    EmptyDescription,
    /// The doctor output could not be captured.
    #[error("could not capture the `fiber doctor` output: {0}")]
    Doctor(String),
    /// The report file could not be written.
    #[error("could not write {path}: {source}", path = path.display())]
    Write {
        /// The report file that failed.
        path: PathBuf,
        /// Why writing it failed.
        source: io::Error,
    },
}

impl From<FeedbackError> for Failure {
    fn from(error: FeedbackError) -> Self {
        let code = match &error {
            FeedbackError::EmptyDescription => ErrorCode::Usage,
            FeedbackError::Doctor(_) | FeedbackError::Write { .. } => ErrorCode::IoFailed,
        };
        Failure {
            code,
            message: error.to_string(),
            retry_after_ms: None,
            provider: None,
        }
    }
}

#[cfg(test)]
#[path = "feedback_tests.rs"]
mod tests;

/// The report's five sections in order: the title, the version,
/// platform and install method, the description, then the doctor
/// output verbatim.
pub fn build_report(
    version: &str,
    platform: &str,
    install: &str,
    description: &str,
    doctor: &str,
) -> String {
    format!(
        "# Fiber feedback\n\
         \n\
         - Version: fiber {version}\n\
         - Platform: {platform}\n\
         - Install: {install}\n\
         \n\
         ## What happened\n\
         \n\
         {description}\n\
         \n\
         ## Doctor\n\
         \n\
         {doctor}"
    )
}

/// How Fiber was installed: a release install reports its commit in the
/// version text, and a source build reports none.
fn install_method(version: &str) -> &'static str {
    if version.contains('(') {
        "install.sh"
    } else {
        "unknown"
    }
}

/// The platform line: the OS and architecture the binary runs on.
fn platform() -> String {
    format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The issue title: `Feedback: ` plus the description's first line, cut
/// to 120 characters on a character boundary.
fn title(description: &str) -> String {
    let first = description.lines().next().unwrap_or("");
    let title = format!("Feedback: {first}");
    match title.char_indices().nth(120) {
        Some((at, _)) => title[..at].to_owned(),
        None => title,
    }
}

/// The new-issue link for the report. At most 2000 characters: at or
/// under it the body carries the full report, over it the body carries
/// the description with the line saying to attach the file instead. The
/// title is always carried.
fn issue_link(title: &str, report: &str, description: &str) -> String {
    const BASE: &str = "https://github.com/aakshintala/fiber/issues/new";
    const LIMIT: usize = 2000;
    let full = format!("{BASE}?title={}&body={}", encode(title), encode(report));
    if full.len() <= LIMIT {
        return full;
    }
    let fallback = format!(
        "{description}\n\nThe full report was too long for a link: attach the report file."
    );
    format!("{BASE}?title={}&body={}", encode(title), encode(&fallback))
}

/// One `%XX` query component: UTF-8 bytes, leaving only the unreserved
/// set bare.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Builds the report in memory, writes it once, and returns its path
/// with the issue link. The file is `fiber-feedback-<ms>.md` in the
/// context's directory, bumped with `-2`, `-3` while the name exists, so
/// a same-millisecond rerun never overwrites. A write failure removes
/// any partial file and writes nothing else.
pub fn write(
    description: &str,
    ctx: &ReportContext<'_>,
) -> Result<contract::feedback::FeedbackDone, FeedbackError> {
    let description = description.trim();
    if description.is_empty() {
        return Err(FeedbackError::EmptyDescription);
    }
    let doctor = (ctx.doctor)()?;
    let report = build_report(
        ctx.version,
        &platform(),
        install_method(ctx.version),
        description,
        &doctor,
    );
    let link = issue_link(&title(description), &report, description);
    let stamp = clock::wall_ms(ctx.clock.wall());
    let mut path = ctx.cwd.join(format!("fiber-feedback-{stamp}.md"));
    let mut bump = 2;
    while path.exists() {
        path = ctx.cwd.join(format!("fiber-feedback-{stamp}-{bump}.md"));
        bump += 1;
    }
    if let Err(source) = std::fs::write(&path, report.as_bytes()) {
        // Best-effort: the report was built fully in memory, so a
        // failed write leaves at most a partial file; removing it
        // keeps a rerun from seeing one. A failed removal changes
        // nothing: the door prints the write failure either way.
        drop(std::fs::remove_file(&path));
        return Err(FeedbackError::Write { path, source });
    }
    Ok(contract::feedback::FeedbackDone { path, link })
}
