//! Tests for the clipboard image reader: the command choice, the frame
//! decode, the size checks and the read under its deadline.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::clock::Clock;
use fakes::children;
use fakes::clock::FakeClock;
use fakes::{TempDir, Watchdog, group_empties, kill_pid, pids_exit};

use super::{Decode, Failed, Reader, apple_script_png, command, png_size, read, refused};

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// The fake-clock limit every read test allows.
const LIMIT: Duration = Duration::from_secs(10);

/// The environment holding `vars`.
fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let map: HashMap<String, OsString> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    move |name: &str| map.get(name).cloned()
}

/// `exists` holding the programs on `PATH`.
fn on_path(programs: &[&str]) -> impl Fn(&str) -> bool {
    let path: Vec<String> = programs.iter().map(|name| (*name).to_owned()).collect();
    move |name: &str| path.iter().any(|program| program == name)
}

fn osascript() -> Reader {
    Reader {
        argv: vec![
            "osascript".to_owned(),
            "-e".to_owned(),
            "the clipboard as «class PNGf»".to_owned(),
        ],
        decode: Decode::AppleScript,
    }
}

fn wl_paste() -> Reader {
    Reader {
        argv: vec![
            "wl-paste".to_owned(),
            "--type".to_owned(),
            "image/png".to_owned(),
        ],
        decode: Decode::Raw,
    }
}

fn xclip() -> Reader {
    Reader {
        argv: vec![
            "xclip".to_owned(),
            "-selection".to_owned(),
            "clipboard".to_owned(),
            "-t".to_owned(),
            "image/png".to_owned(),
            "-o".to_owned(),
        ],
        decode: Decode::Raw,
    }
}

#[test]
fn the_clipboard_command_follows_the_machine() {
    // C1: SSH_CONNECTION alone reads nothing.
    assert_eq!(
        command(
            false,
            env(&[("SSH_CONNECTION", "x"), ("DISPLAY", ":0")]),
            on_path(&["xclip"])
        ),
        None
    );
    // C2: SSH_TTY alone reads nothing.
    assert_eq!(
        command(
            false,
            env(&[("SSH_TTY", "/dev/ttys0"), ("DISPLAY", ":0")]),
            on_path(&["xclip"])
        ),
        None
    );
    // C3: macOS with osascript reads through it.
    assert_eq!(
        command(true, env(&[]), on_path(&["osascript"])),
        Some(osascript())
    );
    // C4: macOS without osascript reads nothing, even with X11 set.
    assert_eq!(
        command(
            true,
            env(&[("DISPLAY", ":0")]),
            on_path(&["xclip", "wl-paste"])
        ),
        None
    );
    // C5: off macOS, osascript alone on PATH is nothing.
    assert_eq!(
        command(false, env(&[]), on_path(&["osascript"])),
        None
    );
    // C6: Wayland reads through wl-paste; without it, through xclip; an
    // empty WAYLAND_DISPLAY is unset.
    assert_eq!(
        command(
            false,
            env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]),
            on_path(&["wl-paste", "xclip"])
        ),
        Some(wl_paste())
    );
    assert_eq!(
        command(
            false,
            env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]),
            on_path(&["xclip"])
        ),
        Some(xclip())
    );
    assert_eq!(
        command(
            false,
            env(&[("WAYLAND_DISPLAY", "")]),
            on_path(&["wl-paste", "xclip"])
        ),
        None
    );
    // C7: X11 reads through xclip; without either side, nothing.
    assert_eq!(
        command(false, env(&[("DISPLAY", ":0")]), on_path(&["xclip"])),
        Some(xclip())
    );
    assert_eq!(
        command(false, env(&[("DISPLAY", ":0")]), on_path(&["wl-paste"])),
        None
    );
    assert_eq!(
        command(false, env(&[]), on_path(&["xclip"])),
        None
    );
    // C8: every reader's arguments and decode, exactly.
    assert_eq!(osascript().decode, Decode::AppleScript);
    assert_eq!(wl_paste().decode, Decode::Raw);
    assert_eq!(xclip().decode, Decode::Raw);
}

