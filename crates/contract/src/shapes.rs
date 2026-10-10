//! The shapes several payloads share (`docs/events.md`, "Payload types").
//! An optional key is absent when it does not apply, never `null`.

use std::collections::BTreeMap;

use serde::de::{Error as _, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{CommandId, ErrorCode, Seq, SessionId};

/// `error`: why something failed (`docs/errors.md`, "The shape").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    /// A stable label a consumer switches on.
    pub code: ErrorCode,
    /// Fiber's own sentence, saying what to do when there is a fix.
    pub message: String,
    /// On a failed model call, the wait the provider asked for, in
    /// milliseconds, rounded up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// On a failed model call, what the provider itself said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderFailure>,
}

/// The provider's side of a failed model call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFailure {
    /// The provider's name.
    pub name: String,
    /// The HTTP status; absent when an extension provider's `credential()`
    /// or `sign()` failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The provider's own message.
    pub message: String,
}

/// `process`: how a process ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    /// The exit code, when the process exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The signal's name, such as `SIGKILL`, when a signal ended the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// Whether Fiber stopped it at its timeout.
    pub timed_out: bool,
}

/// One content part, keyed by `type`. The set is open: a part this build does
/// not know reads as [`ContentPart::Unknown`], which a consumer shows as a
/// placeholder and never writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Text.
    Text {
        /// The text.
        text: String,
    },
    /// An image file in the session's `artifacts/`. The log never holds an
    /// image's bytes.
    Image {
        /// The file's path, relative to the session directory.
        path: String,
        /// Its type, such as `image/png`.
        mime_type: String,
        /// Its width in pixels.
        width: u32,
        /// Its height in pixels.
        height: u32,
    },
    /// A PDF file in the session's `artifacts/`, with its pages rendered
    /// as image parts (`docs/tools.md`, "read"). The log never holds the
    /// PDF's bytes.
    Pdf(PdfPart),
    /// A part this build does not know.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// An image part's data without its tag: image-only data serialised as an
/// `image` part, so it is written as `{"type":"image",…}` and reading
/// anything but an `image` part is an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "ContentPart", try_from = "ContentPart")]
pub struct ImagePart {
    /// The file's path, relative to the session directory.
    pub path: String,
    /// Its type, such as `image/png`.
    pub mime_type: String,
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
}

impl From<ImagePart> for ContentPart {
    fn from(part: ImagePart) -> Self {
        ContentPart::Image {
            path: part.path,
            mime_type: part.mime_type,
            width: part.width,
            height: part.height,
        }
    }
}

impl TryFrom<ContentPart> for ImagePart {
    type Error = ImagePartError;

    fn try_from(part: ContentPart) -> Result<Self, Self::Error> {
        match part {
            ContentPart::Image {
                path,
                mime_type,
                width,
                height,
            } => Ok(ImagePart {
                path,
                mime_type,
                width,
                height,
            }),
            ContentPart::Text { .. } => Err(ImagePartError::Text),
            ContentPart::Pdf(_) => Err(ImagePartError::Pdf),
            ContentPart::Unknown => Err(ImagePartError::Unknown),
        }
    }
}

/// Why a content part is not an image part.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImagePartError {
    /// The part holds text.
    #[error("expected an image part, found text")]
    Text,
    /// The part holds a PDF.
    #[error("expected an image part, found pdf")]
    Pdf,
    /// The part is one this build does not know.
    #[error("expected an image part, found unknown")]
    Unknown,
}

impl From<ImagePart> for crate::provider::ImageRef {
    fn from(part: ImagePart) -> Self {
        crate::provider::ImageRef {
            path: part.path,
            mime_type: part.mime_type,
            width: part.width,
            height: part.height,
        }
    }
}

/// A PDF part: the PDF in the session's `artifacts/`, the number of pages
/// sent, and the pages rendered as image parts, absent when they could not
/// be rendered (`docs/tools.md`, "read"). Two invariants hold:
/// `page_count >= 1`, and when `pages` is present, `pages.len() ==
/// page_count`. Both are checked in [`PdfPart::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PdfPart {
    path: String,
    page_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pages: Option<Vec<ImagePart>>,
}

impl PdfPart {
    /// Builds a PDF part, refusing `page_count == 0` and `pages` whose
    /// length is not `page_count`.
    pub fn new(
        path: String,
        page_count: u32,
        pages: Option<Vec<ImagePart>>,
    ) -> Result<Self, PdfPartError> {
        if page_count == 0 {
            return Err(PdfPartError::NoPages);
        }
        if let Some(pages) = &pages
            && pages.len() != usize::try_from(page_count).unwrap_or(usize::MAX)
        {
            return Err(PdfPartError::CountMismatch {
                page_count,
                pages: pages.len(),
            });
        }
        Ok(PdfPart {
            path,
            page_count,
            pages,
        })
    }

    /// The file's path, relative to the session directory when written.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The number of pages sent.
    pub fn page_count(&self) -> u32 {
        self.page_count
    }

