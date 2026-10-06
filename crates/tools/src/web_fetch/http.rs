//! One `web_fetch` hop over HTTP, on a socket Fiber owns so a watcher thread
//! can close it (`docs/tools.md`, "Cancellation"). ureq runs behind a
//! connector that keeps a handle to each `TcpStream` it opens; shutting that
//! handle down ends a read blocked inside ureq, under TLS too. The watcher
//! waits on the injected clock for the hop's deadline and on the call's
//! cancel, so ureq's own timeouts, which read the process clock, stay unset.
//! The chain tunnels through the proxy the environment names
//! (`docs/dependencies.md`, "Proxies").

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::Instant;

use contract::clock::{Clock, Wake};
use contract::tool::Cancel;
use ureq::Agent;
use ureq::config::Config;
use ureq::http::Uri;
use ureq::tls::{RootCerts, TlsConfig};
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    Buffers, ConnectProxyConnector, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout,
    RustlsConnector, Transport,
};

/// How many resolved addresses ureq keeps.
const MAX_ADDRS: usize = 16;

/// Which limit a hop's deadline is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Limit {
    /// The 60 seconds one request may take.
    Request,
    /// The 5 minutes the whole fetch may take.
    Fetch,
}

/// Why the watcher ended a hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    Timeout(Limit),
    Cancelled,
}

/// How [`guarded`] ended without a result of its own.
#[derive(Debug)]
pub(super) enum Ended<E> {
    /// The watcher stopped the work, which may have failed or finished.
    Stopped(Stop),
    /// The work failed first.
    Failed(E),
    /// The watcher thread could not start, so the work did not run.
    NoWatcher(io::Error),
}

#[derive(Default)]
struct State {
    /// Moves on every wake, so a wake between the watcher's checks and its
    /// wait is not lost.
    seq: u64,
    done: bool,
    stop: Option<Stop>,
    socket: Option<TcpStream>,
}

/// One hop's shared state: the watcher's wake-up, why it stopped the hop,
/// and the hop's open socket.
#[derive(Default)]
pub(super) struct Hop {
    state: Mutex<State>,
    changed: Condvar,
}

impl std::fmt::Debug for Hop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hop").finish_non_exhaustive()
    }
}

impl Wake for Hop {
    fn wake(&self) {
        let mut state = self.lock();
        state.seq = state.seq.wrapping_add(1);
        self.changed.notify_all();
    }
}

impl Hop {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Release builds abort on panic, so no holder can poison the lock.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Why the hop was stopped, if it was.
    pub(super) fn stopped(&self) -> Option<Stop> {
        self.lock().stop
    }

    /// Stops the hop for `why` unless it is already stopped, and closes its
    /// socket. A socket opened later is refused as it opens.
    fn halt(&self, why: Stop) {
        let mut state = self.lock();
        if state.stop.is_none() {
            state.stop = Some(why);
        }
        if let Some(socket) = state.socket.take() {
            // A socket the peer already closed fails to shut down, and is
            // closed either way.
            let _closed = socket.shutdown(Shutdown::Both);
        }
    }

    fn finish(&self) {
        let mut state = self.lock();
        state.done = true;
        state.seq = state.seq.wrapping_add(1);
        self.changed.notify_all();
    }

    /// Keeps a handle to `socket`, or refuses it once the hop is stopped.
    fn keep(&self, socket: &TcpStream) -> io::Result<()> {
        let mut state = self.lock();
        if state.stop.is_some() {
            return Err(io::Error::other("the fetch was stopped"));
        }
        state.socket = Some(socket.try_clone()?);
        Ok(())
    }

