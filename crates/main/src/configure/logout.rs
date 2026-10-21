//! `/logout` through the delete path `fiber logout` takes
//! (`docs/invocation.md`, "Commands and flags"): the rows list every stored
//! provider label and declared secret, and the delete removes the file only,
//! leaving `providers."<name>".credential` as is. No key or secret reaches
//! stdout, stderr, `Debug` or a log.

use std::path::Path;

use config::{Config, Origin, Sources};
use contract::shapes::Failure;

use super::{from_config, from_failure};
use tui::{ConfigureError, LoginKind, LogoutRow};

/// The configuration the rows read: Fiber home stands in for the
/// workspace, so the global layer always loads; the project layer is
/// home's own. `fiber login` writes the selected label to the global
/// layer, so the default mark reads it here.
fn load(home: &Path) -> Result<Config, ConfigureError> {
    let (_, project) = ::cli::project_of(home, home).map_err(from_failure)?;
    Config::load(Sources {
        home: home.to_path_buf(),
        workspace: home.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .map_err(from_config)
}

/// Where a non-deletable label's key comes from, described without its
/// secret: the program alone for a command, since its arguments may hold
/// a key.
fn describe(origin: &Origin) -> String {
    match origin {
        Origin::Stored => String::new(),
        Origin::Env(var) => format!("environment variable {var}"),
        Origin::File(file) => format!("file {}", file.display()),
        Origin::Command(program) => format!("command {program}"),
    }
}

/// One provider's rows: one per label it has, stored or configured, the
/// selected label first and the rest sorted. A label whose file vanished
/// between the listing and the check lists nothing.
fn provider_rows(
    config: &Config,
    providers: &extensions::Providers,
    name: &str,
    kind: LoginKind,
) -> Vec<LogoutRow> {
    let Some(data) = providers.get(name) else {
        return Vec::new();
    };
    let stored = data
        .credential_name
        .as_deref()
        .unwrap_or(&data.name)
        .to_owned();
    // The siblings `fiber logout` names in its removal message: the
    // providers reading the same stored directory.
    let siblings: Vec<String> = providers
        .names()
        .filter(|other| *other != name)
        .filter(|other| {
            providers.get(other).is_some_and(|sibling| {
                sibling.credential_name.as_deref().unwrap_or(&sibling.name) == stored
            })
        })
        .map(str::to_owned)
        .collect();
    let selected = config.credentials().credential_label(data);
    let mut labels = config.credentials().credential_origins(data);
    // The selected label first, the rest in label order: origins arrive
    // sorted, so the sort only moves the selected one.
    labels.sort_by(|left, right| {
        (left.0 != selected)
            .cmp(&(right.0 != selected))
            .then(left.0.cmp(&right.0))
    });
    labels
        .into_iter()
        .filter_map(|(label, origin)| match origin {
            Ok(Origin::Stored) => Some(LogoutRow {
                name: name.to_owned(),
                label: Some(label.clone()),
                kind,
                default: label == selected,
                deletable: true,
                origin: None,
                siblings: siblings.clone(),
            }),
            Ok(origin) => Some(LogoutRow {
                name: name.to_owned(),
                label: Some(label.clone()),
                kind,
                default: label == selected,
                deletable: false,
                origin: Some(describe(&origin)),
                siblings: siblings.clone(),
            }),
            Err(_) => None,
        })
        .collect()
}

/// One declared secret's row: deletable only while a secret is stored.
fn secret_row(home: &Path, name: &str) -> Result<LogoutRow, ConfigureError> {
    let stored = config::read_secret(home, name).map_err(from_config)?;
    Ok(LogoutRow {
        name: name.to_owned(),
        label: None,
        kind: LoginKind::Secret,
        default: false,
        deletable: stored.is_some(),
        origin: None,
        siblings: Vec::new(),
    })
}

/// Every stored provider label and declared secret, providers in
/// `login_targets` order then secrets.
pub(super) fn rows(home: &Path) -> Result<Vec<LogoutRow>, ConfigureError> {
    let targets = ::cli::login_targets(home).map_err(|e| from_failure(Failure::from(e)))?;
    let config = load(home)?;
    let providers = ::cli::providers_in(home).map_err(|e| from_failure(Failure::from(e)))?;
    let mut rows = Vec::new();
    for target in &targets {
        match target {
            ::cli::LoginName::Provider(name) => {
                rows.extend(provider_rows(&config, &providers, name, LoginKind::Key));
            }
            ::cli::LoginName::Browser(name) => {
                rows.extend(provider_rows(&config, &providers, name, LoginKind::Browser));
            }
            ::cli::LoginName::Secret(name) => rows.push(secret_row(home, name)?),
        }
    }
    Ok(rows)
}

/// Deletes the stored credential `name` holds under `label` (`None` for a
/// secret): the file only, leaving `providers."<name>".credential` as is.
/// Answers the file under Fiber home, such as `credentials/acme/work`. A
/// label that vanished between the listing and the delete is the
/// no-stored-label message, as `fiber logout` says it.
pub(super) fn delete(
    home: &Path,
    name: &str,
    label: Option<String>,
) -> Result<String, ConfigureError> {
    match label {
        Some(label) => {
            let providers =
                ::cli::providers_in(home).map_err(|e| from_failure(Failure::from(e)))?;
            let Some(data) = providers.get(name) else {
                return Err(from_failure(Failure::from(
                    ::cli::CliError::ProviderNotInstalled {
                        name: name.to_owned(),
                        installed: providers.names().map(str::to_owned).collect(),
                    },
                )));
            };
            let stored = data
                .credential_name
                .as_deref()
                .unwrap_or(&data.name)
                .to_owned();
            if config::delete_credential(home, &stored, &label).map_err(from_config)? {
                Ok(format!("credentials/{stored}/{label}"))
            } else {
                Err(from_failure(Failure::from(
                    ::cli::CliError::NoStoredLabel {
                        name: name.to_owned(),
                        label,
                    },
                )))
            }
        }
        None => {
            if config::delete_secret(home, name).map_err(from_config)? {
                Ok(format!("credentials/{name}"))
            } else {
                Err(from_failure(Failure::from(
                    ::cli::CliError::NoStoredCredential {
                        name: name.to_owned(),
                    },
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! `/logout` through the seam: the rows it lists and the credentials
    //! it deletes (`docs/invocation.md`, "Commands and flags").

    use std::fs;
    use std::path::PathBuf;

    use serde_json::{Value, json};
    use tui::{Configure, LoginKind};

    use super::super::Seam;
    use crate::test_support::install_extension;

    struct Setup {
        root: fakes::TempDir,
    }

    impl Setup {
        fn new() -> Self {
            let root = fakes::TempDir::new("fiber-configure-logout");
            fs::create_dir_all(root.path().join("home")).unwrap();
            Self { root }
        }

        fn home(&self) -> PathBuf {
            self.root.path().join("home")
        }

        /// Installs a provider `name`; `credential_name` is the stored
        /// credential it reads.
        fn install(&self, name: &str, credential_name: Option<&str>) {
            let mut data = json!({
                "name": name,
                "models": [{"id": "m", "protocol": "openai-responses",
                    "base_url": "http://x/v1", "context_window": 1000}],
            });
            if let Some(stored) = credential_name {
                data["credential_name"] = json!(stored);
            }
            install_extension(
                &self.home(),
                &format!("extensions/{name}"),
                json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
                &[(name, data)],
            );
        }

        /// Installs the extension `extension` with no provider, declaring
        /// `secrets`.
        fn declare(&self, extension: &str, secrets: &[&str]) {
            install_extension(
                &self.home(),
                &format!("extensions/{extension}"),
                json!({"name": extension, "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
                    "secrets": secrets}),
                &[],
            );
        }

        fn write_config(&self, value: &Value) {
            fs::write(self.home().join("config.json"), value.to_string()).unwrap();
        }

        fn global(&self) -> Value {
            serde_json::from_str(&fs::read_to_string(self.home().join("config.json")).unwrap())
                .unwrap()
        }

        fn seam(&self) -> Seam {
            Seam::new(self.home())
        }
    }

    /// Stores `key` for `name` under `label` through the seam, as
    /// `fiber login` would, naming the first label the default.
    fn store(seam: &Seam, name: &str, label: &str, key: &str) {
        seam.store_key(name, Some(label), contract::Secret::new(key.to_owned()))
            .unwrap();
    }

    #[test]
    fn stored_labels_list_with_the_selected_label_marked_first() {
        let setup = Setup::new();
        setup.install("acme", None);
        setup.declare("acme-secrets", &["acme.api_key"]);
        let seam = setup.seam();
        store(&seam, "acme", "work", "sk-work");
        store(&seam, "acme", "home", "sk-home");
        config::store_secret(
            &setup.home(),
            "acme.api_key",
            &contract::Secret::new("sk-secret".to_owned()),
        )
        .unwrap();
        let rows = seam.logout_rows().unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| (
                    row.name.as_str(),
                    row.label.as_deref(),
                    row.kind,
                    row.default,
                    row.deletable,
                    row.origin.clone(),
                    row.siblings.clone(),
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "acme",
                    Some("work"),
                    LoginKind::Key,
                    true,
                    true,
                    None,
                    vec![]
                ),
                (
                    "acme",
                    Some("home"),
                    LoginKind::Key,
                    false,
                    true,
                    None,
                    vec![]
                ),
                (
                    "acme.api_key",
                    None,
                    LoginKind::Secret,
                    false,
                    true,
                    None,
                    vec![]
                ),
            ]
        );
    }

    #[test]
    fn delete_removes_the_file_and_leaves_the_stale_pointer() {
        let setup = Setup::new();
        setup.install("acme", None);
        let seam = setup.seam();
        store(&seam, "acme", "default", "sk-a");
        let removed = seam
            .delete_stored("acme", Some("default".to_owned()))
            .unwrap();
        assert_eq!(removed, "credentials/acme/default");
        assert!(
            !setup.home().join("credentials/acme/default").exists(),
            "the file is gone"
        );
        // The delete removes the file only: the default pointer stays,
        // as `fiber logout` leaves it.
        assert_eq!(
            setup.global(),
            json!({"providers": {"acme": {"credential": "default"}}})
        );
        assert!(seam.logout_rows().unwrap().is_empty());
    }

    #[test]
    fn secret_delete_removes_the_file() {
        let setup = Setup::new();
        setup.declare("acme-secrets", &["acme.api_key"]);
        config::store_secret(
            &setup.home(),
            "acme.api_key",
            &contract::Secret::new("sk-secret".to_owned()),
        )
        .unwrap();
        let seam = setup.seam();
        let removed = seam.delete_stored("acme.api_key", None).unwrap();
        assert_eq!(removed, "credentials/acme.api_key");
        assert!(
            !setup.home().join("credentials/acme.api_key").exists(),
            "the file is gone"
        );
        // With nothing stored the secret lists a dim row, deleted by
        // nothing.
        assert_eq!(
            seam.logout_rows().unwrap(),
            [tui::LogoutRow {
                name: "acme.api_key".to_owned(),
                label: None,
                kind: LoginKind::Secret,
                default: false,
                deletable: false,
                origin: None,
                siblings: Vec::new(),
            }]
        );
    }

    #[test]
    fn configured_sources_list_dim_rows_with_their_origin() {
        for (source, origin) in [
            (json!({"env": "ACME_KEY"}), "environment variable ACME_KEY"),
            (json!({"file": "/etc/acme.key"}), "file /etc/acme.key"),
            (
                json!({"command": ["pass", "show", "sk-live-secret-arg"]}),
                "command pass",
            ),
        ] {
            let setup = Setup::new();
            setup.install("acme", None);
            setup.write_config(&json!({"providers": {"acme": {"credentials": {
                "work": source,
            }}}}));
            let rows = setup.seam().logout_rows().unwrap();
            assert_eq!(
                rows.iter()
                    .map(|row| (
                        row.name.as_str(),
                        row.label.as_deref(),
                        row.deletable,
                        row.origin.as_deref()
                    ))
                    .collect::<Vec<_>>(),
                [("acme", Some("work"), false, Some(origin))],
                "{source:?}"
            );
            // A command's arguments may hold a key: only its program shows.
            assert!(
                rows.iter().all(|row| row
                    .origin
                    .as_deref()
                    .is_none_or(|text| !text.contains("sk-live-secret-arg"))),
                "{rows:?}"
            );
        }
    }

    #[test]
    fn a_delete_while_the_lock_is_held_is_the_holds_its_lock_error() {
        let setup = Setup::new();
        setup.install("acme", None);
        let seam = setup.seam();
        store(&seam, "acme", "default", "sk-a");
        let file = config::CredentialFile::new(&setup.home(), "acme", "default").unwrap();
        let _held = file.try_lock().unwrap().expect("the lock is free");
        let error = seam
            .delete_stored("acme", Some("default".to_owned()))
            .err()
            .unwrap();
        assert_eq!(error.code, contract::ErrorCode::IoFailed);
        assert!(
            error
                .message
                .contains("another login or logout holds its lock"),
            "{}",
            error.message
        );
        // The failed delete removed nothing.
        assert!(
            setup.home().join("credentials/acme/default").exists(),
            "the file stays"
        );
    }

    #[test]
    fn deleted_between_list_and_delete_is_the_no_stored_label_error() {
        let setup = Setup::new();
        setup.install("acme", None);
        let seam = setup.seam();
        store(&seam, "acme", "default", "sk-a");
        assert_eq!(seam.logout_rows().unwrap().len(), 1);
        fs::remove_file(setup.home().join("credentials/acme/default")).unwrap();
        let error = seam
            .delete_stored("acme", Some("default".to_owned()))
            .err()
            .unwrap();
        assert_eq!(error.code, contract::ErrorCode::CredentialMissing);
        assert_eq!(error.message, "no stored credential default for acme");
    }

    #[test]
    fn deleting_an_unknown_provider_names_the_installed_ones() {
        let setup = Setup::new();
        setup.install("acme", None);
        let error = setup
            .seam()
            .delete_stored("nope", Some("default".to_owned()))
            .err()
            .unwrap();
        assert_eq!(error.code, contract::ErrorCode::Usage);
        assert!(error.message.contains("nope"), "{}", error.message);
        assert!(error.message.contains("acme"), "{}", error.message);
    }

    #[test]
    fn a_link_at_the_stored_path_lists_no_row_for_the_label() {
        let setup = Setup::new();
        setup.install("acme", None);
        setup.write_config(&json!({"providers": {"acme": {"credentials": {
            "work": {"env": "ACME_KEY"},
        }}}}));
        // A link the listing refuses: neither a stored row to delete
        // nor a read the delete could take.
        let dir = setup.home().join("credentials/acme");
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink("/etc/acme.key", dir.join("work")).unwrap();
        assert!(setup.seam().logout_rows().unwrap().is_empty());
    }

    #[test]
    fn siblings_sharing_the_stored_name_are_listed_on_the_row() {
        let setup = Setup::new();
        setup.install("acme", None);
        setup.install("beta", Some("acme"));
        let seam = setup.seam();
        store(&seam, "acme", "default", "sk-a");
        let rows = seam.logout_rows().unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.siblings.clone()))
                .collect::<Vec<_>>(),
            [
                ("acme", vec!["beta".to_owned()]),
                ("beta", vec!["acme".to_owned()]),
            ]
        );
    }

    #[test]
    fn deleting_a_stored_label_covered_by_a_source_leaves_the_dim_row() {
        let setup = Setup::new();
        setup.install("acme", None);
        let key_file = setup.home().join("acme.key");
        fs::write(&key_file, "sk-file").unwrap();
        setup.write_config(&json!({"providers": {"acme": {"credentials": {
            "work": {"file": key_file.to_str().unwrap()},
        }}}}));
        let seam = setup.seam();
        config::store_credential(
            &setup.home(),
            "acme",
            "work",
            &contract::Secret::new("sk-stored".to_owned()),
        )
        .unwrap();
        let before = seam.logout_rows().unwrap();
        assert!(
            before
                .iter()
                .any(|row| row.label.as_deref() == Some("work") && row.deletable),
            "{before:?}"
        );
        assert_eq!(
            seam.delete_stored("acme", Some("work".to_owned())).unwrap(),
            "credentials/acme/work"
        );
        // The stored row is gone; the configured source's dim row
        // remains, and later sessions read the source.
        let after = seam.logout_rows().unwrap();
        assert_eq!(after.len(), 1);
        assert!(!after[0].deletable);
        assert_eq!(
            after[0].origin.as_deref(),
            Some(format!("file {}", key_file.display()).as_str())
        );
        let config = super::load(&setup.home()).unwrap();
        let providers = ::cli::providers_in(&setup.home()).unwrap();
        let data = providers.get("acme").unwrap();
        let read = config
            .credentials()
            .credential_with(data, "work", &|_, _| {
                unreachable!("a file source runs no command")
            })
            .unwrap();
        assert_eq!(read.secret.expose(), "sk-file");
    }
}
