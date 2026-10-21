//! Tests for the shared feedback report both doors call: the report
//! text, the install value, the title cut, the link limit, and the
//! write-once file with its collision bump.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use super::{FeedbackError, ReportContext, build_report, install_method, issue_link, title, write};
use contract::clock::{Clock, wall_ms};
use fakes::clock::FakeClock;
use std::fs;
use std::path::Path;

const VERSION: &str = "0.3.0 (abc123)";
const PLATFORM: &str = "linux/x86_64";
const INSTALL: &str = "install.sh";
const DESCRIPTION: &str = "The picker opened empty.";
const DOCTOR: &str = "fiber 0.3.0 (abc123)\ndefault model: none\n";

/// A context writing into `cwd` with the stub doctor report.
fn ctx<'a>(
    cwd: &'a Path,
    clock: &'a dyn Clock,
    doctor: &'a dyn Fn() -> Result<String, FeedbackError>,
) -> ReportContext<'a> {
    ReportContext {
        version: VERSION,
        cwd,
        clock,
        doctor,
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
    let mut chars = component.as_bytes().iter();
    while let Some(&byte) = chars.next() {
        if byte == b'%' {
            let hi = chars.next().copied().unwrap_or(b'0');
            let lo = chars.next().copied().unwrap_or(b'0');
            let hex = |digit: u8| {
                u8::try_from(
                    char::from_u32(u32::from(digit))
                        .unwrap()
                        .to_digit(16)
                        .unwrap_or(0),
                )
                .unwrap_or(0)
            };
            bytes.push(hex(hi) * 16 + hex(lo));
        } else {
            bytes.push(byte);
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
    let done = write(DESCRIPTION, &ctx(root.path(), clock.as_ref(), &doctor)).unwrap();
    let expected = root
        .path()
        .join(format!("fiber-feedback-{}.md", wall_ms(clock.wall())));
    assert_eq!(done.path, expected);
    assert_eq!(
        fs::read_to_string(&expected).unwrap(),
        build_report(
            VERSION,
            &super::platform(),
            install_method(VERSION),
            DESCRIPTION,
            DOCTOR
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
    let second = write(DESCRIPTION, &ctx(root.path(), clock.as_ref(), &doctor)).unwrap();
    assert_eq!(
        second.path,
        root.path().join(format!("fiber-feedback-{stamp}-2.md"))
    );
    let third = write(DESCRIPTION, &ctx(root.path(), clock.as_ref(), &doctor)).unwrap();
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
    let error = write(DESCRIPTION, &ctx(&not_a_dir, clock.as_ref(), &doctor)).unwrap_err();
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
        let error = write(description, &ctx(root.path(), clock.as_ref(), &doctor)).unwrap_err();
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
    let error = write(DESCRIPTION, &ctx(root.path(), clock.as_ref(), &doctor)).unwrap_err();
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
    let (report, can_start) =
        super::super::doctor::capture(&home, &workspace, VERSION, clock.as_ref(), &Remote).unwrap();
    assert_eq!(report.lines().next(), Some("fiber 0.3.0 (abc123)"));
    assert!(report.contains("hub: not running"));
    assert!(!can_start);
}
