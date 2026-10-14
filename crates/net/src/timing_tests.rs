//! One actual connector attempt group timed through the installed
//! callable (`docs/state.md`, "Diagnostic logs"): the observed agent
//! starts one `net.connect` scope around the addresses it really tries,
//! while the plain agent, an empty address list, a stop before the first
//! attempt and an already-built tunnel start none.

use std::io::{self, Read};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::diag::{Op, Timing};
use fakes::Deadline;
use ureq::unversioned::resolver::{DefaultResolver, Resolver};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, Transport,
};

use super::{KeepSocket, open, open_timed};
use crate::{Keep, LIMITS};

/// One test's wall-clock bound. Every wait names it.
const EACH_WITHIN: Duration = Duration::from_secs(5);

/// One invocation's arguments: the operation, the tool and whether an
/// explicit start stamp was supplied, with the start the spy read itself.
struct Call {
    op: Op,
    tool: Option<String>,
    started: bool,
}

/// A capture Timing: records each invocation and each owned guard's drop
/// stamp, so a test pins exactly one scope per attempt group. The guard
/// is owned, never stored: after the exchange exactly one drop exists.
struct Capture {
    clock: Arc<fakes::clock::FakeClock>,
    calls: Mutex<Vec<Call>>,
    ends: Arc<Mutex<Vec<Instant>>>,
}

impl Capture {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            clock: fakes::clock::FakeClock::new(),
            calls: Mutex::new(Vec::new()),
            ends: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn timing(self: &Arc<Self>) -> Timing {
        let me = Arc::clone(self);
        Arc::new(move |op, tool, started| {
            me.calls.lock().unwrap().push(Call {
                op,
                tool: tool.map(str::to_owned),
                started: started.is_some(),
            });
            let ends = Arc::clone(&me.ends);
            let clock = Arc::clone(&me.clock);
            let guard = Guard { clock, ends };
            Some(Box::new(guard) as Box<dyn Send>)
        })
    }

    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn ends(&self) -> usize {
        self.ends.lock().unwrap().len()
    }
}

/// The capture's owned guard: dropping it stamps the fake clock's end.
struct Guard {
    clock: Arc<fakes::clock::FakeClock>,
    ends: Arc<Mutex<Vec<Instant>>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.ends.lock().unwrap().push(self.clock.now());
    }
}

/// Builds an observed agent over `keep` with `capture` installed, and no
/// proxy whatever the developer's shell names.
fn observed_agent(keep: Arc<dyn Keep>, capture: &Arc<Capture>) -> ureq::Agent {
    crate::agent_observed(
        crate::config().proxy(None).build(),
        keep,
        DefaultResolver::default(),
        LIMITS,
        capture.timing(),
    )
}

/// GETs `url` on its own thread under [`EACH_WITHIN`], reading the body
/// to a string on success.
fn get(agent: ureq::Agent, url: String) -> Result<String, ureq::Error> {
    fakes::within("the observed GET to answer", EACH_WITHIN, move || {
        let mut body = agent.get(url).call()?.into_body().into_reader();
        let mut text = String::new();
        body.read_to_string(&mut text).map_err(ureq::Error::Io)?;
        Ok(text)
    })
}

/// A keep that holds a handle to the socket it was handed.
#[derive(Debug, Default)]
struct Recording {
    kept: Mutex<Option<TcpStream>>,
}

impl Keep for Recording {
    fn keep(&self, socket: &TcpStream) -> io::Result<()> {
        *self.kept.lock().unwrap() = Some(socket.try_clone()?);
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        false
    }
}

#[test]
fn an_observed_agent_times_its_one_connector_scope() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let url = server.url();
    let capture = Capture::new();
    let keep = Arc::new(Recording::default());
    let text = get(observed_agent(keep.clone(), &capture), url.clone()).unwrap();
    assert_eq!(text, "{}", "the observed agent read the fake's response");
    assert_eq!(server.requests().len(), 1, "the fake saw the GET");
    let calls = capture.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "one group starts one scope");
    assert_eq!(calls[0].op, Op::NetConnect, "the scope names the connector");
    assert_eq!(calls[0].tool, None, "a connector scope names no tool");
    assert!(!calls[0].started, "the scope stamps its own start");
    assert_eq!(capture.ends(), 1, "the owned guard dropped once");
    let kept = keep.kept.lock().unwrap();
    let kept = kept.as_ref().expect("the keep was handed the socket");
    let port: u16 = url
        .rsplit(':')
        .next()
        .unwrap()
        .trim_end_matches('/')
        .parse()
        .unwrap();
    assert_eq!(
        kept.peer_addr().unwrap().port(),
        port,
        "the handed socket talks to the fake"
    );
}

