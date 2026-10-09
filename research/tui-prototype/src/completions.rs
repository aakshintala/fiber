//! The `/` and `@` completion panels (#1631): one list above the input box.
//! Fixture data only: the stream has no command list, so the entries, the
//! file paths and the tags below are made up.

use crate::input::{Key, Mods};
use crate::{bold, dim, fg, fit, left_cut, row, slab, sp, Row, Ui, BLUE, ORANGE};
use ratatui::style::Style;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

pub struct Entry {
    pub name: &'static str,
    pub desc: &'static str,
    pub hint: Option<&'static str>,
    /// command, skill, template, the extension's name, or the MCP server's name
    pub tag: &'static str,
}

/// Forty entries covering every tag kind.
pub fn entries() -> Vec<Entry> {
    vec![
        Entry { name: "context", desc: "show the context breakdown", hint: None, tag: "command" },
        Entry { name: "model", desc: "pick the model for this session", hint: None, tag: "command" },
        Entry { name: "help", desc: "list every binding by area", hint: None, tag: "command" },
        Entry { name: "new", desc: "start a new session", hint: None, tag: "command" },
        Entry { name: "home", desc: "go back to the home screen", hint: None, tag: "command" },
        Entry { name: "quit", desc: "clear the draft, then quit", hint: None, tag: "command" },
        Entry { name: "approvals", desc: "reopen a request put aside", hint: None, tag: "command" },
        Entry { name: "panel", desc: "show or hide the side panel", hint: None, tag: "command" },
        Entry { name: "search", desc: "search the conversation", hint: None, tag: "command" },
        Entry {
            name: "login",
            desc: "sign in to a provider and refresh every cached credential, then retry the queued requests",
            hint: None,
            tag: "command",
        },
        Entry { name: "review", desc: "review this change for correctness", hint: Some("<path>"), tag: "skill" },
        Entry { name: "test-plan", desc: "draft a test plan for the ticket", hint: Some("<ticket>"), tag: "skill" },
        Entry { name: "refactor", desc: "restructure without changing behaviour", hint: None, tag: "skill" },
        Entry { name: "explain", desc: "explain this code in plain words", hint: Some("<symbol>"), tag: "skill" },
        Entry { name: "commit", desc: "write a commit message for the diff", hint: None, tag: "skill" },
        Entry { name: "changelog", desc: "draft the release notes", hint: None, tag: "skill" },
        Entry { name: "onboard", desc: "tour the repo for a newcomer", hint: None, tag: "skill" },
        Entry { name: "debug", desc: "find the root cause of a failure", hint: Some("<test>"), tag: "skill" },
        Entry { name: "perf", desc: "profile the slow path and report", hint: None, tag: "skill" },
        Entry { name: "docs", desc: "write the missing documentation", hint: None, tag: "skill" },
        Entry { name: "plan", desc: "a plan the owner can approve", hint: None, tag: "template" },
        Entry { name: "bug", desc: "a bug report with repro steps", hint: None, tag: "template" },
        Entry { name: "feature", desc: "a feature spec with acceptance", hint: None, tag: "template" },
        Entry { name: "rfc", desc: "a design proposal for review", hint: None, tag: "template" },
        Entry { name: "retro", desc: "a retrospective with lessons", hint: None, tag: "template" },
        Entry { name: "spike", desc: "a timeboxed investigation", hint: None, tag: "template" },
        Entry { name: "adr", desc: "an architecture decision record", hint: None, tag: "template" },
        Entry { name: "release", desc: "release notes and rollout steps", hint: None, tag: "template" },
        Entry { name: "summarize", desc: "summarize the thread for handoff", hint: None, tag: "review" },
        Entry { name: "inline-comments", desc: "post review comments inline", hint: Some("<path>"), tag: "review" },
        Entry { name: "approve-pr", desc: "approve the pull request", hint: None, tag: "review" },
        Entry { name: "request-changes", desc: "ask for changes on the diff", hint: None, tag: "review" },
        Entry { name: "re-review", desc: "re-check after the fixes land", hint: None, tag: "review" },
        Entry { name: "dismiss", desc: "dismiss a stale review", hint: None, tag: "review" },
        Entry { name: "ticket", desc: "open a Linear issue from this thread", hint: None, tag: "linear" },
        Entry { name: "triage", desc: "triage the backlog by priority", hint: None, tag: "linear" },
        Entry { name: "cycle", desc: "show the current cycle's burndown", hint: None, tag: "linear" },
        Entry { name: "assign", desc: "assign the issue to an owner", hint: Some("<user>"), tag: "linear" },
        Entry { name: "label", desc: "label the issue by area", hint: None, tag: "linear" },
        Entry { name: "close-ticket", desc: "close the issue with a note", hint: None, tag: "linear" },
    ]
}

