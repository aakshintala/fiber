//! Binary-level tests for `/release-notes` (see #3447): the built fiber
//! shows its embedded title-stub view with the releases URL, and a
//! synthetic long-notes terminal wraps bullets on a narrow pty.
//!
//! The notes path sends nothing: the real-binary test routes the child's
//! every HTTP request through a recording CONNECT proxy and asserts no
//! new request across open, scroll, close and reopen. The synthetic test
//! runs `tui::run` in process, and that crate holds no HTTP client, so
//! its only wire is the hub socket, which the test drains and asserts
//! empty; the no-emission effect itself is proved by the `Effect::None`
//! assertions in `crates/tui/src/release_notes_tests.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};

/// Spawns the terminal at 120 by 32 on a fresh offline home, with the
/// child's every HTTP request tunnelled through `proxy`. No provider is
/// installed: home and the notes need none. The hub exits a second after
/// its last client leaves, so `wait` never outlasts the test's deadline.
fn terminal(setup: &support::Setup, proxy: &fakes::ConnectProxy) -> support::pty::Run {
    let url = proxy.url();
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    support::pty::Run::spawn(
        setup,
        120,
        32,
        &[],
        &[
            ("ALL_PROXY", url.as_str()),
            ("all_proxy", url.as_str()),
            ("HTTPS_PROXY", url.as_str()),
            ("https_proxy", url.as_str()),
            ("HTTP_PROXY", url.as_str()),
            ("http_proxy", url.as_str()),
        ],
    )
}

/// Quits to a clean exit: the primary screen is back and the process
/// ends 0.
fn quit(mut run: support::pty::Run) {
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored terminal", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// The grid's rows holding `needle`.
fn rows_with(grid: &support::pty::Grid, needle: &str) -> Vec<String> {
    grid.rows
        .iter()
        .filter(|row| row.contains(needle))
        .cloned()
        .collect()
}

#[test]
fn release_notes_show_the_stub_view_with_the_url_and_make_no_request() {
    let proxy = fakes::ConnectProxy::start().unwrap();
    let setup = support::Setup::new();
    let mut run = terminal(&setup, &proxy);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_bytes(0, support::pty::MOTION, "mouse motion enabled");
    let baseline = proxy.connects().len();
    run.write(b"/release-notes\r");
    run.wait_screen("the no-notes view", |grid| {
        grid.contents.contains("No notes for this version")
    });
    // The stub shows the releases page (`docs/tui.md`, "Swapped views",
    // "The release notes"): presence, not a count.
    assert!(
        !rows_with(&run.screen(), contract::releases::RELEASES).is_empty(),
        "the releases url draws"
    );
    // Esc returns to home; reopening shows the view again.
    run.write(b"\x1b[27u");
    run.wait_screen("home again", |grid| {
        !grid.contents.contains("Release notes")
    });
    run.write(b"/release-notes\r");
    let grid = run.wait_screen("the reopened view", |grid| {
        grid.contents.contains("No notes for this version")
    });
    // The header is the swapped view's fixed top row (`docs/tui.md`,
    // "Swapped views"): row 0 names the view, with "Esc returns" at its
    // right end. Nothing behind the static stub can move it — the
    // view reads the embedded stub and asks nothing — so this completed
    // frame is the click's readiness signal (`docs/testing/screens.md`,
    // "Screens"). A press and release on it closes through the header.
    let header = grid.rows.first().expect("the header row");
    assert!(
        header.contains("Release notes") && header.contains("Esc returns"),
        "the fixed top row names the view: {header:?}"
    );
    run.write(b"\x1b[<0;5;1M");
    run.write(b"\x1b[<0;5;1m");
    run.wait_screen("the closed view", |grid| {
        !grid.contents.contains("Release notes")
    });
    // A child-backed resize keeps the view reachable.
    run.write(b"/release-notes\r");
    run.wait_screen("the view before resize", |grid| {
        grid.contents.contains("No notes for this version")
    });
    run.resize(100, 28);
    run.wait_screen("the view after resize", |grid| {
        grid.contents.contains("No notes for this version")
    });
    // Presence again after the resize: no count pinned.
    assert!(
        !rows_with(&run.screen(), contract::releases::RELEASES).is_empty(),
        "the url survives resize"
    );
    quit(run);
    // Opening, scrolling, closing and reopening the notes tunnelled no
    // request through the proxy: the view reads the embedded stub and
    // asks nothing (`docs/tui.md`, "Swapped views", "The release notes").
    let connects = proxy.connects();
    assert_eq!(
        connects.len(),
        baseline,
        "no request while the notes were open: {connects:?}"
    );
    assert!(
        connects.iter().all(|target| {
            !target.contains("release")
                && !target.contains("changelog")
                && !target.contains("notes")
        }),
        "no notes-path request: {connects:?}"
    );
}

/// A long-notes source at the exact running version, with a tail
/// sentinel per entry: bullets, a paragraph and an unbroken url.
static SYNTHETIC: &str = "# Changelog\n\n## 0.0.1\n\n### New features\n\n- First synthetic bullet, long enough to wrap several times across the narrow screen, ending with the sentinel synthetic-bullet-one-tail.\n- Second synthetic bullet, also long enough to wrap several times across the narrow screen, ending with the sentinel synthetic-bullet-two-tail.\n\nA synthetic paragraph running long enough to wrap across several rows at this width, ending with the sentinel synthetic-para-tail.\n\nhttps://example.com/synthetic/long/path/with/no/spaces/that/the/wrapper/must/split/ending-with-synthetic-url-tail\n";

/// A completed frame's end: synchronized output's close marker, which the
/// TUI brackets every frame with. Waiting for it after an input proves the
/// frame drew instead of matching a stale grid.
const FRAME_END: &[u8] = b"\x1b[?2026l";

/// Reads hub lines until `feed` and `recent` both arrived: home's
/// one-shot startup burst, sent once the link is up. Bounded by the
/// deadline, so a missing burst fails instead of hanging.
fn wait_home_burst(hub: &mut UnixStream, deadline: support::Deadline) {
    hub.set_read_timeout(Some(deadline.left())).unwrap();
    let mut seen = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match hub.read(&mut chunk) {
            Ok(0) => panic!("the hub closed before its feed and recent"),
            Ok(n) => {
                seen.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&seen);
                if text.contains("\"command\":\"feed\"") && text.contains("\"command\":\"recent\"")
                {
                    break;
                }
            }
            Err(error) => panic!("hub read: {error}"),
        }
    }
    hub.set_read_timeout(None).unwrap();
    hub.set_nonblocking(true).unwrap();
    let _ = drain(hub);
}

