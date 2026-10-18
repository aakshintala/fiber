//! Command-line details a socket test cannot see.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

#[allow(clippy::duplicate_mod, reason = "unit tests share the socket fixture")]
#[path = "../tests/support/mod.rs"]
mod support;

#[test]
fn reload_delivers_an_ack_that_accepts_rejects_or_closes_when_dropped() {
    use contract::ErrorCode;
    use contract::inbox::{Delivery, Rejection};
    use fakes::{Client, Deadline};
    use std::sync::Arc;
    use support::{DEADLINE, Opened, rejection, response, send, subscribe};
    for answer in ["accept", "reject", "drop"] {
        let opened = Opened::open(Vec::new());
        let socket = opened.socket.clone();
        opened
            .session
            .run(Vec::new(), Arc::new(|| false), move |inbox| {
                let client = Client::connect(&socket).unwrap();
                subscribe(&client, "sub", "summary");
                send(&client, r#"{"id":"reload","command":"reload"}"#);
                let Delivery::Reload(ack) = Deadline::after(DEADLINE)
                    .recv(&inbox)
                    .expect("reload reached the inbox")
                else {
                    panic!("reload delivery");
                };
                match answer {
                    "accept" => ack.0(Ok(None)),
                    "reject" => ack.0(Err(Rejection {
                        code: ErrorCode::ConfigInvalid,
                        message: "bad config file".into(),
                    })),
                    _ => drop(ack),
                }
                let line = response(&client, "reload");
                match answer {
                    "accept" => {
                        assert_eq!(support::kind(&line), "command_accepted");
                        assert!(line["payload"].get("result").is_none());
                    }
                    "reject" => assert_eq!(rejection(&line), ("config_invalid", "bad config file")),
                    _ => assert_eq!(rejection(&line).0, "closing"),
                }
                send(
                    &client,
                    r#"{"id":"args","command":"reload","args":{"unexpected":true}}"#,
                );
                assert_eq!(rejection(&response(&client, "args")).0, "invalid_arguments");
                Ok(())
            })
            .unwrap();
        opened.close();
    }
}
