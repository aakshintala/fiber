//! `SessionExtensions::reply` across two extensions
//! (`docs/extensions.md`, "Commands and screens"): the extension holding
//! the ask takes the reply, in load order; any other reply is handed back
//! for the loop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::RequestId;
use contract::commands::{Reply, ReplyAnswer};
use contract::events::{InteractionRequested, ResolvedBy};
use contract::extension::ExtensionDoor;
use contract::inbox::{Ack, Delivery};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::json;

use super::super::SessionExtensions;
use crate::host::FakeLock;
use crate::{Origin, Request, plan};

/// Wall-clock bound on a wait for a delivery.
const WAIT: Duration = Duration::from_secs(5);

struct Home {
    root: fakes::TempDir,
}

impl Home {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-door-reply");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn install(&self, short: &str, init: &str) {
        let src = self.root.path().join("src").join(short);
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("extension.json"),
            json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1})
                .to_string(),
        )
        .unwrap();
        fs::write(src.join("init.lua"), init).unwrap();
        plan(
            &self.home(),
            &Request::Path(src),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    #[track_caller]
    fn load(&self) -> Arc<SessionExtensions> {
        let config = Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: Vec::new(),
        })
        .unwrap();
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        Arc::new(bounded(move || {
            SessionExtensions::load(&home, &config, FakeClock::new(), locks, None)
        }))
    }
}

fn asking(short: &str) -> String {
    format!(
        "fiber.command(\"go\", {{ timeout = 5000, run = function()\n\
         local answer = host.ask(\"confirm\", {{ prompt = \"{short}?\" }})\n\
         return tostring(answer.confirmed) end }})\n"
    )
}

/// Runs `command` on `session`'s `which`-th extension and sends its result.
fn run(
    session: &Arc<SessionExtensions>,
    which: usize,
    command: &str,
) -> mpsc::Receiver<Result<String, crate::Error>> {
    let lua = Arc::clone(&session.lua[which]);
    let command = command.to_owned();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done_tx.send(lua.command(&command, ""));
    });
    done_rx
}

#[track_caller]
fn interaction(rx: &mpsc::Receiver<Delivery>, wait: &Deadline) -> InteractionRequested {
    let Delivery::Interaction(requested) = wait.recv(rx).expect("the Interaction arrives") else {
        panic!("an unexpected delivery arrives");
    };
    requested
}

/// Runs `f` on its own thread and waits for it under [`WAIT`], so a load
/// that never returns fails the test instead of hanging it.
#[track_caller]
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(f());
    });
    Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the extensions")
}

#[test]
fn the_extension_holding_the_ask_takes_the_reply_in_load_order() {
    let home = Home::new();
    home.install("a", &asking("a"));
    home.install("b", &asking("b"));
    let session = home.load();
    let (inbox_tx, rx) = mpsc::channel();
    for lua in &session.lua {
        lua.deliver_to(inbox_tx.clone());
    }
    session.answerable(true);
    // Both commands park on their asks; each Interaction names its own
    // extension, whatever order they arrive in.
    let first = run(&session, 0, "go");
    let second = run(&session, 1, "go");
    let mut ids = std::collections::BTreeMap::new();
    // One deadline for the whole wait: both Interactions arrive on one
    // stream with nothing between them but a map insert.
    let wait = Deadline::after(WAIT);
    for _ in 0..2 {
        let requested = interaction(&rx, &wait);
        ids.insert(
            requested.extension.clone().unwrap_or_default(),
            requested.request_id,
        );
    }
    let (id_a, id_b) = (
        ids.remove("fiber.test/a").expect("a asks"),
        ids.remove("fiber.test/b").expect("b asks"),
    );
    // A reply for B passes through A (which hands it back) to B.
    let (tx, accepted) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer.is_ok());
    }));
    assert!(
        session
            .reply(
                Reply {
                    request_id: id_b.clone(),
                    answer: ReplyAnswer::Confirmed { confirmed: true },
                },
                ack
            )
            .is_none(),
        "the holding extension takes the reply"
    );
    let Delivery::Resolved(resolved, ack) = Deadline::after(WAIT)
        .recv(&rx)
        .expect("the Resolved arrives")
    else {
        panic!("a Resolved arrives");
    };
    assert_eq!(resolved.request_id, id_b);
    assert_eq!(resolved.by, ResolvedBy::Person);
    (ack.0)(Ok(None));
    assert!(
        Deadline::after(WAIT)
            .recv(&accepted)
            .expect("the reply is answered"),
        "the reply is accepted once the line is in the log"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&second)
            .expect("the command returns")
            .unwrap(),
        "true"
    );
    // A's ask is still pending: a fitting reply resolves it too.
    let (tx, _) = mpsc::channel();
    let ack = Ack(Box::new(move |_| {
        let _sent = tx.send(());
    }));
    assert!(
        session
            .reply(
                Reply {
                    request_id: id_a,
                    answer: ReplyAnswer::Confirmed { confirmed: false },
                },
                ack
            )
            .is_none()
    );
    let Delivery::Resolved(_, ack) = Deadline::after(WAIT)
        .recv(&rx)
        .expect("the Resolved arrives")
    else {
        panic!("a Resolved arrives");
    };
    (ack.0)(Ok(None));
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&first)
            .expect("the command returns")
            .unwrap(),
        "false"
    );
}

#[test]
fn a_reply_no_extension_holds_is_handed_back() {
    let home = Home::new();
    home.install("a", &asking("a"));
    let session = home.load();
    session.answerable(true);
    let (tx, _) = mpsc::channel();
    let ack = Ack(Box::new(move |_| {
        let _sent = tx.send(());
    }));
    assert!(
        session
            .reply(
                Reply {
                    request_id: RequestId("r_other".into()),
                    answer: ReplyAnswer::Confirmed { confirmed: true },
                },
                ack
            )
            .is_some(),
        "an unknown id is handed back for the loop's stale_request"
    );
}