    /// The pages rendered as image parts, absent when they could not be
    /// rendered.
    pub fn pages(&self) -> Option<&[ImagePart]> {
        self.pages.as_deref()
    }
}

/// The serialised form a log line reads through [`PdfPart::new`], so a
/// line that breaks an invariant fails to read.
#[derive(Debug, Deserialize)]
struct RawPdfPart {
    path: String,
    page_count: u32,
    #[serde(default)]
    pages: Option<Vec<ContentPart>>,
}

impl<'de> Deserialize<'de> for PdfPart {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawPdfPart::deserialize(deserializer)?;
        let pages = raw
            .pages
            .map(|parts| {
                parts
                    .into_iter()
                    .map(ImagePart::try_from)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(D::Error::custom)
            })
            .transpose()?;
        PdfPart::new(raw.path, raw.page_count, pages).map_err(D::Error::custom)
    }
}

/// Why a PDF part could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PdfPartError {
    /// The page count is 0.
    #[error("a PDF part holds at least one page")]
    NoPages,
    /// The rendered pages do not match the page count.
    #[error("a PDF part of {page_count} pages holds {pages} rendered pages")]
    CountMismatch {
        /// The page count the part claims.
        page_count: u32,
        /// The rendered pages it holds.
        pages: usize,
    },
}

/// One effect a tool call declares (`docs/permissions.md`, "Effects").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// It reads.
    Reads,
    /// It writes.
    Writes,
    /// It executes.
    Executes,
    /// It uses the network.
    Network,
}

/// Declared effects, on `tool_call_started` and `permission_requested`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredEffects {
    /// Each effect that applies; empty when the call declared none.
    pub effects: Vec<Effect>,
    /// Whether the call is reversible.
    pub reversible: bool,
    /// The paths the call touches, where the tool declared them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
}

/// `tokens`: a model call's token counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    /// Input tokens neither read from nor written to the cache.
    pub input: u64,
    /// Input tokens read from the cache.
    pub cache_read: u64,
    /// Input tokens written to the cache, keyed by cache lifetime (`"5m"`,
    /// `"1h"`); empty when nothing was written.
    pub cache_write: BTreeMap<String, u64>,
    /// Output tokens, reasoning included.
    pub output: u64,
}

/// `usage`: totals over some set of model calls, folded from their
/// `usage_recorded` lines. They are output, never a source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// The calls' tokens, summed.
    pub tokens: Tokens,
    /// US dollars billed per token, summed over the calls without
    /// `subscription` whose cost is known; `0` when there were none; `null`
    /// when there were some and none had a known cost.
    #[serde(deserialize_with = "nullable")]
    pub cost: Option<f64>,
    /// US dollars at API prices for the calls with `subscription`; `0` when
    /// there were none.
    pub subscription_cost: f64,
}

impl Default for Usage {
    /// No model call is known, so no tokens and no cost: zero counts and
    /// a known `0` cost, never `null`, which would mean an unknown one.
    fn default() -> Self {
        Self {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        }
    }
}

/// One `ask_user` question as the model called it (`docs/tools.md`, "The
/// call"). Its keys are the tool's argument names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// The question's short header.
    pub header: String,
    /// The question.
    pub question: String,
    /// The options offered; a free-text question has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<Choice>,
    /// Whether several options may be chosen.
    #[serde(
        rename = "multiSelect",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub multi_select: Option<bool>,
}

/// An option a question or an interaction offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// The option's label, which an answer names.
    pub label: String,
    /// What the option means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Where a message came from: `source`, and the key that goes with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Origin {
    /// A client's command.
    Driver,
    /// An extension's `host.drive`.
    Extension {
        /// The extension's name.
        extension: String,
    },
    /// Another session's `session_message`.
    Session {
        /// The sending session.
        from_session_id: SessionId,
    },
    /// Fiber's own message, such as the ending notice.
    Fiber,
}

/// The keys of "Where a message came from".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    /// Who sent it.
    #[serde(flatten)]
    pub origin: Origin,
    /// The `prompt`, `steer` or `message` command that sent it; `None`
    /// only for Fiber's own message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<CommandId>,
}

/// A point in a session's log that a fork or a rewind continues from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    /// The session.
    pub session_id: SessionId,
    /// The position in its log.
    pub seq: Seq,
}

/// A git worktree Fiber created for a delegate or a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// Its path.
    pub path: String,
    /// Its branch.
    pub branch: String,
}

/// A marker key whose only value is `true`, such as `declined` or `skipped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct True;

impl Serialize for True {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for True {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Ok(True)
        } else {
            Err(D::Error::invalid_value(Unexpected::Bool(false), &"true"))
        }
    }
}

/// Reads a key that is required but may be `null`. serde reads a missing
/// `Option` key as `None` unless a field names its own deserializer, so this
/// makes a missing key an error while `null` stays `None`.
pub(crate) fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[cfg(test)]
#[path = "shapes_tests.rs"]
mod tests;