/// Drains every byte waiting on `stream`: the hub socket's writes, if
/// the terminal sent any driver line.
fn drain(stream: &UnixStream) -> Vec<u8> {
    let stream = &mut stream.try_clone().expect("the hub side clones");
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("hub read: {error}"),
        }
    }
    out
}

#[test]
fn synthetic_long_notes_wrap_and_page_on_a_narrow_pty() {
    let deadline = support::Deadline::start();
    let term = support::pty::open(48, 14);
    let master = File::from(term.main);
    let slave = term.terminal;
    let mut run = support::pty::Run::attach(master, 48, 14, deadline);
    let (client, mut hub) = UnixStream::pair().unwrap();
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: Default::default(),
    };
    let mut held = Some((client, hello));
    let connect: tui::Connect = Box::new(move || {
        held.take()
            .ok_or_else(|| io::Error::other("the test's one connection is used"))
    });
    let clock = fakes::clock::FakeClock::new();
    // A real workspace, or home waits in its workspace picker and the
    // typed command never reaches the slash dispatch.
    let workspace = fakes::TempDir::new("fiber-notes-ws");
    let diag_home = fakes::TempDir::new("fiber-notes-diag");
    let diag = Arc::new(log::diag::Diag::new(
        diag_home.path(),
        log::diag::Process::Tui,
        log::diag::Level::Info,
        clock.clone(),
    ));
    let launch = tui::Launch {
        location: tui::LaunchLocation::Local {
            workspace: workspace.path().to_path_buf(),
            project: "notes-ws".to_owned(),
            git: false,
        },
        version: "0.0.1".to_owned(),
        release_notes: SYNTHETIC,
        model: Some("test/model".to_owned()),
        logo_glyph: "⌇".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: vec!["session".to_owned()],
        ..Default::default()
    };
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("notes-run".to_owned())
        .spawn(move || {
            let code = tui::run(slave, launch, connect, Box::new(|_| {}), clock, diag)
                .unwrap_or_else(|error| panic!("run: {error}"));
            done.send(code).unwrap_or(());
        })
        .unwrap();
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // Home's one-shot `feed` and `recent` burst: only the test's own
    // actions may write after it.
    wait_home_burst(&mut hub, deadline);
    run.write(b"/release-notes\r");
    run.wait_screen("the notes view", |grid| {
        grid.contents.contains("Release notes")
    });
    // The bullet draws its accent marker with hanging continuations.
    run.wait_screen("the wrapped bullet", |grid| {
        grid.rows.iter().any(|row| row.contains("• "))
            && grid.rows.iter().any(|row| row.starts_with("  "))
    });
    // The entries overflow the short screen: paging reaches each tail.
    // No page pins a tail: each tail pages until it shows, bounded so a
    // stuck page fails instead of hanging. The tails run in the
    // fixture's source order, which the view draws top to bottom.
    for tail in [
        "synthetic-bullet-one-tail",
        "synthetic-bullet-two-tail",
        "synthetic-para-tail",
        "synthetic-url-tail",
    ] {
        for _ in 0..10 {
            // The unbroken url splits across physical rows: join them to
            // read the tail whole.
            if run.screen().rows.join("").contains(tail) {
                break;
            }
            let from = run.output().len();
            run.write(b"\x1b[6~");
            run.wait_bytes(from, FRAME_END, "the page frame");
        }
        assert!(
            run.screen().rows.join("").contains(tail),
            "paging reaches {tail}"
        );
    }
    // Esc returns home; reopening shows the view again.
    run.write(b"\x1b[27u");
    run.wait_screen("home again", |grid| {
        !grid.contents.contains("Release notes")
    });
    run.write(b"/release-notes\r");
    run.wait_screen("the reopened view", |grid| {
        // Reopening starts at the top: the first bullet's tail shows
        // with no paging.
        grid.contents.contains("synthetic-bullet-one-tail")
    });
    // Open, scroll, close and reopen wrote no driver line to the hub:
    // the view is local, and this crate holds no HTTP client.
    let hub_bytes = drain(&hub);
    assert!(
        hub_bytes.is_empty(),
        "the notes sent nothing to the hub: {:?}",
        String::from_utf8_lossy(&hub_bytes)
    );
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored terminal", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let code = deadline
        .recv(&finished)
        .expect("run returns under the deadline");
    assert_eq!(code, 0);
}
