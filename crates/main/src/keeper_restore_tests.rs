//! Restored jobs: what the runner appends, delivers and reports.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::clock::Clock;
use contract::events::{Event, JobStarted, Outcome};
use contract::inbox::Delivery;
use contract::jobs::Jobs as _;
use contract::keeper::KeptJob;
use contract::{ErrorCode, JobId, SessionId};
use fakes::Deadline;
use fakes::clock::FakeClock;

use super::{Restored, restore};
use crate::keeper::{Attached, Host, handover_over, host_without_keeper};

fn deadline() -> Deadline {
    Deadline::after(Duration::from_secs(10))
}

/// A session directory with its log, the registry over its artifacts, the
/// host, and the keeper's end of a handover connection.
struct World {
    _root: fakes::TempDir,
    dir: log::SessionDir,
    log: log::Log,
    registry: Arc<jobs::Registry>,
    recorder: Arc<fakes::Recorder>,
    host: Arc<Host>,
    keeper: UnixStream,
    session_end: Option<UnixStream>,
}

fn world() -> World {
    let root = fakes::TempDir::new("keeper-restore");
    let clock = FakeClock::new();
    let id = SessionId("s_0123456789abcdef".into());
    let log = log::Log::create(root.path(), id.clone(), clock.clone()).unwrap();
    let dir = log::SessionDir::new(root.path(), &id);
    std::fs::create_dir_all(dir.artifacts()).unwrap();
    let recorder = Arc::new(fakes::Recorder::default());
    let as_clock: Arc<dyn Clock> = clock.clone();
    let registry = jobs::Registry::new(dir.artifacts(), Arc::clone(&as_clock), recorder.clone());
    let (session_end, keeper) = UnixStream::pair().unwrap();
    World {
        host: host_without_keeper(as_clock),
        _root: root,
        dir,
        log,
        registry,
        recorder,
        keeper,
        session_end: Some(session_end),
    }
}

impl World {
    /// A job the log holds open, with the keeper's bytes already in its file.
    fn job(&self, id: &str, call: &str, keeper_bytes: &[u8]) -> KeptJob {
        self.log
            .append(
                &Event::JobStarted(JobStarted {
                    job_id: JobId(id.into()),
                    tool: Some("shell".into()),
                    extension: None,
                    description: format!("job {id}"),
                    output_path: format!("artifacts/{id}.log"),
                }),
                None,
                None,
            )
            .unwrap();
        let output = self.dir.artifacts().join(format!("{id}.log"));
        std::fs::write(&output, keeper_bytes).unwrap();
        KeptJob {
            id: call.into(),
            job_id: JobId(id.into()),
            output_path: self.dir.path().join(format!("artifacts/{id}.log")),
            errors_path: None,
            tty: false,
            lines: false,
        }
    }

    fn attach(&mut self, running: Vec<KeptJob>) -> Attached {
        let stream = self.session_end.take().expect("one handover");
        self.host
            .attach_handover(handover_over(stream, running).unwrap())
            .unwrap()
    }

    fn send(&mut self, frame: serde_json::Value) {
        serde_json::to_writer(&mut self.keeper, &frame).unwrap();
        self.keeper.write_all(b"\n").unwrap();
    }

    fn output(&self, id: &str) -> Vec<u8> {
        std::fs::read(self.dir.artifacts().join(format!("{id}.log"))).unwrap()
    }
}

fn output_frame(call: &str, stream: &str, text: &str) -> serde_json::Value {
    use base64::Engine as _;
    serde_json::json!({"output": {"id": call, "stream": stream,
        "bytes": base64::engine::general_purpose::STANDARD.encode(text)}})
}

fn exit_frame(call: &str, code: i32) -> serde_json::Value {
    serde_json::json!({"exit": {"id": call, "code": code, "signal": null,
        "timed_out": false, "output_cap": false}})
}

fn inbox(world: &World) -> mpsc::Receiver<Delivery> {
    let (send, deliveries) = mpsc::channel();
    world.registry.deliver_to(send);
    deliveries
}

