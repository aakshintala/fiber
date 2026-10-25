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
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;
use fakes::TempDir;
use fakes::clock::FakeClock;

use super::*;
use crate::write::atomic::{reset_write_count, write_count};

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
    reset_write_count();
    let planned = classify(&fixture.home(), &fixture.source(), ImportSource::ClaudeCode).unwrap();
    // docs/configuration.md, "Importing": at most 512 entries per source
    // directory, one overflow line past 512, kept entries in name order,
    // which 512 unspecified.
    assert_eq!(planned.skills.len(), 512);
    let report = planned.report();
    assert_eq!(
        reasons(&report),
        vec![("skills", "more than 512 entries, keeping the first 512",)],
    );
    assert!(
        report
            .imported
            .skills
            .windows(2)
            .all(|pair| pair[0] <= pair[1]),
        "kept entries are imported in name order: {:?}",
        report.imported.skills,
    );
    assert_eq!(write_count(), 0, "enumeration must write nothing");
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
    reset_write_count();
    let planned = classify(&fixture.home(), &fixture.source(), ImportSource::ClaudeCode).unwrap();
    // docs/configuration.md, "Importing": at most 512 entries per source
    // directory, no overflow line at or under 512, kept entries in name
    // order, which 512 unspecified.
    assert_eq!(planned.skills.len(), 512);
    let report = planned.report();
    assert!(report.left_behind.is_empty(), "{report:?}");
    assert!(
        report
            .imported
            .skills
            .windows(2)
            .all(|pair| pair[0] <= pair[1]),
        "kept entries are imported in name order: {:?}",
        report.imported.skills,
    );
    assert_eq!(write_count(), 0, "enumeration must write nothing");
}

#[test]
fn a_commands_dir_past_512_reports_under_its_own_name() {
    let fixture = Fixture::new();
    for index in 0..513 {
        fixture.write_source(&format!("commands/do-{index:03}.md"), b"Do it.\n");
    }
    reset_write_count();
    let planned = classify(&fixture.home(), &fixture.source(), ImportSource::ClaudeCode).unwrap();
    // docs/configuration.md, "Importing": at most 512 entries per source
    // directory, one overflow line past 512, kept entries in name order,
    // which 512 unspecified.
    assert_eq!(planned.commands.len(), 512);
    let report = planned.report();
    assert_eq!(
        reasons(&report),
        vec![("commands", "more than 512 entries, keeping the first 512",)],
    );
    assert!(
        report
            .imported
            .commands
            .windows(2)
            .all(|pair| pair[0] <= pair[1]),
        "kept entries are imported in name order: {:?}",
        report.imported.commands,
    );
    assert_eq!(write_count(), 0, "enumeration must write nothing");
}

#[test]
fn a_source_home_past_512_reports_under_the_source_name() {
    let fixture = Fixture::new();
    for index in 0..513 {
        fixture.write_source(&format!("junk-{index:03}.txt"), b"junk\n");
    }
    reset_write_count();
    let planned = classify(&fixture.home(), &fixture.source(), ImportSource::ClaudeCode).unwrap();
    let report = planned.report();
    // docs/configuration.md, "Importing": one overflow line past 512 under
    // the source's own name, kept entries in name order, which 512
    // unspecified, so no total or membership is pinned.
    let overflow = reasons(&report)
        .iter()
        .filter(|(item, reason)| {
            *item == ".claude" && *reason == "more than 512 entries, keeping the first 512"
        })
        .count();
    assert_eq!(overflow, 1, "{report:?}");
    let unmapped: Vec<&str> = report
        .left_behind
        .iter()
        .filter(|left| left.item != ".claude")
        .map(|left| left.item.as_str())
        .collect();
    assert!(
        unmapped.windows(2).all(|pair| pair[0] <= pair[1]),
        "kept entries are imported in name order: {unmapped:?}",
    );
    assert_eq!(write_count(), 0, "enumeration must write nothing");
}

#[test]
fn a_three_skill_import_writes_each_item_plus_the_receipt() {
    let fixture = Fixture::new();
    for name in ["alpha", "beta", "gamma"] {
        fixture.write_source(
            &format!("skills/{name}/SKILL.md"),
            b"---\nname: x\ndescription: y.\n---\n\nBody.\n",
        );
    }
    reset_write_count();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    // docs/configuration.md, "Importing": under 512 entries the full count
    // is kept, in name order.
    assert_eq!(
        report.imported.skills,
        vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()],
    );
    assert!(report.left_behind.is_empty(), "{report:?}");
    // Three item writes plus the receipt: no servers, no collisions, so
    // `write_servers` writes nothing. A dropped receipt or a doubled item
    // write breaks this count.
    assert_eq!(write_count(), 4);
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