/// The files `@` searches: the files git tracks, made up here.
pub fn files() -> Vec<&'static str> {
    vec![
        "crates/log/tests/lock.rs",
        "crates/loop/tests/cancel.rs",
        "crates/log/src/serve_attach.rs",
        "docs/tui.md",
        "docs/workflow.md",
        "research/tui-prototype/src/main.rs",
        "research/tui-prototype/src/model_picker.rs",
        "fixtures/session.jsonl",
    ]
}

/// The entries whose name holds the query's first word, case-insensitive;
/// empty matches all, so arguments after the name keep filtering on it.
pub fn filter<'a>(all: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let q = query.split_whitespace().next().unwrap_or("").to_lowercase();
    all.iter()
        .filter(|e| e.name.to_lowercase().contains(&q))
        .collect()
}

/// The files whose path holds the query's first word, case-insensitive; empty matches all.
pub fn filter_files<'a>(all: &'a [&'static str], query: &str) -> Vec<&'a str> {
    let q = query.split_whitespace().next().unwrap_or("").to_lowercase();
    all.iter()
        .copied()
        .filter(|p| p.to_lowercase().contains(&q))
        .collect()
}

/// The focused row, clamped into the filtered list wherever it is used, so
/// rendering and selection always agree on the same row.
fn clamped(focus: usize, len: usize) -> usize {
    if len == 0 { 0 } else { focus.min(len - 1) }
}

pub struct State {
    /// the focused row, as an index into the filtered list
    pub focus: usize,
}

pub fn for_case(case: &str) -> State {
    let focus = match case {
        "slash" | "slash-filtered" | "at" | "at-empty" | "narrow-slash" | "narrow-at" => 0,
        // the skill with an argument-hint, focused so its row shows
        "slash-hint" => entries().iter().position(|e| e.hint.is_some()).unwrap_or(0),
        _ => panic!(
            "--completions slash|slash-filtered|slash-hint|at|at-empty|narrow-slash|narrow-at"
        ),
    };
    State { focus }
}

/// The input box's text for a static case: the query already typed.
pub fn input_for(case: &str) -> String {
    match case {
        "slash" | "slash-hint" | "narrow-slash" => "/".into(),
        "slash-filtered" => "/re".into(),
        "at" | "narrow-at" => "@test".into(),
        "at-empty" => "@zzz".into(),
        _ => panic!(
            "--completions slash|slash-filtered|slash-hint|at|at-empty|narrow-slash|narrow-at"
        ),
    }
}

/// The query behind the leading `/` or `@`.
pub fn query_of(input: &str) -> &str {
    input
        .strip_prefix('/')
        .or_else(|| input.strip_prefix('@'))
        .unwrap_or(input)
}

/// Cuts text to at most `max` cells, ending in an ellipsis when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.into();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut n = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if n + cw + 1 > max {
            break;
        }
        out.push(ch);
        n += cw;
    }
    out.push('…');
    out
}

