use super::HubEventKind;

#[test]
fn every_documented_hub_name_maps_to_its_member() {
    for (name, expected) in [
        ("hub_hello", HubEventKind::HubHello),
        ("attention", HubEventKind::Attention),
        ("device_changed", HubEventKind::DeviceChanged),
        ("session_left", HubEventKind::SessionLeft),
        ("session_deleted", HubEventKind::SessionDeleted),
        ("session_archived", HubEventKind::SessionArchived),
        ("stream_closed", HubEventKind::StreamClosed),
        ("stream_reopened", HubEventKind::StreamReopened),
        ("command_accepted", HubEventKind::CommandAccepted),
        ("command_rejected", HubEventKind::CommandRejected),
        ("keepers", HubEventKind::Keepers),
    ] {
        assert_eq!(HubEventKind::from(name.to_owned()), expected, "{name}");
    }
}

#[test]
fn an_unfamiliar_name_keeps_its_exact_text() {
    assert_eq!(
        HubEventKind::from("future_kind".to_owned()),
        HubEventKind::Unknown("future_kind".to_owned())
    );
}

#[test]
fn near_miss_spellings_never_alias_a_known_member() {
    for name in [
        "Hub_Hello",
        "HUB_HELLO",
        "hub_hello ",
        " hub_hello",
        "hub_hello2",
        "command_accepted2",
        "commandaccepted",
        "",
        " ",
    ] {
        let parsed = HubEventKind::from(name.to_owned());
        assert_eq!(parsed, HubEventKind::Unknown(name.to_owned()), "{name}");
        assert!(!matches!(
            parsed,
            HubEventKind::HubHello
                | HubEventKind::Attention
                | HubEventKind::DeviceChanged
                | HubEventKind::SessionLeft
                | HubEventKind::StreamClosed
                | HubEventKind::StreamReopened
                | HubEventKind::CommandAccepted
                | HubEventKind::CommandRejected
                | HubEventKind::Keepers
        ));
    }
}

#[test]
fn unicode_names_keep_their_exact_text() {
    for name in ["hup_héllo", "未来", "hub_hello🎉"] {
        assert_eq!(
            HubEventKind::from(name.to_owned()),
            HubEventKind::Unknown(name.to_owned()),
            "{name}"
        );
    }
}

#[test]
fn every_documented_hub_name_round_trips_through_name() {
    for name in [
        "hub_hello",
        "attention",
        "device_changed",
        "session_left",
        "session_deleted",
        "session_archived",
        "stream_closed",
        "stream_reopened",
        "command_accepted",
        "command_rejected",
        "keepers",
    ] {
        let kind = HubEventKind::from(name.to_owned());
        assert_eq!(kind.name(), name, "{name}");
        assert!(
            !matches!(kind, HubEventKind::Unknown(_)),
            "documented name became Unknown: {name}"
        );
    }
}

#[test]
fn an_unfamiliar_hub_name_keeps_its_exact_text_through_name() {
    for name in [
        "future_kind",
        "future_kind_é",
        "hup_héllo",
        "未来",
        "hub_hello🎉",
        "Hub_Hello",
        "HUB_HELLO",
        "hub_hello ",
        " hub_hello",
        "hub_hello2",
        "command_accepted2",
        "commandaccepted",
        "",
        " ",
        " \t",
    ] {
        let kind = HubEventKind::from(name.to_owned());
        assert_eq!(kind, HubEventKind::Unknown(name.to_owned()), "{name}");
        assert_eq!(kind.name(), name, "{name}");
    }
}