/// A 1x1 PNG, 69 bytes, as `crates/main/tests/session_command.rs` holds
/// it: within every cap, so the image child stores it byte for byte.
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// `bytes` as a shell `printf` format: one octal escape per byte.
fn octal(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("\\{byte:03o}"))
        .collect::<Vec<_>>()
        .join("")
}

/// `bytes` as upper-case hex, as `osascript` prints them.
fn hex_upper(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join("")
}

/// `bytes` as lower-case hex.
fn hex_lower(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

/// `PIXEL` as an `osascript` frame: `«data PNGf<hex>»` and a line break,
/// with octal escapes for the non-ASCII brackets.
fn frame(hex: &str) -> String {
    format!("\\302\\253data PNGf{hex}\\302\\273\\n")
}

/// A PNG header holding `width` by `height`: the signature, a 13-byte
/// `IHDR` length and the chunk name, then the size big-endian.
fn header(width: u32, height: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend_from_slice(&13u32.to_be_bytes());
    out.extend_from_slice(b"IHDR");
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out
}

#[test]
fn apple_script_output_decodes_to_png_bytes() {
    let hex = hex_upper(PIXEL);
    let framed = format!("«data PNGf{hex}»\n").into_bytes();
    // D3: with and without the trailing line break, the same bytes.
    assert_eq!(apple_script_png(&framed), Some(PIXEL.to_vec()));
    assert_eq!(
        apple_script_png(&format!("«data PNGf{hex}»").into_bytes()),
        Some(PIXEL.to_vec())
    );
    // D1: no `«data PNGf` is none.
    assert_eq!(apple_script_png("data PNGf00»\n".as_bytes()), None);
    assert_eq!(apple_script_png(b""), None);
    // D2: no `»` is none.
    assert_eq!(
        apple_script_png(&format!("«data PNGf{hex}\n").into_bytes()),
        None
    );
    // D4: an odd digit count is none.
    assert_eq!(apple_script_png("«data PNGf0»\n".as_bytes()), None);
    // D5: a non-hex digit is none.
    assert_eq!(apple_script_png("«data PNGf0g»\n".as_bytes()), None);
    // D6: upper and lower case read as the same bytes.
    assert_eq!(
        apple_script_png(&format!("«data PNGf{}»\n", hex_lower(PIXEL)).into_bytes()),
        Some(PIXEL.to_vec())
    );
    // D7: `«data PNGf»` is empty, then no image.
    assert_eq!(apple_script_png("«data PNGf»\n".as_bytes()), Some(Vec::new()));
}

#[test]
fn png_header_gives_the_size() {
    // P1: a valid 1x1 PNG; exactly 24 bytes still read, 23 do not.
    assert_eq!(png_size(PIXEL), Some((1, 1)));
    assert_eq!(png_size(&PIXEL[..24]), Some((1, 1)));
    assert_eq!(png_size(&PIXEL[..23]), None);
    // P2: one wrong signature byte, and `IHDR` misspelt, are none.
    let mut bad = PIXEL.to_vec();
    bad[0] ^= 0x01;
    assert_eq!(png_size(&bad), None);
    let mut bad = PIXEL.to_vec();
    bad[12] = b'X';
    assert_eq!(png_size(&bad), None);
}

#[test]
fn failures_name_what_went_wrong() {
    assert_eq!(
        Failed::NoImage.notice(),
        "No image on the clipboard.".to_owned()
    );
    assert_eq!(
        Failed::TimedOut.notice(),
        "Reading the clipboard took longer than 10 seconds.".to_owned()
    );
    assert_eq!(
        Failed::TooLarge.notice(),
        "The image on the clipboard is over 256 MiB.".to_owned()
    );
    assert_eq!(
        Failed::Unreadable.notice(),
        "The image on the clipboard could not be read.".to_owned()
    );
    assert_eq!(
        Failed::Spawn("no such file".to_owned()).notice(),
        "Could not read the clipboard: no such file.".to_owned()
    );
    // P4: the pixel refusal in the session's own words, exactly.
    assert_eq!(
        Failed::TooManyPixels {
            width: 8000,
            height: 7000
        }
        .notice(),
        "The image on the clipboard cannot be read: \
         8000x7000 is 56000000 pixels; the limit is 50000000"
            .to_owned()
    );
}

#[test]
fn refused_never_names_one_or_none() {
    assert!(refused(0));
    assert!(refused(1));
    assert!(!refused(2));
}

/// A child test's directory and ready FIFO.
struct ChildTest {
    dir: TempDir,
    ready: children::Ready,
}

impl ChildTest {
    /// A directory with a ready FIFO the command writes its group id to.
    fn new() -> Self {
        let dir = TempDir::new("fiber-paste-image");
        let ready = children::Ready::new(dir.path());
        Self { dir, ready }
    }

    /// A reader running `sh -c` with `script`: the script's first line
    /// writes the group id to the ready FIFO.
    fn reader(&self, script: &str, decode: Decode) -> Reader {
        Reader {
            argv: vec![
                "sh".to_owned(),
                "-c".to_owned(),
                format!("echo $$ > '{}'\n{script}", self.ready.path().display()),
            ],
            decode,
        }
    }

    /// The ready FIFO's path, for a body that writes its own lines.
    fn ready_path(&self) -> &Path {
        self.ready.path()
    }
}

/// Runs `read` on its own thread: the test advances the fake clock while
/// it parks, and every wait on the answer is wall-clock bounded.
fn spawn_read(
    reader: Reader,
    clock: Arc<FakeClock>,
    limit: Duration,
    cap: usize,
) -> mpsc::Receiver<Result<Vec<u8>, Failed>> {
    let (tx, rx) = mpsc::channel();
    let clock: Arc<dyn Clock> = clock;
    std::thread::Builder::new()
        .name("paste-test-read".to_owned())
        .spawn(move || {
            tx.send(read(&reader, &clock, limit, cap)).unwrap();
        })
        .unwrap();
    rx
}

/// The read's answer within [`DEADLINE`], failing after it.
fn answered(rx: &mpsc::Receiver<Result<Vec<u8>, Failed>>, what: &str) -> Result<Vec<u8>, Failed> {
    rx.recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("waited {DEADLINE:?} for {what}"))
}

