use super::*;
use crate::switch::{
    Door,
    tests::{fixture, switching},
};
use contract::commands::ModelArgs;
use fakes::Deadline;
use std::time::Duration;

fn readers(switching: &Arc<Switching>, started: &str) -> CredentialReaders {
    switching.credential_readers(
        switching.clone().closure(Door {
            declare: Arc::new(|_, _| {}),
            hosted_stands: false,
            backend: None,
        }),
        started.into(),
    )
}

fn prepare(
    readers: &CredentialReaders,
    model: &str,
    label: Option<&str>,
) -> Result<r#loop::Prepared, contract::inbox::Rejection> {
    (readers.prepare)(
        &ModelArgs {
            model: model.into(),
            thinking: None,
        },
        label,
        None,
    )
}

#[test]
fn labels_name_the_last_admitted_provider_and_only_the_applied_label() {
    fakes::within(
        "credential readers",
        Deadline::after(Duration::from_secs(20)),
        || {
            let fixture = fixture("fiber-credential-readers");
            let switching = switching(
                &fixture,
                &[
                    "providers.fake.credentials.home.env=PATH",
                    "providers.fake.credentials.work.env=PATH",
                    "providers.other.credentials.day.env=PATH",
                    "providers.other.credentials.night.env=PATH",
                    "providers.other.credential=day",
                ],
            );
            switching.repoint("fake", "home");
            let readers = readers(&switching, "fake/m");
            assert_eq!((readers.admitted)(), "fake");
            assert_eq!(
                (readers.labels)().unwrap(),
                CredentialLabels {
                    provider: "fake".into(),
                    credentials: vec!["home".into(), "work".into()],
                    credential: Some("home".into()),
                }
            );
            let mut work = prepare(&readers, "fake/m", Some("work")).unwrap();
            assert_eq!(
                (readers.labels)().unwrap().credential.as_deref(),
                Some("home")
            );
            work.applied.take().unwrap()();
            assert_eq!(
                (readers.labels)().unwrap().credential.as_deref(),
                Some("work")
            );
            let mut other = prepare(&readers, "other/m", None).unwrap();
            let expected = CredentialLabels {
                provider: "other".into(),
                credentials: vec!["day".into(), "default".into(), "night".into()],
                credential: None,
            };
            assert_eq!((readers.admitted)(), "other");
            assert_eq!((readers.labels)().unwrap(), expected);
            let mut night = prepare(&readers, "other/m", Some("night")).unwrap();
            assert_eq!((readers.admitted)(), "other");
            assert_eq!((readers.labels)().unwrap(), expected);
            let refused = prepare(&readers, "fake/unknown", None).err().unwrap();
            assert_eq!(refused.code, contract::ErrorCode::InvalidArguments);
            assert_eq!((readers.admitted)(), "other");
            assert_eq!((readers.labels)().unwrap(), expected);
            let refused = prepare(&readers, "fake/m", Some("absent")).err().unwrap();
            assert_eq!(refused.code, contract::ErrorCode::CredentialMissing);
            assert_eq!((readers.admitted)(), "other");
            assert_eq!((readers.labels)().unwrap(), expected);
            other.applied.take().unwrap()();
            assert_eq!(
                (readers.labels)().unwrap().credential.as_deref(),
                Some("day")
            );
            night.applied.take().unwrap()();
            assert_eq!(
                (readers.labels)().unwrap().credential.as_deref(),
                Some("night")
            );
            assert_eq!((readers.admitted)(), "other");
        },
    );
}

#[test]
fn scripted_and_unknown_providers_have_no_labels_even_with_a_current_entry() {
    let fixture = fixture("fiber-credential-readers-empty");
    let mut switching = switching(&fixture, &[]);
    Arc::get_mut(&mut switching)
        .unwrap()
        .registry
        .add_scripted("scripted/demo");
    for provider in ["scripted", "absent"] {
        switching.repoint(provider, "ignored");
        let readers = readers(&switching, &format!("{provider}/demo"));
        assert_eq!((readers.admitted)(), provider);
        assert_eq!(
            (readers.labels)().unwrap(),
            CredentialLabels {
                provider: provider.into(),
                credentials: vec![],
                credential: None
            }
        );
    }
}

#[test]
fn readers_release_switching_but_keep_the_admitted_provider() {
    let fixture = fixture("fiber-credential-readers-ended");
    let switching = switching(&fixture, &[]);
    let CredentialReaders {
        prepare,
        labels,
        admitted,
    } = readers(&switching, "fake/m");
    assert_eq!(labels().unwrap().credential.as_deref(), Some("default"));
    drop(prepare);
    drop(switching);
    assert_eq!(labels(), None);
    assert_eq!(admitted(), "fake");
}
