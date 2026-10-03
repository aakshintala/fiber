//! The fake provider server (`docs/testing.md`, "Model calls"): a local
//! HTTP/1.1 server that answers each request with the next response of its
//! script and records every request it receives.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Header names whose values are credentials, lowercase. Their values are
/// replaced by a fingerprint before a request is recorded, as is a `key`
/// query parameter, which Google's API also accepts (`docs/model-routing.md`).
const CREDENTIAL_HEADERS: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-goog-api-key",
    "api-key",
    "cookie",
];

/// A credential's fingerprint: `sha256:` and the first 8 lowercase hex digits
/// of SHA-256 over `value`, so a recording can be asserted without holding
/// the credential.
pub fn fingerprint(value: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, value.as_bytes());
    let hex: String = digest
        .as_ref()
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// One scripted response: a status, headers and the body bytes, sent as they
/// are. A recorded stream is served by passing its bytes as the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status code.
    pub status: u16,
    /// Response headers in the order they are sent. The server adds
    /// `content-length` and `connection: close` itself.
    pub headers: Vec<(String, String)>,
    /// The body bytes, byte for byte.
    pub body: Vec<u8>,
}

impl Response {
    /// A 200 server-sent-events stream with these bytes as its body.
    pub fn stream(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".to_owned(), "text/event-stream".to_owned())],
            body: body.into(),
        }
    }

    /// A JSON response with this status, such as a 429 or a 500 error body.
    pub fn status(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: body.into(),
        }
    }

    /// The same response with one more header, such as `retry-after`.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// One request the server received, with credentials replaced by their
/// fingerprints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The method, such as `POST`.
    pub method: String,
    /// The request target: the path and any query, with a `key` query
    /// parameter's value replaced by its fingerprint.
    pub path: String,
    /// Headers in the order received, names lowercased, credential values
    /// replaced by their fingerprints.
    pub headers: Vec<(String, String)>,
    /// The body bytes, as received.
    pub body: Vec<u8>,
}

impl Request {
    /// The value of the first header with this lowercase name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct State {
    script: VecDeque<Response>,
    requests: Vec<Request>,
    stopping: bool,
    /// When set, a recorded request is not answered until [`ProviderServer::release`].
    hold: bool,
}

/// A fake provider listening on a local port. Each request gets the next
/// response of the script, and a request past its end gets a 500 saying so.
/// Dropping the server closes its port.
pub struct ProviderServer {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    arrived: Arc<Condvar>,
    accept: Option<JoinHandle<()>>,
}

impl ProviderServer {
    /// Listens on a free port on 127.0.0.1 and serves `script` in order. The
    /// port accepts connections when this returns.
    pub fn start(script: impl IntoIterator<Item = Response>) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State {
            script: script.into_iter().collect(),
            ..State::default()
        }));
        let arrived = Arc::new(Condvar::new());
        let shared = Arc::clone(&state);
        let wake = Arc::clone(&arrived);
        let accept = thread::Builder::new()
            .name("fake-provider".to_owned())
            .spawn(move || accept_loop(&listener, &shared, &wake))?;
        Ok(Self {
            addr,
            state,
            arrived,
            accept: Some(accept),
        })
    }

    /// The base URL, such as `http://127.0.0.1:49152`, for a provider
    /// definition's base URL.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every request received so far, in arrival order. A request is
    /// recorded before its response is sent, so once a client has its
    /// response the request is here.
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.state).requests.clone()
    }

    /// Waits, at most `within` of real time, until at least `count` requests
    /// are recorded. True once they are; false at the deadline. Dropping the
    /// server ends no wait early.
    pub fn await_requests(&self, count: usize, within: Duration) -> bool {
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.requests.len() < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.requests.len() >= count
    }

    /// Holds every response until [`ProviderServer::release`]. A request is
    /// still recorded first, so [`ProviderServer::await_requests`] sees it
    /// while the client waits for the body.
    pub fn hold(&self) {
        lock(&self.state).hold = true;
    }

    /// Sends the responses [`ProviderServer::hold`] is holding.
    pub fn release(&self) {
        lock(&self.state).hold = false;
        self.arrived.notify_all();
    }
}