#[test]
fn a_skill_without_its_file_is_left_behind() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.source().join("skills/review")).unwrap();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert!(report.imported.skills.is_empty(), "{report:?}");
    assert!(
        reasons(&report).contains(&("skills/review/SKILL.md", "holds no SKILL.md",)),
        "{report:?}",
    );
}

#[test]
fn a_settings_dir_is_left_behind_unread() {
    let fixture = Fixture::new();
    // A directory where `settings.json` should be: the open succeeds,
    // the read fails, and the import names it left behind instead of
    // refusing.
    std::fs::create_dir_all(fixture.source().join("settings.json")).unwrap();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert!(report.imported.mcp_servers.is_empty(), "{report:?}");
    assert!(
        reasons(&report).iter().any(
            |(item, reason)| *item == "settings.json" && reason.starts_with("cannot be read: ")
        ),
        "{report:?}",
    );
}

#[test]
fn read_source_missing_file_is_missing() {
    let fixture = Fixture::new();
    let mut budget = Budget::default();
    let mut left_behind = Vec::new();
    let missing = fixture.source().join("skills/review/SKILL.md");
    let got = read_source(
        &missing,
        "skills/review/SKILL.md",
        &mut budget,
        &mut left_behind,
    )
    .unwrap();
    assert!(matches!(got, SourceFile::Missing), "{got:?}");
    assert!(left_behind.is_empty());
    assert_eq!(budget.used, 0);
}

#[test]
fn read_source_other_io_error_is_an_error() {
    // A NUL byte never reaches the syscall: the open fails the same way
    // on every platform, pinning the error arm against the missing one.
    let bad = PathBuf::from("skills/re\0view/SKILL.md");
    let mut budget = Budget::default();
    let mut left_behind = Vec::new();
    let err = read_source(
        &bad,
        "skills/review/SKILL.md",
        &mut budget,
        &mut left_behind,
    )
    .unwrap_err();
    assert!(
        matches!(&err, ConfigError::Io { file, .. } if file == &bad),
        "{err:?}",
    );
    assert!(left_behind.is_empty());
    assert_eq!(budget.used, 0);
}

#[test]
fn read_source_dir_missing_dir_is_empty() {
    let fixture = Fixture::new();
    let mut left_behind = Vec::new();
    let names =
        read_source_dir(&fixture.source().join("skills"), "skills", &mut left_behind).unwrap();
    assert!(names.is_empty());
    assert!(left_behind.is_empty());
}

#[test]
fn read_source_dir_other_io_error_is_an_error() {
    let bad = PathBuf::from("skills\0dir");
    let mut left_behind = Vec::new();
    let err = read_source_dir(&bad, "skills", &mut left_behind).unwrap_err();
    assert!(
        matches!(&err, ConfigError::Io { file, .. } if file == &bad),
        "{err:?}",
    );
    assert!(left_behind.is_empty());
}

/// One wall guard for the FIFO writer's report in a growth test.
const WRITER_HANG: Duration = Duration::from_secs(10);

/// Streams `total` filler bytes into a FIFO at `rel` under the source home,
/// then reports. The FIFO's metadata size stays 0 while its stream runs
/// longer: the same skew a file grown between the metadata check and the
/// read shows. The open rendezvous orders the two ends, so no sleep does;
/// a reader that stops at its cap closes the pipe, and the writes past it
/// fail, which the fixed byte count already made moot. The test joins the
/// worker only after its report arrives, so a worker that never reports is
/// never joined.
fn stream_fifo(
    fixture: &Fixture,
    rel: &str,
    total: u64,
) -> (thread::JoinHandle<()>, mpsc::Receiver<()>) {
    let fifo = fixture.source().join(rel);
    std::fs::create_dir_all(fifo.parent().unwrap()).unwrap();
    fakes::fifo(&fifo);
    let (done, finished) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("import-growth".to_owned())
        .spawn(move || {
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .open(&fifo)
                .expect("the reader opens the FIFO");
            let chunk = vec![b'y'; 64 * 1024];
            let mut left = usize::try_from(total).unwrap();
            use std::io::Write as _;
            while left > 0 {
                let next = left.min(chunk.len());
                if out.write_all(&chunk[..next]).is_err() {
                    break;
                }
                left -= next;
            }
            done.send(()).unwrap_or(());
        })
        .unwrap();
    (worker, finished)
}

