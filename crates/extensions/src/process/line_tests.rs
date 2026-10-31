use super::{ProcessCallLine, ProcessLineKind};

#[test]
fn every_line_kind_names_its_wire_name() {
    for (kind, name) in [
        (ProcessLineKind::ExtensionHello, "extension_hello"),
        (ProcessLineKind::ExtensionRequest, "extension_request"),
        (ProcessLineKind::CommandAccepted, "command_accepted"),
        (ProcessLineKind::CommandRejected, "command_rejected"),
    ] {
        assert_eq!(kind.name(), name);
    }
}

#[test]
fn the_serialized_line_carries_the_kind_name() {
    for (kind, name) in [
        (ProcessLineKind::ExtensionHello, "extension_hello"),
        (ProcessLineKind::ExtensionRequest, "extension_request"),
        (ProcessLineKind::CommandAccepted, "command_accepted"),
        (ProcessLineKind::CommandRejected, "command_rejected"),
    ] {
        let line = ProcessCallLine {
            kind,
            ts: 7,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload: serde_json::Map::new(),
        };
        let value: serde_json::Value =
            serde_json::from_slice(&line.to_vec()).expect("the line serializes as JSON");
        assert_eq!(value["kind"], serde_json::Value::String(name.to_owned()));
    }
}
