//! Tests beside [`super::content`]: sent parts become logged parts through
//! a fake [`Images`].

use std::collections::VecDeque;
use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::ErrorCode;
use contract::commands::SentPart;
use contract::images::{ImageError, Images};
use contract::provider::ImageRef;
use contract::shapes::ContentPart;
use contract::tool::Cancel;
use fakes::CancelToken;

use super::content;

struct Fake {
    seen: Mutex<Vec<(Vec<u8>, bool)>>,
    next: Mutex<VecDeque<Result<ImageRef, ImageError>>>,
}

impl Fake {
    fn returning(results: Vec<Result<ImageRef, ImageError>>) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            next: Mutex::new(results.into()),
        }
    }

    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn bytes(&self, index: usize) -> Vec<u8> {
        self.seen.lock().unwrap()[index].0.clone()
    }

    fn saw_cancelled(&self, index: usize) -> bool {
        self.seen.lock().unwrap()[index].1
    }
}

impl Images for Fake {
    fn process(&self, bytes: &[u8], cancel: &dyn Cancel) -> Result<ImageRef, ImageError> {
        self.seen
            .lock()
            .unwrap()
            .push((bytes.to_vec(), cancel.is_cancelled()));
        self.next.lock().unwrap().pop_front().unwrap()
    }
}

fn stored(path: &str) -> ImageRef {
    ImageRef {
        path: path.to_owned(),
        mime_type: "image/png".to_owned(),
        width: 1,
        height: 1,
    }
}

fn text(text: &str) -> SentPart {
    SentPart::Text {
        text: text.to_owned(),
    }
}

fn image(data: &str) -> SentPart {
    SentPart::Image {
        data: data.to_owned(),
        mime_type: "image/png".to_owned(),
    }
}

#[test]
fn text_only_passes_through_without_calling_the_child() {
    let fake = Fake::returning(vec![]);
    let got = content(vec![text("hi")], Some(&fake), &CancelToken::new()).unwrap();
    assert_eq!(got, vec![ContentPart::Text { text: "hi".into() }]);
    assert_eq!(fake.calls(), 0);
}

#[test]
fn text_then_image_yields_text_then_the_stored_image() {
    let fake = Fake::returning(vec![Ok(stored("artifacts/i_1.png"))]);
    let data = STANDARD.encode(b"bytes");
    let got = content(
        vec![text("look"), image(&data)],
        Some(&fake),
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(
        got,
        vec![
            ContentPart::Text {
                text: "look".into()
            },
            ContentPart::Image {
                path: "artifacts/i_1.png".into(),
                mime_type: "image/png".into(),
                width: 1,
                height: 1,
            },
        ]
    );
    assert_eq!(fake.bytes(0), b"bytes");
}

#[test]
fn invalid_base64_is_invalid_arguments_naming_the_image() {
    let fake = Fake::returning(vec![]);
    let Err((code, message)) = content(vec![image("!!!")], Some(&fake), &CancelToken::new()) else {
        panic!("rejected");
    };
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert_eq!(message, "Image 1 cannot be read: its data is not base64.");
    assert_eq!(fake.calls(), 0);
}

#[test]
fn empty_data_reaches_the_child_as_empty_bytes() {
    let fake = Fake::returning(vec![Err(ImageError::Unreadable("empty".into()))]);
    let Err((code, message)) = content(vec![image("")], Some(&fake), &CancelToken::new()) else {
        panic!("rejected");
    };
    assert_eq!(fake.bytes(0), Vec::<u8>::new());
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert_eq!(message, "Image 1 cannot be read: empty");
}

#[test]
fn unreadable_maps_to_invalid_arguments() {
    let fake = Fake::returning(vec![Err(ImageError::Unreadable("no pixels".into()))]);
    let Err((code, message)) = content(vec![image("YQ==")], Some(&fake), &CancelToken::new())
    else {
        panic!("rejected");
    };
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert_eq!(message, "Image 1 cannot be read: no pixels");
}

#[test]
fn failed_maps_to_io_failed() {
    let fake = Fake::returning(vec![Err(ImageError::Failed("boom".into()))]);
    let Err((code, message)) = content(vec![image("YQ==")], Some(&fake), &CancelToken::new())
    else {
        panic!("rejected");
    };
    assert_eq!(code, ErrorCode::IoFailed);
    assert_eq!(message, "Image 1 could not be processed: boom");
}

#[test]
fn cancelled_maps_to_closing() {
    let fake = Fake::returning(vec![Err(ImageError::Cancelled)]);
    let Err((code, message)) = content(vec![image("YQ==")], Some(&fake), &CancelToken::new())
    else {
        panic!("rejected");
    };
    assert_eq!(code, ErrorCode::Closing);
    assert_eq!(
        message,
        "Image 1 was not processed: the session is closing."
    );
}

#[test]
fn the_second_image_is_named_image_2_and_the_third_never_runs() {
    let fake = Fake::returning(vec![
        Ok(stored("artifacts/i_1.png")),
        Err(ImageError::Unreadable("bad".into())),
    ]);
    let Err((code, message)) = content(
        vec![image("YQ=="), text("mid"), image("YQ=="), image("YQ==")],
        Some(&fake),
        &CancelToken::new(),
    ) else {
        panic!("rejected");
    };
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert_eq!(message, "Image 2 cannot be read: bad");
    assert_eq!(fake.calls(), 2);
}

#[test]
fn a_first_failure_stops_before_the_second_image() {
    let fake = Fake::returning(vec![Err(ImageError::Unreadable("bad".into()))]);
    let Err((_, message)) = content(
        vec![image("!!!"), image("YQ==")],
        Some(&fake),
        &CancelToken::new(),
    ) else {
        panic!("rejected");
    };
    assert!(message.contains("Image 1"), "{message}");
    assert_eq!(fake.calls(), 0);
    let failing = Fake::returning(vec![Err(ImageError::Unreadable("bad".into()))]);
    let Err((_, _)) = content(
        vec![image("YQ=="), image("YQ==")],
        Some(&failing),
        &CancelToken::new(),
    ) else {
        panic!("rejected");
    };
    assert_eq!(failing.calls(), 1);
}

#[test]
fn no_images_wired_keeps_todays_message() {
    let Err((code, message)) = content(vec![image("YQ==")], None, &CancelToken::new()) else {
        panic!("rejected");
    };
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert_eq!(
        message,
        "Image 1 cannot be read: this Fiber processes no images yet."
    );
}

#[test]
fn the_child_sees_whether_the_session_is_closing() {
    let fake = Fake::returning(vec![Ok(stored("artifacts/i_1.png"))]);
    content(vec![image("YQ==")], Some(&fake), &CancelToken::new()).unwrap();
    assert!(!fake.saw_cancelled(0));
    let closing = Fake::returning(vec![Ok(stored("artifacts/i_2.png"))]);
    let cancel = CancelToken::new();
    cancel.cancel();
    content(vec![image("YQ==")], Some(&closing), &cancel).unwrap();
    assert!(closing.saw_cancelled(0));
}
