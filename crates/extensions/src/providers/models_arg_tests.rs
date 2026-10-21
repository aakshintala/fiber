use std::fs;

use config::{CredentialSource, ProjectKey, ProviderData, Sources};
use fakes::Deadline;
use fakes::clock::FakeClock;

use super::*;

/// A credential source that cannot be read is the error, never an
/// absent key: `models_arg` returns `Error::Config`, so the caller
/// reports the real cause instead of running `models()` keyless.
#[test]
fn models_arg_returns_a_credential_read_failure() {
    let root = fakes::TempDir::new("fiber-models-arg-credential-error");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    // A directory reads as a file nowhere: canonicalisation
    // succeeds, the read fails, and the failure holds on every
    // platform.
    let unreadable = root.path().join("unreadable");
    fs::create_dir_all(&unreadable).unwrap();
    let dir = root.path().join("ext");
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("providers").join("p.lua"),
        "fiber.provider(\"p\", { models = { timeout = 5000, \
             run = function() return {} end } })",
    )
    .unwrap();
    let extension = Arc::new(crate::LuaExtension::new(
        "ext",
        dir,
        home.clone(),
        FakeClock::new(),
    ));
    let provider = LuaProvider::new(extension, "p");
    let mut by_name = BTreeMap::new();
    by_name.insert(
        "p".to_owned(),
        ProviderData {
            name: "p".to_owned(),
            credential: Some(CredentialSource::File(unreadable)),
            credential_name: None,
            headers: BTreeMap::new(),
            placeholders: BTreeMap::new(),
            models: Vec::new(),
            reviewer_model: None,
            login: None,
        },
    );
    let providers = Providers {
        by_name,
        ..Default::default()
    };
    let config = Config::load(Sources {
        home,
        workspace,
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let error = fakes::within(
        "providers.models_arg",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            providers.models_arg(
                "ext",
                &provider,
                &config,
                &fakes::children::grouped_output,
                &|_| None,
            )
        },
    )
    .unwrap_err();
    assert!(
        matches!(error, Error::Config(_)),
        "an unreadable credential file is the error, never an absent key: {error:?}"
    );
    assert_eq!(error.code(), ErrorCode::IoFailed);
}
