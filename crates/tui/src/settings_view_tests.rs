//! Tests for `/settings`: the rows, the edit field and its layers, the
//! theme choices, and when a write applies (`docs/tui.md`, "Swapped
//! views", "Themes"; `docs/configuration.md`, "When Fiber reads
//! configuration").

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{Act, Applies, Ctx, Settings, applies, applies_text, as_typed, layers};
use crate::ThemeSetting;
use crate::configure::{Layer, SettingRow, Shown, WriteScope};
use crate::configure_fake::{Fake, file, row};
use crate::keys::{Edit, Key};
use crate::swapped::{Ink, Spot, render};

/// A value shown as compact JSON.
fn value(json: &str) -> Shown {
    Shown::Value(json.to_owned())
}

/// The rows the tests read: one per kind of value and write scope.
fn rows() -> Vec<SettingRow> {
    vec![
        row(
            "diagnostics.level",
            value("\"info\""),
            "default",
            WriteScope::GlobalOnly,
        ),
        row(
            "handoff.tokens",
            value("400000"),
            "default",
            WriteScope::Any { repo: true },
        ),
        row(
            "model",
            value("\"a/b\""),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "providers.openai.credentials.work",
            Shown::Redacted("command op".to_owned()),
            "global",
            WriteScope::Any { repo: false },
        ),
        row(
            "repository_extensions",
            Shown::Unset,
            "default",
            WriteScope::RepoOnly,
        ),
        row(
            "skills.disabled",
            Shown::Union {
                names: vec![
                    ("a".to_owned(), "global".to_owned()),
                    ("b".to_owned(), "project".to_owned()),
                ],
                own: vec![
                    (Layer::Global, "[\"a\"]".to_owned()),
                    (Layer::Project, "[\"b\"]".to_owned()),
                ],
            },
            "global + project",
            WriteScope::Any { repo: false },
        ),
        row(
            "tui.hover",
            value("true"),
            "default",
            WriteScope::Any { repo: false },
        ),
        row(
            "tui.theme",
            Shown::Unset,
            "default",
            WriteScope::Any { repo: false },
        ),
    ]
}

/// A call's context over `fake`, 24 rows tall, in `/w`.
fn ctx(fake: &Fake) -> Ctx<'_> {
    Ctx {
        seam: fake,
        workspace: Path::new("/w"),
        height: 24,
        width: 80,
        usage: None,
    }
}

/// The view over `fake` with the row for `key` selected.
fn at(fake: &Fake, key: &str) -> Settings {
    let mut settings = Settings::open(&ctx(fake));
    let index = settings
        .rows
        .iter()
        .position(|row| row.key == key)
        .unwrap_or_else(|| panic!("no row {key}"));
    settings.list.select(index + 1, settings.list_len(), 20);
    settings
}

/// Presses each key in turn.
fn press(settings: &mut Settings, fake: &Fake, keys: &[Key]) -> Vec<Act> {
    keys.iter()
        .map(|key| settings.key(key, &ctx(fake)))
        .collect()
}

/// Types `text` into the open field.
fn typed(settings: &mut Settings, fake: &Fake, text: &str) {
    let keys: Vec<Key> = text.chars().map(Key::Char).collect();
    press(settings, fake, &keys);
}

/// The field's text and layer, while it is open.
fn field(settings: &Settings) -> Option<(String, Layer)> {
    match &settings.mode {
        super::Mode::Field(field) => Some((field.draft.expand(), field.layer())),
        super::Mode::Rows | super::Mode::Choices { .. } => None,
    }
}

/// `settings` drawn at `width` x `height`, one string per row.
fn drawn(settings: &Settings, usage: Option<u64>, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let frame = settings.frame(usize::from(width), usage);
    render(&frame, area, &mut buf, &mut Vec::new());
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The line of `screen` that starts with `key`.
fn line_of<'a>(screen: &'a str, key: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.trim_start_matches(['›', ' ']).starts_with(key))
        .unwrap_or_else(|| panic!("no line for {key} in {screen}"))
}

/// The column where `needle` starts in `line`, counted in characters.
fn column(line: &str, needle: &str) -> usize {
    let byte = line
        .find(needle)
        .unwrap_or_else(|| panic!("no {needle} in {line}"));
    line[..byte].chars().count()
}

