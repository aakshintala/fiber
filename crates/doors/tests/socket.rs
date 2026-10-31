//! Clients on a session socket: subscriptions, acknowledgements, the commands
//! served at once, and close (`docs/invocation.md`, "Driver commands").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use support::*;

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{
    CacheLifetime, CommandInfo, CommandResult, Empty, Event, ExtensionsLoaded, FiberExited,
    InputItem, LoadedExtension, NamedBy, Notice, PreambleBuilt, PreambleReason, QueuedMessage,
    SentTool, SessionNamed, SessionState, SessionStatus, SkillInfo, SkillSource, SteeringQueue,
    ToolInfo, ToolSource, ToolState, TurnStarted, UsageRecorded,
};
use contract::extension::Drive;
use contract::inbox::{Ack, Answer, Delivery, Message, Rejection};
use contract::jobs::{Foreground, Jobs, Opening, Stop};
use contract::provider::ToolDefinition;
use contract::shapes::{
    ContentPart, DeclaredEffects, Effect, Failure, Origin, Process, Sender, Tokens, Usage,
};
use contract::tool::{
    Bound, Cancel, Effects, EffectsError, Output, SpillFile, Spilled, Tool, Unsaved,
};
use contract::{ActionId, CommandId, ErrorCode, GenerationId, SessionId};
use fakes::Client;
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::jobs::FakeJobs;
use log::Log;
use serde_json::{Map, Value};

const MALFORMED: &str = "A command is one JSON object per line, with a string `id` and `command`.";
const NOT_SUBSCRIBED: &str = "Send `subscribe` first.";
const ALREADY: &str = "This connection is already subscribed at this level.";
const UNFIT: &str = "The arguments do not fit this command.";
const PAST: &str = "`from_seq` is past the latest line.";
const REVERSED: &str = "`to_seq` is before `from_seq`.";
const ENDED: &str = "The session ended before answering.";

fn step() -> Event {
    Event::StepStarted(Empty {})
}

fn notice(message: &str) -> Event {
    Event::Notice(Notice {
        code: ErrorCode::IoFailed,
        message: message.to_owned(),
        extension: None,
    })
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 0,
        },
        cost: Some(0.0),
        subscription_cost: 0.0,
    }
}

fn status(name: &str) -> Event {
    Event::SessionStatus(SessionStatus {
        name: name.to_owned(),
        named: false,
        workspace: "/w".into(),
        parent: None,
        model: "m".into(),
        thinking: None,
        state: SessionState::Idle,
        since: 0,
        started: 0,
        calls: 0,
        turns: Some(0),
        step: None,
        git: None,
        context: None,
        spend: usage(),
        delegates: 0,
        jobs: 0,
        project: "-w".into(),
        clients: 0,
        closing: false,
        supervised: false,
    })
}

fn extensions() -> Event {
    Event::ExtensionsLoaded(ExtensionsLoaded {
        extensions: vec![LoadedExtension {
            name: "demo".into(),
            version: "1".into(),
            replaces: Vec::new(),
        }],
    })
}

fn exited() -> Event {
    Event::FiberExited(FiberExited {
        exit_code: 0,
        usage: usage(),
        final_message: None,
        error: None,
        suspended_on: None,
        questions: None,
    })
}

fn turn() -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text { text: "hi".into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
            changed_by: None,
        }],
    })
}

fn tool() -> ToolInfo {
    ToolInfo {
        name: "read".into(),
        source: ToolSource::Builtin,
        state: ToolState::Full,
        bytes: 12,
        tokens: None,
    }
}

fn text_of(message: &Message) -> String {
    match message.content.as_slice() {
        [ContentPart::Text { text }] => text.clone(),
        _ => panic!("a text part"),
    }
}

/// What the stand-in took, and it accepts the delivery.
#[track_caller]
fn take(inbox: &Receiver<Delivery>, wait: &Deadline) -> String {
    match wait.recv(inbox).expect("a delivery") {
        Delivery::Prompt(message, ack) => {
            let text = text_of(&message);
            ack.0(Ok(None));
            format!("prompt {text}")
        }
        Delivery::Steer(message, ack) => {
            let text = text_of(&message);
            ack.0(Ok(None));
            format!("steer {text}")
        }
        Delivery::SteerDrop(id, ack) => {
            ack.0(Ok(None));
            format!("drop {}", id.0)
        }
        Delivery::Message(message, ack) => {
            ack.0(Ok(None));
            format!(
                "message {} {}",
                message.from_session_id.0, message.command_id.0
            )
        }
        Delivery::Reply(reply, ack) => {
            ack.0(Ok(None));
            format!("reply {}", reply.request_id.0)
        }
        Delivery::Handoff(id, args, ack) => {
            ack.0(Ok(None));
            format!("handoff {} {:?}", id.0, args.instructions)
        }
        Delivery::Model(args, ack) => {
            ack.0(Ok(None));
            format!("model {}", args.model)
        }
        Delivery::McpReconnect(args, ack) => {
            ack.0(Ok(None));
            format!("reconnect {}", args.server)
        }
        Delivery::Reload(ack) => {
            ack.0(Ok(None));
            "reload".to_owned()
        }
        Delivery::Credential(args, ack) => {
            ack.0(Ok(None));
            format!("credential {}", args.label)
        }
        Delivery::Name(args, ack) => {
            ack.0(Ok(None));
            format!("name {:?}", args.text)
        }
        Delivery::AddDirectory(args, ack) => {
            ack.0(Ok(None));
            format!("add_directory {:?}", args.path)
        }
        Delivery::Close(ack) => {
            ack.0(Ok(None));
            "close".to_owned()
        }
        Delivery::Rewind(_, args, ack) => {
            ack.0(Ok(None));
            match args {
                contract::commands::RewindArgs::Rewind { seq, .. } => {
                    format!("rewind {seq:?}")
                }
                contract::commands::RewindArgs::AtEnd => "rewind at_end".to_owned(),
            }
        }
        Delivery::Cancelled | Delivery::Shell(..) => panic!("a wake arrives as a delivery"),
        Delivery::Job(_)
        | Delivery::JobLine(_)
        | Delivery::Interaction(_)
        | Delivery::Resolved(..)
        | Delivery::ExtensionExec(_)
        | Delivery::InnerCall(_)
        | Delivery::ExtensionLog(_)
        | Delivery::ProposeNotes(..)
        | Delivery::ExtensionJob(..)
        | Delivery::ExtensionUsage(_)
        | Delivery::DelegateUsage { .. } => {
            panic!("no job runs here")
        }
        Delivery::ChangeWorkspace(..) => {
            panic!("no job runs here")
        }
    }
}