/// Starts the watchdog for the group the ready FIFO's first line names,
/// returning the watchdog and the group id.
fn watch(test: &ChildTest) -> (Watchdog, u32) {
    let ids = test.ready.wait(DEADLINE);
    let pgid = ids.first().copied().unwrap_or_else(|| panic!("no ready line"));
    (Watchdog::group(pgid), pgid)
}

/// The group emptied after the read, then the watchdog stands down.
fn reaped(watchdog: Watchdog, pgid: u32, what: &str) {
    assert!(group_empties(pgid, DEADLINE), "{what} left its group behind");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn stdout_past_the_cap_is_too_large_and_the_group_is_killed() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    // R1: 17 bytes past a cap of 16. The sleep forks before the bytes
    // print, so every group member exists before the over-cap signal can
    // fire; a signal cannot reach a member forked after it.
    let reader = test.reader("sleep 3600 & printf '12345678901234567'; wait", Decode::Raw);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 16);
    let (watchdog, pgid) = watch(&test);
    assert_eq!(answered(&rx, "the over-cap read"), Err(Failed::TooLarge));
    reaped(watchdog, pgid, "the over-cap read");
}

#[test]
fn stdout_of_exactly_the_cap_is_read() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    // R1b: exactly the cap passes the cap: the bytes fail the PNG check,
    // not the cap.
    let reader = test.reader("printf '1234567890123456'", Decode::Raw);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 16);
    let (watchdog, pgid) = watch(&test);
    assert_eq!(answered(&rx, "the exact-cap read"), Err(Failed::Unreadable));
    reaped(watchdog, pgid, "the exact-cap read");
}

#[test]
fn apple_script_at_its_frame_cap_is_read() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    // R1c: a frame for exactly 16 image bytes is 2 * 16 + 14 bytes of
    // standard output, which passes the frame cap and fails the PNG
    // check instead.
    let body = format!("printf '{}'", frame(&"0".repeat(32)));
    let reader = test.reader(&body, Decode::AppleScript);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 16);
    let (watchdog, pgid) = watch(&test);
    assert_eq!(
        answered(&rx, "the exact frame-cap read"),
        Err(Failed::Unreadable)
    );
    reaped(watchdog, pgid, "the exact frame-cap read");
}