/// The name with its matched letters in bold, when the query matches.
fn highlight(text: &str, query: &str, base: Style) -> Vec<Span<'static>> {
    let lower = text.to_lowercase();
    let Some(i) = query
        .to_lowercase()
        .split_whitespace()
        .next()
        .and_then(|q| lower.find(q))
    else {
        return vec![sp(text, base)];
    };
    let q = query.split_whitespace().next().unwrap_or(query);
    // byte offsets of the match, widened to char boundaries
    let mut a = i;
    while !text.is_char_boundary(a) {
        a += 1;
    }
    let mut b = (i + q.len()).min(text.len());
    while !text.is_char_boundary(b) {
        b -= 1;
    }
    vec![
        sp(&text[..a], base),
        sp(
            &text[a..b],
            base.add_modifier(ratatui::style::Modifier::BOLD),
        ),
        sp(&text[b..], base),
    ]
}

/// One `/` row: the name in the accent colour, the description dim, the skill's
/// argument-hint in the attention colour, and the tag right-aligned. The
/// description gives way first, so the tag is never cut.
pub fn slash_row(e: &Entry, focused: bool, query: &str, w: usize) -> Row {
    let name_style = if focused {
        bold().patch(fg(BLUE))
    } else {
        fg(BLUE)
    };
    let mut spans = vec![sp(
        if focused { "› " } else { "  " },
        if focused { fg(ORANGE) } else { dim() },
    )];
    spans.extend(highlight(e.name, query, name_style));
    spans.push(sp(" ", Style::new()));
    let hint = e.hint.map_or(String::new(), |h| format!(" {h}"));
    let fixed = 2 + e.name.width() + 1 + hint.width() + 1 + e.tag.width();
    spans.push(sp(truncate(e.desc, w.saturating_sub(fixed)), dim()));
    if e.hint.is_some() {
        spans.push(sp(hint, fg(ORANGE)));
    }
    spans.extend([sp("\t", Style::new()), sp(e.tag, dim())]);
    row(fit(&spans, w))
}

/// One `@` row: the path in the accent colour, cut from the left when long so
/// the file name stays.
pub fn file_row(path: &str, focused: bool, query: &str, w: usize) -> Row {
    let name_style = if focused {
        bold().patch(fg(BLUE))
    } else {
        fg(BLUE)
    };
    let cut = left_cut(path, w.saturating_sub(2));
    let mut spans = vec![sp(
        if focused { "› " } else { "  " },
        if focused { fg(ORANGE) } else { dim() },
    )];
    spans.extend(highlight(&cut, query, name_style));
    row(fit(&spans, w))
}

/// Rows on the raised surface, under slab edges like the input box.
fn panel(rows: Vec<Row>, w: usize) -> Vec<Row> {
    slab(rows, crate::SEL, None, w)
}

/// At most this many rows show; the footer says what is above or below.
const SHOWN: usize = 8;

/// The first and one-past-last rows shown, keeping the focus on screen.
fn window(focus: usize, len: usize) -> (usize, usize) {
    if len <= SHOWN {
        return (0, len);
    }
    let f = focus.min(len - 1);
    let start = (f + 1).saturating_sub(SHOWN).min(len - SHOWN);
    (start, start + SHOWN)
}

/// "3–8 of 40 · ↓ 32 more": the scroll hint when the list does not all fit.
fn footer(start: usize, end: usize, total: usize) -> Row {
    let mut parts = vec![];
    if start > 0 {
        parts.push(format!("↑ {start} above"));
    }
    parts.push(format!("{}–{end} of {total}", start + 1));
    if end < total {
        parts.push(format!("↓ {} more", total - end));
    }
    row(vec![sp(format!("  {}", parts.join(" · ")), dim())])
}

