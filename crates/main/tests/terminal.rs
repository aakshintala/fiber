//! Binary-level tests of the terminal door (`docs/testing.md`, "Screens"):
//! the real binary in a pseudo-terminal: the first frame, a journey that
//! types a prompt, sees the answer and cancels a turn, an approval
//! answered from the panel, a repository's offer answered from its view,
//! resize, and no tty.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Response, Watchdog};
use rustix::pty;
use serde_json::{Value, json};
use support::Deadline;

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        Self::within(Deadline::start())
    }

    fn within(deadline: Deadline) -> Self {
        let root = fakes::TempDir::new("ft");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, makes `fake/m` the configured model, and idles the hub
    /// out a second after its last client leaves, so no hub lingers.
    fn provider(&self, server: &ProviderServer) {
        self.provider_with(&json!({}), server);
    }

    /// [`Setup::provider`], with the fake model declaring `input`
    /// `["text", "image"]`, so a pasted image is sent as an image part.
    fn provider_with_images(&self, server: &ProviderServer) {
        self.provider_with(&json!({"input": ["text", "image"]}), server);
    }

    fn provider_with(&self, model_extra: &Value, server: &ProviderServer) {
        self.provider_full(model_extra, server, None);
    }

    /// [`Setup::provider_with`], with the fake model declaring thinking
    /// levels and the panel pinned to `panel_width` percent of the
    /// screen (`docs/tui.md`, "Layout").
    fn provider_with_panel(&self, server: &ProviderServer, panel_width: f64) {
        self.provider_full(
            &json!({"thinking_levels": ["low", "high"], "thinking_default": "high"}),
            server,
            Some(panel_width),
        );
    }

    fn provider_full(
        &self,
        model_extra: &Value,
        server: &ProviderServer,
        panel_width: Option<f64>,
    ) {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        let mut model = json!({"id": "m", "protocol": "openai-responses",
            "base_url": format!("{}/v1", server.url()), "context_window": 100000});
        for (key, value) in model_extra.as_object().unwrap() {
            model[key] = value.clone();
        }
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [model]
            }),
        );
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        let mut config = json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}});
        if let Some(width) = panel_width {
            config["tui"] = json!({"panel": {"width": width}});
        }
        write(&self.home().join("config.json"), &config);
    }

    /// The project's sessions directory, through the canonical workspace,
    /// as the project key names it.
    fn sessions(&self) -> PathBuf {
        let workspace = fs::canonicalize(self.workspace()).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        self.home().join("projects").join(key).join("sessions")
    }

    /// The one session in [`Setup::sessions`].
    fn only_session(&self) -> String {
        let mut ids: Vec<String> = fs::read_dir(self.sessions())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(ids.len(), 1, "one session");
        ids.pop().unwrap()
    }

    /// Runs `fiber` with `args` headless in its own process group,
    /// waiting under the test's [`Deadline`]. A watchdog beside it kills
    /// that group if this process dies first.
    fn fiber(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(
            fakes::group_empties(group, self.deadline.left()),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup());
        output
    }
}

/// A pseudo-terminal. The main side stays open while the run uses
/// the terminal side.
struct Terminal {
    main: OwnedFd,
    terminal: fs::File,
    cols: u16,
    rows: u16,
}

impl Terminal {
    /// A 120x32 terminal.
    fn open() -> Self {
        Self::sized(120, 32)
    }

    /// A `cols` by `rows` terminal, as `open` is 120 by 32.
    fn sized(cols: u16, rows: u16) -> Self {
        let main = pty::openpt(pty::OpenptFlags::RDWR | pty::OpenptFlags::NOCTTY).unwrap();
        // Not inherited: a hub `fiber` starts would hold the master open.
        rustix::io::fcntl_setfd(&main, rustix::io::FdFlags::CLOEXEC).unwrap();
        pty::grantpt(&main).unwrap();
        pty::unlockpt(&main).unwrap();
        let name = pty::ptsname(&main, Vec::new()).unwrap();
        let path = PathBuf::from(OsStr::from_bytes(name.as_bytes()));
        let terminal = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        rustix::termios::tcsetwinsize(
            &terminal,
            rustix::termios::Winsize {
                ws_col: cols,
                ws_row: rows,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .unwrap();
        Self {
            main,
            terminal,
            cols,
            rows,
        }
    }

    fn stdin(&self) -> Stdio {
        Stdio::from(self.terminal.try_clone().unwrap())
    }
}

/// Spawns `command` in a new process group, then a watchdog in its own
/// group. Dropping the watchdog kills the group; standing it down exits
/// quietly once the child is reaped and the group is empty.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        support::kill_group_detached(self.0, "KILL");
    }
}

/// The terminal under test: its child, the pty master, and one reader
/// thread appending every master chunk to the output, waking a waiter
/// after each.
/// Sessions the hub starts run in their own process groups, guarded by
/// a matching watchdog on the workspace; the hub idles out on its own.
struct Run {
    child: Child,
    /// The hub's socket, removed when the hub exits.
    hub_socket: PathBuf,
    watchdog: Watchdog,
    #[allow(dead_code, reason = "held until the end to kill sessions on drop")]
    sessions: Watchdog,
    main: fs::File,
    /// One wake per chunk appended; held by one waiter at a time.
    wakes: Arc<Mutex<mpsc::Receiver<()>>>,
    output: Arc<Mutex<Vec<u8>>>,
    /// The screen grid rebuilt from the output so far, and whether the
    /// reader reached end of file.
    screen: Arc<Mutex<Shared>>,
    /// A resize the reader has not applied to its parser yet, as
    /// (columns, rows).
    pending_size: Arc<Mutex<Option<(u16, u16)>>>,
    /// Where the last `read_until` match ended.
    seen: usize,
    deadline: Deadline,
}

impl Run {
    /// Spawns `fiber` with no arguments on a pty: standard input, output
    /// and error all on the terminal side, as on a real terminal.
    fn terminal(setup: &Setup) -> Self {
        Self::terminal_full(setup, &[], &[])
    }

    /// Spawns `fiber` as [`terminal`] does, with `env` added to the
    /// child's environment.
    fn terminal_with(setup: &Setup, env: &[(&str, &str)]) -> Self {
        Self::terminal_full(setup, env, &[])
    }

    /// Spawns `fiber` as [`terminal`] does, with `args` after the binary.
    fn terminal_args(setup: &Setup, args: &[&str]) -> Self {
        Self::terminal_full(setup, &[], args)
    }

