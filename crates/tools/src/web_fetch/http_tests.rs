//! Tests beside [`super`]: the watcher, the socket the hop keeps, and the
//! resolver that holds a connection to the addresses already checked.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use fakes::CancelToken;
use fakes::clock::FakeClock;
use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::Resolver;
use ureq::unversioned::transport::NextTimeout;

use super::{Ended, Hop, Limit, Pinned, Stop, guarded};
use net::Keep as _;

/// How long a test waits on a socket or a waiter.
const SIGNAL: Duration = Duration::from_secs(10);

fn clock() -> (Arc<FakeClock>, Arc<dyn Clock>) {
    let fake = FakeClock::new();
    let clock: Arc<dyn Clock> = fake.clone();
    (fake, clock)
}

#[test]
fn work_that_finishes_is_returned_and_the_watcher_is_gone() {
    let (fake, clock) = clock();
    let deadline = clock.now() + Duration::from_secs(60);
    let result = guarded(
        &clock,
        &CancelToken::new(),
        deadline,
        Limit::Request,
        |_| Ok::<_, ()>(7),
    );
    assert!(matches!(result, Ok(7)));
    assert!(fake.parked().is_empty(), "no watcher is left waiting");
}

#[test]
fn work_that_fails_is_its_own_failure() {
    let (_fake, clock) = clock();
    let deadline = clock.now() + Duration::from_secs(60);
    let result = guarded(
        &clock,
        &CancelToken::new(),
        deadline,
        Limit::Request,
        |_| Err::<(), _>("broke"),
    );
    assert!(matches!(result, Err(Ended::Failed("broke"))));
}

#[test]
fn a_deadline_already_passed_runs_no_work() {
    let (_fake, clock) = clock();
    let ran = AtomicBool::new(false);
    let result = guarded(
        &clock,
        &CancelToken::new(),
        clock.now(),
        Limit::Fetch,
        |_| {
            ran.store(true, Ordering::SeqCst);
            Ok::<_, ()>(())
        },
    );
    assert!(matches!(
        result,
        Err(Ended::Stopped(Stop::Timeout(Limit::Fetch)))
    ));
    assert!(!ran.load(Ordering::SeqCst));
}

#[test]
fn a_deadline_one_tick_ahead_runs_the_work() {
    let (_fake, clock) = clock();
    let deadline = clock.now() + Duration::from_nanos(1);
    let result = guarded(
        &clock,
        &CancelToken::new(),
        deadline,
        Limit::Request,
        |_| Ok::<_, ()>(1),
    );
    assert!(matches!(result, Ok(1)));
}

/// Makes the watcher's next look at the hop see it finished, so it exits
/// without checking the cancel or the clock: once it is parked at
/// `deadline` (past its own checks), `done` is set without a wake. The
/// trigger the caller fires next is what wakes it, and the loop's first
/// check is `done`. Only the check after the join can then catch the stop.
fn miss_the_watcher(fake: &FakeClock, hop: &Hop, deadline: std::time::Instant) {
    assert!(
        fake.await_parked(deadline, Duration::from_secs(10)),
        "the watcher parks at the deadline"
    );
    hop.lock().done = true;
}

#[test]
fn a_deadline_passed_while_work_ran_stops_the_work() {
    let (fake, clock) = clock();
    let deadline = clock.now() + Duration::from_secs(60);
    let result = guarded(
        &clock,
        &CancelToken::new(),
        deadline,
        Limit::Request,
        |hop| {
            miss_the_watcher(&fake, hop, deadline);
            fake.advance(Duration::from_secs(60));
            Ok::<_, ()>(1)
        },
    );
    assert!(matches!(
        result,
        Err(Ended::Stopped(Stop::Timeout(Limit::Request)))
    ));
}

#[test]
fn a_cancel_while_work_ran_stops_the_work() {
    let (fake, clock) = clock();
    let cancel = CancelToken::new();
    let deadline = clock.now() + Duration::from_secs(60);
    let result = guarded(&clock, &cancel, deadline, Limit::Request, |hop| {
        miss_the_watcher(&fake, hop, deadline);
        cancel.cancel();
        Ok::<_, ()>(1)
    });
    assert!(matches!(result, Err(Ended::Stopped(Stop::Cancelled))));
}

