//! Tests for the slash aliases (#3013): each runs its row's command,
//! rides on that row, and gives way to an extension that takes its name.

use super::commands_testkit::{answer_rows, attached, now, sent, shown, type_text};
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::slash;
use contract::events::CommandInfo;
use serde_json::json;

/// Every row that carries aliases, as `(name, aliases)`, in table order.
const ALIASES: &[(&str, &[&str])] = &[
    ("thinking", &["effort"]),
    ("usage", &["cost"]),
    ("pwd", &["cwd"]),
    ("rules", &["permissions"]),
    ("settings", &["config"]),
    ("keys", &["keybindings", "keymap"]),
    ("handoff", &["compact"]),
    ("name", &["rename"]),
    ("close", &["stop"]),
    ("feedback", &["bug"]),
    ("quit", &["exit"]),
];

/// Every `(name, alias)` pair, in table order.
fn pairs() -> Vec<(&'static str, &'static str)> {
    ALIASES
        .iter()
        .flat_map(|(name, aliases)| aliases.iter().map(|alias| (*name, *alias)))
        .collect()
}

/// Runs `command` through Enter on `app` with the draft set whole, so
/// dispatch reads the name as typed rather than a completion row.
fn run(app: &mut App, command: &str) -> Effect {
    app.draft.set(command);
    app.on_enter()
}

/// An extension's `commands` row named `name`, tagged `acme`.
fn extension(name: &str) -> CommandInfo {
    CommandInfo {
        name: name.to_owned(),
        description: format!("The {name} extension."),
        argument_hint: None,
        tag: "acme".to_owned(),
        extension: true,
    }
}

#[test]
fn each_alias_rides_on_its_commands_row_and_has_no_row_of_its_own() {
    let all = slash::rows(&[]);
    let carried: Vec<(&str, Vec<&str>)> = all
        .iter()
        .map(|row| {
            (
                row.name.as_str(),
                row.aliases.iter().map(String::as_str).collect(),
            )
        })
        .filter(|(_, aliases): &(&str, Vec<&str>)| !aliases.is_empty())
        .collect();
    let wanted: Vec<(&str, Vec<&str>)> = ALIASES
        .iter()
        .map(|(name, aliases)| (*name, aliases.to_vec()))
        .collect();
    // The `/` list draws built-ins in this order with each alias
    // after its row's name, never as a row (`docs/tui.md:1953,1957`).
    assert_eq!(carried, wanted);
    for (_, alias) in pairs() {
        // Aliases never draw as rows of their own (`docs/tui.md:1957`).
        assert!(all.iter().all(|row| row.name != alias), "/{alias}");
    }
    // Aliases draw after their name (`docs/tui.md:1957`).
    let name = all.iter().find(|row| row.name == "name").unwrap();
    assert_eq!(
        name.line(),
        "/name /rename <text>  Names the session.  command"
    );
    let keys = all.iter().find(|row| row.name == "keys").unwrap();
    // The table lists `/keybindings` before `/keymap` (`docs/tui.md:1937,1957`).
    assert_eq!(
        keys.line(),
        "/keys /keybindings /keymap  Opens the rebinding screen.  command"
    );
}

#[test]
fn typing_an_alias_finds_only_its_commands_row() {
    let all = slash::rows(&[]);
    for (name, alias) in pairs() {
        let found: Vec<&str> = slash::filter(&all, alias)
            .iter()
            .map(|row| row.name.as_str())
            .collect();
        // Typing an alias runs its row's command (`docs/tui.md:1957`).
        assert_eq!(found, [name], "/{alias}");
    }
}

#[test]
fn rename_names_the_session_as_name_does() {
    for command in ["/name fix the parser", "/rename fix the parser"] {
        let mut app = attached();
        let lines = sent(run(&mut app, command));
        assert_eq!(lines.len(), 1, "{command}");
        assert_eq!(lines[0]["command"], "name", "{command}");
        assert_eq!(
            lines[0]["args"],
            json!({"text": "fix the parser"}),
            "{command}"
        );
    }
}

#[test]
fn config_opens_the_configuration_keys_as_settings_does() {
    for command in ["/settings", "/config"] {
        let mut app = attached();
        assert_eq!(run(&mut app, command), Effect::None, "{command}");
        let frame = app.config_view_screen().expect("the view is open");
        assert_eq!(frame.title, "Settings", "{command}");
        assert_eq!(frame.command, "/settings", "{command}");
    }
}

#[test]
fn cost_opens_the_usage_view_as_usage_does() {
    for command in ["/usage", "/cost"] {
        let mut app = attached();
        assert_eq!(run(&mut app, command), Effect::None, "{command}");
        let frame = app.session_view_screen(80).expect("the view is open");
        assert_eq!(frame.title, "Usage", "{command}");
        assert_eq!(app.draft(), "", "{command}");
    }
}

#[test]
fn keybindings_opens_the_rebinding_screen_as_keys_does() {
    for command in ["/keys", "/keybindings"] {
        let mut app = attached();
        run(&mut app, command);
        assert!(app.keys_screen_open(), "{command}");
    }
}

#[test]
fn bug_writes_a_feedback_report_as_feedback_does() {
    for command in ["/feedback the panel flickers", "/bug the panel flickers"] {
        let mut app = attached();
        assert_eq!(
            run(&mut app, command),
            Effect::Feedback {
                description: "the panel flickers".to_owned()
            },
            "{command}"
        );
    }
}

#[test]
fn effort_takes_a_thinking_level_as_thinking_does() {
    for command in ["/thinking turbo", "/effort turbo"] {
        let mut app = attached();
        assert_eq!(run(&mut app, command), Effect::None, "{command}");
        assert_eq!(
            app.notice(),
            Some(
                "Unknown thinking level \"turbo\"; the levels are off, minimal, low, medium, high, xhigh, max."
            ),
            "{command}"
        );
    }
}