    /// Spawns `fiber` as `terminal` does, with `env` added to the
    /// child's environment and `args` after the binary.
    fn terminal_full(setup: &Setup, env: &[(&str, &str)], args: &[&str]) -> Self {
        Self::terminal_full_sized(setup, env, args, 120, 32)
    }

    /// Spawns `fiber` as `terminal_full` does, on a `cols` by `rows`
    /// terminal.
    fn terminal_full_sized(
        setup: &Setup,
        env: &[(&str, &str)],
        args: &[&str],
        cols: u16,
        rows: u16,
    ) -> Self {
        let terminal = Terminal::sized(cols, rows);
        let sessions = Watchdog::matching(setup.workspace().to_str().unwrap());
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .current_dir(setup.workspace())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", setup.root.path())
            .env("FIBER_HOME", setup.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            // The grid the harness rebuilds is a fixed dark xterm: the
            // binary must not read the ambient terminal's kind or theme.
            .env("TERM", "xterm-256color");
        for (key, value) in env {
            command.env(key, value);
        }
        command.args(args);
        command
            .stdin(terminal.stdin())
            .stdout(terminal.stdin())
            .stderr(terminal.stdin());
        let (child, watchdog) = spawn_watched(&mut command);
        let main = fs::File::from(terminal.main);
        let (tx, rx) = mpsc::channel();
        let output = Arc::new(Mutex::new(Vec::new()));
        let screen = Arc::new(Mutex::new(Shared::default()));
        let pending_size = Arc::new(Mutex::new(None));
        {
            let appended = Arc::clone(&output);
            let screen = Arc::clone(&screen);
            let pending_size = Arc::clone(&pending_size);
            let mut reader = main.try_clone().unwrap();
            let mut writer = main.try_clone().unwrap();
            thread::Builder::new()
                .name("terminal-read".to_owned())
                .spawn(move || {
                    // One parser for the whole run: styles, the cursor and
                    // the alternate screen carry across reads, so a query
                    // split across two chunks is still answered.
                    let mut parser = vt100::Parser::new(rows, cols, 0);
                    let mut pending: Vec<u8> = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        if let Some((cols, rows)) = pending_size.lock().unwrap().take() {
                            parser.screen_mut().set_size(rows, cols);
                        }
                        match reader.read(&mut buf) {
                            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let Some(bytes) = buf.get(..n) else { break };
                                pending.extend_from_slice(bytes);
                                let replies = query_replies(&mut pending);
                                // A write that fails means the terminal is
                                // gone: end quietly, as at end of file.
                                if !replies.is_empty() && writer.write_all(&replies).is_err() {
                                    break;
                                }
                                parser.process(bytes);
                                appended.lock().unwrap().extend_from_slice(bytes);
                                screen.lock().unwrap().grid = snapshot(&parser);
                                tx.send(()).unwrap_or(());
                            }
                        }
                    }
                    screen.lock().unwrap().ended = true;
                    tx.send(()).unwrap_or(());
                })
                .unwrap();
        }
        Self {
            child,
            hub_socket: setup.home().join("run").join("hub"),
            watchdog,
            sessions,
            main,
            wakes: Arc::new(Mutex::new(rx)),
            output,
            screen,
            pending_size,
            seen: 0,
            deadline: setup.deadline,
        }
    }
}

/// The rebuilt screen the grid waits read: text without styling, the
/// cursor, and the flags the restore assertions need.
#[derive(Clone, Debug, Default)]
struct Grid {
    contents: String,
    rows: Vec<String>,
    cursor: (u16, u16),
    #[allow(dead_code, reason = "the restore assertions read it from a later task on")]
    alternate_screen: bool,
    #[allow(dead_code, reason = "the restore assertions read it from a later task on")]
    hide_cursor: bool,
}

/// What the reader shares with the test: the latest grid, and whether
/// the reader reached end of file.
#[derive(Clone, Debug, Default)]
struct Shared {
    grid: Grid,
    ended: bool,
}

/// One snapshot of the parser's screen.
fn snapshot(parser: &vt100::Parser) -> Grid {
    let screen = parser.screen();
    let (_, cols) = screen.size();
    Grid {
        contents: screen.contents(),
        rows: screen.rows(0, cols).collect(),
        cursor: screen.cursor_position(),
        alternate_screen: screen.alternate_screen(),
        hide_cursor: screen.hide_cursor(),
    }
}

