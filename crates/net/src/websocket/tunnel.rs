//! Resolves and dials a socket directly or through an HTTP CONNECT proxy.
use super::{Endpoint, Via, handshake};
use crate::Error;
use base64::{Engine, prelude::BASE64_STANDARD as STANDARD};
use std::io::{self, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;
use ureq::unversioned::transport::NextTimeout;

pub(super) fn io_error(error: io::Error, bound: Duration) -> Error {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        Error::Stalled { idle: bound }
    } else {
        Error::Io(error)
    }
}

pub(super) fn connect(endpoint: &Endpoint, via: &Via) -> Result<(TcpStream, Vec<u8>), Error> {
    let target = handshake::authority(&endpoint.host, endpoint.port);
    let uri = format!("http://{target}/")
        .parse::<ureq::http::Uri>()
        .map_err(|_| Error::Resolve {
            host: endpoint.host.clone(),
            reason: "invalid address".into(),
        })?;
    let proxy = via.proxy.as_ref().filter(|proxy| !proxy.is_no_proxy(&uri));
    let (host, port) = if let Some(proxy) = proxy {
        if proxy.protocol() != ureq::ProxyProtocol::Http {
            return Err(Error::ProxyScheme {
                scheme: proxy.protocol().to_string().to_ascii_lowercase(),
            });
        }
        (proxy.host(), proxy.port())
    } else {
        (endpoint.host.as_str(), endpoint.port)
    };
    let mut stream = dial(host, port, via, &mut TcpStream::connect_timeout)?;
    stream.set_read_timeout(Some(via.limits.connect()))?;
    stream.set_write_timeout(Some(via.limits.connect()))?;
    let mut tail = Vec::new();
    if let Some(proxy) = proxy {
        let auth = match proxy.username() {
            Some(user) => format!(
                "Proxy-Authorization: Basic {}\r\n",
                STANDARD.encode(format!("{user}:{}", proxy.password().unwrap_or_default()))
            ),
            None => String::new(),
        };
        let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth}\r\n");
        stream
            .write_all(request.as_bytes())
            .map_err(|e| io_error(e, via.limits.connect()))?;
        let (headers, buffered) = handshake::headers(&mut stream).map_err(|e| {
            if let Error::Io(e) = e {
                io_error(e, via.limits.connect())
            } else {
                e
            }
        })?;
        let status = headers.lines().next().unwrap_or_default();
        if status.split_whitespace().nth(1) != Some("200") {
            return Err(Error::ProxyRefused {
                status_line: status.into(),
            });
        }
        tail = buffered;
    }
    Ok((stream, tail))
}

fn dial(
    host: &str,
    port: u16,
    via: &Via,
    connect: &mut impl FnMut(&SocketAddr, Duration) -> io::Result<TcpStream>,
) -> Result<TcpStream, Error> {
    let uri = format!("http://{}/", handshake::authority(host, port))
        .parse::<ureq::http::Uri>()
        .map_err(|_| Error::Resolve {
            host: host.into(),
            reason: "invalid address".into(),
        })?;
    let addresses = via
        .resolver
        .resolve(
            &uri,
            &ureq::config::Config::default(),
            NextTimeout {
                after: via.limits.connect().into(),
                reason: ureq::Timeout::Resolve,
            },
        )
        .map_err(|e| Error::Resolve {
            host: host.into(),
            reason: e.to_string(),
        })?;
    let mut last = Error::Resolve {
        host: host.into(),
        reason: "no addresses".into(),
    };
    for addr in addresses.iter().copied() {
        match connect(&addr, via.limits.connect()) {
            Ok(stream) => return Ok(stream),
            Err(source) if source.kind() == io::ErrorKind::TimedOut => {
                last = Error::ConnectTimedOut {
                    addr,
                    limit: via.limits.connect(),
                }
            }
            Err(source) => last = Error::Connect { addr, source },
        }
    }
    Err(last)
}

#[cfg(test)]
#[path = "tunnel_tests.rs"]
mod tests;