#[test]
fn a_plain_agent_reads_the_same_body_with_no_timer() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let url = server.url();
    let text = get(
        crate::agent(
            crate::config().proxy(None).build(),
            Arc::new(()),
            DefaultResolver::default(),
            LIMITS,
        ),
        url,
    )
    .unwrap();
    assert_eq!(text, "{}", "the plain agent reads the same body");
    assert_eq!(server.requests().len(), 1, "the fake saw the GET");
}

#[test]
fn building_an_observed_agent_starts_no_scope() {
    let capture = Capture::new();
    drop(observed_agent(Arc::new(()), &capture));
    assert_eq!(capture.calls(), 0, "construction alone invokes nothing");
    assert_eq!(capture.ends(), 0, "nothing started, nothing dropped");
}

#[test]
fn a_refused_observed_call_fails_refused_and_emits_once() {
    let capture = Capture::new();
    let error = get(
        observed_agent(Arc::new(()), &capture),
        fakes::refused::url(),
    )
    .unwrap_err();
    let ureq::Error::Io(refused) = &error else {
        panic!("a refused call fails as io: {error}");
    };
    assert_eq!(
        refused.kind(),
        io::ErrorKind::ConnectionRefused,
        "a refused call fails refused: {error}"
    );
    assert!(
        !crate::timed_out(&error),
        "a refused peer is not a timeout: {error}"
    );
    assert_eq!(capture.calls(), 1, "a failed group still started once");
    assert_eq!(capture.ends(), 1, "a failed group still dropped once");
}

/// A resolver returning `addrs` for every name, so one group walks past
/// a refused address to a live one.
#[derive(Debug, Clone)]
struct FixedAddrs(Vec<SocketAddr>);

impl Resolver for FixedAddrs {
    fn resolve(
        &self,
        _uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ArrayVec<SocketAddr, 16>, ureq::Error> {
        let mut addrs =
            ureq::unversioned::resolver::ArrayVec::from_fn(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        addrs.truncate(0);
        for addr in &self.0 {
            addrs.push(*addr);
        }
        Ok(addrs)
    }
}

/// The socket address `server` listens on.
fn socket_of(server: &fakes::ProviderServer) -> SocketAddr {
    server.url()["http://".len()..].parse().unwrap()
}

#[test]
fn one_group_over_two_addresses_times_once() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let live = socket_of(&server);
    let refused = SocketAddr::from(fakes::refused::ADDR);
    let capture = Capture::new();
    let agent = crate::agent_observed(
        crate::config().proxy(None).build(),
        Arc::new(()),
        FixedAddrs(vec![refused, live]),
        LIMITS,
        capture.timing(),
    );
    let text = get(agent, "http://fiber.invalid/".to_owned()).unwrap();
    assert_eq!(text, "{}", "the group reached the listener past refusal");
    assert_eq!(capture.calls(), 1, "two addresses are one group, not two");
    assert_eq!(capture.ends(), 1, "one group drops once");
}

#[test]
fn open_timed_over_two_refused_addresses_fails_once_with_the_last_refusal() {
    let capture = Capture::new();
    let refused = SocketAddr::from(fakes::refused::ADDR);
    let timing = capture.timing();
    let error = fakes::within(
        "an observed open past two refusals to fail",
        EACH_WITHIN,
        move || {
            open_timed(&[refused, refused], &(), LIMITS, wait(), Some(&timing))
                .map(|_| ())
                .unwrap_err()
        },
    );
    let ureq::Error::Io(refused_error) = &error else {
        panic!("two refusals fail as io: {error}");
    };
    assert_eq!(
        refused_error.kind(),
        io::ErrorKind::ConnectionRefused,
        "the group reports the last refusal: {error}"
    );
    assert_eq!(capture.calls(), 1, "two refusals start one scope");
    assert_eq!(capture.ends(), 1, "two refusals drop one scope");
}

#[test]
fn open_timed_with_no_addresses_times_nothing() {
    let capture = Capture::new();
    let timing = capture.timing();
    let timed = fakes::within("an observed empty open to fail", EACH_WITHIN, move || {
        open_timed(&[], &(), LIMITS, wait(), Some(&timing))
            .map(|_| ())
            .unwrap_err()
    });
    let plain = fakes::within("a plain empty open to fail", EACH_WITHIN, move || {
        open(&[], &(), LIMITS, wait()).map(|_| ()).unwrap_err()
    });
    assert_eq!(
        format!("{timed:?}"),
        format!("{plain:?}"),
        "the empty list keeps its ordinary error"
    );
    assert_eq!(capture.calls(), 0, "no address starts no scope");
    assert_eq!(capture.ends(), 0, "no scope drops nothing");
}

/// A keep stopped before the open starts.
#[derive(Debug, Default)]
struct AlreadyStopped;

impl Keep for AlreadyStopped {
    fn keep(&self, _socket: &TcpStream) -> io::Result<()> {
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        true
    }
}

#[test]
fn open_timed_that_starts_stopped_times_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let capture = Capture::new();
    let timing = capture.timing();
    let error = fakes::within("a stopped observed open to fail", EACH_WITHIN, move || {
        open_timed(&[addr], &AlreadyStopped, LIMITS, wait(), Some(&timing))
            .map(|_| ())
            .unwrap_err()
    });
    assert!(is_stopped(&error), "a stopped open fails stopped: {error}");
    assert_eq!(
        capture.calls(),
        0,
        "a stop before the first attempt starts nothing"
    );
    assert_eq!(capture.ends(), 0, "nothing started, nothing dropped");
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock,
        "a stopped open connects nowhere"
    );
}

