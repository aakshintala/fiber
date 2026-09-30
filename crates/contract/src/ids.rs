//! The identifiers the vocabulary names (`docs/events.md`, "Identity and
//! ordering"). Each kind of id is its own type, so one cannot be passed where
//! another belongs.

use serde::{Deserialize, Serialize};

/// The session an event belongs to. Opaque, and unique within Fiber home.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

/// The turn an event is about. Opaque, and unique within its session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TurnId(pub String);

/// The action an event is about. Opaque, and unique within its session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActionId(pub String);

/// A durable event's position in its session log: contiguous, never reset or
/// reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub u64);

/// A driver command, minted by the client that sends it
/// (`docs/invocation.md`, "The command line").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandId(pub String);

/// An approval or interaction waiting on an answer; a `reply` names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub String);

/// A background job, a delegate's included.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub String);

/// The provider's own id for one generation, on `usage_recorded`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GenerationId(pub String);

/// The provider's own id for a tool call. Recorded to rebuild provider
/// history, never used for correlation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderCallId(pub String);
