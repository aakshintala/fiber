#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::os::unix::fs::symlink;

use contract::ErrorCode;
use fakes::TempDir;

use super::*;
use crate::{Config, ProjectKey, Sources};

fn secret_config(root: &TempDir) -> (Config, std::path::PathBuf) {
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    let config = Config::load(Sources {
        home: home.clone(),
        workspace,
        project: ProjectKey::new("-Users-alice-work-app-.git").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    (config, home)
}

#[test]
fn a_path_that_cannot_be_canonicalised_is_never_read() {
    let root = TempDir::new("cred-canonical");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    let config = Config::load(Sources {
        home,
        workspace,
        project: ProjectKey::new("-Users-alice-work-app-.git").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let planted = root.path().join("planted");
    fs::write(&planted, "planted-key").unwrap();
    let path = root.path().join("key");
    let provider = ProviderData {
        name: "acme".into(),
        credential: Some(CredentialSource::File(path.clone())),
        credential_name: None,
        headers: Default::default(),
        placeholders: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
        login: None,
        credential_label: None,
    };
    // Canonicalising fails, and before it returns another process points
    // the path at a key file.
    let canonical = |file: &Path| -> io::Result<PathBuf> {
        symlink(&planted, file).unwrap();
        Err(io::Error::from(ErrorKind::NotFound))
    };
    let run =
        |_: &mut Command, _: Duration| -> io::Result<Output> { unreachable!("a file source") };
    let err = config
        .credentials()
        .credential_through(&provider, "default", &run, &canonical)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
    let message = err.to_string();
    assert!(message.contains("does not exist"), "{message}");
    assert!(!message.contains("planted-key"), "{message}");
    // The path now resolves to the key, which was never read.
    assert_eq!(fs::read_to_string(&path).unwrap(), "planted-key");
}

#[test]
fn secret_reads_a_stored_secret_and_none_when_absent() {
    let root = TempDir::new("cred-secret");
    let (config, home) = secret_config(&root);
    assert!(
        config
            .credentials()
            .secret("mcp.docs.TOKEN")
            .unwrap()
            .is_none()
    );
    crate::secret::store_secret(&home, "mcp.docs.TOKEN", &Secret::new("tok-1".into())).unwrap();
    let read = config
        .credentials()
        .secret("mcp.docs.TOKEN")
        .unwrap()
        .expect("the stored secret reads");
    assert_eq!(read.expose(), "tok-1");
}

#[test]
fn secret_refuses_a_name_that_is_not_one_file() {
    let root = TempDir::new("cred-secret-name");
    let (config, _) = secret_config(&root);
    for name in ["mcp/docs.TOKEN", "", ".", "..", "a\0b"] {
        let error = config.credentials().secret(name).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArguments, "{name}");
    }
}

#[test]
fn secret_refuses_a_symbolic_link() {
    let root = TempDir::new("cred-secret-link");
    let (config, home) = secret_config(&root);
    let planted = root.path().join("planted");
    fs::write(&planted, "planted-key").unwrap();
    fs::create_dir_all(home.join("credentials")).unwrap();
    symlink(&planted, home.join("credentials").join("mcp.docs.TOKEN")).unwrap();
    let error = config.credentials().secret("mcp.docs.TOKEN").unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    let message = error.to_string();
    assert!(!message.contains("planted-key"), "{message}");
}

#[test]
fn secret_returns_the_stored_value_exactly() {
    let root = TempDir::new("cred-secret-exact");
    let (config, home) = secret_config(&root);
    crate::secret::store_secret(&home, "mcp.docs.TOKEN", &Secret::new("exact-value".into()))
        .unwrap();
    let read = config.credentials().secret("mcp.docs.TOKEN").unwrap();
    assert_eq!(read.as_ref().map(Secret::expose), Some("exact-value"));
}

#[test]
fn secret_is_none_when_absent_and_the_stored_value_otherwise() {
    let root = TempDir::new("cred-secret-default");
    let (config, home) = secret_config(&root);
    assert!(
        config
            .credentials()
            .secret("mcp.docs.TOKEN")
            .unwrap()
            .is_none()
    );
    crate::secret::store_secret(&home, "mcp.docs.TOKEN", &Secret::new("tok-2".into())).unwrap();
    let read = config.credentials().secret("mcp.docs.TOKEN").unwrap();
    assert_eq!(read.as_ref().map(Secret::expose), Some("tok-2"));
    assert_ne!(read.as_ref().map(Secret::expose), Some(""));
}
