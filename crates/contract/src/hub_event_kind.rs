//! The hub connection's closed kind set (`docs/events.md`, "The envelope"):
//! the seven hub lines plus the two command replies. A reader that must
//! tolerate names it does not know keeps an unfamiliar name in
//! [`HubEventKind::Unknown`], with its exact text, never as a known member.

/// Every `kind` a hub connection sends: seven hub lines and the two
/// command replies (`docs/events.md`, "The envelope"). An unfamiliar
/// name reads as [`HubEventKind::Unknown`] carrying its exact text, so a
/// consumer that skips what it does not recognise keeps skipping it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubEventKind {
    /// The first line on every connection.
    HubHello,
    /// A top-level session needs the person.
    Attention,
    /// A device was paired or revoked.
    DeviceChanged,
    /// A session's process ended on the feed.
    SessionLeft,
    /// A relayed session closed its connection.
    StreamClosed,
    /// A resumed session subscribed again at `full`.
    StreamReopened,
    /// The hub accepted a command.
    CommandAccepted,
    /// The hub refused a command.
    CommandRejected,
    /// The keepers of gone sessions the hub's probe confirms.
    Keepers,
    /// A name this build does not know, with its exact text. A consumer
    /// that skips what it does not recognise skips this; a command's
    /// arguments refuse it as `invalid_arguments`.
    Unknown(String),
}

/// Every known hub line, once: its wire name and its member, so `name`
/// and `From<String>` cannot drift apart. `Unknown` is no row: any
/// other text keeps its exact text.
const KINDS: [(&str, HubEventKind); 9] = [
    ("hub_hello", HubEventKind::HubHello),
    ("attention", HubEventKind::Attention),
    ("device_changed", HubEventKind::DeviceChanged),
    ("session_left", HubEventKind::SessionLeft),
    ("stream_closed", HubEventKind::StreamClosed),
    ("stream_reopened", HubEventKind::StreamReopened),
    ("command_accepted", HubEventKind::CommandAccepted),
    ("command_rejected", HubEventKind::CommandRejected),
    ("keepers", HubEventKind::Keepers),
];

impl HubEventKind {
    /// Returns the original hub line name for this kind
    /// (`docs/events.md`, "The envelope").
    ///
    /// Known members name their wire text; [`HubEventKind::Unknown`]
    /// returns its exact text, never a placeholder.
    pub fn name(&self) -> &str {
        match self {
            HubEventKind::Unknown(name) => name,
            // The table holds every known member, so this finds one; a
            // row dropped by mistake reads a placeholder the round-trip
            // tests refuse.
            known @ HubEventKind::HubHello
            | known @ HubEventKind::Attention
            | known @ HubEventKind::DeviceChanged
            | known @ HubEventKind::SessionLeft
            | known @ HubEventKind::StreamClosed
            | known @ HubEventKind::StreamReopened
            | known @ HubEventKind::CommandAccepted
            | known @ HubEventKind::CommandRejected
            | known @ HubEventKind::Keepers => KINDS
                .iter()
                .find(|(_, kind)| kind == known)
                .map_or("unknown", |(name, _)| name),
        }
    }
}

impl From<String> for HubEventKind {
    /// The wire name into its member; any other text stays exact in
    /// [`HubEventKind::Unknown`].
    fn from(name: String) -> Self {
        KINDS
            .iter()
            .find(|(label, _)| *label == name)
            .map(|(_, kind)| kind.clone())
            .unwrap_or(HubEventKind::Unknown(name))
    }
}

#[cfg(test)]
#[path = "hub_event_kind_tests.rs"]
mod tests;
