use super::HubEventKind;

#[test]
fn every_documented_hub_name_maps_to_its_member() {
    for (name, expected) in [
        ("hub_hello", HubEventKind::HubHello),
        ("attention", HubEventKind::Attention),
        ("device_changed", HubEventKind::DeviceChanged),
        ("session_left", HubEventKind::SessionLeft),
        ("stream_closed", HubEventKind::StreamClosed),
        ("stream_reopened", HubEventKind::StreamReopened),
        ("command_accepted", HubEventKind::CommandAccepted),
        ("command_rejected", HubEventKind::CommandRejected),
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
