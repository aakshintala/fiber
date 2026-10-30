use super::Address;
use crate::Error;
use contract::Secret;
use contract::{HubEventKind, HubLine, clock::Clock};
use net::websocket::Via;
use serde_json::{Map, Value};
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// The hub selected for one client command.
#[derive(Debug)]
pub enum Target {
    /// The local hub, started on demand.
    Local,
    /// A listed hub, never started by this client.
    Listed {
        /// Its name in the client's list.
        name: String,
        /// The configured endpoint.
        address: Address,
        /// A device token for websocket connections only.
        token: Option<Secret>,
    },
}

impl Target {
    /// Whether this hub is remote, including a forwarded socket.
    pub fn is_listed(&self) -> bool {
        matches!(self, Self::Listed { .. })
    }
}

/// Loads the workspace's configuration layers and resolves its selected hub.
pub fn load_target(home: &Path, workspace: &Path, named: Option<&str>) -> Result<Target, Error> {
    let project = config::ProjectKey::new(log::project_key(&worktree::project(workspace)))?;
    let config = config::Config::load(config::Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: Vec::new(),
    })?;
    target(&config, home, named)
}

/// Resolves the flag or configured default from this command's loaded layers.
pub fn target(config: &config::Config, home: &Path, named: Option<&str>) -> Result<Target, Error> {
    let default = config
        .get("hub.default", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    let Some(name) = named.or(default.as_deref()) else {
        return Ok(Target::Local);
    };
    let missing = || {
        if named.is_some() {
            Error::UnknownHub { name: name.into() }
        } else {
            Error::MissingDefault { name: name.into() }
        }
    };
    super::check_name(name).map_err(|_| missing())?;
    let key = format!("hubs.\"{name}\".address");
    let (value, _) = config.get(&key, None).ok_or_else(missing)?;
    let address = Address::parse(value.as_str().unwrap_or_default()).map_err(|error| {
        Error::StoredAddress {
            key,
            why: error.to_string(),
        }
    })?;
    let token = match &address {
        Address::Unix(_) => None,
        Address::Web(_) => Some(
            config::read_credential(home, "hubs", name)?
                .ok_or_else(|| Error::MissingToken { name: name.into() })?,
        ),
    };
    Ok(Target::Listed {
        name: name.into(),
        address,
        token,
    })
}

/// Connects to the selected hub; only a local target may call `start`.
pub fn connect_target(
    home: &Path,
    target: &Target,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
) -> Result<super::Hub, Error> {
    match target {
        Target::Local => super::connect(home, start, clock).map_err(Error::Local),
        Target::Listed {
            name,
            address,
            token,
        } => {
            let hub = connect_listed_with(name, address, &Via::from_env(), clock)?;
            if let Address::Web(_) = address {
                let token = token
                    .as_ref()
                    .ok_or_else(|| Error::MissingToken { name: name.clone() })?;
                authenticate(&hub.0, name, token, clock, net::LIMITS.idle())?;
            }
            Ok(hub)
        }
    }
}

/// Connects and reads the hello without presenting a token, for pairing.
pub fn connect_unpaired(address: &Address, clock: &dyn Clock) -> Result<super::Hub, Error> {
    connect_listed_with("the hub", address, &Via::from_env(), clock)
}

fn connect_listed_with(
    name: &str,
    address: &Address,
    via: &Via,
    clock: &dyn Clock,
) -> Result<super::Hub, Error> {
    let stream = match address {
        Address::Unix(path) => UnixStream::connect(path).map_err(|source| Error::Unix {
            hub: name.into(),
            path: path.clone(),
            source,
        })?,
        Address::Web(endpoint) => {
            net::websocket::connect_with(endpoint, via).map_err(|source| Error::Unreachable {
                hub: name.into(),
                source,
            })?
        }
    };
    let idle = via.limits.idle();
    let label = match address {
        Address::Unix(path) => path.clone(),
        Address::Web(_) => name.into(),
    };
    super::read_hello(
        stream,
        &label,
        clock.now().checked_add(idle),
        idle,
        clock,
        &mut || {},
    )
    .map_err(|poll| {
        let source = match poll {
            super::Poll::Race => io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the hub closed before hub_hello",
            ),
            // Listed always connects, so absence and post-accept
            // failures surface as their own error.
            super::Poll::Failed(source)
            | super::Poll::Accepted(source)
            | super::Poll::Absent(source) => source,
        };
        Error::Hello {
            hub: name.into(),
            source,
        }
    })
}

