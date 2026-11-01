//! Tests for `/mcp`: one row per server with its state and count, the
//! rows sorted, Enter per row kind, the last note, and the wait's answer
//! matching (`docs/tui.md`, "Swapped views"; `docs/mcp.md`, "Servers,
//! their state and the per-server commands").

use std::path::Path;

use super::{Act, Mcp, fit_name};
use crate::configure_fake::Fake;
use crate::keys::Key;
use crate::settings_view::Ctx;
use crate::swapped::{Ink, Spot};
use crate::theme::Role;
use contract::events::{McpServerLayer, McpServerRow, McpServerState};

const ID: &str = "c_1";

fn row(
    name: &str,
    layer: McpServerLayer,
    state: McpServerState,
    reason: &str,
    tools: Option<u64>,
    approved: bool,
    auth_url: Option<&str>,
) -> McpServerRow {
    McpServerRow {
        name: name.to_owned(),
        layer,
        state,
        reason: reason.to_owned(),
        tools,
        approved,
        auth_url: auth_url.map(str::to_owned),
    }
}

fn ready(name: &str) -> McpServerRow {
    row(
        name,
        McpServerLayer::Global,
        McpServerState::Ready,
        "",
        Some(3),
        true,
        None,
    )
}

fn failed(name: &str) -> McpServerRow {
    row(
        name,
        McpServerLayer::Project,
        McpServerState::Failed,
        &format!("The MCP server '{name}' failed to start: refused."),
        Some(2),
        true,
        None,
    )
}

fn needs_login(name: &str) -> McpServerRow {
    row(
        name,
        McpServerLayer::Global,
        McpServerState::NeedsLogin,
        &format!("The MCP server '{name}' failed to start: HTTP 401."),
        Some(0),
        true,
        None,
    )
}

fn not_started(name: &str) -> McpServerRow {
    row(
        name,
        McpServerLayer::Repository,
        McpServerState::NotStarted,
        "",
        None,
        true,
        None,
    )
}

/// A context over `fake`: the view reads no seam, so any fake fills the
/// shape.
fn context(fake: &Fake) -> Ctx<'_> {
    Ctx {
        seam: fake,
        workspace: Path::new("/w"),
        height: 22,
        width: 80,
        usage: None,
    }
}

/// The view over `rows` answered to its opening command, with its fake.
fn opened(rows: &[McpServerRow]) -> (Fake, Mcp) {
    let fake = Fake::new(Vec::new());
    let context = context(&fake);
    let mut mcp = Mcp::open(ID.to_owned());
    mcp.answered(ID, rows, &context);
    (fake, mcp)
}

