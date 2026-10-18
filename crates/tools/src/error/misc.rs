//! Failures of the small tools: `ask_user` and `web_search` here;
/// `skill` joins them in task 3.
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
