//! Tests for the import's reader-thread bounds: directory enumeration
//! stops at 512 kept entries, files over 1 MiB are left unread, and a
//! source's reads stop at 8 MiB, each excess named in left-behind.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failure is the test's"
)]

use std::path::PathBuf;
use std::sync::Arc;

use fakes::TempDir;
use fakes::clock::FakeClock;

use super::*;

/// One Fiber home beside one source home.
struct Fixture {
    tmp: TempDir,
    clock: Arc<FakeClock>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            tmp: TempDir::new("fiber-import-bounds"),
            clock: FakeClock::new(),
        }
    }

    /// Writes `content` to `rel` under the source home, creating parents.
    fn write_source(&self, rel: &str, content: &[u8]) {
        let path = self.source().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn source(&self) -> PathBuf {
        self.tmp.path().join("source")
    }

    /// Runs the import for `source` against this fixture.
    fn run(&self, source: ImportSource) -> Result<ImportReport, ConfigError> {
        import_source(&self.home(), &self.source(), source, self.clock.as_ref())
    }
}

/// `size` filler bytes as a skill body: the content never matters to the
/// bounds, only the length.
fn skill_body(size: usize) -> Vec<u8> {
    vec![b'x'; size]
}

/// The left-behind reasons the bounds report.
fn reasons(report: &ImportReport) -> Vec<(&str, &str)> {
    report
        .left_behind
        .iter()
        .map(|left| (left.item.as_str(), left.reason.as_str()))
        .collect()
}

#[test]
fn six_hundred_skill_dirs_import_the_first_512_and_report_more_than_512() {
    let fixture = Fixture::new();
    for index in 0..600 {
        fixture.write_source(
            &format!("skills/skill-{index:03}/SKILL.md"),
            b"---\nname: x\ndescription: y.\n---\n\nBody.\n",
        );
    }
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills.len(), 512);
    assert_eq!(
        reasons(&report),
        vec![("skills", "more than 512 entries, keeping the first 512",)],
    );
}

#[test]
fn five_hundred_twelve_skill_dirs_report_nothing() {
    let fixture = Fixture::new();
    for index in 0..512 {
        fixture.write_source(
            &format!("skills/skill-{index:03}/SKILL.md"),
            b"---\nname: x\ndescription: y.\n---\n\nBody.\n",
        );
    }
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills.len(), 512);
    assert!(report.left_behind.is_empty(), "{report:?}");
}

#[test]
fn a_commands_dir_past_512_reports_under_its_own_name() {
    let fixture = Fixture::new();
    for index in 0..513 {
        fixture.write_source(&format!("commands/do-{index:03}.md"), b"Do it.\n");
    }
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.commands.len(), 512);
    assert!(
        reasons(&report).contains(&("commands", "more than 512 entries, keeping the first 512",)),
        "{report:?}",
    );
}

#[test]
fn a_source_home_past_512_reports_under_the_source_name() {
    let fixture = Fixture::new();
    for index in 0..513 {
        fixture.write_source(&format!("junk-{index:03}.txt"), b"junk\n");
    }
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    // The 512 kept entries each land unmapped, and the 513th reports the
    // cutoff under the source's own name.
    assert_eq!(report.left_behind.len(), 513);
    assert!(
        reasons(&report).contains(&(".claude", "more than 512 entries, keeping the first 512",)),
        "{report:?}",
    );
}

#[test]
fn a_two_mebibyte_skill_md_joins_left_behind_unread() {
    let fixture = Fixture::new();
    fixture.write_source("skills/review/SKILL.md", &skill_body(2 * 1024 * 1024));
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert!(report.imported.skills.is_empty(), "{report:?}");
    assert_eq!(
        reasons(&report),
        vec![("skills/review/SKILL.md", "larger than 1 MiB, left unread",)],
    );
    assert!(
        !fixture.home().join("skills/review/SKILL.md").exists(),
        "an unread skill writes nothing"
    );
}

#[test]
fn a_one_mebibyte_skill_md_reads_fine() {
    let fixture = Fixture::new();
    fixture.write_source("skills/review/SKILL.md", &skill_body(1024 * 1024));
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills, vec!["review".to_owned()]);
    assert!(report.left_behind.is_empty(), "{report:?}");
}

#[test]
fn reads_stop_at_the_eight_mebibyte_total() {
    let fixture = Fixture::new();
    for index in 0..9 {
        fixture.write_source(
            &format!("skills/skill-{index}/SKILL.md"),
            &skill_body(1024 * 1024),
        );
    }
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills.len(), 8);
    let reasons = reasons(&report);
    assert_eq!(reasons.len(), 1, "{report:?}");
    assert!(reasons[0].0.starts_with("skills/skill-"), "{report:?}");
    assert_eq!(reasons[0].1, "over the 8 MiB read budget, left unread",);
}

#[test]
fn the_offer_shows_until_a_receipt_lands() {
    let fixture = Fixture::new();
    let home = fixture.home();
    std::fs::create_dir_all(&home).unwrap();
    assert!(import_pending(&home));
    fixture.run(ImportSource::Codex).unwrap();
    assert!(!import_pending(&home), "one receipt stills the offer");
}

#[test]
fn eight_one_mebibyte_skills_fit_without_a_budget_entry() {
    let fixture = Fixture::new();
    for index in 0..8 {
        fixture.write_source(
            &format!("skills/skill-{index}/SKILL.md"),
            &skill_body(1024 * 1024),
        );
    }
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills.len(), 8);
    assert!(report.left_behind.is_empty(), "{report:?}");
}