#[test]
fn apple_script_past_its_frame_cap_is_too_large() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    // R1d: one stdout byte past the frame cap is too large.
    let body = format!("printf '{}'", frame(&"0".repeat(33)));
    let reader = test.reader(&body, Decode::AppleScript);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 16);
    let (watchdog, pgid) = watch(&test);
    assert_eq!(
        answered(&rx, "the over frame-cap read"),
        Err(Failed::TooLarge)
    );
    reaped(watchdog, pgid, "the over frame-cap read");
}

/// Advances the clock to just before the limit and proves the worker
/// parked again there; the answer channel is still empty.
fn waiting(rx: &mpsc::Receiver<Result<Vec<u8>, Failed>>, clock: &Arc<FakeClock>, end: std::time::Instant, what: &str) {
    assert!(
        clock.await_parked(end, DEADLINE),
        "{what}: the read never parked at its deadline"
    );
    let mark = clock.advance_marked(LIMIT.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(
        clock.await_parked_since(&mark, Some(end), DEADLINE),
        "{what}: the read never parked again before its deadline"
    );
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "{what}: the read answered before its deadline"
    );
}

#[test]
fn the_limit_kills_a_hung_command() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    let start = clock.now();
    let end = start + LIMIT;
    let reader = test.reader("sleep 3600", Decode::Raw);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
    let (watchdog, pgid) = watch(&test);
    // R2: a millisecond before the limit the read has not answered; a
    // millisecond later it has timed out.
    waiting(&rx, &clock, end, "the hung command");
    clock.advance(Duration::from_millis(1));
    assert_eq!(
        answered(&rx, "the hung command"),
        Err(Failed::TimedOut)
    );
    reaped(watchdog, pgid, "the hung command");
}

#[test]
fn a_failing_exit_is_no_image() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    // R3: exit 1 with no bytes.
    let reader = test.reader("exit 1", Decode::Raw);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
    let (watchdog, pgid) = watch(&test);
    assert_eq!(answered(&rx, "the failing exit"), Err(Failed::NoImage));
    reaped(watchdog, pgid, "the failing exit");
}

#[test]
fn an_empty_read_is_no_image() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    // R4: exit 0 with no bytes.
    let reader = test.reader("exit 0", Decode::Raw);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
    let (watchdog, pgid) = watch(&test);
    assert_eq!(answered(&rx, "the empty read"), Err(Failed::NoImage));
    reaped(watchdog, pgid, "the empty read");
}

#[test]
fn a_signalled_command_is_no_image() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    // R5: killed by a signal.
    let reader = test.reader("kill -KILL $$", Decode::Raw);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
    let (watchdog, pgid) = watch(&test);
    assert_eq!(
        answered(&rx, "the signalled command"),
        Err(Failed::NoImage)
    );
    reaped(watchdog, pgid, "the signalled command");
}

#[test]
fn a_missing_program_is_a_spawn_error() {
    // R6: nothing runs, so no watchdog and no clock.
    let clock = FakeClock::new();
    let reader = Reader {
        argv: vec!["fiber-test-no-such-program".to_owned()],
        decode: Decode::Raw,
    };
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
    assert!(matches!(
        answered(&rx, "the missing program"),
        Err(Failed::Spawn(_))
    ));
}

#[test]
fn raw_bytes_pass_through_and_apple_script_is_decoded() {
    // R7: both decodes carry the pixel through.
    for (script, decode) in [
        (format!("printf '{}'", octal(PIXEL)), Decode::Raw),
        (
            format!("printf '{}'", frame(&hex_upper(PIXEL))),
            Decode::AppleScript,
        ),
    ] {
        let test = ChildTest::new();
        let clock = FakeClock::new();
        let reader = test.reader(&script, decode);
        let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
        let (watchdog, pgid) = watch(&test);
        assert_eq!(answered(&rx, "the pixel read"), Ok(PIXEL.to_vec()));
        reaped(watchdog, pgid, "the pixel read");
    }
}