#[test]
fn a_call_cancelled_before_it_starts_runs_no_work_and_wins_over_a_passed_deadline() {
    let (_fake, clock) = clock();
    let cancel = CancelToken::new();
    cancel.cancel();
    let ran = AtomicBool::new(false);
    let result = guarded(&clock, &cancel, clock.now(), Limit::Request, |_| {
        ran.store(true, Ordering::SeqCst);
        Ok::<_, ()>(())
    });
    assert!(matches!(result, Err(Ended::Stopped(Stop::Cancelled))));
    assert!(!ran.load(Ordering::SeqCst));
}

/// A connected pair: the client end for the hop to keep, and the server end.
fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    server.set_read_timeout(Some(SIGNAL)).unwrap();
    (client, server)
}

#[test]
fn a_hop_is_not_stopped_until_it_halts() {
    let hop = Hop::default();
    assert!(!hop.is_stopped(), "a fresh hop is not stopped");
    hop.halt(Stop::Cancelled);
    assert!(hop.is_stopped(), "a halted hop is stopped");
}

#[test]
fn a_stop_closes_the_kept_socket_and_refuses_a_later_one() {
    let hop = Hop::default();
    let (client, mut server) = pair();
    hop.keep(&client).unwrap();
    hop.halt(Stop::Timeout(Limit::Request));
    let mut byte = [0u8; 1];
    assert_eq!(
        server.read(&mut byte).unwrap(),
        0,
        "the peer sees the close"
    );
    assert_eq!(hop.stopped(), Some(Stop::Timeout(Limit::Request)));
    let (later, _peer) = pair();
    assert!(hop.keep(&later).is_err());
}

#[test]
fn a_socket_is_kept_until_a_stop_and_nothing_closes_it_before() {
    let hop = Hop::default();
    let (client, mut server) = pair();
    server
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    hop.keep(&client).unwrap();
    assert_eq!(hop.stopped(), None);
    let mut byte = [0u8; 1];
    let error = server.read(&mut byte).unwrap_err();
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "{error}"
    );
}

#[test]
fn the_first_stop_stays() {
    let hop = Hop::default();
    hop.halt(Stop::Timeout(Limit::Fetch));
    hop.halt(Stop::Cancelled);
    assert_eq!(hop.stopped(), Some(Stop::Timeout(Limit::Fetch)));
    let hop = Hop::default();
    hop.halt(Stop::Cancelled);
    hop.halt(Stop::Timeout(Limit::Request));
    assert_eq!(hop.stopped(), Some(Stop::Cancelled));
}

#[test]
fn a_stopped_hop_sends_no_request() {
    let hop = Arc::new(Hop::default());
    hop.halt(Stop::Cancelled);
    let uri: Uri = "http://127.0.0.1:9/".parse().unwrap();
    let request = super::Get {
        uri: &uri,
        pinned: Some(&[SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9)]),
        proxy: None,
    };
    let result = hop.get(&request, |_, _| Ok(()));
    assert!(result.is_err());
}

fn resolve(pinned: Option<Vec<SocketAddr>>, uri: &str) -> Result<Vec<SocketAddr>, ureq::Error> {
    let uri: Uri = uri.parse().unwrap();
    let timeout = NextTimeout {
        after: ureq::unversioned::transport::time::Duration::NotHappening,
        reason: ureq::Timeout::Global,
    };
    Pinned(pinned)
        .resolve(&uri, &Config::default(), timeout)
        .map(|addrs| addrs.iter().copied().collect())
}

fn address(last: u8, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, last)), port)
}

#[test]
fn the_pinned_resolver_answers_with_the_checked_addresses_whatever_the_name() {
    let pinned = vec![address(1, 80), address(2, 80)];
    assert_eq!(
        resolve(Some(pinned.clone()), "http://anything.invalid/").unwrap(),
        pinned
    );
}

#[test]
fn the_pinned_resolver_keeps_at_most_sixteen_addresses() {
    let pinned: Vec<_> = (0..20).map(|n| address(n, 80)).collect();
    let kept = resolve(Some(pinned.clone()), "http://x.invalid/").unwrap();
    assert_eq!(kept, pinned[..16]);
}

#[test]
fn the_pinned_resolver_with_no_addresses_finds_no_host() {
    let error = resolve(Some(Vec::new()), "http://x.invalid/").unwrap_err();
    assert!(matches!(error, ureq::Error::HostNotFound), "{error}");
}

#[test]
fn the_unpinned_resolver_resolves_normally() {
    let kept = resolve(None, "http://127.0.0.1:4321/").unwrap();
    assert_eq!(
        kept,
        [SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4321)]
    );
}