#[test]
fn rows_show_key_value_and_layer_as_aligned_columns() {
    let fake = Fake::new(rows());
    let settings = at(&fake, "handoff.tokens");
    let screen = drawn(&settings, None, 80, 24);
    // The widest key is 33 columns and the widest value 23, so the layer
    // column starts at 2 + 33 + 2 + 23 + 2.
    assert_eq!(
        line_of(&screen, "handoff.tokens"),
        format!("› {:<35}{:<25}default", "handoff.tokens", "400000")
    );
    assert_eq!(
        line_of(&screen, "model"),
        format!("  {:<35}{:<25}global", "model", "a/b")
    );
    assert_eq!(
        line_of(&screen, "skills.disabled"),
        format!(
            "  {:<35}{:<25}global + project",
            "skills.disabled", "a (global), b (project)"
        )
    );
    assert_eq!(
        line_of(&screen, "providers.openai.credentials.work"),
        format!(
            "  {:<35}{:<25}global",
            "providers.openai.credentials.work", "command op"
        )
    );
    assert_eq!(
        line_of(&screen, "tui.theme"),
        format!("  {:<35}{:<25}default", "tui.theme", "unset")
    );
    assert_eq!(fake.reads(), [PathBuf::from("/w")]);
}

#[test]
fn a_dim_header_row_heads_the_table() {
    let fake = Fake::new(rows());
    let settings = Settings::open(&ctx(&fake));
    let frame = settings.frame(80, None);
    let header: Vec<(&str, Ink)> = frame.rows[0]
        .iter()
        .map(|(text, _, ink)| (text.trim_end(), *ink))
        .collect();
    assert_eq!(
        header,
        [
            ("key", Ink::Muted),
            ("value", Ink::Muted),
            ("layer", Ink::Muted)
        ]
    );
    // Every line's layer starts where the header's does: 2 + 33 + 2 + 23
    // + 2 columns in.
    let screen = drawn(&settings, None, 80, 24);
    let layer = column(line_of(&screen, "key"), "layer");
    assert_eq!(layer, 62);
    for (key, word) in [
        ("handoff.tokens", "default"),
        ("model", "global"),
        ("tui.theme", "default"),
        ("skills.disabled", "global + project"),
        ("providers.openai.credentials.work", "global"),
    ] {
        let line = line_of(&screen, key);
        assert_eq!(column(line, word), layer, "{line}");
    }
}

#[test]
fn the_first_key_is_selected_and_the_header_takes_no_selection() {
    let fake = Fake::new(rows());
    let mut settings = Settings::open(&ctx(&fake));
    assert_eq!(settings.list.selected(), 1);
    assert_eq!(
        settings.selected().map(|row| row.key.as_str()),
        Some("diagnostics.level")
    );
    // Up stays on the first key.
    press(&mut settings, &fake, &[Key::Up]);
    assert_eq!(settings.list.selected(), 1);
    // A click on the header selects nothing, and nothing opens.
    press(&mut settings, &fake, &[Key::Down, Key::Down]);
    assert_eq!(settings.list.selected(), 3);
    assert!(matches!(
        settings.click(Spot::Row(0), &ctx(&fake)),
        Act::Stay
    ));
    assert_eq!(settings.list.selected(), 3);
    // Selecting the header by force: no key is selected, so Enter opens
    // no field and nothing is written.
    settings.list.select(0, settings.list_len(), 20);
    assert!(settings.selected().is_none());
    press(&mut settings, &fake, &[Key::Enter]);
    assert_eq!(field(&settings), None);
    assert!(fake.writes().is_empty());
    // PageUp over the keys lands on the first key, never the header.
    settings.list.select(3, settings.list_len(), 20);
    press(&mut settings, &fake, &[Key::PageUp]);
    assert_eq!(settings.list.selected(), 1);
}

#[test]
fn rows_that_arrive_after_a_failed_read_start_on_the_first_key() {
    let fake = Fake::new(Vec::new());
    if let Ok(mut known) = fake.rows.lock() {
        *known = Err(crate::ConfigureError {
            code: contract::ErrorCode::ConfigInvalid,
            message: "not JSON".to_owned(),
        });
    }
    let mut settings = Settings::open(&ctx(&fake));
    assert!(settings.rows.is_empty());
    if let Ok(mut known) = fake.rows.lock() {
        *known = Ok(rows());
    }
    settings.reread(&ctx(&fake));
    assert_eq!(settings.list.selected(), 1);
}