#[test]
fn a_command_that_closes_stdout_and_keeps_running_hits_the_limit() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    // R8: end-of-file alone never ends the read: the child keeps running
    // with stdout closed, so a bounded wait times out while it lives.
    let reader = test.reader("exec >&-; sleep 3600", Decode::Raw);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
    let (watchdog, pgid) = watch(&test);
    assert!(matches!(
        rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(kill_pid(pgid, "0").unwrap());
    clock.advance(LIMIT);
    assert_eq!(
        answered(&rx, "the closed-stdout command"),
        Err(Failed::TimedOut)
    );
    reaped(watchdog, pgid, "the closed-stdout command");
}

#[test]
fn a_descendant_that_escapes_the_group_cannot_hold_the_read() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    // R9: a process that leaves the group holds stdout open past the
    // limit, out of the group signal's reach by design.
    let body = children::escapes_group(test.ready_path());
    let reader = Reader {
        argv: vec!["sh".to_owned(), "-c".to_owned(), body],
        decode: Decode::Raw,
    };
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
    let (watchdog, pgid) = watch(&test);
    let escaped = test.ready.wait(DEADLINE);
    let escaped = escaped.first().copied().unwrap_or_else(|| panic!("no escaped pid"));
    let marker = Watchdog::matching(&test.dir.path().display().to_string());
    clock.advance(LIMIT);
    assert_eq!(
        answered(&rx, "the escaped descendant"),
        Err(Failed::TimedOut)
    );
    assert!(group_empties(pgid, DEADLINE));
    assert!(kill_pid(escaped, "0").unwrap());
    drop(marker);
    assert!(pids_exit(&[escaped], DEADLINE));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn an_exited_shell_whose_child_holds_stdout_waits_for_the_limit() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    let start = clock.now();
    let end = start + LIMIT;
    // R10: the shell's exit alone never ends the read: its child holds
    // stdout open, so the read waits for the limit.
    let body = format!(
        "sleep 3600 & echo $! >> '{}'; exit 0",
        test.ready_path().display()
    );
    let reader = test.reader(&body, Decode::Raw);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
    let (watchdog, pgid) = watch(&test);
    let _ = test.ready.wait(DEADLINE);
    waiting(&rx, &clock, end, "the exited shell");
    clock.advance(Duration::from_millis(1));
    assert_eq!(answered(&rx, "the exited shell"), Err(Failed::TimedOut));
    reaped(watchdog, pgid, "the exited shell");
}

#[test]
fn a_successful_read_kills_what_the_command_left_behind() {
    let test = ChildTest::new();
    let clock = FakeClock::new();
    // R11: the pixel reads whole, and the group signal ends the sleep
    // the command left behind, success included.
    let body = format!(
        "printf '{}'; sleep 3600 >/dev/null & echo $! >> '{}'; exit 0",
        octal(PIXEL),
        test.ready_path().display()
    );
    let reader = test.reader(&body, Decode::Raw);
    let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
    let (watchdog, pgid) = watch(&test);
    let left = test.ready.wait(DEADLINE);
    let sleep = left.first().copied().unwrap_or_else(|| panic!("no sleep pid"));
    assert_eq!(answered(&rx, "the successful read"), Ok(PIXEL.to_vec()));
    assert!(group_empties(pgid, DEADLINE));
    assert!(pids_exit(&[sleep], DEADLINE));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn the_pixel_limit_is_the_sessions() {
    // P3: exactly 50,000,000 pixels pass; one more, and u32::MAX squared,
    // are refused without overflow.
    for (width, height, want) in [
        (10_000u32, 5_000u32, Ok(header(10_000, 5_000))),
        (
            50_000_001u32,
            1u32,
            Err(Failed::TooManyPixels {
                width: 50_000_001,
                height: 1,
            }),
        ),
        (
            u32::MAX,
            u32::MAX,
            Err(Failed::TooManyPixels {
                width: u32::MAX,
                height: u32::MAX,
            }),
        ),
    ] {
        let test = ChildTest::new();
        let clock = FakeClock::new();
        let body = format!("printf '{}'", octal(&header(width, height)));
        let reader = test.reader(&body, Decode::Raw);
        let rx = spawn_read(reader, Arc::clone(&clock), LIMIT, 1024);
        let (watchdog, pgid) = watch(&test);
        assert_eq!(answered(&rx, "the pixel limit"), want);
        reaped(watchdog, pgid, "the pixel limit");
    }
}