/// Every row's text joined.
fn texts(mcp: &Mcp) -> Vec<String> {
    mcp.frame(None)
        .rows
        .into_iter()
        .map(|cells| {
            cells
                .into_iter()
                .map(|(text, _, _)| text)
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

/// Every row's ink.
fn inks(mcp: &Mcp) -> Vec<Ink> {
    mcp.frame(None)
        .rows
        .into_iter()
        .map(|cells| cells[0].2)
        .collect()
}

#[test]
fn a_name_exactly_the_column_wide_is_not_cut() {
    // The boundary a mutant flipping the comparison would cut with `…`.
    let name = "r".repeat(super::NAME);
    assert_eq!(fit_name(&name), name);
}

#[test]
fn rows_show_name_layer_state_and_count() {
    let (_fake, mcp) = opened(&[ready("linear"), failed("down"), needs_login("remote")]);
    assert_eq!(
        texts(&mcp)[..3],
        [
            format!(
                "  {} · project · failed · The MCP server 'down' failed to start: refused.",
                fit_name("down")
            ),
            format!("  {} · global · ready · 3 tools", fit_name("linear")),
            format!("  {} · global · needs-login", fit_name("remote")),
        ]
    );
}

#[test]
fn rows_sort_by_name_with_the_note_last() {
    let (_fake, mcp) = opened(&[ready("zeta"), ready("alpha"), not_started("mid")]);
    let texts = texts(&mcp);
    assert_eq!(texts.len(), 4);
    assert!(texts[0].contains("alpha"));
    assert!(texts[1].contains("mid"));
    assert!(texts[2].contains("zeta"));
    assert_eq!(texts[3], "  Per-tool on/off switches stay in `/tools`.");
}

#[test]
fn states_keep_their_colours_and_the_note_is_dim() {
    let (_fake, mcp) = opened(&[
        not_started("new"),
        ready("up"),
        failed("down"),
        needs_login("remote"),
    ]);
    // Sorted: down, new, remote, up, then the note.
    assert_eq!(
        inks(&mcp),
        [
            Ink::Role(Role::Error),
            Ink::Muted,
            Ink::Role(Role::Attention),
            Ink::Plain,
            Ink::Muted,
        ]
    );
}

#[test]
fn not_started_has_no_count_and_one_tool_sings_singular() {
    let (_fake, mcp) = opened(&[
        not_started("new"),
        row(
            "solo",
            McpServerLayer::Global,
            McpServerState::Ready,
            "",
            Some(1),
            true,
            None,
        ),
    ]);
    let texts = texts(&mcp);
    assert!(texts[0].contains("not started") && !texts[0].contains("tool"));
    assert!(texts[1].contains("ready · 1 tool"));
}

#[test]
fn page_down_moves_by_the_shown_height_not_one_row() {
    // `shown` is the frame's rows in the 22-row context; a mutant
    // returning one row would move a single row per page.
    let servers: Vec<McpServerRow> = (0..25).map(|n| failed(&format!("s{n:02}"))).collect();
    let (fake, mut mcp) = opened(&servers);
    let context = context(&fake);
    assert!(matches!(mcp.key(&Key::PageDown, &context), Act::Stay));
    let Act::McpReconnect(name) = mcp.key(&Key::Enter, &context) else {
        panic!("a page down and Enter reconnects");
    };
    assert_ne!(name, "s01", "a page moves more than one row");
}

#[test]
fn enter_reconnects_a_failed_server() {
    let (fake, mut mcp) = opened(&[ready("up"), failed("down")]);
    // Sorted first: down.
    let context = context(&fake);
    assert!(
        matches!(mcp.key(&Key::Enter, &context), Act::McpReconnect(name) if name == "down"),
        "enter reconnects the failed server"
    );
}

#[test]
fn enter_logs_in_a_server_that_needs_it() {
    let (fake, mut mcp) = opened(&[needs_login("remote")]);
    let context = context(&fake);
    assert!(
        matches!(mcp.key(&Key::Enter, &context), Act::McpLogin(name) if name == "remote"),
        "enter logs in the server that needs it"
    );
}

#[test]
fn enter_is_silent_on_ready_not_started_and_the_note() {
    let (fake, mut mcp) = opened(&[not_started("new"), ready("up")]);
    let context = context(&fake);
    assert!(matches!(mcp.key(&Key::Enter, &context), Act::Stay));
    assert!(mcp.frame(None).below.is_empty());
    assert!(matches!(mcp.key(&Key::Down, &context), Act::Stay));
    assert!(matches!(mcp.key(&Key::Enter, &context), Act::Stay));
    assert!(mcp.frame(None).below.is_empty());
    assert!(matches!(mcp.key(&Key::Down, &context), Act::Stay));
    assert!(matches!(mcp.key(&Key::Enter, &context), Act::Stay));
    assert!(mcp.frame(None).below.is_empty());
}

#[test]
fn enter_on_unapproved_gives_the_approve_advice_and_sends_nothing() {
    let (fake, mut mcp) = opened(&[row(
        "repo",
        McpServerLayer::Repository,
        McpServerState::Failed,
        "The MCP server 'repo' is not approved.",
        Some(0),
        false,
        None,
    )]);
    let context = context(&fake);
    assert!(matches!(mcp.key(&Key::Enter, &context), Act::Stay));
    assert_eq!(
        mcp.frame(None).below,
        [
            "`repo` is declared by the repository but not approved; run `fiber approve` in the repository."
        ]
    );
}

#[test]
fn unapproved_rows_show_as_such() {
    let (_fake, mcp) = opened(&[row(
        "repo",
        McpServerLayer::Repository,
        McpServerState::NotStarted,
        "",
        None,
        false,
        None,
    )]);
    // The skipped server never started, but its row names the reason:
    // not its not-started state.
    assert_eq!(
        texts(&mcp)[..1],
        [format!(
            "  {} · repository · not approved",
            fit_name("repo")
        )]
    );
    assert_eq!(inks(&mcp)[0], Ink::Role(Role::Attention));
}

#[test]
fn the_wait_shows_with_the_url_cut_to_fit() {
    let url = format!("https://login.example/{}", "a".repeat(200));
    let (fake, mut mcp) = opened(&[row(
        "remote",
        McpServerLayer::Global,
        McpServerState::NeedsLogin,
        "The MCP server 'remote' failed to start: HTTP 401.",
        Some(0),
        true,
        Some(&url),
    )]);
    mcp.sent("remote", "c_9".to_owned(), true);
    let rows = texts(&mcp);
    assert_eq!(rows.len(), 2);
    let row = &rows[0];
    assert!(
        row.starts_with(&format!(
            "  {} · global · needs-login · waiting for login · https://login.",
            fit_name("remote")
        )),
        "the wait shows with the login URL: {row}"
    );
    assert_eq!(row.chars().count(), 80);
    assert_eq!(inks(&mcp)[0], Ink::Role(Role::Attention));
    let _ = &fake;
}

#[test]
fn a_login_sent_without_a_url_yet_still_shows_the_wait() {
    // The hub answers before the login presents its URL: the row still
    // waits, with no URL to cut. A mutant dropping the sent wait, or
    // requiring the URL beside it, reads plain needs-login.
    let (fake, mut mcp) = opened(&[needs_login("remote")]);
    mcp.sent("remote", "c_9".to_owned(), true);
    let rows = texts(&mcp);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        format!(
            "  {} · global · needs-login · waiting for login",
            fit_name("remote")
        )
    );
    assert_eq!(inks(&mcp)[0], Ink::Role(Role::Attention));
    let _ = &fake;
}

#[test]
fn another_clients_login_url_shows_as_a_wait_cut_to_fit() {
    // No command sent here: the snapshot carries another client's
    // login URL, and the row waits on it, cut to the 80 columns the
    // answer arrived for.
    let url = format!("https://login.example/{}", "a".repeat(200));
    let (_fake, mcp) = opened(&[row(
        "remote",
        McpServerLayer::Global,
        McpServerState::NeedsLogin,
        "The MCP server 'remote' failed to start: HTTP 401.",
        Some(0),
        true,
        Some(&url),
    )]);
    let rows = texts(&mcp);
    assert_eq!(rows.len(), 2);
    let row = &rows[0];
    assert!(
        row.starts_with(&format!(
            "  {} · global · needs-login · waiting for login · https://login.",
            fit_name("remote")
        )),
        "the wait shows with the login URL: {row}"
    );
    assert_eq!(row.chars().count(), 80);
    assert_eq!(inks(&mcp)[0], Ink::Role(Role::Attention));
}

#[test]
fn only_the_own_answer_fills_and_only_its_rejection_says() {
    let fake = Fake::new(Vec::new());
    let context = context(&fake);
    let mut mcp = Mcp::open(ID.to_owned());
    mcp.answered("c_old", &[ready("up")], &context);
    // An older answer changes nothing: the view still waits.
    assert!(mcp.frame(None).below[0].contains("Reading the servers"));
    mcp.rejected("c_old", "gone");
    assert!(mcp.frame(None).below[0].contains("Reading the servers"));
    mcp.rejected(ID, "denied");
    assert_eq!(mcp.frame(None).below, ["denied"]);
}

#[test]
fn a_followup_answer_clears_its_wait_and_nothing_else() {
    let (_fake, mut mcp) = opened(&[needs_login("remote"), failed("down")]);
    mcp.sent("remote", "c_9".to_owned(), true);
    mcp.sent("down", "c_10".to_owned(), false);
    assert!(mcp.followup("c_9"));
    assert!(!mcp.followup("c_9"));
    assert!(!mcp.followup("c_other"));
    assert!(mcp.followup("c_10"));
}

#[test]
fn a_fresh_snapshot_keeps_only_the_waits_its_rows_show() {
    // A wait for a server the snapshot still names survives; one for
    // a server it no longer names is over. A mutant flipping the
    // retain's comparison would keep the gone wait and drop the live
    // one.
    let (fake, mut mcp) = opened(&[needs_login("remote"), failed("down")]);
    mcp.sent("remote", "c_9".to_owned(), true);
    mcp.sent("down", "c_10".to_owned(), false);
    let context = context(&fake);
    mcp.expect("c_2".to_owned());
    mcp.answered("c_2", &[needs_login("remote")], &context);
    assert!(mcp.followup("c_9"), "the live wait survives");
    assert!(!mcp.followup("c_10"), "the gone wait is over");
}

#[test]
fn esc_closes_and_moving_clears_what_was_said() {
    let (fake, mut mcp) = opened(&[
        failed("down"),
        row(
            "repo",
            McpServerLayer::Repository,
            McpServerState::Failed,
            "The MCP server 'repo' is not approved.",
            Some(0),
            false,
            None,
        ),
    ]);
    let context = context(&fake);
    // Sorted: down, repo, then the note.
    assert!(matches!(mcp.key(&Key::Down, &context), Act::Stay));
    assert!(matches!(mcp.key(&Key::Enter, &context), Act::Stay));
    assert!(!mcp.frame(None).below.is_empty());
    assert!(matches!(mcp.key(&Key::Down, &context), Act::Stay));
    assert!(mcp.frame(None).below.is_empty());
    assert!(matches!(mcp.key(&Key::Esc, &context), Act::Close));
}

#[test]
fn clicking_selects_the_row_enter_acts_on() {
    let (fake, mut mcp) = opened(&[failed("down"), needs_login("remote")]);
    let context = context(&fake);
    mcp.click(Spot::Row(1), &context);
    assert!(
        matches!(mcp.key(&Key::Enter, &context), Act::McpLogin(name) if name == "remote"),
        "enter acts on the clicked row"
    );
}

/// The wheel shifts the visible rows three a step, keeping a visible
/// selection where it was (`docs/tui.md:576`, "Swapped views"; see #3578).
#[test]
fn wheel_shifts_three_rows_and_keeps_a_visible_selection() {
    let rows: Vec<McpServerRow> = (0..30).map(|n| ready(&format!("s{n:02}"))).collect();
    let (fake, mut mcp) = opened(&rows);
    let context = context(&fake);
    for _ in 0..10 {
        assert!(matches!(mcp.key(&Key::Down, &context), Act::Stay));
    }
    let (top, selected) = (mcp.list.top(), mcp.list.selected());
    assert!(matches!(mcp.wheel(false, &context), Act::Stay));
    assert_eq!(mcp.list.top(), top + 3);
    assert_eq!(mcp.list.selected(), selected, "a visible selection stays");
}

/// A wheel that moves clears what was said, while a wheel clamped at the
/// top preserves it (`docs/tui.md:576`).
#[test]
fn wheel_move_clears_and_clamp_preserves_what_was_said() {
    let rows: Vec<McpServerRow> = (0..30).map(|n| ready(&format!("s{n:02}"))).collect();
    let (fake, mut mcp) = opened(&rows);
    let context = context(&fake);
    mcp.said = vec!["noted".to_owned()];
    let (top, selected) = (mcp.list.top(), mcp.list.selected());
    assert!(matches!(mcp.wheel(false, &context), Act::Stay));
    assert_eq!(mcp.list.top(), top + 3, "the wheel moves three rows");
    assert!(mcp.said.is_empty(), "a moving wheel clears the message");
    mcp.said = vec!["noted".to_owned()];
    assert!(matches!(mcp.wheel(true, &context), Act::Stay));
    assert!(matches!(mcp.wheel(true, &context), Act::Stay));
    let (clamped_top, clamped_selected) = (mcp.list.top(), mcp.list.selected());
    mcp.said = vec!["held".to_owned()];
    assert!(matches!(mcp.wheel(true, &context), Act::Stay));
    assert_eq!(
        (mcp.list.top(), mcp.list.selected()),
        (clamped_top, clamped_selected),
        "a clamped wheel moves nothing"
    );
    assert_eq!(mcp.said, ["held"], "a clamped wheel preserves the message");
    let _ = selected;
}