/// The restore of every job `attached` carries, activated.
fn restored(world: &World, attached: &mut Attached) -> Restored {
    restore(&world.registry, &world.dir, attached.take_jobs()).unwrap()
}

#[test]
fn monitor_lines_without_errors_path_restored() {
    // A monitor whose stderr file could not be created: errors_path is empty
    // and lines is true, as the tools hand-off sends it.
    let mut world = world();
    let mut job = world.job("j_mon", "call_0", b"from the keeper\n");
    job.lines = true;
    let mut attached = world.attach(vec![job]);
    let deliveries = inbox(&world);
    let restored = restored(&world, &mut attached);
    assert_eq!(restored.job_ids(), [JobId("j_mon".into())]);
    let active = restored.activate();
    // The keeper has not acknowledged yet: `ready` waits behind this barrier.
    let (send, ready) = mpsc::channel();
    let waiter = std::thread::spawn(
        move || match send.send(attached.ready().map(|()| attached)) {
            Ok(()) | Err(_) => {}
        },
    );
    let mut keeper = world.keeper.try_clone().unwrap();
    let mut adopted = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(&mut keeper), &mut adopted).unwrap();
    assert_eq!(adopted.trim(), r#"{"adopted":true}"#);
    world.send(output_frame("call_0", "out", "hello\nwor"));
    world.send(output_frame("call_0", "out", "ld\n"));
    world.send(exit_frame("call_0", 0));
    // The lines arrive before the ending, and neither waits for the ack.
    let dl = deadline();
    let mut text = Vec::new();
    let completed = loop {
        let delivery = dl.recv_or_fail(&deliveries, "a line or the ending");
        if let Delivery::JobLine(line) = delivery {
            text.push(line.lines);
        } else if let Delivery::Job(notice) = delivery {
            break notice.completed;
        } else {
            panic!("unexpected delivery {delivery:?}");
        }
    };
    assert_eq!(text.join("\n"), "hello\nworld");
    assert_eq!(completed.job_id, JobId("j_mon".into()));
    assert_eq!(completed.status, Outcome::Completed);
    assert!(ready.try_recv().is_err(), "the acknowledgement is withheld");
    // The keeper's bytes stay, the session's follow.
    assert_eq!(world.output("j_mon"), b"from the keeper\nhello\nworld\n");
    // Progress for clients carries what the session appended, not the
    // keeper's earlier bytes, and is ephemeral output of the same job.
    let shown: String = world
        .recorder
        .events()
        .into_iter()
        .filter_map(|event| {
            if let Event::JobDelta(delta) = event {
                delta.progress.text
            } else {
                None
            }
        })
        .collect();
    assert_eq!(shown, "hello\nworld\n");
    world.send(serde_json::json!({"adopt_done": true}));
    assert!(dl.recv_or_fail(&ready, "the acknowledgement").is_ok());
    waiter.join().unwrap();
    active.keep();
    assert!(
        deliveries.try_recv().is_err(),
        "the ending is delivered once"
    );
}

#[test]
fn lines_without_a_monitor_flag_deliver_nothing() {
    let mut world = world();
    let job = world.job("j_plain", "call_0", b"");
    let mut attached = world.attach(vec![job]);
    let deliveries = inbox(&world);
    let restored = restored(&world, &mut attached);
    let active = restored.activate();
    world.send(output_frame("call_0", "out", "a\nb\n"));
    world.send(exit_frame("call_0", 0));
    let delivery = deadline().recv_or_fail(&deliveries, "the ending");
    assert!(matches!(delivery, Delivery::Job(_)), "{delivery:?}");
    active.keep();
}

