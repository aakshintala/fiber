//! The protocol tags of the settings types: the wire carries them, so each
//! is pinned by its text.

use super::{SettingControl, SettingsTab};

#[test]
fn each_tab_has_its_protocol_tag_and_the_tag_names_it_back() {
    let tags = [
        (SettingsTab::Appearance, "appearance"),
        (SettingsTab::Notifications, "notifications"),
        (SettingsTab::Models, "models"),
        (SettingsTab::Permissions, "permissions"),
        (SettingsTab::Sessions, "sessions"),
        (SettingsTab::Advanced, "advanced"),
    ];
    assert_eq!(tags.len(), SettingsTab::ALL.len());
    for (tab, tag) in tags {
        assert_eq!(tab.tag(), tag);
        assert_eq!(SettingsTab::from_tag(tag), Some(tab), "{tag}");
    }
    assert_eq!(SettingsTab::from_tag("nonsense"), None);
    assert_eq!(SettingsTab::from_tag(""), None);
}

#[test]
fn each_control_has_its_protocol_tag() {
    let tags = [
        (SettingControl::Boolean, "boolean"),
        (SettingControl::Number, "number"),
        (SettingControl::Text, "text"),
        (SettingControl::Fixed(vec!["a".to_owned()]), "fixed"),
        (SettingControl::Model, "model"),
        (SettingControl::Theme, "theme"),
        (SettingControl::StringList, "string_list"),
        (
            SettingControl::RepositoryExtensions,
            "repository_extensions",
        ),
        (SettingControl::Json, "json"),
    ];
    for (control, tag) in tags {
        assert_eq!(control.tag(), tag);
    }
}
