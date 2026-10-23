//! Failures of `web_fetch` (`docs/tools.md`, "web_fetch"): the URL checks,
//! the hops, the saved download and the response a page was refused for.

use std::io;
use std::path::PathBuf;

use contract::ErrorCode;
use ureq::http::uri::InvalidUri;

use super::ArgumentsError;

/// A URL the tool does not accept. It has no code of its own: the case that
/// carries it names one (`docs/errors.md`, "web_fetch").
#[derive(Debug, thiserror::Error)]
pub(crate) enum UrlError {
    /// The text is not a URL at all.
    #[error("`{url}` is not a URL to fetch: {source}.")]
    Malformed {
        /// The argument as the call named it.
        url: String,
        /// The parser's reason.
        #[source]
        source: InvalidUri,
    },
    /// The scheme is neither `http` nor `https`.
    #[error("`{url}` is not a URL to fetch: the scheme must be `http` or `https`.")]
    WrongScheme {
        /// The argument as the call named it.
        url: String,
    },
    /// The URL names no host.
    #[error("`{url}` is not a URL to fetch: it names no host.")]
    NoHost {
        /// The argument as the call named it.
        url: String,
    },
}

/// Every way `web_fetch` fails. Each case keeps the code `docs/errors.md`
/// gives its message.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WebFetchError {
    /// The call's arguments do not fit the schema.
    #[error(transparent)]
    Arguments(#[from] ArgumentsError),
    /// The URL is not one the tool fetches.
    #[error(transparent)]
    Url(#[from] UrlError),
    /// The redirects run past the limit.
    #[error("more than {max} redirects, the last from {last}.", max = crate::web_fetch::MAX_REDIRECTS)]
    TooManyRedirects {
        /// The URL the last redirect came from, as its `Uri` displays.
        last: String,
    },
    /// A redirect's `location` is not a URL the tool fetches.
    #[error("{from} redirected to `{location}`: {source}")]
    BadRedirect {
        /// The URL that redirected, as its `Uri` displays.
        from: String,
        /// The `location` header as the server sent it.
        location: String,
        /// Why the location is refused.
        #[source]
        source: UrlError,
    },
    /// The fetch's watcher thread could not start.
    #[error("could not start the fetch's watcher: {source}.")]
    NoWatcher {
        /// The spawn failure.
        #[source]
        source: io::Error,
    },
    /// The host is a link-local address or a cloud metadata host.
    #[error(
        "`{host}` is a link-local address or a cloud metadata host, which `web_fetch` does not reach."
    )]
    Blocked {
        /// The host as the URL names it.
        host: String,
    },
    /// The host's name does not resolve.
    #[error("could not resolve {host}: {source}.")]
    Unresolved {
        /// The host as the URL names it.
        host: String,
        /// The resolver's failure.
        #[source]
        source: io::Error,
    },
    /// The host's name resolves to no address.
    #[error("{host} resolved to no address.")]
    NoAddress {
        /// The host as the URL names it.
        host: String,
    },
    /// The request failed before a response. The text is made in `http.rs`,
    /// by `Hop::get`.
    // debt: Hop::get returns its failure as text, until http.rs's get returns a typed error
    #[error("could not fetch {uri}: {message}.")]
    NotFetched {
        /// The URL as its `Uri` displays.
        uri: String,
        /// What `Hop::get` reports, as text.
        message: String,
    },
    /// The page is past the limit, and nothing was kept.
    #[error(
        "{uri} is larger than 10 MiB ({max} bytes); nothing was kept.",
        max = crate::web_fetch::MAX_BODY
    )]
    TooLarge {
        /// The URL as its `Uri` displays.
        uri: String,
    },
    /// The download could not be saved to `path`.
    #[error("could not save the download to {}: {source}.", path.display())]
    Unsaved {
        /// The file the download was written to.
        path: PathBuf,
        /// The filesystem failure.
        #[source]
        source: io::Error,
    },
    /// A page of a content type `web_fetch` reads no way.
    #[error("{uri} is `{content_type}`, which `web_fetch` cannot read ({length} bytes).")]
    UnsupportedType {
        /// The URL as its `Uri` displays.
        uri: String,
        /// The response's content type.
        content_type: String,
        /// The body's length in bytes.
        length: u64,
    },
    /// A page with no content type, which `web_fetch` reads no way.
    #[error("{uri} is of no content type, which `web_fetch` cannot read ({length} bytes).")]
    NoContentType {
        /// The URL as its `Uri` displays.
        uri: String,
        /// The body's length in bytes.
        length: u64,
    },
    /// The response is not 2xx, so its body's start is the result.
    #[error("HTTP {status} from {uri}. The body begins:\n{body}")]
    Refused {
        /// The response's status.
        status: u16,
        /// The URL as its `Uri` displays.
        uri: String,
        /// The start of the body, cut at a character boundary within 2048
        /// bytes.
        body: String,
    },
    /// One request took over its limit.
    #[error("{uri}: the request took over 60 seconds.")]
    RequestTimeout {
        /// The URL as its `Uri` displays.
        uri: String,
    },
    /// The whole fetch took over its limit.
    #[error("{uri}: the fetch took over 5 minutes.")]
    FetchTimeout {
        /// The URL as its `Uri` displays.
        uri: String,
    },
}

impl WebFetchError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::Arguments(error) => error.code(),
            Self::Url(_) => ErrorCode::InvalidArguments,
            Self::TooManyRedirects { .. } | Self::BadRedirect { .. } | Self::Refused { .. } => {
                ErrorCode::HttpError
            }
            Self::NoWatcher { .. } | Self::Unsaved { .. } => ErrorCode::ToolError,
            Self::Blocked { .. } => ErrorCode::BlockedHost,
            Self::Unresolved { .. } | Self::NoAddress { .. } | Self::NotFetched { .. } => {
                ErrorCode::ConnectionFailed
            }
            Self::TooLarge { .. } => ErrorCode::TooLarge,
            Self::UnsupportedType { .. } | Self::NoContentType { .. } => ErrorCode::UnsupportedFile,
            Self::RequestTimeout { .. } | Self::FetchTimeout { .. } => ErrorCode::Timeout,
        }
    }
}
