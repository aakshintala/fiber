//! `fiber feedback`: the one code path both doors call. It captures
//! the production `doctor` report into a buffer, builds the Markdown in
//! memory, writes it once under the current directory, and returns the
//! file path with the new-issue link. A failure removes any partial
//! file, and the report never carries session content: only the version,
//! platform, install method, description and doctor output, then the
//! diagnostics the saved file ends with: the content-free lines of
//! `logs/` and the crash reports' thread and backtrace. The issue link
//! never carries the diagnostics.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use contract::shapes::Failure;
use contract::{ErrorCode, SessionId, clock};
use doors::failure;

/// What the report writer reads: the version text, the directory the
/// report lands in, the clock for the file stamp, and the doctor-output
/// and diagnostics capturers. Tests inject stub capturers; both doors
/// inject the production ones.
pub struct ReportContext<'a> {
    /// The bare version text, as `fiber version` prints after `fiber `.
    pub version: &'a str,
    /// The directory the report file lands in.
    pub cwd: &'a Path,
    /// The clock for the file stamp.
    pub clock: &'a dyn clock::Clock,
    /// The doctor output, or its capture failure.
    pub doctor: &'a dyn Fn() -> Result<String, FeedbackError>,
    /// The diagnostics section for the selected session, or none.
    pub diagnostics: &'a dyn Fn(Option<&SessionId>) -> String,
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

/// How far back a hub line with no session id is still reported.
const HUB_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;

/// The most hub lines the report holds.
const HUB_LINES: usize = 500;

/// The most session lines the report holds.
const SESSION_LINES: usize = 2000;

/// The diagnostic file kinds one session writes, in the order their lines
/// break a tie.
const SESSION_KINDS: [&str; 3] = ["session", "ask", "tui"];

/// The line a crash report puts between the panic message and the
/// backtrace.
const BACKTRACE_MARKER: &str = "\nstack backtrace:\n";

/// One reportable diagnostic line.
struct Entry {
    ts: u64,
    /// An `error` or `warn` line, which the cap keeps before any other.
    priority: bool,
    session: Option<String>,
    /// The line as recorded.
    text: String,
}

/// `line` as an entry, when it is a diagnostic line of `process` with every
/// documented field in its documented shape (`docs/state.md`, "Diagnostic
/// logs"), including object `data` on debug lines. Anything else is left out,
/// as is an `extension_log` line: it is the extension's own text, which Fiber
/// cannot vouch for.
fn entry(line: &str, process: &str) -> Option<Entry> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let fields = value.as_object()?;
    if fields.get("data").is_some_and(|data| !data.is_object()) {
        return None;
    }
    let ts = fields.get("ts")?.as_u64()?;
    let level = fields.get("level")?.as_str()?;
    if !matches!(level, "error" | "warn" | "info" | "debug") {
        return None;
    }
    if level == "debug" && !fields.get("data").is_some_and(serde_json::Value::is_object) {
        return None;
    }
    if fields.get("process")?.as_str()? != process {
        return None;
    }
    let code = fields.get("code")?.as_str()?;
    fields.get("message")?.as_str()?;
    if code == "extension_log" {
        return None;
    }
    let session = match fields.get("session_id") {
        Some(id) => Some(id.as_str()?.to_owned()),
        None => None,
    };
    Some(Entry {
        ts,
        priority: matches!(level, "error" | "warn"),
        session,
        text: line.trim_end().to_owned(),
    })
}

/// Appends the entries of the file at `path`, in line order. A file that
/// cannot be read holds none.
fn read_entries(path: &Path, process: &str, into: &mut Vec<Entry>) {
    let Ok(bytes) = fs::read(path) else {
        return;
    };
    // Per line, not `read_to_string`: a line torn mid-character by a
    // crash must not hide every line before and after it.
    into.extend(bytes.split(|byte| *byte == b'\n').filter_map(|raw| {
        let line = std::str::from_utf8(raw).ok()?;
        entry(line, process)
    }));
}

/// The newest `limit` entries, oldest first, and how many were dropped.
/// Ties in `ts` keep the order the entries came in. Every `error` and
/// `warn` entry is kept before any other, newest first when they alone
/// pass `limit`.
fn newest(mut entries: Vec<Entry>, limit: usize) -> (Vec<Entry>, usize) {
    entries.sort_by_key(|entry| entry.ts);
    let total = entries.len();
    let priority = entries.iter().filter(|entry| entry.priority).count();
    let mut room_priority = limit;
    let mut room_other = limit.saturating_sub(priority);
    let mut kept = Vec::new();
    for entry in entries.into_iter().rev() {
        let room = if entry.priority {
            &mut room_priority
        } else {
            &mut room_other
        };
        if *room > 0 {
            *room -= 1;
            kept.push(entry);
        }
    }
    kept.reverse();
    let dropped = total - kept.len();
    (kept, dropped)
}

/// The hub's reportable lines: `hub.log.1` then `hub.log`, each with a
/// `ts` no later than `now`, and either no session id and no more than a
/// day old, or the selected session's id at any age.
fn hub_entries(logs: &Path, now: u64, session: Option<&SessionId>) -> Vec<Entry> {
    let mut entries = Vec::new();
    for name in ["hub.log.1", "hub.log"] {
        read_entries(&logs.join(name), "hub", &mut entries);
    }
    let oldest = now.saturating_sub(HUB_WINDOW_MS);
    entries.retain(|entry| {
        entry.ts <= now
            && match (entry.session.as_deref(), session) {
                (None, _) => entry.ts >= oldest,
                (Some(recorded), Some(selected)) => recorded == selected.0,
                (Some(_), None) => false,
            }
    });
    entries
}

