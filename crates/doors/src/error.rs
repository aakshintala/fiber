use contract::ErrorCode;

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;

/// A hub client's validation, configuration or connection failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A flag named a hub absent from the client's list.
    #[error(
        "No hub named `{name}` in this client's list; run `fiber hub add <name> <address>`. Run `fiber --help` for usage."
    )]
    UnknownHub {
        /// The name supplied by the client.
        name: String,
    },
    /// The configured default does not name an entry.
    #[error("`hub.default` names `{name}`, which is not in this client's list.")]
    MissingDefault {
        /// The configured default.
        name: String,
    },
    /// A configured address is outside the address grammar.
    #[error("`{key}`: {why}")]
    StoredAddress {
        /// The configuration key.
        key: String,
        /// The grammar failure without the stored address.
        why: String,
    },
    /// A websocket entry has no device token.
    #[error(
        "No device token for hub `{name}`; run `fiber hub remove {name}` then `fiber hub add {name} <address>` to pair again."
    )]
    MissingToken {
        /// The entry that needs pairing.
        name: String,
    },
    /// A configuration or credential read failed.
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    /// The websocket transport failed before admission.
    #[error("{hub}: {source}")]
    Unreachable {
        /// The listed hub's name.
        hub: String,
        /// The transport failure.
        source: net::Error,
    },
    /// A listed socket did not accept a connection.
    #[error("{hub}: could not connect to {}: {source}", path.display())]
    Unix {
        /// The listed hub's name.
        hub: String,
        /// The socket path.
        path: std::path::PathBuf,
        /// The socket failure.
        source: std::io::Error,
    },
    /// The hello or authentication exchange failed.
    #[error("{hub}: {source}")]
    Hello {
        /// The listed hub's name.
        hub: String,
        /// The protocol or timeout failure.
        source: std::io::Error,
    },
    /// The hub refused the client's device token.
    #[error("{hub}: {message}")]
    Refused {
        /// The listed hub's name.
        hub: String,
        /// The hub's refusal message, with secrets removed.
        message: String,
    },
    /// The existing local hub path failed.
    #[error(transparent)]
    Local(std::io::Error),
    /// A name that cannot safely name a hub's credential file.
    #[error("{}", crate::hub::NAME_RULE)]
    BadName {
        /// The refused name.
        name: String,
    },
    /// An address argument outside the supported grammar.
    #[error("{why}")]
    BadAddress {
        /// The address grammar, without the supplied address's secrets.
        why: String,
    },
}

impl Error {
    /// The stable error code for this failure.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::BadName { .. } | Self::BadAddress { .. } | Self::UnknownHub { .. } => {
                ErrorCode::Usage
            }
            Self::StoredAddress { .. } | Self::MissingDefault { .. } => ErrorCode::ConfigInvalid,
            Self::MissingToken { .. } => ErrorCode::CredentialMissing,
            Self::Config(error) => error.code(),
            Self::Unreachable { .. } | Self::Unix { .. } | Self::Hello { .. } => {
                ErrorCode::ConnectionFailed
            }
            Self::Refused { .. } => ErrorCode::AuthenticationFailed,
            Self::Local(_) => ErrorCode::IoFailed,
        }
    }
}
