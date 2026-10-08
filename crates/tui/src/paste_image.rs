//! Reading an image from the system clipboard (`docs/tui.md`, "The input
//! box"): the command for this machine, and the read under a deadline on
//! the injected clock.
//!
//! The worker parks on the clock through a wake pipe: every clock move
//! writes a byte to the pipe, so the worker's poll wakes, the worker
//! re-reads the clock and parks again. An exit watcher sees the child's
//! exit without reaping it; the worker alone reaps, after SIGKILL to the
//! command's group and to the child when it has not exited.

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, Weak, mpsc};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::clock::{Clock, Wake};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};

use crate::Input;

/// How long a clipboard read may take, on the injected clock.
pub(crate) const READ_LIMIT: Duration = Duration::from_secs(10);

/// How many image bytes a paste may hold: 256 MiB.
pub(crate) const IMAGE_CAP: usize = 256 * 1024 * 1024;

/// How many pixels a pasted image may hold
/// (`docs/model-routing.md`, "Image limits").
pub(crate) const MAX_PIXELS: u64 = 50_000_000;

/// The bytes around `osascript`'s hex: `«data PNGf` is 11, `»` is 2, and
/// the line break is 1, each in UTF-8.
const FRAME: usize = 14;

/// The MIME type of every pasted image: each clipboard command returns PNG.
pub(crate) const MIME_TYPE: &str = "image/png";

/// What Ctrl+V shows where no clipboard reads.
pub(crate) const NO_CLIPBOARD: &str = "No clipboard to read on this machine.";

/// How the command's standard output reads: raw PNG bytes, or
/// `osascript`'s `«data PNGf<hex>»` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decode {
    /// `wl-paste` and `xclip` print the PNG byte for byte.
    Raw,
    /// `osascript` prints the bytes as hex inside a frame.
    AppleScript,
}

/// The clipboard command for this machine: its arguments and how its
/// output decodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reader {
    /// The program and its arguments.
    pub(crate) argv: Vec<String>,
    /// How standard output decodes.
    pub(crate) decode: Decode,
}

/// Why a clipboard read gave no image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failed {
    /// A non-zero exit, a signal, or no bytes.
    NoImage,
    /// Past the deadline.
    TimedOut,
    /// Past the byte cap.
    TooLarge,
    /// Output that does not decode, or is no PNG with an `IHDR` header.
    Unreadable,
    /// Past the pixel limit, with the size read from the PNG header.
    TooManyPixels {
        /// The PNG's width.
        width: u32,
        /// The PNG's height.
        height: u32,
    },
    /// The command or the watcher thread could not start.
    Spawn(String),
}

impl Failed {
    /// What Ctrl+V shows: the draft stays as it is.
    pub(crate) fn notice(&self) -> String {
        match self {
            Failed::NoImage => "No image on the clipboard.".to_owned(),
            Failed::TimedOut => "Reading the clipboard took longer than 10 seconds.".to_owned(),
            Failed::TooLarge => "The image on the clipboard is over 256 MiB.".to_owned(),
            Failed::Unreadable => "The image on the clipboard could not be read.".to_owned(),
            Failed::TooManyPixels { width, height } => {
                let pixels = u64::from(*width) * u64::from(*height);
                format!(
                    "The image on the clipboard cannot be read: \
                     {width}x{height} is {pixels} pixels; the limit is {MAX_PIXELS}"
                )
            }
            Failed::Spawn(error) => format!("Could not read the clipboard: {error}."),
        }
    }
}

/// Whether an environment variable counts as set: present and not empty.
fn is_set(env: &dyn Fn(&str) -> Option<OsString>, name: &str) -> bool {
    env(name).is_some_and(|value| !value.is_empty())
}

/// The clipboard command for this machine: over SSH there is none; on
/// macOS `osascript` reading `«class PNGf»` when it is on `PATH`, else
/// none; elsewhere `wl-paste --type image/png` under Wayland, else
/// `xclip -selection clipboard -t image/png -o`, each when its display
/// variable is set and its program is on `PATH`.
pub(crate) fn command(
    macos: bool,
    env: impl Fn(&str) -> Option<OsString>,
    exists: impl Fn(&str) -> bool,
) -> Option<Reader> {
    if crate::clipboard::over_ssh(&env) {
        return None;
    }
    if macos {
        return exists("osascript").then(|| Reader {
            argv: vec![
                "osascript".to_owned(),
                "-e".to_owned(),
                "the clipboard as «class PNGf»".to_owned(),
            ],
            decode: Decode::AppleScript,
        });
    }
    let env = &env;
    let is_set = |name: &str| is_set(env, name);
    if is_set("WAYLAND_DISPLAY") && exists("wl-paste") {
        return Some(Reader {
            argv: vec![
                "wl-paste".to_owned(),
                "--type".to_owned(),
                "image/png".to_owned(),
            ],
            decode: Decode::Raw,
        });
    }
    if is_set("DISPLAY") && exists("xclip") {
        return Some(Reader {
            argv: vec![
                "xclip".to_owned(),
                "-selection".to_owned(),
                "clipboard".to_owned(),
                "-t".to_owned(),
                "image/png".to_owned(),
                "-o".to_owned(),
            ],
            decode: Decode::Raw,
        });
    }
    None
}

