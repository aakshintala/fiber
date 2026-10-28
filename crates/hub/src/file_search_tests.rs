//! Search supersession is forced with a held walk and a reader barrier.
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, atomic::AtomicBool, mpsc};

use crate::connection::{Accept, Hub, lock, serve_counted};
use crate::fake::{FakeMachine, FakeStarter, SearchGate};
use crate::testkit::{DEADLINE, await_true, hub};
use fakes::clock::FakeClock;
use fakes::{Deadline, TempDir};
use serde_json::{Value, json};

pub(crate) struct Client {
    pub(crate) socket: UnixStream,
    pub(crate) answers: mpsc::Receiver<Value>,
    ended: mpsc::Receiver<()>,
    pub(crate) hub: Arc<Hub>,
    _home: TempDir,
}
impl Client {
    pub(crate) fn new(machine: Arc<FakeMachine>, fail: bool) -> Self {
        let home = TempDir::new("search-hub");
        let h = hub(
            home.path(),
            Arc::new(FakeStarter::hang(home.path())),
            FakeClock::new(),
        )
        .with_machine(machine)
        .with_environments(crate::Environments::new(
            vec![("HOME".into(), "/hub-home".into())],
            home.path(),
        ));
        if fail {
            *lock(&h.fail_spawn) = Some("hub-search");
        }
        let h = Arc::new(h);
        let (socket, server) = UnixStream::pair().unwrap();
        socket.set_write_timeout(Some(DEADLINE)).unwrap();
        let (done, ended) = mpsc::channel();
        let held = h.clone();
        std::thread::spawn(move || {
            held.environments.first(held.clock.as_ref(), &held.diag);
            match held.poll_accept(&server, &AtomicBool::new(false)) {
                Accept::Counted(n) => serve_counted(server, held, n),
                Accept::Dropped => panic!("test connection stream could not be cloned"),
                Accept::Exiting => panic!("fresh test hub unexpectedly exiting"),
            }
            done.send(()).unwrap_or(());
        });
        let (out, answers) = mpsc::channel();
        let read = socket.try_clone().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(read).lines() {
                let Ok(line) = line else { break };
                let value: Value = serde_json::from_str(&line).unwrap();
                if out.send(value).is_err() {
                    break;
                }
            }
        });
        let client = Self {
            socket,
            answers,
            ended,
            hub: h,
            _home: home,
        };
        assert_eq!(client.next()["kind"], "hub_hello");
        client
    }
    pub(crate) fn send(&mut self, id: &str, command: &str, args: Value) {
        writeln!(
            self.socket,
            "{}",
            json!({"id":id,"command":command,"args":args})
        )
        .unwrap();
    }
    fn search(&mut self, id: &str, workspace: &str, text: &str, fresh: bool) {
        self.send(
            id,
            "search_files",
            json!({"workspace":workspace,"text":text,"fresh":fresh}),
        );
    }
    pub(crate) fn next(&self) -> Value {
        Deadline::after(DEADLINE).recv_or_fail(&self.answers, "hub answer")
    }
    pub(crate) fn barrier(&mut self) {
        self.send("barrier", "status", json!({}));
        assert_eq!(self.next()["payload"]["command_id"], "barrier");
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        drop(self.socket.shutdown(Shutdown::Both));
        Deadline::after(DEADLINE).recv_or_fail(&self.ended, "reader ended");
    }
}
fn held(ignore_cancel: bool) -> (Arc<FakeMachine>, mpsc::Receiver<String>, mpsc::Sender<()>) {
    let machine = Arc::new(FakeMachine::default());
    let (started, starts) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    *lock(&machine.search_gate) = Some(Arc::new(SearchGate {
        started,
        release: std::sync::Mutex::new(gate),
        ignore_cancel,
        cancelled: std::sync::atomic::AtomicBool::new(false),
    }));
    (machine, starts, release)
}
fn started(starts: &mpsc::Receiver<String>, text: &str) {
    assert_eq!(
        Deadline::after(DEADLINE).recv_or_fail(starts, "walk started"),
        text
    );
}
fn stale(answer: Value, id: &str) {
    assert_eq!(answer["kind"], "command_rejected");
    assert_eq!(answer["payload"]["command_id"], id);
    assert_eq!(answer["payload"]["code"], "stale_request");
}
#[test]
fn newer_queries_supersede_walks_without_holding_other_commands() {
    for ignore in [false, true] {
        let (machine, starts, release) = held(ignore);
        let mut client = Client::new(machine, false);
        client.search("s1", "/w", "a", false);
        started(&starts, "a");
        client.search("s2", "/w", "b", false);
        client.barrier();
        release.send(()).unwrap();
        stale(client.next(), "s1");
        started(&starts, "b");
        release.send(()).unwrap();
        let answer = client.next();
        assert_eq!(answer["payload"]["command_id"], "s2");
        assert_eq!(answer["payload"]["result"], json!({"rows":["b"]}));
    }
}
#[test]
fn skipped_jobs_still_replace_the_listing_for_fresh_and_workspace_changes() {
    for (workspace, fresh, creations) in [("/w", false, 1), ("/w", true, 2), ("/w2", false, 3)] {
        let (machine, starts, release) = held(false);
        let mut client = Client::new(machine.clone(), false);
        client.search("s1", "/w", "a", false);
        started(&starts, "a");
        client.search("s2", workspace, "skipped", fresh);
        client.search("s3", "/w", "c", false);
        client.barrier();
        release.send(()).unwrap();
        stale(client.next(), "s1");
        stale(client.next(), "s2");
        started(&starts, "c");
        release.send(()).unwrap();
        assert_eq!(client.next()["payload"]["result"], json!({"rows":["c"]}));
        assert_eq!(lock(&machine.calls).len(), creations);
        assert_eq!(
            lock(&machine.calls)[0],
            json!({"search":["/w","/hub-home"]})
        );
    }
}
#[test]
fn unheld_searches_reuse_a_listing_until_fresh_or_a_workspace_change() {
    let machine = Arc::new(FakeMachine::default());
    let mut client = Client::new(machine.clone(), false);
    for (workspace, fresh, count) in [
        ("/w", false, 1),
        ("/w", false, 1),
        ("/w", true, 2),
        ("/w2", false, 3),
    ] {
        client.search("s", workspace, "x", fresh);
        assert_eq!(client.next()["payload"]["result"], json!({"rows":["x"]}));
        assert_eq!(lock(&machine.calls).len(), count);
    }
}
#[test]
fn disconnect_abandons_a_walk_before_its_rows_can_be_sent() {
    let (machine, starts, release) = held(false);
    let mut client = Client::new(machine.clone(), false);
    client.search("s", "/w", "a", false);
    started(&starts, "a");
    client.socket.shutdown(Shutdown::Both).unwrap();
    let hub = client.hub.clone();
    await_true("client disconnected", move || hub.clients() == 0);
    let gate = lock(&machine.search_gate).take().unwrap();
    release.send(()).unwrap();
    await_true("walk observed cancellation", move || {
        gate.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    });
}
#[test]
fn bad_search_arguments_never_reach_the_worker() {
    let machine = Arc::new(FakeMachine::default());
    let mut client = Client::new(machine.clone(), false);
    for args in [
        json!({}),
        json!({"workspace":null,"text":"a"}),
        json!({"workspace":"/w","text":null}),
        json!({"workspace":"relative","text":"a"}),
        json!({"workspace":"/w","text":"a","fresh":null}),
        json!({"workspace":"/w","text":"a","fresh":1}),
        json!({"workspace":"/w","text":"a","extra":true}),
    ] {
        client.send("s", "search_files", args);
        assert_eq!(client.next()["payload"]["code"], "invalid_arguments");
    }
    assert!(lock(&machine.calls).is_empty());
}
#[test]
fn failed_spawn_answers_and_a_later_query_retries() {
    let mut client = Client::new(Arc::new(FakeMachine::default()), true);
    client.search("s1", "/w", "a", false);
    assert_eq!(client.next()["payload"]["code"], "io_failed");
    *lock(&client.hub.fail_spawn) = None;
    client.search("s2", "/w", "b", false);
    assert_eq!(client.next()["payload"]["result"], json!({"rows":["b"]}));
}
