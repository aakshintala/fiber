//! A pending offer is the authority for a reader-thread source request.

use std::sync::{Arc, Mutex, mpsc};

use contract::events::{
    CommandResult, Event, OfferedItem, OfferedKind, RepositoryCodeOffered, RepositoryCodeResolved,
};
use contract::inbox::{Ack, Answer};
use contract::repository::{Decided, RepositoryCode, Unapproved};
use contract::shapes::Failure;
use contract::{ErrorCode, RequestId};
use fakes::Deadline;
use serde_json::json;

#[path = "../tests/support/mod.rs"]
mod support;
use support::{DEADLINE, Opened};

#[derive(Default)]
struct Files {
    seen: Mutex<Vec<String>>,
    failure: Option<ErrorCode>,
}

impl RepositoryCode for Files {
    fn unapproved(&self) -> Result<Vec<Unapproved>, Failure> {
        Ok(vec![])
    }
    fn decide(
        &self,
        _: &OfferedItem,
        _: contract::events::OfferDecision,
    ) -> Result<Decided, Failure> {
        Ok(Decided::Obsolete)
    }
    fn file(&self, path: &str) -> Result<String, Failure> {
        self.seen.lock().unwrap().push(path.into());
        if let Some(code) = &self.failure {
            return Err(Failure {
                code: code.clone(),
                message: format!("{path}: refused by repository"),
                retry_after_ms: None,
                provider: None,
            });
        }
        Ok("echo source\n".into())
    }
}

fn offer(opened: &Opened) {
    let items = ["tools/a.sh", "tools/db.sh"]
        .map(|path| OfferedItem {
            kind: OfferedKind::Hook,
            name: path.into(),
            hash: "h".into(),
            required: false,
            summary: "hook".into(),
            files: vec![path.into()],
            version: None,
            diff: None,
        })
        .to_vec();
    opened
        .log
        .append(
            &Event::RepositoryCodeOffered(RepositoryCodeOffered {
                request_id: RequestId("r1".into()),
                items,
            }),
            None,
            None,
        )
        .unwrap();
}

fn request(driver: Arc<dyn contract::extension::Drive>, request: &str, path: &str) -> Answer {
    let args = serde_json::from_value(json!({"request_id": request, "path": path})).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        driver.drive(
            "fiber.test/source",
            "offered_file",
            args,
            Ack(Box::new(move |answer| {
                tx.send(answer).unwrap();
            })),
        )
    });
    Deadline::after(DEADLINE).recv_or_fail(&rx, "the offered file's answer")
}

fn setup() -> (Opened, Arc<Files>) {
    let opened = Opened::open(vec![]);
    let files = Arc::new(Files::default());
    opened.session.repository_code(files.clone());
    offer(&opened);
    (opened, files)
}

#[test]
fn a_listed_file_is_answered_with_its_text() {
    let (opened, files) = setup();
    assert_eq!(
        request(opened.session.driver(), "r1", "tools/db.sh").unwrap(),
        Some(CommandResult::OfferedFile {
            text: "echo source\n".into()
        })
    );
    assert_eq!(*files.seen.lock().unwrap(), ["tools/db.sh"]);
    assert!(opened.log.latest("repository_code_offered").is_some());
    assert_eq!(opened.log.count(), 1);
    opened.close();
}

#[test]
fn a_path_listed_by_another_item_is_answered() {
    let (opened, files) = setup();
    assert!(
        request(opened.session.driver(), "r1", "tools/a.sh")
            .unwrap()
            .is_some()
    );
    assert_eq!(*files.seen.lock().unwrap(), ["tools/a.sh"]);
    opened.close();
}

#[test]
fn an_unlisted_path_is_invalid_arguments() {
    let (opened, files) = setup();
    let refused = request(opened.session.driver(), "r1", "secret").unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidArguments);
    assert_eq!(refused.message, "That path is not in the pending offer.");
    assert!(files.seen.lock().unwrap().is_empty());
    opened.close();
}

#[test]
fn a_near_miss_path_is_invalid_arguments() {
    let (opened, files) = setup();
    for path in ["./tools/db.sh", "tools//db.sh", "tools/db.sh/"] {
        assert_eq!(
            request(opened.session.driver(), "r1", path)
                .unwrap_err()
                .code,
            ErrorCode::InvalidArguments
        );
    }
    assert!(files.seen.lock().unwrap().is_empty());
    opened.close();
}

