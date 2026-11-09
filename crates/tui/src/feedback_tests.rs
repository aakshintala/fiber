//! The injected feedback writer: what `start` runs and posts.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::SessionId;
use fakes::Deadline;

const DEADLINE: Duration = Duration::from_secs(10);

fn session() -> SessionId {
    SessionId("s_aaaaaaaaaaaaaaaa".to_owned())
}

/// What a writer saw: the description, the session id and the frame
/// shapes it was given.
type Seen = Arc<
    Mutex<
        Vec<(
            String,
            Option<SessionId>,
            Vec<contract::feedback::FrameShape>,
        )>,
    >,
>;

/// A writer answering `done` for every description, recording what it saw.
fn stub(done: crate::FeedbackDone) -> (crate::WriteFeedback, Seen) {
    let seen: Seen = Arc::default();
    let writer: crate::WriteFeedback = Arc::new({
        let seen = Arc::clone(&seen);
        move |description: &str,
              session: Option<&SessionId>,
              shapes: &[contract::feedback::FrameShape]| {
            seen.lock()
                .unwrap()
                .push((description.to_owned(), session.cloned(), shapes.to_vec()));
            Ok(done.clone())
        }
    });
    (writer, seen)
}

fn done() -> crate::FeedbackDone {
    crate::FeedbackDone {
        path: PathBuf::from("/w/fiber-feedback-7.md"),
        link: "https://example.test/new?title=x".to_owned(),
    }
}

/// The worker's post within [`DEADLINE`], failing after it.
#[track_caller]
fn posted(rx: &mpsc::Receiver<crate::Input>, what: &str) -> crate::Input {
    Deadline::after(DEADLINE).recv_or_fail(rx, what)
}

/// One captured frame's shape, empty: the worker moves whatever
/// snapshot it is handed, byte for byte.
fn shape() -> contract::feedback::FrameShape {
    contract::feedback::FrameShape {
        width: 80,
        height: 24,
        top: Some(12),
        has_new: false,
        palette: Vec::new(),
        rows: Vec::new(),
    }
}

#[test]
fn start_runs_the_writer_and_posts_its_answer() {
    let expected = done();
    let (writer, seen) = stub(expected.clone());
    let (tx, rx) = mpsc::channel();
    assert_eq!(
        crate::feedback::start(
            Some(writer),
            Some(&tx),
            "the sound".to_owned(),
            Some(session()),
            vec![shape()],
        ),
        None
    );
    let crate::Input::Feedback(result) = posted(&rx, "feedback answer") else {
        panic!("feedback answer");
    };
    assert_eq!(result.unwrap(), expected);
    assert_eq!(
        *seen.lock().unwrap(),
        [("the sound".to_owned(), Some(session()), vec![shape()])]
    );
}

#[test]
fn start_hands_the_writer_no_session_when_none_is_attached() {
    let (writer, seen) = stub(done());
    let (tx, rx) = mpsc::channel();
    assert_eq!(
        crate::feedback::start(
            Some(writer),
            Some(&tx),
            "the sound".to_owned(),
            None,
            vec![shape()],
        ),
        None
    );
    posted(&rx, "feedback answer");
    assert_eq!(
        *seen.lock().unwrap(),
        [("the sound".to_owned(), None, vec![shape()])]
    );
}

#[test]
fn start_without_a_writer_says_so_and_posts_nothing() {
    let (tx, rx) = mpsc::channel();
    let notice = crate::feedback::start(None, Some(&tx), "the sound".to_owned(), None, vec![]);
    assert_eq!(notice, Some(crate::feedback::NO_WRITER.to_owned()));
    assert!(rx.try_recv().is_err());
}

#[test]
fn start_without_a_channel_says_so_and_writes_nothing() {
    let (writer, seen) = stub(done());
    let notice = crate::feedback::start(Some(writer), None, "the sound".to_owned(), None, vec![]);
    assert_eq!(notice, Some(crate::feedback::NO_CHANNEL.to_owned()));
    assert!(seen.lock().unwrap().is_empty());
}