/// Reads `rel` through `read_source` with `budget`, then joins the FIFO
/// writer once its report arrives under the wall guard.
fn read_grown(
    fixture: &Fixture,
    rel: &str,
    budget: &mut Budget,
    left_behind: &mut Vec<LeftBehind>,
    worker: thread::JoinHandle<()>,
    finished: mpsc::Receiver<()>,
) -> SourceFile {
    let got = read_source(&fixture.source().join(rel), rel, budget, left_behind).unwrap();
    Deadline::after(WRITER_HANG)
        .recv(&finished)
        .expect("the FIFO writer did not finish");
    worker.join().expect("the FIFO writer panicked");
    got
}

#[test]
fn a_file_growing_past_one_mebibyte_mid_read_is_left_behind() {
    let fixture = Fixture::new();
    // The FIFO's size 0 passes both metadata checks; the stream runs past
    // 1 MiB, which the capped read reports under the file cap's reason.
    let (worker, finished) = stream_fifo(&fixture, "skills/review/SKILL.md", MAX_FILE_BYTES + 512);
    let mut budget = Budget::default();
    let mut left_behind = Vec::new();
    let got = read_grown(
        &fixture,
        "skills/review/SKILL.md",
        &mut budget,
        &mut left_behind,
        worker,
        finished,
    );
    assert!(matches!(got, SourceFile::Skipped), "{got:?}");
    assert_eq!(left_behind.len(), 1);
    assert_eq!(left_behind[0].item, "skills/review/SKILL.md");
    assert_eq!(left_behind[0].reason, "larger than 1 MiB, left unread");
    assert_eq!(budget.used, 0);
}

#[test]
fn a_file_growing_past_the_eight_mebibyte_total_mid_read_is_left_behind() {
    let fixture = Fixture::new();
    // Ten bytes of budget left and a 1 KiB stream past them: the size 0
    // passes the metadata check, and the capped read stops eleven bytes in
    // under the budget's reason.
    let (worker, finished) = stream_fifo(&fixture, "skills/review/SKILL.md", 1024);
    let mut budget = Budget {
        used: MAX_SOURCE_BYTES - 10,
    };
    let mut left_behind = Vec::new();
    let got = read_grown(
        &fixture,
        "skills/review/SKILL.md",
        &mut budget,
        &mut left_behind,
        worker,
        finished,
    );
    assert!(matches!(got, SourceFile::Skipped), "{got:?}");
    assert_eq!(left_behind.len(), 1);
    assert_eq!(left_behind[0].item, "skills/review/SKILL.md");
    assert_eq!(
        left_behind[0].reason,
        "over the 8 MiB read budget, left unread"
    );
    assert_eq!(budget.used, MAX_SOURCE_BYTES - 10);
}

#[test]
fn a_file_reading_exactly_to_the_eight_mebibyte_total_is_kept() {
    let fixture = Fixture::new();
    // Ten bytes left, ten streamed: the strict cutoff keeps the read, and
    // the budget closes exactly.
    let (worker, finished) = stream_fifo(&fixture, "skills/review/SKILL.md", 10);
    let mut budget = Budget {
        used: MAX_SOURCE_BYTES - 10,
    };
    let mut left_behind = Vec::new();
    let got = read_grown(
        &fixture,
        "skills/review/SKILL.md",
        &mut budget,
        &mut left_behind,
        worker,
        finished,
    );
    match got {
        SourceFile::Read(bytes) => assert_eq!(bytes.len(), 10),
        SourceFile::Missing | SourceFile::Skipped => panic!("the boundary read is kept: {got:?}"),
    }
    assert!(left_behind.is_empty());
    assert_eq!(budget.used, MAX_SOURCE_BYTES);
}

#[test]
fn a_one_mebibyte_stream_past_a_shorter_budget_reports_the_budget() {
    let fixture = Fixture::new();
    // The stream reaches exactly 1 MiB while one byte less of budget is
    // left: the read proves the file fits its cap, so the budget's reason
    // wins over the file cap's.
    let (worker, finished) = stream_fifo(&fixture, "skills/review/SKILL.md", MAX_FILE_BYTES);
    let mut budget = Budget {
        used: MAX_SOURCE_BYTES - (MAX_FILE_BYTES - 1),
    };
    let mut left_behind = Vec::new();
    let got = read_grown(
        &fixture,
        "skills/review/SKILL.md",
        &mut budget,
        &mut left_behind,
        worker,
        finished,
    );
    assert!(matches!(got, SourceFile::Skipped), "{got:?}");
    assert_eq!(left_behind.len(), 1);
    assert_eq!(left_behind[0].item, "skills/review/SKILL.md");
    assert_eq!(
        left_behind[0].reason,
        "over the 8 MiB read budget, left unread"
    );
    assert_eq!(budget.used, MAX_SOURCE_BYTES - (MAX_FILE_BYTES - 1));
}
