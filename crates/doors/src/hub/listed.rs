use super::Address;
use crate::Error;
use contract::Secret;
use std::path::Path;

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

/// Resolves the flag or configured default from this command's loaded layers.
pub fn target(config: &config::Config, home: &Path, named: Option<&str>) -> Result<Target, Error> {
    let default = config
        .get("hub.default", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    let Some(name) = named.or(default.as_deref()) else {
        return Ok(Target::Local);
    };
    super::check_name(name)?;
    let key = format!("hubs.\"{name}\".address");
    let Some((value, _)) = config.get(&key, None) else {
        return Err(if named.is_some() {
            Error::UnknownHub { name: name.into() }
        } else {
            Error::MissingDefault { name: name.into() }
        });
    };
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

#[cfg(test)]
#[path = "listed_tests.rs"]
mod tests;
