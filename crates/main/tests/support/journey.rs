//! The helpers of the journeys in `journeys.rs` (`docs/testing/screens.md`,
//! "Screens"): the snapshot text of a settled grid with its live values
//! replaced, the `tui` config every journey writes, and the fake provider,
//! session and wait helpers the journeys share.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};

use super::pty::{Grid, Run};
use super::{Setup, write_json};

/// The spinner glyphs of the working line: braille frames, or `●` under
/// reduced motion.
const SPINNERS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏●";

/// The byte length of the `format::duration` shape at the start of
/// `text`: `<n>s`, `<n>m <nn>s` or `<n>h <nn>m`.
fn duration_len(text: &str) -> Option<usize> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &text[digits..];
    let two_then = |tail: &str, unit: u8| {
        let bytes = tail.strip_prefix(' ')?.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_digit()
            && bytes[1].is_ascii_digit()
            && bytes[2] == unit)
            .then_some(1 + 3)
    };
    match rest.as_bytes().first()? {
        b's' => Some(digits + 1),
        b'm' => Some(digits + 1 + two_then(&rest[1..], b's')?),
        b'h' => Some(digits + 1 + two_then(&rest[1..], b'm')?),
        _ => None,
    }
}

/// Each `i_<id>.png` image line name reads `i_[id].png`.
fn image_ids(row: &str) -> String {
    let mut out = String::new();
    let mut rest = row;
    while let Some(at) = rest.find("i_") {
        let after = &rest[at + 2..];
        let id_len = after.find([' ', '.']).unwrap_or(after.len());
        if id_len > 0 && after[id_len..].starts_with(".png") {
            out.push_str(&rest[..at]);
            out.push_str("i_[id].png");
            rest = &after[id_len + ".png".len()..];
        } else {
            out.push_str(&rest[..at + 2]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Each `dd:dd` with no digit or `:` on either side reads `HH:MM`.
fn clock_times(row: &str) -> String {
    let cells: Vec<char> = row.chars().collect();
    let live = |cell: Option<&char>| cell.is_some_and(|c| c.is_ascii_digit() || *c == ':');
    let mut out = String::new();
    let mut at = 0;
    while at < cells.len() {
        let shape = cells.get(at..at + 5).is_some_and(|w| {
            w[0].is_ascii_digit()
                && w[1].is_ascii_digit()
                && w[2] == ':'
                && w[3].is_ascii_digit()
                && w[4].is_ascii_digit()
        });
        let before = at.checked_sub(1).and_then(|p| cells.get(p));
        if shape && !live(before) && !live(cells.get(at + 5)) {
            out.push_str("HH:MM");
            at += 5;
        } else {
            out.push(cells[at]);
            at += 1;
        }
    }
    out
}

/// A closing row: `▣ ` then `completed`, `interrupted` or `failed`. Its
/// duration, absent under a second and present after, reads
/// ` · [duration]` after the outcome word either way.
fn closing_row(row: &str) -> String {
    let Some(at) = row.find("▣ ") else {
        return row.to_owned();
    };
    let from = at + "▣ ".len();
    let Some(word) = ["completed", "interrupted", "failed"]
        .into_iter()
        .find(|word| row[from..].starts_with(word))
    else {
        return row.to_owned();
    };
    let (head, after) = row.split_at(from + word.len());
    if !after.is_empty() && !after.starts_with(" · ") {
        return row.to_owned();
    }
    let (kept, pad) = after
        .strip_prefix(" · ")
        .and_then(|tail| {
            let n = duration_len(tail)?;
            (tail[n..].is_empty() || tail[n..].starts_with(" · ")).then_some((&tail[n..], n + 3))
        })
        .unwrap_or((after, 0));
    // The drawn row gave the padding after the duration n + 3 fewer
    // columns while the duration was present, so hand them back at the
    // first wide space run. A frame-adjacent run counts even as one
    // space, since the fill before `⋮` shrinks to one. A bare row holds
    // no wide run and no frame, and reads on unchanged.
    let mut kept = kept.to_owned();
    if let Some(from) = kept.find("  ") {
        kept.insert_str(from, &" ".repeat(pad));
    } else if let Some(body) = kept.strip_suffix("⋮") {
        let trimmed = body.trim_end_matches(' ');
        kept.insert_str(trimmed.len(), &" ".repeat(pad));
    }
    format!("{head} · [duration]{kept}")
}

/// The working line: `Working ` and `Esc to interrupt` on a row whose
/// cell two before `Working` is a spinner glyph. The glyph reads `*` and
/// the elapsed time `[elapsed]`.
fn working_line(row: &str) -> String {
    let Some(at) = row.find("Working ") else {
        return row.to_owned();
    };
    let before: Vec<char> = row[..at].chars().collect();
    let spinner = before
        .len()
        .checked_sub(2)
        .filter(|cell| row.contains("Esc to interrupt") && SPINNERS.contains(before[*cell]));
    let Some(cell) = spinner else {
        return row.to_owned();
    };
    let tail = &row[at + "Working ".len()..];
    let Some(n) = duration_len(tail) else {
        return row.to_owned();
    };
    let mut out: String = before[..cell].iter().collect();
    out.push('*');
    out.extend(&before[cell + 1..]);
    format!("{out}Working [elapsed]{}", &tail[n..])
}

/// A tool-group summary: a row ending in ` ▸` or ` ▾` whose last word
/// before it is a duration. A running group leads with a braille spinner
/// frame, which reads `*`; the `●` of a finished group stays.
fn tool_group(row: &str) -> String {
    let Some(body) = row.strip_suffix(" ▸").or_else(|| row.strip_suffix(" ▾")) else {
        return row.to_owned();
    };
    let suffix = &row[body.len()..];
    let indent = body.len() - body.trim_start().len();
    let spun = body[indent..].strip_prefix(|glyph| SPINNERS.contains(glyph) && glyph != '●');
    let owned;
    let body = match spun.filter(|rest| rest.starts_with(' ')) {
        Some(rest) => {
            owned = format!("{}*{rest}", &body[..indent]);
            owned.as_str()
        }
        None => body,
    };
    for (space, _) in body.match_indices(' ') {
        let tail = &body[space + 1..];
        if duration_len(tail) == Some(tail.len()) {
            // The drawn row gave the padding before the duration n - 2
            // fewer columns than the 2-char form, so hand them back.
            // n is at least 2, so a 2-char duration adds nothing.
            let pad = " ".repeat(tail.len() - 2);
            return format!("{}{pad} [duration]{suffix}", &body[..space]);
        }
    }
    row.to_owned()
}

/// The picker's `fake` provider heading, with no suffix, `⟳ refreshing`
/// and its spinner, or `· updated <age> ago`, reads `fake [freshness]`.
pub(crate) fn provider_heading(row: &str) -> String {
    let indent = row.len() - row.trim_start().len();
    let Some(rest) = row[indent..].strip_prefix("fake") else {
        return row.to_owned();
    };
    let dynamic = rest.is_empty()
        || rest
            .strip_prefix(" ⟳ refreshing ")
            .is_some_and(|spinner| !spinner.is_empty() && !spinner.contains(' '))
        || rest
            .strip_prefix(" · updated ")
            .is_some_and(|age| age.len() > " ago".len() && age.ends_with(" ago"));
    if dynamic {
        format!("{}fake [freshness]", &row[..indent])
    } else {
        row.to_owned()
    }
}

/// What changes every run, replaced before a snapshot. The settled grid's
/// text: each row right-trimmed, rows joined by `\n`, then
/// `\ncursor=(<row>,<col>)`, or `\ncursor=hidden` when the cursor is hidden:
/// a hidden cursor's parked coordinate is not user-visible and varies
/// between identical frames. The replacements run in order: `ids`, image
/// names, clock times, the closing row, the working line, a tool-group
/// summary, the provider heading.
pub(crate) fn grid_text(
    rows: &[String],
    cursor: (u16, u16),
    hide_cursor: bool,
    ids: &[String],
) -> String {
    let mut text = Vec::with_capacity(rows.len() + 1);
    for row in rows {
        let mut row = row.trim_end().to_owned();
        for id in ids.iter().filter(|id| !id.is_empty()) {
            row = row.replace(id.as_str(), "[session]");
        }
        let row = image_ids(&row);
        let row = clock_times(&row);
        let row = closing_row(&row);
        let row = working_line(&row);
        let row = tool_group(&row);
        text.push(provider_heading(&row));
    }
    text.push(if hide_cursor {
        "cursor=hidden".to_owned()
    } else {
        format!("cursor=({},{})", cursor.0, cursor.1)
    });
    text.join("\n")
}

/// The one `tui` config object every journey writes: no rail and no panel
/// cards, so no live figure or temp path reaches a snapshot.
pub(crate) fn tui_off() -> Value {
    json!({"rail": {"shown": false}, "panel": {"cards": []}})
}

/// A reply saying `text` in two deltas.
pub(crate) fn reply(text: &str) -> Response {
    let at = text.len() / 2;
    let (first, rest) = text.split_at(at);
    super::stream(&[
        json!({"type": "response.output_text.delta", "delta": first}),
        json!({"type": "response.output_text.delta", "delta": rest}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": text}]
        }}),
    ])
}

/// Whether any grid row holds `needle`.
pub(crate) fn shows(screen: &Grid, needle: &str) -> bool {
    screen.rows.iter().any(|row| row.contains(needle))
}

/// Whether any grid row's trimmed text equals `expected`: a whole-row
/// match, so a row with any additional text does not match. Trim is the
/// only normalization.
pub(crate) fn shows_row(screen: &Grid, expected: &str) -> bool {
    screen.rows.iter().any(|row| row.trim() == expected)
}

/// Clicks the `✕` on the grid's `cache_rebuild` notice row: an SGR press
/// then its release on the same cell. The notice fades only after ten
/// seconds of real time, so a journey dismisses it instead.
#[track_caller]
pub(crate) fn dismiss_cache_rebuild(run: &mut Run, screen: &Grid) {
    let y = screen
        .rows
        .iter()
        .position(|row| row.contains("cache_rebuild"))
        .unwrap_or_else(|| panic!("no cache_rebuild notice in {:?}", screen.rows));
    let y = u16::try_from(y).expect("the notice fits the screen");
    let (cols, _) = screen.size();
    let x = (0..cols)
        .find(|x| screen.cell(*x, y).symbol == "✕")
        .unwrap_or_else(|| panic!("no ✕ on the notice row in {:?}", screen.rows));
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// Installs a provider `fake` with models `alpha-1` (named
/// "Claude Opus 5.5") and `beta-2` (no name) on `openai-responses` at the
/// fake server, reading its key from `FIBER_TEST_FAKE_KEY`, and makes
/// `fake/alpha-1` the configured model. Neither the provider nor either id
/// contains "opus": only the display name can carry the picker's
/// "opus 5.5" query.
pub(crate) fn install_picker_provider(setup: &Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [
                {"id": "alpha-1", "protocol": "openai-responses",
                 "base_url": format!("{}/v1", server.url()),
                 "context_window": 100000, "name": "Claude Opus 5.5"},
                {"id": "beta-2", "protocol": "openai-responses",
                 "base_url": format!("{}/v1", server.url()),
                 "context_window": 100000},
            ],
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/alpha-1", "hub": {"idle_exit_ms": 200}, "tui": tui_off()}),
    );
}

/// Installs a provider `fake` with model `m` on `openai-responses` at the
/// fake server, `model_extra` merged into the model, and writes
/// `config.json` once: `fake/m` as the model, the hub idling out a second
/// after its last client, `tui_off()`, and each top-level key of
/// `extra_config` other than `tui`.
pub(crate) fn provider_full(
    setup: &Setup,
    model_extra: &Value,
    server: &ProviderServer,
    extra_config: &Value,
) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    let mut model = json!({"id": "m", "protocol": "openai-responses",
        "base_url": format!("{}/v1", server.url()), "context_window": 100000});
    for (key, value) in model_extra.as_object().unwrap() {
        model[key] = value.clone();
    }
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [model]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    let mut config = json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}, "tui": tui_off()});
    for (key, value) in extra_config.as_object().unwrap() {
        if key != "tui" {
            config[key] = value.clone();
        }
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// The one session in the project's sessions directory.
pub(crate) fn only_session(setup: &Setup) -> String {
    let mut ids: Vec<String> = fs::read_dir(log::sessions_dir(
        &setup.home(),
        &doors::project(&setup.workspace()),
    ))
    .unwrap()
    .flatten()
    .map(|entry| entry.file_name().to_string_lossy().into_owned())
    .collect();
    assert_eq!(ids.len(), 1, "one session");
    ids.pop().unwrap()
}

/// Whether the grid shows a working line with its elapsed count:
/// "Working" plus a digit right after its space. The working line draws
/// only once `turn_started` has opened the turn, so an Esc sent after it
/// is never dropped as not-busy.
pub(crate) fn working_elapsed(contents: &str) -> bool {
    contents.match_indices("Working ").any(|(at, _)| {
        contents[at + "Working ".len()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_digit())
    })
}

/// A reply that stalls mid-body: the turn stays running until cancelled.
pub(crate) fn stalled() -> Response {
    let prefix = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Working\"}\n\n";
    Response::stall(200, prefix, prefix.len() + 100000).header("content-type", "text/event-stream")
}
