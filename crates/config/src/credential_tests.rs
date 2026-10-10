#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::os::unix::fs::symlink;

use contract::ErrorCode;
use fakes::TempDir;

use super::*;
use crate::{ProjectKey, Sources};

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
    };
    // Canonicalising fails, and before it returns another process points
    // the path at a key file.
    let canonical = |file: &Path| -> io::Result<PathBuf> {
        symlink(&planted, file).unwrap();
        Err(io::Error::from(ErrorKind::NotFound))
    };
    let run = |_: &mut Command| -> io::Result<Output> { unreachable!("a file source") };
    let err = config
        .credential_through(&provider, "default", &run, &canonical)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
    let message = err.to_string();
    assert!(message.contains("does not exist"), "{message}");
    assert!(!message.contains("planted-key"), "{message}");
    // The path now resolves to the key, which was never read.
    assert_eq!(fs::read_to_string(&path).unwrap(), "planted-key");
}