#[test]
fn a_move_clears_what_the_last_action_said_and_a_stay_keeps_it() {
    let fake = Fake::new(rows());
    let mut settings = Settings::open(&ctx(&fake));
    // On the first key, Up stays: the line stays too.
    settings.said = vec!["Saved.".to_owned()];
    press(&mut settings, &fake, &[Key::Up]);
    assert_eq!(settings.list.selected(), 1);
    assert_eq!(settings.frame(80, None).below, ["Saved."]);
    // Down moves: the line goes, and the selected key's timing shows.
    press(&mut settings, &fake, &[Key::Down]);
    assert_eq!(settings.list.selected(), 2);
    assert!(settings.said.is_empty());
    assert_eq!(
        settings.frame(80, None).below,
        ["changing handoff.tokens needs a reload"]
    );
    // And back up, the same.
    settings.said = vec!["Saved.".to_owned()];
    press(&mut settings, &fake, &[Key::Up]);
    assert_eq!(settings.list.selected(), 1);
    assert!(settings.said.is_empty());
}

#[test]
fn a_string_draws_bare_and_anything_else_as_its_json() {
    let fake = Fake::new(vec![
        row("a", value("\"a/b\""), "global", WriteScope::GlobalOnly),
        row("b", value("400000"), "global", WriteScope::GlobalOnly),
        row("c", value("[1,2]"), "global", WriteScope::GlobalOnly),
        row("d", value("true"), "global", WriteScope::GlobalOnly),
        row(
            "e",
            value("\"two\\nlines\""),
            "global",
            WriteScope::GlobalOnly,
        ),
    ]);
    let settings = Settings::open(&ctx(&fake));
    let frame = settings.frame(80, None);
    let values: Vec<&str> = frame.rows[1..]
        .iter()
        .map(|cells| cells[1].0.trim_end())
        .collect();
    assert_eq!(values, ["a/b", "400000", "[1,2]", "true", "two lines"]);
}

#[test]
fn unset_keys_say_unset() {
    let fake = Fake::new(rows());
    let settings = Settings::open(&ctx(&fake));
    let screen = drawn(&settings, None, 80, 24);
    assert!(line_of(&screen, "tui.theme").contains("unset"), "{screen}");
}

/// The view over one key `k` whose value is a string of `len` columns, and
/// its layer's column at `width` 80.
fn layer_column(len: usize) -> (usize, String) {
    let fake = Fake::new(vec![row(
        "tui.hover",
        value(&format!("\"{}\"", "v".repeat(len))),
        "global",
        WriteScope::GlobalOnly,
    )]);
    let settings = Settings::open(&ctx(&fake));
    let screen = drawn(&settings, None, 80, 24);
    let line = line_of(&screen, "tui.hover").to_owned();
    (column(&line, "global"), line)
}

#[test]
fn a_value_past_the_room_is_cut_and_the_layer_column_stays() {
    // The key column is 9 wide, the layer's room 10, two gaps of 2 and
    // the gutter 2: 80 - 25 = 55 columns are left for the value.
    let (below, _) = layer_column(54);
    let (at, line) = layer_column(55);
    let (past, cut) = layer_column(56);
    assert_eq!(at, 2 + 9 + 2 + 55 + 2);
    assert_eq!(below + 1, at);
    assert!(!line.contains('…'), "{line}");
    assert_eq!(past, at);
    assert!(cut.contains(&format!("{}…", "v".repeat(54))), "{cut}");
    let (far, _) = layer_column(200);
    assert_eq!(far, at);
}

#[test]
fn a_key_past_its_column_pushes_its_value() {
    let long = "k".repeat(40);
    let fake = Fake::new(vec![
        row(&long, value("1"), "global", WriteScope::GlobalOnly),
        row("model", value("2"), "global", WriteScope::GlobalOnly),
    ]);
    let settings = Settings::open(&ctx(&fake));
    let screen = drawn(&settings, None, 80, 24);
    // The column is 36 wide: the short key's value starts after it, the
    // long key's after its own width.
    assert_eq!(column(line_of(&screen, "model"), "2"), 2 + 36 + 2);
    assert_eq!(column(line_of(&screen, &long), "1"), 2 + 40 + 2);
}