#[test]
fn an_ending_before_the_runner_runs_stays_accounted_until_it_reads_it() {
    let mut world = world();
    let job = world.job("j_quick", "call_0", b"early\n");
    let mut attached = world.attach(vec![job]);
    let deliveries = inbox(&world);
    let restored = restored(&world, &mut attached);
    // The job ends while the session still prepares.
    world.send(output_frame("call_0", "out", "late\n"));
    world.send(exit_frame("call_0", 3));
    // Still a running job of the registry, with nothing delivered.
    assert_eq!(world.registry.running(), vec![JobId("j_quick".into())]);
    assert!(deliveries.try_recv().is_err());
    let active = restored.activate();
    let delivery = deadline().recv_or_fail(&deliveries, "the ending");
    let Delivery::Job(notice) = delivery else {
        panic!("expected the ending, got {delivery:?}")
    };
    assert_eq!(notice.completed.job_id, JobId("j_quick".into()));
    assert_eq!(
        notice
            .completed
            .process
            .and_then(|process| process.exit_code),
        Some(3)
    );
    assert_eq!(world.output("j_quick"), b"early\nlate\n");
    // A session-written ending carries the output tail, as a live job's does:
    // without it a later resume would take it for a keeper-written ending and
    // tell the model of it again.
    assert_eq!(
        notice.completed.output_tail.as_deref(),
        Some("early\nlate\n")
    );
    assert!(world.registry.running().is_empty());
    active.keep();
}

#[test]
fn stderr_stays_in_its_file_and_a_terminal_and_stop_reach_the_owning_keeper() {
    let mut world = world();
    let mut job = world.job("j_tty", "call_0", b"");
    let errors = world.dir.artifacts().join("j_tty.stderr.log");
    std::fs::write(&errors, b"old\n").unwrap();
    job.errors_path = Some(errors.clone());
    job.tty = true;
    let mut attached = world.attach(vec![job]);
    let deliveries = inbox(&world);
    let restored = restored(&world, &mut attached);
    let active = restored.activate();
    world.send(output_frame("call_0", "out", "out\n"));
    world.send(output_frame("call_0", "err", "err\n"));
    world.send(exit_frame("call_0", 0));
    let delivery = deadline().recv_or_fail(&deliveries, "the ending");
    assert!(matches!(delivery, Delivery::Job(_)));
    assert_eq!(world.output("j_tty"), b"out\n");
    assert_eq!(std::fs::read(&errors).unwrap(), b"old\nerr\n");
    active.keep();
}

#[test]
fn the_stop_and_the_terminal_of_a_running_restored_job_reach_its_keeper() {
    let mut world = world();
    let mut job = world.job("j_run", "call_0", b"");
    job.tty = true;
    let mut attached = world.attach(vec![job]);
    let _deliveries = inbox(&world);
    let restored = restored(&world, &mut attached);
    let active = restored.activate();
    let (send, answer) = mpsc::channel();
    let registry = Arc::clone(&world.registry);
    std::thread::spawn(
        move || match send.send(registry.write(&JobId("j_run".into()), "ls\n")) {
            Ok(()) | Err(_) => {}
        },
    );
    let mut reader = std::io::BufReader::new(world.keeper.try_clone().unwrap());
    let mut line = String::new();
    std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        serde_json::json!({"write": {"id": "call_0", "text": "ls\n"}})
    );
    assert!(world.registry.stop(&JobId("j_run".into())));
    line.clear();
    std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
    assert_eq!(line.trim(), r#"{"stop":"call_0"}"#);
    world.send(exit_frame("call_0", 0));
    drop(deadline().recv(&answer));
    active.keep();
}

fn refused(world: &mut World, running: Vec<KeptJob>) -> std::io::Error {
    let mut attached = world.attach(running);
    match restore(&world.registry, &world.dir, attached.take_jobs()) {
        Ok(_) => panic!("a restore of a job the log does not describe succeeded"),
        Err(error) => error,
    }
}

#[test]
fn a_job_the_log_does_not_hold_open_is_refused_and_nothing_is_recorded() {
    let mut world = world();
    let mut job = world.job("j_known", "call_0", b"");
    job.job_id = JobId("j_unknown".into());
    let error = refused(&mut world, vec![job]);
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(world.registry.running().is_empty());
}

