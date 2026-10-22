//! Tests for the shared feedback report both doors call: the report
//! text, the install value, the title cut, the link limit, the
//! write-once file with its collision bump, and the Diagnostics section's
//! filtering, windows and caps under a fake clock.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use super::{
    FeedbackError, ReportContext, build_report, crash_details, diagnostics, install_method,
    issue_link, title, write,
};
use contract::clock::{Clock, wall_ms};
use fakes::clock::FakeClock;
use std::fs;
use std::path::Path;

const VERSION: &str = "0.3.0 (abc123)";
const PLATFORM: &str = "linux/x86_64";
const INSTALL: &str = "install.sh";
const DESCRIPTION: &str = "The picker opened empty.";
const DOCTOR: &str = "fiber 0.3.0 (abc123)\ndefault model: none\n";

const DIAGNOSTICS: &str = "## Diagnostics\n\nstub diagnostics\n";

fn stub_diagnostics(_: Option<&str>) -> Result<String, FeedbackError> {
    Ok(DIAGNOSTICS.to_owned())
}

/// A context writing into `cwd` with the stub doctor report and the stub
/// diagnostics.
fn ctx<'a>(
    cwd: &'a Path,
    clock: &'a dyn Clock,
    doctor: &'a dyn Fn() -> Result<String, FeedbackError>,
) -> ReportContext<'a> {
    ctx_with(cwd, clock, doctor, &stub_diagnostics)
}

fn ctx_with<'a>(
    cwd: &'a Path,
    clock: &'a dyn Clock,
    doctor: &'a dyn Fn() -> Result<String, FeedbackError>,
    diagnostics: &'a dyn Fn(Option<&str>) -> Result<String, FeedbackError>,
) -> ReportContext<'a> {
    ReportContext {
        version: VERSION,
        cwd,
        clock,
        doctor,
        diagnostics,
    }
}

#[test]
fn the_report_pins_every_section_in_order() {
    assert_eq!(
        build_report(VERSION, PLATFORM, INSTALL, DESCRIPTION, DOCTOR),
        "# Fiber feedback\n\
         \n\
         - Version: fiber 0.3.0 (abc123)\n\
         - Platform: linux/x86_64\n\
         - Install: install.sh\n\
         \n\
         ## What happened\n\
         \n\
         The picker opened empty.\n\
         \n\
         ## Doctor\n\
         \n\
         fiber 0.3.0 (abc123)\n\
         default model: none\n"
    );
}

#[test]
fn the_doctor_output_stays_verbatim() {
    let doctor = "fiber 0.3.0 (abc123)\nnewest error: x, 1s old, usage: No.\n";
    let report = build_report(VERSION, PLATFORM, INSTALL, DESCRIPTION, doctor);
    assert!(
        report
            .ends_with("## Doctor\n\nfiber 0.3.0 (abc123)\nnewest error: x, 1s old, usage: No.\n")
    );
}

#[test]
fn install_is_install_sh_with_a_commit_and_unknown_without() {
    assert_eq!(install_method("0.3.0 (abc123)"), "install.sh");
    assert_eq!(install_method("0.3.0"), "unknown");
}

#[test]
fn the_title_takes_the_description_first_line_only() {
    assert_eq!(title("first line\nsecond line"), "Feedback: first line");
    assert_eq!(title(""), "Feedback: ");
}

#[test]
fn the_title_keeps_120_chars_and_cuts_121() {
    let at = "a".repeat(110);
    let kept = title(&at);
    assert_eq!(kept.chars().count(), 120);
    assert_eq!(kept, format!("Feedback: {at}"));
    let over = "a".repeat(111);
    let cut = title(&over);
    assert_eq!(cut.chars().count(), 120);
    assert_eq!(cut, format!("Feedback: {}", "a".repeat(110)));
}

#[test]
fn the_title_cut_lands_on_a_character_boundary() {
    let over = "é".repeat(115);
    let cut = title(&over);
    assert_eq!(cut.chars().count(), 120);
    assert_eq!(cut, format!("Feedback: {}", "é".repeat(110)));
}

/// The full issue link for a description of `desc_len` `a`s and a doctor
/// report of `doctor_len` `d`s.
fn full_url(desc_len: usize, doctor_len: usize) -> String {
    let description = "a".repeat(desc_len);
    let doctor = "d".repeat(doctor_len);
    let report = build_report(VERSION, PLATFORM, INSTALL, &description, &doctor);
    issue_link(&title(&description), &report, &description)
}