#[test]
fn a_narrow_width_clips_without_panicking() {
    let fake = Fake::new(rows());
    let settings = Settings::open(&ctx(&fake));
    for width in [0u16, 1, 5, 20, 40, 79] {
        drawn(&settings, None, width, 6);
    }
}

#[test]
fn the_rows_end_in_the_file_hint_with_the_selected_keys_timing_above() {
    let fake = Fake::new(rows());
    let settings = at(&fake, "handoff.tokens");
    let frame = settings.frame(80, Some(180_000));
    assert_eq!(frame.footer, "Ctrl+G opens the file");
    assert_eq!(
        frame.below,
        ["changing handoff.tokens needs a reload (~180k tokens)"]
    );
    let frame = settings.frame(80, None);
    assert_eq!(frame.below, ["changing handoff.tokens needs a reload"]);
    let theme = at(&fake, "tui.theme").frame(80, None);
    assert_eq!(theme.below, ["Applies now."]);
    let mut said = at(&fake, "tui.theme");
    said.said = vec!["Saved.".to_owned()];
    assert_eq!(said.frame(80, None).below, ["Saved."]);
}

#[test]
fn the_field_and_the_choices_keep_their_own_hints() {
    let fake = themed();
    let mut field_view = at(&fake, "model");
    press(&mut field_view, &fake, &[Key::Enter]);
    assert_eq!(
        field_view.frame(80, None).footer,
        "Tab layer · Enter save · Esc cancel"
    );
    // The table stays above the field.
    assert_eq!(field_view.frame(80, None).rows.len(), rows().len() + 1);
    let mut choices_view = at(&fake, "tui.theme");
    press(&mut choices_view, &fake, &[Key::Enter]);
    let frame = choices_view.frame(80, None);
    assert_eq!(frame.footer, "↑↓ move · Enter choose · Esc back");
    assert_eq!(frame.rows.len(), 5);
    assert!(frame.list.is_gutter());
}

#[test]
fn a_failed_read_says_why_and_shows_no_rows() {
    let fake = Fake::new(Vec::new());
    if let Ok(mut rows) = fake.rows.lock() {
        *rows = Err(crate::ConfigureError {
            code: contract::ErrorCode::ConfigInvalid,
            message: "config.json: not JSON".to_owned(),
        });
    }
    let settings = Settings::open(&ctx(&fake));
    assert!(settings.rows.is_empty());
    assert!(drawn(&settings, None, 80, 24).contains("config.json: not JSON"));
    // With no key there is no header and nothing is marked.
    assert!(settings.frame(80, None).rows.is_empty());
    assert_ne!(settings.frame(80, None).list.selected(), 0);
}

#[test]
fn settings_80x24() {
    let fake = Fake::new(rows());
    let settings = at(&fake, "handoff.tokens");
    insta::assert_snapshot!("settings_80x24", drawn(&settings, Some(180_000), 80, 24));
}

#[test]
fn enter_opens_the_field_with_the_value_as_set_takes_it() {
    let fake = Fake::new(rows());
    for (key, text) in [
        ("model", "a/b"),
        ("handoff.tokens", "400000"),
        ("skills.disabled", "[\"a\"]"),
        ("repository_extensions", ""),
    ] {
        let mut settings = at(&fake, key);
        press(&mut settings, &fake, &[Key::Enter]);
        assert_eq!(
            field(&settings).map(|(text, _)| text),
            Some(text.to_owned()),
            "{key}"
        );
    }
}

#[test]
fn tab_skips_layers_the_key_may_not_be_written_to() {
    let fake = Fake::new(rows());
    for (key, cycle) in [
        (
            "handoff.tokens",
            vec![
                Layer::Global,
                Layer::Project,
                Layer::Repository,
                Layer::Global,
            ],
        ),
        (
            "tui.hover",
            vec![Layer::Global, Layer::Project, Layer::Global],
        ),
        ("diagnostics.level", vec![Layer::Global, Layer::Global]),
        (
            "repository_extensions",
            vec![Layer::Repository, Layer::Repository],
        ),
    ] {
        let mut settings = at(&fake, key);
        press(&mut settings, &fake, &[Key::Enter]);
        let mut seen = vec![field(&settings).map(|(_, layer)| layer)];
        for _ in 1..cycle.len() {
            press(&mut settings, &fake, &[Key::Tab]);
            seen.push(field(&settings).map(|(_, layer)| layer));
        }
        let cycle: Vec<Option<Layer>> = cycle.into_iter().map(Some).collect();
        assert_eq!(seen, cycle, "{key}");
    }
}