#[test]
fn another_request_id_is_stale() {
    let (opened, files) = setup();
    let refused = request(opened.session.driver(), "r2", "tools/db.sh").unwrap_err();
    assert_eq!(refused.code, ErrorCode::StaleRequest);
    assert_eq!(refused.message, "That offer is no longer pending.");
    assert!(files.seen.lock().unwrap().is_empty());
    opened.close();
}

#[test]
fn a_resolved_offer_is_stale() {
    let (opened, files) = setup();
    opened
        .log
        .append(
            &Event::RepositoryCodeResolved(RepositoryCodeResolved {
                request_id: RequestId("r1".into()),
                decisions: vec![],
            }),
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        request(opened.session.driver(), "r1", "tools/db.sh")
            .unwrap_err()
            .code,
        ErrorCode::StaleRequest
    );
    assert!(files.seen.lock().unwrap().is_empty());
    opened.close();
}

#[test]
fn an_offer_ended_by_preamble_built_is_stale() {
    let (opened, files) = setup();
    let preamble = serde_json::from_value(json!({"reason": "start", "model": "p/m", "context_window": 1000, "tool_choice": "auto", "cache_lifetime": "5m", "system_prompt": "", "tools": []})).unwrap();
    opened
        .log
        .append(&Event::PreambleBuilt(preamble), None, None)
        .unwrap();
    assert_eq!(
        request(opened.session.driver(), "r1", "tools/db.sh")
            .unwrap_err()
            .code,
        ErrorCode::StaleRequest
    );
    assert!(files.seen.lock().unwrap().is_empty());
    opened.close();
}

#[test]
fn no_offer_is_stale() {
    let opened = Opened::open(vec![]);
    opened.session.repository_code(Arc::new(Files::default()));
    assert_eq!(
        request(opened.session.driver(), "r1", "tools/db.sh")
            .unwrap_err()
            .code,
        ErrorCode::StaleRequest
    );
    opened.close();
}

#[test]
fn no_repository_code_set_is_stale() {
    let opened = Opened::open(vec![]);
    offer(&opened);
    assert_eq!(
        request(opened.session.driver(), "r1", "tools/db.sh")
            .unwrap_err()
            .code,
        ErrorCode::StaleRequest
    );
    opened.close();
}

#[test]
fn a_closed_log_is_closing() {
    let (opened, _) = setup();
    let driver = opened.session.driver();
    let Opened {
        session,
        log,
        sessions,
        id,
        _temp,
        ..
    } = opened;
    drop(log);
    assert_eq!(
        request(driver, "r1", "tools/db.sh").unwrap_err().code,
        ErrorCode::Closing
    );
    let reopened = Arc::new(log::Log::open(&sessions, id, fakes::clock::FakeClock::new()).unwrap());
    support::close_within(session, reopened);
    drop(_temp);
}

#[test]
fn the_seams_failure_is_passed_on_with_its_code() {
    for code in [ErrorCode::IoFailed, ErrorCode::InvalidArguments] {
        let opened = Opened::open(vec![]);
        opened.session.repository_code(Arc::new(Files {
            failure: Some(code.clone()),
            ..Files::default()
        }));
        offer(&opened);
        let refused = request(opened.session.driver(), "r1", "tools/db.sh").unwrap_err();
        assert_eq!(refused.code, code);
        assert_eq!(refused.message, "tools/db.sh: refused by repository");
        opened.close();
    }
}

#[test]
fn host_drive_gets_the_text() {
    let (opened, _) = setup();
    assert_eq!(
        request(opened.session.driver(), "r1", "tools/db.sh").unwrap(),
        Some(CommandResult::OfferedFile {
            text: "echo source\n".into()
        })
    );
    opened.close();
}

#[test]
fn it_is_answered_while_a_turn_streams() {
    let (opened, _) = setup();
    opened
        .session
        .run(vec![], Arc::new(|| true), |inbox| {
            assert!(
                request(opened.session.driver(), "r1", "tools/db.sh")
                    .unwrap()
                    .is_some()
            );
            assert!(matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)));
            Ok(())
        })
        .unwrap();
    opened.close();
}