/// Reads the clipboard with `reader`: blocking, on the worker thread only.
/// The read ends when standard output reaches end-of-file and the child
/// has exited, both under the one deadline, or at the deadline, or past
/// `cap` image bytes. Every read then ends the same way, success
/// included: SIGKILL to the command's process group, SIGKILL to the
/// child's pid when it has not exited, the read end dropped, the watcher's
/// word, then the reap, by this worker alone.
pub(crate) fn read(
    reader: &Reader,
    clock: &Arc<dyn Clock>,
    limit: Duration,
    cap: usize,
) -> Result<Vec<u8>, Failed> {
    let deadline = clock
        .now()
        .checked_add(limit)
        .unwrap_or_else(|| clock.now());
    let (program, args) = reader.argv.split_first().map_or_else(
        || (String::new(), [].as_slice()),
        |(program, args)| (program.clone(), args),
    );
    let mut child = match Command::new(&program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return Err(Failed::Spawn(error.to_string())),
    };
    run(&mut child, reader.decode, clock, deadline, cap)
}

/// No signal ever names an id of 1 or less: signalling group -1 reaches
/// every process its owner holds, and 0 this process's own group.
fn refused(id: u32) -> bool {
    id <= 1
}

/// `id` as a [`Pid`]: refused ids and ids past `i32` never reach a call.
fn pid_of(id: u32) -> Option<Pid> {
    if refused(id) {
        return None;
    }
    Pid::from_raw(i32::try_from(id).ok()?)
}

/// SIGKILL to the command's process group, so a member left behind dies.
/// A refused id sends nothing.
fn signal_group(id: u32) {
    let Some(pid) = pid_of(id) else {
        return;
    };
    match rustix::process::kill_process_group(pid, Signal::KILL) {
        Ok(_) | Err(_) => {}
    }
}

/// SIGKILL to the child itself. A refused id sends nothing.
fn signal_pid(id: u32) {
    let Some(pid) = pid_of(id) else {
        return;
    };
    match rustix::process::kill_process(pid, Signal::KILL) {
        Ok(_) | Err(_) => {}
    }
}

/// How the worker's wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// Standard output reached end-of-file and the child exited.
    Done,
    /// Past the deadline.
    TimedOut,
    /// Past the standard-output cap.
    TooLarge,
}

/// The wake pipe's writer: every clock move writes one byte to it, so the
/// worker's poll wakes. A write that fails, on a full pipe or once the
/// worker has gone, is ignored: a byte already waiting wakes as well.
struct PipeWake {
    write: Mutex<std::io::PipeWriter>,
}