/// A keep that lets the first stop check pass and stops every later one.
#[derive(Debug, Default)]
struct StopsAfterFirstCheck {
    seen: std::sync::atomic::AtomicBool,
}

impl Keep for StopsAfterFirstCheck {
    fn keep(&self, _socket: &TcpStream) -> io::Result<()> {
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        self.seen.swap(true, Ordering::SeqCst)
    }
}

#[test]
fn open_timed_stops_before_its_remaining_addresses_and_emits_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let refused = SocketAddr::from(fakes::refused::ADDR);
    let capture = Capture::new();
    let timing = capture.timing();
    let error = fakes::within(
        "an observed open stopped mid-walk to fail",
        EACH_WITHIN,
        move || {
            open_timed(
                &[refused, addr],
                &StopsAfterFirstCheck::default(),
                LIMITS,
                wait(),
                Some(&timing),
            )
            .map(|_| ())
            .unwrap_err()
        },
    );
    assert!(
        is_stopped(&error),
        "an open stopped mid-walk fails stopped: {error}"
    );
    assert_eq!(
        capture.calls(),
        1,
        "the refused first attempt started one scope"
    );
    assert_eq!(capture.ends(), 1, "the stop drops that scope once");
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock,
        "the listener saw no connection"
    );
}

/// A keep that refuses every socket it is handed, holding the clone.
#[derive(Debug, Default)]
struct Refuses {
    kept: Mutex<Option<TcpStream>>,
}

impl Keep for Refuses {
    fn keep(&self, socket: &TcpStream) -> io::Result<()> {
        *self.kept.lock().unwrap() = Some(socket.try_clone()?);
        Err(io::Error::other("a keep that refuses for the test"))
    }

    fn is_stopped(&self) -> bool {
        false
    }
}

