//! How a configuration key or a `/settings` screen row is presented
//! (`docs/tui.md`, "Swapped views", `/settings`). Data only: the schema in
//! `config` and the screen rows in `tui` each hold values of these types.

/// A tab of the `/settings` view, in the order the view draws them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    /// Theme, layout, motion and other looks of the terminal.
    Appearance,
    /// How the terminal and Fiber get the person's attention.
    Notifications,
    /// Which model a session uses, and how model calls are made.
    Models,
    /// The reviewer and the sandbox.
    Permissions,
    /// How a session runs, hands off and spends.
    Sessions,
    /// What a person rarely changes.
    Advanced,
}

/// What the `/settings` view shows for one key or screen row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsMetadata {
    /// The tab the entry sits on. `None` for a key with a `*` in its path
    /// and for a key a screen row covers, which the view draws once, as
    /// that screen row.
    pub tab: Option<SettingsTab>,
    /// The one-line name the view shows.
    pub label: &'static str,
    /// One line saying what the setting does.
    pub description: &'static str,
}