/// The panel above the input box: the `/` list or the `@` file search.
pub fn view(s: &State, input: &str, w: usize) -> Vec<Row> {
    let query = query_of(input);
    if input.starts_with('@') {
        let all = files();
        let m = filter_files(&all, query);
        if m.is_empty() {
            return panel(vec![row(vec![sp("  no files match", dim())])], w);
        }
        let (start, end) = window(s.focus, m.len());
        let mut rows: Vec<Row> = m[start..end]
            .iter()
            .enumerate()
            .map(|(k, p)| file_row(p, start + k == clamped(s.focus, m.len()), query, w))
            .collect();
        if m.len() > SHOWN {
            rows.push(footer(start, end, m.len()));
        }
        panel(rows, w)
    } else {
        let all = entries();
        let m = filter(&all, query);
        if m.is_empty() {
            return panel(vec![row(vec![sp("  no matches", dim())])], w);
        }
        let (start, end) = window(s.focus, m.len());
        let mut rows: Vec<Row> = m[start..end]
            .iter()
            .enumerate()
            .map(|(k, e)| slash_row(e, start + k == clamped(s.focus, m.len()), query, w))
            .collect();
        if m.len() > SHOWN {
            rows.push(footer(start, end, m.len()));
        }
        panel(rows, w)
    }
}

/// How many rows the panel shows for this input, so movement can clamp.
fn matches_len(input: &str) -> usize {
    let q = query_of(input);
    if input.starts_with('@') {
        filter_files(&files(), q).len()
    } else {
        filter(&entries(), q).len()
    }
}

fn move_focus(ui: &mut Ui, d: isize) {
    let n = matches_len(&ui.input);
    if n == 0 {
        return;
    }
    let f = ui.completions.as_ref().map_or(0, |c| c.focus) as isize;
    let f = clamped((f + d).max(0) as usize, n);
    if let Some(c) = ui.completions.as_mut() {
        c.focus = f;
    }
}

/// Tab completes the focused row into the input box, keeping the panel open.
/// Only the leading command token (or the `@` token) is replaced, so any
/// arguments already typed are kept. Enter on a `/` row closes the panel and
/// falls through to the normal Enter, which runs the completed command.
fn complete(ui: &mut Ui, close: bool) {
    let (at, focus, query) = (
        ui.input.starts_with('@'),
        ui.completions.as_ref().map_or(0, |c| c.focus),
        query_of(&ui.input).to_string(),
    );
    // the rest of the draft after the leading token, kept as typed
    let rest = ui
        .input
        .find(char::is_whitespace)
        .map_or("", |i| ui.input[i..].trim_start());
    let next = if at {
        let all = files();
        let m = filter_files(&all, &query);
        m.get(clamped(focus, m.len()))
            .map(|p| format!("{p} {rest}"))
    } else {
        let all = entries();
        let m = filter(&all, &query);
        m.get(clamped(focus, m.len()))
            .map(|e| format!("/{} {rest}", e.name))
    };
    if let Some(n) = next {
        ui.input = n;
    }
    // the filtered list changed behind the stored focus
    if let Some(c) = ui.completions.as_mut() {
        c.focus = clamped(c.focus, matches_len(&ui.input));
    }
    if close {
        ui.completions = None;
    }
}

/// The panel's keys while it is open. True when the key was consumed.
pub fn on_key(ui: &mut Ui, k: Key, m: Mods) -> bool {
    if ui.completions.is_none() {
        return false;
    }
    let plain = !m.ctrl && !m.alt && !m.sup;
    match k {
        Key::Up if !m.alt && !m.ctrl => move_focus(ui, -1),
        Key::Down if !m.alt && !m.ctrl => move_focus(ui, 1),
        Key::Tab if plain => complete(ui, false),
        Key::Enter if plain => {
            // `/` runs at once: complete, close, and let the normal Enter below
            // send the command (`/context` and `/model` open their views there).
            // `@` inserts the file's path as text, sending nothing.
            let at = ui.input.starts_with('@');
            complete(ui, true);
            if !at {
                return false;
            }
        }
        Key::Esc => ui.completions = None,
        _ => return false,
    }
    true
}

