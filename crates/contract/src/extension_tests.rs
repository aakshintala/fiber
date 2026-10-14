use super::*;
use crate::RequestId;
use crate::commands::{Reply, ReplyAnswer};
use crate::inbox::Ack;
use crate::shapes::True;

struct StubDoor;

impl ExtensionDoor for StubDoor {
    fn command(
        &self,
        _id: &crate::CommandId,
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
fn default_reply_hands_the_reply_back() {
    let door = StubDoor;
    let reply = Reply {
        request_id: RequestId("r_1".into()),
        answer: ReplyAnswer::Declined { declined: True },
    };
    let ack = Ack(Box::new(|_| {}));
    let Some((back, _ack)) = door.reply(reply, ack) else {
        panic!("default reply dropped the reply");
    };
    assert_eq!(back.request_id, RequestId("r_1".into()));
}