#[test]
fn enter_on_a_typed_alias_runs_its_commands_row() {
    let mut app = attached();
    type_text(&mut app, "/cost");
    assert_eq!(
        shown(&app),
        ["/usage /cost  Opens the usage view.  command"]
    );
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.title, "Usage");
}

#[test]
fn an_extension_named_like_an_alias_takes_that_name() {
    let rows = slash::rows(&[extension("config")]);
    let settings = rows.iter().find(|row| row.name == "settings").unwrap();
    // The extension takes this alias from the built-in row (`docs/tui.md:1966`).
    assert!(settings.aliases.is_empty());
    assert_eq!(
        settings.line(),
        "/settings  Opens the configuration keys.  command"
    );
    let configs: Vec<&slash::Row> = rows
        .iter()
        .filter(|row| row.name == "config" || row.aliases.iter().any(|alias| alias == "config"))
        .collect();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].tag, "acme");
    let mut app = attached();
    answer_rows(
        &mut app,
        json!([{"name": "config", "description": "The config extension.",
            "tag": "acme", "extension": true}]),
    );
    let lines = sent(run(&mut app, "/config"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["args"], json!({"name": "config"}));
    assert!(app.config_view_screen().is_none());
}

#[test]
fn a_skill_named_like_an_alias_is_left_out_of_the_list() {
    let skill = CommandInfo {
        extension: false,
        tag: "skill".to_owned(),
        ..extension("bug")
    };
    let rows = slash::rows(&[skill]);
    assert!(rows.iter().all(|row| row.tag != "skill"));
    let feedback = rows.iter().find(|row| row.name == "feedback").unwrap();
    assert_eq!(feedback.aliases, ["bug"]);
}

#[test]
fn compact_starts_a_handoff_with_instructions_as_handoff_does() {
    for command in ["/handoff take over", "/compact take over"] {
        let mut app = attached();
        let lines = sent(run(&mut app, command));
        // Typing the alias runs the same one command (`docs/tui.md:1957`).
        assert_eq!(lines.len(), 1, "{command}");
        assert_eq!(lines[0]["command"], "handoff", "{command}");
        assert_eq!(
            lines[0]["args"],
            json!({"instructions": "take over"}),
            "{command}"
        );
    }
}

#[test]
fn keymap_opens_the_rebinding_screen_as_keys_does() {
    // Typing an alias runs its row's command (`docs/tui.md:1957`).
    for command in ["/keys", "/keymap"] {
        let mut app = attached();
        run(&mut app, command);
        assert!(app.keys_screen_open(), "{command}");
    }
}

#[test]
fn permissions_opens_the_standing_rules_as_rules_does() {
    // Typing an alias runs its row's command (`docs/tui.md:1957`).
    for command in ["/rules", "/permissions"] {
        let mut app = attached();
        assert_eq!(run(&mut app, command), Effect::None, "{command}");
        let frame = app.config_view_screen().expect("the view is open");
        assert_eq!(frame.title, "Rules", "{command}");
        assert_eq!(frame.command, "/rules", "{command}");
    }
}

#[test]
fn stop_stops_the_session_as_close_does() {
    for command in ["/close", "/stop"] {
        let mut app = attached();
        let lines = sent(run(&mut app, command));
        // Typing the alias runs the same one command (`docs/tui.md:1957`).
        assert_eq!(lines.len(), 1, "{command}");
        assert_eq!(lines[0]["command"], "close", "{command}");
        assert_eq!(lines[0]["args"], json!({"now": true}), "{command}");
    }
}

#[test]
fn stop_all_asks_to_close_all_as_close_all_does() {
    let mut first = attached();
    let mut second = attached();
    assert_eq!(run(&mut first, "/close all"), Effect::None);
    assert_eq!(run(&mut second, "/stop all"), Effect::None);
    assert_eq!(first.close_all_question(), second.close_all_question());
    assert_eq!(first.notice(), second.notice());
    assert!(
        first.close_all_question().is_some() || first.notice().is_some(),
        "one of the close-all paths answers"
    );
}

#[test]
fn an_extension_named_like_one_keys_alias_keeps_the_other() {
    for (taken, kept) in [("keymap", "keybindings"), ("keybindings", "keymap")] {
        let rows = slash::rows(&[extension(taken)]);
        let keys = rows.iter().find(|row| row.name == "keys").unwrap();
        assert_eq!(keys.aliases, [kept], "{taken}");
        assert_eq!(
            keys.line(),
            format!("/keys /{kept}  Opens the rebinding screen.  command"),
            "{taken}"
        );
        let claimed: Vec<&slash::Row> = rows
            .iter()
            .filter(|row| row.name == taken || row.aliases.iter().any(|alias| alias == taken))
            .collect();
        // Only the extension's row shows the taken alias: the
        // built-in row drops it (`docs/tui.md:1966`).
        assert_eq!(claimed.len(), 1, "{taken}");
        assert_eq!(claimed[0].tag, "acme", "{taken}");
        let mut app = attached();
        answer_rows(
            &mut app,
            json!([{"name": taken, "description": format!("The {taken} extension."),
                "tag": "acme", "extension": true}]),
        );
        let lines = sent(run(&mut app, &format!("/{taken}")));
        // The taken name runs the extension's command (`docs/tui.md:1966`).
        assert_eq!(lines.len(), 1, "{taken}");
        assert_eq!(lines[0]["command"], "command", "{taken}");
        assert_eq!(lines[0]["args"], json!({"name": taken}), "{taken}");
        assert!(!app.keys_screen_open(), "{taken}");
    }
}
