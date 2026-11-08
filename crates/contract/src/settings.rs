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

impl SettingsTab {
    /// Every tab, in the order the view draws them.
    pub const ALL: [Self; 6] = [
        Self::Appearance,
        Self::Notifications,
        Self::Models,
        Self::Permissions,
        Self::Sessions,
        Self::Advanced,
    ];

    /// The lowercase tag the configure protocol carries.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Notifications => "notifications",
            Self::Models => "models",
            Self::Permissions => "permissions",
            Self::Sessions => "sessions",
            Self::Advanced => "advanced",
        }
    }

    /// The tab a protocol tag names.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.tag() == tag)
    }
}

/// How `/settings` edits one key's value. Derived from the type the schema
/// checks (`config`), so the view holds no type table of its own; it is
/// presentation, not a validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingControl {
    /// `true` or `false`.
    Boolean,
    /// A whole number or a number, typed into a field.
    Number,
    /// Free text, typed into a field.
    Text,
    /// One of a fixed set of strings, in the schema's order.
    Fixed(Vec<String>),
    /// A model reference, chosen in the model picker.
    Model,
    /// A theme's name: a built-in or a theme file's.
    Theme,
    /// A list of strings, edited on its own page.
    StringList,
    /// A list of `{path, required}` objects, edited on its own page.
    RepositoryExtensions,
    /// A structured value with no inline control: shown, edited with
    /// `fiber config set`.
    Json,
}

impl SettingControl {
    /// The lowercase tag the configure protocol carries.
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Number => "number",
            Self::Text => "text",
            Self::Fixed(_) => "fixed",
            Self::Model => "model",
            Self::Theme => "theme",
            Self::StringList => "string_list",
            Self::RepositoryExtensions => "repository_extensions",
            Self::Json => "json",
        }
    }
}

/// What `/settings` shows for one key, as owned data a protocol reply can
/// carry: `SettingsMetadata` is the static form the schema is written in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingPresentation {
    /// The tab the key sits on, if it has one.
    pub tab: Option<SettingsTab>,
    /// The one-line name.
    pub label: String,
    /// One line saying what the setting does.
    pub description: String,
}

impl From<SettingsMetadata> for SettingPresentation {
    fn from(metadata: SettingsMetadata) -> Self {
        Self {
            tab: metadata.tab,
            label: metadata.label.to_owned(),
            description: metadata.description.to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