impl Drop for ProviderServer {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.state);
            state.stopping = true;
            state.hold = false;
        }
        self.arrived.notify_all();
        // A connection wakes the accept thread to see `stopping`. If none can
        // be made the thread is left blocked rather than joined forever.
        if TcpStream::connect(self.addr).is_ok()
            && let Some(accept) = self.accept.take()
        {
            // Builds abort on panic (docs/code-quality.md, "Panics"), so a
            // join never carries one.
            match accept.join() {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// A lock that outlives a panicked holder: the state is plain data, and a
/// test thread that panicked has already failed its test.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn accept_loop(listener: &TcpListener, state: &Arc<Mutex<State>>, arrived: &Arc<Condvar>) {
    for stream in listener.incoming() {
        if lock(state).stopping {
            return;
        }
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(state);
        let arrived = Arc::clone(arrived);
        // One thread per connection, so a client holding a connection open
        // never stalls another.
        let spawned = thread::Builder::new()
            .name("fake-provider-conn".to_owned())
            .spawn(move || {
                // A connection that breaks mid-request has already failed its
                // client, and there is no complete request to record.
                match serve(stream, &state, &arrived) {
                    Ok(()) | Err(_) => {}
                }
            });
        // Out of threads: the dropped connection fails its client.
        if spawned.is_err() {
            continue;
        }
    }
}

fn serve(stream: TcpStream, state: &Mutex<State>, arrived: &Condvar) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let (request, malformed) = read_request(&mut reader)?;
    let response = {
        let mut state = lock(state);
        state.requests.push(request);
        arrived.notify_all();
        // A malformed body is the client's bug: it gets a 400 and the script
        // keeps its next response.
        let response = match malformed {
            Some(why) => Response::status(
                400,
                format!(r#"{{"error":"fakes: malformed chunked body: {why}"}}"#),
            ),
            None => state.script.pop_front().unwrap_or_else(|| {
                Response::status(
                    500,
                    r#"{"error":"fakes: no scripted response left for this request"}"#,
                )
            }),
        };
        if state.hold {
            while state.hold && !state.stopping {
                state = arrived.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
        }
        response
    };
    write_response(reader.get_mut(), &response)
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// The request, and why its body's framing is malformed if it is.
fn read_request(reader: &mut impl BufRead) -> io::Result<(Request, Option<Malformed>)> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(invalid("no request line"));
    };
    let (method, path) = (method.to_owned(), fingerprint_query(target));

    let mut headers = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(invalid("headers end before a blank line"));
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| invalid("header without a colon"))?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        let value = if CREDENTIAL_HEADERS.contains(&name.as_str()) {
            fingerprint(value)
        } else {
            value.to_owned()
        };
        headers.push((name, value));
    }

    let chunked = headers
        .iter()
        // A request with any transfer coding ends in chunked (RFC 9112,
        // section 6.1).
        .any(|(n, _)| n == "transfer-encoding");
    let mut malformed = None;
    let body = if chunked {
        let mut body = Vec::new();
        malformed = read_chunked(reader, &mut body).err();
        body
    } else {
        let length = headers
            .iter()
            .find(|(n, _)| n == "content-length")
            .map_or(Ok(0), |(_, v)| v.parse::<usize>())
            .map_err(|_| invalid("content-length is not a number"))?;
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        body
    };
    Ok((
        Request {
            method,
            path,
            headers,
            body,
        },
        malformed,
    ))
}

/// Why a chunked request body could not be decoded.
type Malformed = &'static str;

const TRUNCATED: Malformed = "the chunked body ends before its framing does";

/// Decodes a chunked body (RFC 9112, section 7.1) into `body`: each chunk's
/// bytes in order, with chunk extensions and trailers dropped. On malformed
/// or truncated framing, `body` holds what decoded before it.
fn read_chunked(reader: &mut impl BufRead, body: &mut Vec<u8>) -> Result<(), Malformed> {
    loop {
        let line = framing_line(reader)?;
        let size = line.split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size, 16).map_err(|_| "a chunk size is not a hex number")?;
        if size == 0 {
            break;
        }
        // `take` rather than a buffer of `size`, so a huge declared size
        // cannot allocate before a byte arrives.
        let read = reader
            .by_ref()
            .take(size)
            .read_to_end(body)
            .map_err(|_| TRUNCATED)?;
        if u64::try_from(read) != Ok(size) {
            return Err(TRUNCATED);
        }
        if !framing_line(reader)?.is_empty() {
            return Err("a chunk's data is not followed by CRLF");
        }
    }
    // Trailers, up to the blank line that ends the body. They are read so
    // the client's bytes are all consumed before the connection closes.
    while !framing_line(reader)?.is_empty() {}
    Ok(())
}

/// One line of chunked framing, without its CRLF.
fn framing_line(reader: &mut impl BufRead) -> Result<String, Malformed> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => Err(TRUNCATED),
        Ok(_) if !line.ends_with('\n') => Err(TRUNCATED),
        Ok(_) => line
            .strip_suffix("\r\n")
            .map(str::to_owned)
            .ok_or("a chunked framing line does not end in CRLF"),
    }
}

/// The request target with any `key` query parameter's value replaced by
/// its fingerprint.
fn fingerprint_query(target: &str) -> String {
    let Some((path, query)) = target.split_once('?') else {
        return target.to_owned();
    };
    let query: Vec<String> = query
        .split('&')
        .map(|param| match param.split_once('=') {
            Some(("key", value)) => format!("key={}", fingerprint(value)),
            _ => param.to_owned(),
        })
        .collect();
    format!("{path}?{}", query.join("&"))
}

fn write_response(stream: &mut TcpStream, response: &Response) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {} Fake\r\n", response.status);
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        response.body.len()
    ));
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

#[cfg(test)]
#[path = "provider_server_tests.rs"]
mod tests;