#[test]
fn layers_follow_each_write_scope() {
    assert_eq!(
        layers(WriteScope::Any { repo: true }),
        [Layer::Global, Layer::Project, Layer::Repository]
    );
    assert_eq!(
        layers(WriteScope::Any { repo: false }),
        [Layer::Global, Layer::Project]
    );
    assert_eq!(
        layers(WriteScope::PersonFiles),
        [Layer::Global, Layer::Project]
    );
    assert_eq!(layers(WriteScope::GlobalOnly), [Layer::Global]);
    assert_eq!(layers(WriteScope::RepoOnly), [Layer::Repository]);
}

#[test]
fn a_saved_write_rereads_and_says_the_file() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "handoff.tokens");
    press(&mut settings, &fake, &[Key::Enter, Key::Tab]);
    for _ in 0..6 {
        press(&mut settings, &fake, &[Key::Backspace]);
    }
    typed(&mut settings, &fake, "200000");
    press(&mut settings, &fake, &[Key::Enter]);
    assert_eq!(
        fake.writes(),
        [(
            PathBuf::from("/w"),
            Layer::Project,
            "handoff.tokens".to_owned(),
            "200000".to_owned()
        )]
    );
    assert_eq!(field(&settings), None);
    assert_eq!(fake.reads().len(), 2);
    assert_eq!(
        settings.said,
        [
            format!("Saved to {}.", file(Layer::Project).display()),
            "changing handoff.tokens needs a reload".to_owned(),
        ]
    );
}

#[test]
fn a_failed_write_keeps_the_text_and_layer() {
    let fake = Fake::new(rows());
    if let Ok(mut refuse) = fake.refuse.lock() {
        *refuse = Some("`model` is not a number".to_owned());
    }
    let mut settings = at(&fake, "model");
    press(&mut settings, &fake, &[Key::Enter, Key::Tab]);
    typed(&mut settings, &fake, "x");
    press(&mut settings, &fake, &[Key::Enter]);
    assert_eq!(field(&settings), Some(("a/bx".to_owned(), Layer::Project)));
    assert_eq!(settings.said, ["`model` is not a number"]);
    assert_eq!(fake.reads().len(), 1);
}

#[test]
fn esc_closes_the_field_and_writes_nothing() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "model");
    press(&mut settings, &fake, &[Key::Enter]);
    typed(&mut settings, &fake, "zz");
    let acts = press(&mut settings, &fake, &[Key::Esc]);
    assert!(matches!(acts.as_slice(), [Act::Stay]));
    assert_eq!(field(&settings), None);
    assert!(fake.writes().is_empty());
    // A second Esc closes the view.
    let acts = press(&mut settings, &fake, &[Key::Esc]);
    assert!(matches!(acts.as_slice(), [Act::Close]));
}

#[test]
fn a_redacted_row_opens_an_empty_field() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "providers.openai.credentials.work");
    press(&mut settings, &fake, &[Key::Enter]);
    assert_eq!(field(&settings), Some((String::new(), Layer::Global)));
    let screen = drawn(&settings, None, 80, 24);
    assert!(screen.contains("The stored value is hidden; what you type replaces it."));
    typed(&mut settings, &fake, r#"{"env": "K"}"#);
    press(&mut settings, &fake, &[Key::Enter]);
    assert_eq!(
        fake.writes()
            .into_iter()
            .map(|(_, _, _, text)| text)
            .collect::<Vec<_>>(),
        [r#"{"env": "K"}"#]
    );
}

#[test]
fn debug_of_a_field_holding_a_typed_secret_hides_it() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "providers.openai.credentials.work");
    press(&mut settings, &fake, &[Key::Enter]);
    typed(&mut settings, &fake, "s3cr3t-k3y-xyz");
    // A type that holds a secret prints it redacted in `Debug`
    // (`docs/code-quality.md`, "Errors").
    let shown = format!("{:?}", settings);
    assert!(!shown.contains("s3cr3t-k3y-xyz"), "{shown}");
}

