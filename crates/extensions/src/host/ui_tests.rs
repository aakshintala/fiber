//! `host.status`, `host.widget` and `host.emit` (`docs/extensions.md`,
//! "Commands and screens"): each emits exactly its `ExtensionUi` /
//! `ExtensionMessage`; a call in `init.lua` is buffered and flushed on
//! `emit_to` in order; after drop nothing is emitted; each argument error
//! raises.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::{Arc, Mutex};

use contract::emit::Emit;
use contract::events::{Event, ExtensionMessage, ExtensionUi, Ui};
use fakes::clock::FakeClock;
use mlua::{Lua, StdLib};

use super::install;
use crate::lua::Hub;

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
}

impl Emit for Recorder {
    fn emit(&self, event: &Event) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn installed() -> (Lua, Arc<Hub>) {
    let lua = Lua::new_with(
        StdLib::ALL_SAFE & !StdLib::IO & !StdLib::OS & !StdLib::PACKAGE,
        mlua::LuaOptions::new().catch_rust_panics(false),
    )
    .unwrap();
    let hub = Hub::new(FakeClock::new());
    let host = lua.create_table().unwrap();
    install(&lua, &host, &hub, "fiber.test/a").unwrap();
    lua.globals().set("host", host).unwrap();
    (lua, hub)
}

fn ui_of(event: &Event) -> ExtensionUi {
    let Event::ExtensionUi(ui) = event else {
        panic!("expected extension_ui, got {event:?}");
    };
    ui.clone()
}

fn message_of(event: &Event) -> ExtensionMessage {
    let Event::ExtensionMessage(message) = event else {
        panic!("expected extension_message, got {event:?}");
    };
    message.clone()
}

#[test]
fn status_emits_exactly_its_extension_ui() {
    let (lua, hub) = installed();
    let recorder = Arc::new(Recorder::default());
    hub.set_emit(recorder.clone() as Arc<dyn Emit>);
    lua.load(r#"host.status("syncing 3/10")"#).exec().unwrap();
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        ui_of(&events[0]),
        ExtensionUi {
            extension: "fiber.test/a".to_owned(),
            ui: Ui::Status {
                status: "syncing 3/10".to_owned(),
            },
        }
    );
}

#[test]
fn widget_emits_exactly_its_extension_ui() {
    let (lua, hub) = installed();
    let recorder = Arc::new(Recorder::default());
    hub.set_emit(recorder.clone() as Arc<dyn Emit>);
    lua.load(r#"host.widget("models", {"gpt-x", "claude-y"})"#)
        .exec()
        .unwrap();
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        ui_of(&events[0]),
        ExtensionUi {
            extension: "fiber.test/a".to_owned(),
            ui: Ui::Widget {
                widget: "models".to_owned(),
                lines: vec!["gpt-x".to_owned(), "claude-y".to_owned()],
            },
        }
    );
}

#[test]
fn emit_sends_its_data_as_an_extension_message() {
    let (lua, hub) = installed();
    let recorder = Arc::new(Recorder::default());
    hub.set_emit(recorder.clone() as Arc<dyn Emit>);
    lua.load(r#"host.emit({ picked = 2 })"#).exec().unwrap();
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    let message = message_of(&events[0]);
    assert_eq!(message.extension, "fiber.test/a");
    assert_eq!(message.data, serde_json::json!({"picked": 2}),);
}

#[test]
fn calls_before_emit_to_are_buffered_and_flushed_in_order() {
    // A call in `init.lua` is buffered and flushed on `emit_to` in order.
    let (lua, hub) = installed();
    lua.load(r#"host.status("first")"#).exec().unwrap();
    lua.load(r#"host.widget("w", {"a"})"#).exec().unwrap();
    lua.load(r#"host.status("second")"#).exec().unwrap();
    let recorder = Arc::new(Recorder::default());
    hub.set_emit(recorder.clone() as Arc<dyn Emit>);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(
        ui_of(&events[0]),
        ExtensionUi {
            extension: "fiber.test/a".to_owned(),
            ui: Ui::Status {
                status: "first".to_owned(),
            },
        }
    );
    assert_eq!(
        ui_of(&events[1]),
        ExtensionUi {
            extension: "fiber.test/a".to_owned(),
            ui: Ui::Widget {
                widget: "w".to_owned(),
                lines: vec!["a".to_owned()],
            },
        }
    );
    assert_eq!(
        ui_of(&events[2]),
        ExtensionUi {
            extension: "fiber.test/a".to_owned(),
            ui: Ui::Status {
                status: "second".to_owned(),
            },
        }
    );
}

#[test]
fn after_seal_nothing_is_emitted() {
    // After `seal`, a resumed emission reaches neither the emitter nor anyone.
    let (lua, hub) = installed();
    let recorder = Arc::new(Recorder::default());
    hub.set_emit(recorder.clone() as Arc<dyn Emit>);
    hub.seal();
    lua.load(r#"host.status("late")"#).exec().unwrap();
    assert!(recorder.events.lock().unwrap().is_empty());
}

#[test]
fn status_with_a_non_string_raises() {
    let (lua, _hub) = installed();
    let err = lua.load(r#"host.status(42)"#).exec().unwrap_err();
    assert!(err.to_string().contains("must be a string"), "{err}");
}

#[test]
fn widget_with_a_non_string_id_raises() {
    let (lua, _hub) = installed();
    let err = lua.load(r#"host.widget(42, {})"#).exec().unwrap_err();
    assert!(err.to_string().contains("must be a string"), "{err}");
}

#[test]
fn widget_with_non_string_lines_raises() {
    let (lua, _hub) = installed();
    let err = lua
        .load(r#"host.widget("w", {"ok", 42})"#)
        .exec()
        .unwrap_err();
    assert!(
        err.to_string().contains("must be a list of strings"),
        "{err}"
    );
}

#[test]
fn emit_of_a_function_raises() {
    let (lua, hub) = installed();
    let recorder = Arc::new(Recorder::default());
    hub.set_emit(recorder.clone() as Arc<dyn Emit>);
    let err = lua.load(r#"host.emit(print)"#).exec().unwrap_err();
    assert!(err.to_string().contains("host.emit"), "{err}");
    assert!(recorder.events.lock().unwrap().is_empty());
}
