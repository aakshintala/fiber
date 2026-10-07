//! The default `reply` hands the reply and its ack back unchanged
//! (`docs/extensions.md`, "Commands and screens").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use super::*;

struct Door;

impl ExtensionDoor for Door {
    fn command(
        &self,
        _name: &str,
        _text: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, crate::inbox::Rejection> {
        Err(crate::inbox::Rejection {
            code: crate::ErrorCode::UnknownCommand,
            message: "unknown".into(),
        })
    }

    fn seal(&self) {}
}

#[test]
fn the_default_reply_hands_the_reply_and_ack_back() {
    let door = Door;
    let reply = crate::commands::Reply {
        request_id: crate::RequestId("r_test".into()),
        answer: crate::commands::ReplyAnswer::Declined {
            declined: crate::shapes::True,
        },
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let ack = crate::inbox::Ack(Box::new(move |answer| {
        let _sent = tx.send(answer.is_ok());
    }));
    let (back_reply, back_ack) = door.reply(reply, ack).expect("the default hands back");
    assert_eq!(back_reply.request_id, crate::RequestId("r_test".into()));
    (back_ack.0)(Ok(None));
    assert!(rx.recv().unwrap(), "the handed-back ack still answers");
}