#[test]
fn enter_on_an_empty_redacted_field_writes_nothing() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "providers.openai.credentials.work");
    press(&mut settings, &fake, &[Key::Enter, Key::Enter]);
    assert!(fake.writes().is_empty());
    assert_eq!(settings.said, ["Nothing was typed; nothing was written."]);
    assert!(field(&settings).is_some());
}

#[test]
fn an_empty_field_on_a_plain_row_is_written() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "repository_extensions");
    press(&mut settings, &fake, &[Key::Enter, Key::Enter]);
    assert_eq!(fake.writes().len(), 1);
}

#[test]
fn skills_disabled_edits_the_chosen_layers_own_list() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "skills.disabled");
    press(&mut settings, &fake, &[Key::Enter]);
    assert_eq!(
        field(&settings),
        Some(("[\"a\"]".to_owned(), Layer::Global))
    );
    press(&mut settings, &fake, &[Key::Tab]);
    assert_eq!(
        field(&settings),
        Some(("[\"b\"]".to_owned(), Layer::Project))
    );
    press(&mut settings, &fake, &[Key::Enter]);
    assert_eq!(
        fake.writes(),
        [(
            PathBuf::from("/w"),
            Layer::Project,
            "skills.disabled".to_owned(),
            "[\"b\"]".to_owned()
        )]
    );
}

#[test]
fn a_layer_with_no_list_of_its_own_starts_empty() {
    let mut rows = rows();
    if let Some(row) = rows.iter_mut().find(|row| row.key == "skills.disabled") {
        row.value = Shown::Union {
            names: vec![("a".to_owned(), "global".to_owned())],
            own: vec![(Layer::Global, "[\"a\"]".to_owned())],
        };
    }
    let fake = Fake::new(rows);
    let mut settings = at(&fake, "skills.disabled");
    press(&mut settings, &fake, &[Key::Enter, Key::Tab]);
    assert_eq!(field(&settings), Some(("[]".to_owned(), Layer::Project)));
}

#[test]
fn editing_keys_reach_the_field_and_paste_is_typed() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "model");
    press(&mut settings, &fake, &[Key::Enter]);
    settings.edit_key(Edit::Left);
    settings.edit_key(Edit::Paste("xy".to_owned()));
    assert_eq!(field(&settings), Some(("a/xyb".to_owned(), Layer::Global)));
    // Over the rows an edit does nothing.
    let mut rows_only = at(&fake, "model");
    rows_only.edit_key(Edit::Paste("z".to_owned()));
    assert_eq!(field(&rows_only), None);
}

#[test]
fn applies_by_key() {
    for (key, when) in [
        ("tui.theme", Applies::Now),
        ("tui.hover", Applies::TerminalStart),
        ("hub.port", Applies::HubStart),
        ("hubs.x.address", Applies::NextConnection),
        ("hub.default", Applies::NextConnection),
        ("hub.idle_exit_ms", Applies::HubStart),
        ("diagnostics.level", Applies::ProcessStart),
        ("model", Applies::NewSessions),
        ("thinking", Applies::NewSessions),
        ("models.x.thinking", Applies::NewSessions),
        ("models.openai/gpt.6.thinking", Applies::NewSessions),
        ("providers.openai.credential", Applies::NewSessions),
        ("providers.openai.credentials", Applies::NewSessions),
        ("providers.openai.credentials.work", Applies::NewSessions),
        ("providers.openai.reviewer_model", Applies::Reload),
        ("models.x.price", Applies::Reload),
        ("models.x.thinking.extra", Applies::Reload),
        ("providers.openai.credential.extra", Applies::Reload),
        ("providers.openai.credentials_extra", Applies::Reload),
        ("skills.disabled", Applies::NextTurn),
        ("handoff.tokens", Applies::Reload),
    ] {
        assert_eq!(applies(key), when, "{key}");
    }
}

#[test]
fn a_credential_write_applies_to_new_sessions() {
    assert_eq!(
        applies_text("providers.openai.credential", None),
        "Applies to new sessions."
    );
}

#[test]
fn client_hub_keys_apply_at_the_next_connection() {
    for key in ["hub.default", "hubs.box.address"] {
        assert_eq!(
            applies_text(key, None),
            "Applies at this client's next connection."
        );
    }
}

