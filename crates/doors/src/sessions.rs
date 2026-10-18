//! Running-session discovery over summary subscriptions (`docs/tools.md`,
//! "Messaging other sessions"). One poll loop owns every connection and
//! closes it on an answer, cancellation or the shared deadline.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use contract::SessionId;
use contract::clock::{Clock, Wake};
use contract::sessions::{ListError, Running, Sessions};
use contract::tool::Cancel;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use serde_json::Value;

pub(crate) const LIST_DEADLINE: Duration = Duration::from_secs(2);
pub(crate) const CLEANUP_MARGIN: Duration = Duration::from_millis(50);
pub(crate) const MAX_LINE: usize = 1024 * 1024;
pub(crate) const READ_CHUNK: usize = 65_536;

/// The account's session sockets, queried afresh on each call.
pub struct SessionSockets {
    run: PathBuf,
    clock: Arc<dyn Clock>,
}

impl SessionSockets {
    /// Uses Fiber home's run directory and the supplied deadline clock.
    pub fn new(home: &Path, clock: Arc<dyn Clock>) -> Self {
        Self {
            run: config::run_dir(home),
            clock,
        }
    }

    fn list_until(&self, cutoff: Instant, cancel: &dyn Cancel) -> Result<Vec<Running>, ListError> {
        self.list_until_with_read(cutoff, cancel, &mut |stream, chunk| stream.read(chunk))
    }