#[test]
fn a_bad_line_is_malformed_and_only_a_string_id_is_echoed() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            let cases = [
                ("not json", None),
                ("[]", None),
                (r#"{"command":"tools"}"#, None),
                (r#"{"id":1,"command":"tools"}"#, None),
                (r#"{"id":"c_1","command":1}"#, Some("c_1")),
                (r#"{"id":"c_2","command":"tools","extra":1}"#, Some("c_2")),
                (r#"{"id":"c_3","command":"tools","args":[]}"#, Some("c_3")),
                (r#"{"id":"c_4","command":"tools","args":null}"#, Some("c_4")),
            ];
            for (line, id) in cases {
                send(&client, line);
                let answer = next(&client);
                assert_eq!(rejection(&answer), ("malformed", MALFORMED), "{line}");
                assert_eq!(command_id(&answer), id, "{line}");
            }

            let mut raw = UnixStream::connect(&socket).unwrap();
            raw.write_all(b"\xff\n").unwrap();
            raw.flush().unwrap();
            raw.set_read_timeout(Some(DEADLINE)).unwrap();
            let mut buf = Vec::new();
            BufReader::new(raw)
                .read_until(b'\n', &mut buf)
                .expect("a malformed line is answered");
            buf.pop();
            let answer: Value = serde_json::from_slice(&buf).unwrap();
            assert_eq!(rejection(&answer), ("malformed", MALFORMED));
            assert_eq!(command_id(&answer), None);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn a_command_without_a_trailing_newline_is_answered() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let mut raw = UnixStream::connect(&socket).unwrap();
            raw.write_all(br#"{"id":"c_nl","command":"tools"}"#)
                .unwrap();
            raw.shutdown(std::net::Shutdown::Write).unwrap();
            raw.set_read_timeout(Some(DEADLINE)).unwrap();
            let mut line = String::new();
            BufReader::new(&raw)
                .read_line(&mut line)
                .expect("a command without a trailing newline is answered");
            let value: Value = serde_json::from_str(line.trim_end()).unwrap();
            assert_eq!(command_id(&value), Some("c_nl"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn subscribe_is_first_and_unknown_or_unfit_commands_are_rejected() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    let home = opened.home.clone();
    let mut big = String::new();
    for i in 0..200 {
        big.push_str(&format!("line {i:03} with some text\n"));
    }
    opened
        .log
        .write_artifact("a_200.txt", big.as_bytes())
        .unwrap();
    let mut cut = vec![b'a'; 65535];
    cut.extend_from_slice("é tail\n".as_bytes());
    opened.log.write_artifact("a_cut.txt", &cut).unwrap();
    opened
        .log
        .write_artifact("a_uni.txt", "héllo\n".as_bytes())
        .unwrap();
    fs::create_dir_all(dir.join("artifacts").join("sub")).unwrap();
    fs::write(home.join("outside.txt"), b"no session keeps this").unwrap();
    std::os::unix::fs::symlink(
        home.join("outside.txt"),
        dir.join("artifacts").join("evil.png"),
    )
    .unwrap();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            send(&client, r#"{"id":"c_early","command":"tools"}"#);
            let early = next(&client);
            assert_eq!(rejection(&early), ("not_subscribed", NOT_SUBSCRIBED));
            assert_eq!(command_id(&early), Some("c_early"));

            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_again","command":"subscribe","args":{"level":"full"}}"#,
            );
            let again = response(&client, "c_again");
            assert_eq!(rejection(&again), ("invalid_arguments", ALREADY));

            send(&client, r#"{"id":"c_nope","command":"frob"}"#);
            let unknown = response(&client, "c_nope");
            assert_eq!(
                rejection(&unknown),
                ("unknown_command", "`frob` is not built in this Fiber yet.")
            );

            send(
                &client,
                r#"{"id":"c_nope_args","command":"frob","args":{"x":1}}"#,
            );
            let unknown_args = response(&client, "c_nope_args");
            assert_eq!(
                rejection(&unknown_args),
                ("unknown_command", "`frob` is not built in this Fiber yet.")
            );

            send(&client, r#"{"id":"c_reload","command":"reload"}"#);
            let Delivery::Reload(ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("reload reached the loop inbox") else {
                panic!("reload arrives as Reload");
            };
            ack.0(Ok(None));
            let line = response(&client, "c_reload");
            assert_eq!(kind(&line), "command_accepted");
            assert!(line["payload"].get("result").is_none());

            send(
                &client,
                r#"{"id":"c_reload_args","command":"reload","args":{"x":1}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_reload_args")),
                ("invalid_arguments", UNFIT)
            );

            // `artifact_page` answers a 200-line file in four pages: the
            // texts concatenate to the file, each page carries its size.
            let mut offset = 0u64;
            let mut text = String::new();
            let mut pages = 0;
            loop {
                let id = format!("c_big{pages}");
                send(
                    &client,
                    &format!(
                        r#"{{"id":"{id}","command":"artifact_page","args":{{"path":"artifacts/a_200.txt","offset":{offset}}}}}"#
                    ),
                );
                let page = response(&client, &id);
                assert_eq!(kind(&page), "command_accepted", "{page}");
                assert_eq!(
                    page["payload"]["result"]["size"].as_u64().unwrap(),
                    u64::try_from(big.len()).unwrap()
                );
                text.push_str(page["payload"]["result"]["text"].as_str().unwrap());
                pages += 1;
                match page["payload"]["result"].get("next") {
                    Some(next) => offset = next.as_u64().unwrap(),
                    None => break,
                }
                assert!(pages < 10, "paged past the end of the file");
            }
            assert_eq!(pages, 4);
            assert_eq!(text, big);

            // A character cut by the byte cap ends the first page at its
            // lead byte; the second page starts with the character.
            send(&client, r#"{"id":"c_cut0","command":"artifact_page","args":{"path":"artifacts/a_cut.txt","offset":0}}"#);
            let cut0 = response(&client, "c_cut0");
            assert_eq!(kind(&cut0), "command_accepted", "{cut0}");
            assert_eq!(cut0["payload"]["result"]["next"].as_u64().unwrap(), 65535);
            assert_eq!(
                cut0["payload"]["result"]["text"].as_str().unwrap().len(),
                65535
            );
            send(&client, r#"{"id":"c_cut1","command":"artifact_page","args":{"path":"artifacts/a_cut.txt","offset":65535}}"#);
            let cut1 = response(&client, "c_cut1");
            assert_eq!(kind(&cut1), "command_accepted", "{cut1}");
            assert!(
                cut1["payload"]["result"]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with('é'),
            );

            // Outside `artifacts/` is `invalid_arguments`, by exact code
            // and message, whether lexically or through a link.
            for (id, path) in [
                ("c_out0", "artifacts/../events.jsonl"),
                ("c_out1", "events.jsonl"),
                ("c_out2", "/etc/passwd"),
                ("c_evil", "artifacts/evil.png"),
            ] {
                let message = format!(
                    "`{path}` is not a file under the session's `artifacts/`."
                );
                send(
                    &client,
                    &format!(
                        r#"{{"id":"{id}","command":"artifact_page","args":{{"path":"{path}"}}}}"#
                    ),
                );
                assert_eq!(
                    rejection(&response(&client, id)),
                    ("invalid_arguments", message.as_str()),
                    "{path}"
                );
            }
            send(&client, r#"{"id":"c_missing","command":"artifact_page","args":{"path":"artifacts/missing.txt"}}"#);
            assert_eq!(
                rejection(&response(&client, "c_missing")),
                ("not_found", "No file `artifacts/missing.txt` in this session.")
            );
            send(&client, r#"{"id":"c_dir","command":"artifact_page","args":{"path":"artifacts/sub"}}"#);
            assert_eq!(
                rejection(&response(&client, "c_dir")),
                ("invalid_arguments", "`artifacts/sub` is not a regular file.")
            );
            send(&client, r#"{"id":"c_past","command":"artifact_page","args":{"path":"artifacts/a_uni.txt","offset":8}}"#);
            assert_eq!(
                rejection(&response(&client, "c_past")),
                ("invalid_arguments", "`offset` is past the end of the file.")
            );
            send(&client, r#"{"id":"c_inside","command":"artifact_page","args":{"path":"artifacts/a_uni.txt","offset":2}}"#);
            assert_eq!(
                rejection(&response(&client, "c_inside")),
                ("invalid_arguments", "`offset` is inside a character.")
            );
            // An offset at the end is an accepted empty page.
            send(&client, r#"{"id":"c_end","command":"artifact_page","args":{"path":"artifacts/a_uni.txt","offset":7}}"#);
            let end = response(&client, "c_end");
            assert_eq!(kind(&end), "command_accepted", "{end}");
            assert_eq!(end["payload"]["result"]["text"], "");
            assert!(end["payload"]["result"].get("next").is_none());
            assert_eq!(end["payload"]["result"]["size"].as_u64().unwrap(), 7);
            // A missing `path` never reaches the command.
            send(&client, r#"{"id":"c_page_bad","command":"artifact_page"}"#);
            assert_eq!(
                rejection(&response(&client, "c_page_bad")),
                ("invalid_arguments", UNFIT)
            );

            send(&client, r#"{"id":"c_name","command":"name"}"#);
            assert_eq!(
                rejection(&response(&client, "c_name")),
                ("invalid_arguments", UNFIT)
            );

            send(&client, r#"{"id":"c_shell","command":"shell"}"#);
            assert_eq!(
                rejection(&response(&client, "c_shell")),
                ("invalid_arguments", UNFIT)
            );
            send(
                &client,
                r#"{"id":"c_bad","command":"prompt","args":{"content":"nope"}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_bad")),
                ("invalid_arguments", UNFIT)
            );
            send(
                &client,
                r#"{"id":"c_null","command":"history","args":{"from_seq":null}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_null")),
                ("invalid_arguments", UNFIT)
            );
            send(
                &client,
                r#"{"id":"c_img","command":"prompt","args":{"content":[{"type":"image","data":"YQ==","mime_type":"image/png"}]}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_img")),
                (
                    "invalid_arguments",
                    "Image 1 cannot be read: this Fiber processes no images yet."
                )
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn artifact_page_lines_concatenate_and_mark_cut_boundaries() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let mut big = String::new();
    for i in 0..200 {
        big.push_str(&format!("line {i:03} with some text\n"));
    }
    opened
        .log
        .write_artifact("a_lines.txt", big.as_bytes())
        .unwrap();
    let mut cut = vec![b'a'; 65535];
    cut.extend_from_slice("é tail\n".as_bytes());
    opened.log.write_artifact("a_lines_cut.txt", &cut).unwrap();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            // Bounded walk: every page stays within the documented
            // bounds, texts concatenate to the file and line starts
            // concatenate to the file's line starts.
            let mut offset = 0u64;
            let mut text = String::new();
            let mut starts: Vec<u64> = Vec::new();
            let mut pages = 0;
            let mut size: Option<u64> = None;
            loop {
                let id = format!("c_walk{pages}");
                send(
                    &client,
                    &format!(
                        r#"{{"id":"{id}","command":"artifact_page","args":{{"path":"artifacts/a_lines.txt","offset":{offset}}}}}"#
                    ),
                );
                let page = response(&client, &id);
                assert_eq!(kind(&page), "command_accepted", "{page}");
                let result = &page["payload"]["result"];
                let chunk = result["text"].as_str().unwrap();
                let lines: Vec<u64> = result["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap())
                    .collect();
                // `docs/invocation.md:462` promises at most 64 lines per page.
                assert!(lines.len() <= 64, "at most 64 lines a page: {page}");
                // `docs/invocation.md:462` promises at most 64 KiB per page.
                assert!(chunk.len() <= 65536, "at most 64 KiB a page: {page}");
                assert_eq!(lines.len(), chunk.split_inclusive('\n').count());
                let page_size = result["size"].as_u64().unwrap();
                match size {
                    None => size = Some(page_size),
                    Some(seen) => assert_eq!(seen, page_size, "size stays put: {page}"),
                }
                text.push_str(chunk);
                starts.extend(lines);
                pages += 1;
                match result.get("next") {
                    Some(next) => offset = next.as_u64().unwrap(),
                    None => break,
                }
                assert!(pages < 10, "paged past the end of the file");
            }
            // A 200-line file at `docs/invocation.md:462`'s 64-line bound needs more
            // than one page; any paging of it walks the same concatenation.
            assert!(pages >= 2, "a 200-line file pages: {pages}");
            assert_eq!(text, big);
            assert_eq!(size, Some(u64::try_from(big.len()).unwrap()));
            let mut expected = Vec::new();
            let mut at = 0u64;
            for line in big.split_inclusive('\n') {
                expected.push(at);
                at += u64::try_from(line.len()).unwrap();
            }
            assert_eq!(starts, expected);
            // Cut pages: the long line cuts within the byte bound and
            // continues whole on the next page, whose first start is
            // the boundary offset; the pages concatenate to the file.
            send(&client, r#"{"id":"c_cut0","command":"artifact_page","args":{"path":"artifacts/a_lines_cut.txt","offset":0}}"#);
            let cut0 = response(&client, "c_cut0");
            let cut0_text = cut0["payload"]["result"]["text"].as_str().unwrap();
            let cut0_lines: Vec<u64> = cut0["payload"]["result"]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap())
                .collect();
            assert_eq!(cut0_lines.len(), cut0_text.split_inclusive('\n').count());
            // `docs/invocation.md:462` promises at most 64 KiB per page.
            assert!(
                cut0_text.len() <= 65536,
                "the cut stays within 64 KiB: {}",
                cut0_text.len()
            );
            let edge = cut0["payload"]["result"]["next"]
                .as_u64()
                .expect("the long line continues on a next page");
            // `docs/invocation.md:462`: a longer line cuts at a character boundary
            // within 64 KiB and continues on the next page.
            assert!(
                edge <= 65536,
                "the boundary stays within 64 KiB: {edge}"
            );
            send(
                &client,
                &format!(
                    r#"{{"id":"c_cut1","command":"artifact_page","args":{{"path":"artifacts/a_lines_cut.txt","offset":{edge}}}}}"#
                ),
            );
            let cut1 = response(&client, "c_cut1");
            let cut1_text = cut1["payload"]["result"]["text"].as_str().unwrap();
            let cut1_lines = cut1["payload"]["result"]["lines"].as_array().unwrap();
            assert_eq!(
                cut1_lines[0].as_u64().unwrap(),
                edge,
                "the next page resumes at the cut"
            );
            assert!(
                cut1_text.starts_with('é'),
                "the cut kept the character whole: {cut1_text:?}"
            );
            assert!(
                cut1["payload"]["result"].get("next").is_none(),
                "the file ends on the second page: {cut1}"
            );
            let cut = String::from_utf8(cut).unwrap();
            assert_eq!(format!("{cut0_text}{cut1_text}"), cut);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn full_folds_the_log_then_live_lines_and_the_latest_status() {
    let opened = Opened::open(vec![]);
    opened.log.append(&step(), None, None).unwrap();
    opened.log.append(&step(), None, None).unwrap();
    opened.log.append(&status("alpha"), None, None).unwrap();
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            let folded = until(&client, |line| kind(line) == "session_status");
            let seq_at = folded
                .iter()
                .position(|line| line["seq"].as_u64() == Some(1))
                .expect("seq 1 is in the fold");
            let status_at = folded
                .iter()
                .position(|line| kind(line) == "session_status")
                .unwrap();
            assert!(seq_at < status_at, "status follows the fold");
            assert_eq!(folded[status_at]["payload"]["name"], "alpha");
            assert_eq!(seqs(&folded), vec![0, 1]);

            log.append(&step(), None, None).unwrap();
            let live = until(&client, |line| line["seq"].as_u64() == Some(2));
            assert_eq!(seqs(&live), vec![2]);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn two_full_clients_see_the_same_durable_lines() {
    let opened = Opened::open(vec![]);
    opened.log.append(&step(), None, None).unwrap();
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let first = Client::connect(&socket).unwrap();
            let second = Client::connect(&socket).unwrap();
            subscribe(&first, "c_a", "full");
            subscribe(&second, "c_b", "full");
            log.append(&step(), None, None).unwrap();
            log.append(&step(), None, None).unwrap();
            let a = until(&first, |line| line["seq"].as_u64() == Some(2));
            let b = until(&second, |line| line["seq"].as_u64() == Some(2));
            assert_eq!(seqs(&a), vec![0, 1, 2]);
            assert_eq!(seqs(&b), seqs(&a));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn summary_gets_the_latest_status_and_extensions_and_nothing_else() {
    let opened = Opened::open(vec![]);
    opened.log.append(&status("alpha"), None, None).unwrap();
    opened.log.append(&extensions(), None, None).unwrap();
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            let ack = subscribe(&client, "c_sub", "summary");
            let status_line = next(&client);
            let extensions_line = next(&client);
            assert_eq!(kind(&ack), "command_accepted");
            assert_eq!(kind(&status_line), "session_status");
            assert_eq!(status_line["payload"]["name"], "alpha");
            assert_eq!(kind(&extensions_line), "extensions_loaded");
            assert_eq!(extensions_line["payload"]["extensions"][0]["name"], "demo");

            log.append(&step(), None, None).unwrap();
            log.append(&notice("nope"), None, None).unwrap();
            log.append(&status("beta"), None, None).unwrap();
            let later = until(&client, |line| {
                kind(line) == "session_status" && line["payload"]["name"] == "beta"
            });
            let mut lines = vec![ack, status_line, extensions_line];
            lines.extend(later);
            assert!(lines.iter().all(|line| {
                matches!(
                    kind(line),
                    "command_accepted" | "session_status" | "extensions_loaded"
                )
            }));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn clients_counts_full_connections_on_attach_and_leave() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let summary = Client::connect(&socket).unwrap();
            subscribe(&summary, "c_sum", "summary");
            let full = Client::connect(&socket).unwrap();
            subscribe(&full, "c_full", "full");
            let attached = until(&full, |line| kind(line) == "clients");
            let counts: Vec<u64> = attached
                .iter()
                .filter(|line| kind(line) == "clients")
                .map(|line| line["payload"]["count"].as_u64().unwrap())
                .collect();
            assert_eq!(counts, vec![1], "a summary connection is not counted");

            let other = Client::connect(&socket).unwrap();
            subscribe(&other, "c_other", "full");
            let both = until(&full, |line| {
                kind(line) == "clients" && line["payload"]["count"] == 2
            });
            assert!(both.iter().any(|line| line["payload"]["count"] == 2));
            let other_counts: Vec<u64> = until(&other, |line| kind(line) == "clients")
                .iter()
                .filter(|line| kind(line) == "clients")
                .map(|line| line["payload"]["count"].as_u64().unwrap())
                .collect();
            assert_eq!(other_counts, vec![2]);

            drop(other);
            let left = until(&full, |line| {
                kind(line) == "clients" && line["payload"]["count"] == 1
            });
            assert!(left.iter().any(|line| line["payload"]["count"] == 1));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn tools_and_history_answer_while_the_inbox_is_unread() {
    let opened = Opened::open(vec![tool()]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"wait"}]}}"#,
            );
            send(&client, r#"{"id":"c_hist0","command":"history","args":{"from_seq":0}}"#);
            let empty = response(&client, "c_hist0");
            assert_eq!(rejection(&empty), ("invalid_arguments", PAST));
            assert_eq!(command_id(&empty), Some("c_hist0"));
            send(&client, r#"{"id":"c_empty","command":"history","args":{}}"#);
            let empty = response(&client, "c_empty");
            assert_eq!(kind(&empty), "command_accepted", "{empty}");
            assert_eq!(
                empty["payload"]["result"]["lines"].as_array().unwrap().len(),
                0
            );
            send(
                &client,
                r#"{"id":"c_empty_to","command":"history","args":{"to_seq":3}}"#,
            );
            let empty_to = response(&client, "c_empty_to");
            assert_eq!(kind(&empty_to), "command_accepted", "{empty_to}");
            assert_eq!(
                empty_to["payload"]["result"]["lines"]
                    .as_array()
                    .unwrap()
                    .len(),
                0
            );
            send(&client, r#"{"id":"c_bare","command":"history"}"#);
            let bare = response(&client, "c_bare");
            assert_eq!(kind(&bare), "command_accepted", "{bare}");
            assert_eq!(
                bare["payload"]["result"]["lines"].as_array().unwrap().len(),
                0
            );

            send(&client, r#"{"id":"c_tools","command":"tools"}"#);
            let tools = response(&client, "c_tools");
            assert_eq!(kind(&tools), "command_accepted");
            assert_eq!(tools["payload"]["result"]["tools"][0]["name"], "read");
            assert!(tools["payload"]["result"]["tools"][0].get("tokens").is_none());

            for _ in 0..300 {
                log.append(&step(), None, None).unwrap();
            }
            send(&client, r#"{"id":"c_page","command":"history","args":{"from_seq":0}}"#);
            let page = response(&client, "c_page");
            assert_eq!(kind(&page), "command_accepted");
            let lines = &page["payload"]["result"]["lines"];
            assert_eq!(lines.as_array().unwrap().len(), 256);
            assert_eq!(lines[0]["seq"], 0);
            assert_eq!(lines[255]["seq"], 255);
            let window = |id: &str, args: &str| -> Vec<u64> {
                send(
                    &client,
                    &format!(r#"{{"id":"{id}","command":"history","args":{args}}}"#),
                );
                let answer = response(&client, id);
                assert_eq!(kind(&answer), "command_accepted", "{answer}");
                answer["payload"]["result"]["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|line| line["seq"].as_u64().unwrap())
                    .collect()
            };
            assert_eq!(window("c_last", "{}"), (44..=299).collect::<Vec<u64>>());
            assert_eq!(
                window("c_last_over", r#"{"to_seq":5000}"#),
                (44..=299).collect::<Vec<u64>>()
            );
            assert_eq!(
                window("c_last_end", r#"{"to_seq":299}"#),
                (44..=299).collect::<Vec<u64>>()
            );
            assert_eq!(
                window("c_last_edge", r#"{"to_seq":256}"#),
                (1..=256).collect::<Vec<u64>>()
            );
            assert_eq!(
                window("c_last_cap", r#"{"to_seq":255}"#),
                (0..=255).collect::<Vec<u64>>()
            );
            assert_eq!(
                window("c_last_under", r#"{"to_seq":254}"#),
                (0..=254).collect::<Vec<u64>>()
            );
            assert_eq!(window("c_last_first", r#"{"to_seq":0}"#), vec![0]);

            send(
                &client,
                r#"{"id":"c_rev","command":"history","args":{"from_seq":5,"to_seq":1}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_rev")),
                ("invalid_arguments", REVERSED)
            );
            send(
                &client,
                r#"{"id":"c_latest","command":"history","args":{"from_seq":299}}"#,
            );
            let latest = response(&client, "c_latest");
            assert_eq!(kind(&latest), "command_accepted");
            let latest_seqs: Vec<u64> = latest["payload"]["result"]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line["seq"].as_u64().unwrap())
                .collect();
            assert_eq!(latest_seqs, vec![299]);
            send(
                &client,
                r#"{"id":"c_past","command":"history","args":{"from_seq":300}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_past")),
                ("invalid_arguments", PAST)
            );
            send(
                &client,
                r#"{"id":"c_range","command":"history","args":{"from_seq":2,"to_seq":4}}"#,
            );
            let range = response(&client, "c_range");
            assert_eq!(kind(&range), "command_accepted");
            let got: Vec<u64> = range["payload"]["result"]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line["seq"].as_u64().unwrap())
                .collect();
            assert_eq!(got, vec![2, 3, 4]);
            send(
                &client,
                r#"{"id":"c_one","command":"history","args":{"from_seq":4,"to_seq":4}}"#,
            );
            let one = response(&client, "c_one");
            let one: Vec<u64> = one["payload"]["result"]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line["seq"].as_u64().unwrap())
                .collect();
            assert_eq!(one, vec![4]);
            assert!(
                matches!(inbox.try_recv(), Ok(Delivery::Prompt(_, _))),
                "the prompt was waiting unread while tools and history answered"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn tools_give_tokens_from_the_first_request_after_each_preamble() {
    fn definition() -> Map<String, Value> {
        serde_json::from_value(serde_json::json!({"type": "object"})).unwrap()
    }

    fn preamble(reason: PreambleReason, system_prompt: &str) -> Event {
        Event::PreambleBuilt(PreambleBuilt {
            reason,
            model: "fake/m".into(),
            context_window: 200_000,
            trigger_at: None,
            budget: None,
            thinking: None,
            tool_choice: "auto".into(),
            cache_lifetime: CacheLifetime::FiveMinutes,
            credential: None,
            system_prompt: system_prompt.into(),
            tools: vec![SentTool {
                name: "read".into(),
                registered_by: "builtin".into(),
                deferred: false,
                definition: definition(),
            }],
            replaced: Vec::new(),
        })
    }

    fn usage(
        generation: &str,
        input: u64,
        cache_write: Vec<(&str, u64)>,
        input_bytes: u64,
        input_media: Option<bool>,
        extension: Option<&str>,
        origin: Option<SessionId>,
    ) -> Event {
        Event::UsageRecorded(UsageRecorded {
            generation_id: GenerationId(generation.into()),
            model: "fake/m".into(),
            tokens: Tokens {
                input,
                cache_read: 0,
                cache_write: cache_write
                    .into_iter()
                    .map(|(lifetime, written)| (lifetime.to_owned(), written))
                    .collect(),
                output: 3,
            },
            web_searches: None,
            cost: None,
            subscription: None,
            extension: extension.map(str::to_owned),
            origin_session_id: origin,
            reviewer: None,
            warming: None,
            input_bytes,
            input_media,
        })
    }

    fn started() -> Event {
        Event::AssistantMessageStarted(Empty {})
    }

    fn tools_of(answer: &Value) -> &Vec<Value> {
        answer["payload"]["result"]["tools"].as_array().unwrap()
    }

    let opened = Opened::open(vec![tool()]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");

            send(&client, r#"{"id":"c_t0","command":"tools"}"#);
            let t0 = response(&client, "c_t0");
            assert_eq!(kind(&t0), "command_accepted", "{t0}");
            assert_eq!(tools_of(&t0).len(), 1);
            assert_eq!(tools_of(&t0)[0]["bytes"], 12);
            assert!(tools_of(&t0)[0].get("tokens").is_none(), "{t0}");

            log.append(&preamble(PreambleReason::Start, "system-a"), None, None)
                .unwrap();
            send(&client, r#"{"id":"c_t1","command":"tools"}"#);
            let t1 = response(&client, "c_t1");
            assert_eq!(tools_of(&t1)[0]["bytes"], 12);
            assert!(tools_of(&t1)[0].get("tokens").is_none(), "{t1}");

            let a1 = Some(ActionId("a_1".into()));
            log.append(&started(), None, a1.clone()).unwrap();
            log.append(
                &usage("g_x", 10, vec![("5m", 1000)], 500, None, None, None),
                None,
                None,
            )
            .unwrap();
            log.append(
                &usage("g_x", 10, vec![("5m", 1000)], 500, None, Some("x"), None),
                None,
                a1.clone(),
            )
            .unwrap();
            log.append(
                &usage(
                    "g_x",
                    10,
                    vec![("5m", 1000)],
                    500,
                    None,
                    None,
                    Some(SessionId("s_other".into())),
                ),
                None,
                a1.clone(),
            )
            .unwrap();
            log.append(
                &usage("g_x", 10, vec![("5m", 1000)], 500, None, None, None),
                None,
                Some(ActionId("a_9".into())),
            )
            .unwrap();
            log.append(
                &usage("g_x", 10, vec![("5m", 5000)], 500, Some(true), None, None),
                None,
                a1.clone(),
            )
            .unwrap();
            send(&client, r#"{"id":"c_t2","command":"tools"}"#);
            let t2 = response(&client, "c_t2");
            assert_eq!(tools_of(&t2)[0]["bytes"], 12);
            assert!(tools_of(&t2)[0].get("tokens").is_none(), "{t2}");

            log.append(
                &usage("g_1", 10, vec![("5m", 90)], 500, None, None, None),
                None,
                a1.clone(),
            )
            .unwrap();
            send(&client, r#"{"id":"c_t3","command":"tools"}"#);
            let t3 = response(&client, "c_t3");
            let expected = 12 * 100 / 500;
            assert!(expected >= 2 && (12 * 100) % 500 != 0);
            assert_eq!(tools_of(&t3)[0]["bytes"], 12);
            assert_eq!(
                tools_of(&t3)[0]["tokens"].as_u64().unwrap(),
                expected,
                "{t3}"
            );

            log.append(
                &usage("g_1", 10, vec![("5m", 555)], 500, None, None, None),
                None,
                a1.clone(),
            )
            .unwrap();
            send(&client, r#"{"id":"c_t4","command":"tools"}"#);
            let t4 = response(&client, "c_t4");
            assert_eq!(tools_of(&t4)[0]["bytes"], 12);
            assert_eq!(
                tools_of(&t4)[0]["tokens"].as_u64().unwrap(),
                expected,
                "{t4}"
            );

            log.append(&preamble(PreambleReason::Reload, "system-b"), None, None)
                .unwrap();
            send(&client, r#"{"id":"c_t5","command":"tools"}"#);
            let t5 = response(&client, "c_t5");
            assert_eq!(tools_of(&t5)[0]["bytes"], 12);
            assert!(tools_of(&t5)[0].get("tokens").is_none(), "{t5}");

            log.append(
                &usage("g_1", 10, vec![("5m", 1000)], 500, None, None, None),
                None,
                a1.clone(),
            )
            .unwrap();
            send(&client, r#"{"id":"c_t6","command":"tools"}"#);
            let t6 = response(&client, "c_t6");
            assert_eq!(tools_of(&t6)[0]["bytes"], 12);
            assert!(tools_of(&t6)[0].get("tokens").is_none(), "{t6}");

            log.append(&started(), None, Some(ActionId("a_2".into())))
                .unwrap();
            log.append(
                &usage("g_2", 0, Vec::new(), 500, None, None, None),
                None,
                Some(ActionId("a_2".into())),
            )
            .unwrap();
            send(&client, r#"{"id":"c_t7","command":"tools"}"#);
            let t7 = response(&client, "c_t7");
            assert_eq!(tools_of(&t7)[0]["bytes"], 12);
            // A request that reported no input tokens gives no rate.
            assert!(tools_of(&t7)[0].get("tokens").is_none(), "{t7}");
            Ok(())
        })
        .unwrap();
    opened.close();
}

/// Overwrites line `index` (from 0) of the session's log in place with bytes
/// that do not parse, keeping its length.
fn corrupt(dir: &Path, index: usize) {
    use std::os::unix::fs::FileExt;
    let path = dir.join("events.jsonl");
    let whole = fs::read(&path).unwrap();
    let start: usize = whole
        .split_inclusive(|b| *b == b'\n')
        .take(index)
        .map(<[u8]>::len)
        .sum();
    let len = whole[start..].iter().position(|b| *b == b'\n').unwrap();
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all_at(&vec![b'x'; len], u64::try_from(start).unwrap())
        .unwrap();
}

fn history_chain(opened: &Opened) -> (PathBuf, PathBuf) {
    use serde_json::json;
    let root = opened.sessions.join("s_0000000000000000");
    let ancestor = opened.sessions.join("s_aaaaaaaaaaaaaaaa");
    let payload = |from: Value| {
        let mut payload = json!({"workspace":"/w", "variables":{
            "path":"/usr/bin", "names":[], "source":"inherited"}});
        if !from.is_null() {
            payload["forked_from"] = from;
        }
        payload
    };
    for (dir, id, from, count) in [
        (&root, "s_0000000000000000", Value::Null, 8),
        (
            &ancestor,
            "s_aaaaaaaaaaaaaaaa",
            json!({"session_id":"s_0000000000000000", "seq":3}),
            305,
        ),
    ] {
        fs::create_dir_all(dir).unwrap();
        let lines: String = (0..count)
            .map(|seq| {
                let (kind, body) = if seq == 0 {
                    ("session_started", payload(from.clone()))
                } else {
                    ("step_started", json!({}))
                };
                format!(
                    "{}\n",
                    json!({"kind":kind,"session_id":id,"seq":seq,
                "ts":0,"schema_version":1,"payload":body})
                )
            })
            .collect();
        fs::write(dir.join("events.jsonl"), lines).unwrap();
    }
    let event = Event::SessionStarted(
        serde_json::from_value(payload(
            json!({"session_id":"s_aaaaaaaaaaaaaaaa", "seq":299}),
        ))
        .unwrap(),
    );
    opened.log.append(&event, None, None).unwrap();
    (root, ancestor)
}

fn history_page(client: &Client, args: Value) -> Value {
    let line = serde_json::json!({"id":"c_page", "command":"history", "args":args});
    send(client, &line.to_string());
    response(client, "c_page")
}

#[test]
fn history_pages_ancestors_to_each_continuation_point() {
    use serde_json::json;
    let opened = Opened::open(vec![]);
    let (root, ancestor) = history_chain(&opened);
    for seq in 300..305 {
        corrupt(&ancestor, seq);
    }
    let socket = opened.socket.clone();
    let own = opened.id.0.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "summary");
            let a = "s_aaaaaaaaaaaaaaaa";
            let cases = [
                (json!({"session_id":a}), (44..=299).collect::<Vec<_>>()),
                (json!({"session_id":a,"to_seq":400}), (44..=299).collect()),
                (json!({"session_id":a,"to_seq":299}), (44..=299).collect()),
                (json!({"session_id":a,"to_seq":298}), (43..=298).collect()),
                (json!({"session_id":a,"to_seq":10}), (0..=10).collect()),
                (json!({"session_id":a,"from_seq":298}), vec![298, 299]),
                (json!({"session_id":a,"from_seq":299}), vec![299]),
                (
                    json!({"session_id":a,"from_seq":290,"to_seq":400}),
                    (290..=299).collect(),
                ),
                (json!({"session_id":a,"from_seq":5,"to_seq":5}), vec![5]),
                (json!({"session_id":a,"from_seq":5,"to_seq":6}), vec![5, 6]),
                (
                    json!({"session_id":"s_0000000000000000"}),
                    (0..=3).collect(),
                ),
                (json!({"session_id":own,"from_seq":0}), vec![0]),
            ];
            for (index, (args, expected)) in cases.into_iter().enumerate() {
                // Accepted command ids are unique; rejected ones may be retried.
                let id = format!("c_page_{index}");
                send(
                    &client,
                    &json!({"id":id,"command":"history","args":args}).to_string(),
                );
                let answer = response(&client, &id);
                assert_eq!(kind(&answer), "command_accepted", "{answer}");
                let lines = answer["payload"]["result"]["lines"].as_array().unwrap();
                assert_eq!(seqs(lines), expected, "{args}");
                assert!(
                    lines
                        .iter()
                        .all(|line| matches!(kind(line), "session_started" | "step_started"))
                );
            }
            for (args, message) in [
                (json!({"session_id":a,"from_seq":300}), PAST.to_owned()),
                (
                    json!({"session_id":a,"from_seq":5,"to_seq":3}),
                    REVERSED.to_owned(),
                ),
                (
                    json!({"session_id":"s_ffffffffffffffff"}),
                    "Session s_ffffffffffffffff is neither this session nor one it continues."
                        .to_owned(),
                ),
            ] {
                assert_eq!(
                    rejection(&history_page(&client, args)),
                    ("invalid_arguments", message.as_str())
                );
            }
            // Malformed ids are refused before even a broken chain is read.
            fs::remove_dir_all(&root).unwrap();
            for id in [
                "../x",
                "s_AAAAAAAAAAAAAAAA",
                "s_aaaaaaaaaaaaaaa",
                "s_aaaaaaaaaaaaaaaaa",
                "s_gggggggggggggggg",
                "s_aaaaaaaaaaaaaaa/",
                "s_aaaaaaaaaaaaaaa:",
                "s_aaaaaaaaaaaaaaa`",
            ] {
                let message = format!(
                    "{id} is not a session id: one is `s_` followed by 16 lowercase hex digits."
                );
                assert_eq!(
                    rejection(&history_page(&client, json!({"session_id":id}))),
                    ("invalid_arguments", message.as_str())
                );
            }
            // An explicit own id never reads the chain.
            send(
                &client,
                &json!({"id":"c_own", "command":"history","args":{"session_id":own,"from_seq":0}})
                    .to_string(),
            );
            assert_eq!(
                response(&client, "c_own")["payload"]["result"]["lines"][0]["seq"],
                0
            );
            assert_eq!(
                rejection(&history_page(&client, json!({"session_id":a}))),
                ("invalid_arguments", UNFIT)
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn history_checks_only_the_requested_segments_window_and_bound() {
    use serde_json::json;
    let opened = Opened::open(vec![]);
    let (_root, ancestor) = history_chain(&opened);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "summary");
            corrupt(&ancestor, 43);
            let last = json!({"id":"c_last","command":"history","args":{"session_id":"s_aaaaaaaaaaaaaaaa"}});
            send(&client, &last.to_string());
            let answer = response(&client, "c_last");
            assert_eq!(
                seqs(answer["payload"]["result"]["lines"].as_array().unwrap()),
                (44..=299).collect::<Vec<_>>()
            );
            let before = json!({"session_id":"s_aaaaaaaaaaaaaaaa","to_seq":298});
            assert_eq!(
                rejection(&history_page(&client, before)),
                ("invalid_arguments", UNFIT)
            );
            let bytes = fs::read_to_string(ancestor.join("events.jsonl")).unwrap();
            fs::write(
                ancestor.join("events.jsonl"),
                format!("{}\n", bytes.lines().next().unwrap()),
            )
            .unwrap();
            let truncated = json!({"session_id":"s_aaaaaaaaaaaaaaaa"});
            assert_eq!(
                rejection(&history_page(&client, truncated)),
                ("invalid_arguments", UNFIT)
            );
            let root = json!({"id":"c_root","command":"history","args":{"session_id":"s_0000000000000000"}});
            send(&client, &root.to_string());
            let answer = response(&client, "c_root");
            assert_eq!(
                seqs(answer["payload"]["result"]["lines"].as_array().unwrap()),
                vec![0, 1, 2, 3]
            );
            fs::remove_dir_all(&ancestor).unwrap();
            let missing = json!({"session_id":"s_aaaaaaaaaaaaaaaa"});
            assert_eq!(
                rejection(&history_page(&client, missing)),
                ("invalid_arguments", UNFIT)
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn history_reads_only_its_window_of_the_log() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            for _ in 0..8 {
                log.append(&step(), None, None).unwrap();
            }
            // Bad lines before and after the window.
            corrupt(&dir, 1);
            corrupt(&dir, 6);
            let client = Client::connect(&socket).unwrap();
            // A full subscribe would read the bad line in its first page.
            subscribe(&client, "c_sub", "summary");
            send(
                &client,
                r#"{"id":"c_after","command":"history","args":{"from_seq":2,"to_seq":5}}"#,
            );
            let after = response(&client, "c_after");
            assert_eq!(kind(&after), "command_accepted", "{after}");
            let got: Vec<u64> = after["payload"]["result"]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line["seq"].as_u64().unwrap())
                .collect();
            assert_eq!(got, vec![2, 3, 4, 5]);
            send(
                &client,
                r#"{"id":"c_over","command":"history","args":{"from_seq":0,"to_seq":4}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_over")),
                ("invalid_arguments", UNFIT)
            );
            // The checks before the read need no parse.
            send(
                &client,
                r#"{"id":"c_past","command":"history","args":{"from_seq":8}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_past")),
                ("invalid_arguments", PAST)
            );
            send(
                &client,
                r#"{"id":"c_rev","command":"history","args":{"from_seq":3,"to_seq":1}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_rev")),
                ("invalid_arguments", REVERSED)
            );
            // A window cut short by `to_seq` reads no further.
            send(
                &client,
                r#"{"id":"c_first","command":"history","args":{"from_seq":0,"to_seq":0}}"#,
            );
            let first = response(&client, "c_first");
            assert_eq!(kind(&first), "command_accepted", "{first}");
            assert_eq!(first["payload"]["result"]["lines"][0]["seq"], 0);
            assert_eq!(
                first["payload"]["result"]["lines"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn history_without_from_seq_reads_only_the_last_window() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            for _ in 0..300 {
                log.append(&step(), None, None).unwrap();
            }
            // The line just before the last 256.
            corrupt(&dir, 43);
            let client = Client::connect(&socket).unwrap();
            // A full subscribe would read the bad line in its first page.
            subscribe(&client, "c_sub", "summary");
            let last: Vec<u64> = (44..=299).collect();
            for (id, args) in [("c_last", "{}"), ("c_last_end", r#"{"to_seq":299}"#)] {
                send(
                    &client,
                    &format!(r#"{{"id":"{id}","command":"history","args":{args}}}"#),
                );
                let answer = response(&client, id);
                assert_eq!(kind(&answer), "command_accepted", "{answer}");
                let got: Vec<u64> = answer["payload"]["result"]["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|line| line["seq"].as_u64().unwrap())
                    .collect();
                assert_eq!(got, last, "{id}");
            }
            // The window ending one line earlier holds the bad line.
            send(
                &client,
                r#"{"id":"c_bad","command":"history","args":{"to_seq":298}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_bad")),
                ("invalid_arguments", UNFIT)
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn inbox_commands_are_answered_only_on_the_connection_that_sent_them() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let dir = opened.dir.clone();
    let out = opened.out.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let sender = Client::connect(&socket).unwrap();
            let other = Client::connect(&socket).unwrap();
            subscribe(&sender, "c_a", "full");
            subscribe(&other, "c_b", "full");
            send(
                &sender,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
            );
            send(
                &sender,
                r#"{"id":"c_steer","command":"steer","args":{"content":[{"type":"text","text":"more"}]}}"#,
            );
            send(
                &sender,
                r#"{"id":"c_drop","command":"steer_drop","args":{"command_id":"c_steer"}}"#,
            );
            send(
                &sender,
                r#"{"id":"c_reply","command":"reply","args":{"request_id":"r_1","confirmed":true}}"#,
            );
            send(
                &sender,
                r#"{"id":"c_handoff","command":"handoff","args":{"instructions":"focus on tests"}}"#,
            );
            send(&sender, r#"{"id":"c_bare","command":"handoff","args":{}}"#);
            send(&sender, r#"{"id":"c_noargs","command":"handoff"}"#);
            send(&sender, r#"{"id":"c_close","command":"close"}"#);
            let wait = Deadline::after(DEADLINE);
            // The drop is answered at once and never delivered: the reader
            // took its entry off the shared queue, so the drain has
            // nothing to reconcile.
            assert_eq!(
                (0..7).map(|_| take(&inbox, &wait)).collect::<Vec<_>>(),
                vec![
                    "prompt hi".to_owned(),
                    "steer more".to_owned(),
                    "reply r_1".to_owned(),
                    "handoff c_handoff Some(\"focus on tests\")".to_owned(),
                    "handoff c_bare None".to_owned(),
                    "handoff c_noargs None".to_owned(),
                    "close".to_owned(),
                ]
            );
            log.append(&step(), None, None).unwrap();
            let own = until(&sender, |line| line["seq"].as_u64() == Some(0));
            let mut ids: Vec<&str> = own.iter().filter_map(command_id).collect();
            // The at-once steer and drop answers race the answers the
            // stand-in takes, so the acknowledgement order is not pinned.
            ids.sort();
            assert_eq!(
                ids,
                vec![
                    "c_bare",
                    "c_close",
                    "c_drop",
                    "c_handoff",
                    "c_noargs",
                    "c_prompt",
                    "c_reply",
                    "c_steer"
                ]
            );
            assert!(own.iter().all(|line| kind(line) != "command_rejected"));
            let seen = until(&other, |line| line["seq"].as_u64() == Some(0));
            assert!(
                seen.iter().all(|line| command_id(line).is_none()),
                "the other connection sees the step and none of the acknowledgements"
            );
            let printed = String::from_utf8(out.buf.lock().unwrap().clone()).unwrap();
            assert!(!printed.contains("c_prompt"));
            let file = fs::read_to_string(dir.join("events.jsonl")).unwrap();
            assert!(file.contains("step_started"));
            assert!(!file.contains("command_accepted"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn model_arrives_as_delivery_with_its_args_and_its_rejection_stays_put() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let sender = Client::connect(&socket).unwrap();
            let other = Client::connect(&socket).unwrap();
            let mut own = vec![subscribe(&sender, "c_a", "full")];
            let mut other_stream = vec![subscribe(&other, "c_b", "full")];
            // Which connection attaches first is not fixed, so `sender` may or may
            // not have seen a count of 1 before `other` attached. Both streams
            // read on to the attach that counts both before anything is sent.
            let both = |line: &Value| kind(line) == "clients" && line["payload"]["count"] == 2;
            own.extend(until(&sender, both));
            other_stream.extend(until(&other, both));
            send(
                &sender,
                r#"{"id":"c_m1","command":"model","args":{"model":"fake/n","thinking":"high"}}"#,
            );
            match Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the model is delivered")
            {
                Delivery::Model(args, ack) => {
                    assert_eq!(args.model, "fake/n");
                    assert_eq!(args.thinking.as_deref(), Some("high"));
                    ack.0(Ok(None));
                }
                Delivery::Prompt(..)
                | Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Name(..)
                | Delivery::AddDirectory(..)
                | Delivery::Handoff(..)
                | Delivery::Credential(..)
                | Delivery::Reload(..)
                | Delivery::McpReconnect(..)
                | Delivery::Reply(..)
                | Delivery::Rewind(..)
                | Delivery::ChangeWorkspace(..)
                | Delivery::Close(_)
                | Delivery::Job(_)
                | Delivery::JobLine(_)
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)
                | Delivery::ExtensionExec(_)
                | Delivery::Message(..)
                | Delivery::InnerCall(_)
                | Delivery::ExtensionLog(_)
                | Delivery::ProposeNotes(..)
                | Delivery::ExtensionJob(..)
                | Delivery::ExtensionUsage(_)
                | Delivery::DelegateUsage { .. }
                | Delivery::Shell(..)
                | Delivery::Cancelled => panic!("the model arrives as a model"),
            }
            let accepted_lines = until(&sender, |line| command_id(line) == Some("c_m1"));
            assert_eq!(kind(accepted_lines.last().unwrap()), "command_accepted");
            own.extend(accepted_lines);
            send(
                &sender,
                r#"{"id":"c_m2","command":"model","args":{"model":"fake/nope"}}"#,
            );
            match Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the second model is delivered")
            {
                Delivery::Model(args, ack) => {
                    assert_eq!(args.model, "fake/nope");
                    ack.0(Err(Rejection {
                        code: ErrorCode::InvalidArguments,
                        message: "no such model".into(),
                    }));
                }
                Delivery::Prompt(..)
                | Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Name(..)
                | Delivery::AddDirectory(..)
                | Delivery::Handoff(..)
                | Delivery::Credential(..)
                | Delivery::Reload(..)
                | Delivery::McpReconnect(..)
                | Delivery::Reply(..)
                | Delivery::Rewind(..)
                | Delivery::ChangeWorkspace(..)
                | Delivery::Close(_)
                | Delivery::Job(_)
                | Delivery::JobLine(_)
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)
                | Delivery::ExtensionExec(_)
                | Delivery::Message(..)
                | Delivery::InnerCall(_)
                | Delivery::ExtensionLog(_)
                | Delivery::ProposeNotes(..)
                | Delivery::ExtensionJob(..)
                | Delivery::ExtensionUsage(_)
                | Delivery::DelegateUsage { .. }
                | Delivery::Shell(..)
                | Delivery::Cancelled => panic!("the model arrives as a model"),
            }
            let rejected_lines = until(&sender, |line| command_id(line) == Some("c_m2"));
            assert_eq!(
                rejection(rejected_lines.last().unwrap()),
                ("invalid_arguments", "no such model")
            );
            own.extend(rejected_lines);
            // The other connection sees neither acknowledgement: both stay
            // on the connection that sent them.
            send(&other, r#"{"id":"c_tools","command":"tools"}"#);
            let seen = until(&other, |line| command_id(line) == Some("c_tools"));
            assert!(
                seen.iter()
                    .all(|line| !matches!(command_id(line), Some("c_m1" | "c_m2"))),
                "the other connection sees no model acknowledgement"
            );
            assert_eq!(kind(seen.last().unwrap()), "command_accepted");
            other_stream.extend(seen);
            // A count of 1 is the first attach, seen by one or both connections
            // depending on the order they attached in; only the count of 2 is
            // certain to reach both.
            let settled = |lines: &[Value]| -> Vec<Value> {
                lines
                    .iter()
                    .filter(|line| !(kind(line) == "clients" && line["payload"]["count"] == 1))
                    .cloned()
                    .collect()
            };
            assert_eq!(
                kinds(&settled(&own)),
                [
                    "command_accepted",
                    "clients",
                    "command_accepted",
                    "command_rejected"
                ]
            );
            assert_eq!(
                kinds(&settled(&other_stream)),
                ["command_accepted", "clients", "command_accepted"]
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn credential_arrives_as_delivery_and_its_rejection_reaches_the_client() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let sender = Client::connect(&socket).unwrap();
            let mut own = vec![subscribe(&sender, "c_a", "full")];
            send(
                &sender,
                r#"{"id":"c_cred","command":"credential","args":{"label":"work"}}"#,
            );
            match Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the credential is delivered")
            {
                Delivery::Credential(args, ack) => {
                    assert_eq!(args.label, "work");
                    ack.0(Err(Rejection {
                        code: ErrorCode::CredentialMissing,
                        message: "no such label".into(),
                    }));
                }
                Delivery::Prompt(..)
                | Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Name(..)
                | Delivery::AddDirectory(..)
                | Delivery::Handoff(..)
                | Delivery::Model(..)
                | Delivery::Reload(..)
                | Delivery::McpReconnect(..)
                | Delivery::Reply(..)
                | Delivery::Rewind(..)
                | Delivery::ChangeWorkspace(..)
                | Delivery::Close(_)
                | Delivery::Job(_)
                | Delivery::JobLine(_)
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)
                | Delivery::ExtensionExec(_)
                | Delivery::Message(..)
                | Delivery::InnerCall(_)
                | Delivery::ExtensionLog(_)
                | Delivery::ProposeNotes(..)
                | Delivery::ExtensionJob(..)
                | Delivery::ExtensionUsage(_)
                | Delivery::DelegateUsage { .. }
                | Delivery::Shell(..)
                | Delivery::Cancelled => panic!("the credential arrives as a credential"),
            }
            let rejected = until(&sender, |line| command_id(line) == Some("c_cred"));
            assert_eq!(kind(rejected.last().unwrap()), "command_rejected");
            assert_eq!(
                rejection(rejected.last().unwrap()),
                ("credential_missing", "no such label")
            );
            own.extend(rejected);
            assert_eq!(
                kinds(&own),
                ["command_accepted", "clients", "command_rejected"]
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn name_is_delivered_with_its_text_exactly() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_n1","command":"name","args":{"text":"  Mine  "}}"#,
            );
            let wait = Deadline::after(DEADLINE);
            assert_eq!(take(&inbox, &wait), "name \"  Mine  \"");
            let accepted = response(&client, "c_n1");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_n1"));
            send(
                &client,
                r#"{"id":"c_n2","command":"name","args":{"text":""}}"#,
            );
            assert_eq!(take(&inbox, &wait), "name \"\"");
            let accepted = response(&client, "c_n2");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_n2"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn add_directory_is_delivered_with_its_path() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_a1","command":"add_directory","args":{"path":"shared"}}"#,
            );
            let wait = Deadline::after(DEADLINE);
            assert_eq!(take(&inbox, &wait), "add_directory \"shared\"");
            let accepted = response(&client, "c_a1");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_a1"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn rewind_arrives_as_delivery_and_its_answer_reaches_the_client() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let sender = Client::connect(&socket).unwrap();
            subscribe(&sender, "c_a", "full");
            send(
                &sender,
                r#"{"id":"c_r1","command":"rewind","args":{"seq":5}}"#,
            );
            match Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the rewind is delivered")
            {
                Delivery::Rewind(id, args, ack) => {
                    assert_eq!(id.0, "c_r1");
                    assert!(matches!(
                        args,
                        contract::commands::RewindArgs::Rewind {
                            seq: Some(contract::Seq(5)),
                            from_session_id: None,
                            summarise: false,
                        }
                    ));
                    ack.0(Ok(Some(CommandResult::Rewind {
                        new_session_id: SessionId("s_9e2b0000000000b2".into()),
                    })));
                }
                Delivery::Prompt(..)
                | Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Name(..)
                | Delivery::AddDirectory(..)
                | Delivery::Handoff(..)
                | Delivery::Model(..)
                | Delivery::Reload(..)
                | Delivery::McpReconnect(..)
                | Delivery::Credential(..)
                | Delivery::Reply(..)
                | Delivery::Close(_)
                | Delivery::Job(_)
                | Delivery::JobLine(_)
                | Delivery::ExtensionExec(_)
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)
                | Delivery::Message(..)
                | Delivery::InnerCall(_)
                | Delivery::ExtensionLog(_)
                | Delivery::ProposeNotes(..)
                | Delivery::ExtensionJob(..)
                | Delivery::ExtensionUsage(_)
                | Delivery::DelegateUsage { .. }
                | Delivery::Shell(..)
                | Delivery::ChangeWorkspace(..)
                | Delivery::Cancelled => panic!("the rewind arrives as a rewind"),
            }
            let accepted = until(&sender, |line| command_id(line) == Some("c_r1"));
            let last = accepted.last().unwrap();
            assert_eq!(kind(last), "command_accepted");
            assert_eq!(
                last["payload"]["result"]["new_session_id"],
                "s_9e2b0000000000b2"
            );
            // A rejection sends `command_rejected` and frees the id: it
            // can be used again.
            send(&sender, r#"{"id":"c_r2","command":"rewind"}"#);
            match Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the second rewind is delivered")
            {
                Delivery::Rewind(id, args, ack) => {
                    assert_eq!(id.0, "c_r2");
                    assert!(matches!(
                        args,
                        contract::commands::RewindArgs::Rewind {
                            seq: None,
                            from_session_id: None,
                            summarise: false,
                        }
                    ));
                    ack.0(Err(Rejection {
                        code: ErrorCode::Busy,
                        message: "a turn is running".into(),
                    }));
                }
                Delivery::Prompt(..)
                | Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Name(..)
                | Delivery::AddDirectory(..)
                | Delivery::Handoff(..)
                | Delivery::Model(..)
                | Delivery::Reload(..)
                | Delivery::McpReconnect(..)
                | Delivery::Credential(..)
                | Delivery::Reply(..)
                | Delivery::Close(_)
                | Delivery::Job(_)
                | Delivery::JobLine(_)
                | Delivery::ExtensionExec(_)
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)
                | Delivery::Message(..)
                | Delivery::InnerCall(_)
                | Delivery::ExtensionLog(_)
                | Delivery::ProposeNotes(..)
                | Delivery::ExtensionJob(..)
                | Delivery::ExtensionUsage(_)
                | Delivery::DelegateUsage { .. }
                | Delivery::Shell(..)
                | Delivery::ChangeWorkspace(..)
                | Delivery::Cancelled => panic!("the rewind arrives as a rewind"),
            }
            let rejected_lines = until(&sender, |line| command_id(line) == Some("c_r2"));
            assert_eq!(
                rejection(rejected_lines.last().unwrap()),
                ("busy", "a turn is running")
            );
            send(&sender, r#"{"id":"c_r2","command":"tools"}"#);
            let reused = until(&sender, |line| command_id(line) == Some("c_r2"));
            assert_eq!(kind(reused.last().unwrap()), "command_accepted");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn model_without_args_is_unfit_and_before_subscribe_is_not_subscribed() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            let mut stream = Vec::new();
            send(
                &client,
                r#"{"id":"c_early","command":"model","args":{"model":"fake/n"}}"#,
            );
            let early = next(&client);
            assert_eq!(rejection(&early), ("not_subscribed", NOT_SUBSCRIBED));
            stream.push(early);
            stream.push(subscribe(&client, "c_sub", "full"));
            send(&client, r#"{"id":"c_bare","command":"model"}"#);
            let bare_lines = until(&client, |line| command_id(line) == Some("c_bare"));
            assert_eq!(
                rejection(bare_lines.last().unwrap()),
                ("invalid_arguments", UNFIT)
            );
            stream.extend(bare_lines);
            send(&client, r#"{"id":"c_empty","command":"model","args":{}}"#);
            let empty_lines = until(&client, |line| command_id(line) == Some("c_empty"));
            assert_eq!(
                rejection(empty_lines.last().unwrap()),
                ("invalid_arguments", UNFIT)
            );
            stream.extend(empty_lines);
            assert_eq!(
                kinds(&stream),
                [
                    "command_rejected",
                    "command_accepted",
                    "clients",
                    "command_rejected",
                    "command_rejected"
                ]
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn an_acknowledgement_dropped_uncalled_answers_closing() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
            );
            let Delivery::Prompt(_, ack) = Deadline::after(DEADLINE).recv(&inbox)
                .expect("the prompt is delivered")
            else {
                panic!("the prompt is delivered");
            };
            drop(ack);
            let line = until(&client, |line| kind(line) == "command_rejected");
            let line = line.last().unwrap();
            assert_eq!(rejection(line), ("closing", ENDED));
            assert_eq!(command_id(line), Some("c_prompt"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_delivers_fiber_exited_to_a_client_that_is_still_reading() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            log.append(&exited(), None, None).unwrap();
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client subscribed");
    let lines = until(&client, |line| kind(line) == "fiber_exited");
    assert_eq!(kind(lines.last().unwrap()), "fiber_exited");
    assert_eq!(lines.last().unwrap()["payload"]["exit_code"], 0);
    opened.close();
}

#[test]
fn close_releases_the_log_lock() {
    let opened = Opened::open(vec![]);
    opened.log.append(&turn(), None, None).unwrap();
    let sessions = opened.sessions.clone();
    let id = opened.id.clone();
    let clock = Arc::clone(&opened.clock);
    let dir = opened.dir.clone();
    let kept = opened.close();
    assert!(
        dir.is_dir(),
        "a session that got a turn keeps its directory"
    );
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    match Log::open(&sessions, id, timed) {
        Ok(_) => {}
        Err(error) => panic!("the lock was not released: {error}"),
    }
    drop(kept);
}

#[test]
fn cancel_with_no_turn_running_is_rejected_stale() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"cancel"}"#);
            let rejected = response(&client, "c_1");
            assert_eq!(
                rejection(&rejected),
                ("stale_request", "No turn is running.")
            );
            assert_eq!(command_id(&rejected), Some("c_1"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn cancel_with_a_turn_running_is_accepted_and_wakes_the_inbox() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| true), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"cancel"}"#);
            let accepted = response(&client, "c_1");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_1"));
            // The wake carries no meaning beyond waking the loop.
            assert!(matches!(
                Deadline::after(DEADLINE).recv(&inbox),
                Ok(Delivery::Cancelled)
            ));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn a_full_subscriber_after_a_queued_steer_gets_the_latest_steering_queue() {
    let opened = Opened::open(vec![]);
    opened.log.append(&step(), None, None).unwrap();
    opened
        .log
        .append(
            &Event::SteeringQueue(SteeringQueue {
                messages: vec![QueuedMessage {
                    content: vec![ContentPart::Text {
                        text: "later".into(),
                    }],
                    sender: Sender {
                        origin: Origin::Driver,
                        command_id: Some(CommandId("c_later".into())),
                    },
                }],
            }),
            None,
            None,
        )
        .unwrap();
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            let lines = until(&client, |line| kind(line) == "steering_queue");
            let at = lines
                .iter()
                .position(|line| kind(line) == "steering_queue")
                .unwrap();
            // The queue arrives after the fold, which does not have it.
            assert!(
                lines[..at]
                    .iter()
                    .any(|line| line["seq"].as_u64() == Some(0))
            );
            assert_eq!(
                lines[at]["payload"]["messages"][0]["content"][0]["text"],
                "later"
            );
            assert_eq!(lines[at]["payload"]["messages"][0]["command_id"], "c_later");
            Ok(())
        })
        .unwrap();
    opened.close();
}

fn shell_line(id: &str, command: &str) -> String {
    format!(r#"{{"id":"{id}","command":"shell","args":{{"command":"{command}"}}}}"#)
}

fn ended(code: i32, text: &str) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
        process: Some(Process {
            exit_code: Some(code),
            signal: None,
            timed_out: false,
        }),
        ..Output::default()
    }
}

fn fixed(output: Output, bound: Bound) -> Arc<dyn Tool> {
    Arc::new(Fixed {
        output,
        bound,
        ran: Arc::new(AtomicBool::new(false)),
    })
}

fn fixed_ran(output: Output, bound: Bound, ran: Arc<AtomicBool>) -> Arc<dyn Tool> {
    Arc::new(Fixed { output, bound, ran })
}

struct Fixed {
    output: Output,
    bound: Bound,
    ran: Arc<AtomicBool>,
}

/// A shell tool whose output is `len` `x` bytes, read from the command's
/// trailing number: `big 9` returns nine `x` bytes.
struct Lengths {
    bound: Bound,
}

impl Tool for Lengths {
    fn definition(&self) -> ToolDefinition {
        shell_definition()
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let command = arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("");
        let len: usize = command
            .rsplit(' ')
            .next()
            .and_then(|last| last.parse().ok())
            .unwrap_or(0);
        Output {
            content: vec![ContentPart::Text {
                text: "x".repeat(len),
            }],
            process: Some(Process {
                exit_code: Some(0),
                signal: None,
                timed_out: false,
            }),
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        self.bound
    }
}

impl Tool for Fixed {
    fn definition(&self) -> ToolDefinition {
        shell_definition()
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        // True when the command started with no cancel on it.
        self.ran.store(!cancel.is_cancelled(), Ordering::Relaxed);
        self.output.clone()
    }

    fn bound(&self) -> Bound {
        self.bound
    }
}

fn shell_definition() -> ToolDefinition {
    ToolDefinition {
        name: "shell".to_owned(),
        description: "test".to_owned(),
        input_schema: Value::Object(Map::new()),
        deferred: false,
        hosted: None,
    }
}

struct Flag {
    ready: Mutex<bool>,
    cv: Condvar,
}

impl Wake for Flag {
    fn wake(&self) {
        *self.ready.lock().expect("the flag lock") = true;
        self.cv.notify_all();
    }
}

struct Blocks {
    entered: Mutex<Option<mpsc::Sender<()>>>,
}

impl Tool for Blocks {
    fn definition(&self) -> ToolDefinition {
        shell_definition()
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let flag = Arc::new(Flag {
            ready: Mutex::new(false),
            cv: Condvar::new(),
        });
        let wake: Arc<dyn Wake> = flag.clone();
        cancel.subscribe(Arc::downgrade(&wake));
        if let Some(sender) = self.entered.lock().expect("the entered lock").take() {
            sender.send(()).expect("the test is waiting");
        }
        if !cancel.is_cancelled() {
            let guard = flag.ready.lock().expect("the flag lock");
            let _wait = flag
                .cv
                .wait_timeout_while(guard, DEADLINE, |_| !cancel.is_cancelled());
        }
        assert!(
            cancel.is_cancelled(),
            "timed out waiting for the shell to be cancelled"
        );
        Output {
            content: vec![ContentPart::Text {
                text: "Cancelled and stopped.\n".to_owned(),
            }],
            process: Some(Process {
                exit_code: None,
                signal: None,
                timed_out: false,
            }),
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// Longer than [`DEADLINE`], so a close that never cancels the shell fails
/// the test's own wait rather than this tool's timeout.
const SHELL_LIMIT: Duration = Duration::from_secs(30);

struct Hangs {
    entered: Mutex<Option<mpsc::Sender<()>>>,
    saw_cancel: Arc<AtomicBool>,
}

impl Tool for Hangs {
    fn definition(&self) -> ToolDefinition {
        shell_definition()
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let flag = Arc::new(Flag {
            ready: Mutex::new(false),
            cv: Condvar::new(),
        });
        let wake: Arc<dyn Wake> = flag.clone();
        cancel.subscribe(Arc::downgrade(&wake));
        if let Some(sender) = self.entered.lock().expect("the entered lock").take() {
            sender.send(()).expect("the test is waiting");
        }
        if !cancel.is_cancelled() {
            let guard = flag.ready.lock().expect("the flag lock");
            let _wait = flag
                .cv
                .wait_timeout_while(guard, SHELL_LIMIT, |_| !cancel.is_cancelled());
        }
        self.saw_cancel
            .store(cancel.is_cancelled(), Ordering::Relaxed);
        Output {
            content: vec![ContentPart::Text {
                text: "Cancelled and stopped.\n".to_owned(),
            }],
            process: Some(Process {
                exit_code: None,
                signal: None,
                timed_out: false,
            }),
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

fn no_durable(dir: &Path) {
    assert!(
        log::read(dir).unwrap().is_empty(),
        "a driver shell writes nothing to the log"
    );
}

#[test]
fn shell_before_subscribe_is_not_subscribed() {
    let opened = Opened::open(vec![]);
    opened
        .session
        .shell(fixed(ended(0, "hi\n"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            send(&client, &shell_line("c_1", "echo hi"));
            let line = response(&client, "c_1");
            assert_eq!(rejection(&line), ("not_subscribed", NOT_SUBSCRIBED));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_with_no_tool_is_unknown() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_1", "echo hi"));
            let line = response(&client, "c_1");
            assert_eq!(
                rejection(&line),
                ("unknown_command", "`shell` is not built in this Fiber yet.")
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_exit_zero_stays_on_the_sending_connection() {
    let opened = Opened::open(vec![]);
    opened
        .session
        .shell(fixed(ended(0, "hi\n"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            let other = Client::connect(&socket).unwrap();
            subscribe(&other, "c_other", "full");
            send(&client, &shell_line("c_1", "echo hi"));
            let line = response(&client, "c_1");
            let answered = &line["payload"]["result"];
            assert_eq!(answered["output"], "hi\n");
            assert_eq!(answered["process"]["exit_code"], 0);
            assert_eq!(answered["process"]["timed_out"], false);
            assert!(answered.get("artifact").is_none());
            send(&other, r#"{"id":"c_tools","command":"tools"}"#);
            let seen = until(&other, |line| command_id(line) == Some("c_tools"));
            assert!(
                seen.iter().all(|line| command_id(line) != Some("c_1")),
                "the other client sees the shell answer"
            );
            // The shell answers before it leaves the running list, so a
            // `cancel` right after the answer may still be accepted. The
            // retries run on a thread so the whole wait has one deadline.
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                for attempt in 0.. {
                    let id = format!("c_cancel_{attempt}");
                    send(&client, &format!(r#"{{"id":"{id}","command":"cancel"}}"#));
                    let line = response(&client, &id);
                    if kind(&line) != "command_accepted" {
                        drop(tx.send(line));
                        return;
                    }
                }
            });
            let rejected = Deadline::after(DEADLINE)
                .recv(&rx)
                .expect("the finished shell left the running list");
            assert_eq!(
                rejection(&rejected),
                ("stale_request", "No turn is running.")
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_nonzero_exit_is_accepted() {
    let opened = Opened::open(vec![]);
    opened.session.shell(fixed(ended(2, "no"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_1", "false"));
            let line = response(&client, "c_1");
            assert_eq!(kind(&line), "command_accepted");
            assert_eq!(line["payload"]["result"]["output"], "no");
            assert_eq!(line["payload"]["result"]["process"]["exit_code"], 2);
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_output_over_the_cap_is_cut_under_a_minted_name() {
    let opened = Opened::open(vec![]);
    let full = "0123456789abcdefghij";
    opened
        .session
        .shell(fixed(ended(0, full), Bound { start: 4, end: 4 }));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"../x","command":"shell","args":{"command":"big"}}"#,
            );
            let line = response(&client, "../x");
            let artifact = line["payload"]["result"]["artifact"]
                .as_str()
                .expect("the cut output names an artifact");
            assert!(artifact.starts_with("artifacts/o_"), "{artifact}");
            assert!(!artifact.contains("../x"), "{artifact}");
            let saved = fs::read(dir.join(artifact)).unwrap();
            assert_eq!(saved, full.as_bytes());
            let output = line["payload"]["result"]["output"].as_str().unwrap();
            assert_ne!(output, full);
            assert!(output.contains("bytes cut"), "{output}");
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_is_answered_while_a_turn_sits_in_the_inbox() {
    let opened = Opened::open(vec![]);
    opened
        .session
        .shell(fixed(ended(0, "hi\n"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"wait"}]}}"#,
            );
            send(&client, &shell_line("c_shell", "echo hi"));
            let line = response(&client, "c_shell");
            assert_eq!(line["payload"]["result"]["output"], "hi\n");
            let delivery = Deadline::after(DEADLINE).recv(&inbox).expect("the prompt is still queued");
            assert!(matches!(delivery, Delivery::Prompt(..)));
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn cancel_from_another_connection_stops_a_driver_shell() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let opened = Opened::open(vec![]);
    opened.session.shell(Arc::new(Blocks {
        entered: Mutex::new(Some(entered_tx)),
    }));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            let other = Client::connect(&socket).unwrap();
            subscribe(&other, "c_other", "full");
            send(&client, &shell_line("c_shell", "sleep 60"));
            Deadline::after(DEADLINE)
                .recv(&entered_rx)
                .expect("the shell is running");
            send(&other, r#"{"id":"c_cancel","command":"cancel"}"#);
            let accepted = response(&other, "c_cancel");
            assert_eq!(kind(&accepted), "command_accepted");
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "a shell cancel does not wake the loop"
            );
            let line = response(&client, "c_shell");
            assert_eq!(
                line["payload"]["result"]["output"],
                "Cancelled and stopped.\n"
            );
            assert!(
                line["payload"]["result"]["process"]
                    .get("exit_code")
                    .is_none()
            );
            assert_eq!(line["payload"]["result"]["process"]["timed_out"], false);
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

/// The answer to `id`, or none when a line is not read within [`DEADLINE`].
/// Lets a test close its session before it asserts.
fn answer_within(client: &Client, id: &str) -> Option<Value> {
    answers_within(client, &[id]).pop().flatten()
}

/// The answers to `ids` in the order named, whatever order they arrive in.
/// Reading stops once each is answered, or when a line is not read within
/// [`DEADLINE`], which leaves the rest none.
fn answers_within(client: &Client, ids: &[&str]) -> Vec<Option<Value>> {
    let mut answers: Vec<Option<Value>> = vec![None; ids.len()];
    while answers.iter().any(Option::is_none) {
        let Some(line) = client.recv(fakes::Deadline::after(DEADLINE)) else {
            break;
        };
        if let Some(slot) = ids
            .iter()
            .position(|id| command_id(&line) == Some(*id))
            .and_then(|at| answers.get_mut(at))
        {
            *slot = Some(line);
        }
    }
    answers
}

#[test]
fn cancel_on_the_same_connection_stops_a_driver_shell() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let opened = Opened::open(vec![]);
    opened.session.shell(Arc::new(Hangs {
        entered: Mutex::new(Some(entered_tx)),
        saw_cancel: Arc::clone(&saw_cancel),
    }));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    let mut seen = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_shell", "sleep 60"));
            Deadline::after(DEADLINE)
                .recv(&entered_rx)
                .expect("the shell is running");
            send(&client, r#"{"id":"c_cancel","command":"cancel"}"#);
            // The cancel wakes the shell before its own answer is queued,
            // so either answer can come first. On a reader blocked in the
            // shell, neither would come before close.
            let mut answers = answers_within(&client, &["c_cancel", "c_shell"]).into_iter();
            let (cancel, shell) = (answers.next().flatten(), answers.next().flatten());
            let woken = !matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty));
            no_durable(&dir);
            seen = Some((cancel, woken, shell, client));
            Ok(())
        })
        .unwrap();
    opened.close();
    let (cancel, woken, shell, _client) = seen.expect("the client ran");
    let cancel = cancel.expect("cancel on the shell's own connection was answered");
    assert_eq!(kind(&cancel), "command_accepted", "{cancel}");
    assert!(!woken, "a shell cancel does not wake the loop");
    assert!(
        saw_cancel.load(Ordering::Relaxed),
        "the tool saw its cancel"
    );
    let line = shell.expect("the cancelled shell was answered");
    assert_eq!(kind(&line), "command_accepted", "{line}");
    assert_eq!(
        line["payload"]["result"]["output"],
        "Cancelled and stopped.\n"
    );
    assert!(
        line["payload"]["result"]["process"]
            .get("exit_code")
            .is_none()
    );
    assert_eq!(line["payload"]["result"]["process"]["timed_out"], false);
}

/// Hangs on its first call, as [`Hangs`]; answers every later one at once.
struct HangsOnce {
    hangs: Hangs,
    calls: AtomicUsize,
}

impl Tool for HangsOnce {
    fn definition(&self) -> ToolDefinition {
        shell_definition()
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, emit: &dyn Emit) -> Output {
        if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
            self.hangs.run(arguments, cancel, emit)
        } else {
            ended(0, "hi\n")
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

#[test]
fn a_connection_reads_on_while_its_driver_shell_runs() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let opened = Opened::open(vec![tool()]);
    opened.session.shell(Arc::new(HangsOnce {
        hangs: Hangs {
            entered: Mutex::new(Some(entered_tx)),
            saw_cancel: Arc::clone(&saw_cancel),
        },
        calls: AtomicUsize::new(0),
    }));
    let socket = opened.socket.clone();
    let mut seen = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_held", "sleep 60"));
            Deadline::after(DEADLINE)
                .recv(&entered_rx)
                .expect("the shell is running");
            send(&client, r#"{"id":"c_tools","command":"tools"}"#);
            let tools = answer_within(&client, "c_tools");
            send(&client, &shell_line("c_second", "echo hi"));
            let second = answer_within(&client, "c_second");
            seen = Some((tools, second, client));
            Ok(())
        })
        .unwrap();
    opened.close();
    let (tools, second, _client) = seen.expect("the client ran");
    let tools = tools.expect("tools was answered while the shell ran");
    assert_eq!(kind(&tools), "command_accepted", "{tools}");
    assert_eq!(tools["payload"]["result"]["tools"][0]["name"], "read");
    let second = second.expect("a second shell was answered while the first ran");
    assert_eq!(second["payload"]["result"]["output"], "hi\n");
    assert!(
        saw_cancel.load(Ordering::Relaxed),
        "close cancelled the first shell"
    );
}

#[test]
fn close_cancels_a_shell_blocked_in_its_tool() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let opened = Opened::open(vec![]);
    opened.session.shell(Arc::new(Hangs {
        entered: Mutex::new(Some(entered_tx)),
        saw_cancel: Arc::clone(&saw_cancel),
    }));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_shell", "sleep 60"));
            Deadline::after(DEADLINE)
                .recv(&entered_rx)
                .expect("the shell is running");
            Ok(())
        })
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let session = opened.session;
    let log = opened.log;
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("close returned without the shell's timeout");
    assert!(
        saw_cancel.load(Ordering::Relaxed),
        "the tool did not see its cancel"
    );
}

#[test]
fn a_shell_nobody_cancelled_starts_uncancelled() {
    let opened = Opened::open(vec![]);
    let ran = Arc::new(AtomicBool::new(false));
    opened.session.shell(fixed_ran(
        ended(0, "hi\n"),
        Bound::DEFAULT,
        Arc::clone(&ran),
    ));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_1", "echo hi"));
            response(&client, "c_1");
            assert!(ran.load(Ordering::Relaxed), "the tool saw a cancel");
            Ok(())
        })
        .unwrap();
    opened.close();
}

fn shell_send_line(id: &str, command: &str) -> String {
    format!(r#"{{"id":"{id}","command":"shell","args":{{"command":"{command}","send":true}}}}"#)
}

#[test]
fn shell_send_true_is_delivered_to_the_inbox_and_accepted_without_a_result() {
    let opened = Opened::open(vec![]);
    opened
        .session
        .shell(fixed(ended(0, "hi\n"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_send_line("c_2", "echo hi"));
            // The loop's side takes the delivery before the client is answered.
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the sent shell is delivered");
            let Delivery::Shell(line, ack) = delivery else {
                panic!("a sent shell arrives as a shell");
            };
            assert_eq!(line.command, "echo hi");
            assert_eq!(line.output, "hi\n");
            assert!(line.artifact.is_none());
            assert_eq!(line.process.exit_code, Some(0));
            assert_eq!(line.process.signal, None);
            assert!(!line.process.timed_out);
            // The ack is still held, so no answer can arrive yet: the loop
            // answers the client only when it takes the delivery.
            ack.0(Ok(None));
            let accepted = response(&client, "c_2");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_2"));
            assert!(
                accepted["payload"].get("result").is_none(),
                "a sent shell answers with no result: {accepted}"
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_send_true_cut_boundary_names_artifact_only_past_the_cap() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened.session.shell(Arc::new(Lengths {
        bound: Bound { start: 4, end: 4 },
    }));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            // The cap is 8: below it and at it the output stays whole,
            // past it the delivery names the cut file.
            for (id, len, cut) in [("c_7", 7, false), ("c_8", 8, false), ("c_9", 9, true)] {
                let command = format!("big {len}");
                send(&client, &shell_send_line(id, &command));
                let delivery = Deadline::after(DEADLINE)
                    .recv(&inbox)
                    .expect("the sent shell is delivered");
                let Delivery::Shell(line, ack) = delivery else {
                    panic!("a sent shell arrives as a shell");
                };
                assert_eq!(line.command, command);
                assert_eq!(line.artifact.is_some(), cut, "{id}");
                if cut {
                    let artifact = line.artifact.as_deref().unwrap();
                    assert!(artifact.starts_with("artifacts/o_"), "{artifact}");
                    assert_eq!(fs::read(dir.join(artifact)).unwrap(), vec![b'x'; len]);
                    assert!(
                        line.output.contains("bytes cut"),
                        "the cut output says so: {}",
                        line.output
                    );
                } else {
                    assert_eq!(line.output, "x".repeat(len));
                }
                assert_eq!(line.process.exit_code, Some(0));
                ack.0(Ok(None));
                let accepted = response(&client, id);
                assert_eq!(kind(&accepted), "command_accepted");
                assert!(accepted["payload"].get("result").is_none());
            }
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_send_true_spilled_output_delivers_the_spill_file() {
    let opened = Opened::open(vec![]);
    let path = opened.dir.join("artifacts/s_test.txt");
    let saved = b"original spilled bytes, kept whole";
    fs::write(&path, saved).unwrap();
    let size = u64::try_from(saved.len()).unwrap();
    opened.session.shell(fixed(
        spilled_ok(
            "HEAD",
            "TAIL",
            5,
            path.clone(),
            size,
            "competing, ignored",
            Some(0),
        ),
        Bound { start: 1, end: 1 },
    ));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_send_line("c_big", "cat big"));
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the sent shell is delivered");
            let Delivery::Shell(line, ack) = delivery else {
                panic!("a sent shell arrives as a shell");
            };
            let absolute = path.display().to_string();
            let notice =
                format!("[5 bytes cut. The full output is in {absolute}; read it with `read`.]");
            assert_eq!(line.output, format!("HEAD\n{notice}\nTAIL"));
            assert_eq!(line.artifact.as_deref(), Some("artifacts/s_test.txt"));
            assert_eq!(line.process.exit_code, Some(0));
            ack.0(Ok(None));
            let accepted = response(&client, "c_big");
            assert_eq!(kind(&accepted), "command_accepted");
            assert!(accepted["payload"].get("result").is_none());
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_send_true_cancelled_tool_delivers_its_signal() {
    let opened = Opened::open(vec![]);
    opened.session.shell(fixed(
        Output {
            content: vec![ContentPart::Text {
                text: "stopped\n".to_owned(),
            }],
            process: Some(Process {
                exit_code: None,
                signal: Some("SIGKILL".to_owned()),
                timed_out: false,
            }),
            ..Output::default()
        },
        Bound::DEFAULT,
    ));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_send_line("c_sig", "killed"));
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the sent shell is delivered");
            let Delivery::Shell(line, ack) = delivery else {
                panic!("a sent shell arrives as a shell");
            };
            assert_eq!(line.output, "stopped\n");
            assert!(line.artifact.is_none());
            assert_eq!(line.process.exit_code, None);
            assert_eq!(line.process.signal.as_deref(), Some("SIGKILL"));
            ack.0(Ok(None));
            let accepted = response(&client, "c_sig");
            assert_eq!(kind(&accepted), "command_accepted");
            assert!(accepted["payload"].get("result").is_none());
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_send_true_without_a_process_is_invalid_arguments() {
    let opened = Opened::open(vec![]);
    opened.session.shell(fixed(
        Output {
            error: Some(Failure {
                code: ErrorCode::InvalidArguments,
                message: "Give one command.".to_owned(),
                retry_after_ms: None,
                provider: None,
            }),
            ..Output::default()
        },
        Bound::DEFAULT,
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_send_line("c_1", ""));
            let line = response(&client, "c_1");
            assert_eq!(rejection(&line), ("invalid_arguments", "Give one command."));
            assert!(
                inbox.try_recv().is_err(),
                "a shell that never started reaches no inbox"
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_send_false_delivers_nothing_to_the_inbox() {
    let opened = Opened::open(vec![]);
    opened
        .session
        .shell(fixed(ended(0, "hi\n"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_1", "echo hi"));
            let line = response(&client, "c_1");
            assert_eq!(line["payload"]["result"]["output"], "hi\n");
            assert!(
                inbox.try_recv().is_err(),
                "an unsent shell reaches no inbox"
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_send_true_once_the_inbox_is_gone_is_rejected_ended() {
    let opened = Opened::open(vec![]);
    opened
        .session
        .shell(fixed(ended(0, "hi\n"), Bound::DEFAULT));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            drop(inbox);
            send(&client, &shell_send_line("c_gone", "echo hi"));
            let rejected = response(&client, "c_gone");
            assert_eq!(rejection(&rejected), ("closing", ENDED));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_without_a_process_is_invalid_arguments() {
    let opened = Opened::open(vec![]);
    opened.session.shell(fixed(
        Output {
            error: Some(Failure {
                code: ErrorCode::InvalidArguments,
                message: "Give one command.".to_owned(),
                retry_after_ms: None,
                provider: None,
            }),
            ..Output::default()
        },
        Bound::DEFAULT,
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &shell_line("c_1", ""));
            let line = response(&client, "c_1");
            assert_eq!(rejection(&line), ("invalid_arguments", "Give one command."));
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

fn job_stop_line(id: &str, job_id: &str) -> String {
    format!(r#"{{"id":"{id}","command":"job_stop","args":{{"job_id":"{job_id}"}}}}"#)
}

/// Opens a job on `jobs` whose stop signals `fired`.
fn open_job(jobs: &FakeJobs, fired: mpsc::Sender<()>) -> contract::jobs::Opened {
    jobs.open(Opening {
        tool: "shell".into(),
        description: "sleep 60".into(),
        stop: Stop(Box::new(move || fired.send(()).unwrap())),
        lines: false,
        input: None,
    })
    .unwrap()
}

#[test]
fn job_stop_is_answered_on_the_reader_thread_while_the_loop_is_blocked() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let (fired_tx, fired_rx) = mpsc::channel();
    let job = open_job(&jobs, fired_tx);
    let job_id = job.started.job_id.0.clone();
    let opened = Opened::open(vec![]);
    opened.session.jobs(jobs.clone());
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            // This body is the loop's side: it drains nothing until the
            // stop has been sent, so the reader thread answered alone.
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &job_stop_line("c_stop", &job_id));
            let accepted = response(&client, "c_stop");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_stop"));
            assert!(
                Deadline::after(DEADLINE).recv(&fired_rx).is_ok(),
                "the job's stop was not sent"
            );
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "job_stop waited for the drain"
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    drop(job);
    opened.close();
}

#[test]
fn job_stop_for_an_unknown_or_ended_job_is_rejected_stale() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let (fired_tx, fired_rx) = mpsc::channel();
    let job = open_job(&jobs, fired_tx);
    let ended_id = job.started.job_id.0.clone();
    (job.end.0)(contract::events::JobCompleted {
        job_id: job.started.job_id.clone(),
        status: contract::events::Outcome::Completed,
        error: None,
        process: None,
        output_tail: None,

        notice: None,
        changed_by: None,
        protected_paths: None,
    });
    let opened = Opened::open(vec![]);
    opened.session.jobs(jobs.clone());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            for (id, job_id) in [("c_1", "j_5e10c0ffee123456"), ("c_2", ended_id.as_str())] {
                send(&client, &job_stop_line(id, job_id));
                let rejected = response(&client, id);
                assert_eq!(
                    rejection(&rejected),
                    ("stale_request", "That job is not running.")
                );
                assert_eq!(command_id(&rejected), Some(id));
            }
            assert!(
                fired_rx.try_recv().is_err(),
                "a stale stop called a job's stop"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn job_stop_and_background_without_jobs_are_rejected_stale() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &job_stop_line("c_1", "j_5e10c0ffee123456"));
            assert_eq!(
                rejection(&response(&client, "c_1")),
                ("stale_request", "That job is not running.")
            );
            send(&client, r#"{"id":"c_2","command":"background"}"#);
            let rejected = response(&client, "c_2");
            assert_eq!(
                rejection(&rejected),
                ("stale_request", "No shell call is running.")
            );
            assert_eq!(command_id(&rejected), Some("c_2"));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn job_stop_with_a_malformed_job_id_is_invalid_arguments() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"job_stop"}"#);
            assert_eq!(
                rejection(&response(&client, "c_1")),
                ("invalid_arguments", UNFIT)
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

fn job_input_line(id: &str, job_id: &str, text: &str) -> String {
    let text = serde_json::to_string(text).unwrap();
    format!(r#"{{"id":"{id}","command":"job_input","args":{{"job_id":"{job_id}","text":{text}}}}}"#)
}

/// Jobs whose `write` types into the inner fake's terminal, as the registry
/// does: accepted while the job runs with one, `NotTty` while it runs
/// without one, `NotRunning` for an unknown id and `Ended` once it ended.
/// A job
/// passed to [`WritableJobs::stick`] never takes another byte instead:
/// its write parks until the session clock moves a second on, then reports
/// the timeout the registry reports.
struct WritableJobs {
    inner: Arc<FakeJobs>,
    clock: Arc<FakeClock>,
    stuck: Mutex<Vec<(contract::JobId, mpsc::Sender<std::thread::ThreadId>)>>,
}

impl WritableJobs {
    /// Wraps `jobs`, parking stuck writes on the session clock.
    fn wrap(jobs: Arc<FakeJobs>, clock: Arc<FakeClock>) -> Arc<Self> {
        Arc::new(Self {
            inner: jobs,
            clock,
            stuck: Mutex::new(Vec::new()),
        })
    }

    /// Parks the next `write` to `job_id` until the session clock moves a
    /// second on. Returns a receiver that fires once the write parks.
    fn stick(&self, job_id: &contract::JobId) -> mpsc::Receiver<std::thread::ThreadId> {
        let (tx, rx) = mpsc::channel();
        self.stuck.lock().unwrap().push((job_id.clone(), tx));
        rx
    }
}

impl Jobs for WritableJobs {
    fn open(&self, opening: Opening) -> Result<contract::jobs::Opened, contract::jobs::OpenError> {
        self.inner.open(opening)
    }

    fn stop(&self, job_id: &contract::JobId) -> bool {
        self.inner.stop(job_id)
    }

    fn background(&self) -> usize {
        self.inner.background()
    }

    fn foreground(&self, call: Foreground) {
        self.inner.foreground(call)
    }

    fn running(&self) -> Vec<contract::JobId> {
        self.inner.running()
    }

    fn stop_delegates(&self) -> usize {
        self.inner.stop_delegates()
    }

    fn deliver_to(&self, inbox: mpsc::Sender<Delivery>) {
        self.inner.deliver_to(inbox)
    }

    #[track_caller]
    fn write(
        &self,
        job_id: &contract::JobId,
        text: &str,
    ) -> Result<(), contract::jobs::WriteError> {
        if !self.inner.running().contains(job_id) {
            if self
                .inner
                .started()
                .iter()
                .any(|started| &started.job_id == job_id)
            {
                return Err(contract::jobs::WriteError::Ended(
                    contract::events::Outcome::Completed,
                ));
            }
            return Err(contract::jobs::WriteError::NotRunning);
        }
        if let Some(entered) = self
            .stuck
            .lock()
            .unwrap()
            .iter()
            .find(|(stuck, _)| stuck == job_id)
            .map(|(_, entered)| entered.clone())
        {
            struct Nudge(mpsc::Sender<()>);
            impl Wake for Nudge {
                fn wake(&self) {
                    let _sent = self.0.send(());
                }
            }
            let (tx, rx) = mpsc::channel();
            let wake: Arc<dyn Wake> = Arc::new(Nudge(tx));
            self.clock.subscribe(Arc::downgrade(&wake));
            let until = self.clock.now() + Duration::from_secs(1);
            // The test moves the clock only after this fires, so the
            // deadline is computed and the waker subscribed before any
            // advance.
            let _sent = entered.send(thread::current().id());
            loop {
                // The writer's own clock check: past it before parking.
                if self.clock.now() >= until {
                    break;
                }
                let mut expired = false;
                self.clock.wait_until(None, &mut |_| {
                    // Bounded in real time: a hang-guard expiry fails
                    // the test with another error instead of the timeout.
                    if Deadline::after(Duration::from_secs(10)).recv(&rx).is_err() {
                        expired = true;
                    }
                });
                if expired {
                    return Err(contract::jobs::WriteError::Io(std::io::Error::other(
                        "hang guard expired without the clock moving",
                    )));
                }
            }
            return Err(contract::jobs::WriteError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "The job's terminal did not take the input.",
            )));
        }
        match self.inner.type_into(
            job_id,
            text.as_bytes(),
            self.clock.as_ref(),
            &fakes::CancelToken::new(),
        ) {
            Some(Ok(_)) => Ok(()),
            Some(Err(error)) => Err(contract::jobs::WriteError::Io(error)),
            None => Err(contract::jobs::WriteError::NotTty),
        }
    }
}

/// Opens a `tty` job on `jobs`, recording what is typed into it.
fn open_tty_job(jobs: &FakeJobs, typed: Arc<Mutex<Vec<u8>>>) -> contract::jobs::Opened {
    jobs.open(Opening {
        tool: "shell".into(),
        description: "cat".into(),
        stop: Stop(Box::new(|| {})),
        lines: false,
        input: Some(contract::jobs::Input(Box::new(move |bytes, _, _| {
            typed.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }))),
    })
    .unwrap()
}

#[test]
fn job_input_is_answered_on_the_reader_thread_while_the_loop_is_blocked() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let typed = Arc::new(Mutex::new(Vec::new()));
    let job = open_tty_job(&jobs, Arc::clone(&typed));
    let job_id = job.started.job_id.0.clone();
    let opened = Opened::open(vec![]);
    opened
        .session
        .jobs(WritableJobs::wrap(jobs, opened.clock.clone()));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            // This body is the loop's side: it drains nothing until the
            // write has been sent, so the reader thread answered alone.
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &job_input_line("c_in", &job_id, "hi\u{1b}[A"));
            let accepted = response(&client, "c_in");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_in"));
            assert_eq!(typed.lock().unwrap().as_slice(), b"hi\x1b[A");
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "job_input waited for the drain"
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    drop(job);
    opened.close();
}

#[test]
fn job_input_for_an_unknown_or_ended_job_is_rejected_stale() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let job = open_tty_job(&jobs, Arc::new(Mutex::new(Vec::new())));
    let ended_id = job.started.job_id.0.clone();
    (job.end.0)(contract::events::JobCompleted {
        job_id: job.started.job_id.clone(),
        status: contract::events::Outcome::Completed,
        error: None,
        process: None,
        output_tail: None,

        notice: None,
        changed_by: None,
        protected_paths: None,
    });
    let opened = Opened::open(vec![]);
    opened
        .session
        .jobs(WritableJobs::wrap(jobs, opened.clock.clone()));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            for (id, job_id) in [("c_1", "j_5e10c0ffee123456"), ("c_2", ended_id.as_str())] {
                send(&client, &job_input_line(id, job_id, "x"));
                let rejected = response(&client, id);
                assert_eq!(
                    rejection(&rejected),
                    ("stale_request", "That job is not running.")
                );
                assert_eq!(command_id(&rejected), Some(id));
            }
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn job_input_to_a_job_without_a_terminal_is_rejected_invalid_arguments() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let (fired_tx, _fired_rx) = mpsc::channel();
    let job = open_job(&jobs, fired_tx);
    let job_id = job.started.job_id.0.clone();
    let opened = Opened::open(vec![]);
    opened
        .session
        .jobs(WritableJobs::wrap(jobs, opened.clock.clone()));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &job_input_line("c_1", &job_id, "x"));
            assert_eq!(
                rejection(&response(&client, "c_1")),
                ("invalid_arguments", "That job was not started with `tty`.")
            );
            Ok(())
        })
        .unwrap();
    drop(job);
    opened.close();
}

#[test]
fn job_input_to_a_closed_terminal_is_rejected_io_failed() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let job = jobs
        .open(Opening {
            tool: "shell".into(),
            description: "cat".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(|_, _, _| {
                Err(std::io::Error::other("the terminal is closed"))
            }))),
        })
        .unwrap();
    let job_id = job.started.job_id.0.clone();
    let opened = Opened::open(vec![]);
    opened
        .session
        .jobs(WritableJobs::wrap(jobs, opened.clock.clone()));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &job_input_line("c_1", &job_id, "x"));
            assert_eq!(
                rejection(&response(&client, "c_1")),
                (
                    "io_failed",
                    "Writing to the job's terminal failed: the terminal is closed."
                )
            );
            Ok(())
        })
        .unwrap();
    drop(job);
    opened.close();
}

#[test]
fn job_stop_is_answered_after_a_stuck_job_input_times_out() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let job = open_tty_job(&jobs, Arc::new(Mutex::new(Vec::new())));
    let job_id = job.started.job_id.0.clone();
    let opened = Opened::open(vec![]);
    let writable = WritableJobs::wrap(jobs, opened.clock.clone());
    let parked = writable.stick(&contract::JobId(job_id.clone()));
    opened.session.jobs(writable);
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    let clock = opened.clock.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &job_input_line("c_in", &job_id, "x"));
            // The reader is stuck in the write: parked inside the clock
            // wait, past its own check, before the clock moves.
            let writer = Deadline::after(DEADLINE)
                .recv(&parked)
                .expect("the write parked in the terminal");
            clock.advance_when_parked(
                None,
                Some(writer),
                Duration::from_secs(2),
                &Deadline::after(DEADLINE),
            );
            let rejected = response(&client, "c_in");
            assert_eq!(
                rejection(&rejected),
                ("io_failed", "The job's terminal did not take the input.")
            );
            assert_eq!(command_id(&rejected), Some("c_in"));
            // The same connection keeps reading.
            send(&client, &job_stop_line("c_stop", &job_id));
            let accepted = response(&client, "c_stop");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_stop"));
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    drop(job);
    opened.close();
}

#[test]
fn job_input_with_malformed_args_is_invalid_arguments() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"job_input"}"#);
            assert_eq!(
                rejection(&response(&client, "c_1")),
                ("invalid_arguments", UNFIT)
            );
            send(
                &client,
                r#"{"id":"c_2","command":"job_input","args":{"job_id":"j_1"}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_2")),
                ("invalid_arguments", UNFIT)
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn job_input_without_jobs_is_rejected_stale() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &job_input_line("c_1", "j_5e10c0ffee123456", "x"));
            assert_eq!(
                rejection(&response(&client, "c_1")),
                ("stale_request", "That job is not running.")
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn background_asks_the_running_foreground_calls_and_writes_nothing() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let asked = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&asked);
    let call: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        seen.store(true, Ordering::SeqCst);
        true
    });
    jobs.foreground(Foreground(Arc::downgrade(&call)));
    let opened = Opened::open(vec![]);
    opened.session.jobs(jobs.clone());
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"background"}"#);
            let accepted = response(&client, "c_1");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_1"));
            assert!(
                asked.load(Ordering::SeqCst),
                "the call was not asked to move"
            );
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "background waited for the drain"
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
    drop(call);
}

#[test]
fn background_with_no_call_moving_is_rejected_stale() {
    let temp = Temp::new();
    let jobs = FakeJobs::new(&temp.0);
    let staying: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| false);
    jobs.foreground(Foreground(Arc::downgrade(&staying)));
    let opened = Opened::open(vec![]);
    opened.session.jobs(jobs.clone());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"background"}"#);
            assert_eq!(
                rejection(&response(&client, "c_1")),
                ("stale_request", "No shell call is running.")
            );
            Ok(())
        })
        .unwrap();
    opened.close();
    drop(staying);
}

#[test]
fn commands_answers_with_the_rows_it_was_given_while_the_inbox_is_unread() {
    let opened = Opened::open(vec![]);
    opened.session.commands(vec![
        CommandInfo {
            extension: false,
            name: "review".into(),
            description: "Review a diff.".into(),
            argument_hint: Some("[base]".into()),
            tag: "template".into(),
        },
        CommandInfo {
            extension: false,
            name: "tdd".into(),
            description: "Test first.".into(),
            argument_hint: None,
            tag: "skill".into(),
        },
    ]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            send(&client, r#"{"id":"c_early","command":"commands"}"#);
            assert_eq!(
                rejection(&response(&client, "c_early")),
                ("not_subscribed", NOT_SUBSCRIBED)
            );
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"wait"}]}}"#,
            );
            send(&client, r#"{"id":"c_1","command":"commands"}"#);
            let answer = response(&client, "c_1");
            assert_eq!(kind(&answer), "command_accepted");
            assert_eq!(
                answer["payload"]["result"],
                serde_json::json!({"commands": [
                    {"name": "review", "description": "Review a diff.",
                     "argument_hint": "[base]", "tag": "template"},
                    {"name": "tdd", "description": "Test first.", "tag": "skill"}]})
            );
            send(
                &client,
                r#"{"id":"c_2","command":"commands","args":{"future":1}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_2")),
                ("invalid_arguments", UNFIT)
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn commands_with_none_given_answers_an_empty_list() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"commands","args":{}}"#);
            let answer = response(&client, "c_1");
            assert_eq!(kind(&answer), "command_accepted");
            assert_eq!(
                answer["payload"]["result"],
                serde_json::json!({"commands": []})
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn skills_answers_with_the_rows_it_was_given_while_the_inbox_is_unread() {
    let opened = Opened::open(vec![]);
    opened.session.skills(vec![
        SkillInfo {
            name: "review".into(),
            description: "Repository review.".into(),
            path: "/w/.agents/skills/review/SKILL.md".into(),
            source: SkillSource::Repository,
            extension: None,
            model_invocable: true,
            disabled: false,
            shadows: vec!["/h/skills/review/SKILL.md".into()],
            shadowed_by: None,
        },
        SkillInfo {
            name: "review".into(),
            description: "Personal review.".into(),
            path: "/h/skills/review/SKILL.md".into(),
            source: SkillSource::Personal,
            extension: None,
            model_invocable: true,
            disabled: false,
            shadows: vec![],
            shadowed_by: Some("/w/.agents/skills/review/SKILL.md".into()),
        },
        SkillInfo {
            name: "tidy".into(),
            description: "Tidies.".into(),
            path: "/h/extensions/acme/skills/tidy/SKILL.md".into(),
            source: SkillSource::Extension,
            extension: Some("acme".into()),
            model_invocable: true,
            disabled: true,
            shadows: vec![],
            shadowed_by: None,
        },
    ]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            send(&client, r#"{"id":"c_early","command":"skills"}"#);
            assert_eq!(
                rejection(&response(&client, "c_early")),
                ("not_subscribed", NOT_SUBSCRIBED)
            );
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"wait"}]}}"#,
            );
            send(&client, r#"{"id":"c_1","command":"skills"}"#);
            let answer = response(&client, "c_1");
            assert_eq!(kind(&answer), "command_accepted");
            assert_eq!(
                answer["payload"]["result"],
                serde_json::json!({"skills": [
                    {"name": "review", "description": "Repository review.",
                     "path": "/w/.agents/skills/review/SKILL.md", "source": "repository",
                     "model_invocable": true, "disabled": false,
                     "shadows": ["/h/skills/review/SKILL.md"]},
                    {"name": "review", "description": "Personal review.",
                     "path": "/h/skills/review/SKILL.md", "source": "personal",
                     "model_invocable": true, "disabled": false, "shadows": [],
                     "shadowed_by": "/w/.agents/skills/review/SKILL.md"},
                    {"name": "tidy", "description": "Tidies.",
                     "path": "/h/extensions/acme/skills/tidy/SKILL.md",
                     "source": "extension", "extension": "acme",
                     "model_invocable": true, "disabled": true, "shadows": []}]})
            );
            send(
                &client,
                r#"{"id":"c_2","command":"skills","args":{"future":1}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "c_2")),
                ("invalid_arguments", UNFIT)
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn skills_with_none_given_answers_an_empty_list() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_1","command":"skills","args":{}}"#);
            let answer = response(&client, "c_1");
            assert_eq!(kind(&answer), "command_accepted");
            assert_eq!(
                answer["payload"]["result"],
                serde_json::json!({"skills": []})
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn quota_answers_without_waiting_for_the_inbox() {
    let opened = Opened::open(vec![]);
    let rows = serde_json::json!([{"provider": "p", "credential": "work", "credit": {"remaining": 4.2, "limit": 10.0}, "unreachable": true}]);
    let quota: Vec<contract::events::QuotaInfo> = serde_json::from_value(rows.clone()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    opened.session.quota(Arc::new({
        let seen = Arc::clone(&seen);
        move |fetch| {
            seen.lock().unwrap().push(fetch);
            (quota.clone(), 70.0)
        }
    }));
    let socket = opened.socket.clone();
    opened.session.run(Vec::new(), Arc::new(|| false), move |_inbox| {
        let client = Client::connect(&socket).unwrap();
        send(&client, r#"{"id":"early","command":"quota"}"#);
        assert_eq!(rejection(&response(&client, "early")), ("not_subscribed", NOT_SUBSCRIBED));
        subscribe(&client, "sub", "full");
        send(&client, r#"{"id":"prompt","command":"prompt","args":{"content":[{"type":"text","text":"wait"}]}}"#);
        send(&client, r#"{"id":"quota","command":"quota"}"#);
        let answer = response(&client, "quota");
        assert_eq!(kind(&answer), "command_accepted");
        assert_eq!(answer["payload"]["result"], serde_json::json!({"quota": rows, "notice_at": 70.0}));
        // The fetch flag travels from the line to the reader: the same
        // rows come back, and the reader saw the flag.
        send(&client, r#"{"id":"fetch","command":"quota","args":{"fetch":true}}"#);
        let answer = response(&client, "fetch");
        assert_eq!(kind(&answer), "command_accepted");
        assert_eq!(answer["payload"]["result"], serde_json::json!({"quota": rows, "notice_at": 70.0}));
        send(&client, r#"{"id":"bad","command":"quota","args":{"future":1}}"#);
        assert_eq!(rejection(&response(&client, "bad")), ("invalid_arguments", UNFIT));
        Ok(())
    }).unwrap();
    assert_eq!(*seen.lock().unwrap(), [false, true]);
    opened.close();
}

#[test]
fn quota_without_a_reader_answers_empty() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "sub", "full");
            send(&client, r#"{"id":"quota","command":"quota","args":{}}"#);
            let answer = response(&client, "quota");
            assert_eq!(kind(&answer), "command_accepted");
            assert_eq!(
                answer["payload"]["result"],
                serde_json::json!({"quota": [], "notice_at": 80.0})
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn credentials_answers_without_waiting_for_the_inbox() {
    let opened = Opened::open(vec![]);
    let rows = serde_json::json!({"provider":"fake", "credentials":["default","home","work"], "credential":"home"});
    let labels: contract::events::CredentialLabels = serde_json::from_value(rows.clone()).unwrap();
    opened.session.credentials(
        Arc::new(move || Some(labels.clone())),
        Arc::new(|| "fake".into()),
    );
    let socket = opened.socket.clone();
    opened.session.run(Vec::new(), Arc::new(|| false), move |_inbox| {
        let client = Client::connect(&socket).unwrap();
        send(&client, r#"{"id":"early","command":"credentials"}"#);
        assert_eq!(rejection(&response(&client, "early")), ("not_subscribed", NOT_SUBSCRIBED));
        subscribe(&client, "sub", "full");
        send(&client, r#"{"id":"prompt","command":"prompt","args":{"content":[{"type":"text","text":"wait"}]}}"#);
        send(&client, r#"{"id":"labels","command":"credentials"}"#);
        let answer = response(&client, "labels");
        assert_eq!(kind(&answer), "command_accepted");
        assert_eq!(answer["payload"]["result"], rows);
        send(&client, r#"{"id":"bad","command":"credentials","args":{"future":1}}"#);
        assert_eq!(rejection(&response(&client, "bad")), ("invalid_arguments", UNFIT));
        Ok(())
    }).unwrap();
    opened.close();
}

#[test]
fn credentials_without_a_reader_is_unknown() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "sub", "full");
            send(
                &client,
                r#"{"id":"labels","command":"credentials","args":{}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "labels")),
                (
                    "unknown_command",
                    "`credentials` is not built in this Fiber yet."
                )
            );
            send(
                &client,
                r#"{"id":"switch","command":"credential","args":{"label":"work"}}"#,
            );
            let Delivery::Credential(_, ack) =
                Deadline::after(DEADLINE).recv_or_fail(&inbox, "credential delivery")
            else {
                panic!("expected credential");
            };
            ack.0(Ok(None));
            let answer = response(&client, "switch");
            assert_eq!(kind(&answer), "command_accepted");
            assert!(answer["payload"].get("result").is_none());
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn credentials_after_its_session_is_gone_is_closing() {
    let opened = Opened::open(vec![]);
    opened
        .session
        .credentials(Arc::new(|| None), Arc::new(|| "fake".into()));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "sub", "full");
            send(&client, r#"{"id":"labels","command":"credentials"}"#);
            assert_eq!(rejection(&response(&client, "labels")), ("closing", ENDED));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn a_credential_acceptance_names_the_provider_read_when_the_loop_answers() {
    let opened = Opened::open(vec![]);
    let provider = Arc::new(Mutex::new(String::from("fake")));
    let read = Arc::clone(&provider);
    opened.session.credentials(
        Arc::new(|| None),
        Arc::new(move || read.lock().unwrap().clone()),
    );
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "sub", "full");
            send(
                &client,
                r#"{"id":"switch","command":"credential","args":{"label":"work"}}"#,
            );
            let Delivery::Credential(args, ack) =
                Deadline::after(DEADLINE).recv_or_fail(&inbox, "credential delivery")
            else {
                panic!("expected credential");
            };
            assert_eq!(args.label, "work");
            *provider.lock().unwrap() = "other".into();
            ack.0(Ok(None));
            let answer = response(&client, "switch");
            assert_eq!(kind(&answer), "command_accepted");
            assert_eq!(
                answer["payload"]["result"],
                serde_json::json!({"provider":"other"})
            );
            send(
                &client,
                r#"{"id":"reject","command":"credential","args":{"label":"nope"}}"#,
            );
            let Delivery::Credential(_, ack) =
                Deadline::after(DEADLINE).recv_or_fail(&inbox, "rejected credential delivery")
            else {
                panic!("expected credential");
            };
            ack.0(Err(Rejection {
                code: ErrorCode::CredentialMissing,
                message: "no such label".into(),
            }));
            let answer = response(&client, "reject");
            assert_eq!(rejection(&answer), ("credential_missing", "no such label"));
            assert!(answer["payload"].get("result").is_none());
            Ok(())
        })
        .unwrap();
    opened.close();
}

struct FakeDoor {
    calls: Mutex<Vec<(String, String)>>,
}

impl FakeDoor {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl contract::extension::ExtensionDoor for FakeDoor {
    fn command(
        &self,
        _id: &contract::CommandId,
        name: &str,
        text: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
        if name == "known" {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_owned(), text.to_owned()));
            Ok(Box::new(|| {}))
        } else {
            Err(contract::inbox::Rejection {
                code: ErrorCode::UnknownCommand,
                message: format!("`{name}` names no extension command."),
            })
        }
    }

    fn seal(&self) {}
}

struct HeldDoor {
    entered: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl contract::extension::ExtensionDoor for HeldDoor {
    fn command(
        &self,
        _id: &contract::CommandId,
        name: &str,
        _text: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
        if name != "slow" {
            return Err(contract::inbox::Rejection {
                code: ErrorCode::UnknownCommand,
                message: format!("`{name}` names no extension command."),
            });
        }
        self.entered.send(()).unwrap_or(());
        let rx = self
            .release
            .lock()
            .unwrap()
            .take()
            .expect("one slow command");
        // The release runs only after `command_accepted` is on the queue;
        // the test releases the waiter then. The sender lives only in the
        // test, so unwinding drops it and disconnects this wait.
        Ok(Box::new(move || {
            let _ = rx.recv().ok();
        }))
    }

    fn seal(&self) {}
}

#[test]
fn command_accepted_arrives_before_run_starts_and_text_absent_is_empty() {
    // `command_accepted` is on the client's stream before the fake's release
    // is called; `text` absent is `""`.
    let opened = Opened::open(vec![]);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let door = Arc::new(HeldDoor {
        entered: entered_tx,
        release: Mutex::new(Some(release_rx)),
    });
    opened.session.extensions(door.clone());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_1","command":"command","args":{"name":"slow"}}"#,
            );
            let accepted = response(&client, "c_1");
            assert_eq!(kind(&accepted), "command_accepted");
            // The fake's release blocks until the test lets it go; the
            // acceptance above arrived first, which is the order asserted.
            Deadline::after(DEADLINE)
                .recv(&entered_rx)
                .expect("admitted");
            release_tx.send(()).unwrap();
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn command_ids_are_admitted_once_across_connections() {
    // The same id sent again on the same connection, and again on a second
    // connection after the first disconnects, is rejected `duplicate_command`
    // and the fake's `command` is called once. A rejected `command` resent
    // with the same id after the name exists is admitted.
    let opened = Opened::open(vec![]);
    let door = Arc::new(FakeDoor::new());
    opened.session.extensions(door.clone());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_1","command":"command","args":{"name":"known","text":"hi"}}"#,
            );
            let accepted = response(&client, "c_1");
            assert_eq!(kind(&accepted), "command_accepted");
            send(
                &client,
                r#"{"id":"c_1","command":"command","args":{"name":"known","text":"hi"}}"#,
            );
            let duplicate = response(&client, "c_1");
            assert_eq!(rejection(&duplicate).0, "duplicate_command");
            drop(client);
            let second = Client::connect(&socket).unwrap();
            subscribe(&second, "c_sub2", "full");
            send(
                &second,
                r#"{"id":"c_1","command":"command","args":{"name":"known","text":"hi"}}"#,
            );
            let retransmit = response(&second, "c_1");
            assert_eq!(rejection(&retransmit).0, "duplicate_command");
            // Unknown name rejects without recording the id...
            send(
                &second,
                r#"{"id":"c_2","command":"command","args":{"name":"nope"}}"#,
            );
            let unknown = response(&second, "c_2");
            assert_eq!(
                rejection(&unknown),
                ("unknown_command", "`nope` names no extension command.")
            );
            // ...and missing `name` rejects `invalid_arguments`.
            send(&second, r#"{"id":"c_3","command":"command","args":{}}"#);
            let missing = response(&second, "c_3");
            assert_eq!(
                rejection(&missing),
                ("invalid_arguments", UNFIT),
                "a missing name does not fit the command"
            );
            assert_eq!(door.calls.lock().unwrap().len(), 1);
            assert_eq!(
                door.calls.lock().unwrap()[0],
                ("known".to_owned(), "hi".to_owned())
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn command_without_a_door_rejects_unknown_command() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_1","command":"command","args":{"name":"known"}}"#,
            );
            let rejected = response(&client, "c_1");
            assert_eq!(
                rejection(&rejected),
                ("unknown_command", "`known` names no extension command.")
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn quiesce_seals_the_door() {
    struct Sealed {
        sealed: AtomicBool,
    }
    impl contract::extension::ExtensionDoor for Sealed {
        fn command(
            &self,
            _id: &contract::CommandId,
            _name: &str,
            _text: &str,
        ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
            Err(contract::inbox::Rejection {
                code: ErrorCode::UnknownCommand,
                message: "none".into(),
            })
        }
        fn seal(&self) {
            self.sealed.store(true, Ordering::SeqCst);
        }
    }
    let opened = Opened::open(vec![]);
    let door = Arc::new(Sealed {
        sealed: AtomicBool::new(false),
    });
    opened.session.extensions(door.clone());
    opened.session.quiesce();
    assert!(door.sealed.load(Ordering::SeqCst));
    opened.close();
}

/// How long a pasted-image test waits for the fake to start and for the
/// rejection after the stopper. A wait that reaches it fails the test.
const PASTE_LIMIT: Duration = Duration::from_secs(10);

struct OkImage;

impl contract::images::Images for OkImage {
    fn process(
        &self,
        _bytes: &[u8],
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<contract::provider::ImageRef, contract::images::ImageError> {
        Ok(contract::provider::ImageRef {
            path: "artifacts/i_test.png".into(),
            mime_type: "image/png".into(),
            width: 3,
            height: 2,
        })
    }
}

struct FailingImage;

impl contract::images::Images for FailingImage {
    fn process(
        &self,
        _bytes: &[u8],
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<contract::provider::ImageRef, contract::images::ImageError> {
        Err(contract::images::ImageError::Failed("boom".into()))
    }
}

struct BlockingImage {
    entered: Mutex<Option<mpsc::Sender<()>>>,
}

struct PasteWake {
    tx: Mutex<Option<mpsc::Sender<()>>>,
}

impl contract::clock::Wake for PasteWake {
    fn wake(&self) {
        if let Some(tx) = self.tx.lock().unwrap().take()
            && let Ok(()) = tx.send(())
        {}
        {}
    }
}

impl contract::images::Images for BlockingImage {
    #[track_caller]
    fn process(
        &self,
        _bytes: &[u8],
        cancel: &dyn contract::tool::Cancel,
    ) -> Result<contract::provider::ImageRef, contract::images::ImageError> {
        if let Some(tx) = self.entered.lock().unwrap().take()
            && let Ok(()) = tx.send(())
        {}
        {}
        if cancel.is_cancelled() {
            return Err(contract::images::ImageError::Cancelled);
        }
        let (tx, rx) = mpsc::channel();
        let waker: Arc<dyn contract::clock::Wake> = Arc::new(PasteWake {
            tx: Mutex::new(Some(tx)),
        });
        cancel.subscribe(Arc::downgrade(&waker));
        if cancel.is_cancelled() {
            return Err(contract::images::ImageError::Cancelled);
        }
        if let Ok(()) = Deadline::after(PASTE_LIMIT).recv(&rx) {}
        if cancel.is_cancelled() {
            return Err(contract::images::ImageError::Cancelled);
        }
        Err(contract::images::ImageError::Failed(
            "the test image was never cancelled".into(),
        ))
    }
}

#[test]
fn prompt_with_an_image_is_delivered_with_the_processed_part() {
    let opened = Opened::open(vec![]);
    opened.session.images(Arc::new(OkImage));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"look"},{"type":"image","data":"YQ==","mime_type":"image/png"}]}}"#,
            );
            let delivery = Deadline::after(DEADLINE).recv(&inbox).expect("the prompt is delivered");
            let Delivery::Prompt(message, ack) = delivery else {
                panic!("a prompt: {delivery:?}");
            };
            assert_eq!(
                message.content,
                vec![
                    ContentPart::Text { text: "look".into() },
                    ContentPart::Image {
                        path: "artifacts/i_test.png".into(),
                        mime_type: "image/png".into(),
                        width: 3,
                        height: 2,
                    },
                ]
            );
            ack.0(Ok(None));
            let line = response(&client, "c_prompt");
            assert_eq!(kind(&line), "command_accepted", "{line}");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn a_full_subscriber_after_an_extension_ui_line_receives_it_as_seed() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            log.emit(&Event::ExtensionUi(contract::events::ExtensionUi {
                extension: "fiber.test/a".to_owned(),
                ui: contract::events::Ui::Status {
                    status: "syncing".to_owned(),
                },
            }));
            let full = Client::connect(&socket).unwrap();
            subscribe(&full, "c_full", "full");
            let lines = until(&full, |line| kind(line) == "extension_ui");
            assert!(
                lines
                    .iter()
                    .any(|line| line["payload"]["extension"] == "fiber.test/a"
                        && line["payload"]["status"] == "syncing"),
                "a late `full` subscriber receives the kept ui line as seed"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn steer_with_an_image_is_delivered_with_the_processed_part() {
    let opened = Opened::open(vec![]);
    opened.session.images(Arc::new(OkImage));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_steer","command":"steer","args":{"content":[{"type":"image","data":"YQ==","mime_type":"image/png"}]}}"#,
            );
            let delivery = Deadline::after(DEADLINE).recv(&inbox).expect("the steer is delivered");
            let Delivery::Steer(message, ack) = delivery else {
                panic!("a steer: {delivery:?}");
            };
            assert_eq!(
                message.content,
                vec![ContentPart::Image {
                    path: "artifacts/i_test.png".into(),
                    mime_type: "image/png".into(),
                    width: 3,
                    height: 2,
                },]
            );
            ack.0(Ok(None));
            let line = response(&client, "c_steer");
            assert_eq!(kind(&line), "command_accepted", "{line}");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn prompt_with_a_failed_image_is_rejected_io_failed() {
    let opened = Opened::open(vec![]);
    opened.session.images(Arc::new(FailingImage));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"image","data":"YQ==","mime_type":"image/png"}]}}"#,
            );
            let line = response(&client, "c_prompt");
            assert_eq!(
                rejection(&line),
                ("io_failed", "Image 1 could not be processed: boom")
            );
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "a rejected prompt reaches no inbox"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn the_stopper_cancels_a_pasted_image_in_flight() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let opened = Opened::open(vec![]);
    opened.session.images(Arc::new(BlockingImage {
        entered: Mutex::new(Some(entered_tx)),
    }));
    let socket = opened.socket.clone();
    let stopper = opened.session.stopper();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"image","data":"YQ==","mime_type":"image/png"}]}}"#,
            );
            Deadline::after(PASTE_LIMIT).recv(&entered_rx)
                .expect("the image started");
            stopper();
            let line = response(&client, "c_prompt");
            assert_eq!(
                rejection(&line),
                ("closing", "Image 1 was not processed: the session is closing.")
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_now_starts_the_shutdown_and_sends_the_loop_nothing() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let (hook_tx, hook_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    opened.session.close_now(Arc::new(move || {
        hook_tx.send(()).unwrap();
        Deadline::start().recv_or_fail(&release_rx.lock().unwrap(), "the test releases the hook");
    }));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_close_now","command":"close","args":{"now":true}}"#,
            );
            Deadline::after(DEADLINE)
                .recv(&hook_rx)
                .expect("the hook runs");
            // The answer was queued before the hook ran: if the order were
            // reversed, this read would wait out its deadline and fail.
            let answer = response(&client, "c_close_now");
            assert_eq!(kind(&answer), "command_accepted", "{answer}");
            assert_eq!(command_id(&answer), Some("c_close_now"));
            release_tx.send(()).unwrap();
            assert!(hook_rx.try_recv().is_err(), "the hook ran once");
            send(&client, r#"{"id":"c_tools","command":"tools"}"#);
            let tools = response(&client, "c_tools");
            assert_eq!(kind(&tools), "command_accepted", "{tools}");
            // The reader handles lines in order, so every line before the
            // tools answer has been handled: no `Close` reached the loop.
            assert!(inbox.try_recv().is_err(), "no Close reached the loop");
            send(
                &client,
                r#"{"id":"c_close_now_2","command":"close","args":{"now":true}}"#,
            );
            let second = response(&client, "c_close_now_2");
            assert_eq!(kind(&second), "command_accepted", "{second}");
            Deadline::after(DEADLINE)
                .recv(&hook_rx)
                .expect("a repeat is answered too");
            release_tx.send(()).unwrap();
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_without_now_starts_no_shutdown() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let (hook_tx, hook_rx) = mpsc::channel::<()>();
    opened.session.close_now(Arc::new(move || {
        hook_tx.send(()).unwrap();
    }));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_close_bare","command":"close"}"#);
            send(
                &client,
                r#"{"id":"c_close_false","command":"close","args":{"now":false}}"#,
            );
            let wait = Deadline::after(DEADLINE);
            assert_eq!(take(&inbox, &wait), "close");
            assert_eq!(take(&inbox, &wait), "close");
            for id in ["c_close_bare", "c_close_false"] {
                let answer = response(&client, id);
                assert_eq!(kind(&answer), "command_accepted", "{answer}");
            }
            send(&client, r#"{"id":"c_tools","command":"tools"}"#);
            let tools = response(&client, "c_tools");
            assert_eq!(kind(&tools), "command_accepted", "{tools}");
            // The reader handles lines in order, so a hook call would have
            // come before the tools answer.
            assert!(hook_rx.try_recv().is_err(), "no shutdown started");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_now_with_no_shutdown_wired_is_a_plain_close() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_close_now","command":"close","args":{"now":true}}"#,
            );
            assert_eq!(take(&inbox, &Deadline::after(DEADLINE)), "close");
            let answer = response(&client, "c_close_now");
            assert_eq!(kind(&answer), "command_accepted", "{answer}");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn close_now_with_a_non_boolean_is_invalid_and_starts_nothing() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let (hook_tx, hook_rx) = mpsc::channel::<()>();
    opened.session.close_now(Arc::new(move || {
        hook_tx.send(()).unwrap();
    }));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_close_now","command":"close","args":{"now":"yes"}}"#,
            );
            let answer = response(&client, "c_close_now");
            assert_eq!(rejection(&answer), ("invalid_arguments", UNFIT));
            assert_eq!(command_id(&answer), Some("c_close_now"));
            assert!(inbox.try_recv().is_err(), "no Close reached the loop");
            assert!(hook_rx.try_recv().is_err(), "no shutdown started");
            Ok(())
        })
        .unwrap();
    opened.close();
}

const SENDER: &str = "s_0123456789abcdef";
const PARENT: &str = "s_parent00000001";
const MESSAGE_CLOSED: &str = "The session is closing and takes no session message.";

fn message_line(id: &str, from: &str, text: &str) -> String {
    format!(
        r#"{{"id":"{id}","command":"message","args":{{"from_session_id":"{from}","text":"{text}"}}}}"#
    )
}

fn message_line_with_parent(id: &str, from: &str, parent: &str, text: &str) -> String {
    format!(
        r#"{{"id":"{id}","command":"message","args":{{"from_session_id":"{from}","parent_session_id":"{parent}","text":"{text}"}}}}"#
    )
}

/// A sender log under the session's home holding `session_named` `name`,
/// when `name` is one.
fn named_sender(opened: &Opened, sender: &str, name: Option<&str>) {
    let timed = Arc::clone(&opened.clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Log::create(&opened.sessions, SessionId(sender.into()), timed).unwrap();
    if let Some(name) = name {
        log.append(
            &Event::SessionNamed(SessionNamed {
                name: Some(name.into()),
                by: NamedBy::Model,
            }),
            None,
            None,
        )
        .unwrap();
    }
}

#[test]
fn message_is_answered_once_the_loop_takes_it_and_carries_the_senders_name() {
    let opened = Opened::open(vec![]);
    named_sender(&opened, SENDER, Some("builder"));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                &message_line("c_m1", SENDER, "Tests pass on main."),
            );
            send(
                &client,
                &message_line_with_parent("c_m2", SENDER, PARENT, "A child reports."),
            );
            let Delivery::Message(message, ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the message is delivered")
            else {
                panic!("the message is delivered as a message");
            };
            assert_eq!(message.from_session_id.0, SENDER);
            assert_eq!(message.command_id.0, "c_m1");
            assert_eq!(message.text, "Tests pass on main.");
            assert_eq!(message.name.as_deref(), Some("builder"));
            assert_eq!(message.parent_session_id, None);
            // Answered by the take, never at admission.
            ack.0(Ok(None));
            let accepted = response(&client, "c_m1");
            assert_eq!(kind(&accepted), "command_accepted");
            assert_eq!(command_id(&accepted), Some("c_m1"));
            let Delivery::Message(child, child_ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the child message is delivered")
            else {
                panic!("the child message is delivered as a message");
            };
            assert_eq!(child.command_id.0, "c_m2");
            assert_eq!(
                child.parent_session_id.as_ref().map(|id| id.0.as_str()),
                Some(PARENT),
                "the sender's parent reaches the take"
            );
            child_ack.0(Ok(None));
            assert_eq!(kind(&response(&client, "c_m2")), "command_accepted");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn message_from_an_unknown_or_odd_sender_is_delivered_with_no_name() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                &message_line("c_m1", "s_ffffffffffffffff", "Hello."),
            );
            send(&client, &message_line("c_m2", "../x", "Hello."));
            for (id, answer) in [("c_m1", "c_m1"), ("c_m2", "c_m2")] {
                let Delivery::Message(message, ack) = Deadline::after(DEADLINE)
                    .recv(&inbox)
                    .expect("the message is delivered")
                else {
                    panic!("the message is delivered as a message");
                };
                assert_eq!(message.command_id.0, id);
                assert_eq!(message.name, None);
                // Answered by the take, never at admission.
                ack.0(Ok(None));
                assert_eq!(kind(&response(&client, answer)), "command_accepted");
            }
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn message_to_itself_reads_its_own_name() {
    let opened = Opened::open(vec![]);
    opened
        .log
        .append(
            &Event::SessionNamed(SessionNamed {
                name: Some("selfish".into()),
                by: NamedBy::Person,
            }),
            None,
            None,
        )
        .unwrap();
    let socket = opened.socket.clone();
    let own = opened.id.0.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &message_line("c_m1", &own, "Note to self."));
            let Delivery::Message(message, ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the message is delivered")
            else {
                panic!("the message is delivered as a message");
            };
            assert_eq!(message.from_session_id.0, own);
            assert_eq!(message.name.as_deref(), Some("selfish"));
            ack.0(Ok(None));
            assert_eq!(kind(&response(&client, "c_m1")), "command_accepted");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn message_after_close_is_rejected_closing_and_reaches_no_inbox() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, r#"{"id":"c_close","command":"close"}"#);
            // The close answers once the inbox takes it.
            let Delivery::Close(ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the close is delivered")
            else {
                panic!("the close is delivered as a close");
            };
            ack.0(Ok(None));
            assert_eq!(kind(&response(&client, "c_close")), "command_accepted");
            send(&client, &message_line("c_m2", SENDER, "Too late."));
            let rejected = response(&client, "c_m2");
            assert_eq!(rejection(&rejected), ("closing", MESSAGE_CLOSED));
            assert_eq!(command_id(&rejected), Some("c_m2"));
            send(&client, r#"{"id":"c_tools","command":"tools"}"#);
            assert_eq!(kind(&response(&client, "c_tools")), "command_accepted");
            assert!(
                inbox.try_recv().is_err(),
                "the refused message reached no inbox"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn message_after_close_now_is_rejected_closing() {
    let opened = Opened::open(vec![]);
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&shutdowns);
    opened.session.close_now(Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    }));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_close","command":"close","args":{"now":true}}"#,
            );
            assert_eq!(kind(&response(&client, "c_close")), "command_accepted");
            send(&client, &message_line("c_m1", SENDER, "Too late."));
            let rejected = response(&client, "c_m1");
            assert_eq!(rejection(&rejected), ("closing", MESSAGE_CLOSED));
            assert!(
                inbox.try_recv().is_err(),
                "a `close` with `now` delivers nothing"
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    opened.close();
}

#[test]
fn message_after_the_stopper_is_rejected_closing() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let stopper = opened.session.stopper();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            stopper();
            send(&client, &message_line("c_m1", SENDER, "Too late."));
            let rejected = response(&client, "c_m1");
            assert_eq!(rejection(&rejected), ("closing", MESSAGE_CLOSED));
            assert!(
                matches!(inbox.try_recv(), Ok(Delivery::Cancelled)),
                "only the stopper's wake reached the inbox"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

/// `docs/invocation.md`, "Lifecycle": a command after `close` and before `fiber_exited` still gets `closing`, except a `close` with `now`, which is accepted and upgrades the close.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
#[test]
fn message_during_quiesce_is_rejected_closing() {
    struct Sealed {
        entered: Mutex<Option<mpsc::Sender<()>>>,
    }
    impl contract::extension::ExtensionDoor for Sealed {
        fn command(
            &self,
            _id: &contract::CommandId,
            _name: &str,
            _text: &str,
        ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
            Err(contract::inbox::Rejection {
                code: ErrorCode::UnknownCommand,
                message: "none".into(),
            })
        }
        fn seal(&self) {
            if let Some(tx) = self.entered.lock().expect("the seal lock").take() {
                tx.send(()).unwrap_or(());
            }
        }
    }
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let session = &opened.session;
    let (sealed_tx, sealed_rx) = mpsc::channel();
    let door = Arc::new(Sealed {
        entered: Mutex::new(Some(sealed_tx)),
    });
    opened.session.extensions(door);
    let (hook_tx, hook_rx) = mpsc::channel();
    opened.session.close_now(Arc::new(move || {
        hook_tx.send(()).unwrap_or(());
    }));
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            // `c_hold` stays owed: the take below never answers it, so
            // quiesce parks in its answer wait.
            send(
                &client,
                &message_line("c_hold", SENDER, "Hold the quiesce."),
            );
            let Delivery::Message(held, hold_ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the hold message is delivered")
            else {
                panic!("the hold message is delivered as a message");
            };
            assert_eq!(held.command_id.0, "c_hold");
            thread::scope(|scope| {
                let (done_tx, done_rx) = mpsc::channel();
                scope.spawn(move || {
                    session.quiesce();
                    done_tx.send(()).unwrap_or(());
                });
                // Quiesce seals after marking closed, so the seal means a
                // later line is already past the close.
                Deadline::after(DEADLINE)
                    .recv(&sealed_rx)
                    .expect("quiesce started");
                // The readers keep reading through the wait: a line read
                // after the close is answered `closing` at once.
                send(&client, &message_line("c_m1", SENDER, "Too late."));
                let rejected = response(&client, "c_m1");
                assert_eq!(rejection(&rejected), ("closing", MESSAGE_CLOSED));
                // A `close` with `now` in the same window is accepted and
                // upgrades the close.
                send(
                    &client,
                    r#"{"id":"c_now","command":"close","args":{"now":true}}"#,
                );
                let upgraded = response(&client, "c_now");
                assert_eq!(kind(&upgraded), "command_accepted", "{upgraded}");
                assert_eq!(command_id(&upgraded), Some("c_now"));
                Deadline::after(DEADLINE)
                    .recv(&hook_rx)
                    .expect("the shutdown hook ran");
                // Both answers landed while quiesce was still parked on the
                // held message, so before the reader stop and the cut.
                assert!(
                    done_rx.try_recv().is_err(),
                    "quiesce returned before the held answer was released"
                );
                // Release the hold: the acknowledgement is written before it
                // is marked answered, so quiesce returns with every answer
                // on the connection.
                hold_ack.0(Ok(None));
                Deadline::after(DEADLINE)
                    .recv(&done_rx)
                    .expect("quiesce returns once the hold is answered");
            });
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client outlives the run");
    opened.close();
    // End of file carries exactly one answer for the held line: its own.
    let mut lines = Vec::new();
    while let Some(line) = client.recv(Deadline::after(DEADLINE)) {
        lines.push(line);
    }
    let held_answers: Vec<&Value> = lines
        .iter()
        .filter(|line| command_id(line) == Some("c_hold"))
        .collect();
    assert_eq!(
        held_answers.len(),
        1,
        "one answer for the held line: {lines:?}"
    );
    assert_eq!(kind(held_answers[0]), "command_accepted", "{lines:?}");
}

#[test]
fn message_after_the_loop_closed_admission_is_rejected_closing() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let admission = opened.session.admission();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            // As the loop's idle exit does: look a last time, find nothing,
            // and close admission.
            admission.close_unless(|| None::<()>);
            send(&client, &message_line("c_m1", SENDER, "Too late."));
            let rejected = response(&client, "c_m1");
            assert_eq!(rejection(&rejected), ("closing", MESSAGE_CLOSED));
            assert!(
                inbox.try_recv().is_err(),
                "the refused message reached no inbox"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn message_to_an_ask_session_is_rejected_closing() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .ask("hi".into(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(&client, &message_line("c_m1", SENDER, "Too late."));
            let rejected = response(&client, "c_m1");
            assert_eq!(rejection(&rejected), ("closing", MESSAGE_CLOSED));
            // `ask` queued its prompt and its close before any client.
            let Delivery::Prompt(_, _) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the prompt is delivered")
            else {
                panic!("the prompt is delivered as a prompt");
            };
            let Delivery::Close(_) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the close is delivered")
            else {
                panic!("the close is delivered as a close");
            };
            assert!(inbox.try_recv().is_err(), "nothing follows close");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn message_once_the_inbox_is_gone_is_rejected_ended() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            drop(inbox);
            send(&client, &message_line("c_m1", SENDER, "Nobody home."));
            let rejected = response(&client, "c_m1");
            assert_eq!(rejection(&rejected), ("closing", ENDED));
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn message_before_subscribe_is_not_subscribed_and_without_text_is_invalid_arguments() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            send(&client, &message_line("c_m1", SENDER, "First."));
            let rejected = response(&client, "c_m1");
            assert_eq!(rejection(&rejected), ("not_subscribed", NOT_SUBSCRIBED));
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_bad","command":"message","args":{"from_session_id":"s_x"}}"#,
            );
            let bad = response(&client, "c_bad");
            assert_eq!(rejection(&bad), ("invalid_arguments", UNFIT));
            assert!(
                inbox.try_recv().is_err(),
                "neither refused message reached the inbox"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}
fn spilled_process(code: Option<i32>) -> Process {
    Process {
        exit_code: code,
        signal: None,
        timed_out: false,
    }
}

fn spilled_ok(
    head: &str,
    tail: &str,
    removed: u64,
    path: PathBuf,
    size: u64,
    competing: &str,
    code: Option<i32>,
) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: competing.to_owned(),
        }],
        process: Some(spilled_process(code)),
        spilled: Some(Spilled {
            head_bytes: head.len() as u64,
            tail_bytes: tail.len() as u64,
            head: head.to_owned(),
            tail: tail.to_owned(),
            removed,
            file: Ok(SpillFile { path, size }),
        }),
        ..Output::default()
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one spilled fixture builds one Output"
)]
fn spilled_unsaved(
    head: &str,
    tail: &str,
    removed: u64,
    message: String,
    left: Option<PathBuf>,
    process: Option<Process>,
    error: Option<Failure>,
    competing: &str,
) -> Output {
    let mut content = vec![ContentPart::Text {
        text: competing.to_owned(),
    }];
    if competing.is_empty() {
        content.clear();
    }
    Output {
        content,
        error,
        process,
        spilled: Some(Spilled {
            head_bytes: head.len() as u64,
            tail_bytes: tail.len() as u64,
            head: head.to_owned(),
            tail: tail.to_owned(),
            removed,
            file: Err(Unsaved { message, left }),
        }),
        ..Output::default()
    }
}

fn connect_bounded(socket: &Path, deadline: Deadline) -> Client {
    let owned = socket.to_owned();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || match tx.send(Client::connect(&owned)) {
        Ok(()) | Err(_) => {}
    });
    deadline
        .recv_or_fail(&rx, "a connection to the session socket")
        .unwrap()
}

fn send_bounded(client: &Client, line: &str, deadline: Deadline) {
    client
        .send_by(line, &|| deadline.left())
        .expect("the command line was written");
}

fn subscribe_bounded(
    client: &Client,
    id: &str,
    deadline: Deadline,
    stream: &mut Vec<Value>,
) -> Value {
    send_bounded(
        client,
        &format!(r#"{{"id":"{id}","command":"subscribe","args":{{"level":"full"}}}}"#),
        deadline,
    );
    let lines = until(client, |line| command_id(line) == Some(id));
    let ack = lines
        .last()
        .expect("the subscribe acknowledgement arrived")
        .clone();
    assert_eq!(kind(&ack), "command_accepted", "{ack}");
    assert_eq!(command_id(&ack), Some(id));
    stream.extend(lines);
    ack
}

fn shell_answer(client: &Client, id: &str, deadline: Deadline, stream: &mut Vec<Value>) -> Value {
    let lines = until(client, |line| command_id(line) == Some(id));
    stream.extend(lines);
    // A controlled completion boundary: the trailing command proves no second
    // shell answer follows, so a duplicate shell acknowledgement fails below
    // (`docs/testing.md`, "Event streams").
    let sentinel = format!("c_after_{id}");
    send_bounded(
        client,
        &format!(r#"{{"id":"{sentinel}","command":"tools"}}"#),
        deadline,
    );
    let trail = until(client, |line| command_id(line) == Some(sentinel.as_str()));
    stream.extend(trail);
    let matching: Vec<&Value> = stream
        .iter()
        .filter(|line| command_id(line) == Some(id))
        .collect();
    assert_eq!(matching.len(), 1, "exactly one answer for {id}: {stream:?}");
    matching[0].clone()
}

/// The complete ordered event kinds on one subscribed connection: the
/// subscribe acknowledgement, then the shell answer, then the trailing
/// boundary command, with the independent attach line set aside and its
/// count checked, so a duplicated, missing or reordered event fails
/// (`docs/testing.md`, "Event streams").
fn assert_shell_stream(stream: &[Value], sub_id: &str, shell_id: &str, shell_kind: &str) {
    let sentinel = format!("c_after_{shell_id}");
    let main: Vec<&Value> = stream
        .iter()
        .filter(|line| kind(line) != "clients")
        .collect();
    assert_eq!(
        main.iter().map(|line| kind(line)).collect::<Vec<_>>(),
        ["command_accepted", shell_kind, "command_accepted"],
        "the complete ordered event kinds: {stream:?}"
    );
    for id in [sub_id, shell_id, sentinel.as_str()] {
        assert_eq!(
            stream
                .iter()
                .filter(|line| command_id(line) == Some(id))
                .count(),
            1,
            "exactly one acknowledgement for {id}: {stream:?}"
        );
    }
    let attach: Vec<&Value> = stream
        .iter()
        .filter(|line| kind(line) == "clients")
        .collect();
    assert_eq!(attach.len(), 1, "one independent attach line: {stream:?}");
    assert_eq!(
        attach[0]["payload"]["count"].as_u64(),
        Some(1),
        "the attach counts its own connection: {stream:?}"
    );
}

fn artifacts_entries(dir: &Path) -> Vec<String> {
    let artifacts = dir.join("artifacts");
    let mut names: Vec<String> = fs::read_dir(&artifacts)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn shell_spilled_success_returns_original_artifact() {
    let opened = Opened::open(vec![]);
    let path = opened.dir.join("artifacts/s_test.txt");
    let saved = b"original spilled bytes, kept whole";
    fs::write(&path, saved).unwrap();
    let size = u64::try_from(saved.len()).unwrap();
    opened.session.shell(fixed(
        spilled_ok(
            "HEAD",
            "TAIL",
            5,
            path.clone(),
            size,
            "competing, ignored",
            Some(0),
        ),
        Bound { start: 1, end: 1 },
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    let absolute = path.display().to_string();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let client = connect_bounded(&socket, deadline);
            let mut stream = Vec::new();
            let ack = subscribe_bounded(&client, "c_sub", deadline, &mut stream);
            assert_eq!(command_id(&ack), Some("c_sub"));
            send_bounded(&client, &shell_line("c_big", "cat big"), deadline);
            let line = shell_answer(&client, "c_big", deadline, &mut stream);
            assert_shell_stream(&stream, "c_sub", "c_big", "command_accepted");
            assert_eq!(kind(&line), "command_accepted", "{line}");
            let result = &line["payload"]["result"];
            let notice =
                format!("[5 bytes cut. The full output is in {absolute}; read it with `read`.]");
            assert_eq!(result["output"], format!("HEAD\n{notice}\nTAIL"));
            assert_eq!(result["artifact"], "artifacts/s_test.txt");
            assert_eq!(result["process"]["exit_code"], 0);
            assert_eq!(result["process"]["timed_out"], false);
            assert_eq!(fs::read(dir.join("artifacts/s_test.txt")).unwrap(), saved);
            assert_eq!(artifacts_entries(&dir), vec!["s_test.txt".to_owned()]);
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_spilled_zero_removed_joins_without_notice() {
    let opened = Opened::open(vec![]);
    let path = opened.dir.join("artifacts/s_zero.txt");
    let saved = "héllo wörld".as_bytes().to_vec();
    fs::write(&path, &saved).unwrap();
    let size = u64::try_from(saved.len()).unwrap();
    opened.session.shell(fixed(
        spilled_ok("héllo ", "wörld", 0, path.clone(), size, "ignored", Some(0)),
        Bound { start: 1, end: 1 },
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let client = connect_bounded(&socket, deadline);
            let mut stream = Vec::new();
            subscribe_bounded(&client, "c_sub", deadline, &mut stream);
            send_bounded(&client, &shell_line("c_zero", "cat zero"), deadline);
            let line = shell_answer(&client, "c_zero", deadline, &mut stream);
            assert_shell_stream(&stream, "c_sub", "c_zero", "command_accepted");
            assert_eq!(kind(&line), "command_accepted", "{line}");
            let result = &line["payload"]["result"];
            assert_eq!(result["output"], "héllo wörld");
            assert!(
                !result["output"].as_str().unwrap().contains("bytes cut"),
                "{}",
                result["output"]
            );
            assert_eq!(result["artifact"], "artifacts/s_zero.txt");
            assert_eq!(fs::read(dir.join("artifacts/s_zero.txt")).unwrap(), saved);
            assert_eq!(artifacts_entries(&dir), vec!["s_zero.txt".to_owned()]);
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_spilled_outside_session_stays_absolute() {
    let opened = Opened::open(vec![]);
    let elsewhere = Temp::new();
    let path = elsewhere.0.join("s_out.txt");
    let saved = b"raw output bytes";
    fs::write(&path, saved).unwrap();
    let size = u64::try_from(saved.len()).unwrap();
    let absolute = path.display().to_string();
    opened.session.shell(fixed(
        spilled_ok("raw ", "bytes", 6, path.clone(), size, "ignored", Some(0)),
        Bound { start: 1, end: 1 },
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let client = connect_bounded(&socket, deadline);
            let mut stream = Vec::new();
            subscribe_bounded(&client, "c_sub", deadline, &mut stream);
            send_bounded(&client, &shell_line("c_out", "cat out"), deadline);
            let line = shell_answer(&client, "c_out", deadline, &mut stream);
            assert_shell_stream(&stream, "c_sub", "c_out", "command_accepted");
            assert_eq!(kind(&line), "command_accepted", "{line}");
            let result = &line["payload"]["result"];
            assert_eq!(result["artifact"], absolute.as_str());
            let notice =
                format!("[6 bytes cut. The full output is in {absolute}; read it with `read`.]");
            assert_eq!(result["output"], format!("raw \n{notice}\nbytes"));
            assert_eq!(fs::read(&path).unwrap(), saved);
            assert!(
                artifacts_entries(&dir).is_empty(),
                "no file under the log's artifacts: {:?}",
                artifacts_entries(&dir)
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_unsaved_without_left_keeps_process_and_notice() {
    let opened = Opened::open(vec![]);
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let notice = format!("[92 bytes cut. The full output could not be saved: {message}.]");
    opened.session.shell(fixed(
        spilled_unsaved(
            "HHHH",
            "TTTT",
            92,
            message,
            None,
            Some(spilled_process(Some(2))),
            Some(Failure {
                code: ErrorCode::NonzeroExit,
                message: "Exit code 2.".to_owned(),
                retry_after_ms: None,
                provider: None,
            }),
            "competing, ignored",
        ),
        Bound::DEFAULT,
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let client = connect_bounded(&socket, deadline);
            let mut stream = Vec::new();
            subscribe_bounded(&client, "c_sub", deadline, &mut stream);
            send_bounded(&client, &shell_line("c_fail", "false"), deadline);
            let line = shell_answer(&client, "c_fail", deadline, &mut stream);
            assert_shell_stream(&stream, "c_sub", "c_fail", "command_accepted");
            assert_eq!(kind(&line), "command_accepted", "{line}");
            let result = &line["payload"]["result"];
            assert_eq!(result["output"], format!("HHHH\n{notice}\nTTTT"));
            assert!(result.get("artifact").is_none(), "{result}");
            assert_eq!(result["process"]["exit_code"], 2);
            assert_eq!(result["process"]["timed_out"], false);
            assert!(artifacts_entries(&dir).is_empty());
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_unsaved_without_left_zero_removed_still_notices() {
    let opened = Opened::open(vec![]);
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let notice = format!("[0 bytes cut. The full output could not be saved: {message}.]");
    opened.session.shell(fixed(
        spilled_unsaved(
            "HHHH",
            "TTTT",
            0,
            message,
            None,
            Some(spilled_process(Some(0))),
            None,
            "",
        ),
        Bound::DEFAULT,
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let client = connect_bounded(&socket, deadline);
            let mut stream = Vec::new();
            subscribe_bounded(&client, "c_sub", deadline, &mut stream);
            send_bounded(&client, &shell_line("c_zero_fail", "true"), deadline);
            let line = shell_answer(&client, "c_zero_fail", deadline, &mut stream);
            assert_shell_stream(&stream, "c_sub", "c_zero_fail", "command_accepted");
            assert_eq!(kind(&line), "command_accepted", "{line}");
            let result = &line["payload"]["result"];
            assert_eq!(result["output"], format!("HHHH\n{notice}\nTTTT"));
            assert!(result.get("artifact").is_none(), "{result}");
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_unsaved_left_is_removed_before_answer() {
    let opened = Opened::open(vec![]);
    let left = opened.dir.join("artifacts/s_left.txt");
    fs::write(&left, b"partial").unwrap();
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let notice = format!("[92 bytes cut. The full output could not be saved: {message}.]");
    opened.session.shell(fixed(
        spilled_unsaved(
            "HHHH",
            "TTTT",
            92,
            message,
            Some(left.clone()),
            Some(spilled_process(Some(0))),
            None,
            "competing, ignored",
        ),
        Bound::DEFAULT,
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let client = connect_bounded(&socket, deadline);
            let mut stream = Vec::new();
            subscribe_bounded(&client, "c_sub", deadline, &mut stream);
            send_bounded(&client, &shell_line("c_left", "cat left"), deadline);
            let line = shell_answer(&client, "c_left", deadline, &mut stream);
            assert_shell_stream(&stream, "c_sub", "c_left", "command_accepted");
            assert_eq!(kind(&line), "command_accepted", "{line}");
            let result = &line["payload"]["result"];
            assert_eq!(result["output"], format!("HHHH\n{notice}\nTTTT"));
            assert!(result.get("artifact").is_none(), "{result}");
            assert!(
                !left.exists(),
                "the partial file is removed before the answer"
            );
            assert!(artifacts_entries(&dir).is_empty());
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_unsaved_left_removal_failure_rejects_io_failed() {
    let opened = Opened::open(vec![]);
    let held = Temp::new();
    let left = held.0.clone();
    assert!(left.is_dir());
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    opened.session.shell(fixed(
        spilled_unsaved(
            "HHHH",
            "TTTT",
            92,
            message,
            Some(left.clone()),
            Some(spilled_process(Some(0))),
            None,
            "",
        ),
        Bound::DEFAULT,
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    let expected = left.display().to_string();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let client = connect_bounded(&socket, deadline);
            let mut stream = Vec::new();
            subscribe_bounded(&client, "c_sub", deadline, &mut stream);
            send_bounded(&client, &shell_line("c_stuck", "cat stuck"), deadline);
            let line = shell_answer(&client, "c_stuck", deadline, &mut stream);
            assert_shell_stream(&stream, "c_sub", "c_stuck", "command_rejected");
            assert_eq!(kind(&line), "command_rejected", "{line}");
            assert_eq!(rejection(&line).0, "io_failed");
            let (_, text) = rejection(&line);
            assert!(
                text.contains(&expected),
                "the message names {expected}: {text}"
            );
            assert!(
                text.contains("could not be removed"),
                "the message names the removal failure: {text}"
            );
            assert!(left.is_dir(), "a directory is not removed as a file");
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn shell_unsaved_left_without_process_still_cleans_up() {
    let opened = Opened::open(vec![]);
    let left = opened.dir.join("artifacts/s_partial.txt");
    fs::write(&left, b"partial").unwrap();
    opened.session.shell(fixed(
        spilled_unsaved(
            "HHHH",
            "TTTT",
            7,
            "could not save the output to /nowhere/s_x.txt: boom".to_owned(),
            Some(left.clone()),
            None,
            Some(Failure {
                code: ErrorCode::InvalidArguments,
                message: "Give one command.".to_owned(),
                retry_after_ms: None,
                provider: None,
            }),
            "",
        ),
        Bound::DEFAULT,
    ));
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let client = connect_bounded(&socket, deadline);
            let mut stream = Vec::new();
            subscribe_bounded(&client, "c_sub", deadline, &mut stream);
            send_bounded(&client, &shell_line("c_noproc", ""), deadline);
            let line = shell_answer(&client, "c_noproc", deadline, &mut stream);
            assert_shell_stream(&stream, "c_sub", "c_noproc", "command_rejected");
            assert_eq!(rejection(&line), ("invalid_arguments", "Give one command."));
            assert!(
                !left.exists(),
                "the partial file is removed before the rejection"
            );
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    opened.close();
}

/// Reopens the session log on a worker bounded by [`DEADLINE`], so the
/// missing-log tests close through the ordinary path after dropping their
/// strong handle.
fn reopened_log(sessions: PathBuf, id: SessionId, clock: Arc<dyn Clock>) -> Arc<Log> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || match tx.send(Log::open(&sessions, id, clock)) {
        Ok(()) | Err(_) => {}
    });
    let log = Deadline::after(DEADLINE)
        .recv_or_fail(&rx, "the log reopened for close")
        .unwrap();
    Arc::new(log)
}

#[test]
fn drive_quota_is_a_built_command() {
    let opened = Opened::open(vec![]);
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                driver.drive(
                    "fiber.test/a",
                    "quota",
                    Map::new(),
                    Ack(Box::new(move |answer| {
                        tx.send(answer).unwrap();
                    })),
                )
            });
            let answer = Deadline::after(DEADLINE).recv_or_fail(&rx, "the quota drive answer");
            assert_eq!(
                answer.unwrap(),
                Some(contract::events::CommandResult::Quota {
                    quota: vec![],
                    notice_at: 80.0
                })
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

fn drive_shell(driver: &Arc<dyn Drive>, command: &str) -> mpsc::Receiver<Answer> {
    let (tx, rx) = mpsc::channel();
    let driver = Arc::clone(driver);
    let mut args = Map::new();
    args.insert("command".to_owned(), Value::String(command.to_owned()));
    // Dispatch acquires registry locks before returning, so it runs on a worker;
    // callers bound the acknowledgement receive under their existing deadline
    // (`docs/testing.md`, "Waits and timeouts").
    thread::spawn(move || {
        driver.drive(
            "fiber.test/a",
            "shell",
            args,
            Ack(Box::new(move |answer| match tx.send(answer) {
                Ok(()) | Err(_) => {}
            })),
        );
    });
    rx
}

#[test]
fn shell_spilled_success_without_log_is_closing() {
    let opened = Opened::open(vec![]);
    let saved = b"original spilled bytes, kept whole";
    let relative = "artifacts/s_test.txt".to_owned();
    fs::write(opened.dir.join(&relative), saved).unwrap();
    let size = u64::try_from(saved.len()).unwrap();
    let absolute = opened.dir.join(&relative);
    opened.session.shell(fixed(
        spilled_ok(
            "HEAD",
            "TAIL",
            5,
            absolute.clone(),
            size,
            "ignored",
            Some(0),
        ),
        Bound { start: 1, end: 1 },
    ));
    let Opened {
        _temp,
        clock,
        home: _,
        sessions,
        id,
        dir,
        socket: _,
        out: _,
        session,
        log,
    } = opened;
    let driver = session.driver();
    drop(log);
    let timed: Arc<dyn Clock> = clock.clone();
    session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let rx = drive_shell(&driver, "cat big");
            let answer = deadline.recv_or_fail(&rx, "the shell answer without a log");
            match answer {
                Err(rejection) => {
                    assert_eq!(rejection.code, ErrorCode::Closing);
                    assert_eq!(rejection.message, ENDED);
                }
                Ok(_) => panic!("a spilled success without a log is rejected closing"),
            }
            assert_eq!(fs::read(dir.join(&relative)).unwrap(), saved);
            assert_eq!(artifacts_entries(&dir), vec!["s_test.txt".to_owned()]);
            Ok(())
        })
        .unwrap();
    close_within(session, reopened_log(sessions, id, timed));
}

#[test]
fn shell_unsaved_left_without_log_still_cleans_up() {
    let opened = Opened::open(vec![]);
    let relative = "artifacts/s_left.txt".to_owned();
    fs::write(opened.dir.join(&relative), b"partial").unwrap();
    let left = opened.dir.join(&relative);
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let notice = format!("[3 bytes cut. The full output could not be saved: {message}.]");
    opened.session.shell(fixed(
        spilled_unsaved(
            "H",
            "T",
            3,
            message,
            Some(left.clone()),
            Some(spilled_process(Some(0))),
            None,
            "",
        ),
        Bound::DEFAULT,
    ));
    let Opened {
        _temp,
        clock,
        home: _,
        sessions,
        id,
        dir: _,
        socket: _,
        out: _,
        session,
        log,
    } = opened;
    let driver = session.driver();
    drop(log);
    let timed: Arc<dyn Clock> = clock.clone();
    session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let rx = drive_shell(&driver, "cat left");
            let answer = deadline.recv_or_fail(&rx, "the unsaved answer without a log");
            match answer {
                Ok(Some(CommandResult::Shell {
                    output,
                    artifact,
                    process,
                })) => {
                    assert_eq!(output, format!("H\n{notice}\nT"));
                    assert!(artifact.is_none());
                    assert_eq!(process.exit_code, Some(0));
                }
                other => panic!("an unsaved spill without a log is accepted: {other:?}"),
            }
            assert!(!left.exists(), "cleanup does not need the log");
            Ok(())
        })
        .unwrap();
    close_within(session, reopened_log(sessions, id, timed));
}

#[test]
fn shell_unsaved_left_removal_failure_without_log_rejects_io_failed() {
    let held = Temp::new();
    let left = held.0.clone();
    assert!(left.is_dir());
    let opened = Opened::open(vec![]);
    opened.session.shell(fixed(
        spilled_unsaved(
            "H",
            "T",
            3,
            "could not save the output to /nowhere/s_x.txt: boom".to_owned(),
            Some(left.clone()),
            Some(spilled_process(Some(0))),
            None,
            "",
        ),
        Bound::DEFAULT,
    ));
    let Opened {
        _temp,
        clock,
        home: _,
        sessions,
        id,
        dir,
        socket: _,
        out: _,
        session,
        log,
    } = opened;
    let driver = session.driver();
    drop(log);
    let timed: Arc<dyn Clock> = clock.clone();
    let expected = left.display().to_string();
    session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let deadline = Deadline::after(DEADLINE);
            let rx = drive_shell(&driver, "cat stuck");
            let answer = deadline.recv_or_fail(&rx, "the stuck removal without a log");
            match answer {
                Err(rejection) => {
                    assert_eq!(rejection.code, ErrorCode::IoFailed);
                    assert!(
                        rejection.message.contains(&expected),
                        "the message names {expected}: {}",
                        rejection.message
                    );
                }
                Ok(_) => panic!("a stuck removal without a log is rejected io_failed"),
            }
            no_durable(&dir);
            Ok(())
        })
        .unwrap();
    close_within(session, reopened_log(sessions, id, timed));
}

#[test]
fn propose_notes_reaches_the_inbox_with_its_args_and_session_items() {
    let opened = Opened::open(vec![]);
    let work = Temp::new();
    let workspace = work.0.clone();
    // A git workspace whose remote URL carries userinfo, with one CI file.
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(args)
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "--quiet"]);
    git(&[
        "remote",
        "add",
        "origin",
        "https://user:token@github.com/acme/fiber.git",
    ]);
    fs::write(
        workspace.join(".gitlab-ci.yml"),
        "job:\n  script: make lint\n",
    )
    .unwrap();
    // One seeded session in this session's project records the workspace
    // and holds one escalation answered allow.
    let seed = opened
        .home
        .join("projects")
        .join("p")
        .join("sessions")
        .join("s_seed00000001");
    fs::create_dir_all(&seed).unwrap();
    let mut log = serde_json::json!({"kind": "session_started",
        "payload": {"workspace": workspace.to_str().unwrap()}})
    .to_string();
    log.push('\n');
    for (seq, kind, payload) in [
        (
            1,
            "permission_requested",
            serde_json::json!({"request_id": "r1", "step": "review",
                "escalation": {"cause": "consecutive_blocks", "reason": "blocked"},
                "rule": {"subject": "gh pr merge", "prefix": "gh pr merge"}}),
        ),
        (
            2,
            "permission_resolved",
            serde_json::json!({"request_id": "r1", "decision": "allow",
                "decided_by": "person"}),
        ),
    ] {
        log.push_str(
            &serde_json::json!({"kind": kind, "session_id": "s_seed00000001",
                "ts": seq, "schema_version": 1, "seq": seq, "payload": payload})
            .to_string(),
        );
        log.push('\n');
    }
    fs::write(seed.join("events.jsonl"), log).unwrap();
    opened.session.notes_workspace(workspace);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_1","command":"propose_notes","args":{"topic":"routine_actions","answer":"I merge my own PRs"}}"#,
            );
            match Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("propose_notes is delivered")
            {
                Delivery::ProposeNotes(ask, ack) => {
                    assert_eq!(ask.args.topic, contract::notes::Topic::RoutineActions);
                    assert_eq!(ask.args.answer.as_deref(), Some("I merge my own PRs"));
                    assert!(ask.args.earlier.is_empty());
                    assert_eq!(ask.evidence.len(), 3);
                    match &ask.evidence[0] {
                        contract::notes::Evidence::Files { text } => {
                            assert_eq!(
                                text,
                                "git remote origin: https://github.com/acme/fiber.git"
                            );
                        }
                        contract::notes::Evidence::Session(_)
                        | contract::notes::Evidence::Allows(_)
                        | contract::notes::Evidence::Answer { .. } => {
                            panic!("the remote item arrives first")
                        }
                    }
                    match &ask.evidence[1] {
                        contract::notes::Evidence::Files { text } => {
                            assert!(
                                text.starts_with("CI configuration .gitlab-ci.yml:"),
                                "{text}"
                            );
                        }
                        contract::notes::Evidence::Session(_)
                        | contract::notes::Evidence::Allows(_)
                        | contract::notes::Evidence::Answer { .. } => {
                            panic!("the CI item arrives second")
                        }
                    }
                    match &ask.evidence[2] {
                        contract::notes::Evidence::Session(tally) => {
                            assert_eq!(tally.prefix(), "gh pr merge");
                            assert_eq!(tally.count(), 1);
                            assert_eq!(tally.here(), 1);
                        }
                        contract::notes::Evidence::Files { .. }
                        | contract::notes::Evidence::Allows(_)
                        | contract::notes::Evidence::Answer { .. } => {
                            panic!("session evidence arrives as a session item")
                        }
                    }
                    ack.0(Ok(None));
                }
                Delivery::Prompt(..)
                | Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Name(..)
                | Delivery::AddDirectory(..)
                | Delivery::Handoff(..)
                | Delivery::Credential(..)
                | Delivery::Reload(..)
                | Delivery::McpReconnect(..)
                | Delivery::Reply(..)
                | Delivery::Rewind(..)
                | Delivery::ChangeWorkspace(..)
                | Delivery::Close(_)
                | Delivery::Job(_)
                | Delivery::JobLine(_)
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)
                | Delivery::ExtensionExec(_)
                | Delivery::Message(..)
                | Delivery::InnerCall(_)
                | Delivery::ExtensionLog(_)
                | Delivery::ExtensionJob(..)
                | Delivery::ExtensionUsage(_)
        | Delivery::DelegateUsage { .. }
                | Delivery::Model(..)
                | Delivery::Shell(..)
                | Delivery::Cancelled => panic!("propose_notes arrives as itself"),
            }
            let accepted = response(&client, "c_1");
            assert_eq!(kind(&accepted), "command_accepted");
            // The read-only topic gathers the same files and session
            // items: no shell is wired here, so no allow tally joins them.
            send(
                &client,
                r#"{"id":"c_2","command":"propose_notes","args":{"topic":"read_only_commands"}}"#,
            );
            match Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("propose_notes is delivered")
            {
                Delivery::ProposeNotes(ask, ack) => {
                    assert_eq!(
                        ask.args.topic,
                        contract::notes::Topic::ReadOnlyCommands
                    );
                    assert_eq!(ask.evidence.len(), 3);
                    for item in &ask.evidence {
                        assert!(
                            !matches!(item, contract::notes::Evidence::Allows(_)),
                            "no shell is wired, so no allow tally is gathered"
                        );
                    }
                    ack.0(Ok(None));
                }
                Delivery::Prompt(..)
                | Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Name(..)
                | Delivery::AddDirectory(..)
                | Delivery::Handoff(..)
                | Delivery::Credential(..)
                | Delivery::Reload(..)
                | Delivery::McpReconnect(..)
                | Delivery::Reply(..)
                | Delivery::Rewind(..)
                | Delivery::ChangeWorkspace(..)
                | Delivery::Close(_)
                | Delivery::Job(_)
                | Delivery::JobLine(_)
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)
                | Delivery::ExtensionExec(_)
                | Delivery::Message(..)
                | Delivery::InnerCall(_)
                | Delivery::ExtensionLog(_)
                | Delivery::ExtensionJob(..)
                | Delivery::ExtensionUsage(_)
        | Delivery::DelegateUsage { .. }
                | Delivery::Model(..)
                | Delivery::Shell(..)
                | Delivery::Cancelled => panic!("propose_notes arrives as itself"),
            }
            let accepted = response(&client, "c_2");
            assert_eq!(kind(&accepted), "command_accepted");
            Ok(())
        })
        .unwrap();
    opened.close();
    // Without a wired workspace the same command is unknown.
    let bare = Opened::open(vec![]);
    let bare_socket = bare.socket.clone();
    bare.session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&bare_socket).unwrap();
            subscribe(&client, "c_sub", "full");
            send(
                &client,
                r#"{"id":"c_1","command":"propose_notes","args":{"topic":"never"}}"#,
            );
            let line = response(&client, "c_1");
            assert_eq!(
                rejection(&line),
                (
                    "unknown_command",
                    "`propose_notes` is not built in this Fiber yet."
                )
            );
            Ok(())
        })
        .unwrap();
    bare.close();
}

/// One `session_started` line recording `workspace`.
fn ro_started(workspace: &Path) -> String {
    let mut line = serde_json::json!({"kind": "session_started",
        "payload": {"workspace": workspace.to_str().unwrap()}})
    .to_string();
    line.push('\n');
    line
}

/// One envelope line for `kind` holding `action_id` and `payload`.
fn ro_line(kind: &str, action: &str, seq: u64, payload: &serde_json::Value) -> String {
    let mut line = serde_json::json!({"kind": kind, "session_id": "s_seed00000001",
        "ts": seq, "schema_version": 1, "seq": seq, "payload": payload,
        "action_id": action})
    .to_string();
    line.push('\n');
    line
}

/// A shell `tool_call_requested` running `command`.
fn ro_requested(action: &str, seq: u64, command: &str) -> String {
    ro_line(
        "tool_call_requested",
        action,
        seq,
        &serde_json::json!({"name": "shell",
            "arguments": {"command": command, "workdir": "/w"}}),
    )
}

/// A review `permission_requested` carrying `subject` with no escalation.
fn ro_review_request(action: &str, seq: u64, id: &str, subject: &str) -> String {
    ro_line(
        "permission_requested",
        action,
        seq,
        &serde_json::json!({"request_id": id, "step": "review",
            "rule": {"subject": subject, "prefix": subject}}),
    )
}

/// A `permission_resolved` line with `decision` from the reviewer.
fn ro_review_resolved(action: &str, seq: u64, id: &str, decision: &str) -> String {
    ro_line(
        "permission_resolved",
        action,
        seq,
        &serde_json::json!({"request_id": id, "decision": decision,
            "decided_by": "reviewer"}),
    )
}

/// Seeds this session's log under `home` with `body` after its start line.
fn ro_seed(home: &Path, workspace: &Path, body: &str) {
    let seed = home
        .join("projects")
        .join("p")
        .join("sessions")
        .join("s_seed00000001");
    fs::create_dir_all(&seed).unwrap();
    let mut log = ro_started(workspace);
    log.push_str(body);
    fs::write(seed.join("events.jsonl"), log).unwrap();
}

/// Wires `shell` and asks for read-only notes, returning the gathered
/// evidence after accepting the ask.
/// A shell classifying one command as read-only: `effects` answers the
/// declared read with its subject and prefix, and `run` never fires.
struct ReadOnlyStub {
    table: std::collections::HashMap<String, (String, String)>,
}

impl ReadOnlyStub {
    fn new(table: &[(&str, &str, &str)]) -> Self {
        Self {
            table: table
                .iter()
                .map(|(command, subject, prefix)| {
                    (
                        command.to_string(),
                        (subject.to_string(), prefix.to_string()),
                    )
                })
                .collect(),
        }
    }
}

impl Tool for ReadOnlyStub {
    fn definition(&self) -> ToolDefinition {
        shell_definition()
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let command = arguments
            .get("command")
            .and_then(Value::as_str)
            .expect("a command string");
        match self.table.get(command) {
            Some((subject, prefix)) => Ok(Effects {
                declared: DeclaredEffects {
                    effects: vec![Effect::Reads],
                    reversible: true,
                    paths: None,
                    confined: false,
                },
                subject: Some(subject.clone()),
                prefix: Some(prefix.clone()),
                always_reviewed: false,
                review_when_confined: false,
                hosts: None,
            }),
            None => Err(EffectsError::Arguments("unclassifiable".to_owned())),
        }
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        panic!("the stub never runs");
    }
}

/// A git workspace with one CI file: two file-evidence items join every
/// gather under it, so the allow tally is the only moving item.
fn ro_workspace() -> Temp {
    let work = Temp::new();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&work.0)
            .args(args)
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "--quiet"]);
    git(&[
        "remote",
        "add",
        "origin",
        "https://user:token@github.com/acme/fiber.git",
    ]);
    fs::write(work.0.join(".gitlab-ci.yml"), "job:\n  script: make lint\n").unwrap();
    work
}

fn ro_evidence(socket: &Path, inbox: &Receiver<Delivery>) -> Vec<contract::notes::Evidence> {
    let client = Client::connect(socket).unwrap();
    subscribe(&client, "c_sub", "full");
    send(
        &client,
        r#"{"id":"c_1","command":"propose_notes","args":{"topic":"read_only_commands"}}"#,
    );
    let evidence = match Deadline::after(DEADLINE)
        .recv(inbox)
        .expect("propose_notes is delivered")
    {
        Delivery::ProposeNotes(ask, ack) => {
            assert_eq!(ask.args.topic, contract::notes::Topic::ReadOnlyCommands);
            let evidence = ask.evidence.clone();
            ack.0(Ok(None));
            evidence
        }
        Delivery::Prompt(..)
        | Delivery::Steer(..)
        | Delivery::SteerDrop(..)
        | Delivery::Message(..)
        | Delivery::Model(..)
        | Delivery::Credential(..)
        | Delivery::Reload(_)
        | Delivery::McpReconnect(..)
        | Delivery::Name(..)
        | Delivery::AddDirectory(..)
        | Delivery::Rewind(..)
        | Delivery::ChangeWorkspace(..)
        | Delivery::Handoff(..)
        | Delivery::Reply(..)
        | Delivery::Close(_)
        | Delivery::Job(_)
        | Delivery::JobLine(_)
        | Delivery::ExtensionExec(_)
        | Delivery::ExtensionJob(..)
        | Delivery::Interaction(_)
        | Delivery::Resolved(..)
        | Delivery::InnerCall(_)
        | Delivery::ExtensionLog(_)
        | Delivery::ExtensionUsage(_)
        | Delivery::DelegateUsage { .. }
        | Delivery::Cancelled
        | Delivery::Shell(..) => panic!("propose_notes arrives as itself"),
    };
    let accepted = response(&client, "c_1");
    assert_eq!(kind(&accepted), "command_accepted");
    evidence
}

#[test]
fn propose_notes_with_malformed_configured_entries_excludes_allows() {
    let opened = Opened::open(vec![]);
    let work = ro_workspace();
    let workspace = work.0.clone();
    // A malformed configured entry cannot validate, so every shell
    // allowance is withheld even though the shell is wired.
    fs::write(
        opened.home.join("config.json"),
        r#"{"shell.read_only": []}"#,
    )
    .unwrap();
    let mut body = String::new();
    body.push_str(&ro_requested("a1", 1, "ping 1.2.3.4"));
    body.push_str(&ro_review_request("a1", 2, "r1", "ping 1.2.3.4"));
    body.push_str(&ro_review_resolved("a1", 3, "r1", "allow"));
    ro_seed(&opened.home, &workspace, &body);
    opened.session.shell(Arc::new(ReadOnlyStub::new(&[(
        "ping 1.2.3.4",
        "ping 1.2.3.4",
        "ping",
    )])));
    opened.session.notes_workspace(workspace);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let evidence = ro_evidence(&socket, &inbox);
            assert_eq!(evidence.len(), 2);
            for item in &evidence {
                assert!(
                    !matches!(item, contract::notes::Evidence::Allows(_)),
                    "a malformed configured layer withholds every allowance"
                );
            }
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn propose_notes_with_a_deny_excludes_the_covered_allow() {
    let opened = Opened::open(vec![]);
    let work = ro_workspace();
    let workspace = work.0.clone();
    // One allowance for `ping` and one deny covering the same command:
    // the deny disqualifies the tally, so no allow evidence joins.
    let mut body = String::new();
    body.push_str(&ro_requested("a1", 1, "ping 1.2.3.4"));
    body.push_str(&ro_review_request("a1", 2, "r1", "ping 1.2.3.4"));
    body.push_str(&ro_review_resolved("a1", 3, "r1", "allow"));
    body.push_str(&ro_requested("a2", 4, "ping 1.2.3.4"));
    body.push_str(&ro_review_request("a2", 5, "r2", "ping 1.2.3.4"));
    body.push_str(&ro_review_resolved("a2", 6, "r2", "deny"));
    ro_seed(&opened.home, &workspace, &body);
    opened.session.shell(Arc::new(ReadOnlyStub::new(&[(
        "ping 1.2.3.4",
        "ping 1.2.3.4",
        "ping",
    )])));
    opened.session.notes_workspace(workspace);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let evidence = ro_evidence(&socket, &inbox);
            assert_eq!(evidence.len(), 2);
            for item in &evidence {
                assert!(
                    !matches!(item, contract::notes::Evidence::Allows(_)),
                    "a deny covering the command disqualifies its allowance"
                );
            }
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn propose_notes_with_five_allows_includes_the_allowance() {
    let opened = Opened::open(vec![]);
    let work = ro_workspace();
    let workspace = work.0.clone();
    // Five reviewed allows of the same command gather one tally whose
    // echo the reviewer answers.
    let mut body = String::new();
    for (index, seq) in [1u64, 4, 7, 10, 13].iter().enumerate() {
        let action = format!("a{}", index + 1);
        let request = format!("r{}", index + 1);
        body.push_str(&ro_requested(&action, *seq, "ping 1.2.3.4"));
        body.push_str(&ro_review_request(
            &action,
            seq + 1,
            &request,
            "ping 1.2.3.4",
        ));
        body.push_str(&ro_review_resolved(&action, seq + 2, &request, "allow"));
    }
    ro_seed(&opened.home, &workspace, &body);
    opened.session.shell(Arc::new(ReadOnlyStub::new(&[(
        "ping 1.2.3.4",
        "ping 1.2.3.4",
        "ping",
    )])));
    opened.session.notes_workspace(workspace);
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let evidence = ro_evidence(&socket, &inbox);
            assert_eq!(evidence.len(), 3);
            let tally = evidence
                .iter()
                .find_map(|item| match item {
                    contract::notes::Evidence::Allows(tally) => Some(tally),
                    contract::notes::Evidence::Files { .. }
                    | contract::notes::Evidence::Answer { .. }
                    | contract::notes::Evidence::Session(_) => None,
                })
                .expect("five allows gather one tally");
            assert_eq!(tally.subject(), "ping 1.2.3.4");
            assert_eq!(tally.count(), 5);
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn an_undelivered_ack_answers_after_the_inbox_guard_releases() {
    // R42: dropping an undeliverable delivery answers `closing` only after
    // the inbox guard releases, so an answer that delivers again on the
    // same thread cannot deadlock on it.
    let opened = Opened::open(vec![]);
    let work = Temp::new();
    opened.session.notes_workspace(work.0.clone());
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            // Only the receiver is gone: the slot stays `Some`, and the
            // connections are not yet stopped.
            drop(inbox);
            let (notes_tx, notes_rx) = mpsc::channel();
            let (model_tx, model_rx) = mpsc::channel();
            let mut args = Map::new();
            args.insert("topic".to_owned(), Value::String("never".to_owned()));
            let inner = Arc::clone(&driver);
            driver.drive(
                "fiber.test/a",
                "propose_notes",
                args,
                Ack(Box::new(move |answer| {
                    // From inside the answer, `model` delivers again on
                    // this same thread: under the old `deliver` this
                    // deadlocked on the inbox guard.
                    let mut model = Map::new();
                    model.insert("model".to_owned(), Value::String("fake/n".to_owned()));
                    inner.drive(
                        "fiber.test/a",
                        "model",
                        model,
                        Ack(Box::new(move |answer| match model_tx.send(answer) {
                            Ok(()) | Err(_) => {}
                        })),
                    );
                    match notes_tx.send(answer) {
                        Ok(()) | Err(_) => {}
                    }
                })),
            );
            let notes = Deadline::after(DEADLINE).recv_or_fail(&notes_rx, "the notes answer");
            assert_eq!(notes.unwrap_err().code, ErrorCode::Closing);
            let model = Deadline::after(DEADLINE).recv_or_fail(&model_rx, "the model answer");
            assert_eq!(model.unwrap_err().code, ErrorCode::Closing);
            Ok(())
        })
        .unwrap();
    opened.close();
}

/// The `import` driver command answers what a direct shared import
/// reports: the same files plus the same left-behind items, on identical
/// fixtures (`docs/testing.md`, "Levels"). The child re-run carries a
/// private `HOME`, since the dispatch reads the person's home from the
/// environment exactly as `fiber import` does.
mod import_sameness {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use fakes::Deadline;
    use serde_json::{Map, Value, json};

    use super::support::{Opened, kind, response, send, subscribe};

    /// The child's case marker: set, the test runs the named case in the
    /// child and returns, so the parent proves the sameness against a
    /// private `HOME`.
    const CHILD: &str = "FIBER_DOORS_SOCKET_IMPORT_CHILD";

    /// How long a re-run child may run: opening a session and importing
    /// local files only. Twice it stays inside half of nextest's per-test
    /// timeout (`docs/testing.md`, "Waits and timeouts").
    const CHILD_WITHIN: Duration = Duration::from_secs(30);

    fn child_name() -> String {
        module_path!().split_once("::").unwrap().1.to_owned()
    }

    /// Writes `content` to `rel` under `root`, creating parents.
    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A fake `~/.claude` mirroring the probed layout: a skill, a flat
    /// command, instruction content, two mapped servers, and unmapped
    /// entries including a secret.
    fn claude_home(root: &Path) {
        write(
            root,
            ".claude/skills/review/SKILL.md",
            "---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n",
        );
        write(root, ".claude/commands/greet.md", "Hello there.\n");
        write(root, ".claude/CLAUDE.md", "Be kind.\n");
        write(
            root,
            ".claude/settings.json",
            &json!({
                "model": "opus",
                "mcpServers": {
                    "notes": {"command": "notes-server", "args": ["--fast"]},
                    "remote": {"url": "https://mcp.example.com/x"},
                },
            })
            .to_string(),
        );
        write(root, ".claude/agents/worker.md", "worker\n");
        write(root, ".claude/auth.json", "{\"token\": \"x\"}\n");
    }

    /// A fake `~/.codex` mirroring the probed layout: a skill, a flat
    /// prompt, instruction content, two mapped servers, and a secret.
    fn codex_home(root: &Path) {
        write(
            root,
            ".codex/skills/lint/SKILL.md",
            "---\nname: lint\ndescription: Lints.\n---\n\nLint body.\n",
        );
        write(root, ".codex/prompts/plan.md", "Plan it.\n");
        write(root, ".codex/AGENTS.md", "Be thorough.\n");
        write(
            root,
            ".codex/config.toml",
            "[mcp_servers.alpha]\n\
             command = \"alpha-server\"\n\
             \n\
             [mcp_servers.beta]\n\
             url = \"https://beta.example.com/mcp\"\n",
        );
        write(root, ".codex/auth.json", "{\"token\": \"x\"}\n");
    }

    /// Every import-landed file under `home` but the receipts, as relative
    /// paths with bytes, in order: the opened session's own durable files
    /// under `projects/` are the harness's, not the import's.
    fn tree(home: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut pending = vec![home.to_path_buf()];
        while let Some(path) = pending.pop() {
            if path.is_dir() && !path.is_symlink() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    pending.push(entry.unwrap().path());
                }
                continue;
            }
            if path.is_file() {
                let rel = path.strip_prefix(home).unwrap().display().to_string();
                if rel == "import-claude-code.json"
                    || rel == "import-codex.json"
                    || rel.starts_with("projects/")
                {
                    continue;
                }
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
        out.sort();
        out
    }

    /// Runs the shared import directly, as `fiber import` does: the same
    /// code path the driver command runs.
    fn direct(home: &Path, person: &Path, source: config::ImportSource) -> config::ImportReport {
        let clock = fakes::clock::FakeClock::new();
        let source_home = config::source_home(person, source);
        config::import_source(
            home,
            &source_home,
            source,
            &config::no_personal_sources,
            clock.as_ref(),
        )
        .unwrap()
    }

    /// The driver's accepted `result` for the `import` line with `id`.
    fn drive(client: &fakes::Client, line: &str, id: &str) -> Value {
        send(client, line);
        let answer = response(client, id);
        assert_eq!(kind(&answer), "command_accepted", "{answer}");
        answer["payload"]["result"].clone()
    }

    /// Two reports back to back, Claude Code's lists then Codex's, as the
    /// bare driver command aggregates them.
    fn combined(first: &config::ImportReport, second: &config::ImportReport) -> Value {
        let first = serde_json::to_value(first).unwrap();
        let second = serde_json::to_value(second).unwrap();
        let mut imported = Map::new();
        for key in ["mcp_servers", "skills", "commands", "instructions"] {
            let mut names = first["imported"][key].as_array().unwrap().clone();
            names.extend(second["imported"][key].as_array().unwrap().clone());
            imported.insert(key.to_owned(), Value::Array(names));
        }
        let mut left = first["left_behind"].as_array().unwrap().clone();
        left.extend(second["left_behind"].as_array().unwrap().clone());
        json!({"imported": imported, "left_behind": left})
    }

    /// A bare `import` matches two direct shared imports, one per source:
    /// the accepted report, the landed files, and both receipts.
    fn bare(person: &Path) {
        let direct_home = fakes::TempDir::new("fiber-import-direct");
        let opened = Opened::open(Vec::new());
        let socket = opened.socket.clone();
        let home = opened.home.clone();
        let direct_root = direct_home.path().to_path_buf();
        let person = person.to_path_buf();
        opened
            .session
            .run(Vec::new(), Arc::new(|| false), move |_| {
                let client = fakes::Client::connect(&socket).unwrap();
                subscribe(&client, "sub", "summary");
                let result = drive(&client, r#"{"id":"imp","command":"import"}"#, "imp");
                let claude = direct(&direct_root, &person, config::ImportSource::ClaudeCode);
                let codex = direct(&direct_root, &person, config::ImportSource::Codex);
                assert_eq!(result, combined(&claude, &codex), "{result}");
                assert_eq!(tree(&home), tree(&direct_root));
                for receipt in ["import-claude-code.json", "import-codex.json"] {
                    assert!(home.join(receipt).is_file(), "driver writes {receipt}");
                    assert!(
                        direct_root.join(receipt).is_file(),
                        "direct writes {receipt}"
                    );
                }
                Ok(())
            })
            .unwrap();
        opened.close();
    }

    /// A sourced `import` matches the direct shared import for that
    /// source: no Claude receipt, no Claude files.
    fn one(person: &Path) {
        let direct_home = fakes::TempDir::new("fiber-import-direct");
        let opened = Opened::open(Vec::new());
        let socket = opened.socket.clone();
        let home = opened.home.clone();
        let direct_root = direct_home.path().to_path_buf();
        let person = person.to_path_buf();
        opened
            .session
            .run(Vec::new(), Arc::new(|| false), move |_| {
                let client = fakes::Client::connect(&socket).unwrap();
                subscribe(&client, "sub", "summary");
                let result = drive(
                    &client,
                    r#"{"id":"imp","command":"import","args":{"source":"codex"}}"#,
                    "imp",
                );
                let codex = direct(&direct_root, &person, config::ImportSource::Codex);
                assert_eq!(result, serde_json::to_value(&codex).unwrap(), "{result}");
                assert_eq!(tree(&home), tree(&direct_root));
                assert!(!home.join("import-claude-code.json").exists());
                assert!(!direct_root.join("import-claude-code.json").exists());
                Ok(())
            })
            .unwrap();
        opened.close();
    }

    /// The child's entry: the parent's `HOME` holds the fake person home
    /// the case reads.
    fn run_child(case: &str) {
        let person = PathBuf::from(std::env::var_os("HOME").unwrap());
        match case {
            "bare" => bare(&person),
            "one" => one(&person),
            other => panic!("unknown import case {other}"),
        }
    }

    /// Re-runs the current test in a child with `HOME` pointing at
    /// `person`: the child's exit code proves the case.
    #[track_caller]
    fn parent(test: &str, case: &str, person: &Path) {
        let output = fakes::rerun_prepared(
            &format!("{}::{test}", child_name()),
            &[(CHILD, case)],
            Deadline::after(CHILD_WITHIN),
            |command| {
                command.env("HOME", person);
            },
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "child {case} failed: stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[test]
    fn bare_import_matches_a_direct_shared_import() {
        if let Some(case) = std::env::var_os(CHILD) {
            run_child(case.to_str().unwrap());
            return;
        }
        let person = fakes::TempDir::new("fiber-import-person");
        claude_home(person.path());
        codex_home(person.path());
        parent(
            "bare_import_matches_a_direct_shared_import",
            "bare",
            person.path(),
        );
    }

    #[test]
    fn sourced_import_matches_a_direct_shared_import() {
        if let Some(case) = std::env::var_os(CHILD) {
            run_child(case.to_str().unwrap());
            return;
        }
        let person = fakes::TempDir::new("fiber-import-person");
        claude_home(person.path());
        codex_home(person.path());
        parent(
            "sourced_import_matches_a_direct_shared_import",
            "one",
            person.path(),
        );
    }
}

/// Session-owned socket peers, refused at the door (`docs/permissions.md`,
/// "Sessions on the sockets"). A fake owner is `sh` started as
/// `sh session --id=<owned>`: a shell reads its first operand as its script,
/// so the kernel argument vector is `[sh, session, --id=<owned>, ...]`,
/// the shape `session_argv_id` matches. The script spawns the probe, so the
/// probe connects as a session-owned peer. The probe prints each answer
/// line; the test reads them under [`DEADLINE`], so a missing answer fails
/// instead of hanging.
const OWNED_PROBE: &str = r#"import json, socket, sys
path = sys.argv[1]
specs = [json.loads(a) for a in sys.argv[2:]]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(path)
f = s.makefile("r")


def wait_for(cid):
    while True:
        line = f.readline()
        if not line:
            print("SOCKET_CLOSED", flush=True)
            sys.exit(0)
        try:
            v = json.loads(line)
        except Exception:
            continue
        if v.get("payload", {}).get("command_id") == cid:
            return v


s.sendall(
    (json.dumps({"id": "c_sub", "command": "subscribe", "args": {"level": "full"}}) + "\n").encode()
)
wait_for("c_sub")
for spec in specs:
    s.sendall((json.dumps(spec) + "\n").encode())
    print(json.dumps(wait_for(spec["id"])), flush=True)
"#;

/// A fake session owner and the probe it parents: the probe's kernel walk
/// names the owner's `--id=`, so the socket sees a session-owned peer.
/// Dropping kills the owner, so a failed test leaves no stray process.
struct OwnedPeer {
    child: Option<Child>,
    lines: mpsc::Receiver<String>,
}

impl OwnedPeer {
    /// Starts a fake `fiber session --id=<owned>` in `dir`, running the
    /// probe against `socket` with `specs` as its command lines. The fake
    /// is a symlink to the shell, so the kernel runs the shell with argv[0]
    /// naming `fiber`: a shebang script would name its interpreter instead.
    /// The id names nothing; any session-shaped ancestor marks the probe.
    fn start(dir: &Path, owned: &SessionId, socket: &Path, specs: &[Value]) -> OwnedPeer {
        let probe = dir.join("probe.py");
        fs::write(&probe, OWNED_PROBE).unwrap();
        // The first operand names the fake session; the rest are the probe's
        // command lines. The probe runs as a child, so this shell keeps its
        // fake argument vector alive as the probe's parent.
        std::os::unix::fs::symlink("/bin/sh", dir.join("fiber")).unwrap();
        fs::write(
            dir.join("session"),
            "shift\npython3 -u \"$OWNED_PROBE\" \"$OWNED_SOCK\" \"$@\" & wait\n",
        )
        .unwrap();
        let mut child = Command::new(dir.join("fiber"))
            .arg("session")
            .arg(format!("--id={}", owned.0))
            .args(specs.iter().map(ToString::to_string))
            .current_dir(dir)
            .env("OWNED_PROBE", &probe)
            .env("OWNED_SOCK", socket)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        OwnedPeer {
            child: Some(child),
            lines: rx,
        }
    }

    /// The probe's next answer line, parsed, within one [`DEADLINE`].
    fn answer(&self) -> Value {
        let line = Deadline::after(DEADLINE)
            .recv(&self.lines)
            .expect("the probe printed its answer");
        serde_json::from_str(&line).unwrap()
    }

    /// Reaps the probe once every answer is read.
    fn finish(&mut self) {
        let Some(child) = self.child.take() else {
            panic!("the probe was already reaped");
        };
        fakes::wait_output(child, "the probe exited");
    }
}

impl Drop for OwnedPeer {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _killed = child.kill();
        }
    }
}

/// A person-shaped steer or prompt line with `id`.
fn text_command(id: &str, command: &str) -> Value {
    serde_json::json!({
        "id": id,
        "command": command,
        "args": {"content": [{"type": "text", "text": "hi"}]},
    })
}

/// An approval reply line: `decision` answers the approval request.
fn approval_command(id: &str, request: &str) -> Value {
    serde_json::json!({
        "id": id,
        "command": "reply",
        "args": {"request_id": request, "decision": "allow"},
    })
}

/// A question reply line: `confirmed` answers the question asked.
fn question_reply_command(id: &str, request: &str) -> Value {
    serde_json::json!({
        "id": id,
        "command": "reply",
        "args": {"request_id": request, "confirmed": true},
    })
}

/// A message line from `from`, reusing the harness's own shape.
fn owned_message_command(id: &str, from: &SessionId) -> Value {
    serde_json::from_str(&message_line(id, &from.0, "hi")).unwrap()
}

/// A valid session id that is never `id`: the last hex digit flips.
fn other_id(id: &SessionId) -> SessionId {
    let mut other = id.0.clone();
    let last = other.pop().unwrap();
    other.push(if last == '0' { '1' } else { '0' });
    SessionId(other)
}

/// The `from_session` sentence for an approval answered from inside a
/// session: only a person answers.
fn refusal_approval_sentence(target: &SessionId) -> String {
    format!(
        "Only a person answers an approval. This connection comes from inside a session, so `reply` to session `{}` is refused.",
        target.0
    )
}

/// An approval answer from a session-shaped ancestor is refused without
/// any target relation: the fake `fiber session` on the probe's walk marks
/// it, whatever id the fake names. Refusal 2 needs a pid intersection the
/// in-process session cannot express end to end, so the scripted dispatch
/// tests pin it; the binary suite pins it with real session processes.
#[test]
fn approval_from_a_session_shaped_ancestor_is_refused() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let owned = opened.id.clone();
    let specs = [approval_command("c_1", "r_1")];
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let holder = fakes::TempDir::new("fiber-owned");
            let dir = holder.path().to_path_buf();
            let mut peer = OwnedPeer::start(&dir, &owned, &socket, &specs);
            let answer = peer.answer();
            assert_eq!(command_id(&answer), Some("c_1"));
            assert_eq!(
                rejection(&answer),
                ("from_session", refusal_approval_sentence(&owned).as_str()),
                "{answer}"
            );
            assert!(
                inbox.try_recv().is_err(),
                "a refused approval is never delivered"
            );
            peer.finish();
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn session_owned_message_is_delivered() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let owned = opened.id.clone();
    let specs = [owned_message_command("c_1", &opened.id)];
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let holder = fakes::TempDir::new("fiber-owned");
            let dir = holder.path().to_path_buf();
            let mut peer = OwnedPeer::start(&dir, &owned, &socket, &specs);
            let wait = Deadline::after(DEADLINE);
            assert_eq!(
                take(&inbox, &wait),
                format!("message {} c_1", owned.0),
                "a message from the owning session is delivered"
            );
            let answer = peer.answer();
            assert_eq!(command_id(&answer), Some("c_1"));
            assert_eq!(kind(&answer), "command_accepted", "{answer}");
            peer.finish();
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn unrelated_session_steer_reply_and_message_are_allowed() {
    let opened = Opened::open(vec![]);
    let socket = opened.socket.clone();
    let target = opened.id.clone();
    let other = other_id(&target);
    let specs = [
        text_command("c_1", "steer"),
        question_reply_command("c_2", "r_9"),
        owned_message_command("c_3", &other),
        approval_command("c_4", "r_4"),
    ];
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let holder = fakes::TempDir::new("fiber-owned");
            let dir = holder.path().to_path_buf();
            let mut peer = OwnedPeer::start(&dir, &other, &socket, &specs);
            let wait = Deadline::after(DEADLINE);
            assert_eq!(take(&inbox, &wait), "steer hi");
            let answer = peer.answer();
            assert_eq!(command_id(&answer), Some("c_1"));
            assert_eq!(kind(&answer), "command_accepted", "{answer}");
            assert_eq!(take(&inbox, &wait), "reply r_9");
            let answer = peer.answer();
            assert_eq!(command_id(&answer), Some("c_2"));
            assert_eq!(kind(&answer), "command_accepted", "{answer}");
            assert_eq!(
                take(&inbox, &wait),
                format!("message {} c_3", other.0),
                "a message from another session is delivered"
            );
            let answer = peer.answer();
            assert_eq!(command_id(&answer), Some("c_3"));
            assert_eq!(kind(&answer), "command_accepted", "{answer}");
            let answer = peer.answer();
            assert_eq!(command_id(&answer), Some("c_4"));
            assert_eq!(
                rejection(&answer),
                ("from_session", refusal_approval_sentence(&target).as_str()),
                "{answer}"
            );
            assert!(
                inbox.try_recv().is_err(),
                "the refused approval is never delivered"
            );
            peer.finish();
            Ok(())
        })
        .unwrap();
    opened.close();
}
