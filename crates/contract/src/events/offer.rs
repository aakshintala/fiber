//! Payloads of `docs/events.md`, "Repository code".

use serde::{Deserialize, Serialize};

use crate::RequestId;

/// What kind of code a repository offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferedKind {
    /// An extension.
    Extension,
    /// A hook declared in configuration.
    Hook,
    /// An MCP server.
    McpServer,
}

/// One item of an offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferedItem {
    /// What it is.
    pub kind: OfferedKind,
    /// Its name.
    pub name: String,
    /// The content hash an approval records.
    pub hash: String,
    /// Whether the repository marks it `required`.
    pub required: bool,
    /// What an install shows.
    pub summary: String,
    /// The version, for an extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The diff against the copy approved before, when the content changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

/// `repository_code_offered`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryCodeOffered {
    /// The id a `reply` names.
    pub request_id: RequestId,
    /// One per item offered.
    pub items: Vec<OfferedItem>,
}

/// A person's decision on one offered item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferDecision {
    /// Approve it.
    Approve,
    /// Skip it for this session.
    Skip,
    /// Never offer this content again.
    Never,
}

/// `repository_code_resolved`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryCodeResolved {
    /// The offer it answers.
    pub request_id: RequestId,
    /// One per item, in the offer's order.
    pub decisions: Vec<OfferDecision>,
}
