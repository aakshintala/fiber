//! The key bindings, as data (`docs/tui.md`, "Bindings").

/// One row of the "Bindings" table, word for word with the code marks
/// dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Binding {
    /// The key map heading the row shows under.
    pub(crate) area: &'static str,
    /// The action's stable id.
    pub(crate) id: &'static str,
    /// What the action does.
    pub(crate) description: &'static str,
    /// Its keys.
    pub(crate) keys: &'static str,
    /// Its other paths: a slash command, a click; empty when none.
    pub(crate) other_paths: &'static str,
}

/// Every binding, in the table's order.
pub(crate) const BINDINGS: &[Binding] = &[
    Binding {
        area: "Sessions",
        id: "send",
        description: "Send a prompt, or a steering message during a turn",
        keys: "Enter",
        other_paths: "",
    },
    Binding {
        area: "Sessions",
        id: "line_break",
        description: "Insert a line break",
        keys: "Shift+Enter",
        other_paths: "Ctrl+J",
    },
    Binding {
        area: "Sessions",
        id: "close_or_interrupt",
        description: "Close what is on top; interrupt the turn when nothing is open",
        keys: "Esc",
        other_paths: "",
    },
    Binding {
        area: "Sessions",
        id: "clear_then_quit",
        description: "Clear the draft, then quit",
        keys: "Ctrl+C, twice within about a second on an empty box",
        other_paths: "/quit",
    },
    Binding {
        area: "Sessions",
        id: "go_home",
        description: "Go home",
        keys: "⌥0",
        other_paths: "/home",
    },
    Binding {
        area: "Sessions",
        id: "new_session",
        description: "Start a new session",
        keys: "Ctrl+N",
        other_paths: "/new",
    },
    Binding {
        area: "Sessions",
        id: "rail_row_n",
        description: "Switch to the session of rail card N",
        keys: "⌥1 to ⌥9",
        other_paths: "click the card",
    },
    Binding {
        area: "Sessions",
        id: "delete_session",
        description: "Delete the selected exited session in the session list",
        keys: "Delete, or Backspace, on the row",
        other_paths: "",
    },
    Binding {
        area: "The input box",
        id: "recall_prompt",
        description: "Recall an earlier prompt from the project of the session on screen",
        keys: "↑ in an empty box",
        other_paths: "",
    },
    Binding {
        area: "The input box",
        id: "search_prompts",
        description: "Search those prompts",
        keys: "Ctrl+R",
        other_paths: "",
    },
    Binding {
        area: "The input box",
        id: "move_word",
        description: "Move by word",
        keys: "⌥← ⌥→, Ctrl+← Ctrl+→",
        other_paths: "",
    },
    Binding {
        area: "The input box",
        id: "delete_word",
        description: "Delete a word",
        keys: "⌥Backspace",
        other_paths: "",
    },
    Binding {
        area: "The input box",
        id: "line_start_end",
        description: "Start or end of the line",
        keys: "⌘← ⌘→, where the terminal passes them",
        other_paths: "",
    },
    Binding {
        area: "The input box",
        id: "open_in_editor",
        description: "Open the draft, or a pasted token, in $VISUAL or $EDITOR",
        keys: "Ctrl+G",
        other_paths: "click the token",
    },
    Binding {
        area: "The input box",
        id: "paste_image",
        description: "Paste an image",
        keys: "Ctrl+V",
        other_paths: "",
    },
    Binding {
        area: "The conversation",
        id: "toggle_ledgers",
        description: "Open or close the ledgers",
        keys: "Ctrl+O",
        other_paths: "click a group's line",
    },
    Binding {
        area: "The conversation",
        id: "navigate",
        description: "Move focus from the input box into the conversation",
        keys: "Shift+Tab",
        other_paths: "click an item",
    },
    Binding {
        area: "The conversation",
        id: "focus_next_prev",
        description: "Move focus to the next or previous item",
        keys: "↓ ↑, j k",
        other_paths: "click an item",
    },
    Binding {
        area: "The conversation",
        id: "open_focused",
        description: "Open the focused item",
        keys: "Enter",
        other_paths: "click it",
    },
    Binding {
        area: "The conversation",
        id: "copy_focused",
        description: "Copy the focused item",
        keys: "y",
        other_paths: "select it",
    },
    Binding {
        area: "The conversation",
        id: "focus_area",
        description: "Move focus to the panel, the rail, then the conversation",
        keys: "Tab",
        other_paths: "click the area",
    },
    Binding {
        area: "The conversation",
        id: "toggle_panel",
        description: "Show or hide the panel",
        keys: "⌥P",
        other_paths: "",
    },
    Binding {
        area: "The conversation",
        id: "toggle_rail",
        description: "Show or hide the rail",
        keys: "⌥R",
        other_paths: "drag its edge",
    },
    Binding {
        area: "The conversation",
        id: "search",
        description: "Search",
        keys: "Ctrl+F; Cmd+F where forwarded",
        other_paths: "",
    },
    Binding {
        area: "The conversation",
        id: "search_results",
        description: "Open the search results",
        keys: "Ctrl+F with search open",
        other_paths: "click the match count",
    },
    Binding {
        area: "The conversation",
        id: "jump_to_end",
        description: "Jump to the end",
        keys: "End",
        other_paths: "click \"↓ New messages below\"",
    },
    Binding {
        area: "Steering",
        id: "select_steering",
        description: "Select a queued steering message",
        keys: "⌥↑ ⌥↓",
        other_paths: "its mouse target",
    },
    Binding {
        area: "Steering",
        id: "amend_steering",
        description: "Amend it",
        keys: "Enter",
        other_paths: "",
    },
    Binding {
        area: "Steering",
        id: "drop_steering",
        description: "Drop it",
        keys: "⌥X",
        other_paths: "its mouse target",
    },
    Binding {
        area: "Requests, models and help",
        id: "next_request",
        description: "Reopen a request put aside, or move to the next, the oldest first, switching to its session",
        keys: "⌥A",
        other_paths: "/approvals; click the badge or a waiting card",
    },
    Binding {
        area: "Requests, models and help",
        id: "model_picker",
        description: "Open the model picker",
        keys: "Ctrl+L",
        other_paths: "/model",
    },
    Binding {
        area: "Requests, models and help",
        id: "key_map",
        description: "Open the key map",
        keys: "F1",
        other_paths: "/? or /help",
    },
];

#[cfg(test)]
#[path = "bindings_tests.rs"]
mod tests;