    // Keep readiness separate from reads: a ready socket may still WouldBlock.
    fn list_until_with_read(
        &self,
        cutoff: Instant,
        cancel: &dyn Cancel,
        read: &mut impl FnMut(&mut UnixStream, &mut [u8]) -> io::Result<usize>,
    ) -> Result<Vec<Running>, ListError> {
        if cancel.is_cancelled() {
            return Err(ListError::Cancelled);
        }
        let mut names = Vec::new();
        match fs::read_dir(&self.run) {
            Ok(entries) => {
                for entry in entries {
                    if cancel.is_cancelled() {
                        return Err(ListError::Cancelled);
                    }
                    if self.clock.now() >= cutoff {
                        break;
                    }
                    let entry = entry.map_err(|e| io_error(&self.run, e))?;
                    if let Some(name) = entry.file_name().to_str().filter(|name| valid_id(name)) {
                        names.push(name.to_owned());
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(io_error(&self.run, e)),
        }
        names.sort();
        let (mut wake_read, wake_write) = UnixStream::pair().map_err(|e| io_error(&self.run, e))?;
        wake_read
            .set_nonblocking(true)
            .map_err(|e| io_error(&self.run, e))?;
        wake_write
            .set_nonblocking(true)
            .map_err(|e| io_error(&self.run, e))?;
        let waker: Arc<dyn Wake> = Arc::new(Waker(wake_write));
        self.clock.subscribe(Arc::downgrade(&waker));
        cancel.subscribe(Arc::downgrade(&waker));
        let mut connections = Vec::new();
        for name in names {
            if cancel.is_cancelled() {
                return Err(ListError::Cancelled);
            }
            if self.clock.now() >= cutoff {
                break;
            }
            let path = self.run.join(&name);
            let fd = crate::hub::nonblocking_socket().map_err(|e| io_error(&path, e))?;
            let Ok(address) = rustix::net::SocketAddrUnix::new(&path) else {
                continue;
            };
            if rustix::net::connect(&fd, &address).is_err() {
                continue;
            }
            let mut stream = UnixStream::from(fd);
            let id = SessionId(name);
            let command = crate::mint("c_");
            let sent = crate::attach::send(
                &mut stream,
                &id,
                &command,
                "subscribe",
                serde_json::json!({"level":"summary"}),
            );
            connections.push(Connection {
                running: Running { id, status: None },
                stream: sent.is_ok().then_some(stream),
                command,
                stage: Stage::Ack,
                buffer: Vec::new(),
            });
        }
        'listing: loop {
            let mut wake_bytes = [0; 256];
            let _ = wake_read.read(&mut wake_bytes);
            if cancel.is_cancelled() {
                return Err(ListError::Cancelled);
            }
            if self.clock.now() >= cutoff {
                break;
            }
            let active: Vec<_> = connections
                .iter()
                .enumerate()
                .filter_map(|(i, c)| c.stream.as_ref().map(|s| (i, s)))
                .collect();
            if active.is_empty() {
                break;
            }
            let mut fds: Vec<_> = active
                .iter()
                .map(|(_, stream)| {
                    PollFd::new(
                        stream,
                        PollFlags::IN.union(PollFlags::HUP).union(PollFlags::ERR),
                    )
                })
                .collect();
            fds.push(PollFd::new(&wake_read, PollFlags::IN));
            let mut poll_error = None;
            self.clock.wait_until(Some(cutoff), &mut |bound| {
                let timeout = bound.map(Timespec::try_from).transpose();
                match timeout {
                    Ok(timeout) => {
                        if let Err(e) = poll(&mut fds, timeout.as_ref()) {
                            poll_error = Some(io::Error::from(e));
                        }
                    }
                    Err(e) => poll_error = Some(io::Error::other(e)),
                }
            });
            if let Some(e) = poll_error {
                return Err(io_error(&self.run, e));
            }
            let ready: Vec<_> = active
                .iter()
                .zip(fds.iter())
                .filter_map(|((i, _), fd)| (!fd.revents().is_empty()).then_some(*i))
                .collect();
            drop(fds);
            drop(active);
            for index in ready {
                if cancel.is_cancelled() {
                    return Err(ListError::Cancelled);
                }
                if self.clock.now() >= cutoff {
                    break 'listing;
                }
                let Some(connection) = connections.get_mut(index) else {
                    continue;
                };
                let Some(stream) = connection.stream.as_mut() else {
                    continue;
                };
                let mut chunk = [0; READ_CHUNK];
                match read(stream, &mut chunk) {
                    Ok(0) => {
                        connection.stream = None;
                        continue;
                    }
                    Ok(n) => connection
                        .buffer
                        .extend_from_slice(chunk.get(..n).unwrap_or_default()),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(_) => {
                        connection.stream = None;
                        continue;
                    }
                }
                while let Some(end) = connection.buffer.iter().position(|b| *b == b'\n') {
                    if cancel.is_cancelled() {
                        return Err(ListError::Cancelled);
                    }
                    if self.clock.now() >= cutoff {
                        break 'listing;
                    }
                    let line: Vec<_> = connection.buffer.drain(..=end).collect();
                    let Ok(line) = serde_json::from_slice::<Value>(&line) else {
                        continue;
                    };
                    match connection.stage {
                        Stage::Ack => {
                            if !crate::attach::is_answer(
                                &line,
                                "command_accepted",
                                &connection.command,
                            ) || crate::attach::check_version(&connection.running.id, &line)
                                .is_err()
                            {
                                connection.stream = None;
                                break;
                            }
                            connection.stage = Stage::Status;
                        }
                        Stage::Status => {
                            if crate::attach::kind(&line) == "session_status" {
                                connection.running.status =
                                    line.get("payload").and_then(|payload| {
                                        serde_json::from_value(payload.clone()).ok()
                                    });
                                connection.stream = None;
                                break;
                            }
                        }
                    }
                }
                if connection.buffer.len().saturating_sub(MAX_LINE) > 0 {
                    connection.stream = None;
                }
            }
        }
        Ok(connections.into_iter().map(|c| c.running).collect())
    }
}

impl Sessions for SessionSockets {
    fn scope(&self) -> PathBuf {
        self.run.join("")
    }

    fn list(&self, cancel: &dyn Cancel) -> Result<Vec<Running>, ListError> {
        let start = self.clock.now();
        self.list_until(start + LIST_DEADLINE.saturating_sub(CLEANUP_MARGIN), cancel)
    }
}

struct Waker(UnixStream);

impl Wake for Waker {
    fn wake(&self) {
        // A full non-blocking buffer already holds a pending wake.
        match (&self.0).write(&[1]) {
            Ok(_) | Err(_) => {}
        }
    }
}

enum Stage {
    Ack,
    Status,
}

struct Connection {
    running: Running,
    stream: Option<UnixStream>,
    command: String,
    stage: Stage,
    buffer: Vec<u8>,
}

fn valid_id(name: &str) -> bool {
    name.strip_prefix("s_").is_some_and(|hex| {
        hex.len() == 16
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn io_error(path: &Path, error: io::Error) -> ListError {
    ListError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
