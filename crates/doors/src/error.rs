use contract::ErrorCode;

/// A hub client's validation, configuration or connection failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
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
            Self::BadName { .. } | Self::BadAddress { .. } => ErrorCode::Usage,
        }
    }
}