    /// Waits until `until` on `clock`, or a wake after `seen`.
    fn park(&self, clock: &dyn Clock, until: Instant, seen: u64) {
        // Taken before `wait_until`, and held until the condvar wait, so a
        // wake blocks on this lock instead of notifying nobody. `FnMut`
        // cannot move the guard out and back; the slot holds it across the
        // one call.
        let mut slot = Some(self.lock());
        clock.wait_until(Some(until), &mut |bound| {
            let Some(guard) = slot.take() else {
                return;
            };
            if guard.seq != seen || guard.done {
                slot = Some(guard);
                return;
            }
            slot = Some(match bound {
                Some(bound) => {
                    self.changed
                        .wait_timeout(guard, bound)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .changed
                    .wait(guard)
                    .unwrap_or_else(PoisonError::into_inner),
            });
        });
    }
}

/// Runs `work` with a watcher thread beside it, which stops the hop when
/// `deadline` passes on `clock` or `cancel` fires. A hop that starts already
/// cancelled or past its deadline never runs `work`. The watcher has ended
/// when this returns. A stop wins over whatever `work` returned, since the
/// bytes it read may be cut short.
pub(super) fn guarded<T, E>(
    clock: &Arc<dyn Clock>,
    cancel: &dyn Cancel,
    deadline: Instant,
    limit: Limit,
    work: impl FnOnce(&Arc<Hop>) -> Result<T, E>,
) -> Result<T, Ended<E>> {
    if cancel.is_cancelled() {
        return Err(Ended::Stopped(Stop::Cancelled));
    }
    if clock.now() >= deadline {
        return Err(Ended::Stopped(Stop::Timeout(limit)));
    }
    let hop = Arc::new(Hop::default());
    let waker: Weak<dyn Wake> = Arc::<Hop>::downgrade(&hop);
    clock.subscribe(Weak::clone(&waker));
    cancel.subscribe(waker);
    thread::scope(|scope| {
        let watcher = thread::Builder::new()
            .name("web-fetch-watch".to_owned())
            .spawn_scoped(scope, || {
                watch(&hop, clock.as_ref(), cancel, deadline, limit);
            })
            .map_err(Ended::NoWatcher)?;
        let result = work(&hop);
        hop.finish();
        // Builds abort on panic, so a join never carries one.
        match watcher.join() {
            Ok(()) | Err(_) => {}
        }
        // A deadline or cancel that landed while `work` ran, after the
        // watcher last checked, still stops the hop: `work` may have read
        // cut-short bytes. Cancel first, as at hop start.
        match hop.stopped() {
            Some(stop) => Err(Ended::Stopped(stop)),
            None if cancel.is_cancelled() => Err(Ended::Stopped(Stop::Cancelled)),
            None if clock.now() >= deadline => Err(Ended::Stopped(Stop::Timeout(limit))),
            None => result.map_err(Ended::Failed),
        }
    })
}

fn watch(hop: &Hop, clock: &dyn Clock, cancel: &dyn Cancel, deadline: Instant, limit: Limit) {
    loop {
        let (seen, done) = {
            let state = hop.lock();
            (state.seq, state.done)
        };
        if done {
            return;
        }
        if cancel.is_cancelled() {
            hop.halt(Stop::Cancelled);
            return;
        }
        if clock.now() >= deadline {
            hop.halt(Stop::Timeout(limit));
            return;
        }
        hop.park(clock, deadline, seen);
    }
}

/// What the hop's response said before its body.
#[derive(Debug)]
pub(super) struct Head {
    pub(super) status: u16,
    pub(super) content_type: Option<String>,
    pub(super) location: Option<String>,
}

/// What one GET asks for.
pub(super) struct Get<'a> {
    pub(super) uri: &'a Uri,
    /// The only addresses a direct connection may use. `None` when the
    /// request goes through a proxy, which resolves the target itself.
    pub(super) pinned: Option<&'a [SocketAddr]>,
    /// The proxy to tunnel through, or `None` for a direct connection.
    pub(super) proxy: Option<ureq::Proxy>,
}

