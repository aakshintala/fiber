//! The fake provider server (`docs/testing.md`, "Model calls"): a local
//! HTTP/1.1 server that answers each request with the next response of its
//! script and records every request it receives.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use crate::Error;

/// Header names whose values are credentials, lowercase. Their values are
/// masked before a request is recorded, as is a `key` query parameter, which
/// Google's API also accepts (`docs/model-routing.md`).
const CREDENTIAL_HEADERS: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-goog-api-key",
    "api-key",
    "cookie",
];

/// What a masked credential reads as in a recorded request.
const MASK: &str = "<masked>";

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

/// One request the server received, with credentials masked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The method, such as `POST`.
    pub method: String,
    /// The request target: the path and any query, with a `key` query
    /// parameter's value masked.
    pub path: String,
    /// Headers in the order received, names lowercased, credential values
    /// masked.
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
}

/// A fake provider listening on a local port. Each request gets the next
/// response of the script, and a request past its end gets a 500 saying so.
/// Dropping the server closes its port.
pub struct ProviderServer {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept: Option<JoinHandle<()>>,
}

impl ProviderServer {
    /// Listens on a free port on 127.0.0.1 and serves `script` in order. The
    /// port accepts connections when this returns.
    pub fn start(script: impl IntoIterator<Item = Response>) -> Result<Self, Error> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(Error::Bind)?;
        let addr = listener.local_addr().map_err(Error::Bind)?;
        let state = Arc::new(Mutex::new(State {
            script: script.into_iter().collect(),
            ..State::default()
        }));
        let shared = Arc::clone(&state);
        let accept = thread::Builder::new()
            .name("fake-provider".to_owned())
            .spawn(move || accept_loop(&listener, &shared))
            .map_err(Error::Spawn)?;
        Ok(Self {
            addr,
            state,
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
}

impl Drop for ProviderServer {
    fn drop(&mut self) {
        lock(&self.state).stopping = true;
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

fn accept_loop(listener: &TcpListener, state: &Arc<Mutex<State>>) {
    for stream in listener.incoming() {
        if lock(state).stopping {
            return;
        }
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(state);
        // One thread per connection, so a client holding a connection open
        // never stalls another.
        let spawned = thread::Builder::new()
            .name("fake-provider-conn".to_owned())
            .spawn(move || {
                // A connection that breaks mid-request has already failed its
                // client, and there is no complete request to record.
                match serve(stream, &state) {
                    Ok(()) | Err(_) => {}
                }
            });
        // Out of threads: the dropped connection fails its client.
        if spawned.is_err() {
            continue;
        }
    }
}

fn serve(stream: TcpStream, state: &Mutex<State>) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let request = read_request(&mut reader)?;
    let response = {
        let mut state = lock(state);
        state.requests.push(request);
        state.script.pop_front()
    }
    .unwrap_or_else(|| {
        Response::status(
            500,
            r#"{"error":"fakes: no scripted response left for this request"}"#,
        )
    });
    write_response(reader.get_mut(), &response)
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Request> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(invalid("no request line"));
    };
    let (method, path) = (method.to_owned(), mask_query(target));

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
        let value = if CREDENTIAL_HEADERS.contains(&name.as_str()) {
            MASK
        } else {
            value.trim()
        };
        headers.push((name, value.to_owned()));
    }

    let chunked = headers
        .iter()
        // A request with any transfer coding ends in chunked (RFC 9112,
        // section 6.1).
        .any(|(n, _)| n == "transfer-encoding");
    let body = if chunked {
        read_chunked(reader)?
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
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

/// A chunked body (RFC 9112, section 7.1), decoded: each chunk's bytes in
/// order, with chunk extensions and trailers dropped.
fn read_chunked(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let size = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| invalid("chunk size is not a hex number"))?;
        if size == 0 {
            break;
        }
        let mut chunk = vec![0; size];
        reader.read_exact(&mut chunk)?;
        body.extend(chunk);
        line.clear();
        reader.read_line(&mut line)?;
    }
    // Trailers, up to the blank line that ends the body. They are read so
    // the client's bytes are all consumed before the connection closes.
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            return Ok(body);
        }
    }
}

/// The request target with any `key` query parameter's value masked.
fn mask_query(target: &str) -> String {
    let Some((path, query)) = target.split_once('?') else {
        return target.to_owned();
    };
    let query: Vec<&str> = query
        .split('&')
        .map(|p| {
            if p.starts_with("key=") {
                "key=<masked>"
            } else {
                p
            }
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