/// A capability query Fiber emits (`crates/tui/src/term.rs`,
/// `crates/tui/src/appearance.rs`) and the harness's reply: kitty's
/// disambiguate flags, a bare device-attributes answer, and a black
/// background with a dark theme report, so the grid is a fixed dark
/// xterm whose Esc key arrives as `CSI 27 u`.
const CAPABILITIES: [(&[u8], &[u8]); 4] = [
    (b"\x1b[?u", b"\x1b[?1u"),
    (b"\x1b[c", b"\x1b[?0c"),
    (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
    (b"\x1b[?996n", b"\x1b[?997;1n"),
];

/// How many trailing bytes `pending` keeps for a query split across two
/// reads: longer than the longest query above.
const PENDING_KEEP: usize = 16;

/// The replies for every whole query in `pending`, in stream order,
/// dropping the bytes through each answered query and keeping the tail
/// for a query still arriving.
fn query_replies(pending: &mut Vec<u8>) -> Vec<u8> {
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || haystack.len() < needle.len() {
            return None;
        }
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
    let mut replies = Vec::new();
    loop {
        let mut first: Option<(usize, usize)> = None;
        for (at, (query, _)) in CAPABILITIES.iter().enumerate() {
            if let Some(pos) = find(pending, query)
                && first.map_or(true, |(best, _)| pos < best)
            {
                first = Some((pos, at));
            }
        }
        let Some((pos, at)) = first else { break };
        replies.extend_from_slice(CAPABILITIES[at].1);
        pending.drain(..pos + CAPABILITIES[at].0.len());
    }
    let drop = pending.len().saturating_sub(PENDING_KEEP);
    pending.drain(..drop);
    replies
}

impl Run {
    /// The grid rebuilt from the output so far.
    #[allow(dead_code, reason = "the grid waits use it from the next task on")]
    fn screen(&self) -> Grid {
        self.screen.lock().unwrap().grid.clone()
    }

    /// The grid's text.
    #[allow(dead_code, reason = "the grid waits use it from the next task on")]
    fn screen_contents(&self) -> String {
        self.screen().contents
    }

    /// The grid's rows, top to bottom, without newlines.
    #[allow(dead_code, reason = "the grid waits use it from the next task on")]
    fn screen_rows(&self) -> Vec<String> {
        self.screen().rows
    }

    /// The grid's cursor position, as (row, column).
    #[allow(dead_code, reason = "the grid waits use it from the next task on")]
    fn cursor_position(&self) -> (u16, u16) {
        self.screen().cursor
    }

    /// Waits under one named deadline for the whole wait until the grid
    /// matches, however many frames arrive. When the terminal ends first
    /// the panic shows the last grid, so a stall says how far the journey
    /// got.
    #[allow(dead_code, reason = "the grid waits use it from the next task on")]
    fn wait_screen(&self, what: &str, mut matches: impl FnMut(&Grid) -> bool) {
        let wakes = self.wakes.lock().unwrap();
        loop {
            let shared = self.screen.lock().unwrap().clone();
            if matches(&shared.grid) {
                return;
            }
            if shared.ended {
                panic!(
                    "waited until the deadline for {what}; the terminal ended; screen:\n{}",
                    shared.grid.contents
                );
            }
            if wakes.recv_timeout(self.deadline.left()).is_err() {
                let contents = self.screen.lock().unwrap().grid.contents.clone();
                panic!("waited until the deadline for {what}; screen:\n{contents}");
            }
        }
    }

    /// Resizes the terminal to `cols` by `rows`: the parser follows, so
    /// the grid the waits read draws at the new size.
    #[allow(dead_code, reason = "the resize journey uses it from a later task on")]
    fn resize(&mut self, cols: u16, rows: u16) {
        rustix::termios::tcsetwinsize(
            &self.main,
            rustix::termios::Winsize {
                ws_col: cols,
                ws_row: rows,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .unwrap();
        *self.pending_size.lock().unwrap() = Some((cols, rows));
        support::kill_pid(self.deadline, self.child.id(), "WINCH").unwrap();
    }

    /// Types bytes into the terminal, on a thread bounded by the test's
    /// [`Deadline`].
    fn write(&mut self, bytes: &[u8]) {
        let mut main = self.main.try_clone().unwrap();
        let bytes = bytes.to_vec();
        support::bounded(self.deadline, "typing on the terminal", move || {
            main.write_all(&bytes)?;
            main.flush()
        })
        .unwrap();
    }

    /// The output so far.
    fn output(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    /// Reads until the output after the last match holds `needle`, under
    /// one named deadline for the whole wait, however much other output
    /// arrives. On expiry the panic names what it waited for and shows the
    /// output, so a stall says how far the journey got. A needle with a
    /// space matches its words in order with only a terminal frame's gap
    /// between them: spaces, cursor moves (`\x1b[<r>;<c>H`) and SGR
    /// (`\x1b[...m`); unchanged cells are never rewritten, so a space can
    /// arrive as a cursor move rather than a byte, and a style change can
    /// split words with SGR.
    fn read_until(&mut self, needle: &str) {
        let (output, wakes, seen) = (Arc::clone(&self.output), Arc::clone(&self.wakes), self.seen);
        let wanted = needle.to_owned();
        let (done, found) = mpsc::channel();
        thread::spawn(move || {
            let wakes = wakes.lock().unwrap();
            loop {
                if let Some(end) = phrase_end(&output.lock().unwrap(), seen, &wanted) {
                    done.send(end).unwrap_or(());
                    return;
                }
                if wakes.recv().is_err() {
                    return;
                }
            }
        });
        let Ok(end) = found.recv_timeout(self.deadline.left()) else {
            let output = self.output();
            let output = String::from_utf8_lossy(&output);
            panic!("waited until the deadline for {needle:?}; output: {output:?}");
        };
        self.seen = end;
    }

    /// Waits for the child to exit, reaps it, and returns its output, then
    /// for the hub to idle out and remove its socket: a hub whose home is
    /// deleted under it never exits. Dropping `self` kills the hub's
    /// sessions through the matching watchdog.
    fn wait(self) -> std::process::Output {
        // The reader thread holds only a dup of the master; dropping it
        // here does not close the child's terminal.
        let group = self.child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(self.deadline, group, &finished, "`fiber` to exit"),
        };
        assert!(
            fakes::group_empties(group, self.deadline.left()),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        self.watchdog.stand_down(self.deadline.cleanup());
        until_socket(
            self.deadline,
            &self.hub_socket,
            false,
            "the hub to idle out",
        );
        output
    }
}

/// Waits under the test's [`Deadline`] until `socket` exists or not, as
/// `present` says, naming `what` on expiry.
fn until_socket(deadline: Deadline, socket: &Path, present: bool, what: &str) {
    let socket = socket.to_owned();
    let (done, reached) = mpsc::channel();
    thread::spawn(move || {
        while socket.exists() != present {
            thread::yield_now();
        }
        done.send(()).unwrap_or(());
    });
    assert!(
        reached.recv_timeout(deadline.left()).is_ok(),
        "waited until the deadline for {what}"
    );
}

/// Whether `haystack` holds `needle` as bytes.
fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Where `needle` ends in `haystack` at or after `from`: an exact byte
/// match, unless `needle` holds a space and starts with a non-escape
/// byte, when each single space may instead be spaces, cursor moves and
/// SGR in any mix, and nothing else.
fn phrase_end(haystack: &[u8], from: usize, needle: &str) -> Option<usize> {
    let wanted = needle.as_bytes();
    if wanted.is_empty() {
        return Some(from.min(haystack.len()));
    }
    if !needle.contains(' ') || wanted[0] == 0x1b {
        return exact_end(haystack, from, wanted);
    }
    let words: Vec<&[u8]> = needle.split_whitespace().map(str::as_bytes).collect();
    if words.len() < 2 {
        return exact_end(haystack, from, wanted);
    }
    let mut cursor = from;
    while cursor + words[0].len() <= haystack.len() {
        let at = haystack[cursor..]
            .windows(words[0].len())
            .position(|window| window == words[0])?;
        let mut pos = cursor + at + words[0].len();
        let mut matched = true;
        for word in &words[1..] {
            let Some(gap) = frame_gap_end(haystack, pos) else {
                matched = false;
                break;
            };
            pos = gap;
            if haystack[pos..].starts_with(word) {
                pos += word.len();
            } else {
                matched = false;
                break;
            }
        }
        if matched {
            return Some(pos);
        }
        cursor += at + 1;
    }
    None
}

/// Where `needle` ends in `haystack` at or after `from`, as exact bytes.
fn exact_end(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at + needle.len())
}

/// Where the terminal frame's gap starting at `pos` ends: one or more
/// spaces, cursor moves and SGR sequences, and nothing else.
fn frame_gap_end(haystack: &[u8], mut pos: usize) -> Option<usize> {
    let start = pos;
    loop {
        if haystack.get(pos) == Some(&b' ') {
            pos += 1;
        } else if let Some(end) = cursor_move_end(haystack, pos).or_else(|| sgr_end(haystack, pos))
        {
            pos = end;
        } else {
            break;
        }
    }
    (pos > start).then_some(pos)
}

/// Where the cursor move at `pos` ends: `\x1b[<row>;<col>H`.
fn cursor_move_end(haystack: &[u8], pos: usize) -> Option<usize> {
    if haystack.get(pos) != Some(&0x1b) || haystack.get(pos + 1) != Some(&b'[') {
        return None;
    }
    let mut end = pos + 2;
    let row = end;
    while haystack.get(end).is_some_and(|byte| byte.is_ascii_digit()) {
        end += 1;
    }
    if end == row || haystack.get(end) != Some(&b';') {
        return None;
    }
    end += 1;
    let col = end;
    while haystack.get(end).is_some_and(|byte| byte.is_ascii_digit()) {
        end += 1;
    }
    if end == col || haystack.get(end) != Some(&b'H') {
        return None;
    }
    Some(end + 1)
}

/// Where the SGR sequence at `pos` ends: `\x1b[...m`.
fn sgr_end(haystack: &[u8], pos: usize) -> Option<usize> {
    if haystack.get(pos) != Some(&0x1b) || haystack.get(pos + 1) != Some(&b'[') {
        return None;
    }
    let mut end = pos + 2;
    while haystack
        .get(end)
        .is_some_and(|byte| byte.is_ascii_digit() || *byte == b';')
    {
        end += 1;
    }
    (haystack.get(end) == Some(&b'm')).then_some(end + 1)
}

#[test]
fn phrase_end_matches_a_single_word_exactly() {
    assert_eq!(phrase_end(b"completed ok", 0, "completed"), Some(9));
}

#[test]
fn phrase_end_matches_one_space_gap() {
    assert_eq!(phrase_end(b"fiber resume", 0, "fiber resume"), Some(12));
}

#[test]
fn phrase_end_matches_a_cursor_move_gap() {
    assert_eq!(
        phrase_end(b"fiber\x1b[10;1Hresume", 0, "fiber resume"),
        Some(18)
    );
}

#[test]
fn phrase_end_matches_an_sgr_gap() {
    assert_eq!(
        phrase_end(b"fiber\x1b[0mresume", 0, "fiber resume"),
        Some(15)
    );
}

#[test]
fn phrase_end_rejects_a_letter_gap() {
    assert_eq!(phrase_end(b"fiberXresume", 0, "fiber resume"), None);
}

#[test]
fn phrase_end_rejects_an_erase_display_gap() {
    assert_eq!(phrase_end(b"fiber\x1b[2Jresume", 0, "fiber resume"), None);
}

#[test]
fn phrase_end_ignores_a_phrase_starting_before_from() {
    assert_eq!(phrase_end(b"fiber resume", 1, "fiber resume"), None);
}

#[test]
fn phrase_end_rejects_an_empty_gap() {
    assert_eq!(phrase_end(b"fiberresume", 0, "fiber resume"), None);
}

/// "Hello." in two deltas.
fn hello() -> Response {
    let events = [
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

/// A reply saying `text` in two deltas.
fn reply(text: &str) -> Response {
    let at = text.len() / 2;
    let (first, rest) = text.split_at(at);
    let events = [
        json!({"type": "response.output_text.delta", "delta": first}),
        json!({"type": "response.output_text.delta", "delta": rest}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": text}]
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

/// A reply that stalls mid-body: the turn stays running until cancelled.
fn stalled() -> Response {
    let prefix = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Working\"}\n\n";
    Response::stall(200, prefix, prefix.len() + 100000).header("content-type", "text/event-stream")
}

#[test]
fn typing_a_prompt_sees_the_answer_and_cancels_a_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), stalled()]).unwrap();
    setup.provider(&server);
    let mut run = Run::terminal(&setup);
    // The first frame draws the input line before any byte is written to
    // the master: no detection reply goes in.
    run.read_until(">");
    // Enter goes out once the hub connects. Unchanged cells are never
    // rewritten, spaces included, so each wait matches one word.
    run.write(b"say hi\r");
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; the turn's close says it finished.
    run.read_until("Hel");
    run.read_until("completed");
    // The second prompt starts a stalled turn; Esc interrupts it.
    run.write(b"again\r");
    run.read_until("Working");
    // The elapsed count proves the working line draws: a count whose
    // width changes rewrites its cells whole, while unchanged cells are
    // never rewritten, so no other word of the line arrives whole.
    run.read_until("10s");
    // With kitty's flags pushed Esc arrives as `CSI 27 u`, never as a
    // lone byte.
    run.write(b"\x1b[27u");
    run.read_until("interrupted");
    run.write(b"\x03\x03\r");
    // The turn just ended, so its idle status may still be on its way: the
    // quit either exits at once or asks first (`docs/tui.md`, "Quit"). The
    // Enter goes out with the Ctrl+C bytes, so it is processed after them:
    // it leaves working sessions running, and when the terminal already
    // exited it is never read.
    // After the last frame the output holds the alternate-screen leave and
    // the cursor shown, then one resume line per live session ("On exit").
    run.read_until("\x1b[?25h");
    run.read_until("fiber resume");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// A finished turn sends an OSC 9 desktop notification where the
/// terminal supports one.
#[test]
fn a_finished_turn_sends_an_osc_9_notification() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let mut run = Run::terminal_with(&setup, &[("TERM_PROGRAM", "ghostty")]);
    run.read_until(">");
    run.write(b"say hi\r");
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; the turn's close says it finished.
    run.read_until("Hel");
    run.read_until("\x1b]9;Fiber: ");
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// A reply that calls the shell with `echo hi`.
fn calls_echo_hi() -> Response {
    let events = [
        json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": "fc_call_1", "call_id": "call_1", "name": "shell",
            "arguments": json!({"command": "echo hi"}).to_string()
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

#[test]
fn a_standing_ask_opens_the_approval_panel_and_allow_once_runs_the_call() {
    let setup = Setup::new();
    let server = ProviderServer::start([calls_echo_hi(), hello()]).unwrap();
    setup.provider(&server);
    // A standing ask for this exact command: with the terminal connected
    // the loop asks a person (`docs/permissions.md`, "Headless").
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let mut run = Run::terminal(&setup);
    run.read_until(">");
    run.write(b"run it\r");
    run.read_until("asked by a global rule: echo hi");
    run.read_until("allow once");
    // Enter on the first choice allows once; the call runs and the turn
    // finishes with the answer.
    run.write(b"\r");
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; the turn's close says it finished.
    run.read_until("Hel");
    run.read_until("completed");
    // As above: the turn just ended, so quitting either exits at once or
    // asks first. The Enter leaves the session running, and exiting prints
    // its resume line ("Quit", "On exit").
    run.write(b"\x03\x03\r");
    run.read_until("fiber resume");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    // The model got the call's output, not a denial.
    let requests = server.requests();
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let result = second["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(result["output"], "hi\nExit code 0.\n");
}

/// A script step that calls `ask_user` with four questions, then the shell.
fn ask_then_echo_hi_script() -> Value {
    let questions = json!([
        {"header": "Timeout", "question": "How long?",
         "options": [{"label": "1m"}, {"label": "5m"}]},
        {"header": "Scope", "question": "Which scope?",
         "options": [{"label": "a"}, {"label": "b"}]},
        {"header": "Name", "question": "What name?"},
        {"header": "Pick", "question": "Which one?",
         "options": [{"label": "x"}, {"label": "y"}]},
    ]);
    json!({"steps": [{"tool_calls": [
        {"name": "ask_user", "arguments": {"questions": questions}},
        {"name": "shell", "arguments": {"command": "echo hi"}},
    ]}]})
}

#[test]
fn an_ask_and_a_shell_waiting_on_approval_show_no_call_json() {
    let setup = Setup::new();
    // The built-in `scripted` provider answers from a script in the
    // workspace, named as an ordinary model (`docs/model-routing.md`,
    // "The scripted provider"): one step carries both tool calls.
    write(
        &setup.workspace().join("s.json"),
        &ask_then_echo_hi_script(),
    );
    write(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    // A standing project ask for this exact command: with the terminal
    // connected the loop asks a person (`docs/permissions.md`, "Headless").
    // The project's rules live in Fiber home at `projects/<key>/rules`
    // (`docs/state.md`, "Projects"), never in the workspace, so the harness
    // places one through the canonical project key, as `Setup::sessions` does.
    let key = log::project_key(&doors::project(&setup.workspace()));
    let rule = setup.home().join("projects").join(key).join("rules");
    fs::create_dir_all(rule.parent().unwrap()).unwrap();
    fs::write(
        rule,
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let mut run = Run::terminal(&setup);
    // A 160x48 pty, as the ticket's screen: the resize lands before the
    // first prompt, so every frame draws at the ticket's width.
    rustix::termios::tcsetwinsize(
        &run.main,
        rustix::termios::Winsize {
            ws_col: 160,
            ws_row: 48,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    support::kill_pid(setup.deadline, run.child.id(), "WINCH").unwrap();
    run.read_until(">");
    run.write(b"run it\r");
    // The collapsed group line counts the call's parsed form, before the
    // shell's approval panel opens below it.
    run.read_until("asked 4 questions");
    run.read_until("asked by a project rule: echo hi");
    run.read_until("allow once");
    let output = run.output();
    let text = String::from_utf8_lossy(&output);
    // The one-row rule cannot be read from this raw byte stream without a
    // terminal emulator, so this test does not assert it here: it is
    // asserted by the 160-column screen tests
    // `group_line_and_ledger_show_parsed_arguments_never_json` and
    // `ledger_rows_show_parsed_arguments_never_json` in
    // crates/tui/src/view_tests.rs and
    // `group_summary_lines_are_cut_to_one_row_at_the_width` in
    // crates/tui/src/turn_tests.rs.
    // The approval panel shows the shell's arguments for review, so the
    // shell's JSON is expected there; the `ask_user` call's JSON must
    // never draw: neither on the group line nor in its ledger row.
    assert!(!text.contains("{\"questions\""), "{text:?}");
    assert!(!text.contains("\"header\""), "{text:?}");
    assert!(text.contains("asked 4 questions"), "{text:?}");
    assert!(!text.contains("ask_user {"), "{text:?}");
    // No row of the conversation holds `{"` except the shell approval
    // panel's arguments (`{\"command\":\"echo hi\"}`): every occurrence
    // in the captured output is immediately followed by `command"`.
    let mut rest = text.as_ref();
    let mut calls = 0;
    while let Some(at) = rest.find("{\"") {
        calls += 1;
        let after = &rest[at + 2..];
        assert!(after.starts_with("command\""), "{text:?}");
        rest = &rest[at + 2..];
    }
    assert!(calls > 0, "{text:?}");
    // As above: quitting either exits at once or asks first.
    run.write(b"\x03\x03\r");
    run.read_until("fiber resume");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// An `ask_user` call stalled mid-arguments: the added event names the
/// call, then one arguments delta carries the first part of its JSON and
/// the body stalls, so the raw text stays on the group line.
fn streaming_ask_stalls() -> Response {
    let added = json!({"type": "response.output_item.added", "item": {
        "type": "function_call", "id": "fc_ask", "name": "ask_user"
    }});
    let delta = json!({"type": "response.function_call_arguments.delta",
        "item_id": "fc_ask", "delta": "{\"questions\""});
    let prefix: String = [added, delta]
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stall(200, prefix.clone(), prefix.len() + 100000)
        .header("content-type", "text/event-stream")
}

#[test]
fn a_streaming_ask_shows_its_raw_arguments_until_requested() {
    let setup = Setup::new();
    let server = ProviderServer::start([streaming_ask_stalls()]).unwrap();
    setup.provider(&server);
    let mut run = Run::terminal(&setup);
    // A 160x48 pty, as the ticket's screen: the resize lands before the
    // first prompt, so every frame draws at the ticket's width.
    rustix::termios::tcsetwinsize(
        &run.main,
        rustix::termios::Winsize {
            ws_col: 160,
            ws_row: 48,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    support::kill_pid(setup.deadline, run.child.id(), "WINCH").unwrap();
    run.read_until(">");
    run.write(b"run it\r");
    // The call is still streaming its arguments, so the group line shows
    // the raw text; the scripted test above shows it gone once requested.
    run.read_until("{\"questions\"");
    // The turn stalls mid-arguments: Esc interrupts it, as the stalled
    // turn test interrupts its stalled reply. With kitty's flags pushed
    // Esc arrives as `CSI 27 u`, never as a lone byte.
    run.write(b"\x1b[27u");
    run.read_until("interrupted");
    // As above: quitting either exits at once or asks first.
    run.write(b"\x03\x03\r");
    run.read_until("fiber resume");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn a_repository_offer_swaps_in_and_approve_lets_the_turn_run() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    // The workspace's repository declares one MCP server nobody approved.
    write(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"mcp": {"servers": {"db": {"command": "/bin/echo"}}}}),
    );
    let mut run = Run::terminal(&setup);
    run.read_until(">");
    run.write(b"say hi\r");
    // One word: the 60-column terminal wraps the TUI-files line, and
    // unchanged cells are never rewritten, so a phrase can arrive split.
    run.read_until("installed");
    // From skip, ← chooses approve; ↓ moves to Send, and Enter sends.
    run.write(b"\x1b[D");
    run.write(b"\x1b[B");
    run.write(b"\r");
    // The turn runs only once the offer resolves, so the answer shows the
    // reply was accepted and the session counted this terminal first.
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; the turn's close says it finished.
    run.read_until("Hel");
    run.read_until("completed");
    // As above: quitting either exits at once or asks first.
    run.write(b"\x03\x03\r");
    run.read_until("fiber resume");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn resize_redraws_the_input_line_on_the_new_last_row() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let mut run = Run::terminal(&setup);
    run.read_until(">");
    // Default-background cells stay unpainted, so no row is addressed
    // whole; the footer's last word proves the last row drew before
    // the resize.
    run.read_until("quit");
    let marked = run.output().len();
    rustix::termios::tcsetwinsize(
        &run.main,
        rustix::termios::Winsize {
            ws_col: 40,
            ws_row: 10,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    support::kill_pid(setup.deadline, run.child.id(), "WINCH").unwrap();
    // The next frame draws the input line on row 10; rows 11 and 12 are
    // never addressed again.
    run.read_until("\x1b[10;1H");
    let output = run.output();
    let fresh = &output[marked..];
    assert!(!contains(fresh, "\x1b[11;1H"));
    assert!(!contains(fresh, "\x1b[12;1H"));
    // The hub `fiber` started is up before the quit, so `wait` sees it
    // idle out rather than start after the home is gone.
    until_socket(setup.deadline, &run.hub_socket, true, "the hub to start");
    run.write(b"\x03\x03");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn without_a_tty_bare_fiber_names_ask() {
    let deadline = Deadline::start();
    assert_names_ask(deadline, Stdio::piped(), "standard input and output", &[]);
}

#[test]
fn a_tty_on_standard_input_alone_is_not_enough() {
    let deadline = Deadline::start();
    let terminal = Terminal::open();
    assert_names_ask(deadline, terminal.stdin(), "standard output", &[]);
}

#[test]
fn resume_and_continue_without_a_tty_name_ask() {
    let deadline = Deadline::start();
    for args in [&["resume", "s_1"][..], &["resume"][..], &["continue"][..]] {
        assert_names_ask(deadline, Stdio::piped(), "standard input and output", args);
        let terminal = Terminal::open();
        assert_names_ask(deadline, terminal.stdin(), "standard output", args);
    }
}

#[test]
fn resume_opens_the_session_a_prefix_names() {
    let setup = Setup::new();
    let server = ProviderServer::start([reply("Hello.")]).unwrap();
    setup.provider(&server);
    let asked = setup.fiber(&["ask", "say hi"]);
    assert_eq!(asked.status.code(), Some(0));
    let id = setup.only_session();
    let mut run = Run::terminal_args(&setup, &["resume", &id[..4]]);
    run.read_until(">");
    run.read_until("Hello.");
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn continue_opens_the_latest_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([reply("First."), reply("Second.")]).unwrap();
    setup.provider(&server);
    assert_eq!(setup.fiber(&["ask", "first"]).status.code(), Some(0));
    assert_eq!(setup.fiber(&["ask", "second"]).status.code(), Some(0));
    let mut run = Run::terminal_args(&setup, &["continue"]);
    run.read_until(">");
    run.read_until("Second.");
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn resume_without_an_id_opens_home_at_the_session_list() {
    let setup = Setup::new();
    let server = ProviderServer::start([reply("Hello.")]).unwrap();
    setup.provider(&server);
    let asked = setup.fiber(&["ask", "say hi"]);
    assert_eq!(asked.status.code(), Some(0));
    let mut run = Run::terminal_args(&setup, &["resume"]);
    run.read_until(">");
    // The exited session is listed by its first prompt.
    run.read_until("say hi");
    // The list is focused, so Enter opens the row: the reply shows.
    run.write(b"\r");
    run.read_until("Hello.");
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn continue_with_no_session_is_a_usage_error() {
    let setup = Setup::new();
    let mut run = Run::terminal_args(&setup, &["continue"]);
    run.read_until("No session in this project to continue");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn resume_with_an_unknown_prefix_fails_before_any_frame() {
    let setup = Setup::new();
    let mut run = Run::terminal_args(&setup, &["resume", "s_zzz"]);
    run.read_until("no session at");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(1));
}

/// Runs `fiber` with `args`, `stdin` and standard output and error
/// piped: with no tty on `missing`, it exits 2 naming `fiber ask`.
fn assert_names_ask(deadline: Deadline, stdin: Stdio, missing: &str, args: &[&str]) {
    let setup = Setup::within(deadline);
    let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
    command
        .args(args)
        .current_dir(setup.workspace())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.root.path())
        .env("FIBER_HOME", setup.home())
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match finished.recv_timeout(setup.deadline.left()) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(
            setup.deadline,
            group,
            &finished,
            &format!("`fiber` to exit, no tty on {missing}"),
        ),
    };
    assert!(
        fakes::group_empties(group, setup.deadline.left()),
        "waited until the deadline for the process group to empty"
    );
    std::mem::forget(guard);
    watchdog.stand_down(setup.deadline.cleanup());
    assert_eq!(output.status.code(), Some(2), "no tty on {missing}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "fiber: The terminal needs a tty; run `fiber ask \"<prompt>\"`. Run `fiber --help` for usage.\n"
    );
}

fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// A 1x1 PNG, 69 bytes: within every cap, as `session_command.rs` holds
/// it. The fake clipboard program prints these bytes; the session stores
/// them byte for byte.
const PIXEL: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Waits under the setup's deadline for exactly one `artifacts/i_*.png`
/// under the session directory, equal byte for byte to [`PIXEL`].
fn stored_pixel(setup: &Setup) {
    let sessions = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    loop {
        let mut found = Vec::new();
        if let Ok(entries) = fs::read_dir(&sessions) {
            for entry in entries.flatten() {
                if let Ok(files) = fs::read_dir(entry.path().join("artifacts")) {
                    found.extend(files.flatten().map(|file| file.path()).filter(|path| {
                        path.file_name()
                            .and_then(OsStr::to_str)
                            .is_some_and(|name| name.starts_with("i_") && name.ends_with(".png"))
                    }));
                }
            }
        }
        if found.len() == 1 && fs::read(&found[0]).unwrap_or_default() == PIXEL {
            return;
        }
        if setup.deadline.left().is_zero() {
            panic!("waited until the deadline for one stored pixel; found: {found:?}");
        }
        thread::yield_now();
    }
}

#[test]
fn ctrl_v_pastes_an_image_that_the_session_stores() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_images(&server);
    // A fake clipboard program first on the child's PATH, printing the
    // pixel in its real program's form.
    let bin = setup.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (name, body) = if cfg!(target_os = "macos") {
        let hex: String = PIXEL.iter().map(|byte| format!("{byte:02X}")).collect();
        (
            "osascript",
            format!("printf '\\302\\253data PNGf{hex}\\302\\273\\n'"),
        )
    } else {
        let octal: String = PIXEL.iter().map(|byte| format!("\\{byte:03o}")).collect();
        ("wl-paste", format!("printf '{octal}'"))
    };
    fakes::script(&bin, name, &body);
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let mut env: Vec<(&str, &str)> = vec![("PATH", path.to_str().unwrap())];
    if !cfg!(target_os = "macos") {
        env.push(("WAYLAND_DISPLAY", "fiber-test"));
    }
    let mut run = Run::terminal_with(&setup, &env);
    run.read_until(">");
    run.write(&[0x16]);
    run.read_until("[Image #1]");
    run.write(b"\r");
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; quitting needs the turn finished, which the close says.
    run.read_until("completed");
    stored_pixel(&setup);
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    run.read_until("fiber resume");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// The final screen rebuilt from the pty's bytes: one cell per column
/// and row.
struct Screen {
    cells: Vec<Vec<char>>,
}

impl Screen {
    /// Rebuilds the `cols` by `rows` screen from every pty byte: only
    /// what the fixture emits is tracked (CUP, mode sets, the alternate
    /// screen's entry and leave, OSC, and printable text); every other
    /// sequence is skipped.
    fn rebuild(output: &[u8], cols: u16, rows: u16) -> Self {
        let (cols, rows) = (cols as usize, rows as usize);
        let mut screen = Self {
            cells: vec![vec![' '; cols]; rows],
        };
        let (mut row, mut col) = (0usize, 0usize);
        let text = String::from_utf8_lossy(output);
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '\x1b' => match chars.next() {
                    Some('[') => {
                        let mut params = String::new();
                        let mut final_ = '\0';
                        for ch in chars.by_ref() {
                            if ('@'..='~').contains(&ch) {
                                final_ = ch;
                                break;
                            }
                            params.push(ch);
                        }
                        // A missing row or column addresses the first.
                        let number = |at: usize| {
                            params
                                .split(';')
                                .nth(at)
                                .and_then(|n| {
                                    n.trim_start_matches(['?', ' ']).parse::<usize>().ok()
                                })
                                .unwrap_or(0)
                        };
                        match final_ {
                            'H' => {
                                row = number(0).saturating_sub(1).min(rows.saturating_sub(1));
                                col = number(1).saturating_sub(1).min(cols.saturating_sub(1));
                            }
                            'h' | 'l' if params.contains("1049") => {
                                if final_ == 'h' {
                                    for dead in screen.cells.iter_mut() {
                                        dead.fill(' ');
                                    }
                                    (row, col) = (0, 0);
                                } else {
                                    // The quit's leave: what follows is the
                                    // resume lines, not screen.
                                    return screen;
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(']') => {
                        let osc = chars.by_ref();
                        while let Some(ch) = osc.next() {
                            if ch == '\x07' {
                                break;
                            }
                            if ch == '\x1b' && osc.next() == Some('\\') {
                                break;
                            }
                        }
                    }
                    Some(_) | None => {}
                },
                ch if ch.is_control() => {}
                _ => {
                    if row < rows && col < cols {
                        screen.cells[row][col] = ch;
                        col += 1;
                        if col >= cols {
                            col = 0;
                            row = row.saturating_add(1);
                        }
                    }
                }
            }
        }
        screen
    }

    /// The panel's text columns, one right-trimmed row per screen row:
    /// the card text at the panel's second column, three narrower than
    /// the panel (`panel.rs`).
    fn panel_rows(&self, panel_x: usize, text: usize) -> Vec<String> {
        self.cells
            .iter()
            .map(|row| {
                row[panel_x + 2..panel_x + 2 + text]
                    .iter()
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// Asserts the Session card's exact text rows in a `panel`-column
    /// panel at the screen's right: every row is at most the card's text
    /// width, a row too long ends in `…`, and a row that fits is whole
    /// (`docs/tui.md`, "The panel").
    fn assert_session_cut(&self, cols: u16, panel: u16, workspace: &str) {
        let (panel_x, text) = (cols as usize - panel as usize, panel as usize - 3);
        let rows = self.panel_rows(panel_x, text);
        // The Session card is the panel's only card: its text rows sit
        // between its top and bottom half-block edges, with no second
        // card after.
        let is_edge = |row: &str, edge: char| !row.is_empty() && row.chars().all(|ch| ch == edge);
        let top = rows
            .iter()
            .position(|row| is_edge(row, '▄'))
            .unwrap_or_else(|| panic!("no Session card top edge at {cols} columns"));
        let bottom = rows
            .iter()
            .skip(top + 1)
            .position(|row| is_edge(row, '▀'))
            .map(|at| at + top + 1)
            .unwrap_or_else(|| panic!("no Session card bottom edge at {cols} columns"));
        assert!(
            !rows[bottom + 1..].iter().any(|row| is_edge(row, '▄')),
            "a second card follows the Session card at {cols} columns"
        );
        let card: Vec<&str> = rows[top + 1..bottom]
            .iter()
            .filter(|row| !row.is_empty())
            .map(String::as_str)
            .collect();
        // The card's one-column right padding is the panel's last
        // column (text starts at the second column and is three
        // narrower than the panel): no text row may hold a glyph there.
        // Edge rows span the card, so only text rows count.
        for (at, cells) in self.cells.iter().enumerate() {
            if at <= top || at >= bottom {
                continue;
            }
            let text_range: String = cells[panel_x + 2..panel_x + 2 + text].iter().collect();
            let trimmed = text_range.trim_end();
            if trimmed.is_empty() || is_edge(trimmed, '▄') || is_edge(trimmed, '▀') {
                continue;
            }
            assert_eq!(
                cells[cols as usize - 1],
                ' ',
                "a card text row reaches the panel's last column at {cols} columns: {trimmed:?}"
            );
        }
        // The speed value tracks elapsed time, so only its shape is
        // pinned: at the floor its digits never reach the cut, so the
        // row is one fixed string; at the ceiling the whole row is
        // `output speed, last reply  N tokens/s`.
        let speed_at = card
            .iter()
            .position(|row| row.starts_with("output speed, last reply  "))
            .unwrap_or_else(|| panic!("no speed row at {cols} columns: {card:?}"));
        let speed = if panel == 30 {
            "output speed, last reply  …".to_owned()
        } else {
            let tail = &card[speed_at]["output speed, last reply  ".len()..];
            let digits = tail.chars().take_while(|ch| ch.is_ascii_digit()).count();
            assert!(
                digits > 0 && tail[digits..] == *" tokens/s",
                "the speed row is not whole at {cols} columns: {:?}",
                card[speed_at]
            );
            format!("output speed, last reply  {} tokens/s", &tail[..digits])
        };
        let mut expected = expected_card(workspace, text, panel);
        expected.insert(speed_at, speed);
        assert_eq!(
            card,
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "Session card rows at {cols} columns"
        );
    }
}

/// The Session card's exact rows at `text` columns, without the speed
/// row: the scripted turn's fixed usage reads `tokens in / out  10 /
/// 3` with 40% cache hits, the context sits at 0% with the handoff
/// marker at 70.0k, the cost is still unknown, and one turn ran
/// (`panel.rs`).
fn expected_card(workspace: &str, text: usize, panel: u16) -> Vec<String> {
    let mut rows = vec![
        format!(
            "directory  {}",
            cut_left_path(workspace, text - "directory  ".len())
        ),
        fit_row("model  fake/m · thinking high", text),
    ];
    // The new context rows: the 17-cell bar with its marker and size,
    // then the handoff rows. The trigger reads 70.0k.
    match panel {
        30 => rows.extend([
            "░░░░░░░░░░░░░░░░░│       13".to_owned(),
            "handoff at 70… 0% of window".to_owned(),
            fit_row("then a summary, fresh context", text),
        ]),
        60 => rows.extend([
            format!("{}│{}13", "░".repeat(17), " ".repeat(37)),
            format!("handoff at 70.0k{}0% of window", " ".repeat(29)),
            "then a summary, fresh context".to_owned(),
        ]),
        _ => panic!("unexpected panel width {panel}"),
    }
    rows.extend([
        fit_row("tokens in / out  10 / 3", text),
        fit_row("cache hits  40%", text),
        fit_row("cost billed  unknown", text),
        fit_row("turns  1", text),
    ]);
    rows
}

/// `panel.rs` `cut_left` over the ASCII workspace path: at most `max`
/// columns, cut from the left with a leading `…`.
fn cut_left_path(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_owned();
    }
    let kept: String = path
        .chars()
        .rev()
        .take(max - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("…{kept}")
}

/// `panel.rs` `fit_rows` for a single-span row: whole when it fits,
/// else cut to `text` columns with `…` last. Every glyph here is one
/// column wide.
fn fit_row(row: &str, text: usize) -> String {
    if row.chars().count() <= text {
        row.to_owned()
    } else {
        row.chars().take(text - 1).collect::<String>() + "…"
    }
}

#[test]
fn session_card_rows_are_cut_with_an_ellipsis() {
    // `docs/tui.md` "Layout": the panel is `tui.panel.width` percent
    // of the screen, kept from 30 to 60 columns. The terminal stays at
    // 160 by 48; 10.0% (16 columns) lands the panel on its 30-column
    // floor and 50.0% (80 columns) on its 60-column ceiling, beside a
    // conversation of at least 84 either way.
    for (panel, share) in [(30u16, 10.0), (60u16, 50.0)] {
        session_card_cut_with_an_ellipsis(panel, share);
    }
}

/// Drives one turn with the panel pinned to `share` percent of a
/// 160-by-48 screen and asserts the Session card's exact rows: a row
/// too long for the card is cut with `…`, a row that fits is whole.
fn session_card_cut_with_an_ellipsis(panel: u16, share: f64) {
    let setup = Setup::new();
    let server = ProviderServer::start([reply("Hello.")]).unwrap();
    // Thinking levels declared, the default in force, so the card shows
    // the `model fake/m thinking high` row; the reply's fixed usage
    // gives the card its spend, context and speed rows.
    setup.provider_with_panel(&server, share);
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let mut run = Run::terminal_full_sized(&setup, &[], &[], 160, 48);
    run.read_until(">");
    run.write(b"say hi\r");
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; the turn's close says it finished. The updated
    // status (with the turn's usage) can arrive before or after the
    // close line, so only wait when its bytes are not here.
    run.read_until("Hel");
    run.read_until("completed");
    if !contains(&run.output(), "cache hits") {
        run.read_until("cache hits");
    }
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    run.read_until("fiber resume");
    // The quit's bytes come after every panel byte, so the screen
    // rebuilt now holds each frame whole: no row is read mid-frame.
    Screen::rebuild(&run.output(), 160, 48).assert_session_cut(
        160,
        panel,
        workspace.to_str().unwrap(),
    );
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}