#[test]
fn a_keep_refusal_emits_once_and_closes_the_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (peer_tx, peer_rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((peer, _)) = listener.accept() {
            match peer_tx.send(peer) {
                Ok(()) | Err(_) => {}
            }
        }
    });
    let capture = Capture::new();
    let keep = Arc::new(Refuses::default());
    let error = get(
        crate::agent_observed(
            crate::config().proxy(None).build(),
            keep.clone(),
            FixedAddrs(vec![addr]),
            LIMITS,
            capture.timing(),
        ),
        format!("http://{addr}/"),
    )
    .unwrap_err();
    let ureq::Error::Io(refused) = &error else {
        panic!("a refused keep fails the open as io: {error}");
    };
    assert_eq!(
        refused.to_string(),
        "a keep that refuses for the test",
        "the keep's refusal is the error"
    );
    assert_eq!(
        capture.calls(),
        1,
        "the refused socket still started one scope"
    );
    assert_eq!(capture.ends(), 1, "the refusal drops that scope once");
    drop(keep);
    let mut peer = Deadline::after(EACH_WITHIN)
        .recv(&peer_rx)
        .expect("the listener saw the refused socket");
    peer.set_read_timeout(Some(EACH_WITHIN)).unwrap();
    let mut drained = Vec::new();
    peer.read_to_end(&mut drained).unwrap();
    assert!(
        drained.is_empty(),
        "the refusal closed the socket before anything was sent: {drained:?}"
    );
}

/// A keep that holds its cancellation lock across the decision: `keep`
/// signals entry, waits for the test's release, then refuses when the
/// test cancelled meanwhile. The refusal is decided under the same lock
/// the decision takes, so the timing assertion below is deterministic,
//    not scheduler luck.
struct CancelOnKeep {
    lock: Arc<Mutex<()>>,
    kept: Mutex<Option<TcpStream>>,
    cancelled: std::sync::atomic::AtomicBool,
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl std::fmt::Debug for CancelOnKeep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelOnKeep").finish_non_exhaustive()
    }
}

impl Keep for CancelOnKeep {
    fn keep(&self, socket: &TcpStream) -> io::Result<()> {
        let held = self.lock.lock().unwrap();
        *self.kept.lock().unwrap() = Some(socket.try_clone()?);
        match self.entered.send(()) {
            Ok(()) | Err(_) => {}
        }
        let release = self.release.lock().unwrap();
        Deadline::after(EACH_WITHIN)
            .recv(&release)
            .expect("the test releases the keep");
        drop(release);
        drop(held);
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(io::Error::other("the call was cancelled"));
        }
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[test]
fn cancelling_during_keep_refuses_after_release_and_emits_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (peer_tx, peer_rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((peer, _)) = listener.accept() {
            match peer_tx.send(peer) {
                Ok(()) | Err(_) => {}
            }
        }
    });
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let guard_lock = Arc::new(Mutex::new(()));
    let keep = Arc::new(CancelOnKeep {
        lock: Arc::clone(&guard_lock),
        kept: Mutex::new(None),
        cancelled: std::sync::atomic::AtomicBool::new(false),
        entered: entered_tx,
        release: Mutex::new(release_rx),
    });
    // The guard must drop after `keep` released its lock: the drop
    // tries the lock, which fails while `keep` still holds it.
    let drops = Arc::new(AtomicUsize::new(0));
    let drop_locks = Arc::clone(&guard_lock);
    let drop_count = Arc::clone(&drops);
    let timing: Timing = Arc::new(move |op, tool, started| {
        assert_eq!(op, Op::NetConnect, "the scope names the connector");
        assert_eq!(tool, None, "a connector scope names no tool");
        assert_eq!(started, None, "the scope stamps its own start");
        let drop_locks = Arc::clone(&drop_locks);
        let drop_count = Arc::clone(&drop_count);
        struct Check {
            locks: Arc<Mutex<()>>,
            count: Arc<AtomicUsize>,
        }
        impl Drop for Check {
            fn drop(&mut self) {
                assert!(
                    self.locks.try_lock().is_ok(),
                    "the scope drops after the keep lock is released"
                );
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }
        let guard = Check {
            locks: drop_locks,
            count: drop_count,
        };
        Some(Box::new(guard) as Box<dyn Send>)
    });
    let worker = keep.clone();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let error = crate::agent_observed(
            crate::config().proxy(None).build(),
            worker,
            FixedAddrs(vec![addr]),
            LIMITS,
            timing,
        )
        .get(format!("http://{addr}/"))
        .call()
        .map(|_| ())
        .unwrap_err();
        match done_tx.send(error.to_string()) {
            Ok(()) | Err(_) => {}
        }
    });
    Deadline::after(EACH_WITHIN)
        .recv(&entered_rx)
        .expect("the keep saw the connected socket");
    keep.cancelled.store(true, Ordering::SeqCst);
    release_tx.send(()).unwrap();
    let failed = Deadline::after(EACH_WITHIN)
        .recv(&done_rx)
        .expect("the cancelled call returns");
    assert!(
        failed.contains("the call was cancelled"),
        "the keep's refusal is the error: {failed}"
    );
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the cancel drops one scope"
    );
    drop(keep);
    let mut peer = Deadline::after(EACH_WITHIN)
        .recv(&peer_rx)
        .expect("the listener saw the cancelled socket");
    peer.set_read_timeout(Some(EACH_WITHIN)).unwrap();
    let mut drained = Vec::new();
    peer.read_to_end(&mut drained).unwrap();
    assert!(
        drained.is_empty(),
        "the refusal closed the socket before anything was sent: {drained:?}"
    );
}