#[test]
fn a_hop_debug_names_the_hop() {
    assert!(
        format!("{:?}", Hop::default()).contains("Hop"),
        "the debug form names the hop"
    );
}

#[test]
fn a_wake_moves_the_sequence_so_a_wait_after_it_is_not_lost() {
    let hop = Hop::default();
    let seen = hop.lock().seq;
    hop.wake();
    assert_eq!(hop.lock().seq, seen.wrapping_add(1));
}

#[test]
fn finish_marks_the_hop_done() {
    let hop = Hop::default();
    hop.finish();
    let state = hop.lock();
    assert!(state.done);
    assert_eq!(state.seq, 1);
}

#[test]
fn park_waits_for_a_wake_before_the_deadline() {
    let (fake, clock) = clock();
    let hop = Arc::new(Hop::default());
    let until = clock.now() + Duration::from_secs(60);
    let (tx, rx) = mpsc::channel();
    let parked = Arc::clone(&hop);
    let timed = Arc::clone(&clock);
    thread::spawn(move || {
        parked.park(timed.as_ref(), until, 0);
        tx.send(()).unwrap_or(());
    });
    assert!(fake.await_parked(until, SIGNAL), "park waits on the clock");
    hop.wake();
    rx.recv_timeout(SIGNAL)
        .expect("a wake ends park before the deadline");
}

/// Parks after the wake arrived: the sequence moved, so park must not
/// wait. A mutant that needs `done` too, or that misreads the sequence,
/// parks here instead, and the deadline names it.
#[test]
fn park_returns_at_once_for_a_wake_that_arrived_first() {
    let (fake, clock) = clock();
    let hop = Arc::new(Hop::default());
    hop.wake();
    let until = clock.now() + Duration::from_secs(60);
    let (tx, rx) = mpsc::channel();
    let parked = Arc::clone(&hop);
    let timed = Arc::clone(&clock);
    thread::spawn(move || {
        parked.park(timed.as_ref(), until, 0);
        tx.send(()).unwrap_or(());
    });
    rx.recv_timeout(SIGNAL)
        .expect("a prior wake ends park at once");
    assert!(fake.parked().is_empty(), "park never waited on the clock");
}

#[test]
fn park_returns_at_once_for_a_finished_hop() {
    let (fake, clock) = clock();
    let hop = Arc::new(Hop::default());
    hop.finish();
    let seen = hop.lock().seq;
    let until = clock.now() + Duration::from_secs(60);
    let (tx, rx) = mpsc::channel();
    let parked = Arc::clone(&hop);
    let timed = Arc::clone(&clock);
    thread::spawn(move || {
        parked.park(timed.as_ref(), until, seen);
        tx.send(()).unwrap_or(());
    });
    rx.recv_timeout(SIGNAL)
        .expect("a finished hop ends park at once");
    assert!(fake.parked().is_empty(), "park never waited on the clock");
}

/// The head of one GET of `uri`, pinned to `address`. The GET blocks on
/// the network, so it runs under its own deadline and names that wait.
fn head_of(address: SocketAddr, uri: &str) -> super::Head {
    let uri: Uri = uri.parse().unwrap();
    fakes::within("the head of one GET", SIGNAL, move || {
        let request = super::Get {
            uri: &uri,
            pinned: Some(&[address]),
            proxy: None,
        };
        let hop = Arc::new(Hop::default());
        hop.get(&request, |head, _| Ok(head)).unwrap()
    })
}

#[test]
fn the_head_carries_the_stated_length() {
    let server =
        fakes::ProviderServer::start([fakes::Response::status(200, "0123456789")]).unwrap();
    let url = server.url();
    let port: u16 = url.rsplit_once(':').unwrap().1.parse().unwrap();
    let head = head_of(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        &format!("{url}/"),
    );
    assert_eq!(head.content_length, Some(10));
}

#[test]
fn the_head_of_a_body_with_no_stated_length_carries_none() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (served, done) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = fakes::within("the server accepts the request", SIGNAL, move || {
            listener.accept().unwrap()
        });
        stream.set_read_timeout(Some(SIGNAL)).unwrap();
        let mut stream = fakes::within("the server reads the request", SIGNAL, move || {
            let mut request = [0u8; 4096];
            let _read = stream.read(&mut request).unwrap();
            stream
        });
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\nabc")
            .unwrap();
        served.send(()).unwrap();
    });
    let head = head_of(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        &format!("http://127.0.0.1:{port}/"),
    );
    done.recv_timeout(SIGNAL).expect("the server answered");
    assert_eq!(head.content_length, None);
}