#[test]
fn a_job_the_log_already_ended_is_refused() {
    let mut world = world();
    let job = world.job("j_done", "call_0", b"");
    world
        .log
        .append(
            &Event::JobCompleted(tools::kept_end(
                JobId("j_done".into()),
                &contract::keeper::Exit {
                    code: Some(0),
                    signal: None,
                    timed_out: false,
                    output_cap: false,
                },
            )),
            None,
            None,
        )
        .unwrap();
    let error = refused(&mut world, vec![job]);
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn files_the_log_does_not_name_are_refused_before_anything_opens() {
    for bad in ["output", "errors"] {
        let mut world = world();
        let mut job = world.job("j_job", "call_0", b"");
        let elsewhere = world.dir.path().join("elsewhere.log");
        std::fs::write(&elsewhere, b"").unwrap();
        match bad {
            "output" => job.output_path = elsewhere,
            _ => job.errors_path = Some(elsewhere),
        }
        let error = refused(&mut world, vec![job]);
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{bad}");
        assert!(world.registry.running().is_empty(), "{bad}");
    }
}

#[test]
fn one_bad_job_among_good_ones_records_none() {
    let mut world = world();
    let good = world.job("j_good", "call_0", b"");
    let mut bad = world.job("j_bad", "call_1", b"");
    bad.output_path = PathBuf::from("/nowhere");
    let error = refused(&mut world, vec![good, bad]);
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(world.registry.running().is_empty());
}

/// A FIFO an observer holds open for reading: reading it fails with
/// `WouldBlock` while a writer holds it open and returns end of file once
/// none does, so it shows when the session's last handle closes.
struct Writers(std::fs::File);

impl Writers {
    fn at(path: &Path) -> Self {
        use std::os::unix::fs::OpenOptionsExt as _;
        fakes::fifo(path);
        Self(
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NONBLOCK.bits().try_into().unwrap())
                .open(path)
                .unwrap(),
        )
    }

    fn open(&mut self) -> bool {
        let mut byte = [0; 1];
        match self.0.read(&mut byte) {
            Err(error) => error.kind() == std::io::ErrorKind::WouldBlock,
            Ok(read) => {
                assert_eq!(read, 0, "nobody writes into the observer's FIFO");
                false
            }
        }
    }
}

/// A job whose output file is a FIFO that `writers` watches.
fn watched_job(world: &World, id: &str, call: &str) -> (KeptJob, Writers) {
    let job = world.job(id, call, b"");
    std::fs::remove_file(&job.output_path).unwrap();
    let writers = Writers::at(&job.output_path);
    (job, writers)
}

#[test]
fn dropping_restored_jobs_closes_their_handles_and_reports_nothing() {
    let mut world = world();
    let (job, mut writers) = watched_job(&world, "j_job", "call_0");
    let mut attached = world.attach(vec![job]);
    let deliveries = inbox(&world);
    let restored = restored(&world, &mut attached);
    assert!(writers.open(), "the session holds the file");
    // A setup failure before the runners start: handles close, and the job
    // is not completed under the session's writer.
    drop(restored);
    assert!(!writers.open(), "the handle closed with the runner");
    drop(attached);
    assert!(deliveries.try_recv().is_err());
    assert_eq!(world.registry.running(), vec![JobId("j_job".into())]);
}

#[test]
fn abandoning_active_runners_closes_their_handles_and_reports_nothing() {
    let mut world = world();
    let (job, mut writers) = watched_job(&world, "j_job", "call_0");
    let mut attached = world.attach(vec![job]);
    let deliveries = inbox(&world);
    let active = restored(&world, &mut attached).activate();
    assert!(writers.open());
    // The acknowledgement never came: the handles close now, even though the
    // runner is still blocked waiting for its next event.
    drop(active);
    assert!(!writers.open());
    // The keeper's last word reaches a runner that is no longer the owner.
    world.send(exit_frame("call_0", 0));
    assert!(deliveries.recv_timeout_for_test().is_none());
    drop(attached);
    assert!(deliveries.recv_timeout_for_test().is_none());
    assert_eq!(world.registry.running(), vec![JobId("j_job".into())]);
}

trait QuietFor<T> {
    fn recv_timeout_for_test(&self) -> Option<T>;
}
impl<T> QuietFor<T> for mpsc::Receiver<T> {
    fn recv_timeout_for_test(&self) -> Option<T> {
        Deadline::after(Duration::from_millis(200)).recv(self).ok()
    }
}