#[test]
fn the_reload_cost_uses_the_last_usage() {
    assert_eq!(
        applies_text("handoff.tokens", Some(180_000)),
        "changing handoff.tokens needs a reload (~180k tokens)"
    );
    assert_eq!(
        applies_text("handoff.tokens", Some(999)),
        "changing handoff.tokens needs a reload (~999 tokens)"
    );
}

#[test]
fn without_usage_it_says_a_change_needs_a_reload() {
    assert_eq!(
        applies_text("handoff.tokens", None),
        "changing handoff.tokens needs a reload"
    );
    assert_eq!(
        applies_text("budget.usd", None),
        "changing budget.usd needs a reload"
    );
}

#[test]
fn as_typed_strips_a_strings_quotes_only() {
    assert_eq!(as_typed("\"a/b\""), "a/b");
    assert_eq!(as_typed("400000"), "400000");
    assert_eq!(as_typed("[\"a\"]"), "[\"a\"]");
    assert_eq!(as_typed("not json"), "not json");
}

/// The theme choices' names while they are open.
fn choices(settings: &Settings) -> Vec<String> {
    match &settings.mode {
        super::Mode::Choices { names, .. } => names.clone(),
        super::Mode::Rows | super::Mode::Field(_) => Vec::new(),
    }
}

/// A seam whose `themes/` holds `dusk` and `solar`.
fn themed() -> Fake {
    let mut fake = Fake::new(rows());
    fake.themes = vec!["dusk".to_owned(), "solar".to_owned()];
    fake
}

#[test]
fn the_theme_row_offers_auto_dark_light_then_files() {
    let fake = themed();
    let mut settings = at(&fake, "tui.theme");
    press(&mut settings, &fake, &[Key::Enter]);
    assert_eq!(
        choices(&settings),
        ["auto", "dark", "light", "dusk", "solar"]
    );
    assert_eq!(field(&settings), None);
    // Esc goes back to the rows.
    press(&mut settings, &fake, &[Key::Esc]);
    assert!(choices(&settings).is_empty());
}

#[test]
fn the_current_theme_is_marked() {
    let mut fake = themed();
    fake.rows = std::sync::Mutex::new(Ok(vec![row(
        "tui.theme",
        value("\"solar\""),
        "global",
        WriteScope::Any { repo: false },
    )]));
    let mut settings = at(&fake, "tui.theme");
    press(&mut settings, &fake, &[Key::Enter]);
    let screen = drawn(&settings, None, 80, 24);
    assert!(screen.contains("solar  (current)"), "{screen}");
    assert!(!screen.contains("dusk  (current)"), "{screen}");
    let super::Mode::Choices { list, .. } = &settings.mode else {
        panic!("no choices");
    };
    assert_eq!(list.selected(), 4);
}

#[test]
fn choosing_writes_the_global_tui_theme_and_queues_the_setting() {
    let fake = themed();
    let mut settings = at(&fake, "tui.theme");
    let acts = press(
        &mut settings,
        &fake,
        &[Key::Enter, Key::Down, Key::Down, Key::Enter],
    );
    assert!(
        matches!(acts.last(), Some(Act::Theme(ThemeSetting::Light))),
        "{acts:?}"
    );
    assert_eq!(
        fake.writes(),
        [(
            PathBuf::from("/w"),
            Layer::Global,
            "tui.theme".to_owned(),
            "\"light\"".to_owned()
        )]
    );
    assert!(choices(&settings).is_empty());
    assert_eq!(
        settings.said.get(1).map(String::as_str),
        Some("Applies now.")
    );
}

#[test]
fn a_click_on_a_choice_chooses_it() {
    let fake = themed();
    let mut settings = at(&fake, "tui.theme");
    press(&mut settings, &fake, &[Key::Enter]);
    let act = settings.click(Spot::Row(4), &ctx(&fake));
    assert!(
        matches!(&act, Act::Theme(ThemeSetting::File { name, .. }) if name == "solar"),
        "{act:?}"
    );
}

#[test]
fn a_failed_write_queues_nothing() {
    let fake = themed();
    if let Ok(mut refuse) = fake.refuse.lock() {
        *refuse = Some("refused".to_owned());
    }
    let mut settings = at(&fake, "tui.theme");
    let acts = press(&mut settings, &fake, &[Key::Enter, Key::Enter]);
    assert!(
        matches!(acts.as_slice(), [Act::Stay, Act::Stay]),
        "{acts:?}"
    );
    assert_eq!(settings.said, ["refused"]);
}