impl Wake for PipeWake {
    fn wake(&self) {
        use std::os::unix::io::AsFd as _;
        let write = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        match rustix::io::write(write.as_fd(), b"x") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// Drains the wake pipe after each wake, until it would block: the pipe
/// holds at most its capacity, so the drain ends.
fn drain(read: &std::io::PipeReader) {
    use std::os::unix::io::AsFd as _;
    let mut buf = [0u8; 64];
    loop {
        match rustix::io::read(read.as_fd(), &mut buf) {
            Ok(0) => return,
            Ok(_) => {}
            Err(Errno::AGAIN) => return,
            Err(Errno::INTR) => {}
            Err(_) => return,
        }
    }
}

/// Reads standard output while it has bytes: end-of-file, or any read
/// error but an interrupted or empty read, ends the output. Stops past
/// `cap`, so the draft never holds more than the cap plus one byte.
fn read_stdout(stdout: &mut ChildStdout, out: &mut Vec<u8>, eof: &mut bool, cap: usize) {
    use std::io::Read as _;
    let mut buf = [0u8; 8192];
    while out.len() <= cap {
        match stdout.read(&mut buf) {
            Ok(0) => {
                *eof = true;
                return;
            }
            Ok(read) => {
                out.extend_from_slice(buf.get(..read).unwrap_or_default());
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(_) => {
                *eof = true;
                return;
            }
        }
    }
}

/// Blocks in `waitid` for the child `id`, seeing its exit without reaping
/// it. An interrupted wait retries; any other error counts as exited.
fn wait_for_exit(id: u32) {
    let Some(pid) = pid_of(id) else {
        return;
    };
    loop {
        match rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED.union(WaitIdOptions::NOWAIT),
        ) {
            Ok(_) => return,
            Err(Errno::INTR) => {}
            Err(_) => return,
        }
    }
}

/// Signals the command's group, then signals the child only if its exit
/// has not been observed. A watcher notification arriving during the group
/// signal is observed before deciding whether to signal the child.
fn signal_after_group(
    id: u32,
    exited: bool,
    exited_rx: &mpsc::Receiver<()>,
    signal_group: impl FnOnce(u32),
    signal_pid: impl FnOnce(u32),
) {
    signal_group(id);
    if exited || exited_rx.try_recv().is_ok() {
        return;
    }
    signal_pid(id);
}

/// Runs the read of `child` to `end`, then ends it the same way every
/// time: the signals, the read end dropped, the watcher's word, then the
/// reap, by this worker alone.
fn run(
    child: &mut Child,
    decode: Decode,
    clock: &Arc<dyn Clock>,
    deadline: Instant,
    cap: usize,
) -> Result<Vec<u8>, Failed> {
    let id = child.id();
    // The read stops at the cap plus one byte: past `cap` image bytes is
    // too large, and `osascript`'s hex in its frame doubles the count.
    let stdout_cap = match decode {
        Decode::Raw => cap,
        Decode::AppleScript => cap.saturating_mul(2).saturating_add(FRAME),
    };
    let mut stdout = child.stdout.take();
    if let Some(stdout) = stdout.as_mut() {
        use std::os::unix::io::AsFd as _;
        match rustix::io::ioctl_fionbio(stdout.as_fd(), true) {
            Ok(_) | Err(_) => {}
        }
    }
    let (wake_read, wake_write) = match std::io::pipe() {
        Ok((read, write)) => (read, write),
        Err(error) => {
            signal_group(id);
            signal_pid(id);
            let _ = stdout;
            match child.wait() {
                Ok(_) | Err(_) => {}
            }
            return Err(Failed::Spawn(error.to_string()));
        }
    };
    {
        use std::os::unix::io::AsFd as _;
        match rustix::io::ioctl_fionbio(wake_read.as_fd(), true) {
            Ok(_) | Err(_) => {}
        }
        match rustix::io::ioctl_fionbio(wake_write.as_fd(), true) {
            Ok(_) | Err(_) => {}
        }
    }
    let wake: Arc<dyn Wake> = Arc::new(PipeWake {
        write: Mutex::new(wake_write),
    });
    let weak: Weak<dyn Wake> = Arc::downgrade(&wake);
    clock.subscribe(weak);
    let watcher_wake = Arc::clone(&wake);
    let (exited_tx, exited_rx) = mpsc::channel();
    let watcher = std::thread::Builder::new()
        .name("tui-paste-watch".to_owned())
        .spawn(move || {
            wait_for_exit(id);
            match exited_tx.send(()) {
                Ok(_) | Err(_) => {}
            }
            watcher_wake.wake();
        });
    let watcher = match watcher {
        Ok(watcher) => watcher,
        Err(error) => {
            // No thread is left and the child is never left unreaped: the
            // signals go out at once, then the reap.
            signal_group(id);
            signal_pid(id);
            let _ = stdout;
            match child.wait() {
                Ok(_) | Err(_) => {}
            }
            return Err(Failed::Spawn(error.to_string()));
        }
    };
    let mut out = Vec::new();
    let mut eof = false;
    let mut exited = false;
    let end = loop {
        exited = exited || exited_rx.try_recv().is_ok();
        if eof && exited {
            break End::Done;
        }
        if clock.now() >= deadline {
            break End::TimedOut;
        }
        if out.len() > stdout_cap {
            break End::TooLarge;
        }
        clock.wait_until(Some(deadline), &mut |bound| {
            let timeout = bound.map(|left| Timespec::try_from(left).unwrap_or_default());
            if eof {
                // The output ended while the child runs: only the wake
                // pipe can still move the wait.
                let mut fds = [PollFd::new(&wake_read, PollFlags::IN)];
                match poll(&mut fds, timeout.as_ref()) {
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(_) => {}
                }
            } else if let Some(stdout) = stdout.as_mut() {
                let mut fds = [
                    PollFd::new(stdout, PollFlags::IN),
                    PollFd::new(&wake_read, PollFlags::IN),
                ];
                match poll(&mut fds, timeout.as_ref()) {
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(_) => {}
                }
            }
            drain(&wake_read);
            if let Some(stdout) = stdout.as_mut() {
                read_stdout(stdout, &mut out, &mut eof, stdout_cap);
            }
        });
    };
    signal_after_group(id, exited, &exited_rx, signal_group, signal_pid);
    let _ = stdout;
    match exited_rx.recv() {
        Ok(_) | Err(_) => {}
    }
    match watcher.join() {
        Ok(_) | Err(_) => {}
    }
    let status = child.wait().map(|status| status.success()).unwrap_or(false);
    finish(end, status, out, decode)
}

/// Maps the wait's end to the read's result. A non-zero exit, a signal, no
/// bytes, output that does not decode, or no PNG `IHDR` header is no
/// image; past the pixel limit names the size in the session's own words.
fn finish(end: End, exited_zero: bool, out: Vec<u8>, decode: Decode) -> Result<Vec<u8>, Failed> {
    match end {
        End::TimedOut => return Err(Failed::TimedOut),
        End::TooLarge => return Err(Failed::TooLarge),
        End::Done => {}
    }
    if !exited_zero {
        return Err(Failed::NoImage);
    }
    let bytes = match decode {
        Decode::Raw => out,
        Decode::AppleScript => match apple_script_png(&out) {
            Some(bytes) => bytes,
            None => return Err(Failed::Unreadable),
        },
    };
    if bytes.is_empty() {
        return Err(Failed::NoImage);
    }
    let Some((width, height)) = png_size(&bytes) else {
        return Err(Failed::Unreadable);
    };
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Failed::TooManyPixels { width, height });
    }
    Ok(bytes)
}