/// The overhead every full link carries: the URL, the encoded title
/// prefix and the two description copies beyond their own lengths.
fn overhead() -> usize {
    full_url(0, 0).len()
}

/// The doctor length that lands the full link on exactly `target` chars
/// with a 100-char description.
fn doctor_len_for(target: usize) -> usize {
    target - overhead() - 2 * 100
}

/// Decodes one `%XX` query component.
fn decode(component: &str) -> String {
    let mut bytes = Vec::new();
    let raw = component.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            bytes.push(u8::from_str_radix(&component[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).unwrap()
}

/// The title and body the link carries, decoded.
fn linked(url: &str) -> (String, String) {
    let query = url.split("?title=").nth(1).unwrap();
    let (title, body) = query.split_once("&body=").unwrap();
    (decode(title), decode(body))
}

#[test]
fn the_link_carries_the_full_report_at_1999_and_2000_chars() {
    for target in [1999, 2000] {
        let description = "a".repeat(100);
        let doctor = "d".repeat(doctor_len_for(target));
        let report = build_report(VERSION, PLATFORM, INSTALL, &description, &doctor);
        let url = issue_link(&title(&description), &report, &description);
        assert_eq!(url.len(), target, "{target}");
        assert!(url.starts_with("https://github.com/aakshintala/fiber/issues/new?title="));
        let (linked_title, linked_body) = linked(&url);
        assert_eq!(linked_title, title(&description));
        assert_eq!(linked_body, report);
    }
}

#[test]
fn the_link_falls_back_past_2000_chars_with_the_title_kept() {
    let description = "a".repeat(100);
    let doctor = "d".repeat(doctor_len_for(2001));
    let report = build_report(VERSION, PLATFORM, INSTALL, &description, &doctor);
    // The would-be full link is exactly 2001 chars, so the fallback
    // fires: the body is no longer the report.
    let full = format!(
        "https://github.com/aakshintala/fiber/issues/new?title={}&body={}",
        super::encode(&title(&description)),
        super::encode(&report)
    );
    assert_eq!(full.len(), 2001);
    let url = issue_link(&title(&description), &report, &description);
    let (linked_title, linked_body) = linked(&url);
    assert_eq!(linked_title, title(&description));
    assert_ne!(linked_body, report);
    assert!(linked_body.starts_with(&description));
    assert!(linked_body.contains("attach the report file"));
}

#[test]
fn non_ascii_and_newlines_round_trip_through_the_link() {
    let description = "héllo wörld\nsecond line";
    let report = build_report(VERSION, PLATFORM, INSTALL, description, DOCTOR);
    let url = issue_link(&title(description), &report, description);
    assert!(url.len() <= 2000);
    let (linked_title, linked_body) = linked(&url);
    assert_eq!(linked_title, "Feedback: héllo wörld");
    assert_eq!(linked_body, report);
}

#[test]
fn write_writes_the_report_once_and_returns_its_path_and_link() {
    let root = fakes::TempDir::new("fiber-feedback-write");
    let clock = FakeClock::new();
    let doctor = || Ok(DOCTOR.to_owned());
    let done = write(
        DESCRIPTION,
        None,
        &ctx(root.path(), clock.as_ref(), &doctor),
    )
    .unwrap();
    let expected = root
        .path()
        .join(format!("fiber-feedback-{}.md", wall_ms(clock.wall())));
    assert_eq!(done.path, expected);
    assert_eq!(
        fs::read_to_string(&expected).unwrap(),
        format!(
            "{}\n{DIAGNOSTICS}",
            build_report(
                VERSION,
                &super::platform(),
                install_method(VERSION),
                DESCRIPTION,
                DOCTOR
            )
        )
    );
    assert!(
        done.link
            .starts_with("https://github.com/aakshintala/fiber/issues/new?title=")
    );
    let (_, linked_body) = linked(&done.link);
    assert!(linked_body.contains(DESCRIPTION));
}

#[test]
fn write_bumps_the_name_when_the_millisecond_collides() {
    let root = fakes::TempDir::new("fiber-feedback-collision");
    let clock = FakeClock::new();
    let doctor = || Ok(DOCTOR.to_owned());
    let stamp = wall_ms(clock.wall());
    fs::write(
        root.path().join(format!("fiber-feedback-{stamp}.md")),
        "old",
    )
    .unwrap();
    let second = write(
        DESCRIPTION,
        None,
        &ctx(root.path(), clock.as_ref(), &doctor),
    )
    .unwrap();
    assert_eq!(
        second.path,
        root.path().join(format!("fiber-feedback-{stamp}-2.md"))
    );
    let third = write(
        DESCRIPTION,
        None,
        &ctx(root.path(), clock.as_ref(), &doctor),
    )
    .unwrap();
    assert_eq!(
        third.path,
        root.path().join(format!("fiber-feedback-{stamp}-3.md"))
    );
    assert_eq!(
        fs::read_to_string(root.path().join(format!("fiber-feedback-{stamp}.md"))).unwrap(),
        "old"
    );
}

#[test]
fn write_into_a_file_leaves_no_report_behind() {
    let root = fakes::TempDir::new("fiber-feedback-unwritable");
    let not_a_dir = root.path().join("file");
    fs::write(&not_a_dir, "x").unwrap();
    let clock = FakeClock::new();
    let doctor = || Ok(DOCTOR.to_owned());
    let error = write(DESCRIPTION, None, &ctx(&not_a_dir, clock.as_ref(), &doctor)).unwrap_err();
    assert!(matches!(error, FeedbackError::Write { .. }));
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        [std::ffi::OsString::from("file")]
    );
}

#[test]
fn an_empty_description_is_a_usage_error() {
    for description in ["", "   ", "\n  \n"] {
        let root = fakes::TempDir::new("fiber-feedback-empty");
        let clock = FakeClock::new();
        let doctor = || Ok(DOCTOR.to_owned());
        let error = write(
            description,
            None,
            &ctx(root.path(), clock.as_ref(), &doctor),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "No description. Run `fiber feedback \"<description>\"` or `fiber feedback < <file>`."
        );
        assert!(fs::read_dir(root.path()).unwrap().next().is_none());
    }
}

#[test]
fn a_doctor_failure_writes_nothing() {
    let root = fakes::TempDir::new("fiber-feedback-doctor-fails");
    let clock = FakeClock::new();
    let doctor = || Err(FeedbackError::Doctor("closed stdout".to_owned()));
    let error = write(
        DESCRIPTION,
        None,
        &ctx(root.path(), clock.as_ref(), &doctor),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "could not capture the `fiber doctor` output: closed stdout"
    );
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

#[test]
fn platform_names_the_os_and_arch() {
    assert_eq!(
        super::platform(),
        format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH)
    );
}

/// `has_userinfo` for the capture test: what a `@` in the URL means.
struct Remote;

impl contract::McpRemote for Remote {
    fn has_userinfo(&self, url: &str) -> bool {
        url.contains('@')
    }

    fn token_key(&self, _url: &str) -> Result<String, contract::shapes::Failure> {
        unreachable!("feedback capture reads no MCP token")
    }

    fn login(
        &self,
        _files: &dyn contract::mcp_tokens::TokenFiles,
        _url: &str,
        _present: &dyn Fn(&str),
        _clock: &dyn contract::clock::Clock,
    ) -> Result<(), contract::shapes::Failure> {
        unreachable!("feedback capture logs in to no MCP server")
    }
}

#[test]
fn capture_returns_the_printed_report_with_whether_a_session_can_start() {
    let root = fakes::TempDir::new("fiber-feedback-capture");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    let clock = FakeClock::new();
    let report =
        super::super::doctor::capture(&home, &workspace, VERSION, clock.as_ref(), &Remote).unwrap();
    assert_eq!(report.lines().next(), Some("fiber 0.3.0 (abc123)"));
    assert!(report.contains("hub: not running"));
}

#[test]
fn the_file_ends_with_diagnostics_and_the_link_never_carries_them() {
    let root = fakes::TempDir::new("fiber-feedback-link");
    let clock = FakeClock::new();
    let doctor = || Ok(DOCTOR.to_owned());
    let done = write(
        DESCRIPTION,
        None,
        &ctx(root.path(), clock.as_ref(), &doctor),
    )
    .unwrap();
    let saved = fs::read_to_string(&done.path).unwrap();
    assert!(saved.ends_with(&format!("\n{DIAGNOSTICS}")), "{saved}");
    let (_, body) = linked(&done.link);
    assert!(!body.contains("stub diagnostics"), "{body}");
    assert!(!body.contains("## Diagnostics"), "{body}");

    // A report too long for the link falls back to the description, and
    // that form carries no diagnostics either.
    let long = || Ok("d".repeat(3000));
    let done = write(DESCRIPTION, None, &ctx(root.path(), clock.as_ref(), &long)).unwrap();
    let (_, body) = linked(&done.link);
    assert!(body.contains("attach the report file"), "{body}");
    assert!(!body.contains("stub diagnostics"), "{body}");
    assert!(
        fs::read_to_string(&done.path)
            .unwrap()
            .contains("stub diagnostics")
    );
}

#[test]
fn write_hands_the_capturer_the_selected_session() {
    let root = fakes::TempDir::new("fiber-feedback-session");
    let clock = FakeClock::new();
    let doctor = || Ok(DOCTOR.to_owned());
    let seen = std::cell::RefCell::new(Vec::new());
    let capture = |session: Option<&str>| {
        seen.borrow_mut().push(session.map(str::to_owned));
        Ok(DIAGNOSTICS.to_owned())
    };
    let context = ctx_with(root.path(), clock.as_ref(), &doctor, &capture);
    write(DESCRIPTION, Some("s_0123456789abcdef"), &context).unwrap();
    write(DESCRIPTION, None, &context).unwrap();
    assert_eq!(
        *seen.borrow(),
        [Some("s_0123456789abcdef".to_owned()), None]
    );
}

#[test]
fn a_diagnostics_failure_writes_nothing() {
    let root = fakes::TempDir::new("fiber-feedback-diagnostics-fails");
    let clock = FakeClock::new();
    let doctor = || Ok(DOCTOR.to_owned());
    let capture = |_: Option<&str>| Err(FeedbackError::Diagnostics("closed disk".to_owned()));
    let error = write(
        DESCRIPTION,
        None,
        &ctx_with(root.path(), clock.as_ref(), &doctor, &capture),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "could not capture the diagnostics: closed disk"
    );
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

const SESSION: &str = "s_0123456789abcdef";
const OTHER: &str = "s_fedcba9876543210";
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Hub, session and crash files the report reads, for a clock at
/// [`now`].
struct Home(fakes::TempDir);

fn now() -> u64 {
    wall_ms(FakeClock::new().wall())
}

impl Home {
    fn new() -> Self {
        Self(fakes::TempDir::new("fiber-feedback-diagnostics"))
    }

    /// Writes `lines` to `relative` under the home, one per line.
    fn put(&self, relative: &str, lines: &[String]) {
        let path = self.0.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    fn report(&self, session: Option<&str>) -> String {
        diagnostics(self.0.path(), FakeClock::new().as_ref(), session)
    }
}

/// One recorded line of `process`; its message is `tag`, so a test can
/// tell lines apart.
fn line(ts: u64, level: &str, process: &str, session: Option<&str>, tag: &str) -> String {
    let mut fields = serde_json::json!({
        "ts": ts, "level": level, "process": process, "code": "some_code", "message": tag,
    });
    if let Some(session) = session {
        fields["session_id"] = session.into();
    }
    fields.to_string()
}

fn hub(ts: u64, session: Option<&str>, tag: &str) -> String {
    line(ts, "info", "hub", session, tag)
}

/// The messages of the lines under `### title`, in the order printed.
fn messages(report: &str, title: &str) -> Vec<String> {
    let body = report
        .split(&format!("### {title}\n\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("no `{title}` section: {report}"));
    let body = body.split("\n### ").next().unwrap();
    body.lines()
        .filter(|text| text.starts_with('{'))
        .map(|text| {
            let value: serde_json::Value = serde_json::from_str(text).unwrap();
            value["message"].as_str().unwrap().to_owned()
        })
        .collect()
}

#[test]
fn a_session_report_names_the_session_and_how_to_export_it_privately() {
    let home = Home::new();
    home.put(
        &format!("logs/session-{SESSION}.log"),
        &[line(5, "info", "session", Some(SESSION), "kept")],
    );
    let report = home.report(Some(SESSION));
    assert!(report.starts_with("## Diagnostics\n\n"), "{report}");
    assert!(
        report.contains(&format!("Session: {SESSION}\n")),
        "{report}"
    );
    assert!(
        report.contains(&format!("`fiber sessions export {SESSION}`")),
        "{report}"
    );
    assert!(report.contains("privately"), "{report}");
    assert!(!report.contains("no diagnostic lines"), "{report}");
    assert_eq!(messages(&report, "Session log lines"), ["kept"]);
}

#[test]
fn a_session_with_no_local_lines_says_so_and_keeps_the_hub_lines() {
    let home = Home::new();
    home.put("logs/hub.log", &[hub(now(), None, "hub-line")]);
    let report = home.report(Some(SESSION));
    assert!(
        report.contains(&format!(
            "This machine has no diagnostic lines for session {SESSION}.\n"
        )),
        "{report}"
    );
    assert!(
        report.contains(&format!("Session: {SESSION}\n")),
        "{report}"
    );
    assert_eq!(messages(&report, "Hub log lines"), ["hub-line"]);
    assert!(messages(&report, "Session log lines").is_empty());
    // A session whose only lines are left out has none to report either.
    home.put(
        &format!("logs/session-{SESSION}.log"),
        &[line(5, "info", "session", Some(SESSION), "x").replace("some_code", "extension_log")],
    );
    assert!(home.report(Some(SESSION)).contains("no diagnostic lines"));
}

#[test]
fn without_a_session_the_report_holds_hub_lines_only() {
    let home = Home::new();
    home.put("logs/hub.log", &[hub(now(), None, "hub-line")]);
    home.put(
        &format!("logs/session-{SESSION}.log"),
        &[line(5, "info", "session", Some(SESSION), "session-line")],
    );
    home.put(
        &format!("crashes/{SESSION}-5.txt"),
        &["thread 'main' panicked at src/x.rs:1:1:".to_owned()],
    );
    let report = home.report(None);
    assert!(!report.contains("Session:"), "{report}");
    assert!(!report.contains("session-line"), "{report}");
    assert!(!report.contains("Crash reports"), "{report}");
    assert!(!report.contains("fiber sessions export"), "{report}");
    assert_eq!(messages(&report, "Hub log lines"), ["hub-line"]);
}

#[test]
fn nothing_to_report_prints_none_for_each_section() {
    let home = Home::new();
    let report = home.report(Some(SESSION));
    assert_eq!(report.matches("None.\n").count(), 3, "{report}");
}

#[test]
fn a_session_line_needs_every_documented_field_and_the_selected_id() {
    let home = Home::new();
    let good = line(10, "info", "session", Some(SESSION), "good");
    let extension = good.replace("some_code", "extension_log");
    let wrong_level = line(10, "fatal", "session", Some(SESSION), "level");
    let wrong_process = line(10, "info", "ask", Some(SESSION), "process");
    let other = line(10, "info", "session", Some(OTHER), "other");
    let no_id = line(10, "info", "session", None, "no-id");
    let float_ts = r#"{"ts":10.5,"level":"info","process":"session","session_id":"s_0123456789abcdef","code":"c","message":"float"}"#;
    let text_ts = r#"{"ts":"10","level":"info","process":"session","session_id":"s_0123456789abcdef","code":"c","message":"text"}"#;
    let negative_ts = r#"{"ts":-1,"level":"info","process":"session","session_id":"s_0123456789abcdef","code":"c","message":"negative"}"#;
    let no_ts = r#"{"level":"info","process":"session","session_id":"s_0123456789abcdef","code":"c","message":"no-ts"}"#;
    let no_level = r#"{"ts":10,"process":"session","session_id":"s_0123456789abcdef","code":"c","message":"no-level"}"#;
    let no_process = r#"{"ts":10,"level":"info","session_id":"s_0123456789abcdef","code":"c","message":"no-process"}"#;
    let no_code = r#"{"ts":10,"level":"info","process":"session","session_id":"s_0123456789abcdef","message":"no-code"}"#;
    let numeric_code = r#"{"ts":10,"level":"info","process":"session","session_id":"s_0123456789abcdef","code":7,"message":"numeric-code"}"#;
    let no_message = r#"{"ts":10,"level":"info","process":"session","session_id":"s_0123456789abcdef","code":"c"}"#;
    let numeric_message = r#"{"ts":10,"level":"info","process":"session","session_id":"s_0123456789abcdef","code":"c","message":7}"#;
    let numeric_id = r#"{"ts":10,"level":"info","process":"session","session_id":7,"code":"c","message":"numeric-id"}"#;
    home.put(
        &format!("logs/session-{SESSION}.log"),
        &[
            good,
            extension,
            wrong_level,
            wrong_process,
            other,
            no_id,
            float_ts.to_owned(),
            text_ts.to_owned(),
            negative_ts.to_owned(),
            no_ts.to_owned(),
            no_level.to_owned(),
            no_process.to_owned(),
            no_code.to_owned(),
            numeric_code.to_owned(),
            no_message.to_owned(),
            numeric_message.to_owned(),
            numeric_id.to_owned(),
            "not json".to_owned(),
            "[1,2]".to_owned(),
            String::new(),
        ],
    );
    let report = home.report(Some(SESSION));
    assert_eq!(messages(&report, "Session log lines"), ["good"]);
}

#[test]
fn every_level_the_docs_name_is_reported() {
    let home = Home::new();
    let lines: Vec<String> = ["error", "warn", "info", "debug"]
        .iter()
        .enumerate()
        .map(|(at, level)| line(at as u64, level, "tui", Some(SESSION), level))
        .collect();
    home.put(&format!("logs/tui-{SESSION}.log"), &lines);
    assert_eq!(
        messages(&home.report(Some(SESSION)), "Session log lines"),
        ["error", "warn", "info", "debug"]
    );
}

#[test]
fn session_lines_merge_by_time_and_break_ties_by_file_then_line() {
    let home = Home::new();
    home.put(
        &format!("logs/tui-{SESSION}.log"),
        &[
            line(5, "info", "tui", Some(SESSION), "tui-5"),
            line(5, "info", "tui", Some(SESSION), "tui-5b"),
        ],
    );
    home.put(
        &format!("logs/ask-{SESSION}.log"),
        &[line(5, "info", "ask", Some(SESSION), "ask-5")],
    );
    home.put(
        &format!("logs/session-{SESSION}.log"),
        &[
            line(9, "info", "session", Some(SESSION), "session-9"),
            line(5, "info", "session", Some(SESSION), "session-5"),
            line(1, "info", "session", Some(SESSION), "session-1"),
        ],
    );
    assert_eq!(
        messages(&home.report(Some(SESSION)), "Session log lines"),
        [
            "session-1",
            "session-5",
            "ask-5",
            "tui-5",
            "tui-5b",
            "session-9"
        ]
    );
}

/// `count` info lines in the session file at `1..=count`.
fn numbered(count: u64) -> Vec<String> {
    (1..=count)
        .map(|at| line(at, "info", "session", Some(SESSION), &at.to_string()))
        .collect()
}

#[test]
fn the_session_cap_holds_2000_lines_and_counts_the_rest() {
    for (count, left_out) in [(1999_u64, 0_u64), (2000, 0), (2001, 1)] {
        let home = Home::new();
        home.put(&format!("logs/session-{SESSION}.log"), &numbered(count));
        let report = home.report(Some(SESSION));
        let kept = messages(&report, "Session log lines");
        assert_eq!(kept.len() as u64, count - left_out, "{count}");
        assert_eq!(kept.last().unwrap(), &count.to_string());
        assert_eq!(kept[0], (left_out + 1).to_string(), "{count}");
        if left_out == 0 {
            assert!(!report.contains("earlier lines left out"), "{count}");
        } else {
            assert!(
                report.contains(&format!("{left_out} earlier lines left out\n")),
                "{report}"
            );
        }
    }
}

#[test]
fn the_session_cap_keeps_every_error_and_warning_before_newer_lines() {
    let home = Home::new();
    let mut lines = vec![
        line(1, "error", "session", Some(SESSION), "old-error"),
        line(2, "warn", "session", Some(SESSION), "old-warn"),
    ];
    lines.extend((100..2100).map(|at| line(at, "info", "session", Some(SESSION), &at.to_string())));
    home.put(&format!("logs/session-{SESSION}.log"), &lines);
    let report = home.report(Some(SESSION));
    let kept = messages(&report, "Session log lines");
    assert_eq!(kept.len(), 2000);
    assert_eq!(&kept[..3], ["old-error", "old-warn", "102"]);
    assert_eq!(kept.last().unwrap(), "2099");
    assert!(report.contains("2 earlier lines left out\n"), "{report}");
}

#[test]
fn errors_and_warnings_over_the_cap_keep_their_newest_2000() {
    let home = Home::new();
    let mut lines: Vec<String> = (1..=2001)
        .map(|at| {
            let level = if at % 2 == 0 { "error" } else { "warn" };
            line(at, level, "session", Some(SESSION), &at.to_string())
        })
        .collect();
    lines.extend((5000..5005).map(|at| line(at, "info", "session", Some(SESSION), "newer-info")));
    home.put(&format!("logs/session-{SESSION}.log"), &lines);
    let report = home.report(Some(SESSION));
    let kept = messages(&report, "Session log lines");
    assert_eq!(kept.len(), 2000);
    assert_eq!(kept[0], "2");
    assert_eq!(kept.last().unwrap(), "2001");
    assert!(!kept.contains(&"newer-info".to_owned()));
    assert!(report.contains("6 earlier lines left out\n"), "{report}");
}

#[test]
fn the_session_cap_breaks_a_tie_at_the_edge_by_file_order() {
    let home = Home::new();
    let same = |process: &str, tag: &str| line(7, "info", process, Some(SESSION), tag);
    home.put(
        &format!("logs/session-{SESSION}.log"),
        &[same("session", "first")],
    );
    home.put(
        &format!("logs/ask-{SESSION}.log"),
        &(0..1000)
            .map(|_| same("ask", "ask"))
            .collect::<Vec<String>>(),
    );
    home.put(
        &format!("logs/tui-{SESSION}.log"),
        &(0..1000)
            .map(|_| same("tui", "tui"))
            .collect::<Vec<String>>(),
    );
    let report = home.report(Some(SESSION));
    let kept = messages(&report, "Session log lines");
    assert_eq!(kept.len(), 2000);
    assert!(!kept.contains(&"first".to_owned()));
    assert!(report.contains("1 earlier lines left out\n"), "{report}");
}

#[test]
fn the_omission_count_ignores_lines_that_were_never_reportable() {
    let home = Home::new();
    let mut lines = numbered(2001);
    lines.extend((0..50).map(|_| "garbage".to_owned()));
    lines.extend((0..50).map(|at| {
        line(at, "info", "session", Some(SESSION), "x").replace("some_code", "extension_log")
    }));
    home.put(&format!("logs/session-{SESSION}.log"), &lines);
    let report = home.report(Some(SESSION));
    assert!(report.contains("1 earlier lines left out\n"), "{report}");
}

#[test]
fn the_hub_window_is_inclusive_at_a_day_old_and_ends_at_now() {
    let home = Home::new();
    let now = now();
    home.put(
        "logs/hub.log",
        &[
            hub(now - DAY_MS - 1, None, "too-old"),
            hub(now - DAY_MS, None, "a-day-old"),
            hub(now, None, "now"),
            hub(now + 1, None, "future"),
        ],
    );
    assert_eq!(
        messages(&home.report(None), "Hub log lines"),
        ["a-day-old", "now"]
    );
}

#[test]
fn a_hub_line_naming_the_session_skips_the_age_window_but_not_the_clock() {
    let home = Home::new();
    let now = now();
    home.put(
        "logs/hub.log",
        &[
            hub(1, Some(SESSION), "ancient-selected"),
            hub(now, Some(SESSION), "now-selected"),
            hub(now + 1, Some(SESSION), "future-selected"),
            hub(now, Some(OTHER), "other-session"),
            hub(1, None, "ancient-hub"),
            hub(now, None, "now-hub"),
        ],
    );
    assert_eq!(
        messages(&home.report(Some(SESSION)), "Hub log lines"),
        ["ancient-selected", "now-selected", "now-hub"]
    );
    // With no session selected, a line naming one is another session's.
    assert_eq!(messages(&home.report(None), "Hub log lines"), ["now-hub"]);
}

#[test]
fn a_hub_line_needs_every_documented_field() {
    let home = Home::new();
    let now = now();
    let good = hub(now, None, "good");
    home.put(
        "logs/hub.log",
        &[
            good.clone(),
            line(now, "info", "session", None, "wrong-process"),
            good.replace("some_code", "extension_log"),
            format!(
                r#"{{"ts":{now}.5,"level":"info","process":"hub","code":"c","message":"float"}}"#
            ),
            r#"{"level":"info","process":"hub","code":"c","message":"no-ts"}"#.to_owned(),
            "{torn".to_owned(),
        ],
    );
    assert_eq!(messages(&home.report(None), "Hub log lines"), ["good"]);
}

#[test]
fn the_hub_stream_reads_the_rotated_file_first_and_breaks_ties_in_stream_order() {
    let home = Home::new();
    let now = now();
    home.put(
        "logs/hub.log.1",
        &[
            hub(now - 10, None, "rotated-a"),
            hub(now - 5, None, "rotated-b"),
        ],
    );
    home.put(
        "logs/hub.log",
        &[
            hub(now - 5, None, "current-a"),
            hub(now - 20, None, "current-old"),
        ],
    );
    assert_eq!(
        messages(&home.report(None), "Hub log lines"),
        ["current-old", "rotated-a", "rotated-b", "current-a"]
    );
}

#[test]
fn the_hub_cap_holds_500_lines_and_drops_the_oldest_whatever_their_level() {
    for (count, kept) in [(499_u64, 499_usize), (500, 500), (501, 500)] {
        let home = Home::new();
        let now = now();
        let mut lines = vec![line(now - count, "error", "hub", None, "oldest-error")];
        lines.extend((1..count).map(|at| hub(now - count + at, None, &at.to_string())));
        home.put("logs/hub.log", &lines);
        let report = home.report(None);
        let found = messages(&report, "Hub log lines");
        assert_eq!(found.len(), kept, "{count}");
        assert_eq!(
            found.contains(&"oldest-error".to_owned()),
            count <= 500,
            "{count}"
        );
        assert_eq!(found.last().unwrap(), &(count - 1).to_string());
        assert!(!report.contains("earlier lines left out"), "{count}");
    }
}

#[test]
fn the_hub_cap_breaks_a_tie_at_the_edge_by_stream_order() {
    let home = Home::new();
    let now = now();
    home.put("logs/hub.log.1", &[hub(now, None, "rotated")]);
    home.put(
        "logs/hub.log",
        &(0..500)
            .map(|_| hub(now, None, "current"))
            .collect::<Vec<String>>(),
    );
    let found = messages(&home.report(None), "Hub log lines");
    assert_eq!(found.len(), 500);
    assert!(!found.contains(&"rotated".to_owned()));
}

#[test]
fn a_crash_report_keeps_the_thread_line_and_the_backtrace_only() {
    let report = "thread 'worker' panicked at crates/x/src/y.rs:3:9:\n\
                  the secret prompt\nover two lines\n\
                  stack backtrace:\n   0: fiber::main\n   1: std::rt\n";
    assert_eq!(
        crash_details(report),
        "thread 'worker' panicked at crates/x/src/y.rs:3:9:\n   0: fiber::main\n   1: std::rt"
    );
}

#[test]
fn a_panic_message_holding_the_marker_never_leaks_past_the_last_one() {
    let report = "thread 'main' panicked at a.rs:1:1:\n\
                  hunter2\nstack backtrace:\nmore hunter2\n\
                  stack backtrace:\n   0: real\n";
    let kept = crash_details(report);
    assert_eq!(kept, "thread 'main' panicked at a.rs:1:1:\n   0: real");
    assert!(!kept.contains("hunter2"));
}

#[test]
fn a_crash_report_with_no_marker_keeps_its_first_line_only() {
    assert_eq!(
        crash_details("thread 'main' panicked at a.rs:1:1:\nhunter2\nno marker here\n"),
        "thread 'main' panicked at a.rs:1:1:"
    );
    assert_eq!(crash_details(""), "");
}

#[test]
fn every_crash_report_of_the_session_is_listed_oldest_first_with_no_cap() {
    let home = Home::new();
    let crash = |tag: &str| {
        vec![
            "thread 'main' panicked at a.rs:1:1:".to_owned(),
            "hunter2".to_owned(),
            "stack backtrace:".to_owned(),
            format!("   0: {tag}"),
        ]
    };
    home.put(&format!("crashes/{SESSION}-100.txt"), &crash("hundred"));
    home.put(&format!("crashes/{SESSION}-20.txt"), &crash("twenty"));
    for at in 200..260 {
        home.put(&format!("crashes/{SESSION}-{at}.txt"), &crash("many"));
    }
    home.put(&format!("crashes/{OTHER}-1.txt"), &crash("other"));
    home.put("crashes/4242-1.txt", &crash("pid"));
    home.put(&format!("crashes/{SESSION}-x.txt"), &crash("not-ms"));
    home.put(&format!("crashes/{SESSION}-1.log"), &crash("not-txt"));
    home.put(&format!("crashes/{SESSION}1-1.txt"), &crash("longer-id"));
    let report = home.report(Some(SESSION));
    let section = report.split("### Crash reports\n\n").nth(1).unwrap();
    let section = section.split("\n### ").next().unwrap();
    assert!(
        section.starts_with(&format!(
            "```\n{SESSION}-20.txt\nthread 'main' panicked at a.rs:1:1:\n   0: twenty\n```\n\n```\n{SESSION}-100.txt\n"
        )),
        "{section}"
    );
    assert_eq!(section.matches("0: many").count(), 60, "{section}");
    for left_out in ["hunter2", "other", "pid", "not-ms", "not-txt", "longer-id"] {
        assert!(!report.contains(left_out), "{left_out}: {report}");
    }
}