#[test]
fn ctrl_g_opens_the_selected_rows_layer_file() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "model");
    let acts = press(&mut settings, &fake, &[Key::CtrlG]);
    assert!(
        matches!(acts.as_slice(), [Act::Open(path)] if *path == file(Layer::Global)),
        "{acts:?}"
    );
}

#[test]
fn ctrl_g_on_a_default_row_opens_the_global_file() {
    let mut rows = rows();
    rows.push(row(
        "zzz.project",
        value("1"),
        "project",
        WriteScope::Any { repo: true },
    ));
    let fake = Fake::new(rows);
    let mut settings = at(&fake, "handoff.tokens");
    let acts = press(&mut settings, &fake, &[Key::CtrlG]);
    assert!(
        matches!(acts.as_slice(), [Act::Open(path)] if *path == file(Layer::Global)),
        "{acts:?}"
    );
    let mut settings = at(&fake, "zzz.project");
    let acts = press(&mut settings, &fake, &[Key::CtrlG]);
    assert!(
        matches!(acts.as_slice(), [Act::Open(path)] if *path == file(Layer::Project)),
        "{acts:?}"
    );
}

#[test]
fn a_click_on_a_row_selects_it_and_the_x_closes() {
    let fake = Fake::new(rows());
    let mut settings = Settings::open(&ctx(&fake));
    assert!(matches!(
        settings.click(Spot::Row(2), &ctx(&fake)),
        Act::Stay
    ));
    assert_eq!(settings.list.selected(), 2);
    assert!(matches!(
        settings.click(Spot::Close, &ctx(&fake)),
        Act::Close
    ));
}

#[test]
fn settings_editing() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "handoff.tokens");
    press(&mut settings, &fake, &[Key::Enter, Key::Tab]);
    insta::assert_snapshot!("settings_editing", drawn(&settings, None, 80, 24));
}

#[test]
fn debug_of_a_field_names_its_parts_and_hides_the_draft() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "providers.openai.credentials.work");
    press(&mut settings, &fake, &[Key::Enter]);
    typed(&mut settings, &fake, "s3cr3t-k3y-xyz");
    let super::Mode::Field(field) = &settings.mode else {
        panic!("the field is open");
    };
    let shown = format!("{field:?}");
    assert!(shown.starts_with("Field {"), "{shown}");
    assert!(shown.contains("draft: \"redacted\""), "{shown}");
    assert!(shown.contains("redacted: true"), "{shown}");
    assert!(!shown.contains("s3cr3t"), "{shown}");
}

#[test]
fn shown_is_the_height_less_the_header_the_line_below_and_footer() {
    let fake = Fake::new(rows());
    let settings = Settings::open(&ctx(&fake));
    for (height, want) in [(0, 0), (2, 0), (3, 0), (4, 1), (5, 2)] {
        let sized = Ctx {
            height,
            ..ctx(&fake)
        };
        assert_eq!(settings.shown(&sized), want, "height {height}");
    }
}

#[test]
fn ctrl_g_uses_key_ownership_even_for_default_rows() {
    for (key, has_file, client) in [
        ("tui.hover", true, true),
        ("keys.quit", false, true),
        ("model", true, false),
        ("model", false, false),
        ("tui.theme", false, true),
    ] {
        let mut selected = row(key, Shown::Unset, "default", WriteScope::GlobalOnly);
        selected.file = has_file.then(|| PathBuf::from("/settings/config.json"));
        let fake = Fake::new(vec![selected]);
        let mut settings = Settings::open(&ctx(&fake));
        let act = settings.key(&Key::CtrlG, &ctx(&fake));
        match (act, client, has_file) {
            (Act::OpenLocal(Some(path)), true, true) => {
                assert_eq!(path, PathBuf::from("/settings/config.json"))
            }
            (Act::OpenLocal(None), true, false) => {}
            (Act::Open(path), false, true) => {
                assert_eq!(path, PathBuf::from("/settings/config.json"))
            }
            (Act::Open(path), false, false) => assert_eq!(path, fake.global),
            (act, _, _) => panic!("wrong file ownership for {key}: {act:?}"),
        }
    }
}