/// Typing `/` or `@` at the start of the input opens the panel; anything else,
/// or a request on top, closes it.
pub fn sync(ui: &mut Ui, top_open: bool) {
    let slash_at = ui.input.starts_with('/') || ui.input.starts_with('@');
    if ui.completions.is_none() {
        if slash_at && !top_open && ui.search.is_none() && ui.picker.is_none() && ui.qsel.is_none()
        {
            let case = if ui.input.starts_with('@') {
                "at"
            } else {
                "slash"
            };
            ui.completions = Some(for_case(case));
        }
    } else if !slash_at || top_open {
        ui.completions = None;
    }
    // typing changes the filtered list behind the stored focus
    if let Some(c) = ui.completions.as_mut() {
        c.focus = clamped(c.focus, matches_len(&ui.input));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{plain, width};

    fn ui_with(input: &str, case: &str) -> Ui {
        Ui {
            input: input.into(),
            completions: Some(for_case(case)),
            ..Default::default()
        }
    }

    fn text(rows: &[Row]) -> String {
        rows.iter().map(plain).collect::<Vec<_>>().join("\n")
    }
    fn names<'a>(m: &[&'a Entry]) -> Vec<&'a str> {
        m.iter().map(|e| e.name).collect()
    }

    #[test]
    fn forty_entries_cover_every_tag_kind() {
        let all = entries();
        assert_eq!(all.len(), 40);
        for tag in ["command", "skill", "template", "review", "linear"] {
            assert!(all.iter().any(|e| e.tag == tag), "no entry tagged {tag}");
        }
    }

    #[test]
    fn filter_matches_names() {
        let all = entries();
        assert_eq!(filter(&all, "").len(), 40, "empty query matches all");
        let m = filter(&all, "re");
        assert!(!m.is_empty() && m.len() < 40, "only some entries match");
        assert!(
            m.iter().all(|e| e.name.to_lowercase().contains("re")),
            "every match holds the query"
        );
        assert!(names(&m).contains(&"review"), "review matches re");
        assert!(!names(&m).contains(&"onboard"), "onboard does not match re");
        assert!(filter(&all, "zzz").is_empty(), "no entry matches zzz");
        assert_eq!(
            names(&filter(&all, "RE")),
            names(&m),
            "matching ignores case"
        );
        assert!(
            names(&filter(&all, "review --staged")).contains(&"review"),
            "arguments keep filtering on the name"
        );
    }

    #[test]
    fn filter_files_matches_paths() {
        let all = files();
        assert_eq!(filter_files(&all, "").len(), all.len());
        let m = filter_files(&all, "test");
        assert!(
            m.contains(&"crates/log/tests/lock.rs") && m.contains(&"crates/loop/tests/cancel.rs")
        );
        assert!(filter_files(&all, "zzz").is_empty());
        assert_eq!(
            filter_files(&all, "LOCK"),
            filter_files(&all, "lock"),
            "matching ignores case"
        );
    }

    #[test]
    fn a_row_never_exceeds_the_panel_width() {
        let long = entries().into_iter().find(|e| e.name == "login").unwrap();
        // at the width edge, one below, one above
        for w in [59, 60, 61] {
            let r = slash_row(&long, true, "", w);
            assert_eq!(width(&r.spans), w, "slash row at {w}");
            assert!(
                plain(&r).contains('…'),
                "the long description is cut at {w}"
            );
            assert!(plain(&r).contains("command"), "the tag survives at {w}");
        }
        let hinted = entries().into_iter().find(|e| e.hint.is_some()).unwrap();
        for w in [49, 50, 51] {
            let r = slash_row(&hinted, false, "", w);
            assert_eq!(width(&r.spans), w, "hint row at {w}");
        }
        for w in [39, 40, 41] {
            let r = file_row("research/tui-prototype/src/model_picker.rs", false, "", w);
            assert_eq!(width(&r.spans), w, "file row at {w}");
            assert!(
                plain(&r).contains("model_picker.rs"),
                "the file name survives at {w}"
            );
        }
    }

    #[test]
    fn the_slash_panel_shows_each_kind_with_a_scroll_hint() {
        // eight rows show at once, so each tag is checked with the focus in its rows
        for (focus, tag) in [
            (0, "command"),
            (15, "skill"),
            (24, "template"),
            (30, "review"),
            (39, "linear"),
        ] {
            let mut s = for_case("slash");
            s.focus = focus;
            let t = text(&view(&s, "/", 100));
            assert!(t.contains(tag), "missing tag {tag} at focus {focus}");
        }
        let t = text(&view(&for_case("slash"), "/", 100));
        assert!(
            t.contains("↓") && t.contains("more") && t.contains("40"),
            "missing the scroll hint"
        );
    }

    #[test]
    fn the_filtered_panel_shows_only_matches() {
        let t = text(&view(&for_case("slash-filtered"), "/re", 100));
        assert!(t.contains("review"), "review matches re");
        assert!(!t.contains("onboard"), "onboard does not match re");
    }

    #[test]
    fn the_hint_case_shows_the_argument_hint_with_the_cut() {
        let t = text(&view(&for_case("slash-hint"), "/", 100));
        assert!(t.contains("<path>"), "missing the skill's argument-hint");
        assert!(t.contains('…'), "missing the truncated description");
    }

    #[test]
    fn the_at_panel_lists_files_and_empty_says_so() {
        let t = text(&view(&for_case("at"), "@test", 100));
        assert!(
            t.contains("lock.rs") && t.contains("cancel.rs"),
            "missing the file matches"
        );
        let t = text(&view(&for_case("at-empty"), "@zzz", 100));
        assert!(t.contains("no files match"), "missing the empty row");
    }

    #[test]
    fn the_window_follows_the_focus() {
        let (a, b) = (window(0, 40), window(7, 40));
        assert_eq!((a, b), ((0, 8), (0, 8)), "the first window holds eight");
        assert_eq!(window(8, 40), (1, 9), "the window moves one row down");
        assert_eq!(window(99, 40), (32, 40), "the focus clamps to the last row");
        assert_eq!(window(0, 3), (0, 3), "a short list shows all");
        assert_eq!(window(0, 0), (0, 0), "an empty list shows none");
        // the last row on screen carries the focus marker
        let mut s = for_case("slash");
        s.focus = 39;
        let t = text(&view(&s, "/", 100));
        assert!(
            t.contains("↑ 32 above") && t.contains("33–40 of 40"),
            "missing the position"
        );
    }

    #[test]
    fn movement_clamps_at_both_ends() {
        let mut ui = ui_with("/re", "slash-filtered");
        let n = matches_len(&ui.input);
        assert!(n > 1, "the filter leaves room to move");
        move_focus(&mut ui, -1);
        assert_eq!(
            ui.completions.as_ref().unwrap().focus,
            0,
            "focus moved above the first row"
        );
        ui.completions.as_mut().unwrap().focus = n - 1;
        move_focus(&mut ui, 1);
        assert_eq!(
            ui.completions.as_ref().unwrap().focus,
            n - 1,
            "focus moved below the last row"
        );
        // below and above the list both clamp onto it
        ui.completions.as_mut().unwrap().focus = 99;
        move_focus(&mut ui, 1);
        assert_eq!(ui.completions.as_ref().unwrap().focus, n - 1);
        ui.completions.as_mut().unwrap().focus = 99;
        move_focus(&mut ui, -1);
        assert_eq!(ui.completions.as_ref().unwrap().focus, n - 1);
        // an empty list moves nothing
        ui.input = "@zzz".into();
        let held = ui.completions.as_ref().unwrap().focus;
        move_focus(&mut ui, 1);
        assert_eq!(ui.completions.as_ref().unwrap().focus, held);
    }

    #[test]
    fn tab_completes_and_enter_runs() {
        let mut ui = ui_with("/re", "slash-filtered");
        complete(&mut ui, false);
        assert_eq!(ui.input, "/review ", "tab completes the focused row");
        assert!(ui.completions.is_some(), "tab keeps the panel open");
        complete(&mut ui, true);
        assert!(ui.completions.is_none(), "enter closes the panel");
        // `@` inserts the file's path as text
        let mut ui = ui_with("@test", "at");
        complete(&mut ui, true);
        assert!(ui.input.contains("lock.rs"), "enter inserts the path");
        assert!(ui.completions.is_none());
    }

    #[test]
    fn a_stale_focus_clamps_onto_the_filtered_list() {
        // `/`, Down, Tab on `model`: the second row of the full list, one match after
        let mut ui = ui_with("/", "slash");
        move_focus(&mut ui, 1);
        assert_eq!(ui.completions.as_ref().unwrap().focus, 1);
        complete(&mut ui, false);
        assert_eq!(ui.input, "/model ");
        assert_eq!(
            ui.completions.as_ref().unwrap().focus,
            0,
            "one match left, so the focus clamps onto it"
        );
        // rendering and selection agree on the same row
        let t = text(&view(ui.completions.as_ref().unwrap(), &ui.input, 100));
        assert!(t.contains("model"), "the clamped row still shows");
        complete(&mut ui, false);
        assert_eq!(ui.input, "/model ", "a second Tab keeps the same row");
    }

    #[test]
    fn completing_keeps_the_typed_arguments() {
        // Tab selects `/review `, typing `src/main.rs` then Enter keeps `/review src/main.rs`
        let mut ui = ui_with("/re", "slash-filtered");
        complete(&mut ui, false);
        assert_eq!(ui.input, "/review ");
        ui.input.push_str("src/main.rs");
        sync(&mut ui, false);
        assert!(
            ui.completions.is_some(),
            "still a slash command, so the panel stays open"
        );
        complete(&mut ui, true);
        assert_eq!(ui.input, "/review src/main.rs", "Enter keeps the arguments");
        assert!(ui.completions.is_none(), "Enter closes the panel");
    }

    #[test]
    fn enter_on_a_slash_row_runs_and_on_a_file_row_inserts() {
        let mut ui = ui_with("/re", "slash-filtered");
        assert!(
            !on_key(&mut ui, Key::Enter, Mods::default()),
            "Enter on a `/` row falls through to the normal Enter, which sends it"
        );
        assert_eq!(ui.input, "/review ");
        assert!(ui.completions.is_none(), "Enter closes the panel");
        let mut ui = ui_with("@test", "at");
        assert!(
            on_key(&mut ui, Key::Enter, Mods::default()),
            "Enter on an `@` row is consumed: the path is inserted, nothing is sent"
        );
        assert!(ui.input.contains("lock.rs"), "enter inserts the path");
        assert!(ui.completions.is_none());
    }

    #[test]
    fn typing_slash_or_at_opens_the_panel() {
        let mut ui = Ui::default();
        sync(&mut ui, false);
        assert!(ui.completions.is_none(), "plain text opens nothing");
        ui.input = "/re".into();
        sync(&mut ui, false);
        assert!(ui.completions.is_some(), "a leading slash opens the list");
        ui.input = "see @test".into();
        ui.completions = None;
        sync(&mut ui, false);
        assert!(ui.completions.is_none(), "only the input's start counts");
        ui.input = "@test".into();
        sync(&mut ui, false);
        assert!(ui.completions.is_some(), "a leading @ opens file search");
        ui.input = "plain".into();
        sync(&mut ui, false);
        assert!(ui.completions.is_none(), "other text closes it");
        ui.input = "/re".into();
        sync(&mut ui, true);
        assert!(ui.completions.is_none(), "a request on top closes it");
    }
}