/// A keep whose methods fail the test when invoked: the tunnel path must
/// touch neither.
#[derive(Debug, Default)]
struct Untouched;

impl Keep for Untouched {
    fn keep(&self, _socket: &TcpStream) -> io::Result<()> {
        panic!("a tunnelled connect keeps no socket")
    }

    fn is_stopped(&self) -> bool {
        panic!("a tunnelled connect checks no stop")
    }
}

/// A transport standing in for a proxy-built tunnel.
#[derive(Debug)]
struct DummyTransport {
    buffers: LazyBuffers,
}

impl Transport for DummyTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(
        &mut self,
        _amount: usize,
        _timeout: NextTimeout,
    ) -> Result<(), ureq::Error> {
        Ok(())
    }

    fn await_input(&mut self, _timeout: NextTimeout) -> Result<bool, ureq::Error> {
        Ok(false)
    }

    fn is_open(&mut self) -> bool {
        false
    }
}

fn wait() -> NextTimeout {
    NextTimeout {
        after: ureq::unversioned::transport::time::Duration::NotHappening,
        reason: ureq::Timeout::Global,
    }
}

/// Whether `error` is an open failing on a stopped call.
fn is_stopped(error: &ureq::Error) -> bool {
    matches!(
        error,
        ureq::Error::Io(stopped)
            if matches!(
                stopped
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<crate::Error>()),
                Some(crate::Error::Stopped)
            )
    )
}

#[test]
fn an_already_built_tunnel_passes_through_untouched() {
    let uri: ureq::http::Uri = "http://127.0.0.1/".parse().unwrap();
    let config = crate::config().proxy(None).build();
    let resolver = DefaultResolver::default();
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.origin();
    let details = ConnectionDetails {
        uri: &uri,
        addrs: resolver.empty(),
        config: &config,
        request_level: false,
        resolver: &resolver,
        now: ureq::unversioned::transport::time::Instant::Exact(origin),
        timeout: wait(),
        current_time: Arc::new(move || ureq::unversioned::transport::time::Instant::Exact(origin)),
        run_connector: Arc::new(|_| Err(ureq::Error::HostNotFound)),
    };
    let panic_timing: Timing = Arc::new(|_, _, _| panic!("a tunnelled connect starts no scope"));
    let socket = KeepSocket::new(Arc::new(Untouched), LIMITS).with_timing(panic_timing);
    let tunnel: Box<dyn Transport> = Box::new(DummyTransport {
        buffers: LazyBuffers::new(1024, 1024),
    });
    let before = std::ptr::from_ref::<dyn Transport>(&*tunnel);
    let connected = socket
        .connect(&details, Some(Either::B(tunnel)))
        .expect("a tunnel passes through");
    let Some(Either::A(back)) = connected else {
        panic!("a tunnel passes through as a tunnel");
    };
    let after = std::ptr::from_ref::<dyn Transport>(&*back);
    assert!(
        std::ptr::addr_eq(before, after),
        "the same tunnel transport survives"
    );
}

/// The `host:port` the proxy records for a call to `server`.
fn target_of(server: &fakes::ProviderServer) -> String {
    let url = server.url();
    let port = url.rsplit(':').next().unwrap();
    format!("127.0.0.1:{port}")
}