impl Hop {
    /// Sends one GET, then runs `read` on the response head and body. The
    /// body is not read unless `read` reads it. A failure is the message of
    /// what went wrong, for the caller to read together with any stop.
    pub(super) fn get<T>(
        self: &Arc<Self>,
        request: &Get<'_>,
        read: impl FnOnce(Head, &mut dyn Read) -> io::Result<T>,
    ) -> Result<T, String> {
        if self.stopped().is_some() {
            return Err("the fetch was stopped".to_owned());
        }
        let tls = TlsConfig::builder()
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let config = Config::builder()
            .tls_config(tls)
            .proxy(request.proxy.clone())
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        // One agent per hop, so its connector keeps this hop's socket.
        // debt: builds the TLS config per hop; share one agent with a
        // per-hop socket slot if the handshake setup shows in a profile.
        // The proxy step runs before the socket step: it opens the proxy
        // connection by re-running the chain, so the socket this connector
        // keeps is the proxy's, and a stop still closes the tunnel.
        let connector = ConnectProxyConnector::default().chain(KeepSocket(Arc::clone(self)));
        let connector = connector.chain(RustlsConnector::default());
        let resolver = Pinned(request.pinned.map(<[SocketAddr]>::to_vec));
        let agent = Agent::with_parts(config, connector, resolver);
        let response = agent
            .get(request.uri.clone())
            .call()
            .map_err(|error| error.to_string())?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .map(|value| String::from_utf8_lossy(value.as_bytes()).trim().to_owned())
        };
        let head = Head {
            status: response.status().as_u16(),
            content_type: header("content-type"),
            location: header("location"),
        };
        let mut body = response.into_body().into_reader();
        read(head, &mut body).map_err(|error| error.to_string())
    }
}

/// Answers every lookup with the addresses fetch has already checked, so no
/// second lookup can return others (DNS rebinding). With no addresses it
/// resolves normally, which only a proxy's own address needs.
#[derive(Debug)]
struct Pinned(Option<Vec<SocketAddr>>);

impl Resolver for Pinned {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let Some(pinned) = &self.0 else {
            return DefaultResolver::default().resolve(uri, config, timeout);
        };
        let mut addrs = self.empty();
        for addr in pinned.iter().take(MAX_ADDRS) {
            addrs.push(*addr);
        }
        if addrs.is_empty() {
            Err(ureq::Error::HostNotFound)
        } else {
            Ok(addrs)
        }
    }
}

/// The connector that opens the socket and keeps a handle to it. A tunnel
/// the proxy step opened passes through untouched: the socket kept while
/// opening the proxy connection is already the one a stop must close.
// debt: this and `Socket` copy `crates/provider/src/http.rs`, because
// `tools` may not depend on `provider`; share one HTTP crate when a third
// copy is needed.
#[derive(Debug)]
struct KeepSocket(Arc<Hop>);

impl Connector<Either<(), Box<dyn Transport>>> for KeepSocket {
    type Out = Either<Box<dyn Transport>, Socket>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<Either<(), Box<dyn Transport>>>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        // debt: a connect blocked on an unreachable address is not
        // interruptible; the stop lands as soon as it returns. Connect with
        // a timeout or from an interruptible thread if a stop stuck on
        // connect is reported.
        if let Some(Either::B(tunnel)) = chained {
            return Ok(Some(Either::A(tunnel)));
        }
        let addrs: Vec<_> = details.addrs.iter().copied().collect();
        let stream = TcpStream::connect(addrs.as_slice())?;
        if details.config.no_delay() {
            stream.set_nodelay(true)?;
        }
        self.0.keep(&stream)?;
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        Ok(Some(Either::B(Socket {
            stream,
            buffers,
            open: true,
        })))
    }
}

/// A plain TCP transport over the kept socket.
#[derive(Debug)]
struct Socket {
    stream: TcpStream,
    buffers: LazyBuffers,
    /// False once a read found the peer had closed.
    open: bool,
}

impl Transport for Socket {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, _timeout: NextTimeout) -> Result<(), ureq::Error> {
        let output = self
            .buffers
            .output()
            .get(..amount)
            .ok_or_else(|| io::Error::other("ureq asked to send more than its buffer holds"))?;
        self.stream.write_all(output)?;
        Ok(())
    }

    fn await_input(&mut self, _timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let input = self.buffers.input_append_buf();
        let read = self.stream.read(input)?;
        self.buffers.input_appended(read);
        // No bytes is the peer closing: ureq reads `false` as no progress.
        self.open = read != 0;
        Ok(self.open)
    }

    fn is_open(&mut self) -> bool {
        self.open
    }
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