#[derive(Debug)]
struct AuthenticationAnswer {
    kind: HubEventKind,
    payload: Map<String, Value>,
}

impl From<HubLine> for AuthenticationAnswer {
    fn from(line: HubLine) -> Self {
        Self {
            kind: HubEventKind::from(line.kind),
            payload: line.payload,
        }
    }
}

pub(crate) fn authenticate(
    mut stream: &UnixStream,
    name: &str,
    token: &Secret,
    clock: &dyn Clock,
    idle: Duration,
) -> Result<(), Error> {
    let hello_error = |source| Error::Hello {
        hub: name.into(),
        source,
    };
    stream.set_write_timeout(Some(idle)).map_err(hello_error)?;
    let command = serde_json::to_vec(&contract::commands::hub::HubCommandLine {
        id: contract::CommandId("c_authenticate".into()),
        command: contract::commands::hub::HubCommand::Authenticate(
            contract::commands::hub::Authenticate {
                token: token.clone(),
            },
        ),
    })
    .map_err(|_| hello_error(io::Error::other("could not write authentication command")))?;
    support::framing::write_line(&mut stream, &command).map_err(hello_error)?;
    let deadline = clock.now().checked_add(idle);
    loop {
        let mut bytes = Vec::new();
        loop {
            let left = match deadline {
                Some(deadline) => Some(
                    deadline
                        .checked_duration_since(clock.now())
                        .filter(|left| !left.is_zero())
                        .ok_or_else(|| {
                            hello_error(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "authentication timed out",
                            ))
                        })?,
                ),
                None => None,
            };
            super::set_read_timeout(stream, left).map_err(hello_error)?;
            let mut byte = [0];
            match stream.read(&mut byte) {
                Ok(0) => {
                    return Err(hello_error(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the hub closed before authentication answered",
                    )));
                }
                Ok(_) => {
                    if byte[0] == b'\n' {
                        break;
                    } else {
                        bytes.push(byte[0]);
                    }
                }
                Err(error) => {
                    let error = if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) {
                        io::Error::new(io::ErrorKind::TimedOut, "authentication timed out")
                    } else {
                        error
                    };
                    return Err(hello_error(error));
                }
            }
        }
        let line: HubLine = serde_json::from_slice(&bytes).map_err(|_| {
            hello_error(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid authentication answer",
            ))
        })?;
        let answer = AuthenticationAnswer::from(line);
        if answer.payload.get("command_id").and_then(Value::as_str) != Some("c_authenticate") {
            continue;
        }
        match answer.kind {
            HubEventKind::CommandAccepted => {
                super::set_read_timeout(stream, None).map_err(hello_error)?;
                return Ok(());
            }
            HubEventKind::CommandRejected => {
                let message = answer
                    .payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("authentication refused");
                return Err(Error::Refused {
                    hub: name.into(),
                    message: message.replace(token.expose(), "[redacted]"),
                });
            }
            HubEventKind::HubHello
            | HubEventKind::Attention
            | HubEventKind::DeviceChanged
            | HubEventKind::SessionLeft
            | HubEventKind::StreamClosed
            | HubEventKind::StreamReopened
            | HubEventKind::Unknown(_) => continue,
        }
    }
}

#[cfg(test)]
#[path = "listed_tests.rs"]
mod tests;