/// A proxy value pointing at `proxy`, bypassing nothing.
fn proxy_through(proxy: &fakes::ConnectProxy) -> ureq::Proxy {
    ureq::Proxy::builder(ureq::ProxyProtocol::Http)
        .host("127.0.0.1")
        .port(proxy.port())
        .build()
        .unwrap()
}

#[test]
fn an_observed_proxy_call_times_its_one_socket() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let target = target_of(&server);
    let url = format!("{}/v1", server.url());
    let capture = Capture::new();
    let keep = Arc::new(Recording::default());
    let worker = keep.clone();
    let timing = capture.timing();
    let through = proxy_through(&proxy);
    let text = fakes::within("the observed proxy GET to answer", EACH_WITHIN, move || {
        let agent = crate::agent_observed(
            crate::config().proxy(Some(through)).build(),
            worker,
            DefaultResolver::default(),
            LIMITS,
            timing,
        );
        let mut body = agent.get(url).call().unwrap().into_body().into_reader();
        let mut text = String::new();
        body.read_to_string(&mut text).unwrap();
        text
    });
    assert_eq!(text, "{}", "the proxy call reached the origin");
    assert!(
        proxy.await_connects(1, EACH_WITHIN),
        "the proxy recorded CONNECT {target}"
    );
    assert_eq!(proxy.connects(), [target]);
    assert_eq!(capture.calls(), 1, "the proxy socket is the one scope");
    assert_eq!(capture.ends(), 1, "the proxy scope drops once");
    let kept = keep.kept.lock().unwrap();
    assert_eq!(
        kept.as_ref()
            .expect("the keep was handed the proxy socket")
            .peer_addr()
            .unwrap()
            .port(),
        proxy.port(),
        "the kept socket is the proxy's"
    );
}

#[test]
fn a_proxy_refusing_connect_still_times_the_one_socket() {
    let proxy = fakes::ConnectProxy::start().unwrap();
    let refused = SocketAddr::from(fakes::refused::ADDR);
    let url = format!("http://{refused}/");
    let through = proxy_through(&proxy);
    let timed = fakes::within("the refused proxy call to fail", EACH_WITHIN, move || {
        let capture = Capture::new();
        let error = crate::agent_observed(
            crate::config().proxy(Some(through)).build(),
            Arc::new(()),
            DefaultResolver::default(),
            LIMITS,
            capture.timing(),
        )
        .get(url)
        .call()
        .unwrap_err();
        (format!("{error:?}"), capture.calls(), capture.ends())
    });
    let plain = fakes::within(
        "the plain refused proxy call to fail",
        EACH_WITHIN,
        move || {
            crate::agent(
                crate::config().proxy(Some(proxy_through(&proxy))).build(),
                Arc::new(()),
                DefaultResolver::default(),
                LIMITS,
            )
            .get(format!("http://{refused}/"))
            .call()
            .map(|_| ())
            .unwrap_err()
        },
    );
    assert_eq!(
        timed.0,
        format!("{plain:?}"),
        "the proxy refusal keeps its ordinary failure"
    );
    assert_eq!(timed.1, 1, "the refused tunnel still had one socket scope");
    assert_eq!(timed.2, 1, "that scope drops once");
}

#[test]
fn a_no_proxy_bypass_times_the_direct_socket() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let bypass = ureq::Proxy::builder(ureq::ProxyProtocol::Http)
        .host("127.0.0.1")
        .port(proxy.port())
        .no_proxy("127.0.0.1")
        .build()
        .unwrap();
    let url = format!("{}/v1", server.url());
    let capture = Capture::new();
    let timing = capture.timing();
    let text = fakes::within("the bypassed GET to answer", EACH_WITHIN, move || {
        let agent = crate::agent_observed(
            crate::config().proxy(Some(bypass)).build(),
            Arc::new(()),
            DefaultResolver::default(),
            LIMITS,
            timing,
        );
        let mut body = agent.get(url).call().unwrap().into_body().into_reader();
        let mut text = String::new();
        body.read_to_string(&mut text).unwrap();
        text
    });
    assert_eq!(text, "{}", "the bypass reached the origin directly");
    assert!(
        proxy.connects().is_empty(),
        "nothing went through the proxy"
    );
    assert_eq!(capture.calls(), 1, "the direct socket is the one scope");
    assert_eq!(capture.ends(), 1, "the direct scope drops once");
}
