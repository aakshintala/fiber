//! The CONNECT protocol handoff (`docs/testing.md`, "Fakes"): dialling
//! within the trust boundary, the bounded 200 handshake, and stop-aware
//! tunnel transport in both directions. Registration is atomic with the
//! shutdown check, so a shutdown racing it still reaps both copies.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::thread;

use super::connection::{Connection, ReadErr, WaitCtx, classify, pump};
use super::{HttpServer, READ_CHUNK, RequestBudget, WaitEnd, expired, lock};

impl Connection {
    /// Dials a CONNECT target within the trust boundary: a loopback
    /// address or localhost, connected with the remaining handshake
    /// guard. Anything else, or a refusal, is a silent error before any
    /// record, as a failed dial always was.
    pub(crate) fn dial(&self, target: &str) -> io::Result<TcpStream> {
        let op = self.op_budget();
        let addr = HttpServer::check_target(target)?;
        TcpStream::connect_timeout(&addr, op.guard_left().min(self.shared.limits.request))
    }

    /// Runs a CONNECT tunnel to `target` over this connection: the 200
    /// handshake and the bytes read past the head within one bound,
    /// then one named bounded copy per direction until either side
    /// closes. Registration is atomic with the shutdown check, so a
    /// shutdown racing it still reaps both directions; the end names
    /// which side closed first.
    pub(crate) fn tunnel(self, target: &str, origin: TcpStream) -> TunnelEnd {
        let (mut stream, ahead, shared) = self.into_tunnel_parts();
        let op = RequestBudget::start_on(&shared.clock, shared.limits.request, shared.limits.guard);
        // Nonblocking throughout: every byte below bounds itself through
        // the pump, so a slow peer waits out the handshake's budget
        // instead of blocking the tunnel thread past it.
        let mut origin = origin;
        // The 200 and the coalesced bytes within the handshake's budget.
        let mut seq = 0;
        let mut acked = None;
        let mut ctx = WaitCtx {
            budget: &op,
            name: target,
            shared: &shared,
            seq: &mut seq,
            acked: &mut acked,
        };
        if !tunnel_handshake(&mut stream, &mut origin, &ahead, &mut ctx) {
            return TunnelEnd::Stopped;
        }
        // Check-then-register under one lock: shutdown sets stopping
        // before it drains the registry, so either that drain includes
        // this tunnel's break sockets, or the claim below sees stopping
        // and no copy starts. A shutdown racing either way still reaps
        // both directions.
        let id = match shared.register_tunnel(&stream, &origin) {
            Some(id) => id,
            None => return TunnelEnd::Stopped,
        };
        // One named bounded operation per copy direction: each ends when
        // its source closes, when shutdown stops its wait, or past its
        // deadline naming the target. Whoever's copy ends first names
        // the end; both directions break on the first end. 1 names the
        // client, 2 the origin.
        let first = Arc::new(AtomicU8::new(0));
        let first_pump = Arc::clone(&first);
        let mut origin_pump = match origin.try_clone() {
            Ok(origin) => origin,
            Err(_) => return TunnelEnd::OriginClosed,
        };
        let mut client_pump = match stream.try_clone() {
            Ok(client) => client,
            Err(_) => return TunnelEnd::OriginClosed,
        };
        let copying = Arc::clone(&shared);
        let copied = target.to_owned();
        let pump_end = thread::Builder::new()
            .name("fakes-http-tunnel".to_owned())
            .spawn(move || {
                let op = RequestBudget::start_on(
                    &copying.clock,
                    copying.limits.request,
                    copying.limits.guard,
                );
                let mut seq = 0;
                let mut acked = None;
                let mut ctx = WaitCtx {
                    budget: &op,
                    name: &copied,
                    shared: &copying,
                    seq: &mut seq,
                    acked: &mut acked,
                };
                tunnel_copy(
                    &mut origin_pump,
                    &mut client_pump,
                    "to copy the origin to the client",
                    &mut ctx,
                );
                match first_pump.compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst) {
                    Ok(_) | Err(_) => {}
                }
                match client_pump.shutdown(std::net::Shutdown::Both) {
                    Ok(()) | Err(_) => {}
                }
                match origin_pump.shutdown(std::net::Shutdown::Both) {
                    Ok(()) | Err(_) => {}
                }
            });
        {
            let op =
                RequestBudget::start_on(&shared.clock, shared.limits.request, shared.limits.guard);
            let mut seq = 0;
            let mut acked = None;
            let mut ctx = WaitCtx {
                budget: &op,
                name: target,
                shared: &shared,
                seq: &mut seq,
                acked: &mut acked,
            };
            tunnel_copy(
                &mut stream,
                &mut origin,
                "to copy the client to the origin",
                &mut ctx,
            );
        }
        match first.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) | Err(_) => {}
        }
        match stream.shutdown(std::net::Shutdown::Both) {
            Ok(()) | Err(_) => {}
        }
        match origin.shutdown(std::net::Shutdown::Both) {
            Ok(()) | Err(_) => {}
        }
        if let Ok(pump_end) = pump_end {
            match pump_end.join() {
                Ok(()) | Err(_) => {}
            }
        }
        lock(&shared.tunnels)
            .live
            .retain(|(live, _, _)| *live != id);
        if shared.stopping.load(Ordering::SeqCst) {
            TunnelEnd::Stopped
        } else if first.load(Ordering::SeqCst) == 1 {
            TunnelEnd::ClientClosed
        } else {
            TunnelEnd::OriginClosed
        }
    }
}

/// How one tunnel ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TunnelEnd {
    /// The client side closed first.
    ClientClosed,
    /// The origin side closed first.
    OriginClosed,
    /// Shutdown ended the tunnel.
    Stopped,
}

/// The tunnel's 200 handshake and the bytes read past the head within
/// the wait context's operation: false when shutdown ends the wait.
/// Expiry panics naming the target and phase. The test drives this
/// with a finite writer past its capacity, since sockets alone cannot
/// guarantee send-buffer exhaustion.
pub(crate) fn tunnel_handshake(
    client: &mut dyn Write,
    origin: &mut dyn Write,
    ahead: &[u8],
    ctx: &mut WaitCtx<'_>,
) -> bool {
    if !pump(
        client,
        b"HTTP/1.1 200 Connection Established\r\n\r\n",
        "to write the tunnel handshake",
        ctx,
    ) {
        return false;
    }
    pump(origin, ahead, "to forward the bytes past the head", ctx)
}

/// One tunnel direction: bytes from `src` to `dst` within the wait
/// context's operation, a named bounded operation with a deadline.
/// Ends when `src` closes or errors, when shutdown stops the wait, or
/// past the deadline naming the target and phase; the caller marks the
/// end and breaks both sockets, so the other direction ends too.
fn tunnel_copy(
    src: &mut TcpStream,
    dst: &mut TcpStream,
    phase: &'static str,
    ctx: &mut WaitCtx<'_>,
) {
    let mut piece = Vec::new();
    loop {
        let mut chunk = [0u8; READ_CHUNK];
        match src.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => {
                piece.clear();
                piece.extend(chunk.into_iter().take(read));
                if !pump(dst, &piece, phase, ctx) {
                    return;
                }
            }
            Err(error) => match classify(&error) {
                ReadErr::Wait => {}
                ReadErr::Retry => continue,
                ReadErr::End => return,
            },
        }
        match ctx
            .shared
            .wait(ctx.budget, ctx.name, phase, ctx.seq, ctx.acked)
        {
            WaitEnd::Waited => {}
            WaitEnd::Expired => expired(ctx.budget, ctx.name, phase),
            WaitEnd::Stopped => return,
        }
    }
}