/// Starts the clipboard read for `ticket` on a worker thread of its own:
/// the read, the decode, the pixel check and the base64 encode all run
/// there, and the worker posts [`Input::Image`] with the base64 or the
/// notice. `Some` notice when nothing started: no reader, no channel, or
/// the thread could not spawn.
pub(crate) fn start(
    reader: Option<&Reader>,
    clock: &Arc<dyn Clock>,
    out: Option<&mpsc::Sender<Input>>,
    ticket: u64,
) -> Option<String> {
    let (Some(reader), Some(out)) = (reader, out) else {
        return Some(NO_CLIPBOARD.to_owned());
    };
    let (reader, clock, out) = (reader.clone(), Arc::clone(clock), out.clone());
    match std::thread::Builder::new()
        .name("tui-paste-read".to_owned())
        .spawn(move || {
            let result = match read(&reader, &clock, READ_LIMIT, IMAGE_CAP) {
                Ok(bytes) => Ok(STANDARD.encode(bytes)),
                Err(failed) => Err(failed.notice()),
            };
            match out.send(Input::Image { ticket, result }) {
                Ok(()) | Err(_) => {}
            }
        }) {
        Ok(_) => None,
        Err(error) => Some(Failed::Spawn(error.to_string()).notice()),
    }
}

/// `osascript`'s `«data PNGf<hex>»` and line break as PNG bytes: the hex of
/// either case. `None` without the frame, with an odd digit count, or with
/// a non-hex digit.
fn apple_script_png(stdout: &[u8]) -> Option<Vec<u8>> {
    const PREFIX: &[u8] = "«data PNGf".as_bytes();
    const SUFFIX: &[u8] = "»".as_bytes();
    let mut framed = stdout.strip_prefix(PREFIX)?;
    framed = framed.strip_suffix(b"\n").unwrap_or(framed);
    let hex = framed.strip_suffix(SUFFIX)?;
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks(2) {
        let (Some(high), Some(low)) = (pair.first(), pair.get(1)) else {
            return None;
        };
        out.push((hex_val(*high)? << 4) + hex_val(*low)?);
    }
    Some(out)
}

/// One hex digit's value, of either case; `None` for any other byte.
fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// A PNG's width and height, big-endian at bytes 16-19 and 20-23, behind
/// its signature and the `IHDR` chunk name. `None` below 24 bytes, without
/// the signature, or without `IHDR`.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    const IHDR: &[u8; 4] = b"IHDR";
    let head = png.get(..24)?;
    if head.get(..8) != Some(SIGNATURE.as_slice()) {
        return None;
    }
    if head.get(12..16) != Some(IHDR.as_slice()) {
        return None;
    }
    let width = u32::from_be_bytes(head.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(head.get(20..24)?.try_into().ok()?);
    Some((width, height))
}

#[cfg(test)]
#[path = "paste_image_tests.rs"]
mod tests;
