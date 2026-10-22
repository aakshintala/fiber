//! Failures of the small tools: `ask_user` and `web_search` here;
//! `skill` joins them below.
use contract::ErrorCode;
use contract::shapes::Failure;

/// Every way `ask_user` fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AskUserError {
    /// The call's `questions` do not parse.
    #[error("`questions` cannot be read: {source}")]
    QuestionsUnreadable {
        /// The parse failure.
        #[source]
        source: serde_json::Error,
    },
    /// The driver's answer fits no form the call raised.
    #[error("The answer does not fit the form.")]
    AnswerMismatch,
}

impl AskUserError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::QuestionsUnreadable { .. } => ErrorCode::InvalidArguments,
            Self::AnswerMismatch => ErrorCode::ToolError,
        }
    }
}

/// Every way `skill` fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SkillError {
    /// The listing holds no such name.
    #[error(
        "No skill named `{name}` is in the skills listing, or it is switched off. \
         Load a name from the listing."
    )]
    NotListed {
        /// The skill's name, as the call named it.
        name: String,
    },
    /// The skill's file does not resolve. The text is made in `skill.rs`
    /// from `std::fs::canonicalize`'s error.
    // debt: the text is made in skill.rs, until `skill.rs`'s `Resolve::Unreadable`
    // keeps the `io::Error`
    #[error("Could not read skill {}: {error}.", file.display())]
    Unreadable {
        /// The file discovery opened.
        file: std::path::PathBuf,
        /// The resolution failure.
        error: String,
    },
    /// The resolved target is not valid UTF-8.
    #[error("Skill `{name}` cannot be loaded: its path is not valid UTF-8.")]
    NotUtf8 {
        /// The skill's name, as the call named it.
        name: String,
    },
    /// The skill's file moved between the permission check and the read.
    #[error(
        "`{name}`'s SKILL.md changed between the permission check and the read. \
         Nothing was read."
    )]
    PathChanged {
        /// The skill's name, as the call named it.
        name: String,
    },
    /// The skill's body could not be read. The text is made in
    /// `contract::skills`.
    // debt: the text is made in contract::skills, until `contract::skills::SkillRead`
    // carries a typed error
    #[error("{message}")]
    Io {
        /// The read failure.
        message: String,
    },
    /// The skill's header does not parse, names another skill, or turns
    /// off model invocation.
    #[error(
        "Skill `{name}` at {listing} cannot be loaded: its header no longer \
         parses, names another skill, or turns off model invocation."
    )]
    Invalid {
        /// The skill's name, as the call named it.
        name: String,
        /// The file the listing holds under `name`.
        listing: String,
    },
    /// The call's arguments do not fit the schema.
    #[error(transparent)]
    Arguments(#[from] super::ArgumentsError),
}

impl SkillError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::NotListed { .. } | Self::NotUtf8 { .. } | Self::Invalid { .. } => {
                ErrorCode::InvalidArguments
            }
            Self::Unreadable { .. } | Self::Io { .. } => ErrorCode::ToolError,
            Self::PathChanged { .. } => ErrorCode::PathChanged,
            Self::Arguments(error) => error.code(),
        }
    }
}

/// Every way `web_search` fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WebSearchError {
    /// The provider runs a hosted search; Fiber has none to run.
    #[error("The provider runs a hosted search; Fiber has none to run.")]
    Hosted,
    /// The query is empty.
    #[error("`query` must be a non-empty string.")]
    EmptyQuery,
    /// Both domain filters are set.
    #[error("Pass either `allowed_domains` or `blocked_domains`, never both.")]
    BothDomainLists,
    /// The installed backend failed: its code and message reach
    /// `Output.error` unchanged, with no retry or provider carried over.
    /// The code is the backend's.
    #[error("{}", .0.message)]
    Backend(Failure),
    /// The call's arguments do not fit the schema.
    #[error(transparent)]
    Arguments(#[from] super::ArgumentsError),
}

impl WebSearchError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::Hosted => ErrorCode::ToolError,
            Self::EmptyQuery | Self::BothDomainLists => ErrorCode::InvalidArguments,
            Self::Backend(failure) => failure.code.clone(),
            Self::Arguments(error) => error.code(),
        }
    }
}