#[test]
fn a_kept_handover_keeps_its_handles_open() {
    let mut world = world();
    let (job, mut writers) = watched_job(&world, "j_job", "call_0");
    let mut attached = world.attach(vec![job]);
    let _deliveries = inbox(&world);
    let active = restored(&world, &mut attached).activate();
    active.keep();
    assert!(writers.open());
    attached.keep();
}

#[test]
fn a_keeper_that_vanishes_leaves_its_job_failed_indeterminate() {
    let mut world = world();
    let job = world.job("j_job", "call_0", b"");
    let mut attached = world.attach(vec![job]);
    let deliveries = inbox(&world);
    let active = restored(&world, &mut attached).activate();
    world.keeper.shutdown(std::net::Shutdown::Both).unwrap();
    let delivery = deadline().recv_or_fail(&deliveries, "the ending");
    let Delivery::Job(notice) = delivery else {
        panic!("expected the ending, got {delivery:?}")
    };
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice.completed.error.map(|error| error.code),
        Some(ErrorCode::Indeterminate)
    );
    active.keep();
}

#[test]
fn text_decoding_holds_an_incomplete_character_for_the_next_bytes() {
    let mut text = super::Text::default();
    let euro = "€".as_bytes();
    assert_eq!(text.decode(&euro[..2]), "");
    assert_eq!(text.decode(&euro[2..]), "€");
    assert_eq!(text.decode(b"a\xffb"), "a\u{fffd}b");
}

#[test]
fn feed_takes_complete_lines_and_keeps_the_rest() {
    let mut feed = super::Feed::default();
    feed.push(b"one\ntw");
    assert_eq!(feed.take(false), ["one"]);
    assert!(feed.take(false).is_empty());
    feed.push(b"o\n\nthree");
    assert_eq!(feed.take(false), ["two", ""]);
    assert_eq!(feed.take(true), ["three"]);
    assert!(feed.take(true).is_empty());
}

#[test]
fn cut_marks_a_long_line_and_a_long_delivery() {
    let long = "x".repeat(501);
    assert_eq!(super::cut(&[long]), format!("{} [cut]", "x".repeat(500)));
    let many: Vec<String> = (0..400).map(|_| "y".repeat(9)).collect();
    let text = super::cut(&many);
    assert!(text.ends_with("more characters]"), "{text}");
    assert!(text.contains("\n[cut: "));
}

#[test]
fn newest_keeps_the_tail_on_a_character_boundary() {
    let text = format!("a{}", "€".repeat(30_000));
    let kept = super::newest(&text);
    assert!(kept.len() <= 64 << 10);
    assert!(kept.starts_with('€'));
    assert_eq!(super::newest("short"), "short");
}

#[test]
fn a_finish_that_fails_closes_the_writers_before_the_connection_ends() {
    let mut world = world();
    let (job, mut writers) = watched_job(&world, "j_job", "call_0");
    let mut attached = world.attach(vec![job]);
    let deliveries = inbox(&world);
    let restored = restored(&world, &mut attached);
    let handed = super::Handed { restored, attached };
    // The keeper goes away before it acknowledges: its recovery reads the
    // end of the connection, and must find no session handle on the file.
    world.keeper.shutdown(std::net::Shutdown::Both).unwrap();
    let error = handed.finish().unwrap_err();
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::TimedOut
        ),
        "{error}"
    );
    assert!(
        !writers.open(),
        "every append handle closed with the failure"
    );
    assert!(deliveries.recv_timeout_for_test().is_none());
}

#[test]
fn a_finish_the_keeper_acknowledges_keeps_the_writers() {
    let mut world = world();
    let (job, mut writers) = watched_job(&world, "j_job", "call_0");
    let mut attached = world.attach(vec![job]);
    let _deliveries = inbox(&world);
    let restored = restored(&world, &mut attached);
    let handed = super::Handed { restored, attached };
    world.send(serde_json::json!({"adopt_done": true}));
    handed.finish().unwrap();
    assert!(
        writers.open(),
        "the session owns the file after the handover"
    );
}