/// What a crash report keeps: its first line, which names the thread and
/// the source location, and the backtrace after the last backtrace marker.
/// The panic message between them may hold anything, so it never leaves. A
/// report with no marker keeps the first line only.
fn crash_details(text: &str) -> String {
    let first = text.lines().next().unwrap_or("");
    match text.rfind(BACKTRACE_MARKER) {
        Some(at) => format!(
            "{first}\n{}",
            text[at + BACKTRACE_MARKER.len()..].trim_end()
        ),
        None => first.to_owned(),
    }
}

/// The details of every crash report `crashes/<id>-<ms>.txt` for session
/// `id`, oldest first, each with its file name.
fn crash_reports(home: &Path, id: &SessionId) -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir(config::crashes_dir(home)) else {
        return Vec::new();
    };
    let mut found: Vec<(u128, String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let ms = name
                .strip_prefix(id.0.as_str())?
                .strip_prefix('-')?
                .strip_suffix(".txt")?
                .parse()
                .ok()?;
            Some((ms, name, entry.path()))
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .filter_map(|(_, name, path)| {
            let bytes = fs::read(path).ok()?;
            Some((name, crash_details(&String::from_utf8_lossy(&bytes))))
        })
        .collect()
}

/// The session a feedback report carries diagnostics for: `explicit`, which
/// must be a non-delegate session with a log anywhere in Fiber home, or
/// without one the newest session in `workspace`'s project, which may be
/// none.
pub fn select_session(
    home: &Path,
    workspace: &Path,
    explicit: Option<SessionId>,
) -> Result<Option<SessionId>, Failure> {
    let Some(id) = explicit else {
        let project = doors::project(workspace);
        let sessions = log::sessions_dir(home, &project);
        return Ok(log::most_recent(&sessions, &|started| {
            doors::project(Path::new(started)) == project
        }));
    };
    let usable = log::find_logged_session(home, &id)
        .is_some_and(|(_, dir)| log::started_first(&dir).is_ok_and(|started| !started.delegate));
    if usable {
        Ok(Some(id))
    } else {
        Err(failure(
            ErrorCode::SessionNotFound,
            format!("No session `{}`.", id.0),
        ))
    }
}

/// One report section: `title`, then each text in a fence, or `None.`.
fn section(out: &mut String, title: &str, texts: &[String]) {
    out.push_str(&format!("### {title}\n\n"));
    if texts.is_empty() {
        out.push_str("None.\n\n");
    }
    for text in texts {
        out.push_str(&format!("```\n{text}\n```\n\n"));
    }
}

/// The report's Diagnostics section (`docs/user/reporting.md`): the
/// content-free lines of `logs/` and the crash reports' thread and
/// backtrace, never a session's events. With a selected session, its id and
/// how to export its log privately, its `session`, `ask` and `tui` lines,
/// its crash reports and the hub lines naming it; without one, the hub's
/// lines from the last day only. Hub lines are capped at 500 and a
/// session's at 2000, both chosen and not measured; the cap says how many
/// session lines it left out.
pub fn diagnostics(home: &Path, clock: &dyn clock::Clock, session: Option<&SessionId>) -> String {
    let now = clock::wall_ms(clock.wall());
    let logs = config::logs_dir(home);
    let mut out = String::from("## Diagnostics\n\n");
    if let Some(session) = session {
        let id = session.0.as_str();
        out.push_str(&format!(
            "Session: {id}\n\n\
             To share the session's log, run `fiber sessions export {id}` and send the directory privately: it holds your prompts and the model's replies, which this report leaves out.\n\n"
        ));
        let mut lines = Vec::new();
        for kind in SESSION_KINDS {
            read_entries(&logs.join(format!("{kind}-{id}.log")), kind, &mut lines);
        }
        lines.retain(|line| line.session.as_deref() == Some(id));
        if lines.is_empty() {
            out.push_str(&format!(
                "This machine has no diagnostic lines for session {id}.\n\n"
            ));
        }
        let (kept, left_out) = newest(lines, SESSION_LINES);
        if left_out > 0 {
            out.push_str(&format!("{left_out} earlier lines left out\n\n"));
        }
        let texts: Vec<String> = kept.into_iter().map(|line| line.text).collect();
        section(&mut out, "Session log lines", &texts);
        let crashes: Vec<String> = crash_reports(home, session)
            .into_iter()
            .map(|(name, details)| format!("{name}\n{details}"))
            .collect();
        section(&mut out, "Crash reports", &crashes);
    }
    let mut hub = hub_entries(&logs, now, session);
    hub.sort_by_key(|line| line.ts);
    hub.drain(..hub.len().saturating_sub(HUB_LINES));
    let texts: Vec<String> = hub.into_iter().map(|line| line.text).collect();
    section(&mut out, "Hub log lines", &texts);
    out
}

/// Builds the report in memory, writes it once, and returns its path
/// with the issue link. The link carries the report without its
/// Diagnostics, which only the file holds. The file is
/// `fiber-feedback-<ms>.md` in the context's directory, bumped with `-2`,
/// `-3` while the name exists, so a same-millisecond rerun never
/// overwrites. A write failure removes any partial file and writes
/// nothing else.
pub fn write(
    description: &str,
    session: Option<&SessionId>,
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
    let report = format!("{report}\n{}", (ctx.diagnostics)(session));
    let stamp = clock::wall_ms(ctx.clock.wall());
    let mut path = ctx.cwd.join(format!("fiber-feedback-{stamp}.md"));
    for bump in 2.. {
        if !path.exists() {
            break;
        }
        path = ctx.cwd.join(format!("fiber-feedback-{stamp}-{bump}.md"));
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
