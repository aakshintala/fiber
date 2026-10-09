//! Tests for pasting an image through the loop: Ctrl+V without a
//! clipboard, and a read that lands, draws and sends (`docs/tui.md`,
//! "The input box").

use std::io::BufRead;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use ratatui::backend::TestBackend;

use super::tests::{feed, hello, new_loop};
use crate::Input;
use crate::link::Line;
use crate::paste_image::{Decode, Reader};

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// The screen's text.
fn screen(lp: &super::Loop<TestBackend>) -> String {
    crate::view::text(lp.screen.backend().buffer())
}

#[test]
fn ctrl_v_without_a_clipboard_draws_the_notice() {
    // L1: no reader, so Ctrl+V shows why and leaves the draft.
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    assert!(lp.paste_reader.is_none());
    assert_eq!(feed(&mut lp, vec![Input::Bytes(vec![0x16])]), 0);
    let shown = screen(&lp);
    assert!(shown.contains("No clipboard to read"));
    assert!(shown.contains("on this machine."));
    assert_eq!(lp.app.input().expand(), "");
}

/// A 1x1 PNG, 69 bytes, as `crates/main/tests/session_command.rs` holds
/// it.
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// [`PIXEL`] as base64.
const PIXEL_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

/// `bytes` as a shell `printf` format: one octal escape per byte.
fn octal(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("\\{byte:03o}"))
        .collect::<Vec<_>>()
        .join("")
}

/// A reader printing [`PIXEL`] raw: what a clipboard read would return.
fn pixel_reader() -> Reader {
    Reader {
        argv: vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("printf '{}'", octal(PIXEL)),
        ],
        decode: Decode::Raw,
    }
}

#[test]
fn ctrl_v_then_enter_sends_the_image_to_the_hub() {
    // L2: the worker posts the read, stepping it draws "[Image #1]", and
    // Enter sends a prompt with the image part.
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.paste_reader = Some(pixel_reader());
    let (out, worker) = mpsc::channel();
    lp.files_out = Some(out);
    let (ours, theirs) = UnixStream::pair().unwrap();
    lp.hub = Some(ours);
    lp.app
        .attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    assert!(lp.app.on_line(Line::Hub(hello())).is_empty());
    let (_, step_rx) = mpsc::channel();
    assert_eq!(lp.step(Input::Bytes(vec![0x16]), &step_rx), None);
    let landed = worker.recv_timeout(DEADLINE).unwrap();
    let Input::Image { ticket, result } = landed else {
        panic!("the worker posted something else");
    };
    assert_eq!(ticket, 0);
    assert_eq!(result.unwrap(), PIXEL_BASE64);
    assert_eq!(
        lp.step(
            Input::Image {
                ticket,
                result: Ok(PIXEL_BASE64.to_owned())
            },
            &step_rx
        ),
        None
    );
    assert_eq!(lp.app.input().expand(), "[Image #1]");
    assert!(screen(&lp).contains("[Image #1]"));
    assert_eq!(lp.step(Input::Bytes(b"\r".to_vec()), &step_rx), None);
    theirs.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut read = String::new();
    std::io::BufReader::new(theirs)
        .read_line(&mut read)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the hub line: {err}"));
    let line: serde_json::Value = serde_json::from_str(&read).unwrap();
    assert_eq!(line["command"], "prompt");
    let args: contract::commands::ContentArgs =
        serde_json::from_value(line["args"].clone()).unwrap();
    assert_eq!(
        args.content,
        vec![contract::commands::SentPart::Image {
            data: PIXEL_BASE64.to_owned(),
            mime_type: "image/png".to_owned(),
        }]
    );
}

#[test]
fn a_second_press_after_a_synchronous_failure_starts_a_read() {
    // P1: a read that never starts lands its failure, so the gate clears:
    // the notice shows and the next press reads.
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    assert!(lp.paste_reader.is_none());
    let (_, step_rx) = mpsc::channel();
    assert_eq!(lp.step(Input::Bytes(vec![0x16]), &step_rx), None);
    assert!(screen(&lp).contains("No clipboard to read"));
    assert_eq!(lp.app.input().expand(), "");
    // With a reader, the next press starts a read instead of dying quiet.
    lp.paste_reader = Some(pixel_reader());
    let (out, worker) = mpsc::channel();
    lp.files_out = Some(out);
    assert_eq!(lp.step(Input::Bytes(vec![0x16]), &step_rx), None);
    let landed = worker.recv_timeout(DEADLINE).unwrap();
    let Input::Image { ticket, result } = landed else {
        panic!("the worker posted something else");
    };
    assert_eq!(ticket, 1);
    assert_eq!(result.unwrap(), PIXEL_BASE64);
    assert_eq!(
        lp.step(
            Input::Image {
                ticket,
                result: Ok(PIXEL_BASE64.to_owned())
            },
            &step_rx
        ),
        None
    );
    assert_eq!(lp.app.input().expand(), "[Image #1]");
}
